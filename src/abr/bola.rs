//! BOLA-backed [`AbrController`] (default ABR implementation).

use dash_mpd::AdaptationSet;

use super::{
    AbrController, AbrDecision, AbrFactory, QualityRung, quality_ladder_from_adaptation_set,
};
use crate::bola::{Bola, QualityLevel};

/// Default [`AbrFactory`] using BOLA (Buffer Occupancy based Lyapunov Algorithm).
#[derive(Debug, Clone)]
pub struct BolaAbrFactory {
    /// EWMA smoothing factor for throughput estimates.
    pub ewma_alpha: f64,
}

impl Default for BolaAbrFactory {
    fn default() -> Self {
        Self { ewma_alpha: 0.3 }
    }
}

impl AbrFactory for BolaAbrFactory {
    fn create(&self, adaptation_set: &AdaptationSet) -> Option<Box<dyn AbrController>> {
        BolaAbrController::from_adaptation_set(adaptation_set, self.ewma_alpha)
            .map(|controller| Box::new(controller) as Box<dyn AbrController>)
    }
}

struct BolaAbrController {
    bola: Bola,
    rungs: Vec<QualityRung>,
}

impl BolaAbrController {
    fn from_adaptation_set(adaptation_set: &AdaptationSet, ewma_alpha: f64) -> Option<Self> {
        let rungs = quality_ladder_from_adaptation_set(adaptation_set);
        if rungs.is_empty() {
            return None;
        }

        let qualities: Vec<QualityLevel> = rungs
            .iter()
            .map(|rung| QualityLevel {
                label: rung.label.clone(),
                bitrate_bps: rung.bitrate_bps,
                utility: 0.0,
            })
            .collect();

        Some(Self {
            bola: Bola::new(qualities, ewma_alpha),
            rungs,
        })
    }
}

impl AbrController for BolaAbrController {
    fn update_buffer(&mut self, buffer_s: f64) {
        self.bola.update_buffer(buffer_s);
    }

    fn observe_segment_download(
        &mut self,
        throughput_bps: f64,
        downloaded_bytes: usize,
        quality_index: usize,
    ) {
        let estimated = self.bola.estimated_segment_bytes_for_quality(quality_index);
        self.bola
            .observe_segment_download(throughput_bps, downloaded_bytes, estimated);
    }

    fn decide(&self) -> AbrDecision {
        let decision = self.bola.decide();
        AbrDecision {
            quality_index: decision.quality_index,
            bitrate_bps: decision.bitrate_bps,
        }
    }

    fn representation_index_for_quality_index(&self, quality_index: usize) -> usize {
        self.rungs[quality_index].representation_index
    }

    fn bitrate_bps_for_quality_index(&self, quality_index: usize) -> f64 {
        self.rungs[quality_index].bitrate_bps
    }

    fn rung_count(&self) -> usize {
        self.rungs.len()
    }
}
