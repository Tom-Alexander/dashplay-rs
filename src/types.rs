use bytes::Bytes;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::PlayerError;

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
}

impl PlayerTrack {
    #[allow(dead_code)]
    pub fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.tx.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

/// Handles returned when starting playback (dash.js: MediaPlayer exposes stream interfaces).
pub struct PlayerOutputs {
    /// One channel per selected AdaptationSet (audio/video filtered).
    pub tracks: Vec<PlayerTrack>,
    /// Background task running the stream controller loop.
    pub join: JoinHandle<Result<(), PlayerError>>,
}
