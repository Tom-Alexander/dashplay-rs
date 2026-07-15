//! Segment URL resolution, HTTP fetch, and representation fallback.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period, Representation};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::broadcast;

use crate::PlayerError;
use crate::abr::{AbrController, quality_indices_for_fallback};
use crate::cmcd::{CmcdObjectType, CmcdSession, CmsdSnapshot};
use crate::drm::DrmSessionCoordinator;
#[cfg(feature = "drm")]
use crate::drm::{DrmError, License};
use crate::http::SharedHttpClient;
use crate::manifest::{self, ManifestError};
use crate::metrics::TrackMetrics;
use crate::mp4::partial_segment;
use crate::segment::SegmentError;
use crate::segment_blacklist::SegmentBlacklist;
use crate::segment_fetcher::{
    CmcdFetch, FetchedBytes, fetch_bytes_with_base_failover,
    fetch_bytes_with_base_failover_and_range,
};
use crate::track_selection::TrackKind;
use crate::types::PlayerEvent;

use super::segment_plan::SegmentPlan;

pub(super) struct RepFetchEnv<'a> {
    pub(super) client: &'a SharedHttpClient,
    pub(super) segment_base_ctx: &'a manifest::SegmentBaseContext,
    pub(super) period: &'a Period,
    /// Primary and switch/fallback peers keyed by period adaptation index.
    pub(super) adaptation_sets: &'a HashMap<usize, AdaptationSet>,
    pub(super) primary_period_adaptation_index: usize,
    /// `@bitstreamSwitching` (or equivalent) per period adaptation index.
    pub(super) bitstream_switching: &'a HashMap<usize, bool>,
    pub(super) blacklist: &'a SegmentBlacklist,
    pub(super) drm: &'a Arc<AsyncMutex<DrmSessionCoordinator>>,
    pub(super) tx: &'a broadcast::Sender<PlayerEvent>,
    pub(super) metrics: &'a TrackMetrics,
    pub(super) track_kind: TrackKind,
    pub(super) cmcd: Option<&'a CmcdSession>,
}

impl RepFetchEnv<'_> {
    pub(super) fn adaptation_set_for(&self, period_adaptation_index: usize) -> &AdaptationSet {
        self.adaptation_sets
            .get(&period_adaptation_index)
            .or_else(|| {
                self.adaptation_sets
                    .get(&self.primary_period_adaptation_index)
            })
            .expect("primary adaptation set present")
    }

    pub(super) fn resolve_quality(
        &self,
        abr: &dyn AbrController,
        quality_index: usize,
    ) -> (usize, &AdaptationSet, usize) {
        let rung = abr.rung_for_quality_index(quality_index);
        let period_adaptation_index = rung.period_adaptation_index;
        let adaptation_set = self.adaptation_set_for(period_adaptation_index);
        (
            period_adaptation_index,
            adaptation_set,
            rung.representation_index,
        )
    }

    fn bitstream_switching_for(&self, period_adaptation_index: usize) -> bool {
        self.bitstream_switching
            .get(&period_adaptation_index)
            .copied()
            .unwrap_or(false)
    }

    fn cmcd_fetch(
        &self,
        object_type: CmcdObjectType,
        encoded_bitrate_bps: Option<f64>,
        object_duration_ms: Option<u64>,
    ) -> Option<CmcdFetch<'_>> {
        self.cmcd.map(|session| {
            let context = session.context_for(
                object_type,
                Some(self.metrics),
                encoded_bitrate_bps,
                object_duration_ms,
                None,
                None,
            );
            CmcdFetch { session, context }
        })
    }

    fn report_cmsd(&self, cmsd: Option<CmsdSnapshot>) {
        if let Some(cmsd) = cmsd {
            self.metrics.record_cmsd(cmsd.clone());
            let _ = self.tx.send(PlayerEvent::CmsdUpdated { cmsd });
        }
    }

    fn media_object_type(&self) -> CmcdObjectType {
        CmcdObjectType::from_track_kind(self.track_kind)
    }
}

