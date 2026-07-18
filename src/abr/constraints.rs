//! User-facing ABR quality constraints (dash.js: `abr.minBitrate` / `maxBitrate`,
//! `autoSwitchBitrate`, `setQualityFor`, data-saver).

use super::QualityRung;

/// User constraints on representation selection.
///
/// Apply at player construction via [`crate::MediaPlayer::with_quality_constraints`] /
/// [`crate::Player::with_quality_constraints`], or update at runtime with
/// [`crate::PlaybackController::set_quality_constraints`].
///
/// Ladder filtering (`min_bitrate_bps`, `max_bitrate_bps`, `data_saver`) is applied when an
/// [`super::AbrController`] is created. Fixed-quality / autoswitch overrides are applied on
/// every ABR decision so they take effect without restarting the stream.
///
/// Changing bitrate envelope or data-saver at runtime rebuilds ABR state (same interrupt path
/// as [`crate::PlaybackController::set_track_selection`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityConstraints {
    /// Minimum representation `@bandwidth` in bits per second (inclusive).
    pub min_bitrate_bps: Option<u64>,
    /// Maximum representation `@bandwidth` in bits per second (inclusive).
    pub max_bitrate_bps: Option<u64>,
    /// When `false`, ABR autoswitch is disabled and every segment uses
    /// [`Self::fixed_quality_index`] (or the lowest rung when unset).
    pub auto_switch: bool,
    /// Fixed ladder index when [`Self::auto_switch`] is `false`. Clamped to the active ladder.
    pub fixed_quality_index: Option<usize>,
    /// Data-saver mode: keep only the lowest available quality rung.
    pub data_saver: bool,
}

impl Default for QualityConstraints {
    fn default() -> Self {
        Self {
            min_bitrate_bps: None,
            max_bitrate_bps: None,
            auto_switch: true,
            fixed_quality_index: None,
            data_saver: false,
        }
    }
}

impl QualityConstraints {
    /// No user constraints (full ABR freedom).
    pub fn new() -> Self {
        Self::default()
    }

    /// Require representations at or above this `@bandwidth` (bps).
    pub fn min_bitrate_bps(mut self, bps: u64) -> Self {
        self.min_bitrate_bps = Some(bps);
        self
    }

    /// Cap representations at or below this `@bandwidth` (bps).
    pub fn max_bitrate_bps(mut self, bps: u64) -> Self {
        self.max_bitrate_bps = Some(bps);
        self
    }

    /// Enable or disable automatic quality switching.
    pub fn auto_switch(mut self, enabled: bool) -> Self {
        self.auto_switch = enabled;
        self
    }

    /// Pin playback to a ladder index and disable autoswitch (dash.js: `setQualityFor`).
    pub fn fixed_quality(mut self, quality_index: usize) -> Self {
        self.auto_switch = false;
        self.fixed_quality_index = Some(quality_index);
        self
    }

    /// Prefer the lowest quality rung only (data-saver / metered networks).
    pub fn data_saver(mut self, enabled: bool) -> Self {
        self.data_saver = enabled;
        self
    }

    /// `true` when no bitrate envelope, fixed quality, or data-saver is configured.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// `true` when ladder filtering (min/max/data-saver) differs from `other`.
    ///
    /// Autoswitch / fixed-index-only changes return `false` (no ABR rebuild required).
    pub fn ladder_filter_changed(&self, other: &Self) -> bool {
        self.min_bitrate_bps != other.min_bitrate_bps
            || self.max_bitrate_bps != other.max_bitrate_bps
            || self.data_saver != other.data_saver
    }
}

/// Filter a quality ladder by user min/max bitrate and optional data-saver.
///
/// If filtering would remove every rung, the original ladder is retained so playback can continue.
pub(crate) fn apply_user_quality_constraints(
    ladder: Vec<QualityRung>,
    constraints: &QualityConstraints,
) -> Vec<QualityRung> {
    if ladder.is_empty() {
        return ladder;
    }

    let filtered: Vec<QualityRung> = ladder
        .iter()
        .filter(|rung| rung_matches_bitrate_envelope(rung, constraints))
        .cloned()
        .collect();

    let filtered = if filtered.is_empty() {
        ladder
    } else {
        filtered
    };

    if constraints.data_saver {
        filtered.into_iter().take(1).collect()
    } else {
        filtered
    }
}

