//! One DASH stream: initialization + media segments for a single AdaptationSet
//! (dash.js: `Stream` + schedule / fragment pipeline for that stream).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::time::Instant;

use super::PlayerError;
use super::abr::{AbrController, SharedAbrFactory, quality_indices_for_fallback};
use super::delivered_segments::DeliveredSegmentTracker;
use super::drm::License;
use super::drm::coordinator::DrmSessionCoordinator;
use super::http::SharedHttpClient;
use super::manifest::{self, TimelineBuildContext};
use super::media_events;
use super::metrics::TrackMetrics;
use super::partial_segment;
use super::playback_control::{PlaybackController, PlaybackState};
use super::prft;
use super::resync::ProducerReferenceAnchor;
use super::segment_blacklist::SegmentBlacklist;
use super::segment_fetcher::{
    fetch_bytes_with_base_failover, fetch_bytes_with_base_failover_and_range,
};
use super::types::{PartialSegmentChunk, PlayerEvent};
use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period, Representation};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::broadcast;
use tokio::sync::watch;

pub(crate) struct AdaptationStreamContext {
    pub client: SharedHttpClient,
    pub segment_base_ctx: manifest::SegmentBaseContext,
    pub target_time: Option<Duration>,
    pub period_start: Duration,
    pub period: Period,
    pub timeline_ctx: TimelineBuildContext,
    pub adaptation_set: AdaptationSet,
    /// Index into the session's selected `PlayerTrack` list.
    pub track_idx: usize,
    /// Index into `Period.adaptations` for DRM session lookup.
    pub period_adaptation_index: usize,
    pub tx: broadcast::Sender<PlayerEvent>,
    pub have_init: Arc<Vec<AtomicBool>>,
    pub delivered: Arc<Mutex<DeliveredSegmentTracker>>,
    pub blacklist: SegmentBlacklist,
    pub drm: Arc<AsyncMutex<DrmSessionCoordinator>>,
    /// Latest buffer occupancy reported by the consumer (seconds).
    pub buffer_rx: watch::Receiver<f64>,
    pub metrics: TrackMetrics,
    pub playback: PlaybackController,
    pub abr_factory: SharedAbrFactory,
    /// Latest in-band `prft` anchor for `ProducerReferenceTime@inband=true` clock correction.
    pub inband_prt_anchor: Arc<Mutex<Option<ProducerReferenceAnchor>>>,
    /// `@referenceId` from `ServiceDescription::Latency` when present.
    pub prt_reference_id: Option<String>,
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
        adaptation_set,
        track_idx,
        period_adaptation_index,
        tx,
        have_init,
        delivered,
        blacklist,
        drm,
        buffer_rx,
        metrics,
        playback,
        abr_factory,
        inband_prt_anchor,
        prt_reference_id,
    } = ctx;

    let seek_generation_at_start = playback.seek_generation();
    playback.set_state(PlaybackState::Buffering);

    let addressing = manifest::segment_addressing_for_timeline(&period, &adaptation_set)?;

    let segments_all = match &addressing {
        manifest::SegmentAddressing::Base(sb) if sb.indexRange.is_some() => {
            let rep = adaptation_set
                .representations
                .first()
                .ok_or(PlayerError::SegmentExhaustedRepresentations)?;
            let bases = manifest::segment_bases_for_representation(
                &segment_base_ctx,
                &adaptation_set,
                rep,
            )?;
            let rep_addressing =
                manifest::segment_addressing_for_representation(&period, &adaptation_set, rep)?;
            let merged_sb = match rep_addressing {
                manifest::SegmentAddressing::Base(b) => b,
                _ => sb.clone(),
            };
            let index_range = merged_sb
                .indexRange
                .as_deref()
                .ok_or(PlayerError::MissingSegmentBaseIndexRange)?;
            let br = manifest::parse_byte_range(index_range)?;
            let index_bytes =
                fetch_bytes_with_base_failover_and_range(&client, &bases, "", Some(br), &blacklist)
                    .await?;
            manifest::parse_sidx_index(&merged_sb, &index_bytes)?
        }
        _ => manifest::timeline_segments_for_addressing(&addressing, &timeline_ctx)?,
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
        let delivered_tracker = lock_delivered(&delivered);
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

    let Some(mut abr) = abr_factory.create(&adaptation_set) else {
        return Ok(());
    };

    abr.update_buffer(latest_buffer_s(&buffer_rx));
    metrics.record_buffer(latest_buffer_s(&buffer_rx));

    let init_taken = have_init[track_idx]
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();

    // Cache init segments by Representation ID (ABR switches may require different init/boxes/KIDs).
    let mut encrypted_init_by_rep: HashMap<String, Bytes> = HashMap::new();
    let fetch_env = RepFetchEnv {
        client: &client,
        segment_base_ctx: &segment_base_ctx,
        period: &period,
        adaptation_set: &adaptation_set,
        blacklist: &blacklist,
        drm: &drm,
        period_adaptation_index,
        tx: &tx,
    };
    if init_taken {
        let init_res: Result<(), PlayerError> = async {
            let decision = abr.decide();
            let (_, rep_id) = fetch_init_with_rep_fallback(
                &fetch_env,
                abr.as_ref(),
                decision.quality_index,
                &mut encrypted_init_by_rep,
            )
            .await?;
            let _ = rep_id;
            metrics.set_quality_index(decision.quality_index);
            Ok(())
        }
        .await;
        if init_res.is_err() {
            have_init[track_idx].store(false, Ordering::Release);
            init_res?;
        }
    }

    let mut sidx_segments_by_rep: HashMap<String, Vec<manifest::TimelineSegment>> = HashMap::new();
    let mut last_quality_index = metrics.last_quality_index();
    let mut playback_started_emitted = false;

    for (local_idx, seg) in segments.into_iter().enumerate() {
        playback.wait_while_paused().await;
        if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
            return Ok(());
        }

        {
            let delivered_tracker = lock_delivered(&delivered);
            if delivered_tracker.is_delivered(&seg) {
                continue;
            }
        }

        abr.update_buffer(latest_buffer_s(&buffer_rx));
        metrics.record_buffer(latest_buffer_s(&buffer_rx));
        let decision = abr.decide();
        let list_idx = segment_start_index + local_idx;
        let set_availability = manifest::SegmentAvailability::from_addressing(&addressing);
        let chunked = timeline_ctx.is_dynamic
            && manifest::uses_chunked_segment_transfer(&set_availability, &seg);

        let t0 = Instant::now();
        if chunked {
            let (fragments, used_quality_index, seg_for_fetch) =
                fetch_cmaf_media_with_rep_fallback(
                    &fetch_env,
                    abr.as_ref(),
                    MediaFetchParams {
                        start_quality_index: decision.quality_index,
                        seg: &seg,
                        local_idx,
                        list_idx,
                    },
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

            let rep_idx = abr.representation_index_for_quality_index(used_quality_index);
            let rep = &adaptation_set.representations[rep_idx];
            let rep_id = rep.id.as_deref().unwrap_or_default();
            let init_for_decrypt = encrypted_init_by_rep
                .get(rep_id)
                .ok_or(PlayerError::SegmentExhaustedRepresentations)?;

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
                            period_adaptation_index,
                            rep_id,
                            init_for_decrypt,
                            Some(fragment.as_ref()),
                        )
                        .await?;
                }

                let data = decrypt_media_fragment(
                    &drm,
                    period_adaptation_index,
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
                    &adaptation_set,
                    rep,
                    &seg_for_fetch,
                    data,
                    partial,
                    period_start,
                    track_idx,
                    &mut playback_started_emitted,
                    &playback,
                    &inband_prt_anchor,
                    prt_reference_id.as_deref(),
                );
            }

            let mut delivered_tracker = lock_delivered(&delivered);
            delivered_tracker.mark_delivered(&seg_for_fetch);
            continue;
        }

        let (bytes, used_quality_index, seg_for_fetch) = fetch_media_with_rep_fallback(
            &fetch_env,
            abr.as_ref(),
            MediaFetchParams {
                start_quality_index: decision.quality_index,
                seg: &seg,
                local_idx,
                list_idx,
            },
            &mut encrypted_init_by_rep,
            &mut sidx_segments_by_rep,
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

        let rep_idx = abr.representation_index_for_quality_index(used_quality_index);
        let rep = &adaptation_set.representations[rep_idx];
        let rep_id = rep.id.as_deref().unwrap_or_default();
        let init_for_decrypt = encrypted_init_by_rep
            .get(rep_id)
            .ok_or(PlayerError::SegmentExhaustedRepresentations)?;

        {
            let mut guard = drm.lock().await;
            guard
                .ensure_from_fragments(
                    period_adaptation_index,
                    rep_id,
                    init_for_decrypt,
                    Some(&bytes),
                )
                .await?;
        }

        let data = decrypt_media_fragment(
            &drm,
            period_adaptation_index,
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
            &adaptation_set,
            rep,
            &seg_for_fetch,
            data,
            None,
            period_start,
            track_idx,
            &mut playback_started_emitted,
            &playback,
            &inband_prt_anchor,
            prt_reference_id.as_deref(),
        );

        let mut delivered_tracker = lock_delivered(&delivered);
        delivered_tracker.mark_delivered(&seg_for_fetch);
    }

    Ok(())
}

