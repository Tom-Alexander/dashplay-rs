//! Session-local renewal scheduling: deadlines, retry backoff, and poll timing.

use super::policy::{RenewalSchedule, schedule_from_key_control};
use crate::drm::cdm::Key;
use crate::platform::Instant;
use std::time::Duration;

/// Default retry interval when the license policy omits one.
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Cap exponential backoff at 2^6 = 64× the base interval.
const MAX_BACKOFF_SHIFT: u32 = 6;

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
        if let Some(schedule) = schedule_from_key_control(key, applied_at) {
            self.merge_schedule(schedule);
        }
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

    /// Force renewal to be due immediately (test helper).
    #[cfg(test)]
    pub(crate) fn force_due(&mut self, now: Instant) {
        self.merge_schedule(RenewalSchedule {
            can_renew: true,
            renew_after: Some(now),
            ..RenewalSchedule::default()
        });
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
}
