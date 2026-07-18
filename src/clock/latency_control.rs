//! LL-DASH target-latency catch-up (DASH-IF IOP / DVB-DASH §10.20).
//!
//! When `ServiceDescription/Latency@target` is present, the player measures live
//! latency against that target and suggests a consumption (playback) rate within
//! `ServiceDescription/PlaybackRate` bounds so the consumer can chase the target.
//! Decoding and rate application remain out of scope for this crate.

use std::time::Duration;

use dash_mpd::MPD;

use crate::platform::Instant;

/// Default catch-up bounds when `Latency` is present but `PlaybackRate` is omitted.
const DEFAULT_RATE_MIN: f64 = 0.95;
const DEFAULT_RATE_MAX: f64 = 1.05;

/// Dead zone around the target (seconds) where the rate stays at 1.0.
const TARGET_TOLERANCE_S: f64 = 0.02;

/// Minimum absolute rate change before treating the suggestion as updated.
pub(crate) const MIN_RATE_CHANGE: f64 = 0.01;

/// Catch-up parameters from the first in-scope `ServiceDescription`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LatencyPolicy {
    /// Preferred live latency (`Latency@target`, milliseconds in the MPD).
    pub target: Duration,
    /// Optional lower bound (`Latency@min`).
    pub min: Option<Duration>,
    /// Optional upper bound (`Latency@max`); exceeding this triggers a seek to target.
    pub max: Option<Duration>,
    /// Minimum allowed playback rate (`PlaybackRate@min`).
    pub rate_min: f64,
    /// Maximum allowed playback rate (`PlaybackRate@max`).
    pub rate_max: f64,
}

impl LatencyPolicy {
    /// Parse policy from the first in-scope `ServiceDescription` with a usable `Latency@target`.
    pub(crate) fn from_mpd(mpd: &MPD) -> Option<Self> {
        for sd in crate::clock::service_description::in_scope_service_descriptions(mpd) {
            for lat in &sd.Latency {
                let target_ms = lat.target?;
                if !target_ms.is_finite() || target_ms < 0.0 {
                    continue;
                }
                let target = Duration::from_secs_f64(target_ms / 1000.0);
                let min = millis_to_duration(lat.min);
                let max = millis_to_duration(lat.max);

                let (rate_min, rate_max) = playback_rate_bounds(sd);
                return Some(Self {
                    target,
                    min,
                    max,
                    rate_min,
                    rate_max,
                });
            }
        }
        None
    }
}

fn millis_to_duration(ms: Option<f64>) -> Option<Duration> {
    let ms = ms?;
    if !ms.is_finite() || ms < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(ms / 1000.0))
}

fn playback_rate_bounds(sd: &dash_mpd::ServiceDescription) -> (f64, f64) {
    for rate in &sd.PlaybackRate {
        let min = rate
            .min
            .filter(|v| v.is_finite() && *v > 0.0 && *v <= 1.0)
            .unwrap_or(DEFAULT_RATE_MIN);
        let max = rate
            .max
            .filter(|v| v.is_finite() && *v >= 1.0)
            .unwrap_or(DEFAULT_RATE_MAX);
        if min <= max {
            return (min, max);
        }
    }
    (DEFAULT_RATE_MIN, DEFAULT_RATE_MAX)
}

/// Monotonic extrapolation of MPD-timeline time since `availabilityStartTime`.
#[derive(Debug, Clone)]
pub(crate) struct LiveClock {
    since_ast_at_anchor: Duration,
    anchor: Instant,
}

impl LiveClock {
    pub(crate) fn new(since_ast: Duration) -> Self {
        Self {
            since_ast_at_anchor: since_ast,
            anchor: Instant::now(),
        }
    }

    pub(crate) fn since_ast_now(&self) -> Duration {
        self.since_ast_at_anchor
            .saturating_add(self.anchor.elapsed())
    }
}

