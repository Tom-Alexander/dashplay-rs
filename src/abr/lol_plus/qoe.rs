//! QoE evaluation for LoL+ weight selection.
//!
//! Ported from dash.js `LoLpQoEEvaluator` / `QoeInfo`
//! (Bentaleb et al., LoL+).

/// Thresholded latency penalty used by the QoE model.
#[derive(Debug, Clone, Copy)]
struct LatencyPenalty {
    threshold_s: f64,
    penalty: f64,
}

/// Weights applied to each QoE factor for a segment-duration window.
#[derive(Debug, Clone)]
struct QoeWeights {
    bitrate_reward: f64,
    bitrate_switch_penalty: f64,
    rebuffer_penalty: f64,
    latency_penalty: Vec<LatencyPenalty>,
    playback_speed_penalty: f64,
}

/// Accumulated weighted-sum QoE for one evaluation window.
#[derive(Debug, Clone)]
struct QoeInfo {
    weights: QoeWeights,
    last_bitrate_kbps: Option<f64>,
    bitrate_wsum: f64,
    bitrate_switch_wsum: f64,
    rebuffer_wsum: f64,
    latency_wsum: f64,
    playback_speed_wsum: f64,
    total_qoe: f64,
}

impl QoeInfo {
    fn new(segment_duration_s: f64, max_bitrate_kbps: f64, min_bitrate_kbps: f64) -> Self {
        let bitrate_reward = if segment_duration_s > 0.0 {
            segment_duration_s
        } else {
            1.0
        };
        let rebuffer_penalty = if max_bitrate_kbps > 0.0 {
            max_bitrate_kbps
        } else {
            1000.0
        };
        let playback_speed_penalty = if min_bitrate_kbps > 0.0 {
            min_bitrate_kbps
        } else {
            200.0
        };

        Self {
            weights: QoeWeights {
                bitrate_reward,
                bitrate_switch_penalty: 1.0,
                rebuffer_penalty,
                latency_penalty: vec![
                    LatencyPenalty {
                        threshold_s: 1.1,
                        penalty: min_bitrate_kbps.max(0.0) * 0.05,
                    },
                    LatencyPenalty {
                        threshold_s: 100_000_000.0,
                        penalty: max_bitrate_kbps.max(0.0) * 0.1,
                    },
                ],
                playback_speed_penalty,
            },
            last_bitrate_kbps: None,
            bitrate_wsum: 0.0,
            bitrate_switch_wsum: 0.0,
            rebuffer_wsum: 0.0,
            latency_wsum: 0.0,
            playback_speed_wsum: 0.0,
            total_qoe: 0.0,
        }
    }

    fn log_metrics(
        &mut self,
        bitrate_kbps: f64,
        rebuffer_s: f64,
        latency_s: f64,
        playback_rate: f64,
    ) {
        self.bitrate_wsum += self.weights.bitrate_reward * bitrate_kbps;

        if let Some(last) = self.last_bitrate_kbps {
            self.bitrate_switch_wsum +=
                self.weights.bitrate_switch_penalty * (bitrate_kbps - last).abs();
        }
        self.last_bitrate_kbps = Some(bitrate_kbps);

        self.rebuffer_wsum += self.weights.rebuffer_penalty * rebuffer_s;

        for range in &self.weights.latency_penalty {
            if latency_s <= range.threshold_s {
                self.latency_wsum += range.penalty * latency_s;
                break;
            }
        }

        self.playback_speed_wsum +=
            self.weights.playback_speed_penalty * (1.0 - playback_rate).abs();

        self.total_qoe = self.bitrate_wsum
            - self.bitrate_switch_wsum
            - self.rebuffer_wsum
            - self.latency_wsum
            - self.playback_speed_wsum;
    }
}

/// Per-session QoE evaluator used by the LoL+ dynamic weight selector.
#[derive(Debug, Clone)]
pub struct QoeEvaluator {
    segment_duration_s: Option<f64>,
    max_bitrate_kbps: Option<f64>,
    min_bitrate_kbps: Option<f64>,
    per_segment: Option<QoeInfo>,
}

impl QoeEvaluator {
    /// Create an uneconfigured evaluator.
    pub fn new() -> Self {
        Self {
            segment_duration_s: None,
            max_bitrate_kbps: None,
            min_bitrate_kbps: None,
            per_segment: None,
        }
    }

    /// Configure per-segment QoE weights from the quality ladder extrema.
    pub fn setup_per_segment(
        &mut self,
        segment_duration_s: f64,
        max_bitrate_kbps: f64,
        min_bitrate_kbps: f64,
    ) {
        self.segment_duration_s = Some(segment_duration_s);
        self.max_bitrate_kbps = Some(max_bitrate_kbps);
        self.min_bitrate_kbps = Some(min_bitrate_kbps);
        self.per_segment = Some(QoeInfo::new(
            segment_duration_s,
            max_bitrate_kbps,
            min_bitrate_kbps,
        ));
    }

    /// Accumulate metrics for the session-level QoE (optional observability).
    pub fn log_segment_metrics(
        &mut self,
        bitrate_kbps: f64,
        rebuffer_s: f64,
        latency_s: f64,
        playback_rate: f64,
    ) {
        if let Some(info) = self.per_segment.as_mut() {
            info.log_metrics(bitrate_kbps, rebuffer_s, latency_s, playback_rate);
        }
    }

    /// One-shot QoE used by the weight selector (does not mutate session state).
    pub fn calculate_single_use_qoe(
        &self,
        bitrate_kbps: f64,
        rebuffer_s: f64,
        latency_s: f64,
        playback_rate: f64,
    ) -> f64 {
        let (Some(seg_dur), Some(max_br), Some(min_br)) = (
            self.segment_duration_s,
            self.max_bitrate_kbps,
            self.min_bitrate_kbps,
        ) else {
            return 0.0;
        };

        let mut info = QoeInfo::new(seg_dur, max_br, min_br);
        info.log_metrics(bitrate_kbps, rebuffer_s, latency_s, playback_rate);
        info.total_qoe
    }
}

impl Default for QoeEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_bitrate_improves_single_use_qoe() {
        let mut eval = QoeEvaluator::new();
        eval.setup_per_segment(2.0, 5000.0, 300.0);
        let low = eval.calculate_single_use_qoe(300.0, 0.0, 1.0, 1.0);
        let high = eval.calculate_single_use_qoe(5000.0, 0.0, 1.0, 1.0);
        assert!(high > low);
    }

    #[test]
    fn rebuffer_reduces_qoe() {
        let mut eval = QoeEvaluator::new();
        eval.setup_per_segment(2.0, 5000.0, 300.0);
        let clean = eval.calculate_single_use_qoe(2000.0, 0.0, 1.0, 1.0);
        let stalled = eval.calculate_single_use_qoe(2000.0, 1.0, 1.0, 1.0);
        assert!(clean > stalled);
    }
}
