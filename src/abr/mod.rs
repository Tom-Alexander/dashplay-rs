//! Pluggable adaptive bitrate (ABR) for representation selection.
//!
//! The default backend is [`BolaAbrFactory`]. Supply a custom [`AbrFactory`] via
//! [`crate::MediaPlayer::with_abr_factory`] or [`crate::Player::with_abr_factory`] to
//! integrate alternative algorithms (e.g. [`LolPlusAbrFactory`]) or rule engines.

pub mod bola;
pub mod lol_plus;

use std::sync::Arc;

use dash_mpd::AdaptationSet;

pub use bola::BolaAbrFactory;
pub use lol_plus::LolPlusAbrFactory;

use crate::clock::service_description::ResolvedOperatingConstraints;
use crate::track_selection::descriptors::is_delivery_representation;

/// Quality rung in an adaptation-set ladder, ordered low→high bitrate.
#[derive(Debug, Clone)]
pub struct QualityRung {
    /// Index into `AdaptationSet.representations`.
    pub representation_index: usize,
    /// Representation `@id`, if present.
    pub label: String,
    /// Nominal `@bandwidth` in bits per second.
    pub bitrate_bps: f64,
    /// Representation `@qualityRanking` when present (lower is better).
    pub quality_ranking: Option<u8>,
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
    /// Nominal media segment duration (seconds) when known from the timeline.
    pub(crate) segment_duration_s: Option<f64>,
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

    /// Map a quality index to `AdaptationSet.representations` index.
    fn representation_index_for_quality_index(&self, quality_index: usize) -> usize;

    /// Nominal bitrate (bps) for a quality index.
    fn bitrate_bps_for_quality_index(&self, quality_index: usize) -> f64;

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
    let mut ladder: Vec<QualityRung> = adaptation_set
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
            Some(QualityRung {
                representation_index: idx,
                label,
                bitrate_bps: bw,
                quality_ranking: r.qualityRanking,
            })
        })
        .collect();

    ladder.sort_by(|a, b| a.bitrate_bps.total_cmp(&b.bitrate_bps));
    ladder
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
    fn empty_filter_keeps_ladder() {
        let set = adaptation_set_with_bandwidths(&[100_000, 200_000]);
        let ladder = quality_ladder_from_adaptation_set(&set);
        let constraints = ResolvedOperatingConstraints {
            bandwidth_min: Some(5_000_000),
            ..Default::default()
        };
        let filtered = apply_operating_constraints(ladder.clone(), &constraints);
        assert_eq!(filtered.len(), ladder.len());
    }
}
