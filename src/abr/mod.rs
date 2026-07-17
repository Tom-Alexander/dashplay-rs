//! Pluggable adaptive bitrate (ABR) for representation selection.
//!
//! The default backend is [`BolaAbrFactory`]. Supply a custom [`AbrFactory`] via
//! [`crate::MediaPlayer::with_abr_factory`] or [`crate::Player::with_abr_factory`] to
//! integrate alternative algorithms (e.g. [`LolPlusAbrFactory`]) or rule engines.

pub mod bola;
pub mod constraints;
pub mod dropped_frames;
pub mod lol_plus;

use std::sync::Arc;

use dash_mpd::AdaptationSet;

pub use bola::BolaAbrFactory;
pub use constraints::QualityConstraints;
pub use dropped_frames::{
    DEFAULT_DROPPED_FRAMES_PERCENTAGE_THRESHOLD, DEFAULT_MINIMUM_SAMPLE_SIZE, DroppedFramesHistory,
    DroppedFramesParams,
};
pub use lol_plus::LolPlusAbrFactory;

pub(crate) use dropped_frames::apply_dropped_frames_cap;

use crate::clock::service_description::ResolvedOperatingConstraints;
use crate::track_selection::descriptors::is_delivery_representation;

pub(crate) use constraints::{
    apply_user_quality_constraints, clamp_quality_index, forced_quality_index,
};

/// Quality rung in an adaptation-set ladder, ordered low→high bitrate.
#[derive(Debug, Clone)]
pub struct QualityRung {
    /// Index into `Period.adaptations` for the Adaptation Set that owns this representation.
    pub period_adaptation_index: usize,
    /// Index into `AdaptationSet.representations`.
    pub representation_index: usize,
    /// Representation `@id`, if present.
    pub label: String,
    /// Nominal `@bandwidth` in bits per second.
    pub bitrate_bps: f64,
    /// Representation `@qualityRanking` when present (lower is better).
    pub quality_ranking: Option<u8>,
    /// Maximum supported playout rate (`@maxPlayoutRate`), inherited from Representation →
    /// AdaptationSet when unset on the Representation.
    pub max_playout_rate: Option<f64>,
    /// Whether samples have coding dependencies (`@codingDependency`), inherited from
    /// Representation → AdaptationSet. Metadata only; not used for trick-play selection.
    pub coding_dependency: Option<bool>,
}

/// Next-segment representation choice returned by an [`AbrController`].
#[derive(Debug, Clone, PartialEq)]
pub struct AbrDecision {
    /// Index into the quality ladder (0 = lowest bitrate).
    pub quality_index: usize,
    /// Nominal bitrate of the chosen rung (bps).
    pub bitrate_bps: f64,
}

/// Inputs for constructing an [`AbrController`] for one adaptation set.
#[derive(Debug, Clone, Copy, Default)]
pub struct AbrCreateContext<'a> {
    /// Resolved `OperatingBandwidth` / `OperatingQuality` for this media type.
    pub(crate) operating: Option<&'a ResolvedOperatingConstraints>,
    /// User-facing quality constraints (min/max bitrate, data-saver, fixed quality).
    pub(crate) user: Option<&'a QualityConstraints>,
    /// Nominal media segment duration (seconds) when known from the timeline.
    pub(crate) segment_duration_s: Option<f64>,
    /// Prefetch / ABR buffer ceiling (seconds), typically from [`crate::schedule::BufferTarget`].
    pub(crate) buffer_max_s: Option<f64>,
    /// Pre-resolved quality ladder (including cross-AS switch / fallback peers).
    ///
    /// When `Some`, factories should use this instead of building from the single adaptation set.
    pub quality_ladder: Option<&'a [QualityRung]>,
}

/// Per-adaptation-set ABR state (dash.js: one rules controller per stream).
pub trait AbrController: Send + Sync {
    /// Notify the controller that consumer-reported buffer occupancy changed.
    fn update_buffer(&mut self, buffer_s: f64);

    /// Notify the controller of measured live latency (seconds). Default: no-op.
    fn update_latency(&mut self, _latency_s: f64) {}

