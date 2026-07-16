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
//!                 // optional: correct the media-clock estimate with the real decoder buffer
//!                 let _ = buffer.report(4.0);
//!             }
//!             PlayerEvent::BufferUpdated { .. }
//!             | PlayerEvent::BitrateChanged { .. }
//!             | PlayerEvent::ManifestLoaded { .. }
//!             | PlayerEvent::PlaybackStarted
//!             | PlayerEvent::PlayheadUpdated { .. }
//!             | PlayerEvent::PlaybackRateSuggested { .. }
//!             | PlayerEvent::TrackChanged { .. }
//!             | PlayerEvent::PeriodChanged { .. }
//!             | PlayerEvent::MediaEvent(_)
//!             | PlayerEvent::CmsdUpdated { .. } => {}
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
pub use abr::lol_plus::{LolPlus, LolPlusDecision};
mod clock;
pub mod cmcd;
mod delivered_segments;
pub mod drm;
pub mod http;
mod manifest;
mod manifest_lifecycle;
mod media_events;
mod media_player;
mod metrics;
mod mp4;
#[doc(hidden)]
pub mod platform;
mod playback_control;
mod player;
mod schedule;
mod segment;
mod segment_blacklist;
mod segment_fetcher;
mod stream_controller;
mod track_selection;
mod track_session;
mod types;

pub use abr::{
    AbrController, AbrCreateContext, AbrDecision, AbrFactory, BolaAbrFactory, LolPlusAbrFactory,
    QualityRung, SharedAbrFactory, quality_ladder_from_adaptation_set,
    quality_ladder_from_adaptation_sets, shared as shared_abr_factory,
};
pub use cmcd::{
    CmcdConfig, CmcdHeaders, CmcdObjectType, CmcdRequestContext, CmcdStreamType, CmsdHop,
    CmsdSnapshot, CmsdValue, apply_cmcd, encode_headers, parse_cmsd_headers,
};
pub use dash_mpd::SubtitleType;
#[cfg(feature = "drm")]
pub use drm::DrmError;
#[cfg(target_arch = "wasm32")]
pub use http::FetchClient;
#[cfg(feature = "reqwest-http")]
pub use http::ReqwestClient;
pub use http::UnconfiguredHttpClient;
pub use http::{
    HttpClient, HttpError, HttpFuture, HttpMethod, HttpRequest, HttpRequestKind, HttpResponse,
    HttpRetryConfig, HttpRetryPolicy, HttpStreamResponse, SharedHttpClient, shared,
};
pub use manifest::{
    AssetIdentifier, ContentLabel, ManifestError, ManifestMetadata, MetricsRange,
    MpdReportingMetrics, PeriodMetadata, ProgramInformation, ReportingDescriptor, Scte214ContentId,
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
pub use segment::SegmentError;
pub use track_selection::{
    SubTrackInfo, TrackDescriptor, TrackInfo, TrackKind, TrackPreference, TrackSelection,
};
pub use types::{
    BufferFeedback, BufferFeedbackError, PartialSegmentChunk, PeriodTransitionKind, PlayerEvent,
    PlayerEventError, PlayerOutputs, PlayerTrack,
};

/// Top-level error for the playback pipeline.
#[derive(Debug, Error)]
pub enum PlayerError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Segment(#[from] SegmentError),
    #[cfg(feature = "drm")]
    #[error(transparent)]
    Drm(#[from] DrmError),
}

#[cfg(not(feature = "drm"))]
impl From<drm::DrmError> for PlayerError {
    fn from(value: drm::DrmError) -> Self {
        match value {}
    }
}

impl From<dash_mpd::DashMpdError> for PlayerError {
    fn from(value: dash_mpd::DashMpdError) -> Self {
        Self::Manifest(ManifestError::Parse(value))
    }
}

impl From<HttpError> for PlayerError {
    fn from(value: HttpError) -> Self {
        Self::Segment(SegmentError::Request(value))
    }
}

impl From<url::ParseError> for PlayerError {
    fn from(value: url::ParseError) -> Self {
        Self::Manifest(ManifestError::Url(value))
    }
}
