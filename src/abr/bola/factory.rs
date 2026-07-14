//! BOLA-backed [`AbrController`] (default ABR implementation).

use dash_mpd::AdaptationSet;

use super::algorithm::{Bola, QualityLevel};
use crate::abr::{
    AbrController, AbrCreateContext, AbrDecision, AbrFactory, QualityRung,
    apply_operating_constraints, preferred_quality_index, resolve_quality_ladder,
};

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
    fn create(
        &self,
        adaptation_set: &AdaptationSet,
        ctx: &AbrCreateContext<'_>,
    ) -> Option<Box<dyn AbrController>> {
        BolaAbrController::from_adaptation_set(adaptation_set, self.ewma_alpha, ctx)
            .map(|controller| Box::new(controller) as Box<dyn AbrController>)
    }
}

struct BolaAbrController {
    bola: Bola,
    rungs: Vec<QualityRung>,
    /// Prefer this ladder index until the first throughput sample (Operating*@target).
    preferred_quality_index: Option<usize>,
    has_throughput_sample: bool,
}

impl BolaAbrController {
    fn from_adaptation_set(
        adaptation_set: &AdaptationSet,
        ewma_alpha: f64,
        ctx: &AbrCreateContext<'_>,
    ) -> Option<Self> {
        let mut rungs = resolve_quality_ladder(adaptation_set, ctx);
        let preferred = if let Some(ops) = ctx.operating {
            rungs = apply_operating_constraints(rungs, ops);
            preferred_quality_index(&rungs, ops)
        } else {
            None
        };
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
            preferred_quality_index: preferred,
            has_throughput_sample: false,
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
        self.has_throughput_sample = true;
    }

    fn decide(&mut self) -> AbrDecision {
        if !self.has_throughput_sample {
            if let Some(idx) = self.preferred_quality_index {
                return AbrDecision {
                    quality_index: idx,
                    bitrate_bps: self.rungs[idx].bitrate_bps,
                };
            }
        }
        let decision = self.bola.decide();
        AbrDecision {
            quality_index: decision.quality_index,
            bitrate_bps: decision.bitrate_bps,
        }
    }

    fn rung_for_quality_index(&self, quality_index: usize) -> &QualityRung {
        &self.rungs[quality_index]
    }

    fn rung_count(&self) -> usize {
        self.rungs.len()
    }
}