/// Live latency: wall-synced media time minus presentation time.
pub(crate) fn live_latency(since_ast: Duration, presentation_time: Duration) -> Duration {
    since_ast.saturating_sub(presentation_time)
}

/// Suggested consumption rate to chase [`LatencyPolicy::target`].
///
/// Uses a sigmoid mapping (DASH-IF / dash.js default catch-up shape), clamped to
/// `PlaybackRate` bounds and `Latency@min` / `@max` extremes.
pub(crate) fn suggested_playback_rate(current_latency: Duration, policy: &LatencyPolicy) -> f64 {
    let current_s = current_latency.as_secs_f64();
    let target_s = policy.target.as_secs_f64();
    let delta = current_s - target_s;

    if delta.abs() <= TARGET_TOLERANCE_S {
        return 1.0;
    }

    if let Some(min) = policy.min {
        if current_s < min.as_secs_f64() {
            return policy.rate_min;
        }
    }
    if let Some(max) = policy.max {
        if current_s > max.as_secs_f64() {
            return policy.rate_max;
        }
    }

    let catchup_range = if delta < 0.0 {
        (1.0 - policy.rate_min).abs()
    } else {
        (policy.rate_max - 1.0).abs()
    };
    if catchup_range == 0.0 {
        return 1.0;
    }

    // dash.js default: s = (cpr * 2) / (1 + e^(-delta*5)); rate = (1 - cpr) + s
    let d = delta * 5.0;
    let s = (catchup_range * 2.0) / (1.0 + (-d).exp());
    let rate = (1.0 - catchup_range) + s;
    rate.clamp(policy.rate_min, policy.rate_max)
}

/// Whether latency exceeds `Latency@max` and the session should jump to the target edge.
pub(crate) fn should_seek_to_target(current_latency: Duration, policy: &LatencyPolicy) -> bool {
    policy
        .max
        .is_some_and(|max| current_latency > max && max > policy.target)
}

/// Presentation time that realizes [`LatencyPolicy::target`] at `since_ast`.
pub(crate) fn target_presentation_time(since_ast: Duration, policy: &LatencyPolicy) -> Duration {
    since_ast.saturating_sub(policy.target)
}

/// Result of evaluating latency control against the current playhead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LatencyControlUpdate {
    pub latency: Duration,
    pub rate: f64,
    pub rate_changed: bool,
    pub seek_target: Option<Duration>,
}

