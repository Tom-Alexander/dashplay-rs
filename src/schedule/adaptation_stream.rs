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
use crate::drm::DrmSessionCoordinator;
use crate::http::SharedHttpClient;
use crate::manifest::{self, TimelineBuildContext};
use crate::metrics::TrackMetrics;
use crate::playback_control::{PlaybackController, PlaybackState};
use crate::segment::SegmentError;
use crate::segment_blacklist::SegmentBlacklist;
use crate::track_session::TrackSessionState;
use crate::types::PlayerEvent;

use super::buffer_target::{BufferTarget, wait_for_fetch_capacity};
use super::segment_decrypt::decrypt_media_fragment;
use super::segment_emit::{
    emit_segment, latest_buffer_s, partial_chunk_meta, record_quality_switch_and_throughput,
};
use super::segment_fetch::{
    RepFetchEnv, fetch_and_parse_segment_base_index, fetch_and_parse_segment_template_index,
    fetch_cmaf_media_with_rep_fallback, fetch_init_with_rep_fallback,
    fetch_media_with_rep_fallback,
};
use super::segment_plan::{SegmentPlanContext, plan_init, plan_segment};

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
    abr.update_playback_rate(playback.suggested_playback_rate());
}

pub(crate) struct AdaptationStreamContext {
    pub client: SharedHttpClient,
    pub segment_base_ctx: manifest::SegmentBaseContext,
    pub target_time: Option<Duration>,
    pub period_start: Duration,
    pub period: Period,
    pub timeline_ctx: TimelineBuildContext,
    pub template_end_numbers: Option<manifest::SegmentTemplateEndNumbers>,
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
    /// Latest buffer occupancy reported by the consumer (seconds).
    pub buffer_rx: watch::Receiver<f64>,
    /// Prefetch thresholds from `MPD@minBufferTime` and the BOLA buffer ceiling.
    pub buffer_target: BufferTarget,
    pub metrics: TrackMetrics,
    pub playback: PlaybackController,
    pub abr_factory: SharedAbrFactory,
    /// `@referenceId` from `ServiceDescription::Latency` when present.
    pub prt_reference_id: Option<String>,
    /// `OperatingBandwidth` / `OperatingQuality` constraints for this adaptation set.
    pub operating_constraints:
        Option<crate::clock::service_description::ResolvedOperatingConstraints>,
}

