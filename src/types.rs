use bytes::Bytes;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::PlayerError;
use super::playback_control::PlaybackController;
use super::stream_controller::PlaybackLoopState;

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
}

impl BufferFeedback {
    pub(crate) fn new(tx: watch::Sender<f64>) -> Self {
        Self { tx }
    }

    /// Report the current buffer level in seconds.
    ///
    /// Values are clamped internally by the ABR algorithm. Report `0.0` when stalled or empty.
    pub fn report(&self, buffer_s: f64) -> Result<(), BufferFeedbackError> {
        self.tx
            .send(buffer_s)
            .map_err(|_| BufferFeedbackError::StreamEnded)
    }
}

/// Events emitted on a single DASH adaptation-set stream (dash.js: stream / fragment events).
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Init(Bytes),
    Segment {
        number: u64,
        time: u64,
        /// Set when `SegmentTimeline/S@k` > 1 (ISO 23009-1 §5.3.9.6.4); 1-based chunk within the sequence.
        sub_number: Option<u64>,
        data: Bytes,
    },
    /// No more fragments will be sent for this adaptation set (VOD / bounded static window).
    End,
}

/// One DASH adaptation set (audio or video) exposed as a broadcast stream.
#[derive(Clone)]
pub struct PlayerTrack {
    /// `AdaptationSet@mimeType` when present (e.g. `video/mp4`, `audio/mp4`).
    pub mime_type: Option<String>,
    pub(crate) tx: broadcast::Sender<PlayerEvent>,
    pub(crate) buffer_feedback: BufferFeedback,
    pub(crate) buffer_rx: watch::Receiver<f64>,
}

impl PlayerTrack {
    #[allow(dead_code)]
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
}

/// Handles returned when starting playback (dash.js: MediaPlayer exposes stream interfaces).
///
/// [`MediaPlayer::start`](crate::MediaPlayer::start) does not spawn tasks. Call [`Self::run`] on
/// the current async task, or [`Self::spawn`] when a separate Tokio task is desired.
pub struct PlayerOutputs {
    /// One channel per selected AdaptationSet (audio/video filtered).
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
