//! CMCD request context and object/stream type tokens.

use crate::track_selection::TrackKind;

/// CTA-5004 `ot` object type token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmcdObjectType {
    Manifest,
    Audio,
    Video,
    Muxed,
    Init,
    Caption,
    TimedText,
    Key,
    Other,
}

impl CmcdObjectType {
    /// Map a selected track kind to a media object type (not init/manifest).
    pub fn from_track_kind(kind: TrackKind) -> Self {
        match kind {
            TrackKind::Audio => Self::Audio,
            TrackKind::Video | TrackKind::TrickPlay => Self::Video,
            TrackKind::Text => Self::TimedText,
            TrackKind::Image => Self::Other,
        }
    }
}

/// CTA-5004 `st` stream type token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmcdStreamType {
    Vod,
    Live,
}

impl CmcdStreamType {
    pub fn from_dynamic(is_dynamic: bool) -> Self {
        if is_dynamic { Self::Live } else { Self::Vod }
    }
}

/// Per-request values used to build CMCD headers.
#[derive(Debug, Clone, PartialEq)]
pub struct CmcdRequestContext {
    pub session_id: String,
    pub content_id: Option<String>,
    pub stream_type: CmcdStreamType,
    pub object_type: CmcdObjectType,
    /// Encoded bitrate in **kbps** (`br`).
    pub encoded_bitrate_kbps: Option<u64>,
    /// Object duration in milliseconds (`d`).
    pub object_duration_ms: Option<u64>,
    /// Buffer length in milliseconds (`bl`).
    pub buffer_length_ms: Option<u64>,
    /// Measured throughput in kbps before nearest-100 rounding (`mtp`).
    pub measured_throughput_kbps: Option<u64>,
    /// Startup request (`su`).
    pub startup: bool,
    /// Buffer starvation since the prior request (`bs`).
    pub buffer_starvation: bool,
    /// Relative next object URL (`nor`), already percent-encoded when required by CTA-5004.
    pub next_object_request: Option<String>,
    /// Next byte range as `start-end` (`nrr`).
    pub next_range_request: Option<String>,
}
