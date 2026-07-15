use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::PlayerError;
use super::manifest::ManifestMetadata;
use super::metrics::TrackMetrics;
use super::playback_control::PlaybackController;
use super::stream_controller::PlaybackLoopState;
use super::track_selection::TrackInfo;

#[derive(Debug)]
struct TrackMeta {
    mime_type: Option<String>,
    info: TrackInfo,
}

/// How the player treats a Period boundary (ISO/IEC 23009-1 §5.3.2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeriodTransitionKind {
    /// Sample timelines are continuous; Initialization Segment may be reused.
    Continuous,
    /// Initialization Segment is equivalent; presentation times may jump (PTO-adjusted).
    Connected,
    /// Hard boundary: Initialization is re-emitted and delivery state is reset.
    Discontinuous,
}

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
/// The library estimates buffer from delivered media versus an internal media clock, so
/// calling [`Self::report`] is optional. When the consumer has a real decoder / MSE buffer,
/// report periodically so ABR and scheduling stay aligned with actual occupancy.
#[derive(Clone)]
pub struct BufferFeedback {
    tx: watch::Sender<f64>,
    metrics: TrackMetrics,
    event_tx: broadcast::Sender<PlayerEvent>,
    playback: PlaybackController,
    track_idx: usize,
}

impl BufferFeedback {
    pub(crate) fn new(
        tx: watch::Sender<f64>,
        metrics: TrackMetrics,
        event_tx: broadcast::Sender<PlayerEvent>,
        playback: PlaybackController,
        track_idx: usize,
    ) -> Self {
        Self {
            tx,
            metrics,
            event_tx,
            playback,
            track_idx,
        }
    }

