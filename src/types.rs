use std::time::Duration;

use bytes::Bytes;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::PlayerError;
use super::metrics::TrackMetrics;
use super::playback_control::PlaybackController;
use super::stream_controller::PlaybackLoopState;
use super::track_selection::TrackInfo;

/// Playback failure delivered on a track event stream.
///
/// The background task [`JoinHandle`] still returns the full [`PlayerError`] for
/// programmatic handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerEventError(pub String);

impl From<&PlayerError> for PlayerEventError {
    fn from(err: &PlayerError) -> Self {
        Self(err.to_string())
    }
}

/// Error returned when buffer feedback can no longer reach the playback pipeline.
#[derive(Debug, Error)]
pub enum BufferFeedbackError {
    #[error("playback stream ended")]
    StreamEnded,
}

/// Reports playback buffer occupancy (seconds of media buffered ahead) to the ABR controller.
///
/// Call [`Self::report`] periodically as the consumer decodes or renders media so adaptive
/// bitrate decisions reflect actual playback state rather than download timing alone.
#[derive(Clone)]
pub struct BufferFeedback {
    tx: watch::Sender<f64>,
    metrics: TrackMetrics,
    event_tx: broadcast::Sender<PlayerEvent>,
}

impl BufferFeedback {
    pub(crate) fn new(
        tx: watch::Sender<f64>,
        metrics: TrackMetrics,
        event_tx: broadcast::Sender<PlayerEvent>,
    ) -> Self {
        Self {
            tx,
            metrics,
            event_tx,
        }
    }

    /// Report the current buffer level in seconds.
    ///
    /// Values are clamped internally by the ABR algorithm. Report `0.0` when stalled or empty.
    /// Emits [`PlayerEvent::BufferUpdated`] on the track event stream.
    pub fn report(&self, buffer_s: f64) -> Result<(), BufferFeedbackError> {
        self.metrics.record_buffer(buffer_s);
        let _ = self.event_tx.send(PlayerEvent::BufferUpdated { buffer_s });
        self.tx
            .send(buffer_s)
            .map_err(|_| BufferFeedbackError::StreamEnded)
    }
}

/// One chunk of a progressive low-latency segment transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartialSegmentChunk {
    /// 1-based chunk index within this segment transfer.
    pub index: u64,
    /// `true` when this is the last chunk for the segment.
    pub is_final: bool,
}

/// Events emitted on a single DASH adaptation-set stream.
///
/// Fragment events ([`Self::Init`], [`Self::Segment`], [`Self::End`]) carry media bytes.
/// Lifecycle and observability events report manifest, buffer, bitrate, and playback state.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    /// An MPD was fetched and parsed successfully (initial load or live refresh).
    ManifestLoaded {
        /// Whether the MPD is dynamic (live / sliding window).
        is_dynamic: bool,
        /// `MPD@mediaPresentationDuration` when present.
        media_presentation_duration: Option<Duration>,
    },
    /// Consumer-reported buffer occupancy changed for this track.
    BufferUpdated {
        /// Seconds of media buffered ahead of the playhead.
        buffer_s: f64,
    },
    /// The active representation changed on the adaptation ladder.
    BitrateChanged {
        from_quality_index: usize,
        to_quality_index: usize,
        from_bitrate_bps: f64,
        to_bitrate_bps: f64,
    },
    /// The first media segment was delivered for this adaptation set.
    PlaybackStarted,
    /// Playback finished for this adaptation set (VOD end, stop, or bounded window).
    PlaybackEnded,
    /// The playback pipeline failed; see the background task join result for the full error.
    Error(PlayerEventError),
    /// MPD `EventStream` or in-band `emsg` timed event (including SCTE-35 ad markers).
    MediaEvent(super::media_events::MediaEvent),
    /// Initialization segment (`ftyp` + `moov`).
    Init(Bytes),
    Segment {
        number: u64,
        time: u64,
        /// Set when `SegmentTimeline/S@k` > 1 (ISO 23009-1 §5.3.9.6.4); 1-based chunk within the sequence.
        sub_number: Option<u64>,
        /// Progressive low-latency delivery when `@availabilityTimeComplete=false`.
        partial: Option<PartialSegmentChunk>,
        data: Bytes,
    },
    /// No more fragments will be sent for this adaptation set (VOD / bounded static window).
    End,
}