    /// Notify the controller of current / suggested playback rate. Default: no-op.
    fn update_playback_rate(&mut self, _rate: f64) {}

    /// Record throughput after a segment download completes.
    fn observe_segment_download(
        &mut self,
        throughput_bps: f64,
        downloaded_bytes: usize,
        quality_index: usize,
    );

    /// Choose the quality index for the next segment.
    ///
    /// May mutate learning state (e.g. LoL+ SOM updates).
    fn decide(&mut self) -> AbrDecision;

    /// Quality rung for a ladder index.
    fn rung_for_quality_index(&self, quality_index: usize) -> &QualityRung;

    /// Map a quality index to `AdaptationSet.representations` index.
    fn representation_index_for_quality_index(&self, quality_index: usize) -> usize {
        self.rung_for_quality_index(quality_index)
            .representation_index
    }

    /// Nominal bitrate (bps) for a quality index.
    fn bitrate_bps_for_quality_index(&self, quality_index: usize) -> f64 {
        self.rung_for_quality_index(quality_index).bitrate_bps
    }

    /// Number of rungs in the quality ladder.
    fn rung_count(&self) -> usize;
}

/// Creates an [`AbrController`] for each adaptation set when a stream starts.
pub trait AbrFactory: Send + Sync {
    /// Build a controller for `adaptation_set`, or `None` when no delivery representations exist.
    fn create(
        &self,
        adaptation_set: &AdaptationSet,
        ctx: &AbrCreateContext<'_>,
    ) -> Option<Box<dyn AbrController>>;
}

/// Shared handle to an [`AbrFactory`] implementation.
pub type SharedAbrFactory = Arc<dyn AbrFactory>;

/// Wrap a concrete factory for sharing across playback tasks.
pub fn shared(factory: impl AbrFactory + 'static) -> SharedAbrFactory {
    Arc::new(factory)
}

/// Build a bandwidth-ordered quality ladder from delivery representations.
pub fn quality_ladder_from_adaptation_set(adaptation_set: &AdaptationSet) -> Vec<QualityRung> {
    quality_ladder_from_adaptation_sets(&[(0, adaptation_set)])
}

/// Build a bandwidth-ordered quality ladder spanning one or more Adaptation Sets.
///
/// Each entry is `(period_adaptation_index, adaptation_set)`. Used for cross-AS switching
/// and DVB fallback ladders.
pub fn quality_ladder_from_adaptation_sets(sets: &[(usize, &AdaptationSet)]) -> Vec<QualityRung> {
    let mut ladder: Vec<QualityRung> = sets
        .iter()
        .flat_map(|(period_adaptation_index, adaptation_set)| {
            adaptation_set
                .representations
                .iter()
                .enumerate()
                .filter_map(|(idx, r)| {
                    if !is_delivery_representation(r) {
                        return None;
                    }
                    let bw = r.bandwidth.unwrap_or(0) as f64;
                    if bw <= 0.0 {
                        return None;
                    }
                    let label = r.id.as_deref().unwrap_or_default().to_string();
                    let max_playout_rate = r
                        .maxPlayoutRate
                        .filter(|v| v.is_finite() && *v > 0.0)
                        .or_else(|| {
                            adaptation_set
                                .maxPlayoutRate
                                .filter(|v| v.is_finite() && *v > 0.0)
                        });
                    Some(QualityRung {
                        period_adaptation_index: *period_adaptation_index,
                        representation_index: idx,
                        label,
                        bitrate_bps: bw,
                        quality_ranking: r.qualityRanking,
                        max_playout_rate,
                        coding_dependency: r.codingDependency.or(adaptation_set.codingDependency),
                    })
                })
        })
        .collect();

    ladder.sort_by(|a, b| a.bitrate_bps.total_cmp(&b.bitrate_bps));
    ladder
}

/// Resolve the quality ladder for an ABR factory: prefer a pre-built ladder from context.
pub(crate) fn resolve_quality_ladder(
    adaptation_set: &AdaptationSet,
    ctx: &AbrCreateContext<'_>,
) -> Vec<QualityRung> {
    if let Some(ladder) = ctx.quality_ladder {
        ladder.to_vec()
    } else {
        quality_ladder_from_adaptation_set(adaptation_set)
    }
}

