//! Fixed-delay HTTP retry for transient failures (dash.js-style `retryAttempts` /
//! `retryIntervals`).
//!
//! Retries apply **per URL**. After attempts are exhausted, callers fall through to
//! BaseURL failover and representation fallback as today.

use std::future::Future;
use std::time::Duration;

use crate::platform;

use super::HttpError;

/// Class of outbound request used to select retry attempts and interval.
///
/// Defaults follow dash.js `streaming.retryAttempts` / `retryIntervals`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpRequestKind {
    /// MPD fetch (initial load, live refresh, patch).
    Manifest,
    /// Period `xlink:href` expansion.
    Xlink,
    /// Media segment bytes.
    MediaSegment,
    /// Initialization segment bytes.
    InitializationSegment,
    /// Index / `sidx` segment bytes.
    IndexSegment,
    /// Other GETs (content steering, clock sync helpers, …).
    Other,
}

/// Resolved try count and delay for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpRetryPolicy {
    /// Tries for this URL, including the first. `1` means no retry.
    pub max_attempts: u32,
    /// Fixed delay between tries (not exponential).
    pub interval: Duration,
}

/// Per-request-type retry settings with optional low-latency scaling.
///
/// When [`Self::low_latency_scale`] is applied (LL-DASH / CMAF chunked fetch):
/// - intervals are divided by [`Self::low_latency_reduction_factor`] (default 10)
/// - attempt counts are multiplied by [`Self::low_latency_multiply_factor`] (default 5)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRetryConfig {
    manifest_attempts: u32,
    manifest_interval: Duration,
    xlink_attempts: u32,
    xlink_interval: Duration,
    media_attempts: u32,
    media_interval: Duration,
    init_attempts: u32,
    init_interval: Duration,
    index_attempts: u32,
    index_interval: Duration,
    other_attempts: u32,
    other_interval: Duration,
    low_latency_multiply_factor: u32,
    low_latency_reduction_factor: u32,
}

impl Default for HttpRetryConfig {
    fn default() -> Self {
        Self {
            // dash.js: MPD 3 × 500 ms; Media/Init/Index 3 × 1000 ms; XLink 1 × 500 ms
            manifest_attempts: 3,
            manifest_interval: Duration::from_millis(500),
            xlink_attempts: 1,
            xlink_interval: Duration::from_millis(500),
            media_attempts: 3,
            media_interval: Duration::from_millis(1000),
            init_attempts: 3,
            init_interval: Duration::from_millis(1000),
            index_attempts: 3,
            index_interval: Duration::from_millis(1000),
            other_attempts: 3,
            other_interval: Duration::from_millis(1000),
            low_latency_multiply_factor: 5,
            low_latency_reduction_factor: 10,
        }
    }
}

impl HttpRetryConfig {
    /// Dash.js-aligned defaults (enabled).
    pub fn new() -> Self {
        Self::default()
    }

    /// Single attempt for every request type (no retries).
    pub fn disabled() -> Self {
        Self {
            manifest_attempts: 1,
            manifest_interval: Duration::ZERO,
            xlink_attempts: 1,
            xlink_interval: Duration::ZERO,
            media_attempts: 1,
            media_interval: Duration::ZERO,
            init_attempts: 1,
            init_interval: Duration::ZERO,
            index_attempts: 1,
            index_interval: Duration::ZERO,
            other_attempts: 1,
            other_interval: Duration::ZERO,
            low_latency_multiply_factor: 1,
            low_latency_reduction_factor: 1,
        }
    }

    /// Override attempts for media segments (including the first try).
    pub fn with_media_attempts(mut self, attempts: u32) -> Self {
        self.media_attempts = attempts.max(1);
        self
    }

    /// Override fixed delay between media segment tries.
    pub fn with_media_interval(mut self, interval: Duration) -> Self {
        self.media_interval = interval;
        self
    }

    /// Override attempts for MPD fetches.
    pub fn with_manifest_attempts(mut self, attempts: u32) -> Self {
        self.manifest_attempts = attempts.max(1);
        self
    }

    /// Override fixed delay between MPD tries.
    pub fn with_manifest_interval(mut self, interval: Duration) -> Self {
        self.manifest_interval = interval;
        self
    }

    /// Factor applied to attempt counts in low-latency mode (dash.js default 5).
    pub fn with_low_latency_multiply_factor(mut self, factor: u32) -> Self {
        self.low_latency_multiply_factor = factor.max(1);
        self
    }

    /// Divisor applied to intervals in low-latency mode (dash.js default 10).
    pub fn with_low_latency_reduction_factor(mut self, factor: u32) -> Self {
        self.low_latency_reduction_factor = factor.max(1);
        self
    }

    /// Resolve policy for `kind`, optionally scaled for low-latency playback.
    pub fn policy(&self, kind: HttpRequestKind, low_latency: bool) -> HttpRetryPolicy {
        let (mut max_attempts, mut interval) = match kind {
            HttpRequestKind::Manifest => (self.manifest_attempts, self.manifest_interval),
            HttpRequestKind::Xlink => (self.xlink_attempts, self.xlink_interval),
            HttpRequestKind::MediaSegment => (self.media_attempts, self.media_interval),
            HttpRequestKind::InitializationSegment => (self.init_attempts, self.init_interval),
            HttpRequestKind::IndexSegment => (self.index_attempts, self.index_interval),
            HttpRequestKind::Other => (self.other_attempts, self.other_interval),
        };
        max_attempts = max_attempts.max(1);
        if low_latency {
            max_attempts = max_attempts.saturating_mul(self.low_latency_multiply_factor);
            interval /= self.low_latency_reduction_factor;
        }
        HttpRetryPolicy {
            max_attempts,
            interval,
        }
    }
}

