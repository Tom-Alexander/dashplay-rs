//! Segment fetch orchestration for one adaptation set.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period, Representation};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{broadcast, watch};

use crate::PlayerError;
use crate::abr::{AbrController, quality_indices_for_fallback};
use crate::delivered_segments::DeliveredSegmentTracker;
use crate::drm::License;
use crate::drm::coordinator::DrmSessionCoordinator;
use crate::http::SharedHttpClient;
use crate::manifest::{self, TimelineBuildContext};
use crate::media_events;
use crate::metrics::TrackMetrics;
use crate::partial_segment;
use crate::playback_control::{PlaybackController, PlaybackState};
use crate::prft;
use crate::resync::ProducerReferenceAnchor;
use crate::segment_blacklist::SegmentBlacklist;
use crate::segment_fetcher::{
    fetch_bytes_with_base_failover, fetch_bytes_with_base_failover_and_range,
};
use crate::types::{PartialSegmentChunk, PlayerEvent};

pub(super) fn partial_chunk_meta(
    chunk_idx: usize,
    fragment_count: usize,
) -> Option<PartialSegmentChunk> {
    if fragment_count <= 1 {
        return None;
    }
    Some(PartialSegmentChunk {
        index: chunk_idx as u64 + 1,
        is_final: chunk_idx + 1 == fragment_count,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_segment(
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

pub(super) fn segment_presentation_time(
    period_start: Duration,
    seg: &manifest::TimelineSegment,
) -> Duration {
    period_start + Duration::from_secs_f64(seg.presentation_time_s.max(0.0))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn record_quality_switch_and_throughput(
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

pub(super) async fn decrypt_media_fragment(
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

pub(super) fn align_start_index_with_resync(
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

pub(super) fn lock_delivered(
    delivered: &Arc<Mutex<DeliveredSegmentTracker>>,
) -> std::sync::MutexGuard<'_, DeliveredSegmentTracker> {
    delivered.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) struct RepFetchEnv<'a> {
    pub(super) client: &'a SharedHttpClient,
    pub(super) segment_base_ctx: &'a manifest::SegmentBaseContext,
    pub(super) period: &'a Period,
    pub(super) adaptation_set: &'a AdaptationSet,
    pub(super) blacklist: &'a SegmentBlacklist,
    pub(super) drm: &'a Arc<AsyncMutex<DrmSessionCoordinator>>,
    pub(super) period_adaptation_index: usize,
    pub(super) tx: &'a broadcast::Sender<PlayerEvent>,
}

pub(super) struct MediaFetchParams<'a> {
    pub(super) start_quality_index: usize,
    pub(super) seg: &'a manifest::TimelineSegment,
    pub(super) local_idx: usize,
    pub(super) list_idx: usize,
}

pub(super) async fn fetch_init_with_rep_fallback(
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

pub(super) async fn fetch_media_with_rep_fallback(
    env: &RepFetchEnv<'_>,
    abr: &dyn AbrController,
    params: MediaFetchParams<'_>,
    encrypted_init_by_rep: &mut HashMap<String, Bytes>,
    sidx_segments_by_rep: &mut HashMap<String, Vec<manifest::TimelineSegment>>,
    per_segment_index_ranges_by_rep: &mut HashMap<String, HashMap<u64, manifest::ByteRange>>,
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
        match rep_addressing {
            manifest::SegmentAddressing::Base(ref sb) if sb.indexRange.is_some() => {
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
            manifest::SegmentAddressing::Template(ref st)
                if manifest::segment_template_uses_global_sidecar_index(st) =>
            {
                let rep_segs = sidx_segments_for_rep_template(
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
            manifest::SegmentAddressing::Template(ref st)
                if manifest::segment_template_uses_per_segment_index(st) =>
            {
                if let Some(media_range) = media_range_for_per_segment_index(
                    env,
                    rep,
                    &seg_for_fetch,
                    per_segment_index_ranges_by_rep,
                )
                .await?
                {
                    seg_for_fetch.media_range = Some(media_range);
                }
            }
            _ => {}
        }
        let base_vars = manifest::template_vars_for_representation(rep, env.adaptation_set);
        let init_path = manifest::resolved_initialization_path(&rep_addressing, &base_vars);
        let template_vars = manifest::TemplateVars {
            number: Some(seg_for_fetch.number),
            time: Some(seg_for_fetch.time),
            sub_number: seg_for_fetch.sub_number,
            initialization: init_path.as_deref(),
            ..base_vars
        };
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

pub(super) async fn fetch_cmaf_media_with_rep_fallback(
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
        let base_vars = manifest::template_vars_for_representation(rep, env.adaptation_set);
        let init_path = manifest::resolved_initialization_path(&rep_addressing, &base_vars);
        let template_vars = manifest::TemplateVars {
            number: Some(seg_for_fetch.number),
            time: Some(seg_for_fetch.time),
            sub_number: seg_for_fetch.sub_number,
            initialization: init_path.as_deref(),
            ..base_vars
        };
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

pub(super) async fn ensure_init_for_rep(
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
    let template_vars = manifest::template_vars_for_representation(rep, env.adaptation_set);
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

pub(super) fn latest_buffer_s(buffer_rx: &watch::Receiver<f64>) -> f64 {
    *buffer_rx.borrow()
}

pub(super) fn init_target_for_addressing(
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

pub(super) fn media_target_for_addressing(
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
                        number: Some(seg.number),
                        time: Some(seg.time),
                        sub_number: seg.sub_number,
                        ..*vars
                    },
                ),
                range: seg.media_range,
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

pub(super) async fn fetch_segment_target(
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

pub(super) async fn sidx_segments_for_rep_template<'a>(
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
        let merged_st = manifest::segment_template_for_representation(period, adaptation_set, rep)?;
        if manifest::segment_template_uses_per_segment_index(&merged_st) {
            e.insert(Vec::new());
        } else {
            let bases =
                manifest::segment_bases_for_representation(segment_base_ctx, adaptation_set, rep)?;
            let vars = manifest::template_vars_for_representation(rep, adaptation_set);
            let index_target = manifest::segment_template_index_target(&merged_st, &vars)?;
            let index_bytes =
                fetch_segment_target(client, &bases, &index_target, blacklist).await?;
            let segs = manifest::parse_sidx_index_from_template(&merged_st, &index_bytes)?;
            e.insert(segs);
        }
    }
    Ok(cache
        .get(rep.id.as_deref().unwrap_or_default())
        .map(|v| v.as_slice())
        .unwrap_or(&[]))
}

pub(super) async fn media_range_for_per_segment_index(
    env: &RepFetchEnv<'_>,
    rep: &Representation,
    seg: &manifest::TimelineSegment,
    cache: &mut HashMap<String, HashMap<u64, manifest::ByteRange>>,
) -> Result<Option<manifest::ByteRange>, PlayerError> {
    let merged_st =
        manifest::segment_template_for_representation(env.period, env.adaptation_set, rep)?;
    if !manifest::segment_template_uses_per_segment_index(&merged_st) {
        return Ok(None);
    }

    let rep_id = rep.id.as_deref().unwrap_or_default().to_string();
    let per_rep = cache.entry(rep_id).or_default();
    if let Some(media_range) = per_rep.get(&seg.number) {
        return Ok(Some(*media_range));
    }

    let bases =
        manifest::segment_bases_for_representation(env.segment_base_ctx, env.adaptation_set, rep)?;
    let base_vars = manifest::template_vars_for_representation(rep, env.adaptation_set);
    let vars = manifest::TemplateVars {
        number: Some(seg.number),
        time: Some(seg.time),
        sub_number: seg.sub_number,
        ..base_vars
    };
    let index_target = manifest::segment_template_index_target(&merged_st, &vars)?;
    let index_bytes =
        fetch_segment_target(env.client, &bases, &index_target, env.blacklist).await?;
    let media_range = manifest::media_range_from_per_segment_index(&merged_st, &index_bytes)?;
    per_rep.insert(seg.number, media_range);
    Ok(Some(media_range))
}

pub(super) async fn sidx_segments_for_rep<'a>(
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