/// Filter a quality ladder by [`ResolvedOperatingConstraints`] min/max envelopes.
///
/// If filtering would remove every rung, the original ladder is retained so playback can continue.
pub(crate) fn apply_operating_constraints(
    ladder: Vec<QualityRung>,
    constraints: &ResolvedOperatingConstraints,
) -> Vec<QualityRung> {
    if constraints.is_empty() || ladder.is_empty() {
        return ladder;
    }

    let filtered: Vec<QualityRung> = ladder
        .iter()
        .filter(|rung| rung_matches_constraints(rung, constraints))
        .cloned()
        .collect();

    if filtered.is_empty() {
        ladder
    } else {
        filtered
    }
}

/// Ladder index nearest to `OperatingBandwidth@target` / `OperatingQuality@target`, if any.
pub(crate) fn preferred_quality_index(
    ladder: &[QualityRung],
    constraints: &ResolvedOperatingConstraints,
) -> Option<usize> {
    if ladder.is_empty() {
        return None;
    }

    if let Some(target_bw) = constraints.bandwidth_target {
        let target = target_bw as f64;
        return ladder
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (a.bitrate_bps - target)
                    .abs()
                    .total_cmp(&(b.bitrate_bps - target).abs())
            })
            .map(|(i, _)| i);
    }

    if let Some(target_q) = constraints.quality_target {
        return ladder
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.quality_ranking.map(|q| (i, q as u64)))
            .min_by_key(|(_, q)| target_q.abs_diff(*q))
            .map(|(i, _)| i);
    }

    None
}

fn rung_matches_constraints(rung: &QualityRung, c: &ResolvedOperatingConstraints) -> bool {
    let bw = rung.bitrate_bps as u64;
    if let Some(min) = c.bandwidth_min {
        if bw < min {
            return false;
        }
    }
    if let Some(max) = c.bandwidth_max {
        if bw > max {
            return false;
        }
    }
    if let Some(ranking) = rung.quality_ranking {
        let q = ranking as u64;
        if let Some(min) = c.quality_min {
            if q < min {
                return false;
            }
        }
        if let Some(max) = c.quality_max {
            if q > max {
                return false;
            }
        }
    } else if c.quality_min.is_some() || c.quality_max.is_some() {
        // Ranking constraints present but rung has no ranking: keep (unable to evaluate).
    }
    true
}

