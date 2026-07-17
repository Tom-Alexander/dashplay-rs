//! Adaptation-set segment scheduling loop.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::platform::Instant;

use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{broadcast, watch};

use crate::PlayerError;
use crate::abr::SharedAbrFactory;
use crate::cmcd::CmcdSession;
use crate::drm::DrmSessionCoordinator;
use crate::http::{HttpRetryConfig, SharedHttpClient};
use crate::manifest::{self, TimelineBuildContext};
use crate::metrics::TrackMetrics;
use crate::playback_control::{PlaybackController, PlaybackState};
use crate::segment::SegmentError;
use crate::segment_blacklist::SegmentBlacklist;
use crate::track_session::TrackSessionState;
use crate::types::PlayerEvent;

use futures::future::join_all;

use super::buffer_target::{BufferEstimatePublish, BufferTarget, wait_for_fetch_capacity};
use super::parallel_prefetch::prefetch_batch_len;
use super::segment_decrypt::decrypt_media_fragment;
use super::segment_emit::{
    emit_segment, latest_buffer_s, partial_chunk_meta, publish_buffer_estimate,
    record_quality_switch_and_throughput, with_media_clock_ticks,
};
use super::segment_fetch::{
    InitSignalState, RepFetchEnv, download_prepared_media, fetch_and_parse_segment_base_index,
    fetch_and_parse_segment_template_index, fetch_cmaf_media_with_rep_fallback,
    fetch_init_with_rep_fallback, fetch_media_with_rep_fallback, prepare_media_with_rep_fallback,
};
use super::segment_plan::{SegmentPlan, SegmentPlanContext, plan_init, plan_segment};
use super::sync_prefetch::{SyncPrefetchPlan, prefetch_next_period_first_segment};

fn align_start_index_with_resync(
    segments: &[manifest::TimelineSegment],
    start_idx: usize,
    timeline_ctx: &TimelineBuildContext,
    target_presentation_time_s: Option<f64>,
) -> (usize, Option<u64>) {
    let Some(hints) = timeline_ctx.resync_hints else {
        return (start_idx, None);
    };
    manifest::align_start_with_resync_hints(segments, start_idx, hints, target_presentation_time_s)
}

fn nominal_segment_duration_s(segments: &[manifest::TimelineSegment]) -> Option<f64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for seg in segments {
        if seg.duration_s.is_finite() && seg.duration_s > 0.0 {
            sum += seg.duration_s;
            count += 1;
        }
    }
    if count == 0 {
        None
    } else {
        Some(sum / count as f64)
    }
}

fn feed_abr_live_inputs(abr: &mut dyn crate::abr::AbrController, playback: &PlaybackController) {
    if let Some(latency) = playback.live_latency() {
        abr.update_latency(latency.as_secs_f64());
    }
    abr.update_playback_rate(playback.playback_rate());
}

/// Publish `@maxPlayoutRate` from the active video/trick quality rung for rate clamping.
fn update_max_playout_rate_cap(
    playback: &PlaybackController,
    abr: &dyn crate::abr::AbrController,
    track_kind: crate::track_selection::TrackKind,
    quality_index: usize,
) {
    use crate::track_selection::TrackKind;
    if !matches!(track_kind, TrackKind::Video | TrackKind::TrickPlay) {
        return;
    }
    if abr.rung_count() == 0 {
        return;
    }
    let qi = quality_index.min(abr.rung_count() - 1);
    playback.set_max_playout_rate_cap(abr.rung_for_quality_index(qi).max_playout_rate);
}

/// Whether this addressing can invent later `$Number$` segments from `@duration` alone
/// (no MPD refresh / SegmentTimeline update required).
fn duration_template_live_edge(addressing: &manifest::SegmentAddressing, is_dynamic: bool) -> bool {
    if !is_dynamic {
        return false;
    }
    match addressing {
        manifest::SegmentAddressing::Template(st) => {
            st.SegmentTimeline.is_none() && st.duration.is_some_and(|d| d > 0.0)
        }
        _ => false,
    }
}

