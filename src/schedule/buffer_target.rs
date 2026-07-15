//! Buffer-target scheduling: throttle segment prefetch from buffer occupancy.
//!
//! Uses `MPD@minBufferTime` as the rebuffer floor and a high-water mark aligned with the
//! default BOLA buffer ceiling so ABR and scheduling share the same scale.
//! Occupancy comes from the media-clock estimate and optional consumer reports.

use std::time::Duration;

use dash_mpd::MPD;
use tokio::sync::{broadcast, watch};

use crate::metrics::TrackMetrics;
use crate::playback_control::PlaybackController;
use crate::types::PlayerEvent;

use super::segment_emit::{MEDIA_CLOCK_TICK, publish_buffer_estimate};

/// Consumer buffer thresholds for segment download scheduling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BufferTarget {
    /// Minimum buffer to maintain (`MPD@minBufferTime`); triggers rebuffer recovery fetches.
    pub min_buffer_s: f64,
    /// High-water mark: prefetch pauses when consumer buffer is at or above this level.
    pub max_buffer_s: f64,
}

impl BufferTarget {
    /// Default high-water mark (matches BOLA `BUFFER_MAX_S`).
    pub(crate) const DEFAULT_MAX_BUFFER_S: f64 = 25.0;

    /// Fallback minimum when the MPD omits `@minBufferTime`.
    pub(crate) const DEFAULT_MIN_BUFFER_S: f64 = 2.0;

    /// Build thresholds from an MPD's `@minBufferTime`.
    pub(crate) fn from_mpd(mpd: &MPD) -> Self {
        Self::from_min_buffer_time(mpd.minBufferTime)
    }

    /// Build thresholds from a parsed `@minBufferTime` value.
    pub(crate) fn from_min_buffer_time(min_buffer_time: Option<Duration>) -> Self {
        let min_buffer_s = min_buffer_time
            .map(|d| d.as_secs_f64())
            .filter(|s| s.is_finite() && *s >= 0.0)
            .unwrap_or(Self::DEFAULT_MIN_BUFFER_S);
        Self {
            min_buffer_s,
            max_buffer_s: Self::DEFAULT_MAX_BUFFER_S,
        }
    }

    /// Whether a media segment download should proceed for the current buffer level.
    ///
    /// The first media segment is always scheduled so startup can begin. After that,
    /// downloads pause when the consumer buffer is full (`>= max_buffer_s`) and resume
    /// when occupancy drops or falls below `min_buffer_s` (rebuffer recovery).
    pub(crate) fn should_fetch(&self, buffer_s: f64, media_segments_delivered: usize) -> bool {
        if media_segments_delivered == 0 {
            return true;
        }
        if buffer_s < self.min_buffer_s {
            return true;
        }
        buffer_s < self.max_buffer_s
    }
}

/// Context for refreshing estimated buffer while waiting for fetch capacity.
pub(crate) struct BufferEstimatePublish<'a> {
    pub playback: &'a PlaybackController,
    pub track_idx: usize,
    pub buffer_tx: &'a watch::Sender<f64>,
    pub metrics: &'a TrackMetrics,
    pub event_tx: &'a broadcast::Sender<PlayerEvent>,
}

/// Block until buffer occupancy allows another media segment download.
pub(crate) async fn wait_for_fetch_capacity(
    buffer_target: &BufferTarget,
    buffer_rx: &mut watch::Receiver<f64>,
    media_segments_delivered: usize,
    playback: &PlaybackController,
    seek_generation_at_start: u64,
    publish: &BufferEstimatePublish<'_>,
) {
    loop {
        if playback.is_stopped() || playback.seek_generation() != seek_generation_at_start {
            return;
        }
        playback.wait_while_paused().await;

        let _ = publish_buffer_estimate(
            publish.playback,
            publish.track_idx,
            publish.buffer_tx,
            publish.metrics,
            publish.event_tx,
            true,
        );

        let buffer_s = *buffer_rx.borrow();
        if buffer_target.should_fetch(buffer_s, media_segments_delivered) {
            return;
        }

        tokio::select! {
            changed = buffer_rx.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = crate::platform::sleep(MEDIA_CLOCK_TICK) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_min_buffer_time_uses_mpd_value() {
        let target = BufferTarget::from_min_buffer_time(Some(Duration::from_secs(4)));
        assert_eq!(target.min_buffer_s, 4.0);
        assert_eq!(target.max_buffer_s, BufferTarget::DEFAULT_MAX_BUFFER_S);
    }

    #[test]
    fn from_min_buffer_time_defaults_when_absent() {
        let target = BufferTarget::from_min_buffer_time(None);
        assert_eq!(target.min_buffer_s, BufferTarget::DEFAULT_MIN_BUFFER_S);
    }

    #[test]
    fn should_fetch_first_segment_always() {
        let target = BufferTarget::from_min_buffer_time(Some(Duration::from_secs(2)));
        assert!(target.should_fetch(100.0, 0));
    }

    #[test]
    fn should_fetch_throttles_when_buffer_full() {
        let target = BufferTarget::from_min_buffer_time(Some(Duration::from_secs(2)));
        assert!(!target.should_fetch(25.0, 1));
        assert!(!target.should_fetch(30.0, 2));
    }

    #[test]
    fn should_fetch_resumes_when_buffer_drops() {
        let target = BufferTarget::from_min_buffer_time(Some(Duration::from_secs(2)));
        assert!(target.should_fetch(24.0, 1));
        assert!(target.should_fetch(10.0, 1));
    }

    #[test]
    fn should_fetch_rebuffer_recovery_below_min() {
        let target = BufferTarget::from_min_buffer_time(Some(Duration::from_secs(2)));
        assert!(target.should_fetch(1.0, 5));
        assert!(target.should_fetch(0.0, 10));
    }
}
