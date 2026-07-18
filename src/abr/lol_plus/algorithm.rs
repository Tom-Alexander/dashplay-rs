//! LoL+: Low-on-Latency-plus SOM adaptive bitrate algorithm.
//!
//! Based on: "Catching the Moment With LoL⁺ in Twitch-Like Low-Latency Live
//! Streaming Platforms" (Lim, Akcay, Bentaleb, Begen, Zimmermann) and the
//! dash.js `LearningAbrController` / `LoLpRule` reference implementation.

use super::qoe::QoeEvaluator;
use super::weight_selector::{WeightSelection, WeightSelector};

/// SOM target latency (normalized units).
const TARGET_LATENCY: f64 = 0.0;
/// SOM target rebuffer level.
const TARGET_REBUFFER: f64 = 0.0;
/// Throughput margin before encouraging a downshift (bps).
const THROUGHPUT_DELTA_BPS: f64 = 10_000.0;
/// SOM neighbour radius σ.
const NEIGHBOUR_SIGMA: f64 = 0.1;
/// Per-feature learning rates.
const LEARNING_RATES: [f64; 4] = [0.01, 0.01, 0.01, 0.01];
/// Default latency normalization (seconds → unitless).
const DEFAULT_LATENCY_NORM: f64 = 100.0;

/// One entry on the bitrate ladder (bandwidth-ordered).
#[derive(Debug, Clone)]
pub struct QualityLevel {
    /// Human-readable label (e.g. Representation `@id`).
    pub label: String,
    /// Nominal bitrate in bits per second.
    pub bitrate_bps: f64,
}

/// Decision returned by [`LolPlus::decide`].
#[derive(Debug, Clone)]
pub struct LolPlusDecision {
    /// Index into the quality ladder (0 = lowest bitrate).
    pub quality_index: usize,
    /// Nominal bitrate of the chosen quality (bps).
    pub bitrate_bps: f64,
    /// True when the algorithm forced a safety downshift (low buffer).
    pub is_safety_downshift: bool,
}

#[derive(Debug, Clone)]
struct NeuronState {
    throughput: f64,
    latency: f64,
    rebuffer: f64,
    switch: f64,
}

#[derive(Debug, Clone)]
struct Neuron {
    bitrate_bps: f64,
    state: NeuronState,
}

/// Stateful LoL+ controller for one adaptation set.
#[derive(Debug)]
pub struct LolPlus {
    qualities: Vec<QualityLevel>,
    neurons: Vec<Neuron>,
    bitrate_normalization: f64,
    latency_normalization: f64,
    min_bitrate_bps: f64,
    weights: [f64; 4],
    weight_selector: WeightSelector,
    qoe: QoeEvaluator,
    buffer_s: f64,
    latency_s: f64,
    playback_rate: f64,
    throughput_ewma_bps: f64,
    ewma_alpha: f64,
    current_quality_index: usize,
    has_throughput_sample: bool,
    segment_duration_s: f64,
}