/// Quality indices from `start` down to the lowest rung (inclusive), for representation fallback.
pub(crate) fn quality_indices_for_fallback(start: usize) -> impl DoubleEndedIterator<Item = usize> {
    (0..=start).rev()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::service_description::ResolvedOperatingConstraints;
    use dash_mpd::{AdaptationSet, Representation};

    fn adaptation_set_with_bandwidths(bandwidths: &[u64]) -> AdaptationSet {
        AdaptationSet {
            representations: bandwidths
                .iter()
                .enumerate()
                .map(|(idx, bw)| Representation {
                    id: Some(format!("rep-{idx}")),
                    bandwidth: Some(*bw),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn adaptation_set_with_rankings(entries: &[(u64, u8)]) -> AdaptationSet {
        AdaptationSet {
            representations: entries
                .iter()
                .enumerate()
                .map(|(idx, (bw, rank))| Representation {
                    id: Some(format!("rep-{idx}")),
                    bandwidth: Some(*bw),
                    qualityRanking: Some(*rank),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn quality_ladder_sorts_by_bandwidth() {
        let set = adaptation_set_with_bandwidths(&[2_000_000, 500_000, 1_000_000]);
        let ladder = quality_ladder_from_adaptation_set(&set);
        assert_eq!(ladder.len(), 3);
        assert_eq!(ladder[0].bitrate_bps, 500_000.0);
        assert_eq!(ladder[2].bitrate_bps, 2_000_000.0);
    }

    #[test]
    fn quality_ladder_inherits_max_playout_rate_and_coding_dependency() {
        let set = AdaptationSet {
            maxPlayoutRate: Some(2.0),
            codingDependency: Some(true),
            representations: vec![
                Representation {
                    id: Some("inherited".into()),
                    bandwidth: Some(500_000),
                    ..Default::default()
                },
                Representation {
                    id: Some("override".into()),
                    bandwidth: Some(1_000_000),
                    maxPlayoutRate: Some(8.0),
                    codingDependency: Some(false),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let ladder = quality_ladder_from_adaptation_set(&set);
        assert_eq!(ladder.len(), 2);
        assert_eq!(ladder[0].max_playout_rate, Some(2.0));
        assert_eq!(ladder[0].coding_dependency, Some(true));
        assert_eq!(ladder[1].max_playout_rate, Some(8.0));
        assert_eq!(ladder[1].coding_dependency, Some(false));
    }

    #[test]
    fn bola_factory_uses_manifest_timing_from_context() {
        let set = adaptation_set_with_bandwidths(&[500_000, 1_000_000]);
        let factory = BolaAbrFactory::default();
        let mut controller = factory
            .create(
                &set,
                &AbrCreateContext {
                    segment_duration_s: Some(2.0),
                    buffer_max_s: Some(12.5),
                    ..Default::default()
                },
            )
            .expect("controller");
        // Emergency low-water is one segment (2 s), not the 4 s factory default.
        controller.update_buffer(1.0);
        controller.observe_segment_download(10_000_000.0, 250_000, 0);
        let decision = controller.decide();
        assert_eq!(decision.quality_index, 0);
        controller.update_buffer(10.0);
        let decision = controller.decide();
        assert!(decision.quality_index <= 1);
    }

    #[test]
    fn bola_factory_creates_controller() {
        let set = adaptation_set_with_bandwidths(&[500_000, 1_000_000]);
        let factory = BolaAbrFactory::default();
        let controller = factory
            .create(&set, &AbrCreateContext::default())
            .expect("controller");
        assert_eq!(controller.rung_count(), 2);
    }

    #[test]
    fn lol_plus_factory_creates_controller() {
        let set = adaptation_set_with_bandwidths(&[500_000, 1_000_000, 2_000_000]);
        let factory = LolPlusAbrFactory::default();
        let mut controller = factory
            .create(
                &set,
                &AbrCreateContext {
                    segment_duration_s: Some(2.0),
                    ..Default::default()
                },
            )
            .expect("controller");
        assert_eq!(controller.rung_count(), 3);
        controller.update_buffer(4.0);
        controller.update_latency(1.2);
        controller.update_playback_rate(1.0);
        controller.observe_segment_download(5_000_000.0, 1_250_000, 1);
        let decision = controller.decide();
        assert!(decision.quality_index < 3);
    }

    #[test]
    fn operating_bandwidth_filters_ladder() {
        let set = adaptation_set_with_bandwidths(&[100_000, 500_000, 1_000_000, 3_000_000]);
        let ladder = quality_ladder_from_adaptation_set(&set);
        let constraints = ResolvedOperatingConstraints {
            bandwidth_min: Some(400_000),
            bandwidth_max: Some(1_500_000),
            ..Default::default()
        };
        let filtered = apply_operating_constraints(ladder, &constraints);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].bitrate_bps, 500_000.0);
        assert_eq!(filtered[1].bitrate_bps, 1_000_000.0);
    }

    #[test]
    fn operating_quality_filters_by_ranking() {
        let set = adaptation_set_with_rankings(&[
            (100_000, 4),
            (500_000, 2),
            (1_000_000, 1),
            (2_000_000, 3),
        ]);
        let ladder = quality_ladder_from_adaptation_set(&set);
        let constraints = ResolvedOperatingConstraints {
            quality_min: Some(1),
            quality_max: Some(2),
            ..Default::default()
        };
        let filtered = apply_operating_constraints(ladder, &constraints);
        assert_eq!(filtered.len(), 2);
        assert!(
            filtered
                .iter()
                .all(|r| matches!(r.quality_ranking, Some(1) | Some(2)))
        );
    }

    #[test]
    fn preferred_quality_nearest_bandwidth_target() {
        let set = adaptation_set_with_bandwidths(&[100_000, 500_000, 1_000_000]);
        let ladder = quality_ladder_from_adaptation_set(&set);
        let constraints = ResolvedOperatingConstraints {
            bandwidth_target: Some(600_000),
            ..Default::default()
        };
        assert_eq!(preferred_quality_index(&ladder, &constraints), Some(1));
    }

    #[test]
    fn quality_ladder_merges_multiple_adaptation_sets() {
        let low = AdaptationSet {
            id: Some("fb".into()),
            representations: vec![Representation {
                id: Some("lo".into()),
                bandwidth: Some(48_000),
                ..Default::default()
            }],
            ..Default::default()
        };
        let high = AdaptationSet {
            id: Some("main".into()),
            representations: vec![
                Representation {
                    id: Some("mid".into()),
                    bandwidth: Some(128_000),
                    ..Default::default()
                },
                Representation {
                    id: Some("hi".into()),
                    bandwidth: Some(256_000),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let ladder = quality_ladder_from_adaptation_sets(&[(0, &high), (1, &low)]);
        assert_eq!(ladder.len(), 3);
        assert_eq!(ladder[0].period_adaptation_index, 1);
        assert_eq!(ladder[0].bitrate_bps, 48_000.0);
        assert_eq!(ladder[1].period_adaptation_index, 0);
        assert_eq!(ladder[2].bitrate_bps, 256_000.0);
    }

    #[test]
    fn factory_uses_prebuilt_quality_ladder_from_context() {
        let set = adaptation_set_with_bandwidths(&[500_000]);
        let peer = adaptation_set_with_bandwidths(&[100_000, 2_000_000]);
        let ladder = quality_ladder_from_adaptation_sets(&[(5, &set), (7, &peer)]);
        let factory = BolaAbrFactory::default();
        let controller = factory
            .create(
                &set,
                &AbrCreateContext {
                    quality_ladder: Some(ladder.as_slice()),
                    ..Default::default()
                },
            )
            .expect("controller");
        assert_eq!(controller.rung_count(), 3);
        assert_eq!(
            controller.rung_for_quality_index(0).period_adaptation_index,
            7
        );
    }

    #[test]
    fn factory_applies_user_max_bitrate() {
        let set = adaptation_set_with_bandwidths(&[100_000, 500_000, 1_000_000, 3_000_000]);
        let constraints = QualityConstraints::default().max_bitrate_bps(600_000);
        let factory = BolaAbrFactory::default();
        let controller = factory
            .create(
                &set,
                &AbrCreateContext {
                    user: Some(&constraints),
                    ..Default::default()
                },
            )
            .expect("controller");
        assert_eq!(controller.rung_count(), 2);
        assert_eq!(controller.bitrate_bps_for_quality_index(1), 500_000.0);
    }

    #[test]
    fn factory_fixed_quality_pins_decision() {
        let set = adaptation_set_with_bandwidths(&[100_000, 500_000, 1_000_000]);
        let constraints = QualityConstraints::default().fixed_quality(1);
        let factory = BolaAbrFactory::default();
        let mut controller = factory
            .create(
                &set,
                &AbrCreateContext {
                    user: Some(&constraints),
                    ..Default::default()
                },
            )
            .expect("controller");
        controller.update_buffer(20.0);
        controller.observe_segment_download(10_000_000.0, 250_000, 1);
        let decision = controller.decide();
        assert_eq!(decision.quality_index, 1);
        assert_eq!(decision.bitrate_bps, 500_000.0);
    }

    #[test]
    fn factory_data_saver_keeps_lowest_rung() {
        let set = adaptation_set_with_bandwidths(&[100_000, 500_000, 1_000_000]);
        let constraints = QualityConstraints::default().data_saver(true);
        let factory = BolaAbrFactory::default();
        let mut controller = factory
            .create(
                &set,
                &AbrCreateContext {
                    user: Some(&constraints),
                    ..Default::default()
                },
            )
            .expect("controller");
        assert_eq!(controller.rung_count(), 1);
        controller.update_buffer(20.0);
        assert_eq!(controller.decide().quality_index, 0);
    }
}
