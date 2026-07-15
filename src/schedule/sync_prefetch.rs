//! Sync-buffer prefetch of the next Period's first media segment.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period};
use tokio::sync::Mutex as AsyncMutex;

use crate::PlayerError;
use crate::abr::{AbrCreateContext, AbrFactory, BolaAbrFactory};
use crate::drm::DrmSessionCoordinator;
use crate::http::SharedHttpClient;
use crate::manifest::{self, TimelineBuildContext};
use crate::metrics::TrackMetrics;
use crate::segment_blacklist::SegmentBlacklist;
use crate::track_selection::TrackKind;
use crate::track_session::TrackSessionState;
use crate::types::PlayerEvent;

use super::segment_decrypt::decrypt_media_fragment;
use super::segment_emit::segment_presentation_time;
use super::segment_fetch::{
    RepFetchEnv, fetch_init_with_rep_fallback, fetch_media_with_rep_fallback,
};
use super::segment_plan::{SegmentPlanContext, plan_init, plan_segment};

/// Plan for fetching the next Period's leading media once the sync buffer depth is reached.
#[derive(Clone)]
pub(crate) struct SyncPrefetchPlan {
    /// Absolute presentation time at which prefetch may begin.
    pub trigger_abs_s: f64,
    pub next_period_start: Duration,
    pub next_period_idx: usize,
    pub next_period: Period,
    pub next_adaptation_set: AdaptationSet,
    pub next_period_adaptation_index: usize,
    pub next_timeline_ctx: TimelineBuildContext,
    pub next_segment_base_ctx: manifest::SegmentBaseContext,
    pub next_template_end_number: Option<u64>,
}

/// Fetch (but do not emit) the first undelivered media segment of the next Period.
pub(crate) async fn prefetch_next_period_first_segment(
    plan: &SyncPrefetchPlan,
    client: &SharedHttpClient,
    session: &TrackSessionState,
    blacklist: &SegmentBlacklist,
    drm: &Arc<AsyncMutex<DrmSessionCoordinator>>,
    track_kind: TrackKind,
    cmcd: Option<&crate::cmcd::CmcdSession>,
) -> Result<Option<Vec<PlayerEvent>>, PlayerError> {
    let _ = plan.next_period_idx;
    let addressing =
        manifest::segment_addressing_for_timeline(&plan.next_period, &plan.next_adaptation_set)?;
    if matches!(addressing, manifest::SegmentAddressing::Base(_)) {
        return Ok(None);
    }

    let mut segments = manifest::timeline_segments_for_addressing(
        &addressing,
        &plan.next_timeline_ctx,
        plan.next_template_end_number,
    )?;
    segments = manifest::filter_segments_by_availability(
        segments,
        plan.next_timeline_ctx.is_dynamic,
        plan.next_period_start,
        plan.next_timeline_ctx.since_availability_start,
        &addressing,
    );

    let period_start_s = plan.next_period_start.as_secs_f64();
    let Some(seg) = segments
        .into_iter()
        .find(|s| !session.lock_delivered().is_delivered(s, period_start_s))
    else {
        return Ok(None);
    };

    let mut adaptation_sets = HashMap::new();
    adaptation_sets.insert(
        plan.next_period_adaptation_index,
        plan.next_adaptation_set.clone(),
    );
    let mut bitstream_switching = HashMap::new();
    bitstream_switching.insert(
        plan.next_period_adaptation_index,
        manifest::bitstream_switching_enabled(
            &plan.next_period,
            &plan.next_adaptation_set,
            &addressing,
        ),
    );

    let Some(mut abr) = BolaAbrFactory::default().create(
        &plan.next_adaptation_set,
        &AbrCreateContext {
            operating: None,
            segment_duration_s: Some(seg.duration_s).filter(|d| *d > 0.0),
            quality_ladder: None,
        },
    ) else {
        return Ok(None);
    };

    let dummy_tx = tokio::sync::broadcast::channel::<PlayerEvent>(1).0;
    let metrics = TrackMetrics::new();
    let fetch_env = RepFetchEnv {
        client,
        segment_base_ctx: &plan.next_segment_base_ctx,
        period: &plan.next_period,
        adaptation_sets: &adaptation_sets,
        primary_period_adaptation_index: plan.next_period_adaptation_index,
        bitstream_switching: &bitstream_switching,
        blacklist,
        drm,
        tx: &dummy_tx,
        metrics: &metrics,
        track_kind,
        cmcd,
        emit_init: false,
    };

    let mut encrypted_init_by_rep: HashMap<(usize, String), Bytes> = HashMap::new();
    let init_plan = plan_init(abr.as_mut(), 0.0);
    let _ = fetch_init_with_rep_fallback(
        &fetch_env,
        abr.as_ref(),
        init_plan.quality_index,
        &mut encrypted_init_by_rep,
    )
    .await?;

    let plan_seg = plan_segment(
        abr.as_mut(),
        0.0,
        &seg,
        0,
        &SegmentPlanContext {
            segment_start_index: 0,
            primary_period_adaptation_index: plan.next_period_adaptation_index,
            adaptation_sets: &adaptation_sets,
            bitstream_switching: &bitstream_switching,
            addressing: &addressing,
            timeline_ctx: &plan.next_timeline_ctx,
            cached_inits: &encrypted_init_by_rep,
            last_quality_index: None,
        },
    );

    if plan_seg.chunked {
        return Ok(None);
    }

    let mut sidx_segments_by_rep: HashMap<String, Vec<manifest::TimelineSegment>> = HashMap::new();
    let mut per_segment_index_ranges_by_rep: HashMap<String, HashMap<u64, manifest::ByteRange>> =
        HashMap::new();

    let (bytes, used_quality_index, seg_for_fetch) = fetch_media_with_rep_fallback(
        &fetch_env,
        abr.as_ref(),
        &plan_seg,
        &mut encrypted_init_by_rep,
        &mut sidx_segments_by_rep,
        &mut per_segment_index_ranges_by_rep,
    )
    .await?;

    let (rep_period_idx, rep_aset, rep_idx) =
        fetch_env.resolve_quality(abr.as_ref(), used_quality_index);
    let rep = &rep_aset.representations[rep_idx];
    let rep_id = rep.id.as_deref().unwrap_or_default();
    let init_for_decrypt = encrypted_init_by_rep
        .get(&(rep_period_idx, rep_id.to_string()))
        .cloned()
        .unwrap_or_default();

    {
        let mut guard = drm.lock().await;
        guard
            .ensure_from_fragments(rep_period_idx, rep_id, &init_for_decrypt, Some(&bytes))
            .await?;
    }

    let data = decrypt_media_fragment(
        drm,
        rep_period_idx,
        rep_id,
        &init_for_decrypt,
        Bytes::from(bytes),
    )
    .await
    .map_err(PlayerError::from)?;

    let presentation_time = segment_presentation_time(plan.next_period_start, &seg_for_fetch);
    let event = PlayerEvent::Segment {
        number: seg_for_fetch.number,
        time: seg_for_fetch.time,
        presentation_time,
        sub_number: seg_for_fetch.sub_number,
        partial: None,
        data,
    };

    session
        .lock_delivered()
        .mark_delivered(&seg_for_fetch, period_start_s);

    Ok(Some(vec![event]))
}