impl LolPlus {
    /// Build a LoL+ instance from a quality ladder.
    ///
    /// `qualities` must be non-empty. Entries are sorted lowest→highest bitrate.
    /// `rng_seed` seeds the k-means++ centre initialization for reproducible tests.
    pub fn new(
        mut qualities: Vec<QualityLevel>,
        segment_duration_s: f64,
        target_latency_s: f64,
        buffer_min_s: f64,
        ewma_alpha: f64,
        rng_seed: u64,
    ) -> Self {
        assert!(
            !qualities.is_empty(),
            "LoL+ needs at least one quality level"
        );
        assert!(
            (0.0..=1.0).contains(&ewma_alpha),
            "ewma_alpha must be in (0,1]"
        );
        assert!(
            segment_duration_s > 0.0,
            "segment_duration_s must be positive"
        );

        qualities.sort_by(|a, b| a.bitrate_bps.total_cmp(&b.bitrate_bps));

        let bitrates: Vec<f64> = qualities.iter().map(|q| q.bitrate_bps).collect();
        let min_bitrate_bps = bitrates[0];
        let max_bitrate_bps = bitrates[bitrates.len() - 1];
        let bitrate_normalization = magnitude(&bitrates).max(1.0);

        let neurons: Vec<Neuron> = qualities
            .iter()
            .map(|q| Neuron {
                bitrate_bps: q.bitrate_bps,
                state: NeuronState {
                    throughput: q.bitrate_bps / bitrate_normalization,
                    latency: 0.0,
                    rebuffer: 0.0,
                    switch: 0.0,
                },
            })
            .collect();

        let mut rng = Lcg::new(rng_seed);
        let sorted_centers = initial_kmeans_plusplus_centers(&neurons, &mut rng);
        let weights = *sorted_centers
            .last()
            .expect("k-means++ produces at least one centre");

        let mut qoe = QoeEvaluator::new();
        qoe.setup_per_segment(
            segment_duration_s,
            max_bitrate_bps / 1000.0,
            min_bitrate_bps / 1000.0,
        );

        Self {
            qualities,
            neurons,
            bitrate_normalization,
            latency_normalization: DEFAULT_LATENCY_NORM,
            min_bitrate_bps,
            weights,
            weight_selector: WeightSelector::new(
                target_latency_s,
                buffer_min_s,
                segment_duration_s,
            ),
            qoe,
            buffer_s: 0.0,
            latency_s: 0.0,
            playback_rate: 1.0,
            throughput_ewma_bps: 0.0,
            ewma_alpha,
            current_quality_index: 0,
            has_throughput_sample: false,
            segment_duration_s,
        }
    }

    /// Update consumer-reported buffer occupancy (seconds).
    pub fn update_buffer(&mut self, buffer_s: f64) {
        self.buffer_s = buffer_s.max(0.0);
    }

    /// Update measured live latency (seconds). `0.0` when unknown / VOD.
    pub fn update_latency(&mut self, latency_s: f64) {
        self.latency_s = latency_s.max(0.0);
    }

    /// Update suggested / actual playback rate (1.0 = realtime).
    pub fn update_playback_rate(&mut self, rate: f64) {
        self.playback_rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
    }

    /// Update throughput after a completed segment download.
    pub fn observe_throughput(&mut self, throughput_bps: f64) {
        if !throughput_bps.is_finite() || throughput_bps <= 0.0 {
            return;
        }
        if self.throughput_ewma_bps == 0.0 {
            self.throughput_ewma_bps = throughput_bps;
        } else {
            self.throughput_ewma_bps = self.ewma_alpha * throughput_bps
                + (1.0 - self.ewma_alpha) * self.throughput_ewma_bps;
        }
        self.has_throughput_sample = true;
    }

    /// Observe a download, ignoring unrepresentative tiny samples.
    pub fn observe_segment_download(
        &mut self,
        throughput_bps: f64,
        downloaded_bytes: usize,
        quality_index: usize,
    ) {
        let estimated = self.estimated_segment_bytes_for_quality(quality_index);
        const MIN_SAMPLE_FRACTION: f64 = 0.25;
        if estimated > 0.0 && (downloaded_bytes as f64) < estimated * MIN_SAMPLE_FRACTION {
            return;
        }

        let download_time_s = if throughput_bps > 0.0 {
            (downloaded_bytes as f64 * 8.0) / throughput_bps
        } else {
            0.0
        };
        let segment_rebuffer = if download_time_s > self.segment_duration_s {
            download_time_s - self.segment_duration_s
        } else {
            0.0
        };

        let bitrate_kbps = self.qualities[quality_index].bitrate_bps / 1000.0;
        self.qoe.log_segment_metrics(
            bitrate_kbps,
            segment_rebuffer,
            self.latency_s,
            self.playback_rate,
        );

        self.observe_throughput(throughput_bps);
        self.current_quality_index = quality_index;
    }

    /// Nominal segment size (bytes) at a quality index.
    pub fn estimated_segment_bytes_for_quality(&self, quality_index: usize) -> f64 {
        self.qualities[quality_index].bitrate_bps * self.segment_duration_s / 8.0
    }