fn next_duration_template_segment(
    last: &manifest::TimelineSegment,
) -> Option<manifest::TimelineSegment> {
    if !last.duration_s.is_finite() || last.duration_s <= 0.0 {
        return None;
    }
    Some(manifest::TimelineSegment {
        number: last.number.saturating_add(1),
        time: last.time.saturating_add(last.duration),
        duration: last.duration,
        duration_s: last.duration_s,
        presentation_time_s: last.presentation_time_s + last.duration_s,
        sub_number: None,
        resync_start_chunk: None,
        media_url: None,
        media_range: None,
    })
}

struct LiveEdgeExtend<'a> {
    segments: &'a mut Vec<manifest::TimelineSegment>,
    addressing: &'a manifest::SegmentAddressing,
    availability: manifest::SegmentAvailability,
    timeline_ctx: &'a TimelineBuildContext,
    period_start: Duration,
    since_ast_base: Option<Duration>,
    live_edge_anchor: Instant,
    playback: &'a PlaybackController,
    seek_generation_at_start: u64,
    event_tx: &'a broadcast::Sender<PlayerEvent>,
}

/// Wait until the next `@duration` live segment is published, then append it to `segments`.
///
/// Duration-template live does not require `minimumUpdatePeriod` refreshes to discover new
/// `$Number$` values: availability follows wall clock (Instant-extrapolated from the snapshot
/// `since_availability_start`). Returns `false` when the presentation has ended or the client
/// must refresh the MPD (e.g. SegmentTimeline / Period boundary).
async fn extend_duration_template_live_edge(ctx: LiveEdgeExtend<'_>) -> bool {
    let LiveEdgeExtend {
        segments,
        addressing,
        availability,
        timeline_ctx,
        period_start,
        since_ast_base,
        live_edge_anchor,
        playback,
        seek_generation_at_start,
        event_tx,
    } = ctx;

    if !duration_template_live_edge(addressing, timeline_ctx.is_dynamic) {
        return false;
    }
    // Bounded Periods (known `@duration` / end / mediaPresentationDuration) must return so the
    // stream controller can advance Periods and refresh the MPD. Only open-ended live Periods
    // invent `$Number$` values from the wall clock without an MPD update.
    if timeline_ctx.period_end_secs().is_some() {
        return false;
    }
    let Some(since_base) = since_ast_base else {
        return false;
    };
    let Some(last) = segments.last() else {
        return false;
    };
    let Some(next) = next_duration_template_segment(last) else {
        return false;
    };

    let poll =
        Duration::from_millis(100).min(Duration::from_secs_f64(next.duration_s.max(0.05) / 2.0));

    loop {
        playback.wait_while_paused().await;
        if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
            return false;
        }
        if event_tx.receiver_count() == 0 {
            return false;
        }

        let since = since_base.saturating_add(live_edge_anchor.elapsed());
        if manifest::segment_is_available(&next, period_start, since, &availability) {
            segments.push(next);
            return true;
        }

        crate::platform::sleep(poll).await;
    }
}

