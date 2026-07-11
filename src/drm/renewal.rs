//! Widevine license renewal scheduling from KEY_CONTROL blocks and license policy.

use super::cdm::Key;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::widevine::LicenseError;

/// Minimum lead time before a key or license expiry to start renewal attempts.
const RENEWAL_LEAD_TIME: Duration = Duration::from_secs(60);

/// Default retry interval when the license policy omits one.
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Cap exponential backoff at 2^6 = 64× the base interval.
const MAX_BACKOFF_SHIFT: u32 = 6;

/// Parsed renewal timing merged into a session after each license response.
#[derive(Debug, Clone, Default)]
pub(crate) struct RenewalSchedule {
    pub renew_after: Option<Instant>,
    pub expire_at: Option<Instant>,
    pub can_renew: bool,
    pub renewal_retry_interval: Duration,
    pub renewal_server_url: Option<String>,
}

/// Session-local renewal state and retry bookkeeping.
#[derive(Debug, Default)]
pub(crate) struct RenewalState {
    renew_after: Option<Instant>,
    expire_at: Option<Instant>,
    can_renew: bool,
    renewal_retry_interval: Duration,
    renewal_server_url: Option<String>,
    last_attempt: Option<Instant>,
    failed_attempts: u32,
}

impl RenewalState {
    pub(crate) fn merge_schedule(&mut self, schedule: RenewalSchedule) {
        if schedule.can_renew {
            self.can_renew = true;
        }
        if let Some(url) = schedule.renewal_server_url {
            self.renewal_server_url = Some(url);
        }
        if schedule.renewal_retry_interval > Duration::ZERO {
            self.renewal_retry_interval = schedule.renewal_retry_interval;
        }
        self.renew_after = earliest_instant(self.renew_after, schedule.renew_after);
        self.expire_at = earliest_instant(self.expire_at, schedule.expire_at);
    }

    pub(crate) fn update_from_key_control(&mut self, key: &Key, applied_at: Instant) {
        let Some(ttl) = parse_key_control_ttl(&key.key) else {
            return;
        };
        if ttl == 0 {
            return;
        }
        let ttl_duration = Duration::from_secs(u64::from(ttl));
        let expire_at = applied_at.checked_add(ttl_duration).unwrap_or(applied_at);
        let lead = RENEWAL_LEAD_TIME.min(ttl_duration / 10);
        let renew_after = expire_at.checked_sub(lead).unwrap_or(expire_at);
        self.merge_schedule(RenewalSchedule {
            renew_after: Some(renew_after),
            expire_at: Some(expire_at),
            ..RenewalSchedule::default()
        });
    }

    pub(crate) fn needs_renewal(&self, now: Instant) -> bool {
        if !self.can_renew {
            // KEY_CONTROL-only sessions without explicit can_renew still renew before expiry.
            if self.renew_after.is_none() {
                return false;
            }
        }

        let Some(renew_after) = self.renew_after else {
            return false;
        };
        if now < renew_after {
            return false;
        }

        if let Some(last) = self.last_attempt {
            let backoff = self.retry_interval_with_backoff();
            if now.duration_since(last) < backoff {
                return false;
            }
        }

        true
    }

    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        self.expire_at.is_some_and(|deadline| now >= deadline)
    }

    pub(crate) fn mark_renewal_attempt(&mut self, now: Instant) {
        self.last_attempt = Some(now);
    }

    pub(crate) fn mark_renewal_success(&mut self) {
        self.failed_attempts = 0;
    }

    pub(crate) fn mark_renewal_failure(&mut self) {
        self.failed_attempts = self.failed_attempts.saturating_add(1);
    }

    pub(crate) fn can_renew(&self) -> bool {
        self.can_renew || self.renew_after.is_some()
    }

    pub(crate) fn renewal_server_url(&self) -> Option<&str> {
        self.renewal_server_url.as_deref()
    }

    fn retry_interval_with_backoff(&self) -> Duration {
        let base = if self.renewal_retry_interval.is_zero() {
            DEFAULT_RETRY_INTERVAL
        } else {
            self.renewal_retry_interval
        };
        let shift = self.failed_attempts.min(MAX_BACKOFF_SHIFT);
        base.saturating_mul(1u32 << shift)
    }
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
fn parse_key_control_ttl(block: &[u8]) -> Option<u32> {
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

fn earliest_instant(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (None, Some(y)) => Some(y),
        (Some(x), None) => Some(x),
        (None, None) => None,
    }
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
    fn renewal_state_needs_renewal_from_synthetic_key_control() {
        let applied_at = Instant::now();
        let mut state = RenewalState::default();
        state.update_from_key_control(&synthetic_key_control(30), applied_at);

        assert!(
            !state.needs_renewal(applied_at + Duration::from_secs(5)),
            "renewal should not run immediately after apply"
        );
        assert!(
            state.needs_renewal(applied_at + Duration::from_secs(29)),
            "renewal should run before the 30s TTL expires"
        );
    }

    #[test]
    fn renewal_backoff_delays_subsequent_attempts() {
        let applied_at = Instant::now();
        let mut state = RenewalState::default();
        state.merge_schedule(RenewalSchedule {
            can_renew: true,
            renew_after: Some(applied_at),
            renewal_retry_interval: Duration::from_secs(10),
            ..RenewalSchedule::default()
        });
        state.mark_renewal_attempt(applied_at);
        state.mark_renewal_failure();

        assert!(
            !state.needs_renewal(applied_at + Duration::from_secs(5)),
            "backoff should suppress retry within 2^1 * 10s"
        );
        assert!(
            state.needs_renewal(applied_at + Duration::from_secs(21)),
            "backoff window should elapse"
        );
    }

    #[test]
    fn parse_key_control_ttl_reads_kctl_block() {
        let key = synthetic_key_control(3600);
        assert_eq!(parse_key_control_ttl(&key.key), Some(3600));
        assert_eq!(parse_key_control_ttl(b"bad"), None);
    }
}