    /// Choose the quality for the next segment (mutates SOM state).
    pub fn decide(&mut self) -> LolPlusDecision {
        if self.qualities.len() == 1 {
            return LolPlusDecision {
                quality_index: 0,
                bitrate_bps: self.qualities[0].bitrate_bps,
                is_safety_downshift: false,
            };
        }

        let current_throughput = if self.throughput_ewma_bps > 0.0 {
            self.throughput_ewma_bps
        } else {
            // Cold start: assume enough bandwidth for mid ladder.
            self.qualities[self.qualities.len() / 2].bitrate_bps
        };

        let mut throughput_normalized = current_throughput / self.bitrate_normalization;
        if throughput_normalized > 1.0 {
            throughput_normalized = self.max_neuron_throughput();
        }

        let latency_normalized = self.latency_s / self.latency_normalization;
        let current_idx = self.current_quality_index.min(self.neurons.len() - 1);
        let current_bitrate = self.neurons[current_idx].bitrate_bps;

        let download_time =
            (current_bitrate * self.segment_duration_s) / current_throughput.max(1.0);
        let rebuffer = (download_time - self.buffer_s).max(0.0);

        // Safety: buffer would go critically low at the current bitrate.
        if self.buffer_s - download_time < self.weight_selector.min_buffer_s() {
            let down = self.downshift_neuron(current_idx, current_throughput);
            self.current_quality_index = down;
            return LolPlusDecision {
                quality_index: down,
                bitrate_bps: self.qualities[down].bitrate_bps,
                is_safety_downshift: true,
            };
        }

        self.select_dynamic_weights(current_throughput);

        let mut min_distance = f64::INFINITY;
        let mut winner_idx = current_idx;

        for (i, neuron) in self.neurons.iter().enumerate() {
            let som_data = [
                neuron.state.throughput,
                neuron.state.latency,
                neuron.state.rebuffer,
                neuron.state.switch,
            ];
            let mut distance_weights = self.weights;

            let next_buffer = self.weight_selector.next_buffer_with_bitrate(
                neuron.bitrate_bps,
                self.buffer_s,
                current_throughput,
            );
            let is_buffer_low = next_buffer < self.weight_selector.min_buffer_s();

            if (neuron.bitrate_bps > current_throughput - THROUGHPUT_DELTA_BPS || is_buffer_low)
                && (neuron.bitrate_bps - self.min_bitrate_bps).abs() > 1.0
            {
                distance_weights[0] = 100.0;
            }

            let distance = weighted_distance(
                &som_data,
                &[throughput_normalized, TARGET_LATENCY, TARGET_REBUFFER, 0.0],
                &distance_weights,
            );
            if distance < min_distance {
                min_distance = distance;
                winner_idx = i;
            }
        }

        let winner_bitrate = self.neurons[winner_idx].bitrate_bps;
        let bitrate_switch = (current_bitrate - winner_bitrate).abs() / self.bitrate_normalization;

        // Punish / retrain around the previously selected neuron.
        self.update_neurons(
            current_idx,
            [
                throughput_normalized,
                latency_normalized,
                rebuffer,
                bitrate_switch,
            ],
        );
        // Reinforce BMU toward the ideal target.
        self.update_neurons(
            winner_idx,
            [
                throughput_normalized,
                TARGET_LATENCY,
                TARGET_REBUFFER,
                bitrate_switch,
            ],
        );

        self.current_quality_index = winner_idx;
        LolPlusDecision {
            quality_index: winner_idx,
            bitrate_bps: winner_bitrate,
            is_safety_downshift: false,
        }
    }

    /// Current buffer (seconds).
    pub fn buffer_s(&self) -> f64 {
        self.buffer_s
    }

    /// Current throughput EWMA (bps).
    pub fn throughput_bps(&self) -> f64 {
        self.throughput_ewma_bps
    }

    /// Whether at least one throughput sample has been observed.
    pub fn has_throughput_sample(&self) -> bool {
        self.has_throughput_sample
    }

    /// Quality ladder.
    pub fn qualities(&self) -> &[QualityLevel] {
        &self.qualities
    }

    fn select_dynamic_weights(&mut self, current_throughput: f64) {
        let neuron_snapshot: Vec<(f64, f64)> = self
            .neurons
            .iter()
            .map(|n| (n.bitrate_bps, n.state.latency))
            .collect();

        match self.weight_selector.find_weight_vector(
            &neuron_snapshot,
            &self.qoe,
            self.latency_s,
            self.buffer_s,
            current_throughput,
            self.playback_rate,
        ) {
            WeightSelection::Weights(w) => self.weights = w,
            WeightSelection::ConstraintsNotMet => {
                // Keep previous weights (dash.js uses -1 sentinel).
            }
        }
    }