/// Run the fragment loop for one adaptation set until segments are exhausted for this manifest snapshot.
pub(crate) async fn run_adaptation_stream(ctx: AdaptationStreamContext) -> Result<(), PlayerError> {
    let AdaptationStreamContext {
        client,
        segment_base_ctx,
        target_time,
        period_start,
        period,
        timeline_ctx,
        template_end_numbers,
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
        buffer_target,
        metrics,
        playback,
        abr_factory,
        prt_reference_id,
        operating_constraints,
    } = ctx;

    let seek_generation_at_start = playback.seek_generation();
    playback.set_state(PlaybackState::Buffering);

    let addressing = manifest::segment_addressing_for_timeline(&period, &adaptation_set)?;

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
        &addressing,
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
            let start_idx = delivered_tracker.advance_start_index(&segments_all, start_idx);
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
            let start_idx = delivered_tracker.advance_start_index(&segments_all, start_idx);
            (segments_all[start_idx..].to_vec(), start_idx)
        }
    };

    let Some(mut abr) = abr_factory.create(
        &adaptation_set,
        &crate::abr::AbrCreateContext {
            operating: operating_constraints.as_ref(),
            segment_duration_s: nominal_segment_duration_s(&segments),
            quality_ladder: Some(quality_ladder.as_slice()),
        },
    ) else {
        return Ok(());
    };

    feed_abr_live_inputs(abr.as_mut(), &playback);
    abr.update_buffer(latest_buffer_s(&buffer_rx));
    metrics.record_buffer(latest_buffer_s(&buffer_rx));

    let init_taken = session.try_take_init();

    // Cache init segments by (Adaptation Set index, Representation ID).
    let mut encrypted_init_by_rep: HashMap<(usize, String), Bytes> = HashMap::new();
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
    };
    if init_taken {
        let init_plan = plan_init(abr.as_mut(), latest_buffer_s(&buffer_rx));
        let init_res: Result<(), PlayerError> = async {
            let (_, rep_id) = fetch_init_with_rep_fallback(
                &fetch_env,
                abr.as_ref(),
                init_plan.quality_index,
                &mut encrypted_init_by_rep,
            )
            .await?;
            let _ = rep_id;
            metrics.set_quality_index(init_plan.quality_index);
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
    let mut playback_started_emitted = false;
    let mut media_segments_delivered = 0usize;

    for (local_idx, seg) in segments.into_iter().enumerate() {
        playback.wait_while_paused().await;
        if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
            return Ok(());
        }

        {
            let delivered_tracker = session.lock_delivered();
            if delivered_tracker.is_delivered(&seg) {
                continue;
            }
        }

        wait_for_fetch_capacity(
            &buffer_target,
            &mut buffer_rx,
            media_segments_delivered,
            &playback,
            seek_generation_at_start,
        )
        .await;
        if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
            return Ok(());
        }

        feed_abr_live_inputs(abr.as_mut(), &playback);
        abr.update_buffer(latest_buffer_s(&buffer_rx));
        metrics.record_buffer(latest_buffer_s(&buffer_rx));
        let plan = plan_segment(
            abr.as_mut(),
            latest_buffer_s(&buffer_rx),
            &seg,
            local_idx,
            &SegmentPlanContext {
                segment_start_index,
                primary_period_adaptation_index: period_adaptation_index,
                adaptation_sets: &adaptation_sets,
                bitstream_switching: &bitstream_switching,
                addressing: &addressing,
                timeline_ctx: &timeline_ctx,
                cached_inits: &encrypted_init_by_rep,
            },
        );

        let t0 = Instant::now();
        if plan.chunked {
            let (fragments, used_quality_index, seg_for_fetch) =
                fetch_cmaf_media_with_rep_fallback(
                    &fetch_env,
                    abr.as_ref(),
                    &plan,
                    &mut encrypted_init_by_rep,
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
            )
            .await?;

            let (rep_period_idx, rep_aset, rep_idx) =
                fetch_env.resolve_quality(abr.as_ref(), used_quality_index);
            let rep = &rep_aset.representations[rep_idx];
            let rep_id = rep.id.as_deref().unwrap_or_default();
            let init_for_decrypt = encrypted_init_by_rep
                .get(&(rep_period_idx, rep_id.to_string()))
                .ok_or(SegmentError::ExhaustedRepresentations)?;

            let fragment_count = fragments.len();
            let start_chunk = seg.resync_start_chunk.unwrap_or(1);
            for (chunk_idx, fragment) in fragments.into_iter().enumerate() {
                if (chunk_idx as u64 + 1) < start_chunk {
                    continue;
                }
                if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
                    return Ok(());
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
                );
            }

            let mut delivered_tracker = session.lock_delivered();
            delivered_tracker.mark_delivered(&seg_for_fetch);
            media_segments_delivered += 1;
            continue;
        }

        let (bytes, used_quality_index, seg_for_fetch) = fetch_media_with_rep_fallback(
            &fetch_env,
            abr.as_ref(),
            &plan,
            &mut encrypted_init_by_rep,
            &mut sidx_segments_by_rep,
            &mut per_segment_index_ranges_by_rep,
        )
        .await?;
        let elapsed_s = t0.elapsed().as_secs_f64().max(1e-6);
        let download_duration = t0.elapsed();
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
        )
        .await?;

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
            return Ok(());
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
        );

        let mut delivered_tracker = session.lock_delivered();
        delivered_tracker.mark_delivered(&seg_for_fetch);
        media_segments_delivered += 1;
    }

    Ok(())
}
