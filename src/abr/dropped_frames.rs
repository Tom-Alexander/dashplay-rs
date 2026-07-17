//! Dropped-frame ABR rule (dash.js: `DroppedFramesRule` / `DroppedFramesHistory`).
//!
//! Hosts report absolute frame counters (HTML5 `VideoPlaybackQuality` or equivalent).
//! Intervals are attributed to the currently playing quality index; when a quality
//! above the lowest rung exceeds the drop ratio threshold, ABR is capped one rung below.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// dash.js default: frames required before the rule evaluates a representation.
pub const DEFAULT_MINIMUM_SAMPLE_SIZE: u64 = 375;

/// dash.js default: dropped/total ratio that forces a down-switch (0–1).
pub const DEFAULT_DROPPED_FRAMES_PERCENTAGE_THRESHOLD: f64 = 0.15;

/// Parameters for the dropped-frames ABR rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DroppedFramesParams {
    /// Sum of rendered and dropped frames required for each quality before the rule
    /// evaluates that rung (dash.js: `minimumSampleSize`).
    pub minimum_sample_size: u64,
    /// Dropped/total ratio (0–1) that triggers a quality down-switch
    /// (dash.js: `droppedFramesPercentageThreshold`).
    pub dropped_frames_percentage_threshold: f64,
}

impl Default for DroppedFramesParams {
    fn default() -> Self {
        Self {
            minimum_sample_size: DEFAULT_MINIMUM_SAMPLE_SIZE,
            dropped_frames_percentage_threshold: DEFAULT_DROPPED_FRAMES_PERCENTAGE_THRESHOLD,
        }
    }
}