    fn max_neuron_throughput(&self) -> f64 {
        self.neurons
            .iter()
            .map(|n| n.state.throughput)
            .fold(0.0_f64, f64::max)
    }

    fn downshift_neuron(&self, current_idx: usize, current_throughput: f64) -> usize {
        let current_bitrate = self.neurons[current_idx].bitrate_bps;
        let mut best = current_idx;
        let mut max_suitable = 0.0_f64;
        for (i, n) in self.neurons.iter().enumerate() {
            if n.bitrate_bps < current_bitrate
                && n.bitrate_bps > max_suitable
                && current_throughput > n.bitrate_bps
            {
                max_suitable = n.bitrate_bps;
                best = i;
            }
        }
        best
    }

    fn update_neurons(&mut self, winner_idx: usize, x: [f64; 4]) {
        let winner_state = self.neurons[winner_idx].state.clone();
        for neuron in &mut self.neurons {
            let distance = weighted_distance(
                &[
                    neuron.state.throughput,
                    neuron.state.latency,
                    neuron.state.rebuffer,
                    neuron.state.switch,
                ],
                &[
                    winner_state.throughput,
                    winner_state.latency,
                    winner_state.rebuffer,
                    winner_state.switch,
                ],
                &[1.0, 1.0, 1.0, 1.0],
            );
            let neighbourhood = (-distance.powi(2) / (2.0 * NEIGHBOUR_SIGMA.powi(2))).exp();
            neuron.state.throughput +=
                (x[0] - neuron.state.throughput) * LEARNING_RATES[0] * neighbourhood;
            neuron.state.latency +=
                (x[1] - neuron.state.latency) * LEARNING_RATES[1] * neighbourhood;
            neuron.state.rebuffer +=
                (x[2] - neuron.state.rebuffer) * LEARNING_RATES[2] * neighbourhood;
            neuron.state.switch += (x[3] - neuron.state.switch) * LEARNING_RATES[3] * neighbourhood;
        }
    }
}

fn magnitude(values: &[f64]) -> f64 {
    values.iter().map(|x| x * x).sum::<f64>().sqrt()
}

fn weighted_distance(a: &[f64; 4], b: &[f64; 4], w: &[f64; 4]) -> f64 {
    let sum: f64 = (0..4).map(|i| w[i] * (a[i] - b[i]).powi(2)).sum();
    let sign = if sum < 0.0 { -1.0 } else { 1.0 };
    sign * sum.abs().sqrt()
}

fn initial_kmeans_plusplus_centers(neurons: &[Neuron], rng: &mut Lcg) -> Vec<[f64; 4]> {
    let n = neurons.len();
    let max_tp = neurons
        .iter()
        .map(|neu| neu.state.throughput)
        .fold(0.0_f64, f64::max)
        .max(1.0);

    let mut random_data: Vec<[f64; 4]> = Vec::with_capacity(n * n);
    for _ in 0..(n * n) {
        random_data.push([
            rng.next_f64() * max_tp,
            rng.next_f64(),
            rng.next_f64(),
            rng.next_f64(),
        ]);
    }

    let unit = [1.0, 1.0, 1.0, 1.0];
    let mut centers: Vec<[f64; 4]> = Vec::with_capacity(n);
    centers.push(random_data[0]);

    for _ in 1..n {
        let mut next_point = random_data[0];
        let mut max_distance = f64::NEG_INFINITY;
        for point in &random_data {
            let min_distance = centers
                .iter()
                .map(|c| weighted_distance(point, c, &unit))
                .fold(f64::INFINITY, f64::min);
            if min_distance > max_distance {
                max_distance = min_distance;
                next_point = *point;
            }
        }
        centers.push(next_point);
    }

    // Find the least similar centre (max sum of pairwise distances).
    let mut least_similar = 0usize;
    let mut max_sum = f64::NEG_INFINITY;
    for i in 0..centers.len() {
        let mut distance = 0.0;
        for (j, c) in centers.iter().enumerate() {
            if i == j {
                continue;
            }
            distance += weighted_distance(&centers[i], c, &unit);
        }
        if distance > max_sum {
            max_sum = distance;
            least_similar = i;
        }
    }

    let mut sorted = Vec::with_capacity(centers.len());
    sorted.push(centers.remove(least_similar));
    while !centers.is_empty() {
        let mut min_idx = 0usize;
        let mut min_distance = f64::INFINITY;
        for (i, c) in centers.iter().enumerate() {
            let d = weighted_distance(&sorted[0], c, &unit);
            if d < min_distance {
                min_distance = d;
                min_idx = i;
            }
        }
        sorted.push(centers.remove(min_idx));
    }
    sorted
}

