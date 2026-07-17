//! Period-scoped playback context: base URLs, target time, and timeline inputs.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dash_mpd::{AdaptationSet, MPD, Period};
use url::Url;

use super::super::PlayerError;
use super::super::clock::resync;
use super::super::manifest::{self, SegmentBaseContext, TimelineBuildContext};
use super::super::manifest_lifecycle::ContentSteeringState;
use super::super::track_selection::{
    SelectedAdaptationSet, TrackSelection, select_adaptation_sets,
};
use super::super::track_session::TrackSessionState;

/// Shared inputs for all adaptation streams within one Period.
#[derive(Debug, Clone)]
pub(crate) struct PeriodContext {
    pub segment_base_ctx: SegmentBaseContext,
    pub period_target_time: Option<Duration>,
    pub since_ast_utc: Option<Duration>,
    /// Prefer PRT-corrected since-AST when available (same value used for live edge).
    pub since_ast_for_latency: Option<Duration>,
    pub prt_reference_id: Option<String>,
}

impl PeriodContext {
    pub(crate) fn reference_since_ast(&self) -> Option<Duration> {
        self.since_ast_for_latency.or(self.since_ast_utc)
    }
}

pub(crate) struct PeriodContextInputs<'a> {
    pub mpd: &'a MPD,
    pub wall_now: DateTime<Utc>,
    pub current_window: manifest::PeriodWindow,
    pub period: &'a Period,
    pub manifest_uri: &'a Url,
    pub steering: &'a ContentSteeringState,
    pub seek_target_override: Option<Duration>,
    pub track_selection: &'a TrackSelection,
    pub track_sessions: &'a Arc<Vec<Arc<TrackSessionState>>>,
}

pub(crate) struct TimelineContextInputs<'a> {
    pub mpd: &'a MPD,
    pub wall_now: DateTime<Utc>,
    pub is_dynamic: bool,
    pub period_ctx: &'a PeriodContext,
    pub current_window: manifest::PeriodWindow,
    pub period: &'a Period,
    pub adaptation_set: &'a AdaptationSet,
    pub track_idx: usize,
    pub track_sessions: &'a Arc<Vec<Arc<TrackSessionState>>>,
}

pub(crate) fn build_period_context<'a>(
    inputs: PeriodContextInputs<'a>,
) -> Result<(PeriodContext, Vec<SelectedAdaptationSet<'a>>), PlayerError> {
    let PeriodContextInputs {
        mpd,
        wall_now,
        current_window,
        period,
        manifest_uri,
        steering,
        seek_target_override,
        track_selection,
        track_sessions,
    } = inputs;

    let period_start = current_window.start;
    let segment_base_ctx = manifest::SegmentBaseContext {
        manifest_uri: manifest_uri.clone(),
        mpd_base_urls: mpd.base_url.clone(),
        period_base_urls: period.BaseURL.clone(),
        service_location_priority: steering.service_location_priority().to_vec(),
        default_service_location: steering
            .config
            .as_ref()
            .and_then(|c| c.default_service_location.clone()),
        dvb_selection_seed: manifest::new_dvb_selection_seed(manifest_uri),
    };

    let since_ast_utc = manifest::since_availability_start_at(mpd, wall_now)?;
    let adaptation_sets = select_adaptation_sets(period, track_selection);
    let prt_reference_id = resync::latency_reference_id(mpd);

    let reference_since_ast = adaptation_sets
        .first()
        .and_then(|selected| {
            selected
                .adaptation_set
                .representations
                .first()
                .and_then(|rep| {
                    let inband = track_sessions.first().and_then(|s| s.inband_anchor());
                    resync::resync_corrected_since_ast(
                        mpd,
                        wall_now,
                        period,
                        period_start,
                        selected.adaptation_set,
                        rep,
                        inband,
                    )
                })
        })
        .or(since_ast_utc);

    let period_target_time = if let Some(seek) = seek_target_override {
        Some(seek)
    } else if let Some(s) = reference_since_ast {
        Some(manifest::target_presentation_time_from_since(mpd, s))
    } else {
        manifest::target_presentation_time_at(mpd, wall_now)?
    };

    Ok((
        PeriodContext {
            segment_base_ctx,
            period_target_time,
            since_ast_utc,
            since_ast_for_latency: reference_since_ast,
            prt_reference_id,
        },
        adaptation_sets,
    ))
}

pub(crate) fn build_timeline_context(inputs: TimelineContextInputs<'_>) -> TimelineBuildContext {
    let TimelineContextInputs {
        mpd,
        wall_now,
        is_dynamic,
        period_ctx,
        current_window,
        period,
        adaptation_set,
        track_idx,
        track_sessions,
    } = inputs;

    let rep = adaptation_set.representations.first();
    let since_ast = rep
        .and_then(|r| {
            let inband = track_sessions
                .get(track_idx)
                .and_then(|s| s.inband_anchor());
            resync::resync_corrected_since_ast(
                mpd,
                wall_now,
                period,
                current_window.start,
                adaptation_set,
                r,
                inband,
            )
        })
        .or(period_ctx.since_ast_utc);
    let resync_hints = rep.and_then(|r| resync::resync_hints(period, adaptation_set, r));
    TimelineBuildContext {
        is_dynamic,
        period_window: current_window,
        period_duration: period.duration,
        media_presentation_duration: mpd.mediaPresentationDuration,
        time_shift_buffer_depth: mpd.timeShiftBufferDepth,
        since_availability_start: since_ast,
        resync_hints,
    }
}
