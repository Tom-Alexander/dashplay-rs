//! Adaptive bitrate control (dash.js: `AbrController` + rules such as BOLA).

use dash_mpd::AdaptationSet;

use super::bola::{Bola, BolaDecision, QualityLevel};

/// Chooses representation per segment from an adaptation set’s ladder (dash.js-style ABR facade).
pub struct AbrController {
    bola: Bola,
    /// Maps BOLA quality index → `AdaptationSet.representations` index.
    rep_by_quality: Vec<usize>,
}

impl AbrController {
    /// Build a ladder from representations with positive `bandwidth`; sorts low→high like dash.js rungs.
    pub fn from_adaptation_set(adaptation_set: &AdaptationSet, ewma_alpha: f64) -> Option<Self> {
        let mut ladder: Vec<(usize, String, f64)> = adaptation_set
            .representations
            .iter()
            .enumerate()
            .filter_map(|(idx, r)| {
                let bw = r.bandwidth.unwrap_or(0) as f64;
                if bw <= 0.0 {
                    return None;
                }
                let id = r.id.as_deref().unwrap_or_default().to_string();
                Some((idx, id, bw))
            })
            .collect();

        ladder.sort_by(|a, b| a.2.total_cmp(&b.2));

        if ladder.is_empty() {
            return None;
        }

        let rep_by_quality: Vec<usize> = ladder.iter().map(|(idx, _, _)| *idx).collect();
        let qualities: Vec<QualityLevel> = ladder
            .into_iter()
            .map(|(_, id, bw)| QualityLevel {
                label: id,
                bitrate_bps: bw,
                utility: 0.0,
            })
            .collect();

        Some(Self {
            bola: Bola::new(qualities, ewma_alpha),
            rep_by_quality,
        })
    }

    pub fn update_buffer(&mut self, buffer_s: f64) {
        self.bola.update_buffer(buffer_s);
    }

    pub fn observe_segment_download(
        &mut self,
        throughput_bps: f64,
        downloaded_bytes: usize,
        quality_index: usize,
    ) {
        let estimated = self.bola.estimated_segment_bytes_for_quality(quality_index);
        self.bola
            .observe_segment_download(throughput_bps, downloaded_bytes, estimated);
    }

    pub fn decide(&self) -> BolaDecision {
        self.bola.decide()
    }

    #[allow(dead_code)]
    pub fn buffer_s(&self) -> f64 {
        self.bola.buffer_s()
    }

    pub fn representation_index_for_quality_index(&self, quality_index: usize) -> usize {
        self.rep_by_quality[quality_index]
    }

    /// Quality indices from `start` down to the lowest rung (inclusive), for representation fallback.
    pub fn quality_indices_for_fallback(
        &self,
        start: usize,
    ) -> impl DoubleEndedIterator<Item = usize> + '_ {
        (0..=start).rev()
    }

    #[allow(dead_code)]
    pub fn rung_count(&self) -> usize {
        self.rep_by_quality.len()
    }
}