pub(crate) struct AdaptationStreamContext {
    pub client: SharedHttpClient,
    pub segment_base_ctx: manifest::SegmentBaseContext,
    pub target_time: Option<Duration>,
    pub period_start: Duration,
    pub period: Period,
    pub timeline_ctx: TimelineBuildContext,
    pub template_end_numbers: Option<manifest::SegmentTemplateEndNumbers>,
    /// `RandomAccess` elements recovered from raw MPD XML (`dash-mpd` omits them).
    pub random_access: Option<manifest::RandomAccessSupplements>,
    pub period_idx: usize,
    pub adaptation_set: AdaptationSet,
    /// Switch / DVB-fallback peer adaptation sets keyed by period adaptation index.
    pub switch_peers: HashMap<usize, AdaptationSet>,
    /// Index into the session's selected `PlayerTrack` list.
    pub track_idx: usize,
    /// Index into `Period.adaptations` for the primary selected adaptation set.
    pub period_adaptation_index: usize,
    pub tx: broadcast::Sender<PlayerEvent>,
    pub session: Arc<TrackSessionState>,
    pub blacklist: SegmentBlacklist,
    pub drm: Arc<AsyncMutex<DrmSessionCoordinator>>,
    /// Latest buffer occupancy (media-clock estimate and optional consumer reports).
    pub buffer_rx: watch::Receiver<f64>,
    /// Sender used to publish estimated buffer occupancy.
    pub buffer_tx: watch::Sender<f64>,
    /// Host-reported dropped-frame history for the ABR down-switch rule.
    pub dropped_frames: crate::abr::DroppedFramesHistory,
    /// Prefetch thresholds from `MPD@minBufferTime`; ceiling is scaled to segment duration.
    pub buffer_target: BufferTarget,
    pub metrics: TrackMetrics,
    pub playback: PlaybackController,
    pub abr_factory: SharedAbrFactory,
    /// `@referenceId` from `ServiceDescription::Latency` when present.
    pub prt_reference_id: Option<String>,
    /// `OperatingBandwidth` / `OperatingQuality` constraints for this adaptation set.
    pub operating_constraints:
        Option<crate::clock::service_description::ResolvedOperatingConstraints>,
    /// Shared CMCD/CMSD session when enabled on the player.
    pub cmcd: Option<CmcdSession>,
    /// HTTP retry policy for transient failures.
    pub http_retry: HttpRetryConfig,
    /// Kind of the selected track (for CMCD `ot`).
    pub track_kind: crate::track_selection::TrackKind,
    /// Optional sync-buffer prefetch of the next Continuous/Connected Period.
    pub sync_prefetch: Option<SyncPrefetchPlan>,
}