/// Whether an HTTP status should be retried on the same URL.
pub fn is_transient_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || (500..600).contains(&status)
}

/// Run `attempt` up to `policy.max_attempts` times with a fixed delay between tries.
///
/// `is_transient` decides whether the error warrants another try. Permanent failures
/// return immediately without sleeping.
///
/// When `cancel` is set, an in-flight attempt is dropped and pending retry sleeps are
/// aborted as soon as the guard observes cancellation, returning [`HttpError::Cancelled`].
pub async fn with_retry<T, E, F, Fut, P>(
    policy: HttpRetryPolicy,
    kind: HttpRequestKind,
    mut attempt: F,
    is_transient: P,
    mut cancel: Option<&mut crate::playback_control::FetchCancelGuard>,
) -> Result<T, E>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, E>>,
    P: Fn(&E) -> bool,
    E: From<HttpError>,
{
    let max = policy.max_attempts.max(1);
    let mut attempt_idx = 0u32;
    let _ = kind;
    loop {
        if cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            return Err(HttpError::Cancelled.into());
        }

        let result = if let Some(cancel) = cancel.as_deref_mut() {
            tokio::select! {
                biased;
                result = attempt(attempt_idx) => result,
                _ = cancel.cancelled() => Err(HttpError::Cancelled.into()),
            }
        } else {
            attempt(attempt_idx).await
        };

        match result {
            Ok(v) => return Ok(v),
            Err(err) => {
                if cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                    return Err(HttpError::Cancelled.into());
                }
                let retryable = is_transient(&err) && attempt_idx + 1 < max;
                if !retryable {
                    return Err(err);
                }
                if !policy.interval.is_zero() {
                    if let Some(cancel) = cancel.as_deref_mut() {
                        tokio::select! {
                            biased;
                            _ = platform::sleep(policy.interval) => {}
                            _ = cancel.cancelled() => return Err(HttpError::Cancelled.into()),
                        }
                    } else {
                        platform::sleep(policy.interval).await;
                    }
                }
                attempt_idx += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn default_media_policy_matches_dashjs() {
        let cfg = HttpRetryConfig::default();
        let p = cfg.policy(HttpRequestKind::MediaSegment, false);
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.interval, Duration::from_millis(1000));
    }

    #[test]
    fn low_latency_scales_attempts_and_interval() {
        let cfg = HttpRetryConfig::default();
        let p = cfg.policy(HttpRequestKind::MediaSegment, true);
        assert_eq!(p.max_attempts, 15);
        assert_eq!(p.interval, Duration::from_millis(100));
    }

    #[test]
    fn disabled_is_single_attempt() {
        let cfg = HttpRetryConfig::disabled();
        let p = cfg.policy(HttpRequestKind::Manifest, true);
        assert_eq!(p.max_attempts, 1);
    }

    #[tokio::test]
    async fn with_retry_retries_transient_then_succeeds() {
        let hits = AtomicU32::new(0);
        let policy = HttpRetryPolicy {
            max_attempts: 3,
            interval: Duration::ZERO,
        };
        let result = with_retry(
            policy,
            HttpRequestKind::MediaSegment,
            |_| async {
                let n = hits.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(HttpError::Transport("503".into()))
                } else {
                    Ok(42)
                }
            },
            |err| matches!(err, HttpError::Transport(_)),
            None,
        )
        .await;
        assert_eq!(result, Ok(42));
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn with_retry_does_not_retry_permanent() {
        let hits = AtomicU32::new(0);
        let policy = HttpRetryPolicy {
            max_attempts: 5,
            interval: Duration::ZERO,
        };
        let result: Result<(), HttpError> = with_retry(
            policy,
            HttpRequestKind::MediaSegment,
            |_| async {
                hits.fetch_add(1, Ordering::SeqCst);
                Err(HttpError::Transport("404".into()))
            },
            |_| false,
            None,
        )
        .await;
        assert_eq!(result, Err(HttpError::Transport("404".into())));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn with_retry_aborts_pending_sleep_on_cancel() {
        use crate::playback_control::{PausePolicy, PlaybackController};

        let playback = PlaybackController::new();
        playback.mark_started();
        playback.set_pause_policy(PausePolicy::stop_and_cancel_inflight());
        let mut cancel = playback.fetch_cancel_guard();

        let hits = AtomicU32::new(0);
        let policy = HttpRetryPolicy {
            max_attempts: 5,
            interval: Duration::from_secs(30),
        };
        let playback2 = playback.clone();
        let join = tokio::spawn(async move {
            crate::platform::sleep(Duration::from_millis(20)).await;
            playback2.pause().unwrap();
        });

        let result: Result<(), HttpError> = with_retry(
            policy,
            HttpRequestKind::MediaSegment,
            |_| async {
                hits.fetch_add(1, Ordering::SeqCst);
                Err(HttpError::Transport("503".into()))
            },
            |_| true,
            Some(&mut cancel),
        )
        .await;

        let _ = join.await;
        assert_eq!(result, Err(HttpError::Cancelled));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
