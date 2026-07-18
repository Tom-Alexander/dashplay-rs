//! Segment delivery: player events, metrics, and playback state updates.

use std::time::Duration;

use bytes::Bytes;
use dash_mpd::{AdaptationSet, Period, Representation};
use tokio::sync::{broadcast, watch};

use crate::PlayerError;
use crate::abr::AbrController;
use crate::manifest;
use crate::media_events;
use crate::metrics::TrackMetrics;
use crate::mp4::prft;
use crate::playback_control::PlaybackController;
use crate::track_session::TrackSessionState;
use crate::types::{PartialSegmentChunk, PlayerEvent};

use super::segment_fetch::RepFetchEnv;

/// Interval for advancing the media clock and republishing estimated buffer.
pub(super) const MEDIA_CLOCK_TICK: Duration = Duration::from_millis(250);

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
    session: &TrackSessionState,
    prt_reference_id: Option<&str>,
    buffer_tx: &watch::Sender<f64>,
) {
    prft::maybe_update_inband_anchor_from_segment(
        data.as_ref(),
        period,
        adaptation_set,
        rep,
        prt_reference_id,
        session.inband_prt_anchor(),
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
    let segment_end = presentation_time + Duration::from_secs_f64(seg.duration_s.max(0.0));

    let _ = tx.send(PlayerEvent::Segment {
        number: seg.number,
        time: seg.time,
        presentation_time,
        sub_number: seg.sub_number,
        partial,
        data,
    });
    let clock_initialized =
        playback.record_segment_delivery(track_idx, presentation_time, segment_end);
    if clock_initialized {
        let _ = tx.send(PlayerEvent::PlayheadUpdated {
            presentation_time: playback.presentation_time(),
        });
    }
    apply_latency_control(tx, playback);
    metrics.record_segment_delivered();

    if !*playback_started_emitted {
        let _ = tx.send(PlayerEvent::PlaybackStarted);
        *playback_started_emitted = true;
    }

    playback.on_media_delivered(track_idx);
    publish_buffer_estimate(playback, track_idx, buffer_tx, metrics, tx, false);
}

/// Run `fut` while periodically advancing the media clock so stalls are detected during
/// long downloads.
pub(super) async fn with_media_clock_ticks<F, T>(
    fut: F,
    playback: &PlaybackController,
    track_idx: usize,
    buffer_tx: &watch::Sender<f64>,
    metrics: &TrackMetrics,
    event_tx: &broadcast::Sender<PlayerEvent>,
) -> T
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(fut);
    loop {
        tokio::select! {
            result = &mut fut => return result,
            _ = crate::platform::sleep(MEDIA_CLOCK_TICK) => {
                let _ = publish_buffer_estimate(
                    playback,
                    track_idx,
                    buffer_tx,
                    metrics,
                    event_tx,
                    true,
                );
            }
        }
    }
}

/// Advance the media clock (optional), publish estimated buffer, and emit playhead events.
///
/// Returns the latest estimated buffer level for `track_idx`.
pub(super) fn publish_buffer_estimate(
    playback: &PlaybackController,
    track_idx: usize,
    buffer_tx: &watch::Sender<f64>,
    metrics: &TrackMetrics,
    event_tx: &broadcast::Sender<PlayerEvent>,
    advance_clock: bool,
) -> f64 {
    let playhead_changed = if advance_clock {
        playback.advance_media_clock().playhead_changed
    } else {
        false
    };

    let buffer_s = playback.estimated_buffer_s(track_idx);
    let _ = playback.update_stall_state(buffer_s);

    let previous = *buffer_tx.borrow();
    if (previous - buffer_s).abs() > 1e-3 {
        let _ = buffer_tx.send(buffer_s);
        metrics.record_buffer(buffer_s);
        let _ = event_tx.send(PlayerEvent::BufferUpdated { buffer_s });
    }

    if playhead_changed {
        let _ = event_tx.send(PlayerEvent::PlayheadUpdated {
            presentation_time: playback.presentation_time(),
        });
        apply_latency_control(event_tx, playback);
    }

    buffer_s
}

pub(crate) fn segment_presentation_time(
    period_start: Duration,
    seg: &manifest::TimelineSegment,
) -> Duration {
    period_start + Duration::from_secs_f64(seg.presentation_time_s.max(0.0))
}

fn apply_latency_control(tx: &broadcast::Sender<PlayerEvent>, playback: &PlaybackController) {
    let Some(update) = playback.refresh_latency_control() else {
        return;
    };
    if update.rate_changed {
        if let Some(target_latency) = playback.latency_target() {
            let _ = tx.send(PlayerEvent::PlaybackRateSuggested {
                rate: update.rate,
                latency: update.latency,
                target_latency,
            });
        }
    }
    if let Some(seek_target) = update.seek_target {
        let _ = playback.seek(seek_target);
    }
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
    dropped_frames: &crate::abr::DroppedFramesHistory,
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
    dropped_frames.set_active_quality(used_quality_index);

    abr.observe_segment_download(throughput_bps, byte_len, used_quality_index);
    abr.update_buffer(latest_buffer_s(buffer_rx));
    metrics.record_buffer(latest_buffer_s(buffer_rx));
    Ok(())
}

pub(super) fn latest_buffer_s(buffer_rx: &watch::Receiver<f64>) -> f64 {
    *buffer_rx.borrow()
}