impl DroppedFramesParams {
    /// Sanitize caller-provided values, falling back to defaults when invalid.
    pub fn sanitized(self) -> Self {
        let minimum_sample_size = if self.minimum_sample_size > 0 {
            self.minimum_sample_size
        } else {
            DEFAULT_MINIMUM_SAMPLE_SIZE
        };
        let dropped_frames_percentage_threshold =
            if self.dropped_frames_percentage_threshold.is_finite()
                && (0.0..=1.0).contains(&self.dropped_frames_percentage_threshold)
            {
                self.dropped_frames_percentage_threshold
            } else {
                DEFAULT_DROPPED_FRAMES_PERCENTAGE_THRESHOLD
            };
        Self {
            minimum_sample_size,
            dropped_frames_percentage_threshold,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct QualityFrameStats {
    dropped_video_frames: u64,
    total_video_frames: u64,
}

/// Per-track dropped-frame accumulators attributed by quality index.
///
/// Clone handles share the same session (same pattern as [`crate::TrackMetrics`]).
#[derive(Clone)]
pub struct DroppedFramesHistory {
    inner: Arc<Mutex<DroppedFramesHistoryInner>>,
}

#[derive(Debug)]
struct DroppedFramesHistoryInner {
    params: DroppedFramesParams,
    active_quality_index: Option<usize>,
    last_dropped: u64,
    last_total: u64,
    has_baseline: bool,
    by_quality: HashMap<usize, QualityFrameStats>,
}

impl DroppedFramesHistory {
    /// Create a history with dash.js-default parameters.
    pub fn new() -> Self {
        Self::with_params(DroppedFramesParams::default())
    }

    /// Create a history with custom rule parameters.
    pub fn with_params(params: DroppedFramesParams) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DroppedFramesHistoryInner {
                params: params.sanitized(),
                active_quality_index: None,
                last_dropped: 0,
                last_total: 0,
                has_baseline: false,
                by_quality: HashMap::new(),
            })),
        }
    }

    /// Attribute subsequent interval samples to this quality ladder index.
    pub fn set_active_quality(&self, quality_index: usize) {
        self.with_inner_mut(|inner| {
            inner.active_quality_index = Some(quality_index);
        });
    }

    /// Current quality index used for attributing frame intervals, if set.
    pub fn active_quality(&self) -> Option<usize> {
        self.with_inner(|inner| inner.active_quality_index)
    }

    /// Push absolute frame counters from the host decoder / MSE pipeline.
    ///
    /// Non-monotonic or zero-delta samples are ignored. The first sample establishes a
    /// baseline without attributing frames (matches dash.js interval differencing).
    pub fn push(&self, dropped_video_frames: u64, total_video_frames: u64) {
        self.with_inner_mut(|inner| {
            if !inner.has_baseline {
                inner.last_dropped = dropped_video_frames;
                inner.last_total = total_video_frames;
                inner.has_baseline = true;
                return;
            }

            if dropped_video_frames < inner.last_dropped || total_video_frames < inner.last_total {
                // Counters reset (media reload); re-baseline without attributing.
                inner.last_dropped = dropped_video_frames;
                inner.last_total = total_video_frames;
                return;
            }

            let interval_dropped = dropped_video_frames - inner.last_dropped;
            let interval_total = total_video_frames - inner.last_total;
            inner.last_dropped = dropped_video_frames;
            inner.last_total = total_video_frames;

            if interval_total == 0 && interval_dropped == 0 {
                return;
            }

            let Some(quality_index) = inner.active_quality_index else {
                return;
            };

            let entry = inner.by_quality.entry(quality_index).or_default();
            entry.dropped_video_frames =
                entry.dropped_video_frames.saturating_add(interval_dropped);
            entry.total_video_frames = entry.total_video_frames.saturating_add(interval_total);
        });
    }

    /// Maximum quality index ABR may select, if the rule fires.
    ///
    /// Walks qualities from index 1 upward (lowest non-zero rung first). The first rung
    /// with enough samples and a drop ratio above the threshold yields `Some(i - 1)`.
    pub fn quality_cap(&self) -> Option<usize> {
        self.with_inner(|inner| {
            let threshold = inner.params.dropped_frames_percentage_threshold;
            let min_samples = inner.params.minimum_sample_size;

            let mut qualities: Vec<usize> = inner.by_quality.keys().copied().collect();
            qualities.sort_unstable();

            for quality_index in qualities {
                if quality_index == 0 {
                    continue;
                }
                let Some(stats) = inner.by_quality.get(&quality_index) else {
                    continue;
                };
                if stats.total_video_frames <= min_samples {
                    continue;
                }
                let ratio = stats.dropped_video_frames as f64 / stats.total_video_frames as f64;
                if ratio > threshold {
                    return Some(quality_index - 1);
                }
            }
            None
        })
    }

    /// Rule parameters in use.
    pub fn params(&self) -> DroppedFramesParams {
        self.with_inner(|inner| inner.params)
    }

    fn with_inner<R>(&self, f: impl FnOnce(&DroppedFramesHistoryInner) -> R) -> R {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    fn with_inner_mut<R>(&self, f: impl FnOnce(&mut DroppedFramesHistoryInner) -> R) -> R {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }
}

impl Default for DroppedFramesHistory {
    fn default() -> Self {
        Self::new()
    }
}

