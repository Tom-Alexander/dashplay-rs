//! Public metadata for selected adaptation-set tracks.

use super::kind::{TrackDescriptor, TrackKind};

/// Public metadata for one selected adaptation-set track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// Index of this adaptation set within the containing `Period.adaptations` list.
    pub period_adaptation_index: usize,
    /// `AdaptationSet@id`, when present.
    pub id: Option<String>,
    /// Whether this is an audio, video, or text track.
    pub kind: TrackKind,
    /// Subtitle or caption format when [`Self::kind`] is [`TrackKind::Text`].
    pub subtitle_type: Option<dash_mpd::SubtitleType>,
    /// Thumbnail tile layout `(horizontal_tiles, vertical_tiles)` when [`Self::kind`] is
    /// [`TrackKind::Image`] and the manifest declares `thumbnail_tile`.
    pub thumbnail_tile: Option<(u32, u32)>,
    /// Effective MIME type from the adaptation set or one of its representations.
    pub mime_type: Option<String>,
    /// Effective RFC 5646 language from the adaptation set, content component, or representation.
    pub language: Option<String>,
    /// DASH `Role@value` values.
    pub roles: Vec<String>,
    /// Effective RFC 6381 codec strings advertised by the adaptation set or representations.
    pub codecs: Vec<String>,
    /// DASH accessibility descriptors as `(schemeIdUri, value)` pairs.
    pub accessibility: Vec<TrackDescriptor>,
    /// `EssentialProperty` descriptors aggregated from the adaptation set and its children.
    pub essential_properties: Vec<TrackDescriptor>,
    /// `SupplementalProperty` descriptors aggregated from the adaptation set and its children.
    pub supplemental_properties: Vec<TrackDescriptor>,
}