pub(super) async fn fetch_init_with_rep_fallback(
    env: &RepFetchEnv<'_>,
    abr: &dyn AbrController,
    start_quality_index: usize,
    encrypted_init_by_rep: &mut HashMap<(usize, String), Bytes>,
) -> Result<(Bytes, String), PlayerError> {
    let mut last_err = PlayerError::from(SegmentError::ExhaustedRepresentations);
    for quality_index in quality_indices_for_fallback(start_quality_index) {
        let (period_adaptation_index, adaptation_set, rep_idx) =
            env.resolve_quality(abr, quality_index);
        let rep = &adaptation_set.representations[rep_idx];
        match ensure_init_for_rep(
            env,
            period_adaptation_index,
            adaptation_set,
            rep,
            encrypted_init_by_rep,
        )
        .await
        {
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
    plan: &SegmentPlan,
    encrypted_init_by_rep: &mut HashMap<(usize, String), Bytes>,
    sidx_segments_by_rep: &mut HashMap<String, Vec<manifest::TimelineSegment>>,
    per_segment_index_ranges_by_rep: &mut HashMap<String, HashMap<u64, manifest::ByteRange>>,
) -> Result<(Vec<u8>, usize, manifest::TimelineSegment), PlayerError> {
    let mut last_err = PlayerError::from(SegmentError::ExhaustedRepresentations);
    for quality_index in quality_indices_for_fallback(plan.quality_index) {
        let (period_adaptation_index, adaptation_set, rep_idx) =
            env.resolve_quality(abr, quality_index);
        let rep = &adaptation_set.representations[rep_idx];
        let bases =
            manifest::segment_bases_for_representation(env.segment_base_ctx, adaptation_set, rep)?;
        match ensure_init_for_rep(
            env,
            period_adaptation_index,
            adaptation_set,
            rep,
            encrypted_init_by_rep,
        )
        .await
        {
            Ok(_) => {}
            Err(e) => {
                last_err = e;
                continue;
            }
        }

        let rep_addressing =
            manifest::segment_addressing_for_representation(env.period, adaptation_set, rep)?;
        let mut seg_for_fetch = plan.segment.clone();
        if let Some(media_range) = plan.media_range {
            seg_for_fetch.media_range = Some(media_range);
        }
        match rep_addressing {
            manifest::SegmentAddressing::Base(ref sb)
                if manifest::segment_base_uses_sidx_index(sb) =>
            {
                let rep_segs = sidx_segments_for_rep(
                    env.client,
                    env.segment_base_ctx,
                    env.period,
                    adaptation_set,
                    rep,
                    env.blacklist,
                    sidx_segments_by_rep,
                )
                .await?;
                if let Some(rep_seg) = rep_segs.get(plan.local_index) {
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
                    adaptation_set,
                    rep,
                    env.blacklist,
                    sidx_segments_by_rep,
                )
                .await?;
                if let Some(rep_seg) = rep_segs.get(plan.local_index) {
                    seg_for_fetch.media_range = rep_seg.media_range;
                }
            }
            manifest::SegmentAddressing::Template(ref st)
                if manifest::segment_template_uses_per_segment_index(st) =>
            {
                if let Some(media_range) = media_range_for_per_segment_index(
                    env,
                    adaptation_set,
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
        let base_vars = manifest::template_vars_for_representation(rep, adaptation_set);
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
            plan.list_index,
            &template_vars,
        )?;
        let br_bps = rep.bandwidth.map(|b| b as f64);
        let d_ms = object_duration_ms(&seg_for_fetch);
        let cmcd = env.cmcd_fetch(env.media_object_type(), br_bps, d_ms);
        match fetch_segment_target(env.client, &bases, &seg_target, env.blacklist, cmcd).await {
            Ok(fetched) => {
                env.report_cmsd(fetched.cmsd.clone());
                return Ok((fetched.data, quality_index, seg_for_fetch));
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

pub(super) async fn fetch_cmaf_media_with_rep_fallback(
    env: &RepFetchEnv<'_>,
    abr: &dyn AbrController,
    plan: &SegmentPlan,
    encrypted_init_by_rep: &mut HashMap<(usize, String), Bytes>,
) -> Result<(Vec<Bytes>, usize, manifest::TimelineSegment), PlayerError> {
    let mut last_err = PlayerError::from(SegmentError::ExhaustedRepresentations);
    for quality_index in quality_indices_for_fallback(plan.quality_index) {
        let (period_adaptation_index, adaptation_set, rep_idx) =
            env.resolve_quality(abr, quality_index);
        let rep = &adaptation_set.representations[rep_idx];
        let bases =
            manifest::segment_bases_for_representation(env.segment_base_ctx, adaptation_set, rep)?;
        match ensure_init_for_rep(
            env,
            period_adaptation_index,
            adaptation_set,
            rep,
            encrypted_init_by_rep,
        )
        .await
        {
            Ok(_) => {}
            Err(e) => {
                last_err = e;
                continue;
            }
        }

        let rep_addressing =
            manifest::segment_addressing_for_representation(env.period, adaptation_set, rep)?;
        let seg_for_fetch = plan.segment.clone();
        let base_vars = manifest::template_vars_for_representation(rep, adaptation_set);
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
            plan.list_index,
            &template_vars,
        )?;
        let br_bps = rep.bandwidth.map(|b| b as f64);
        let d_ms = object_duration_ms(&seg_for_fetch);
        let cmcd = env.cmcd_fetch(env.media_object_type(), br_bps, d_ms);
        match partial_segment::fetch_cmaf_fragments_for_target(
            env.client,
            &bases,
            &seg_target,
            env.blacklist,
            cmcd,
        )
        .await
        {
            Ok((fragments, cmsd)) if !fragments.is_empty() => {
                env.report_cmsd(cmsd);
                return Ok((fragments, quality_index, seg_for_fetch));
            }
            Ok(_) => last_err = PlayerError::from(SegmentError::ExhaustedRepresentations),
            Err(e) => last_err = e.into(),
        }
    }
    Err(last_err)
}

async fn ensure_init_for_rep(
    env: &RepFetchEnv<'_>,
    period_adaptation_index: usize,
    adaptation_set: &AdaptationSet,
    rep: &Representation,
    encrypted_init_by_rep: &mut HashMap<(usize, String), Bytes>,
) -> Result<Bytes, PlayerError> {
    let rep_id = rep.id.as_deref().unwrap_or_default();
    let cache_key = (period_adaptation_index, rep_id.to_string());
    if let Some(init) = encrypted_init_by_rep.get(&cache_key) {
        return Ok(init.clone());
    }

    // ISO/IEC 23009-1: with bitstream switching, Media Segments from different
    // Representations may be concatenated without re-initializing the decoder.
    // Sharing is scoped to the owning Adaptation Set only.
    if env.bitstream_switching_for(period_adaptation_index)
        && let Some(shared) = encrypted_init_by_rep
            .iter()
            .find(|((aset_idx, _), _)| *aset_idx == period_adaptation_index)
            .map(|(_, v)| v.clone())
    {
        encrypted_init_by_rep.insert(cache_key.clone(), shared.clone());
        return Ok(shared);
    }

    let bases =
        manifest::segment_bases_for_representation(env.segment_base_ctx, adaptation_set, rep)?;
    let rep_addressing =
        manifest::segment_addressing_for_representation(env.period, adaptation_set, rep)?;
    let template_vars = manifest::template_vars_for_representation(rep, adaptation_set);
    let Some(init_target) = init_target_for_addressing(&rep_addressing, &template_vars)? else {
        encrypted_init_by_rep.insert(cache_key, Bytes::new());
        return Ok(Bytes::new());
    };
    let br_bps = rep.bandwidth.map(|b| b as f64);
    let cmcd = env.cmcd_fetch(CmcdObjectType::Init, br_bps, None);
    let fetched =
        fetch_segment_target(env.client, &bases, &init_target, env.blacklist, cmcd).await?;
    env.report_cmsd(fetched.cmsd.clone());
    let init_bytes = Bytes::from(fetched.data);
    encrypted_init_by_rep.insert(cache_key, init_bytes.clone());

    #[cfg(feature = "drm")]
    {
        let mut guard = env.drm.lock().await;
        guard
            .ensure_from_fragments(period_adaptation_index, rep_id, &init_bytes, None)
            .await?;
    }

    #[cfg(feature = "drm")]
    let out = {
        let license = {
            let guard = env.drm.lock().await;
            guard.license_for_rep(period_adaptation_index, rep_id)
        };
        if let Some(ref lic) = license {
            match lic.decrypt(&init_bytes, Option::<&Bytes>::None) {
                Ok(decrypted) => decrypted,
                Err(e) if License::is_likely_missing_key(&e) => {
                    let mut guard = env.drm.lock().await;
                    guard
                        .recover_from_decrypt_failure(
                            period_adaptation_index,
                            rep_id,
                            &init_bytes,
                            &[],
                        )
                        .await?;
                    let refreshed = guard.license_for_rep(period_adaptation_index, rep_id);
                    drop(guard);
                    refreshed
                        .ok_or(DrmError::License(e))?
                        .decrypt(&init_bytes, Option::<&Bytes>::None)
                        .map_err(DrmError::License)?
                }
                Err(e) => return Err(PlayerError::Drm(DrmError::License(e))),
            }
        } else {
            init_bytes.clone()
        }
    };
    #[cfg(not(feature = "drm"))]
    let out = init_bytes.clone();
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
        manifest::SegmentAddressing::List(sl) => Ok(manifest::segment_list_init_target(sl, vars)?),
        manifest::SegmentAddressing::Base(sb) => Ok(manifest::segment_base_init_target(sb, vars)?),
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
                .ok_or(ManifestError::MissingMediaTemplate)?;
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
            Ok(manifest::segment_list_media_target(sl, seg, list_idx)?)
        }
        manifest::SegmentAddressing::Base(sb) => {
            Ok(manifest::segment_base_media_target(sb, seg, vars)?)
        }
    }
}

pub(super) async fn fetch_segment_target(
    client: &SharedHttpClient,
    bases: &[url::Url],
    target: &manifest::SegmentFetchTarget,
    blacklist: &SegmentBlacklist,
    cmcd: Option<CmcdFetch<'_>>,
) -> Result<FetchedBytes, PlayerError> {
    if target.range.is_some() {
        return fetch_bytes_with_base_failover_and_range(
            client,
            bases,
            &target.path,
            target.range,
            blacklist,
            cmcd,
        )
        .await
        .map_err(Into::into);
    }
    fetch_bytes_with_base_failover(client, bases, &target.path, blacklist, cmcd)
        .await
        .map_err(Into::into)
}

fn object_duration_ms(seg: &manifest::TimelineSegment) -> Option<u64> {
    if seg.duration_s > 0.0 {
        Some((seg.duration_s * 1000.0).round() as u64)
    } else {
        None
    }
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
            let segs = fetch_and_parse_segment_template_index(
                client,
                &bases,
                &merged_st,
                rep,
                adaptation_set,
                blacklist,
            )
            .await?;
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
    adaptation_set: &AdaptationSet,
    rep: &Representation,
    seg: &manifest::TimelineSegment,
    cache: &mut HashMap<String, HashMap<u64, manifest::ByteRange>>,
) -> Result<Option<manifest::ByteRange>, PlayerError> {
    let merged_st = manifest::segment_template_for_representation(env.period, adaptation_set, rep)?;
    if !manifest::segment_template_uses_per_segment_index(&merged_st) {
        return Ok(None);
    }

    let rep_id = rep.id.as_deref().unwrap_or_default().to_string();
    let per_rep = cache.entry(rep_id).or_default();
    if let Some(media_range) = per_rep.get(&seg.number) {
        return Ok(Some(*media_range));
    }

    let bases =
        manifest::segment_bases_for_representation(env.segment_base_ctx, adaptation_set, rep)?;
    let base_vars = manifest::template_vars_for_representation(rep, adaptation_set);
    let vars = manifest::TemplateVars {
        number: Some(seg.number),
        time: Some(seg.time),
        sub_number: seg.sub_number,
        ..base_vars
    };
    let index_target = manifest::segment_template_index_target(&merged_st, &vars)?;
    let index_bytes = fetch_complete_template_index_bytes(
        env.client,
        &bases,
        &merged_st,
        &index_target,
        env.blacklist,
    )
    .await?;
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
        let sb = manifest::segment_base_for_representation(period, adaptation_set, rep)?;
        if !manifest::segment_base_uses_sidx_index(&sb) {
            return Ok(&[]);
        }
        let bases =
            manifest::segment_bases_for_representation(segment_base_ctx, adaptation_set, rep)?;
        let segs =
            fetch_and_parse_segment_base_index(client, &bases, &sb, rep, adaptation_set, blacklist)
                .await?;
        e.insert(segs);
    }
    Ok(cache
        .get(rep.id.as_deref().unwrap_or_default())
        .map(|v| v.as_slice())
        .unwrap_or(&[]))
}

/// Fetch `SegmentBase` index bytes (honouring `@indexRangeExact`) and parse the timeline.
///
/// When `@indexRangeExact` is false/absent, the Index Segment may extend past `@indexRange`;
/// additional Range requests are issued until the `sidx` (and nested index boxes) are complete.
pub(super) async fn fetch_and_parse_segment_base_index(
    client: &SharedHttpClient,
    bases: &[url::Url],
    sb: &dash_mpd::SegmentBase,
    rep: &Representation,
    adaptation_set: &AdaptationSet,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<manifest::TimelineSegment>, PlayerError> {
    let vars = manifest::template_vars_for_representation(rep, adaptation_set);
    let index_target = manifest::segment_base_index_target(sb, &vars)?;

    // Same-file `@indexRange`: extend when the Index Segment is incomplete.
    if index_target.range.is_some() && index_target.path.is_empty() {
        let Some(mut br) = index_target.range else {
            return Err(PlayerError::from(
                ManifestError::MissingSegmentBaseIndexRange,
            ));
        };
        let mut index_bytes =
            fetch_bytes_with_base_failover_and_range(client, bases, "", Some(br), blacklist, None)
                .await?
                .into_data();
        loop {
            match manifest::parse_sidx_index_for_segment_base(sb, &index_bytes) {
                Ok(segs) => return Ok(segs),
                Err(ManifestError::IncompleteSidxIndex { need_end }) if need_end > br.end => {
                    let extend = manifest::ByteRange {
                        start: br.end.saturating_add(1),
                        end: need_end,
                    };
                    let more = fetch_bytes_with_base_failover_and_range(
                        client,
                        bases,
                        "",
                        Some(extend),
                        blacklist,
                        None,
                    )
                    .await?
                    .into_data();
                    index_bytes.extend_from_slice(&more);
                    br.end = need_end;
                }
                Err(e) => return Err(PlayerError::from(e)),
            }
        }
    }

    let index_bytes = fetch_segment_target(client, bases, &index_target, blacklist, None)
        .await?
        .into_data();
    Ok(manifest::parse_sidx_index_for_segment_base(
        sb,
        &index_bytes,
    )?)
}

/// Fetch `SegmentTemplate@index` sidecar bytes (honouring `@indexRangeExact`) and parse the timeline.
///
/// When `@indexRangeExact` is false/absent, the Index Segment may extend past `@indexRange` in the
/// sidecar document; additional Range requests are issued until the index is complete.
pub(super) async fn fetch_and_parse_segment_template_index(
    client: &SharedHttpClient,
    bases: &[url::Url],
    st: &dash_mpd::SegmentTemplate,
    rep: &Representation,
    adaptation_set: &AdaptationSet,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<manifest::TimelineSegment>, PlayerError> {
    let vars = manifest::template_vars_for_representation(rep, adaptation_set);
    let index_target = manifest::segment_template_index_target(st, &vars)?;
    let index_bytes =
        fetch_complete_template_index_bytes(client, bases, st, &index_target, blacklist).await?;
    Ok(manifest::parse_sidx_index_from_template(st, &index_bytes)?)
}

/// Fetch sidecar index bytes, extending past `@indexRange` when `@indexRangeExact` allows it.
async fn fetch_complete_template_index_bytes(
    client: &SharedHttpClient,
    bases: &[url::Url],
    st: &dash_mpd::SegmentTemplate,
    index_target: &manifest::SegmentFetchTarget,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<u8>, PlayerError> {
    let Some(mut br) = index_target.range else {
        return Ok(
            fetch_segment_target(client, bases, index_target, blacklist, None)
                .await?
                .into_data(),
        );
    };
    let mut index_bytes = fetch_bytes_with_base_failover_and_range(
        client,
        bases,
        &index_target.path,
        Some(br),
        blacklist,
        None,
    )
    .await?
    .into_data();
    loop {
        match manifest::parse_sidx_index_from_template(st, &index_bytes) {
            Ok(_) => return Ok(index_bytes),
            Err(ManifestError::IncompleteSidxIndex { need_end }) if need_end > br.end => {
                let extend = manifest::ByteRange {
                    start: br.end.saturating_add(1),
                    end: need_end,
                };
                let more = fetch_bytes_with_base_failover_and_range(
                    client,
                    bases,
                    &index_target.path,
                    Some(extend),
                    blacklist,
                    None,
                )
                .await?
                .into_data();
                index_bytes.extend_from_slice(&more);
                br.end = need_end;
            }
            Err(e) => return Err(PlayerError::from(e)),
        }
    }
}