fn partial_chunk_meta(chunk_idx: usize, fragment_count: usize) -> Option<PartialSegmentChunk> {
    if fragment_count <= 1 {
        return None;
    }
    Some(PartialSegmentChunk {
        index: chunk_idx as u64 + 1,
        is_final: chunk_idx + 1 == fragment_count,
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_segment(
    tx: &broadcast::Sender<PlayerEvent>,
    metrics: &TrackMetrics,
    period: &Period,
    adaptation_set: &AdaptationSet,
    rep: &Representation,
    seg: &manifest::TimelineSegment,
    data: Bytes,
    partial: Option<PartialSegmentChunk>,
    period_start: Duration,
    track_idx: usize,
    playback_started_emitted: &mut bool,
    playback: &PlaybackController,
    inband_prt_anchor: &Arc<Mutex<Option<ProducerReferenceAnchor>>>,
    prt_reference_id: Option<&str>,
) {
    prft::maybe_update_inband_anchor_from_segment(
        data.as_ref(),
        period,
        adaptation_set,
        rep,
        prt_reference_id,
        inband_prt_anchor,
    );

    let inband_filters = media_events::inband_event_streams_for_representation(adaptation_set, rep);
    for event in media_events::inband_events_from_segment(
        data.as_ref(),
        &inband_filters,
        seg.number,
        seg.time,
        seg.sub_number,
    ) {
        let _ = tx.send(PlayerEvent::MediaEvent(event));
    }

    let presentation_time = segment_presentation_time(period_start, seg);

    let _ = tx.send(PlayerEvent::Segment {
        number: seg.number,
        time: seg.time,
        presentation_time,
        sub_number: seg.sub_number,
        partial,
        data,
    });
    if playback.record_segment_delivery(track_idx, presentation_time) {
        let _ = tx.send(PlayerEvent::PlayheadUpdated {
            presentation_time: playback.presentation_time(),
        });
    }
    metrics.record_segment_delivered();

    if !*playback_started_emitted {
        let _ = tx.send(PlayerEvent::PlaybackStarted);
        *playback_started_emitted = true;
    }

    if playback.state() != PlaybackState::Playing {
        playback.set_state(PlaybackState::Playing);
    }
}

fn segment_presentation_time(period_start: Duration, seg: &manifest::TimelineSegment) -> Duration {
    period_start + Duration::from_secs_f64(seg.presentation_time_s.max(0.0))
}

#[allow(clippy::too_many_arguments)]
async fn record_quality_switch_and_throughput(
    env: &RepFetchEnv<'_>,
    abr: &mut dyn AbrController,
    metrics: &TrackMetrics,
    tx: &broadcast::Sender<PlayerEvent>,
    last_quality_index: &mut Option<usize>,
    used_quality_index: usize,
    throughput_bps: f64,
    byte_len: usize,
    download_duration: Duration,
    buffer_rx: &watch::Receiver<f64>,
) -> Result<(), PlayerError> {
    let _ = env;
    metrics.record_throughput(throughput_bps, byte_len, download_duration);
    if let Some(prev_q) = *last_quality_index {
        if prev_q != used_quality_index {
            let from_bitrate_bps = abr.bitrate_bps_for_quality_index(prev_q);
            let to_bitrate_bps = abr.bitrate_bps_for_quality_index(used_quality_index);
            metrics.record_bitrate_switch(
                prev_q,
                used_quality_index,
                from_bitrate_bps,
                to_bitrate_bps,
            );
            let _ = tx.send(PlayerEvent::BitrateChanged {
                from_quality_index: prev_q,
                to_quality_index: used_quality_index,
                from_bitrate_bps,
                to_bitrate_bps,
            });
        }
    } else {
        metrics.set_quality_index(used_quality_index);
    }
    *last_quality_index = Some(used_quality_index);

    abr.observe_segment_download(throughput_bps, byte_len, used_quality_index);
    abr.update_buffer(latest_buffer_s(buffer_rx));
    metrics.record_buffer(latest_buffer_s(buffer_rx));
    Ok(())
}

async fn decrypt_media_fragment(
    drm: &Arc<AsyncMutex<DrmSessionCoordinator>>,
    period_adaptation_index: usize,
    rep_id: &str,
    init_bytes: &Bytes,
    data: Bytes,
) -> Result<Bytes, PlayerError> {
    let license = {
        let guard = drm.lock().await;
        guard.license_for_rep(period_adaptation_index, rep_id)
    };
    let Some(lic) = license else {
        return Ok(data);
    };

    match lic.decrypt(&data, Some(init_bytes)) {
        Ok(decrypted) => Ok(decrypted),
        Err(e) if License::is_likely_missing_key(&e) => {
            let mut guard = drm.lock().await;
            guard
                .recover_from_decrypt_failure(
                    period_adaptation_index,
                    rep_id,
                    init_bytes,
                    data.as_ref(),
                )
                .await?;
            let refreshed = guard.license_for_rep(period_adaptation_index, rep_id);
            drop(guard);
            let Some(new_lic) = refreshed else {
                return Err(PlayerError::License(e));
            };
            new_lic
                .decrypt(&data, Some(init_bytes))
                .map_err(PlayerError::License)
        }
        Err(e) => {
            let msg = e.to_string().to_ascii_lowercase();
            if msg.contains("not encrypted") || msg.contains("no") && msg.contains("senc") {
                Ok(data)
            } else {
                Err(PlayerError::License(e))
            }
        }
    }
}

fn align_start_index_with_resync(
    segments: &[manifest::TimelineSegment],
    start_idx: usize,
    timeline_ctx: &TimelineBuildContext,
    target_presentation_time_s: Option<f64>,
) -> (usize, Option<u64>) {
    let Some(hints) = timeline_ctx.resync_hints else {
        return (start_idx, None);
    };

    if hints.random_access_within_segment {
        if let Some(target) = target_presentation_time_s {
            return manifest::mid_segment_resync_alignment(segments, start_idx, target, hints);
        }
    }

    (
        manifest::align_start_index_to_resync(segments, start_idx, hints),
        None,
    )
}

fn lock_delivered(
    delivered: &Arc<Mutex<DeliveredSegmentTracker>>,
) -> std::sync::MutexGuard<'_, DeliveredSegmentTracker> {
    delivered.lock().unwrap_or_else(|e| e.into_inner())
}

struct RepFetchEnv<'a> {
    client: &'a SharedHttpClient,
    segment_base_ctx: &'a manifest::SegmentBaseContext,
    period: &'a Period,
    adaptation_set: &'a AdaptationSet,
    blacklist: &'a SegmentBlacklist,
    drm: &'a Arc<AsyncMutex<DrmSessionCoordinator>>,
    period_adaptation_index: usize,
    tx: &'a broadcast::Sender<PlayerEvent>,
}

struct MediaFetchParams<'a> {
    start_quality_index: usize,
    seg: &'a manifest::TimelineSegment,
    local_idx: usize,
    list_idx: usize,
}