/// Apply a dropped-frames quality cap when autoswitch is enabled.
///
/// Fixed quality / disabled autoswitch / data-saver (`forced_quality_index`) take precedence.
pub(crate) fn apply_dropped_frames_cap(
    quality_index: usize,
    ladder_len: usize,
    constraints: &super::QualityConstraints,
    history: Option<&DroppedFramesHistory>,
) -> usize {
    if ladder_len == 0 {
        return 0;
    }
    if super::forced_quality_index(ladder_len, constraints).is_some() {
        return quality_index.min(ladder_len - 1);
    }
    let Some(history) = history else {
        return quality_index.min(ladder_len - 1);
    };
    let capped = match history.quality_cap() {
        Some(cap) => quality_index.min(cap),
        None => quality_index,
    };
    capped.min(ladder_len - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abr::QualityConstraints;

    #[test]
    fn first_sample_is_baseline_only() {
        let history = DroppedFramesHistory::new();
        history.set_active_quality(1);
        history.push(10, 100);
        assert_eq!(history.quality_cap(), None);
    }

    #[test]
    fn attributes_interval_to_active_quality() {
        let history = DroppedFramesHistory::with_params(DroppedFramesParams {
            minimum_sample_size: 10,
            dropped_frames_percentage_threshold: 0.15,
        });
        history.set_active_quality(2);
        history.push(0, 0);
        history.push(20, 100);
        // 20/100 = 0.2 > 0.15, quality 2 → cap at 1
        assert_eq!(history.quality_cap(), Some(1));
    }

    #[test]
    fn skips_quality_index_zero() {
        let history = DroppedFramesHistory::with_params(DroppedFramesParams {
            minimum_sample_size: 10,
            dropped_frames_percentage_threshold: 0.15,
        });
        history.set_active_quality(0);
        history.push(0, 0);
        history.push(50, 100);
        assert_eq!(history.quality_cap(), None);
    }

    #[test]
    fn requires_minimum_sample_size() {
        let history = DroppedFramesHistory::with_params(DroppedFramesParams {
            minimum_sample_size: 375,
            dropped_frames_percentage_threshold: 0.15,
        });
        history.set_active_quality(1);
        history.push(0, 0);
        history.push(50, 200); // below 375
        assert_eq!(history.quality_cap(), None);
        history.push(100, 400); // +50/+200 → total 400, dropped 100 → 0.25
        assert_eq!(history.quality_cap(), Some(0));
    }

    #[test]
    fn picks_lowest_exceeding_quality() {
        let history = DroppedFramesHistory::with_params(DroppedFramesParams {
            minimum_sample_size: 10,
            dropped_frames_percentage_threshold: 0.15,
        });
        history.set_active_quality(1);
        history.push(0, 0);
        history.push(20, 100); // quality 1 bad → cap 0
        history.set_active_quality(3);
        history.push(40, 200); // quality 3 also bad, but rule breaks at first
        assert_eq!(history.quality_cap(), Some(0));
    }

    #[test]
    fn ignores_non_monotonic_counters() {
        let history = DroppedFramesHistory::with_params(DroppedFramesParams {
            minimum_sample_size: 10,
            dropped_frames_percentage_threshold: 0.15,
        });
        history.set_active_quality(1);
        history.push(0, 0);
        history.push(20, 100);
        assert_eq!(history.quality_cap(), Some(0));
        // Counter reset (media reload): re-baseline without clearing prior attribution.
        history.push(0, 0);
        // Cap still reflects accumulated history for quality 1.
        assert_eq!(history.quality_cap(), Some(0));
        // Further good frames dilute the ratio below the threshold.
        history.push(1, 200);
        assert_eq!(history.quality_cap(), None);
    }

    #[test]
    fn apply_cap_respects_forced_quality() {
        let history = DroppedFramesHistory::with_params(DroppedFramesParams {
            minimum_sample_size: 10,
            dropped_frames_percentage_threshold: 0.15,
        });
        history.set_active_quality(2);
        history.push(0, 0);
        history.push(30, 100);

        let constraints = QualityConstraints::default().fixed_quality(2);
        let capped = apply_dropped_frames_cap(2, 3, &constraints, Some(&history));
        assert_eq!(capped, 2);

        let auto = QualityConstraints::default();
        let capped = apply_dropped_frames_cap(2, 3, &auto, Some(&history));
        assert_eq!(capped, 1);
    }

    #[test]
    fn params_sanitized_rejects_invalid() {
        let p = DroppedFramesParams {
            minimum_sample_size: 0,
            dropped_frames_percentage_threshold: 2.0,
        }
        .sanitized();
        assert_eq!(p.minimum_sample_size, DEFAULT_MINIMUM_SAMPLE_SIZE);
        assert!(
            (p.dropped_frames_percentage_threshold - DEFAULT_DROPPED_FRAMES_PERCENTAGE_THRESHOLD)
                .abs()
                < 1e-9
        );
    }
}