/// Run the fragment loop for one adaptation set until segments are exhausted for this manifest snapshot.
///
/// When sync-buffer prefetch runs, returns `(track_idx, held events)` to emit after the next
/// [`PlayerEvent::PeriodChanged`].
pub(crate) async fn run_adaptation_stream(
    ctx: AdaptationStreamContext,
) -> Result<Option<(usize, Vec<PlayerEvent>)>, PlayerError> {
    let AdaptationStreamContext {
        client,
        segment_base_ctx,
        target_time,
        period_start,
        period,
        timeline_ctx,
        template_end_numbers,
        random_access,
        period_idx,
        adaptation_set,
        switch_peers,
        track_idx,
        period_adaptation_index,
        tx,
        session,
        blacklist,
        drm,
        mut buffer_rx,
        buffer_tx,
        dropped_frames,
        buffer_target,
        metrics,
        playback,
        abr_factory,
        prt_reference_id,
        operating_constraints,
        cmcd,
        http_retry,
        track_kind,
        sync_prefetch,
    } = ctx;

    let seek_generation_at_start = playback.seek_generation();
    playback.set_state(PlaybackState::Buffering);

    let addressing = manifest::segment_addressing_for_timeline(&period, &adaptation_set)?;

    let set_availability = {
        let primary_rep = adaptation_set
            .representations
            .first()
            .ok_or(SegmentError::ExhaustedRepresentations)?;
        let base_url_availability = manifest::base_url_availability_for_representation(
            &segment_base_ctx,
            &adaptation_set,
            primary_rep,
        );
        manifest::SegmentAvailability::for_representation(&addressing, &base_url_availability)
    };

    let mut adaptation_sets = switch_peers;
    adaptation_sets.insert(period_adaptation_index, adaptation_set.clone());

    let mut bitstream_switching = HashMap::new();
    for (idx, aset) in &adaptation_sets {
        let aset_addressing = manifest::segment_addressing_for_timeline(&period, aset)
            .unwrap_or_else(|_| addressing.clone());
        bitstream_switching.insert(
            *idx,
            manifest::bitstream_switching_enabled(&period, aset, &aset_addressing),
        );
    }

    let quality_ladder = {
        let ladder_sets: Vec<(usize, &AdaptationSet)> = adaptation_sets
            .iter()
            .map(|(idx, aset)| (*idx, aset))
            .collect();
        crate::abr::quality_ladder_from_adaptation_sets(&ladder_sets)
    };

    let template_end_number = template_end_numbers.as_ref().and_then(|supplements| {
        manifest::end_number_for_timeline(
            &period,
            &adaptation_set,
            supplements,
            period_idx,
            period_adaptation_index,
        )
    });

    let segments_all = match &addressing {
        manifest::SegmentAddressing::Base(sb) if manifest::segment_base_uses_sidx_index(sb) => {
            let rep = adaptation_set
                .representations
                .first()
                .ok_or(SegmentError::ExhaustedRepresentations)?;
            let merged_sb =
                manifest::segment_base_for_representation(&period, &adaptation_set, rep)?;
            let bases = manifest::segment_bases_for_representation(
                &segment_base_ctx,
                &adaptation_set,
                rep,
            )?;
            fetch_and_parse_segment_base_index(
                &client,
                &bases,
                &merged_sb,
                rep,
                &adaptation_set,
                &blacklist,
                &http_retry,
            )
            .await?
        }
        manifest::SegmentAddressing::Template(st)
            if manifest::segment_template_uses_global_sidecar_index(st) =>
        {
            let rep = adaptation_set
                .representations
                .first()
                .ok_or(SegmentError::ExhaustedRepresentations)?;
            let merged_st =
                manifest::segment_template_for_representation(&period, &adaptation_set, rep)?;
            let bases = manifest::segment_bases_for_representation(
                &segment_base_ctx,
                &adaptation_set,
                rep,
            )?;
            fetch_and_parse_segment_template_index(
                &client,
                &bases,
                &merged_st,
                rep,
                &adaptation_set,
                &blacklist,
                &http_retry,
            )
            .await?
        }
        _ => manifest::timeline_segments_for_addressing(
            &addressing,
            &timeline_ctx,
            template_end_number,
        )?,
    };
    let segments_all = manifest::filter_segments_by_availability(
        segments_all,
        timeline_ctx.is_dynamic,
        period_start,
        timeline_ctx.since_availability_start,
        &set_availability,
    );

    // Align every adaptation set to the same media instant: pick the first segment whose
    // interval (in MPD time) still contains instants after `target`. Using "last segment with
    // start <= target" breaks A/V sync when audio and video use different segment durations
    // (e.g. 6s audio vs 2s video): each track would start at a different segment start time.
    let (segments, segment_start_index) = {
        let delivered_tracker = session.lock_delivered();
        if let Some(target) = target_time {
            let target_s = target.as_secs_f64();
            let p0 = period_start.as_secs_f64();
            let target_in_period = target_s - p0;
            let start_idx = segments_all
                .iter()
                .position(|s| p0 + s.presentation_time_s + s.duration_s > target_s)
                .unwrap_or(0);
            let start_idx =
                manifest::align_start_index_to_sap(&segments_all, start_idx, &adaptation_set);
            let (start_idx, resync_start_chunk) = align_start_index_with_resync(
                &segments_all,
                start_idx,
                &timeline_ctx,
                Some(target_in_period),
            );
            let ra_hints = random_access
                .as_ref()
                .map(|s| s.hints_for(period_idx, period_adaptation_index, None))
                .unwrap_or_default();
            let start_idx = if timeline_ctx.resync_hints.is_none() {
                manifest::align_start_index_with_random_access(&segments_all, start_idx, &ra_hints)
            } else {
                start_idx
            };
            let start_idx = delivered_tracker.advance_start_index(&segments_all, start_idx, p0);
            let mut slice = segments_all[start_idx..].to_vec();
            if let Some(chunk) = resync_start_chunk {
                if let Some(first) = slice.first_mut() {
                    first.resync_start_chunk = Some(chunk);
                }
            }
            (slice, start_idx)
        } else {
            let start_idx = manifest::align_start_index_to_sap(&segments_all, 0, &adaptation_set);
            let (start_idx, _) =
                align_start_index_with_resync(&segments_all, start_idx, &timeline_ctx, None);
            let ra_hints = random_access
                .as_ref()
                .map(|s| s.hints_for(period_idx, period_adaptation_index, None))
                .unwrap_or_default();
            let start_idx = if timeline_ctx.resync_hints.is_none() {
                manifest::align_start_index_with_random_access(&segments_all, start_idx, &ra_hints)
            } else {
                start_idx
            };
            let start_idx = delivered_tracker.advance_start_index(
                &segments_all,
                start_idx,
                period_start.as_secs_f64(),
            );
            (segments_all[start_idx..].to_vec(), start_idx)
        }
    };

    let segment_duration_s = nominal_segment_duration_s(&segments);
    let buffer_target = buffer_target.with_segment_duration(segment_duration_s);

    let quality_constraints = playback.quality_constraints();
    let Some(mut abr) = abr_factory.create(
        &adaptation_set,
        &crate::abr::AbrCreateContext {
            operating: operating_constraints.as_ref(),
            user: Some(&quality_constraints),
            segment_duration_s,
            buffer_max_s: Some(buffer_target.max_buffer_s),
            quality_ladder: Some(quality_ladder.as_slice()),
        },
    ) else {
        return Ok(None);
    };

    feed_abr_live_inputs(abr.as_mut(), &playback);
    abr.update_buffer(latest_buffer_s(&buffer_rx));
    metrics.record_buffer(latest_buffer_s(&buffer_rx));

    let init_taken = session.try_take_init();

    // Cache init segments by (Adaptation Set index, Representation ID).
    let mut encrypted_init_by_rep: HashMap<(usize, String), Bytes> = HashMap::new();
    // Soft period transitions keep `have_init`: suppress re-emitting the continuing Init, but still
    // emit when ABR switches to a Representation whose Init the consumer has not seen.
    let mut init_signal = InitSignalState::new(!init_taken);
    let fetch_env = RepFetchEnv {
        client: &client,
        segment_base_ctx: &segment_base_ctx,
        period: &period,
        adaptation_sets: &adaptation_sets,
        primary_period_adaptation_index: period_adaptation_index,
        bitstream_switching: &bitstream_switching,
        blacklist: &blacklist,
        drm: &drm,
        tx: &tx,
        metrics: &metrics,
        track_kind,
        cmcd: cmcd.as_ref(),
        http_retry: &http_retry,
        emit_init: init_taken,
    };
    if init_taken {
        let init_plan = plan_init(
            abr.as_mut(),
            latest_buffer_s(&buffer_rx),
            &playback.quality_constraints(),
            Some(&dropped_frames),
        );
        let init_res: Result<(), PlayerError> = async {
            let (_, rep_id) = fetch_init_with_rep_fallback(
                &fetch_env,
                abr.as_ref(),
                init_plan.quality_index,
                &mut encrypted_init_by_rep,
                &mut init_signal,
            )
            .await?;
            let _ = rep_id;
            metrics.set_quality_index(init_plan.quality_index);
            dropped_frames.set_active_quality(init_plan.quality_index);
            update_max_playout_rate_cap(
                &playback,
                abr.as_ref(),
                track_kind,
                init_plan.quality_index,
            );
            Ok(())
        }
        .await;
        if init_res.is_err() {
            session.release_init();
            init_res?;
        }
    }

    let mut sidx_segments_by_rep: HashMap<String, Vec<manifest::TimelineSegment>> = HashMap::new();
    let mut per_segment_index_ranges_by_rep: HashMap<String, HashMap<u64, manifest::ByteRange>> =
        HashMap::new();
    let mut last_quality_index = metrics.last_quality_index();
    if let Some(qi) = last_quality_index {
        dropped_frames.set_active_quality(qi);
    }
    let mut playback_started_emitted = false;
    let mut media_segments_delivered = 0usize;
    let mut held: Option<Vec<PlayerEvent>> = None;
    let mut segments: Vec<manifest::TimelineSegment> = segments;
    let mut cursor = 0usize;
    let live_edge_anchor = Instant::now();
    let since_ast_base = timeline_ctx.since_availability_start;

    loop {
        if cursor >= segments.len() {
            let extended = extend_duration_template_live_edge(LiveEdgeExtend {
                segments: &mut segments,
                addressing: &addressing,
                availability: set_availability,
                timeline_ctx: &timeline_ctx,
                period_start,
                since_ast_base,
                live_edge_anchor,
                playback: &playback,
                seek_generation_at_start,
                event_tx: &tx,
            })
            .await;
            if !extended {
                break;
            }
            continue;
        }

        playback.wait_while_paused().await;
        if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
            return Ok(None);
        }

        {
            let delivered_tracker = session.lock_delivered();
            if delivered_tracker.is_delivered(&segments[cursor], period_start.as_secs_f64()) {
                cursor += 1;
                continue;
            }
        }

        if held.is_none()
            && let Some(plan) = sync_prefetch.as_ref()
            && session.last_abs_end_s() + 1e-9 >= plan.trigger_abs_s
        {
            // Start next-period HTTP before fetching remaining current segments.
            held = prefetch_next_period_first_segment(
                plan,
                &client,
                &session,
                &blacklist,
                &drm,
                track_kind,
                cmcd.as_ref(),
                &http_retry,
            )
            .await?;
        }

        wait_for_fetch_capacity(
            &buffer_target,
            &mut buffer_rx,
            media_segments_delivered,
            &playback,
            seek_generation_at_start,
            &BufferEstimatePublish {
                playback: &playback,
                track_idx,
                buffer_tx: &buffer_tx,
                metrics: &metrics,
                event_tx: &tx,
            },
        )
        .await;
        if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
            return Ok(None);
        }

        let buffer_s =
            publish_buffer_estimate(&playback, track_idx, &buffer_tx, &metrics, &tx, true);
        let batch_cap = prefetch_batch_len(
            &buffer_target,
            buffer_s,
            media_segments_delivered,
            segments.len() - cursor,
        );
        if batch_cap == 0 {
            continue;
        }

        feed_abr_live_inputs(abr.as_mut(), &playback);
        abr.update_buffer(buffer_s);

        let plan_ctx = SegmentPlanContext {
            segment_start_index,
            primary_period_adaptation_index: period_adaptation_index,
            adaptation_sets: &adaptation_sets,
            bitstream_switching: &bitstream_switching,
            set_availability,
            timeline_ctx: &timeline_ctx,
            cached_inits: &encrypted_init_by_rep,
            last_quality_index,
            quality_constraints: playback.quality_constraints(),
            dropped_frames: Some(&dropped_frames),
        };

        let first_plan = plan_segment(abr.as_mut(), buffer_s, &segments[cursor], cursor, &plan_ctx);

        // LL-DASH chunked segments stay sequential (progressive emit).
        if first_plan.chunked {
            let t0 = Instant::now();
            let (fragments, used_quality_index, seg_for_fetch) = with_media_clock_ticks(
                fetch_cmaf_media_with_rep_fallback(
                    &fetch_env,
                    abr.as_ref(),
                    &first_plan,
                    &mut encrypted_init_by_rep,
                    &mut init_signal,
                ),
                &playback,
                track_idx,
                &buffer_tx,
                &metrics,
                &tx,
            )
            .await?;
            let elapsed_s = t0.elapsed().as_secs_f64().max(1e-6);
            let download_duration = t0.elapsed();
            let total_bytes: usize = fragments.iter().map(|f| f.len()).sum();
            let throughput_bps = (total_bytes as f64 * 8.0) / elapsed_s;

            record_quality_switch_and_throughput(
                &fetch_env,
                abr.as_mut(),
                &metrics,
                &tx,
                &mut last_quality_index,
                used_quality_index,
                throughput_bps,
                total_bytes,
                download_duration,
                &buffer_rx,
                &dropped_frames,
            )
            .await?;
            update_max_playout_rate_cap(&playback, abr.as_ref(), track_kind, used_quality_index);

            let (rep_period_idx, rep_aset, rep_idx) =
                fetch_env.resolve_quality(abr.as_ref(), used_quality_index);
            let rep = &rep_aset.representations[rep_idx];
            let rep_id = rep.id.as_deref().unwrap_or_default();
            let init_for_decrypt = encrypted_init_by_rep
                .get(&(rep_period_idx, rep_id.to_string()))
                .ok_or(SegmentError::ExhaustedRepresentations)?;

            let fragment_count = fragments.len();
            let start_chunk = segments[cursor].resync_start_chunk.unwrap_or(1);
            for (chunk_idx, fragment) in fragments.into_iter().enumerate() {
                if (chunk_idx as u64 + 1) < start_chunk {
                    continue;
                }
                if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
                    return Ok(None);
                }
                playback.wait_while_paused().await;

                {
                    let mut guard = drm.lock().await;
                    guard
                        .ensure_from_fragments(
                            rep_period_idx,
                            rep_id,
                            init_for_decrypt,
                            Some(fragment.as_ref()),
                        )
                        .await?;
                }

                let data = decrypt_media_fragment(
                    &drm,
                    rep_period_idx,
                    rep_id,
                    init_for_decrypt,
                    fragment,
                )
                .await?;

                if playback.is_paused() {
                    continue;
                }

                let partial = partial_chunk_meta(chunk_idx, fragment_count);
                emit_segment(
                    &tx,
                    &metrics,
                    &period,
                    rep_aset,
                    rep,
                    &seg_for_fetch,
                    data,
                    partial,
                    period_start,
                    track_idx,
                    &mut playback_started_emitted,
                    &playback,
                    &session,
                    prt_reference_id.as_deref(),
                    &buffer_tx,
                );
                // Let event consumers (e.g. WASM → MSE) drain between CMAF chunks.
                tokio::task::yield_now().await;
            }

            let mut delivered_tracker = session.lock_delivered();
            delivered_tracker.mark_delivered(&seg_for_fetch, period_start.as_secs_f64());
            media_segments_delivered += 1;
            cursor += 1;
            continue;
        }

        let mut plans: Vec<SegmentPlan> = Vec::with_capacity(batch_cap);
        plans.push(first_plan);
        let mut speculative_last_q = Some(plans[0].quality_index);

        let mut look = cursor + 1;
        while plans.len() < batch_cap && look < segments.len() {
            {
                let delivered_tracker = session.lock_delivered();
                if delivered_tracker.is_delivered(&segments[look], period_start.as_secs_f64()) {
                    look += 1;
                    continue;
                }
            }
            let plan_ctx = SegmentPlanContext {
                segment_start_index,
                primary_period_adaptation_index: period_adaptation_index,
                adaptation_sets: &adaptation_sets,
                bitstream_switching: &bitstream_switching,
                set_availability,
                timeline_ctx: &timeline_ctx,
                cached_inits: &encrypted_init_by_rep,
                last_quality_index: speculative_last_q,
                quality_constraints: playback.quality_constraints(),
                dropped_frames: Some(&dropped_frames),
            };
            let plan = plan_segment(abr.as_mut(), buffer_s, &segments[look], look, &plan_ctx);
            if plan.chunked {
                break;
            }
            speculative_last_q = Some(plan.quality_index);
            plans.push(plan);
            look += 1;
        }

        let batch_len = plans.len();
        let mut prepared = Vec::with_capacity(batch_len);
        for plan in &plans {
            if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
                return Ok(None);
            }
            prepared.push(
                prepare_media_with_rep_fallback(
                    &fetch_env,
                    abr.as_ref(),
                    plan,
                    &mut encrypted_init_by_rep,
                    &mut sidx_segments_by_rep,
                    &mut per_segment_index_ranges_by_rep,
                    &mut init_signal,
                )
                .await?,
            );
        }

        let t0 = Instant::now();
        let download_results = with_media_clock_ticks(
            join_all(prepared.iter().map(|p| async {
                let started = Instant::now();
                let result = download_prepared_media(&fetch_env, p).await;
                (started.elapsed(), result)
            })),
            &playback,
            track_idx,
            &buffer_tx,
            &metrics,
            &tx,
        )
        .await;
        let _batch_elapsed = t0.elapsed();

        for ((plan, prepared_item), (download_duration, download_result)) in
            plans.into_iter().zip(prepared).zip(download_results)
        {
            if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
                return Ok(None);
            }
            playback.wait_while_paused().await;

            let (download_duration, bytes, used_quality_index, seg_for_fetch) =
                match download_result {
                    Ok(fetched) => (
                        download_duration,
                        fetched.data,
                        prepared_item.quality_index,
                        prepared_item.seg_for_fetch,
                    ),
                    Err(err) if prepared_item.quality_index == 0 => {
                        return Err(err);
                    }
                    Err(_) => {
                        // Media GET failed at the prepared quality; try lower rungs only.
                        let mut fallback_plan = plan;
                        fallback_plan.quality_index = prepared_item.quality_index - 1;
                        let retry_t0 = Instant::now();
                        let (bytes, used_quality_index, seg_for_fetch) =
                            fetch_media_with_rep_fallback(
                                &fetch_env,
                                abr.as_ref(),
                                &fallback_plan,
                                &mut encrypted_init_by_rep,
                                &mut sidx_segments_by_rep,
                                &mut per_segment_index_ranges_by_rep,
                                &mut init_signal,
                            )
                            .await?;
                        (retry_t0.elapsed(), bytes, used_quality_index, seg_for_fetch)
                    }
                };

            let elapsed_s = download_duration.as_secs_f64().max(1e-6);
            let throughput_bps = (bytes.len() as f64 * 8.0) / elapsed_s;

            record_quality_switch_and_throughput(
                &fetch_env,
                abr.as_mut(),
                &metrics,
                &tx,
                &mut last_quality_index,
                used_quality_index,
                throughput_bps,
                bytes.len(),
                download_duration,
                &buffer_rx,
                &dropped_frames,
            )
            .await?;
            update_max_playout_rate_cap(&playback, abr.as_ref(), track_kind, used_quality_index);

            let (rep_period_idx, rep_aset, rep_idx) =
                fetch_env.resolve_quality(abr.as_ref(), used_quality_index);
            let rep = &rep_aset.representations[rep_idx];
            let rep_id = rep.id.as_deref().unwrap_or_default();
            let init_for_decrypt = encrypted_init_by_rep
                .get(&(rep_period_idx, rep_id.to_string()))
                .ok_or(SegmentError::ExhaustedRepresentations)?;

            {
                let mut guard = drm.lock().await;
                guard
                    .ensure_from_fragments(rep_period_idx, rep_id, init_for_decrypt, Some(&bytes))
                    .await?;
            }

            let data = decrypt_media_fragment(
                &drm,
                rep_period_idx,
                rep_id,
                init_for_decrypt,
                Bytes::from(bytes),
            )
            .await?;

            if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
                return Ok(None);
            }
            if playback.is_paused() {
                continue;
            }

            emit_segment(
                &tx,
                &metrics,
                &period,
                rep_aset,
                rep,
                &seg_for_fetch,
                data,
                None,
                period_start,
                track_idx,
                &mut playback_started_emitted,
                &playback,
                &session,
                prt_reference_id.as_deref(),
                &buffer_tx,
            );

            let mut delivered_tracker = session.lock_delivered();
            delivered_tracker.mark_delivered(&seg_for_fetch, period_start.as_secs_f64());
            media_segments_delivered += 1;
        }

        cursor += batch_len;
    }

    Ok(held.map(|events| (track_idx, events)))
}