impl PlayerEvent {
    /// Returns `true` when this event marks the end of the track event stream.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            PlayerEvent::End | PlayerEvent::PlaybackEnded | PlayerEvent::Error(_)
        )
    }

    /// Returns `true` when this event carries a media fragment payload.
    pub fn is_fragment(&self) -> bool {
        matches!(self, PlayerEvent::Init(_) | PlayerEvent::Segment { .. })
    }
}

/// One DASH adaptation set (audio, video, or text) exposed as a broadcast stream.
#[derive(Clone)]
pub struct PlayerTrack {
    /// `AdaptationSet@mimeType` when present (e.g. `video/mp4`, `audio/mp4`).
    pub mime_type: Option<String>,
    /// Language, roles, codecs, accessibility, and other selected-track metadata.
    pub info: TrackInfo,
    pub(crate) tx: broadcast::Sender<PlayerEvent>,
    pub(crate) buffer_feedback: BufferFeedback,
    pub(crate) buffer_rx: watch::Receiver<f64>,
    pub(crate) metrics: TrackMetrics,
}

impl PlayerTrack {
    pub fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.tx.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Send buffer occupancy updates for this track's ABR controller.
    pub fn buffer_feedback(&self) -> BufferFeedback {
        self.buffer_feedback.clone()
    }

    /// Playback metrics for this track (throughput, buffer, startup delay, rebuffer, switches).
    pub fn metrics(&self) -> TrackMetrics {
        self.metrics.clone()
    }
}

/// Handles returned when starting playback (dash.js: MediaPlayer exposes stream interfaces).
///
/// [`MediaPlayer::start`](crate::MediaPlayer::start) does not spawn tasks. Call [`Self::run`] on
/// the current async task, or [`Self::spawn`] when a separate Tokio task is desired.
pub struct PlayerOutputs {
    /// One channel per selected AdaptationSet (audio/video/text/trick-play/image filtered by
    /// [`TrackSelection`]).
    pub tracks: Vec<PlayerTrack>,
    /// Seek, pause, resume, stop, and lifecycle state for this session.
    pub playback: PlaybackController,
    pub(crate) loop_state: PlaybackLoopState,
}

impl PlayerOutputs {
    /// Run the stream controller loop on the current async task.
    ///
    /// Audio and video adaptation sets are fetched concurrently within this task via
    /// cooperative `join` — no additional Tokio tasks are spawned.
    pub async fn run(self) -> Result<(), PlayerError> {
        let Self {
            tracks,
            playback: _,
            loop_state,
        } = self;
        loop_state.run(tracks).await
    }

    /// Spawn the stream controller loop as a separate Tokio task.
    ///
    /// Prefer [`Self::run`] when the caller owns concurrency and wants a single task.
    pub fn spawn(self) -> JoinHandle<Result<(), PlayerError>> {
        tokio::spawn(async move { self.run().await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_and_fragment_classification() {
        assert!(PlayerEvent::End.is_terminal());
        assert!(PlayerEvent::PlaybackEnded.is_terminal());
        assert!(PlayerEvent::Error(PlayerEventError("fail".into())).is_terminal());
        assert!(
            !PlayerEvent::ManifestLoaded {
                is_dynamic: false,
                media_presentation_duration: None,
            }
            .is_terminal()
        );
        assert!(PlayerEvent::Init(Bytes::new()).is_fragment());
        assert!(
            PlayerEvent::Segment {
                number: 1,
                time: 0,
                sub_number: None,
                partial: None,
                data: Bytes::new(),
            }
            .is_fragment()
        );
        assert!(!PlayerEvent::BufferUpdated { buffer_s: 1.0 }.is_fragment());
    }
}
