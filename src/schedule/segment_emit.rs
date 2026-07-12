//! Segment delivery: player events, metrics, and playback state updates.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period, Representation};
use tokio::sync::{broadcast, watch};

use crate::PlayerError;
use crate::abr::AbrController;
use crate::manifest;
use crate::media_events;
use crate::metrics::TrackMetrics;
use crate::playback_control::{PlaybackController, PlaybackState};
use crate::prft;
use crate::resync::ProducerReferenceAnchor;
use crate::types::{PartialSegmentChunk, PlayerEvent};

use super::segment_fetch::RepFetchEnv;

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

pub(super) fn latest_buffer_s(buffer_rx: &watch::Receiver<f64>) -> f64 {
    *buffer_rx.borrow()
}
