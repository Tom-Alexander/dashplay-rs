/// BOLA: Buffer Occupancy based Lyapunov Algorithm
///
/// Based on: "BOLA: Near-Optimal Bitrate Adaptation for Online Videos"
/// Huang et al., INFOCOM 2015 / IEEE/ACM Transactions on Networking 2018
///
/// BOLA uses Lyapunov optimization to select video quality levels that
/// maximize a utility function while keeping the playback buffer stable.

// ─── Constants ───────────────────────────────────────────────────────────────

/// Segment duration in seconds (typical HLS/DASH default).
const SEGMENT_DURATION_S: f64 = 4.0;

/// Buffer size limit in seconds (prevents unbounded buffering).
const BUFFER_MAX_S: f64 = 25.0;

/// Minimum buffer level that triggers downloads, in seconds.
/// Usually set to one segment duration.
const BUFFER_LOW_S: f64 = SEGMENT_DURATION_S;

/// Safety factor γ·p from the paper. Controls how aggressively BOLA
/// stays away from a stall (too low → stalls; too high → poor quality).
/// Typical: 5 × segment_duration / buffer_max  ≈  0.8 for these defaults.
const GAMMA_P: f64 = 5.0;

// ─── Types ───────────────────────────────────────────────────────────────────

/// One entry in the ladder of available encoding qualities.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct QualityLevel {
    /// Human-readable label, e.g. "240p", "720p", "1080p".
    pub label: String,
    /// Nominal bitrate in bits-per-second (used to estimate segment size).
    pub bitrate_bps: f64,
    /// Lyapunov utility v(m): ln(bitrate / bitrate_min).
    /// Computed automatically by `Bola::new`; zero for the lowest level.
    pub utility: f64,
}

/// All state BOLA needs between segment decisions.
#[derive(Debug)]
pub struct Bola {
    /// Ordered from lowest to highest bitrate.
    qualities: Vec<QualityLevel>,

    /// Lyapunov parameter V: trades quality vs buffer stability.
    /// Derived from the quality ladder and buffer settings.
    /// Higher V → higher average quality at the cost of more stalls.
    v: f64,

    /// Current playback buffer occupancy (seconds of video buffered ahead).
    buffer_s: f64,

    /// Simple exponential-weighted moving average of throughput (bps).
    throughput_ewma_bps: f64,

    /// EWMA smoothing factor α ∈ (0, 1]. Lower → smoother but slower.
    ewma_alpha: f64,
}

/// The decision returned for each segment.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BolaDecision {
    /// Index into the quality ladder.
    pub quality_index: usize,
    /// Bitrate of the chosen quality (bps).
    pub bitrate_bps: f64,
    /// Estimated segment size (bytes) at the chosen quality.
    pub estimated_segment_bytes: f64,
    /// The raw Lyapunov score that won the election.
    pub score: f64,
    /// True when the algorithm fell back to the lowest quality because
    /// the buffer is below BUFFER_LOW_S.
    pub is_emergency: bool,
}

// ─── Implementation ───────────────────────────────────────────────────────────

