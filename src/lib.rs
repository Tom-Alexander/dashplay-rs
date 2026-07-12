//! Pure Rust MPEG-DASH player library.
//!
//! `dashplayrs` implements a modular playback pipeline for MPEG-DASH manifests:
//! manifest parsing, timeline resolution, adaptive bitrate selection, segment
//! scheduling, HTTP download, and optional Widevine decryption.
//!
//! # Quick start
//!
//! ```no_run
//! use dashplayrs::{Player, PlayerEvent};
//!
//! # async fn example() -> Result<(), dashplayrs::PlayerError> {
//! let player = Player::new("https://example.com/manifest.mpd", None)?;
//! let outputs = player.start_tracks().await?;
//!
//! if let Some(mut rx) = outputs.subscribe(0) {
//!     let buffer = outputs.buffer_feedback(0).expect("track");
//!     while let Ok(event) = rx.recv().await {
//!         match event {
//!             PlayerEvent::Init(_) | PlayerEvent::Segment { .. } => {
//!                 // decode, then report buffered seconds ahead of the playhead
//!                 let _ = buffer.report(4.0);
//!             }
//!             PlayerEvent::BufferUpdated { .. }
//!             | PlayerEvent::BitrateChanged { .. }
//!             | PlayerEvent::ManifestLoaded { .. }
//!             | PlayerEvent::PlaybackStarted
//!             | PlayerEvent::PlayheadUpdated { .. }
//!             | PlayerEvent::MediaEvent(_) => {}
//!             PlayerEvent::End | PlayerEvent::PlaybackEnded | PlayerEvent::Error(_) => break,
//!         }
//!     }
//! }
//!
//! outputs.join.await.unwrap()?;
//! # Ok(())
//! # }
//! ```
//!
//! See [`ARCHITECTURE.md`](https://github.com/dashplayrs/dashplayrs/blob/main/ARCHITECTURE.md)
//! for the component layout and design goals.

use thiserror::Error;

pub mod abr;
pub use abr::bola::{Bola, BolaDecision, QualityLevel};
mod clock;
mod delivered_segments;
pub mod drm;
pub mod http;
mod manifest;
mod manifest_lifecycle;
mod media_events;
mod media_player;
mod metrics;
mod mp4;
mod playback_control;
mod player;
mod schedule;
mod segment_blacklist;
mod segment_fetcher;
mod stream_controller;
mod track_selection;
mod types;

pub use abr::{
    AbrController, AbrDecision, AbrFactory, BolaAbrFactory, QualityRung, SharedAbrFactory,
    quality_ladder_from_adaptation_set, shared as shared_abr_factory,
};
pub use dash_mpd::SubtitleType;
pub use http::{
    HttpClient, HttpError, HttpMethod, HttpRequest, HttpResponse, ReqwestClient, SharedHttpClient,
    shared,
};
pub use media_events::{MediaEvent, MediaEventSource, Scte35Cue};
pub use media_player::{MediaPlayer, WidevineLicenseFetcher};
pub use metrics::{
    BitrateSwitch, BufferSample, RebufferEvent, ThroughputSample, TrackMetrics,
    TrackMetricsSnapshot,
};
pub use playback_control::{PlaybackControlError, PlaybackController, PlaybackState};
pub use player::{
    Player, PlayerMergedAsyncRead, PlayerMergedOutput, PlayerTrackOutput, PlayerTrackOutputs,
};
pub use track_selection::{TrackDescriptor, TrackInfo, TrackKind, TrackPreference, TrackSelection};
pub use types::{
    BufferFeedback, BufferFeedbackError, PartialSegmentChunk, PlayerEvent, PlayerEventError,
    PlayerOutputs, PlayerTrack,
};

use crate::drm::LicenseError;
use crate::drm::mp4::Mp4DrmError;
use crate::drm::mpd::MpdDrmError;

/// Errors that can occur anywhere in the playback pipeline.
#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("manifest: {0}")]
    Manifest(#[from] dash_mpd::DashMpdError),
    #[error("request: {0}")]
    Request(#[from] HttpError),
    #[error("widevine license HTTP: {0}")]
    WidevineLicenseHttp(String),
    #[error("url: {0}")]
    Url(#[from] url::ParseError),
    #[error("manifest not loaded")]
    ManifestNotLoaded,
    #[error("MPD has no Period")]
    NoPeriod,
    #[error("missing SegmentTemplate")]
    MissingSegmentTemplate,
    #[error("missing SegmentList")]
    MissingSegmentList,
    #[error("missing SegmentBase")]
    MissingSegmentBase,
    #[error("invalid byte range specifier: {0}")]
    InvalidByteRange(String),
    #[error("missing SegmentBase@indexRange")]
    MissingSegmentBaseIndexRange,
    #[error("SegmentBase@indexRange timeline requires fetched sidx index")]
    SegmentBaseIndexNotLoaded,
    #[error("missing SegmentTemplate@indexRange (sidecar index)")]
    MissingSegmentTemplateIndexRange,
    #[error("missing SegmentTemplate@index (sidecar index)")]
    MissingSegmentTemplateIndex,
    #[error("SegmentTemplate@index sidecar timeline requires fetched sidx index")]
    SegmentTemplateIndexNotLoaded,
    #[error("SegmentTemplate@index with $Number$ or $Time$ requires segment number or time")]
    MissingSegmentTemplateIndexVars,
    #[error("failed to parse sidx index: {0}")]
    SidxParse(String),
    #[error("hierarchical sidx references are not supported")]
    HierarchicalSidxNotSupported,
    #[error("SegmentList SegmentURL count does not match expanded timeline")]
    SegmentListUrlTimelineMismatch,
    #[error("SegmentList has no SegmentURL entries")]
    EmptySegmentList,
    #[error("missing SegmentTemplate@initialization")]
    MissingInitializationTemplate,
    #[error("missing SegmentTemplate@media")]
    MissingMediaTemplate,
    #[error("missing SegmentTemplate@duration (no SegmentTimeline)")]
    MissingSegmentDuration,
    #[error("SegmentTemplate@timescale is zero")]
    ZeroTimescale,
    #[error("SegmentTimeline S@d is zero")]
    ZeroTimelineSegmentDuration,
    #[error("SegmentTimeline S@k must be at least 1")]
    InvalidTimelineSegmentK,
    #[error("SegmentTimeline S@d must be divisible by S@k when k > 1 (segment sequences)")]
    TimelineDNotDivisibleByK,
    #[error("dynamic template without @duration addressing needs MPD@availabilityStartTime")]
    MissingAvailabilityStartForDynamicTemplate,
    #[error("static SegmentTemplate@duration needs Period or MPD duration to bound segment count")]
    MissingPeriodExtentForStaticTemplate,
    #[error("SegmentTemplate@endNumber is less than @startNumber")]
    InvalidSegmentTemplateEndNumber,
    #[error(
        "SegmentTimeline S@r<0 needs a following S@t, Period end, or (for dynamic MPD) availabilityStartTime"
    )]
    UnboundedSegmentTimelineRepeat,
    #[error("segment URL blacklisted: {0}")]
    SegmentBlacklisted(String),
    #[error("segment request failed: HTTP {status} for {url}")]
    SegmentRequestFailed { status: u16, url: String },
    #[error("all representation attempts failed for a segment")]
    SegmentExhaustedRepresentations,
    #[error("widevine license/decrypt: {0}")]
    License(#[from] LicenseError),
    #[error("mpd drm parse: {0}")]
    DrmMpd(#[from] MpdDrmError),
    #[error("in-band mp4 drm parse: {0}")]
    InBandDrm(#[from] Mp4DrmError),
}
