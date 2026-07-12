//! Segment URL resolution, HTTP fetch, and representation fallback.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period, Representation};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::broadcast;

use crate::PlayerError;
use crate::abr::{AbrController, quality_indices_for_fallback};
use crate::drm::License;
use crate::drm::coordinator::DrmSessionCoordinator;
use crate::http::SharedHttpClient;
use crate::manifest;
use crate::mp4::partial_segment;
use crate::segment_blacklist::SegmentBlacklist;
use crate::segment_fetcher::{
    fetch_bytes_with_base_failover, fetch_bytes_with_base_failover_and_range,
};
use crate::types::PlayerEvent;

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

async fn sidx_segments_for_rep_template<'a>(
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

async fn media_range_for_per_segment_index(
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