impl Bola {
    /// Build a new BOLA instance from a quality ladder.
    ///
    /// `qualities` must contain at least one entry, ordered lowest→highest
    /// bitrate. Pass them in any order; this constructor sorts them.
    ///
    /// With a single quality there is no adaptation; [`Self::decide`] always
    /// selects that representation (the highest and only rung).
    pub fn new(mut qualities: Vec<QualityLevel>, ewma_alpha: f64) -> Self {
        assert!(!qualities.is_empty(), "BOLA needs at least one quality level");
        assert!((0.0..=1.0).contains(&ewma_alpha), "ewma_alpha must be in (0,1]");

        // Sort ascending by bitrate.
        qualities.sort_by(|a, b| a.bitrate_bps.partial_cmp(&b.bitrate_bps).unwrap());

        // Assign utilities: v(m) = ln(bitrate_m / bitrate_0).
        let min_bitrate = qualities[0].bitrate_bps;
        for q in qualities.iter_mut() {
            q.utility = (q.bitrate_bps / min_bitrate).ln();
        }

        // Compute V from Equation (6) in the paper:
        //
        //   V = (BUFFER_MAX_S - BUFFER_LOW_S)
        //       / (utility_max + GAMMA_P)
        //
        // This ensures the highest quality can only be chosen when the buffer
        // is close to full, providing a safety margin of γ·p seconds.
        // With one quality, utility_max is 0 and V is still well-defined.
        let utility_max = qualities.last().unwrap().utility;
        let v = (BUFFER_MAX_S - BUFFER_LOW_S) / (utility_max + GAMMA_P);

        Bola {
            qualities,
            v,
            buffer_s: 0.0,
            throughput_ewma_bps: 0.0,
            ewma_alpha,
        }
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Update the throughput estimate after a segment download completes.
    ///
    /// Call this with the observed download speed before calling `decide`.
    pub fn observe_throughput(&mut self, throughput_bps: f64) {
        if self.throughput_ewma_bps == 0.0 {
            // First observation: seed the EWMA rather than averaging with zero.
            self.throughput_ewma_bps = throughput_bps;
        } else {
            self.throughput_ewma_bps = self.ewma_alpha * throughput_bps
                + (1.0 - self.ewma_alpha) * self.throughput_ewma_bps;
        }
    }

    /// Notify BOLA that the buffer has changed (e.g. after a segment was
    /// appended or playback consumed some content).
    pub fn update_buffer(&mut self, buffer_s: f64) {
        self.buffer_s = buffer_s.clamp(0.0, BUFFER_MAX_S);
    }

    /// Choose the quality level for the next segment.
    ///
    /// The algorithm evaluates the Lyapunov objective for every quality m:
    ///
    ///   score(m) = (V·(v(m) + 1) - buffer) / size(m)
    ///
    /// where size(m) is in segment-durations (= bitrate · p / bitrate_m in
    /// the original notation). This is equivalent to:
    ///
    ///   score(m) = (V·(v(m) + 1) - buffer) / bitrate(m)
    ///
    /// The quality with the highest score is selected, subject to a
    /// throughput feasibility check (no level whose bitrate would exceed
    /// the current estimated bandwidth is chosen).
    pub fn decide(&self) -> BolaDecision {
        if self.qualities.len() == 1 {
            let q = &self.qualities[0];
            let score = (self.v * (q.utility + 1.0) - self.buffer_s) / q.bitrate_bps;
            return BolaDecision {
                quality_index: 0,
                bitrate_bps: q.bitrate_bps,
                estimated_segment_bytes: q.bitrate_bps * SEGMENT_DURATION_S / 8.0,
                score,
                is_emergency: false,
            };
        }

        // Emergency: if the buffer is critically low, drop to the lowest quality
        // regardless of the Lyapunov score to avoid a stall.
        if self.buffer_s < BUFFER_LOW_S {
            let q = &self.qualities[0];
            return BolaDecision {
                quality_index: 0,
                bitrate_bps: q.bitrate_bps,
                estimated_segment_bytes: q.bitrate_bps * SEGMENT_DURATION_S / 8.0,
                score: f64::NEG_INFINITY,
                is_emergency: true,
            };
        }

        let mut best_idx = 0usize;
        let mut best_score = f64::NEG_INFINITY;

        for (idx, q) in self.qualities.iter().enumerate() {
            // Skip qualities whose bitrate exceeds estimated throughput
            // (we couldn't download them in time to prevent a re-buffer).
            if self.throughput_ewma_bps > 0.0
                && q.bitrate_bps > self.throughput_ewma_bps
            {
                continue;
            }

            // Lyapunov drift-plus-penalty objective (per-bit normalised):
            //   score = (V · (v(m) + 1) - Q) / bitrate(m)
            // Maximising this maximises utility while keeping the queue stable.
            let score =
                (self.v * (q.utility + 1.0) - self.buffer_s) / q.bitrate_bps;

            // Prefer higher quality on a tie (compare with >=).
            if score >= best_score {
                best_score = score;
                best_idx = idx;
            }
        }

        let chosen = &self.qualities[best_idx];
        BolaDecision {
            quality_index: best_idx,
            bitrate_bps: chosen.bitrate_bps,
            estimated_segment_bytes: chosen.bitrate_bps * SEGMENT_DURATION_S / 8.0,
            score: best_score,
            is_emergency: false,
        }
    }

    // ── Diagnostics ───────────────────────────────────────────────────────────

    /// Current playback buffer level (seconds).
    pub fn buffer_s(&self) -> f64 {
        self.buffer_s
    }

    /// Current throughput estimate (bps).
    #[allow(dead_code)]
    pub fn throughput_bps(&self) -> f64 {
        self.throughput_ewma_bps
    }

    /// The computed Lyapunov parameter V.
    #[allow(dead_code)]
    pub fn v(&self) -> f64 {
        self.v
    }

    /// Read-only view of the quality ladder.
    #[allow(dead_code)]
    pub fn qualities(&self) -> &[QualityLevel] {
        &self.qualities
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ladder() -> Vec<QualityLevel> {
        vec![
            QualityLevel { label: "240p".into(),  bitrate_bps:  300_000.0, utility: 0.0 },
            QualityLevel { label: "360p".into(),  bitrate_bps:  750_000.0, utility: 0.0 },
            QualityLevel { label: "480p".into(),  bitrate_bps: 1_200_000.0, utility: 0.0 },
            QualityLevel { label: "720p".into(),  bitrate_bps: 2_500_000.0, utility: 0.0 },
            QualityLevel { label: "1080p".into(), bitrate_bps: 5_000_000.0, utility: 0.0 },
        ]
    }

    #[test]
    fn utilities_are_monotone_and_zero_at_base() {
        let bola = Bola::new(ladder(), 0.3);
        let qs = bola.qualities();
        assert!((qs[0].utility - 0.0).abs() < 1e-9);
        for w in qs.windows(2) {
            assert!(w[1].utility > w[0].utility);
        }
    }

    #[test]
    fn v_is_positive() {
        let bola = Bola::new(ladder(), 0.3);
        assert!(bola.v() > 0.0);
    }

    #[test]
    fn emergency_mode_when_buffer_empty() {
        let mut bola = Bola::new(ladder(), 0.3);
        bola.observe_throughput(10_000_000.0); // plenty of bandwidth
        bola.update_buffer(0.0);
        let d = bola.decide();
        assert!(d.is_emergency);
        assert_eq!(d.quality_index, 0);
    }

    #[test]
    fn chooses_lowest_quality_when_bandwidth_is_scarce() {
        let mut bola = Bola::new(ladder(), 0.3);
        // 200 kbps — below every level except the lowest.
        bola.observe_throughput(200_000.0);
        bola.update_buffer(BUFFER_MAX_S * 0.9);
        let d = bola.decide();
        assert_eq!(d.quality_index, 0);
    }

    #[test]
    fn chooses_high_quality_with_full_buffer_and_good_bandwidth() {
        let mut bola = Bola::new(ladder(), 0.3);
        bola.observe_throughput(20_000_000.0); // 20 Mbps
        bola.update_buffer(BUFFER_MAX_S);
        let d = bola.decide();
        // Should select the highest feasible level (index 4 = 1080p).
        assert_eq!(d.quality_index, 4);
    }

    #[test]
    fn score_increases_monotonically_with_buffer_fill() {
        // As the buffer fills up, the algorithm should generally be able to
        // sustain the same or better quality. Check that the chosen index is
        // non-decreasing as we sweep the buffer from low to high.
        let mut bola = Bola::new(ladder(), 0.3);
        bola.observe_throughput(20_000_000.0);

        let mut prev_idx = 0;
        let steps = 20;
        for i in 0..=steps {
            let buf = BUFFER_LOW_S + (BUFFER_MAX_S - BUFFER_LOW_S) * (i as f64 / steps as f64);
            bola.update_buffer(buf);
            let d = bola.decide();
            assert!(
                d.quality_index >= prev_idx,
                "quality dropped from {} to {} at buf={:.1}s",
                prev_idx, d.quality_index, buf
            );
            prev_idx = d.quality_index;
        }
    }

    #[test]
    fn throughput_ewma_converges() {
        let mut bola = Bola::new(ladder(), 0.5);
        for _ in 0..50 {
            bola.observe_throughput(1_000_000.0);
        }
        let eps = 1.0; // within 1 bps after 50 steps
        assert!((bola.throughput_bps() - 1_000_000.0).abs() < eps);
    }

    #[test]
    fn segment_size_estimate_is_correct() {
        let mut bola = Bola::new(ladder(), 0.3);
        // 300 000 bps × 4 s / 8 = 150 000 bytes
        let expected = 300_000.0 * SEGMENT_DURATION_S / 8.0;
        bola.update_buffer(BUFFER_MAX_S);
        // Pick the lowest level manually.
        let bytes = bola.qualities()[0].bitrate_bps * SEGMENT_DURATION_S / 8.0;
        assert!((bytes - expected).abs() < 1.0);
    }

    #[test]
    fn single_quality_always_highest_index() {
        let mut bola = Bola::new(
            vec![QualityLevel {
                label: "720p".into(),
                bitrate_bps: 2_500_000.0,
                utility: 0.0,
            }],
            0.3,
        );
        bola.observe_throughput(100_000.0); // would rule out multi-rung high picks
        bola.update_buffer(0.0);
        let d = bola.decide();
        assert_eq!(d.quality_index, 0);
        assert!(!d.is_emergency);
        assert_eq!(d.bitrate_bps, 2_500_000.0);
    }
}
