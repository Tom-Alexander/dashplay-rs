//! Build per-track sync-buffer prefetch plans for soft-linked next Periods.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dash_mpd::MPD;

use super::super::manifest::{self, PeriodLink, SegmentTemplateEndNumbers};
use super::super::schedule::SyncPrefetchPlan;
use super::super::track_selection::SelectedAdaptationSet;
use super::super::track_session::TrackSessionState;
use super::period_context::{PeriodContext, TimelineContextInputs, build_timeline_context};

pub(super) struct SyncPrefetchInputs<'a> {
    pub mpd: &'a MPD,
    pub current_window: manifest::PeriodWindow,
    pub next_window: manifest::PeriodWindow,
    pub sync_depth_s: f64,
    pub current_sets: &'a [SelectedAdaptationSet<'a>],
    pub period_ctx: &'a PeriodContext,
    pub wall_now: DateTime<Utc>,
    pub is_dynamic: bool,
    pub template_end_numbers: Option<&'a SegmentTemplateEndNumbers>,
    pub track_sessions: &'a Arc<Vec<Arc<TrackSessionState>>>,
}

/// When the next Period is Continuous/Connected, return per-track prefetch plans keyed by
/// selected-track index.
pub(super) fn build_sync_prefetch_plans(
    inputs: SyncPrefetchInputs<'_>,
) -> Option<HashMap<usize, SyncPrefetchPlan>> {
    let SyncPrefetchInputs {
        mpd,
        current_window,
        next_window,
        sync_depth_s,
        current_sets,
        period_ctx,
        wall_now,
        is_dynamic,
        template_end_numbers,
        track_sessions,
    } = inputs;

    let link = manifest::period_link(mpd, current_window.idx, next_window.idx);
    if !link.allows_soft_transition() {
        return None;
    }
    let period_end = current_window.end?;
    let trigger_abs_s = period_end.as_secs_f64() - sync_depth_s.max(0.0);
    let next_period = mpd.periods.get(next_window.idx)?.clone();
    let prev_period_id = mpd.periods.get(current_window.idx)?.id.clone();

    let mut plans = HashMap::new();
    for (track_idx, selected) in current_sets.iter().enumerate() {
        let Some(next_as) = next_period
            .adaptations
            .iter()
            .find(|aset| {
                manifest::adaptation_set_period_link(
                    selected.adaptation_set,
                    prev_period_id.as_deref(),
                    aset,
                )
                .allows_soft_transition()
                    || (link == PeriodLink::Continuous
                        && selected.adaptation_set.id.is_some()
                        && selected.adaptation_set.id == aset.id)
            })
            .cloned()
        else {
            continue;
        };

        let next_period_adaptation_index = next_period
            .adaptations
            .iter()
            .position(|a| a.id == next_as.id)
            .unwrap_or(0);

        let next_ctx = PeriodContext {
            segment_base_ctx: manifest::SegmentBaseContext {
                manifest_uri: period_ctx.segment_base_ctx.manifest_uri.clone(),
                mpd_base_urls: period_ctx.segment_base_ctx.mpd_base_urls.clone(),
                period_base_urls: next_period.BaseURL.clone(),
                service_location_priority: period_ctx
                    .segment_base_ctx
                    .service_location_priority
                    .clone(),
                default_service_location: period_ctx
                    .segment_base_ctx
                    .default_service_location
                    .clone(),
            },
            period_target_time: None,
            since_ast_utc: period_ctx.since_ast_utc,
            since_ast_for_latency: period_ctx.since_ast_for_latency,
            prt_reference_id: period_ctx.prt_reference_id.clone(),
        };

        let timeline_ctx = build_timeline_context(TimelineContextInputs {
            mpd,
            wall_now,
            is_dynamic,
            period_ctx: &next_ctx,
            current_window: next_window,
            period: &next_period,
            adaptation_set: &next_as,
            track_idx,
            track_sessions,
        });

        let template_end_number = template_end_numbers.and_then(|ends| {
            manifest::end_number_for_timeline(
                &next_period,
                &next_as,
                ends,
                next_window.idx,
                next_period_adaptation_index,
            )
        });

        plans.insert(
            track_idx,
            SyncPrefetchPlan {
                trigger_abs_s,
                next_period_start: next_window.start,
                next_period_idx: next_window.idx,
                next_period: next_period.clone(),
                next_adaptation_set: next_as,
                next_period_adaptation_index,
                next_timeline_ctx: timeline_ctx,
                next_segment_base_ctx: next_ctx.segment_base_ctx,
                next_template_end_number: template_end_number,
            },
        );
    }

    if plans.is_empty() { None } else { Some(plans) }
}
