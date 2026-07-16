//! BOLA-backed [`AbrController`] (default ABR implementation).

use dash_mpd::AdaptationSet;

use super::algorithm::{
    Bola, BolaParams, DEFAULT_BUFFER_MAX_S, DEFAULT_SEGMENT_DURATION_S, QualityLevel,
};
use crate::abr::{
    AbrController, AbrCreateContext, AbrDecision, AbrFactory, QualityRung,
    apply_operating_constraints, apply_user_quality_constraints, forced_quality_index,
    preferred_quality_index, resolve_quality_ladder,
};

/// Default [`AbrFactory`] using BOLA (Buffer Occupancy based Lyapunov Algorithm).
///
/// Segment duration and buffer ceiling are taken from [`AbrCreateContext`] when
/// present (timeline / `BufferTarget`); otherwise the factory fallbacks below are used.
#[derive(Debug, Clone)]
pub struct BolaAbrFactory {
    /// EWMA smoothing factor for throughput estimates.
    pub ewma_alpha: f64,
    /// Fallback segment duration (seconds) when `AbrCreateContext` has none.
    pub segment_duration_s: f64,
    /// Fallback buffer ceiling (seconds) when `AbrCreateContext` has none.
    pub buffer_max_s: f64,
}

impl Default for BolaAbrFactory {
    fn default() -> Self {
        Self {
            ewma_alpha: 0.3,
            segment_duration_s: DEFAULT_SEGMENT_DURATION_S,
            buffer_max_s: DEFAULT_BUFFER_MAX_S,
        }
    }
}

impl AbrFactory for BolaAbrFactory {
    fn create(
        &self,
        adaptation_set: &AdaptationSet,
        ctx: &AbrCreateContext<'_>,
    ) -> Option<Box<dyn AbrController>> {
        BolaAbrController::from_adaptation_set(adaptation_set, self, ctx)
            .map(|controller| Box::new(controller) as Box<dyn AbrController>)
    }
}

struct BolaAbrController {
    bola: Bola,
    rungs: Vec<QualityRung>,
    /// Prefer this ladder index until the first throughput sample (Operating*@target).
    preferred_quality_index: Option<usize>,
    /// Always return this index (user fixed quality / data-saver).
    forced_quality_index: Option<usize>,
    has_throughput_sample: bool,
}

impl BolaAbrController {
    fn from_adaptation_set(
        adaptation_set: &AdaptationSet,
        factory: &BolaAbrFactory,
        ctx: &AbrCreateContext<'_>,
    ) -> Option<Self> {
        let mut rungs = resolve_quality_ladder(adaptation_set, ctx);
        let preferred = if let Some(ops) = ctx.operating {
            rungs = apply_operating_constraints(rungs, ops);
            preferred_quality_index(&rungs, ops)
        } else {
            None
        };
        if let Some(user) = ctx.user {
            rungs = apply_user_quality_constraints(rungs, user);
        }
        if rungs.is_empty() {
            return None;
        }
        // Fixed quality / data-saver override Operating*@target when present.
        let forced = ctx
            .user
            .and_then(|user| forced_quality_index(rungs.len(), user));
        let preferred = forced.or(preferred);

        let qualities: Vec<QualityLevel> = rungs
            .iter()
            .map(|rung| QualityLevel {
                label: rung.label.clone(),
                bitrate_bps: rung.bitrate_bps,
                utility: 0.0,
            })
            .collect();

        let segment_duration_s = ctx
            .segment_duration_s
            .filter(|d| d.is_finite() && *d > 0.0)
            .unwrap_or(factory.segment_duration_s);
        let buffer_max_s = ctx
            .buffer_max_s
            .filter(|d| d.is_finite() && *d > 0.0)
            .unwrap_or(factory.buffer_max_s);

        Some(Self {
            bola: Bola::with_params(
                qualities,
                factory.ewma_alpha,
                BolaParams {
                    segment_duration_s,
                    buffer_max_s,
                },
            ),
            rungs,
            preferred_quality_index: preferred,
            forced_quality_index: forced,
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
        if let Some(idx) = self.forced_quality_index {
            return AbrDecision {
                quality_index: idx,
                bitrate_bps: self.rungs[idx].bitrate_bps,
            };
        }
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
