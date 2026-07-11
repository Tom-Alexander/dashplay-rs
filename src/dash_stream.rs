//! One DASH stream: initialization + media segments for a single AdaptationSet
//! (dash.js: `Stream` + schedule / fragment pipeline for that stream).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::time::Instant;

use super::PlayerError;
use super::abr_controller::AbrController;
use super::drm::License;
use super::manifest::{self, TimelineBuildContext};
use super::segment_blacklist::SegmentBlacklist;
use super::segment_fetcher::fetch_bytes_with_base_failover;
use super::types::PlayerEvent;
use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period};
use reqwest::Client;
use tokio::sync::broadcast;

pub(crate) struct AdaptationStreamContext {
    pub client: Client,
    pub segment_base_ctx: manifest::SegmentBaseContext,
    pub target_time: Option<Duration>,
    pub period_start: Duration,
    pub period: Period,
    pub timeline_ctx: TimelineBuildContext,
    pub adaptation_set: AdaptationSet,
    pub aset_idx: usize,
    pub tx: broadcast::Sender<PlayerEvent>,
    pub have_init: Arc<Vec<AtomicBool>>,
    pub blacklist: SegmentBlacklist,
    pub license: Option<Arc<License>>,
    /// Representation-specific Widevine sessions (effective DRM at Representation level).
    pub wv_by_rep: HashMap<String, Arc<License>>,
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
        aset_idx,
        tx,
        have_init,
        blacklist,
        license,
        wv_by_rep,
    } = ctx;

    let st = manifest::segment_template_for_timeline(&period, &adaptation_set)?;

    let segments_all = manifest::timeline_segments(&st, &timeline_ctx)?;

    // Align every adaptation set to the same media instant: pick the first segment whose
    // interval (in MPD time) still contains instants after `target`. Using "last segment with
    // start <= target" breaks A/V sync when audio and video use different segment durations
    // (e.g. 6s audio vs 2s video): each track would start at a different segment start time.
    let segments: Vec<manifest::TimelineSegment> = if let Some(target) = target_time {
        let target_s = target.as_secs_f64();
        let p0 = period_start.as_secs_f64();
        let start_idx = segments_all
            .iter()
            .position(|s| p0 + s.presentation_time_s + s.duration_s > target_s)
            .unwrap_or(0);
        let start_idx =
            manifest::align_start_index_to_sap(&segments_all, start_idx, &adaptation_set);
        segments_all[start_idx..].to_vec()
    } else {
        let start_idx = manifest::align_start_index_to_sap(&segments_all, 0, &adaptation_set);
        segments_all[start_idx..].to_vec()
    };

    let Some(mut abr) = AbrController::from_adaptation_set(&adaptation_set, 0.3) else {
        return Ok(());
    };

    abr.update_buffer(10.0);

    let init_taken = have_init[aset_idx]
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok();

    // Cache init segments by Representation ID (ABR switches may require different init/boxes/KIDs).
    let mut encrypted_init_by_rep: HashMap<String, Bytes> = HashMap::new();
    let mut active_rep_id: Option<String> = None;
    if init_taken {
        let init_res: Result<(), PlayerError> = async {
            let decision = abr.decide();
            let rep_idx = abr.representation_index_for_quality_index(decision.quality_index);
            let rep = &adaptation_set.representations[rep_idx];
            let bases = manifest::segment_bases_for_representation(
                &segment_base_ctx,
                &adaptation_set,
                rep,
            )?;
            let rep_id = rep.id.as_deref().unwrap_or_default();
            let rep_license = wv_by_rep.get(rep_id).cloned().or_else(|| license.clone());
            let rep_st =
                manifest::segment_template_for_representation(&period, &adaptation_set, rep)?;
            let init_tpl = rep_st
                .initialization
                .as_deref()
                .ok_or(PlayerError::MissingInitializationTemplate)?;
            let init_path = manifest::interpolate_template(init_tpl, rep_id, None, None, None);
            let bytes =
                fetch_bytes_with_base_failover(&client, &bases, &init_path, &blacklist).await?;
            let init_bytes = Bytes::from(bytes);
            encrypted_init_by_rep.insert(rep_id.to_string(), init_bytes.clone());
            active_rep_id = Some(rep_id.to_string());
            let out = if let Some(ref lic) = rep_license {
                lic.decrypt(&init_bytes, Option::<&Bytes>::None)
                    .map_err(PlayerError::License)?
            } else {
                init_bytes
            };
            let _ = tx.send(PlayerEvent::Init(out));
            Ok(())
        }
        .await;
        if init_res.is_err() {
            have_init[aset_idx].store(false, Ordering::Release);
            init_res?;
        }
    }

    let mut buffer_s = abr.buffer_s();

    for seg in segments {
        let decision = abr.decide();
        let rep_idx = abr.representation_index_for_quality_index(decision.quality_index);
        let rep = &adaptation_set.representations[rep_idx];
        let bases =
            manifest::segment_bases_for_representation(&segment_base_ctx, &adaptation_set, rep)?;
        let rep_id = rep.id.as_deref().unwrap_or_default();
        let rep_id_string = rep_id.to_string();
        let rep_license = wv_by_rep.get(rep_id).cloned().or_else(|| license.clone());

        // If ABR switched reps (or init was never fetched for this rep), fetch init for this rep.
        if active_rep_id.as_deref() != Some(rep_id) || !encrypted_init_by_rep.contains_key(rep_id) {
            let rep_st =
                manifest::segment_template_for_representation(&period, &adaptation_set, rep)?;
            let init_tpl = rep_st
                .initialization
                .as_deref()
                .ok_or(PlayerError::MissingInitializationTemplate)?;
            let init_path = manifest::interpolate_template(init_tpl, rep_id, None, None, None);
            let init_bytes =
                fetch_bytes_with_base_failover(&client, &bases, &init_path, &blacklist)
                    .await
                    .map(Bytes::from)?;
            encrypted_init_by_rep.insert(rep_id_string.clone(), init_bytes.clone());
            active_rep_id = Some(rep_id_string.clone());

            // Emit decrypted init on rep switch to keep downstream consumers consistent.
            let out = if let Some(ref lic) = rep_license {
                lic.decrypt(&init_bytes, Option::<&Bytes>::None)
                    .map_err(PlayerError::License)?
            } else {
                init_bytes
            };
            let _ = tx.send(PlayerEvent::Init(out));
        }

        // We only decrypt if we have both a license and a cached encrypted init for this rep.
        let init_for_decrypt = encrypted_init_by_rep.get(rep_id);
        let rep_st = manifest::segment_template_for_representation(&period, &adaptation_set, rep)?;
        let media_tpl = rep_st
            .media
            .as_deref()
            .ok_or(PlayerError::MissingMediaTemplate)?;
        let seg_path = manifest::interpolate_template(
            media_tpl,
            rep_id,
            Some(seg.number),
            Some(seg.time),
            seg.sub_number,
        );
        let t0 = Instant::now();
        let bytes = fetch_bytes_with_base_failover(&client, &bases, &seg_path, &blacklist).await?;
        let elapsed_s = t0.elapsed().as_secs_f64().max(1e-6);
        let throughput_bps = (bytes.len() as f64 * 8.0) / elapsed_s;

        abr.observe_throughput(throughput_bps);
        buffer_s = (buffer_s + seg.duration_s - elapsed_s).max(0.0);
        abr.update_buffer(buffer_s);

        let mut data = Bytes::from(bytes);
        if let (Some(lic), Some(init_bytes)) = (&rep_license, init_for_decrypt) {
            // If the fragment is clear, mp4decrypt may error; treat that as passthrough.
            match lic.decrypt(&data, Some(init_bytes)) {
                Ok(d) => data = d,
                Err(e) => {
                    let msg = e.to_string().to_ascii_lowercase();
                    if msg.contains("not encrypted") || msg.contains("no") && msg.contains("senc") {
                        // Clear fragment; keep `data` as-is.
                    } else {
                        return Err(PlayerError::License(e));
                    }
                }
            }
        }

        let _ = tx.send(PlayerEvent::Segment {
            number: seg.number,
            time: seg.time,
            sub_number: seg.sub_number,
            data,
        });
    }

    Ok(())
}