/// Evaluate catch-up for the current playhead and live clock.
pub(crate) fn evaluate(
    clock: &LiveClock,
    presentation_time: Duration,
    policy: &LatencyPolicy,
    previous_rate: f64,
) -> LatencyControlUpdate {
    let since_ast = clock.since_ast_now();
    let latency = live_latency(since_ast, presentation_time);
    let rate = suggested_playback_rate(latency, policy);
    let rate_changed =
        (rate - previous_rate).abs() >= MIN_RATE_CHANGE || (rate == 1.0 && previous_rate != 1.0);
    let seek_target = if should_seek_to_target(latency, policy) {
        Some(target_presentation_time(since_ast, policy))
    } else {
        None
    };
    LatencyControlUpdate {
        latency,
        rate,
        rate_changed,
        seek_target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_mpd::{Latency, PlaybackRate, ServiceDescription};

    fn policy(target_ms: f64, min_ms: Option<f64>, max_ms: Option<f64>) -> LatencyPolicy {
        LatencyPolicy {
            target: Duration::from_secs_f64(target_ms / 1000.0),
            min: min_ms.map(|ms| Duration::from_secs_f64(ms / 1000.0)),
            max: max_ms.map(|ms| Duration::from_secs_f64(ms / 1000.0)),
            rate_min: 0.96,
            rate_max: 1.04,
        }
    }

    #[test]
    fn from_mpd_reads_latency_and_playback_rate() {
        let mpd = MPD {
            ServiceDescription: vec![ServiceDescription {
                Latency: vec![Latency {
                    target: Some(3500.0),
                    min: Some(2000.0),
                    max: Some(6000.0),
                    ..Default::default()
                }],
                PlaybackRate: vec![PlaybackRate {
                    min: Some(0.96),
                    max: Some(1.04),
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = LatencyPolicy::from_mpd(&mpd).expect("policy");
        assert_eq!(p.target, Duration::from_millis(3500));
        assert_eq!(p.min, Some(Duration::from_secs(2)));
        assert_eq!(p.max, Some(Duration::from_secs(6)));
        assert!((p.rate_min - 0.96).abs() < 1e-9);
        assert!((p.rate_max - 1.04).abs() < 1e-9);
    }

    #[test]
    fn from_mpd_defaults_playback_rate_when_absent() {
        let mpd = MPD {
            ServiceDescription: vec![ServiceDescription {
                Latency: vec![Latency {
                    target: Some(3000.0),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = LatencyPolicy::from_mpd(&mpd).expect("policy");
        assert!((p.rate_min - DEFAULT_RATE_MIN).abs() < 1e-9);
        assert!((p.rate_max - DEFAULT_RATE_MAX).abs() < 1e-9);
    }

    #[test]
    fn from_mpd_skips_out_of_scope_service_description() {
        use dash_mpd::Scope;
        let mpd = MPD {
            ServiceDescription: vec![
                ServiceDescription {
                    scopes: vec![Scope {
                        schemeIdUri: "urn:example:other".into(),
                        ..Default::default()
                    }],
                    Latency: vec![Latency {
                        target: Some(1000.0),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ServiceDescription {
                    Latency: vec![Latency {
                        target: Some(3500.0),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let p = LatencyPolicy::from_mpd(&mpd).expect("policy");
        assert_eq!(p.target, Duration::from_millis(3500));
    }

    #[test]
    fn rate_is_unity_near_target() {
        let p = policy(3500.0, None, None);
        let rate = suggested_playback_rate(Duration::from_millis(3505), &p);
        assert!((rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rate_increases_when_latency_above_target() {
        let p = policy(3500.0, Some(2000.0), Some(6000.0));
        let rate = suggested_playback_rate(Duration::from_millis(4500), &p);
        assert!(rate > 1.0, "expected catch-up, got {rate}");
        assert!(rate <= p.rate_max);
    }

    #[test]
    fn rate_decreases_when_latency_below_target() {
        let p = policy(3500.0, Some(2000.0), Some(6000.0));
        let rate = suggested_playback_rate(Duration::from_millis(2500), &p);
        assert!(rate < 1.0, "expected fall-back, got {rate}");
        assert!(rate >= p.rate_min);
    }

    #[test]
    fn rate_clamps_at_latency_min_and_max() {
        let p = policy(3500.0, Some(2000.0), Some(6000.0));
        assert_eq!(
            suggested_playback_rate(Duration::from_millis(1000), &p),
            p.rate_min
        );
        assert_eq!(
            suggested_playback_rate(Duration::from_millis(7000), &p),
            p.rate_max
        );
    }

    #[test]
    fn seek_when_above_max_latency() {
        let p = policy(3500.0, Some(2000.0), Some(6000.0));
        assert!(!should_seek_to_target(Duration::from_millis(5000), &p));
        assert!(should_seek_to_target(Duration::from_millis(7000), &p));
        assert_eq!(
            target_presentation_time(Duration::from_secs(20), &p),
            Duration::from_millis(16_500)
        );
    }

    #[test]
    fn evaluate_reports_rate_change() {
        let policy = policy(3500.0, None, None);
        let clock = LiveClock {
            since_ast_at_anchor: Duration::from_secs(20),
            anchor: Instant::now(),
        };
        // presentation 15s → latency ~5s → catch-up
        let update = evaluate(&clock, Duration::from_secs(15), &policy, 1.0);
        assert!(update.rate > 1.0);
        assert!(update.rate_changed);
        assert!(update.seek_target.is_none());
    }
}