async fn fetch_init_with_rep_fallback(
    env: &RepFetchEnv<'_>,
    abr: &dyn AbrController,
    start_quality_index: usize,
    encrypted_init_by_rep: &mut HashMap<String, Bytes>,
) -> Result<(Bytes, String), PlayerError> {
    let mut last_err = PlayerError::SegmentExhaustedRepresentations;
    for quality_index in quality_indices_for_fallback(start_quality_index) {
        let rep_idx = abr.representation_index_for_quality_index(quality_index);
        let rep = &env.adaptation_set.representations[rep_idx];
        match ensure_init_for_rep(env, rep, encrypted_init_by_rep).await {
            Ok(init_bytes) => {
                let rep_id = rep.id.as_deref().unwrap_or_default().to_string();
                return Ok((init_bytes, rep_id));
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn fetch_media_with_rep_fallback(
    env: &RepFetchEnv<'_>,
    abr: &dyn AbrController,
    params: MediaFetchParams<'_>,
    encrypted_init_by_rep: &mut HashMap<String, Bytes>,
    sidx_segments_by_rep: &mut HashMap<String, Vec<manifest::TimelineSegment>>,
) -> Result<(Vec<u8>, usize, manifest::TimelineSegment), PlayerError> {
    let mut last_err = PlayerError::SegmentExhaustedRepresentations;
    for quality_index in quality_indices_for_fallback(params.start_quality_index) {
        let rep_idx = abr.representation_index_for_quality_index(quality_index);
        let rep = &env.adaptation_set.representations[rep_idx];
        let bases = manifest::segment_bases_for_representation(
            env.segment_base_ctx,
            env.adaptation_set,
            rep,
        )?;
        match ensure_init_for_rep(env, rep, encrypted_init_by_rep).await {
            Ok(_) => {}
            Err(e) => {
                last_err = e;
                continue;
            }
        }

        let rep_addressing =
            manifest::segment_addressing_for_representation(env.period, env.adaptation_set, rep)?;
        let mut seg_for_fetch = params.seg.clone();
        if let manifest::SegmentAddressing::Base(ref sb) = rep_addressing {
            if sb.indexRange.is_some() {
                let rep_segs = sidx_segments_for_rep(
                    env.client,
                    env.segment_base_ctx,
                    env.period,
                    env.adaptation_set,
                    rep,
                    env.blacklist,
                    sidx_segments_by_rep,
                )
                .await?;
                if let Some(rep_seg) = rep_segs.get(params.local_idx) {
                    seg_for_fetch.media_range = rep_seg.media_range;
                }
            }
        }
        let template_vars = manifest::template_vars_for_representation(rep);
        let seg_target = media_target_for_addressing(
            &rep_addressing,
            &seg_for_fetch,
            params.list_idx,
            &template_vars,
        )?;
        match fetch_segment_target(env.client, &bases, &seg_target, env.blacklist).await {
            Ok(bytes) => return Ok((bytes, quality_index, seg_for_fetch)),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn fetch_cmaf_media_with_rep_fallback(
    env: &RepFetchEnv<'_>,
    abr: &dyn AbrController,
    params: MediaFetchParams<'_>,
    encrypted_init_by_rep: &mut HashMap<String, Bytes>,
) -> Result<(Vec<Bytes>, usize, manifest::TimelineSegment), PlayerError> {
    let mut last_err = PlayerError::SegmentExhaustedRepresentations;
    for quality_index in quality_indices_for_fallback(params.start_quality_index) {
        let rep_idx = abr.representation_index_for_quality_index(quality_index);
        let rep = &env.adaptation_set.representations[rep_idx];
        let bases = manifest::segment_bases_for_representation(
            env.segment_base_ctx,
            env.adaptation_set,
            rep,
        )?;
        match ensure_init_for_rep(env, rep, encrypted_init_by_rep).await {
            Ok(_) => {}
            Err(e) => {
                last_err = e;
                continue;
            }
        }

        let rep_addressing =
            manifest::segment_addressing_for_representation(env.period, env.adaptation_set, rep)?;
        let seg_for_fetch = params.seg.clone();
        let template_vars = manifest::template_vars_for_representation(rep);
        let seg_target = media_target_for_addressing(
            &rep_addressing,
            &seg_for_fetch,
            params.list_idx,
            &template_vars,
        )?;
        match partial_segment::fetch_cmaf_fragments_for_target(
            env.client,
            &bases,
            &seg_target,
            env.blacklist,
        )
        .await
        {
            Ok(fragments) if !fragments.is_empty() => {
                return Ok((fragments, quality_index, seg_for_fetch));
            }
            Ok(_) => last_err = PlayerError::SegmentExhaustedRepresentations,
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn ensure_init_for_rep(
    env: &RepFetchEnv<'_>,
    rep: &Representation,
    encrypted_init_by_rep: &mut HashMap<String, Bytes>,
) -> Result<Bytes, PlayerError> {
    let rep_id = rep.id.as_deref().unwrap_or_default();
    if let Some(init) = encrypted_init_by_rep.get(rep_id) {
        return Ok(init.clone());
    }

    let bases =
        manifest::segment_bases_for_representation(env.segment_base_ctx, env.adaptation_set, rep)?;
    let rep_addressing =
        manifest::segment_addressing_for_representation(env.period, env.adaptation_set, rep)?;
    let template_vars = manifest::template_vars_for_representation(rep);
    let Some(init_target) = init_target_for_addressing(&rep_addressing, &template_vars)? else {
        encrypted_init_by_rep.insert(rep_id.to_string(), Bytes::new());
        return Ok(Bytes::new());
    };
    let bytes = fetch_segment_target(env.client, &bases, &init_target, env.blacklist).await?;
    let init_bytes = Bytes::from(bytes);
    encrypted_init_by_rep.insert(rep_id.to_string(), init_bytes.clone());

    {
        let mut guard = env.drm.lock().await;
        guard
            .ensure_from_fragments(env.period_adaptation_index, rep_id, &init_bytes, None)
            .await?;
    }

    let license = {
        let guard = env.drm.lock().await;
        guard.license_for_rep(env.period_adaptation_index, rep_id)
    };

    let out = if let Some(ref lic) = license {
        match lic.decrypt(&init_bytes, Option::<&Bytes>::None) {
            Ok(decrypted) => decrypted,
            Err(e) if License::is_likely_missing_key(&e) => {
                let mut guard = env.drm.lock().await;
                guard
                    .recover_from_decrypt_failure(
                        env.period_adaptation_index,
                        rep_id,
                        &init_bytes,
                        &[],
                    )
                    .await?;
                let refreshed = guard.license_for_rep(env.period_adaptation_index, rep_id);
                drop(guard);
                refreshed
                    .ok_or(PlayerError::License(e))?
                    .decrypt(&init_bytes, Option::<&Bytes>::None)
                    .map_err(PlayerError::License)?
            }
            Err(e) => return Err(PlayerError::License(e)),
        }
    } else {
        init_bytes.clone()
    };
    let _ = env.tx.send(PlayerEvent::Init(out));
    Ok(init_bytes)
}

fn latest_buffer_s(buffer_rx: &watch::Receiver<f64>) -> f64 {
    *buffer_rx.borrow()
}

fn init_target_for_addressing(
    addressing: &manifest::SegmentAddressing,
    vars: &manifest::TemplateVars<'_>,
) -> Result<Option<manifest::SegmentFetchTarget>, PlayerError> {
    match addressing {
        manifest::SegmentAddressing::Template(st) => {
            Ok(st
                .initialization
                .as_deref()
                .map(|init_tpl| manifest::SegmentFetchTarget {
                    path: manifest::interpolate_template(init_tpl, vars),
                    range: None,
                }))
        }
        manifest::SegmentAddressing::List(sl) => Ok(manifest::segment_list_init_source(sl)
            .ok()
            .map(|init_src| manifest::SegmentFetchTarget {
                path: manifest::interpolate_template(init_src, vars),
                range: None,
            })),
        manifest::SegmentAddressing::Base(sb) => {
            manifest::segment_base_init_target(sb, vars).map(Some)
        }
    }
}

fn media_target_for_addressing(
    addressing: &manifest::SegmentAddressing,
    seg: &manifest::TimelineSegment,
    list_idx: usize,
    vars: &manifest::TemplateVars<'_>,
) -> Result<manifest::SegmentFetchTarget, PlayerError> {
    match addressing {
        manifest::SegmentAddressing::Template(st) => {
            let media_tpl = st
                .media
                .as_deref()
                .ok_or(PlayerError::MissingMediaTemplate)?;
            Ok(manifest::SegmentFetchTarget {
                path: manifest::interpolate_template(
                    media_tpl,
                    &manifest::TemplateVars {
                        representation_id: vars.representation_id,
                        bandwidth: vars.bandwidth,
                        number: Some(seg.number),
                        time: Some(seg.time),
                        sub_number: seg.sub_number,
                    },
                ),
                range: None,
            })
        }
        manifest::SegmentAddressing::List(sl) => {
            let path = if let Some(url) = seg.media_url.as_deref() {
                url.to_string()
            } else {
                manifest::segment_list_media_for_index(sl, list_idx)?.to_string()
            };
            Ok(manifest::SegmentFetchTarget { path, range: None })
        }
        manifest::SegmentAddressing::Base(sb) => manifest::segment_base_media_target(sb, seg, vars),
    }
}

async fn fetch_segment_target(
    client: &SharedHttpClient,
    bases: &[url::Url],
    target: &manifest::SegmentFetchTarget,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<u8>, PlayerError> {
    if target.range.is_some() {
        return fetch_bytes_with_base_failover_and_range(
            client,
            bases,
            &target.path,
            target.range,
            blacklist,
        )
        .await;
    }
    fetch_bytes_with_base_failover(client, bases, &target.path, blacklist).await
}

async fn sidx_segments_for_rep<'a>(
    client: &SharedHttpClient,
    segment_base_ctx: &manifest::SegmentBaseContext,
    period: &Period,
    adaptation_set: &AdaptationSet,
    rep: &Representation,
    blacklist: &SegmentBlacklist,
    cache: &'a mut HashMap<String, Vec<manifest::TimelineSegment>>,
) -> Result<&'a [manifest::TimelineSegment], PlayerError> {
    let rep_id = rep.id.as_deref().unwrap_or_default().to_string();
    if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(rep_id) {
        let rep_addressing =
            manifest::segment_addressing_for_representation(period, adaptation_set, rep)?;
        let sb = match rep_addressing {
            manifest::SegmentAddressing::Base(sb) => sb,
            _ => return Ok(&[]),
        };
        let index_range = sb
            .indexRange
            .as_deref()
            .ok_or(PlayerError::MissingSegmentBaseIndexRange)?;
        let bases =
            manifest::segment_bases_for_representation(segment_base_ctx, adaptation_set, rep)?;
        let br = manifest::parse_byte_range(index_range)?;
        let index_bytes =
            fetch_bytes_with_base_failover_and_range(client, &bases, "", Some(br), blacklist)
                .await?;
        let segs = manifest::parse_sidx_index(&sb, &index_bytes)?;
        e.insert(segs);
    }
    Ok(cache
        .get(rep.id.as_deref().unwrap_or_default())
        .map(|v| v.as_slice())
        .unwrap_or(&[]))
}
