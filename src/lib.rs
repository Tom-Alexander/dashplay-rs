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
//!     while let Ok(event) = rx.recv().await {
//!         match event {
//!             PlayerEvent::Init(_) | PlayerEvent::Segment { .. } => { /* decode */ }
//!             PlayerEvent::End => break,
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

mod abr_controller;
pub mod bola;
mod dash_stream;
pub mod drm;
mod manifest;
mod media_player;
mod player;
mod segment_blacklist;
mod segment_fetcher;
mod stream_controller;
mod types;
mod utc_timing;

pub use media_player::{MediaPlayer, WidevineLicenseFetcher};
pub use player::{Player, PlayerTrackOutput};
pub use types::{PlayerEvent, PlayerOutputs, PlayerTrack};

use crate::drm::LicenseError;
use crate::drm::mpd::MpdDrmError;

/// Errors that can occur anywhere in the playback pipeline.
#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("manifest: {0}")]
    Manifest(#[from] dash_mpd::DashMpdError),
    #[error("request: {0}")]
    Request(#[from] reqwest::Error),
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
}
