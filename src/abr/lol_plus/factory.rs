//! LoL+-backed [`AbrController`] for low-latency live adaptation.

use dash_mpd::AdaptationSet;

use super::algorithm::{LolPlus, QualityLevel};
use crate::abr::{
    AbrController, AbrCreateContext, AbrDecision, AbrFactory, QualityRung,
    apply_operating_constraints, preferred_quality_index, resolve_quality_ladder,
};

/// Default segment duration used when the timeline does not provide one (seconds).
const DEFAULT_SEGMENT_DURATION_S: f64 = 2.0;
/// Dynamic weight-selector latency constraint (dash.js `DWS_TARGET_LATENCY`).
const DWS_TARGET_LATENCY_S: f64 = 1.5;
/// Dynamic weight-selector minimum buffer (dash.js `DWS_BUFFER_MIN`).
const DWS_BUFFER_MIN_S: f64 = 0.3;

/// [`AbrFactory`] using LoL+ (Low-on-Latency-plus SOM).
///
/// Optimized for CMAF low-latency live. Prefer [`crate::abr::BolaAbrFactory`] for
/// standard VOD / non-LL live unless you have chunked CMAF delivery.
#[derive(Debug, Clone)]
pub struct LolPlusAbrFactory {
    /// EWMA smoothing factor for throughput estimates.
    pub ewma_alpha: f64,
    /// Fallback segment duration (seconds) when `AbrCreateContext` has none.
    pub segment_duration_s: f64,
    /// Latency constraint for dynamic weight selection (seconds).
    pub target_latency_s: f64,
    /// Minimum projected buffer for safety / weight constraints (seconds).
    pub buffer_min_s: f64,
    /// Seed for k-means++ weight-centre initialization (reproducible decisions).
    pub rng_seed: u64,
}

impl Default for LolPlusAbrFactory {
    fn default() -> Self {
        Self {
            ewma_alpha: 0.3,
            segment_duration_s: DEFAULT_SEGMENT_DURATION_S,
            target_latency_s: DWS_TARGET_LATENCY_S,
            buffer_min_s: DWS_BUFFER_MIN_S,
            rng_seed: 0x1015_7015,
        }
    }
}

impl AbrFactory for LolPlusAbrFactory {
    fn create(
        &self,
        adaptation_set: &AdaptationSet,
        ctx: &AbrCreateContext<'_>,
    ) -> Option<Box<dyn AbrController>> {
        LolPlusAbrController::from_adaptation_set(adaptation_set, self, ctx)
            .map(|controller| Box::new(controller) as Box<dyn AbrController>)
    }
}

struct LolPlusAbrController {
    lol: LolPlus,
    rungs: Vec<QualityRung>,
    preferred_quality_index: Option<usize>,
}

impl LolPlusAbrController {
    fn from_adaptation_set(
        adaptation_set: &AdaptationSet,
        factory: &LolPlusAbrFactory,
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
            })
            .collect();

        let segment_duration_s = ctx
            .segment_duration_s
            .filter(|d| d.is_finite() && *d > 0.0)
            .unwrap_or(factory.segment_duration_s);

        Some(Self {
            lol: LolPlus::new(
                qualities,
                segment_duration_s,
                factory.target_latency_s,
                factory.buffer_min_s,
                factory.ewma_alpha,
                factory.rng_seed,
            ),
            rungs,
            preferred_quality_index: preferred,
        })
    }
}

impl AbrController for LolPlusAbrController {
    fn update_buffer(&mut self, buffer_s: f64) {
        self.lol.update_buffer(buffer_s);
    }

    fn update_latency(&mut self, latency_s: f64) {
        self.lol.update_latency(latency_s);
    }

    fn update_playback_rate(&mut self, rate: f64) {
        self.lol.update_playback_rate(rate);
    }

    fn observe_segment_download(
        &mut self,
        throughput_bps: f64,
        downloaded_bytes: usize,
        quality_index: usize,
    ) {
        self.lol
            .observe_segment_download(throughput_bps, downloaded_bytes, quality_index);
    }

    fn decide(&mut self) -> AbrDecision {
        if !self.lol.has_throughput_sample() {
            if let Some(idx) = self.preferred_quality_index {
                return AbrDecision {
                    quality_index: idx,
                    bitrate_bps: self.rungs[idx].bitrate_bps,
                };
            }
        }
        let decision = self.lol.decide();
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