/// Resolve a forced quality index for disabled autoswitch or data-saver.
///
/// Returns `None` when ABR may freely choose among ladder rungs.
pub(crate) fn forced_quality_index(
    ladder_len: usize,
    constraints: &QualityConstraints,
) -> Option<usize> {
    if ladder_len == 0 {
        return None;
    }
    if constraints.data_saver {
        return Some(0);
    }
    if !constraints.auto_switch {
        let idx = constraints.fixed_quality_index.unwrap_or(0);
        return Some(idx.min(ladder_len - 1));
    }
    None
}

/// Clamp an ABR decision to user constraints (fixed quality / data-saver).
pub(crate) fn clamp_quality_index(
    quality_index: usize,
    ladder_len: usize,
    constraints: &QualityConstraints,
) -> usize {
    if ladder_len == 0 {
        return 0;
    }
    if let Some(forced) = forced_quality_index(ladder_len, constraints) {
        return forced;
    }
    quality_index.min(ladder_len - 1)
}

fn rung_matches_bitrate_envelope(rung: &QualityRung, c: &QualityConstraints) -> bool {
    let bw = rung.bitrate_bps as u64;
    if let Some(min) = c.min_bitrate_bps {
        if bw < min {
            return false;
        }
    }
    if let Some(max) = c.max_bitrate_bps {
        if bw > max {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abr::QualityRung;

    fn ladder(bandwidths: &[u64]) -> Vec<QualityRung> {
        bandwidths
            .iter()
            .enumerate()
            .map(|(idx, bw)| QualityRung {
                period_adaptation_index: 0,
                representation_index: idx,
                label: format!("r{idx}"),
                bitrate_bps: *bw as f64,
                quality_ranking: None,
                max_playout_rate: None,
                coding_dependency: None,
            })
            .collect()
    }

    #[test]
    fn filters_by_min_and_max_bitrate() {
        let filtered = apply_user_quality_constraints(
            ladder(&[100_000, 500_000, 1_000_000, 3_000_000]),
            &QualityConstraints::default()
                .min_bitrate_bps(400_000)
                .max_bitrate_bps(1_500_000),
        );
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].bitrate_bps, 500_000.0);
        assert_eq!(filtered[1].bitrate_bps, 1_000_000.0);
    }

    #[test]
    fn data_saver_keeps_lowest_rung() {
        let filtered = apply_user_quality_constraints(
            ladder(&[100_000, 500_000, 1_000_000]),
            &QualityConstraints::default().data_saver(true),
        );
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].bitrate_bps, 100_000.0);
    }

    #[test]
    fn empty_filter_retains_original_ladder() {
        let original = ladder(&[100_000, 200_000]);
        let filtered = apply_user_quality_constraints(
            original.clone(),
            &QualityConstraints::default().min_bitrate_bps(10_000_000),
        );
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn fixed_quality_forces_index() {
        let c = QualityConstraints::default().fixed_quality(2);
        assert_eq!(forced_quality_index(4, &c), Some(2));
        assert_eq!(clamp_quality_index(0, 4, &c), 2);
        assert_eq!(clamp_quality_index(3, 2, &c), 1);
    }

    #[test]
    fn auto_switch_leaves_decision() {
        let c = QualityConstraints::default();
        assert_eq!(forced_quality_index(3, &c), None);
        assert_eq!(clamp_quality_index(2, 3, &c), 2);
    }

    #[test]
    fn ladder_filter_changed_ignores_autoswitch() {
        let a = QualityConstraints::default().max_bitrate_bps(1_000_000);
        let b = a.clone().fixed_quality(0);
        assert!(!a.ladder_filter_changed(&b));
        let c = a.clone().min_bitrate_bps(100_000);
        assert!(a.ladder_filter_changed(&c));
    }
}
