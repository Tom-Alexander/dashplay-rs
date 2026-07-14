//! Dynamic weight selection for the LoL+ SOM distance metric.
//!
//! Ported from dash.js `LoLpWeightSelector`.

use super::qoe::QoeEvaluator;

/// Result of a weight-selection search.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WeightSelection {
    /// Winning weight vector `[throughput, latency, buffer, switch]`.
    Weights([f64; 4]),
    /// No candidate satisfied the latency/buffer constraints.
    ConstraintsNotMet,
}

/// Selects SOM feature weights that maximize predicted QoE under constraints.
#[derive(Debug, Clone)]
pub struct WeightSelector {
    target_latency_s: f64,
    buffer_min_s: f64,
    segment_duration_s: f64,
    weight_options: Vec<[f64; 4]>,
    previous_latency_s: f64,
}

impl WeightSelector {
    /// Build a selector with the LoL+ default discrete weight grid `{0.2,0.4,0.6,0.8,1.0}^4`.
    pub fn new(target_latency_s: f64, buffer_min_s: f64, segment_duration_s: f64) -> Self {
        Self {
            target_latency_s,
            buffer_min_s,
            segment_duration_s,
            weight_options: permutations(&[0.2, 0.4, 0.6, 0.8, 1.0]),
            previous_latency_s: 0.0,
        }
    }

    /// Minimum buffer used by the safety / constraint checks (seconds).
    pub fn min_buffer_s(&self) -> f64 {
        self.buffer_min_s
    }

    /// Projected buffer after downloading `bitrate_bps` given current throughput.
    pub fn next_buffer_with_bitrate(
        &self,
        bitrate_bps: f64,
        current_buffer_s: f64,
        throughput_bps: f64,
    ) -> f64 {
        let download_time = if throughput_bps > 0.0 {
            (bitrate_bps * self.segment_duration_s) / throughput_bps
        } else {
            f64::INFINITY
        };
        self.next_buffer(current_buffer_s, download_time)
    }

    /// Projected buffer after a download that takes `download_time_s`.
    pub fn next_buffer(&self, current_buffer_s: f64, download_time_s: f64) -> f64 {
        if download_time_s > self.segment_duration_s {
            current_buffer_s - self.segment_duration_s
        } else {
            current_buffer_s + self.segment_duration_s - download_time_s
        }
    }

    /// Greedy search over weight permutations for the maximum-QoE feasible weight vector.
    ///
    /// `neurons` is `(bandwidth_bps, neuron_latency_state)` for every SOM neuron.
    pub fn find_weight_vector(
        &mut self,
        neurons: &[(f64, f64)],
        qoe: &QoeEvaluator,
        current_latency_s: f64,
        current_buffer_s: f64,
        current_throughput_bps: f64,
        playback_rate: f64,
    ) -> WeightSelection {
        let mut max_qoe: Option<f64> = None;
        let mut winner: Option<[f64; 4]> = None;
        let delta_latency = (current_latency_s - self.previous_latency_s).abs();

        for &(bandwidth_bps, neuron_latency) in neurons {
            for &weight_vector in &self.weight_options {
                let download_time = if current_throughput_bps > 0.0 {
                    (bandwidth_bps * self.segment_duration_s) / current_throughput_bps
                } else {
                    f64::INFINITY
                };
                let next_buffer = self.next_buffer(current_buffer_s, download_time);
                let rebuffer = (download_time - next_buffer).max(0.00001);

                let buffer_wt = if weight_vector[2] == 0.0 {
                    10.0
                } else {
                    1.0 / weight_vector[2]
                };
                let weighted_rebuffer = buffer_wt * rebuffer;

                let latency_wt = if weight_vector[1] == 0.0 {
                    10.0
                } else {
                    1.0 / weight_vector[1]
                };
                let weighted_latency = latency_wt * neuron_latency;

                let total_qoe = qoe.calculate_single_use_qoe(
                    bandwidth_bps / 1000.0,
                    weighted_rebuffer,
                    weighted_latency,
                    playback_rate,
                );

                if self.check_constraints(current_latency_s, next_buffer, delta_latency)
                    && max_qoe.is_none_or(|m| total_qoe > m)
                {
                    max_qoe = Some(total_qoe);
                    winner = Some(weight_vector);
                }
            }
        }

        self.previous_latency_s = current_latency_s;

        match winner {
            Some(w) => WeightSelection::Weights(w),
            None => WeightSelection::ConstraintsNotMet,
        }
    }

    fn check_constraints(
        &self,
        next_latency_s: f64,
        next_buffer_s: f64,
        delta_latency_s: f64,
    ) -> bool {
        if next_latency_s > self.target_latency_s + delta_latency_s {
            return false;
        }
        next_buffer_s >= self.buffer_min_s
    }
}

fn permutations(values: &[f64]) -> Vec<[f64; 4]> {
    let mut out = Vec::with_capacity(values.len().pow(4));
    for &a in values {
        for &b in values {
            for &c in values {
                for &d in values {
                    out.push([a, b, c, d]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permutation_count_is_625() {
        assert_eq!(permutations(&[0.2, 0.4, 0.6, 0.8, 1.0]).len(), 625);
    }

    #[test]
    fn next_buffer_grows_when_download_faster_than_segment() {
        let sel = WeightSelector::new(1.5, 0.3, 2.0);
        let next = sel.next_buffer(1.0, 0.5);
        assert!((next - 2.5).abs() < 1e-9);
    }

    #[test]
    fn find_weights_returns_vector_with_capacity() {
        let mut sel = WeightSelector::new(1.5, 0.3, 2.0);
        let mut qoe = QoeEvaluator::new();
        qoe.setup_per_segment(2.0, 5000.0, 300.0);
        let neurons = [(300_000.0, 0.0), (1_000_000.0, 0.0)];
        let result = sel.find_weight_vector(&neurons, &qoe, 1.0, 2.0, 5_000_000.0, 1.0);
        assert!(matches!(result, WeightSelection::Weights(_)));
    }
}