    /// Report the current buffer level in seconds.
    ///
    /// Optional when relying on the library media-clock estimate. Values are clamped
    /// internally by the ABR algorithm. Report `0.0` when stalled or empty.
    /// Emits [`PlayerEvent::BufferUpdated`] on the track event stream and resyncs the
    /// internal media clock so subsequent estimates follow this correction.
    pub fn report(&self, buffer_s: f64) -> Result<(), BufferFeedbackError> {
        self.playback
            .resync_media_clock_from_buffer(self.track_idx, buffer_s);
        let _ = self.playback.update_stall_state(buffer_s);
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
        /// Descriptive MPD metadata (`ProgramInformation`, `Metrics`, period labels, …).
        metadata: ManifestMetadata,
    },
    /// Buffer occupancy changed for this track (media-clock estimate or consumer report).
    BufferUpdated {
        /// Seconds of media buffered ahead of the playhead.
        buffer_s: f64,
    },
    /// Session presentation time changed (media clock, seek target, or clock init).
    PlayheadUpdated {
        /// Seconds from the start of the presentation; `None` before the first segment.
        presentation_time: Option<Duration>,
    },
    /// Suggested consumption rate to chase `ServiceDescription/Latency@target`.
    ///
    /// Apply this rate to the decoder / media clock; the library does not render.
    /// Absent when the MPD has no usable `Latency@target`. Rate is `1.0` at the target.
    PlaybackRateSuggested {
        /// Multiplier relative to real time (`1.0` = normal speed).
        rate: f64,
        /// Measured live latency (`since availabilityStartTime` − presentation time).
        latency: Duration,
        /// `Latency@target` from the MPD.
        target_latency: Duration,
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
    /// The selected adaptation set for this track slot changed mid-playback.
    ///
    /// Emitted after [`crate::PlaybackController::set_track_selection`] remaps a
    /// track to a different language, role, or other preferred adaptation set.
    /// A fresh [`Self::Init`] follows before segments from the new set.
    TrackChanged {
        /// Updated language, roles, codecs, and other track metadata.
        info: TrackInfo,
    },
    /// Playback entered a new Period (including the first Period of a presentation).
    ///
    /// `start` / `end` are presentation-timeline clip windows for this Period. Consumers
    /// that map into MSE can use them as append-window bounds. Sample-level clipping is
    /// not performed by the library.
    ///
    /// When the previous Period's end is strictly before this Period's start, `gap_before`
    /// is the hole on the Media Presentation timeline (ISO/IEC 23009-1 Period sequencing).
    /// Abutting or overlapping Periods yield `None`. Sample-timeline jumps under
    /// [`PeriodTransitionKind::Connected`] without a Period-window hole also yield `None`.
    PeriodChanged {
        /// Zero-based Period index in the MPD.
        period_index: usize,
        /// `PeriodStart` on the Media Presentation timeline.
        start: Duration,
        /// Period end when known (`Period@duration`, next `@start`, or MPD duration).
        end: Option<Duration>,
        /// Continuity / connectivity relationship to the previous Period.
        transition: PeriodTransitionKind,
        /// Presentation-timeline gap before this Period (`prev.end` → `start`), if any.
        gap_before: Option<Duration>,
    },
    /// The playback pipeline failed; see the background task join result for the full error.
    Error(PlayerEventError),
    /// MPD `EventStream` or in-band `emsg` timed event (including SCTE-35 ad markers).
    MediaEvent(super::media_events::MediaEvent),
    /// CMSD response hints observed on a segment request (CTA-5006; informational only).
    CmsdUpdated { cmsd: super::cmcd::CmsdSnapshot },
    /// Initialization segment when the addressing mode provides one.
    ///
    /// For ISOBMFF/CMAF this is typically `ftyp` + `moov`. MPEG-2 TS (`video/mp2t` /
    /// `audio/mp2t`) and other container profiles may omit initialization; in that case
    /// no [`Self::Init`] is emitted and media begins with [`Self::Segment`].
    Init(Bytes),
    Segment {
        number: u64,
        time: u64,
        /// Presentation time of this segment (seconds from the start of the presentation).
        presentation_time: Duration,
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
    meta: Arc<Mutex<TrackMeta>>,
    pub(crate) tx: broadcast::Sender<PlayerEvent>,
    pub(crate) buffer_feedback: BufferFeedback,
    pub(crate) buffer_tx: watch::Sender<f64>,
    pub(crate) buffer_rx: watch::Receiver<f64>,
    pub(crate) metrics: TrackMetrics,
}

impl PlayerTrack {
    pub(crate) fn new(
        info: TrackInfo,
        tx: broadcast::Sender<PlayerEvent>,
        buffer_feedback: BufferFeedback,
        buffer_tx: watch::Sender<f64>,
        buffer_rx: watch::Receiver<f64>,
        metrics: TrackMetrics,
    ) -> Self {
        Self {
            meta: Arc::new(Mutex::new(TrackMeta {
                mime_type: info.mime_type.clone(),
                info,
            })),
            tx,
            buffer_feedback,
            buffer_tx,
            buffer_rx,
            metrics,
        }
    }

    /// `AdaptationSet@mimeType` when present (e.g. `video/mp4`, `audio/mp4`).
    pub fn mime_type(&self) -> Option<String> {
        self.meta
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mime_type
            .clone()
    }

    /// Language, roles, codecs, accessibility, and other selected-track metadata.
    ///
    /// Updated after mid-playback track switching; see also
    /// [`PlayerEvent::TrackChanged`].
    pub fn info(&self) -> TrackInfo {
        self.meta
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .info
            .clone()
    }

    pub(crate) fn replace_track_info(&self, info: TrackInfo) {
        let mut meta = self.meta.lock().unwrap_or_else(|e| e.into_inner());
        meta.mime_type = info.mime_type.clone();
        meta.info = info;
    }

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
    /// Descriptive metadata from the initially loaded MPD.
    ///
    /// Live refreshes update the same information via [`PlayerEvent::ManifestLoaded`].
    pub manifest_metadata: ManifestMetadata,
    pub(crate) loop_state: PlaybackLoopState,
}

impl PlayerOutputs {
    /// Latest CMSD snapshot from any request in this session, when CMCD is enabled.
    ///
    /// CMSD is observational and does not influence ABR or scheduling.
    pub fn last_cmsd(&self) -> Option<super::cmcd::CmsdSnapshot> {
        self.loop_state
            .cmcd
            .as_ref()
            .and_then(|session| session.last_cmsd())
    }

    /// Run the stream controller loop on the current async task.
    ///
    /// Audio and video adaptation sets are fetched concurrently within this task via
    /// cooperative `join` — no additional Tokio tasks are spawned.
    pub async fn run(self) -> Result<(), PlayerError> {
        let Self {
            tracks,
            playback: _,
            manifest_metadata: _,
            loop_state,
        } = self;
        loop_state.run(tracks).await
    }

    /// Spawn the stream controller loop as a separate Tokio task.
    ///
    /// Prefer [`Self::run`] when the caller owns concurrency and wants a single task.
    pub fn spawn(self) -> JoinHandle<Result<(), PlayerError>> {
        crate::platform::spawn(async move { self.run().await })
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
                metadata: Default::default(),
            }
            .is_terminal()
        );
        assert!(PlayerEvent::Init(Bytes::new()).is_fragment());
        assert!(
            PlayerEvent::Segment {
                number: 1,
                time: 0,
                presentation_time: Duration::ZERO,
                sub_number: None,
                partial: None,
                data: Bytes::new(),
            }
            .is_fragment()
        );
        assert!(!PlayerEvent::BufferUpdated { buffer_s: 1.0 }.is_fragment());
    }
}
