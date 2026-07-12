//! Pluggable adaptive bitrate (ABR) for representation selection.
//!
//! The default backend is [`BolaAbrFactory`]. Supply a custom [`AbrFactory`] via
//! [`crate::MediaPlayer::with_abr_factory`] or [`crate::Player::with_abr_factory`] to
//! integrate alternative algorithms or rule engines.

pub mod bola;

use std::sync::Arc;

use dash_mpd::AdaptationSet;

pub use bola::BolaAbrFactory;

use crate::descriptors::is_delivery_representation;

/// Quality rung in an adaptation-set ladder, ordered low→high bitrate.
#[derive(Debug, Clone)]
pub struct QualityRung {
    /// Index into `AdaptationSet.representations`.
    pub representation_index: usize,
    /// Representation `@id`, if present.
    pub label: String,
    /// Nominal `@bandwidth` in bits per second.
    pub bitrate_bps: f64,
}

/// Next-segment representation choice returned by an [`AbrController`].
#[derive(Debug, Clone, PartialEq)]
pub struct AbrDecision {
    /// Index into the quality ladder (0 = lowest bitrate).
    pub quality_index: usize,
    /// Nominal bitrate of the chosen rung (bps).
    pub bitrate_bps: f64,
}

/// Per-adaptation-set ABR state (dash.js: one rules controller per stream).
pub trait AbrController: Send + Sync {
    /// Notify the controller that consumer-reported buffer occupancy changed.
    fn update_buffer(&mut self, buffer_s: f64);

    /// Record throughput after a segment download completes.
    fn observe_segment_download(
        &mut self,
        throughput_bps: f64,
        downloaded_bytes: usize,
        quality_index: usize,
    );

    /// Choose the quality index for the next segment.
    fn decide(&self) -> AbrDecision;

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
    fn create(&self, adaptation_set: &AdaptationSet) -> Option<Box<dyn AbrController>>;
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
            })
        })
        .collect();

    ladder.sort_by(|a, b| a.bitrate_bps.total_cmp(&b.bitrate_bps));
    ladder
}

/// Quality indices from `start` down to the lowest rung (inclusive), for representation fallback.
pub(crate) fn quality_indices_for_fallback(start: usize) -> impl DoubleEndedIterator<Item = usize> {
    (0..=start).rev()
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let controller = factory.create(&set).expect("controller");
        assert_eq!(controller.rung_count(), 2);
    }
}
