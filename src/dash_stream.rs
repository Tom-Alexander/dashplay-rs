//! One DASH stream: initialization + media segments for a single AdaptationSet
//! (dash.js: `Stream` + schedule / fragment pipeline for that stream).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::time::Instant;

use super::PlayerError;
use super::abr_controller::AbrController;
use super::delivered_segments::DeliveredSegmentTracker;
use super::drm::License;
use super::drm::coordinator::DrmSessionCoordinator;
use super::manifest::{self, TimelineBuildContext};
use super::metrics::TrackMetrics;
use super::playback_control::{PlaybackController, PlaybackState};
use super::segment_blacklist::SegmentBlacklist;
use super::segment_fetcher::{
    fetch_bytes_with_base_failover, fetch_bytes_with_base_failover_and_range,
};
use super::types::PlayerEvent;
use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period, Representation};
use reqwest::Client;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::broadcast;
use tokio::sync::watch;

pub(crate) struct AdaptationStreamContext {
    pub client: Client,
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

    // Align every adaptation set to the same media instant: pick the first segment whose
    // interval (in MPD time) still contains instants after `target`. Using "last segment with
    // start <= target" breaks A/V sync when audio and video use different segment durations
    // (e.g. 6s audio vs 2s video): each track would start at a different segment start time.
    let (segments, segment_start_index) = {
        let delivered_tracker = lock_delivered(&delivered);
        if let Some(target) = target_time {
            let target_s = target.as_secs_f64();
            let p0 = period_start.as_secs_f64();
            let start_idx = segments_all
                .iter()
                .position(|s| p0 + s.presentation_time_s + s.duration_s > target_s)
                .unwrap_or(0);
            let start_idx =
                manifest::align_start_index_to_sap(&segments_all, start_idx, &adaptation_set);
            let start_idx = delivered_tracker.advance_start_index(&segments_all, start_idx);
            (segments_all[start_idx..].to_vec(), start_idx)
        } else {
            let start_idx = manifest::align_start_index_to_sap(&segments_all, 0, &adaptation_set);
            let start_idx = delivered_tracker.advance_start_index(&segments_all, start_idx);
            (segments_all[start_idx..].to_vec(), start_idx)
        }
    };

    let Some(mut abr) = AbrController::from_adaptation_set(&adaptation_set, 0.3) else {
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
                &abr,
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
        let t0 = Instant::now();
        let (bytes, used_quality_index, seg_for_fetch) = fetch_media_with_rep_fallback(
            &fetch_env,
            &abr,
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

        metrics.record_throughput(throughput_bps, bytes.len(), download_duration);
        if let Some(prev_q) = last_quality_index {
            if prev_q != used_quality_index {
                metrics.record_bitrate_switch(
                    prev_q,
                    used_quality_index,
                    abr.bitrate_bps_for_quality_index(prev_q),
                    abr.bitrate_bps_for_quality_index(used_quality_index),
                );
            }
        } else {
            metrics.set_quality_index(used_quality_index);
        }
        last_quality_index = Some(used_quality_index);

        abr.observe_segment_download(throughput_bps, bytes.len(), used_quality_index);
        abr.update_buffer(latest_buffer_s(&buffer_rx));
        metrics.record_buffer(latest_buffer_s(&buffer_rx));

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

        let _ = tx.send(PlayerEvent::Segment {
            number: seg_for_fetch.number,
            time: seg_for_fetch.time,
            sub_number: seg_for_fetch.sub_number,
            data,
        });
        metrics.record_segment_delivered();

        if playback.state() != PlaybackState::Playing {
            playback.set_state(PlaybackState::Playing);
        }

        let mut delivered_tracker = lock_delivered(&delivered);
        delivered_tracker.mark_delivered(&seg_for_fetch);
    }

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

fn lock_delivered(
    delivered: &Arc<Mutex<DeliveredSegmentTracker>>,
) -> std::sync::MutexGuard<'_, DeliveredSegmentTracker> {
    delivered.lock().unwrap_or_else(|e| e.into_inner())
}

struct RepFetchEnv<'a> {
    client: &'a Client,
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
    abr: &AbrController,
    start_quality_index: usize,
    encrypted_init_by_rep: &mut HashMap<String, Bytes>,
) -> Result<(Bytes, String), PlayerError> {
    let mut last_err = PlayerError::SegmentExhaustedRepresentations;
    for quality_index in abr.quality_indices_for_fallback(start_quality_index) {
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
    abr: &AbrController,
    params: MediaFetchParams<'_>,
    encrypted_init_by_rep: &mut HashMap<String, Bytes>,
    sidx_segments_by_rep: &mut HashMap<String, Vec<manifest::TimelineSegment>>,
) -> Result<(Vec<u8>, usize, manifest::TimelineSegment), PlayerError> {
    let mut last_err = PlayerError::SegmentExhaustedRepresentations;
    for quality_index in abr.quality_indices_for_fallback(params.start_quality_index) {
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
    let init_target = init_target_for_addressing(&rep_addressing, &template_vars)?;
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
) -> Result<manifest::SegmentFetchTarget, PlayerError> {
    match addressing {
        manifest::SegmentAddressing::Template(st) => {
            let init_tpl = st
                .initialization
                .as_deref()
                .ok_or(PlayerError::MissingInitializationTemplate)?;
            Ok(manifest::SegmentFetchTarget {
                path: manifest::interpolate_template(init_tpl, vars),
                range: None,
            })
        }
        manifest::SegmentAddressing::List(sl) => {
            let init_src = manifest::segment_list_init_source(sl)?;
            Ok(manifest::SegmentFetchTarget {
                path: manifest::interpolate_template(init_src, vars),
                range: None,
            })
        }
        manifest::SegmentAddressing::Base(sb) => manifest::segment_base_init_target(sb, vars),
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
    client: &Client,
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
    client: &Client,
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