/// Tiny deterministic LCG (no `rand` dependency).
#[derive(Debug, Clone)]
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self {
            state: seed | 1, // avoid zero lock
        }
    }

    fn next_u64(&mut self) -> u64 {
        // Numerical Recipes LCG
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.state
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder() -> Vec<QualityLevel> {
        vec![
            QualityLevel {
                label: "240p".into(),
                bitrate_bps: 300_000.0,
            },
            QualityLevel {
                label: "360p".into(),
                bitrate_bps: 750_000.0,
            },
            QualityLevel {
                label: "480p".into(),
                bitrate_bps: 1_200_000.0,
            },
            QualityLevel {
                label: "720p".into(),
                bitrate_bps: 2_500_000.0,
            },
            QualityLevel {
                label: "1080p".into(),
                bitrate_bps: 5_000_000.0,
            },
        ]
    }

    fn make() -> LolPlus {
        LolPlus::new(ladder(), 2.0, 1.5, 0.3, 0.3, 42)
    }

    #[test]
    fn safety_downshift_when_buffer_too_low_for_current() {
        let mut lol = make();
        lol.observe_throughput(20_000_000.0);
        lol.current_quality_index = 4;
        lol.update_buffer(0.1);
        let d = lol.decide();
        assert!(d.is_safety_downshift);
        assert!(d.quality_index < 4);
    }

    #[test]
    fn scarce_bandwidth_prefers_lower_rungs() {
        let mut lol = make();
        lol.observe_throughput(400_000.0);
        lol.update_buffer(5.0);
        let d = lol.decide();
        assert!(d.quality_index <= 1);
    }

    #[test]
    fn ample_bandwidth_and_buffer_allows_high_quality() {
        let mut lol = make();
        lol.observe_throughput(20_000_000.0);
        lol.update_buffer(8.0);
        lol.update_latency(1.0);
        // Several decisions so SOM can converge from the mid cold start.
        let mut last = 0usize;
        for _ in 0..8 {
            last = lol.decide().quality_index;
        }
        assert!(last >= 2, "expected mid/high quality, got {last}");
    }

    #[test]
    fn single_quality_always_index_zero() {
        let mut lol = LolPlus::new(
            vec![QualityLevel {
                label: "only".into(),
                bitrate_bps: 1_000_000.0,
            }],
            2.0,
            1.5,
            0.3,
            0.3,
            1,
        );
        lol.update_buffer(0.0);
        let d = lol.decide();
        assert_eq!(d.quality_index, 0);
        assert!(!d.is_safety_downshift);
    }

    #[test]
    fn ignores_tiny_throughput_samples() {
        let mut lol = make();
        let estimated = lol.estimated_segment_bytes_for_quality(4);
        lol.observe_segment_download(100_000.0, 24, 4);
        assert_eq!(lol.throughput_bps(), 0.0);
        lol.observe_segment_download(100_000.0, estimated as usize, 4);
        assert_eq!(lol.throughput_bps(), 100_000.0);
    }

    #[test]
    fn decide_is_deterministic_with_fixed_seed() {
        let mut a = make();
        let mut b = make();
        a.observe_throughput(5_000_000.0);
        b.observe_throughput(5_000_000.0);
        a.update_buffer(3.0);
        b.update_buffer(3.0);
        for _ in 0..5 {
            assert_eq!(a.decide().quality_index, b.decide().quality_index);
        }
    }
}
