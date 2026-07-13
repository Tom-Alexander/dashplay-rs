//! CDM protocol parsing for renewal timing: KEY_CONTROL (`kctl`) blocks and license policy.

use crate::drm::cdm::Key;
use crate::drm::widevine::LicenseError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Minimum lead time before a key or license expiry to start renewal attempts.
pub(crate) const RENEWAL_LEAD_TIME: Duration = Duration::from_secs(60);

/// Parsed renewal timing merged into a session after each license response.
#[derive(Debug, Clone, Default)]
pub(crate) struct RenewalSchedule {
    pub renew_after: Option<Instant>,
    pub expire_at: Option<Instant>,
    pub can_renew: bool,
    pub renewal_retry_interval: Duration,
    pub renewal_server_url: Option<String>,
}

/// Build a renewal schedule from a decrypted KEY_CONTROL key.
pub(crate) fn schedule_from_key_control(key: &Key, applied_at: Instant) -> Option<RenewalSchedule> {
    let ttl = parse_key_control_ttl(&key.key)?;
    if ttl == 0 {
        return None;
    }
    let ttl_duration = Duration::from_secs(u64::from(ttl));
    let expire_at = applied_at.checked_add(ttl_duration).unwrap_or(applied_at);
    let lead = RENEWAL_LEAD_TIME.min(ttl_duration / 10);
    let renew_after = expire_at.checked_sub(lead).unwrap_or(expire_at);
    Some(RenewalSchedule {
        renew_after: Some(renew_after),
        expire_at: Some(expire_at),
        ..RenewalSchedule::default()
    })
}

/// Parse renewal timing from a Widevine license response protobuf.
pub(crate) fn schedule_from_license_message(
    license_message: &[u8],
    applied_at: Instant,
) -> Result<RenewalSchedule, LicenseError> {
    use protobuf::Message;
    use widevine_proto::license_protocol::{License, SignedMessage, signed_message::MessageType};

    let signed = SignedMessage::parse_from_bytes(license_message)
        .map_err(|e| LicenseError::RenewalParse(e.to_string()))?;
    if signed.type_() != MessageType::LICENSE {
        return Err(LicenseError::RenewalParse(format!(
            "expected LICENSE message, got {:?}",
            signed.type_()
        )));
    }

    let license = License::parse_from_bytes(signed.msg())
        .map_err(|e| LicenseError::RenewalParse(e.to_string()))?;

    let applied_unix = unix_now();
    let license_start = if license.has_license_start_time() {
        license.license_start_time()
    } else {
        applied_unix
    };

    let mut schedule = RenewalSchedule::default();
    if let Some(policy) = license.policy.as_ref() {
        schedule.can_renew = policy.can_renew();
        if policy.has_renewal_retry_interval_seconds() {
            let secs = policy.renewal_retry_interval_seconds().max(1);
            schedule.renewal_retry_interval = Duration::from_secs(secs as u64);
        }
        if policy.has_renewal_server_url() {
            schedule.renewal_server_url = Some(policy.renewal_server_url().to_string());
        }

        if policy.can_renew() {
            if policy.has_renewal_delay_seconds() {
                let renew_unix = license_start.saturating_add(policy.renewal_delay_seconds());
                schedule.renew_after = Some(unix_to_instant(renew_unix, applied_unix, applied_at));
            }
            if policy.has_license_duration_seconds() {
                let duration = policy.license_duration_seconds();
                if duration > 0 {
                    let expire_unix = license_start.saturating_add(duration);
                    schedule.expire_at =
                        Some(unix_to_instant(expire_unix, applied_unix, applied_at));
                    if schedule.renew_after.is_none() {
                        let lead_secs = RENEWAL_LEAD_TIME.as_secs().min(duration as u64);
                        let renew_unix = expire_unix.saturating_sub(lead_secs as i64);
                        schedule.renew_after =
                            Some(unix_to_instant(renew_unix, applied_unix, applied_at));
                    }
                }
            }
        }
    }

    Ok(schedule)
}

/// Parse the TTL field from a decrypted KEY_CONTROL block (`kctl` magic).
pub(crate) fn parse_key_control_ttl(block: &[u8]) -> Option<u32> {
    if block.len() < 12 || &block[..4] != b"kctl" {
        return None;
    }
    Some(u32::from_be_bytes(block[8..12].try_into().ok()?))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn unix_to_instant(target_unix: i64, reference_unix: i64, reference_instant: Instant) -> Instant {
    let delta = target_unix.saturating_sub(reference_unix);
    reference_instant
        .checked_add(Duration::from_secs(delta.max(0) as u64))
        .unwrap_or(reference_instant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drm::cdm::{Key, KeyType};

    fn synthetic_key_control(ttl_secs: u32) -> Key {
        let mut block = b"kctl".to_vec();
        block.extend_from_slice(&1u32.to_be_bytes());
        block.extend_from_slice(&ttl_secs.to_be_bytes());
        block.extend_from_slice(&0u32.to_be_bytes());
        Key {
            typ: KeyType::KEY_CONTROL,
            kid: [0xAB; 16],
            key: block,
        }
    }

    #[test]
    fn parse_key_control_ttl_reads_kctl_block() {
        let key = synthetic_key_control(3600);
        assert_eq!(parse_key_control_ttl(&key.key), Some(3600));
        assert_eq!(parse_key_control_ttl(b"bad"), None);
    }

    #[test]
    fn schedule_from_key_control_applies_lead_time() {
        let applied_at = Instant::now();
        let schedule = schedule_from_key_control(&synthetic_key_control(30), applied_at)
            .expect("schedule from key control");
        let renew_after = schedule.renew_after.expect("renew_after");
        let expire_at = schedule.expire_at.expect("expire_at");
        assert_eq!(expire_at, applied_at + Duration::from_secs(30));
        assert!(renew_after < expire_at);
        assert!(renew_after >= applied_at);
    }
}
