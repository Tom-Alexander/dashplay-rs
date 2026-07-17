//! Public metadata for selected adaptation-set tracks.

use super::kind::{TrackDescriptor, TrackKind};
use super::sub_representation::SubTrackInfo;
use crate::manifest::ContentLabel;

/// Public metadata for one selected adaptation-set track.
#[derive(Debug, Clone, PartialEq)]
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
    /// Effective RFC 6381 codec strings advertised by the adaptation set, representations, or
    /// sub-representations.
    pub codecs: Vec<String>,
    /// `AdaptationSet@minBandwidth` (bps), when present. Metadata only; does not filter ABR.
    pub min_bandwidth_bps: Option<u64>,
    /// `AdaptationSet@maxBandwidth` (bps), when present. Metadata only; does not filter ABR.
    pub max_bandwidth_bps: Option<u64>,
    /// `AdaptationSet@minWidth`, when present. Metadata only.
    pub min_width: Option<u64>,
    /// `AdaptationSet@maxWidth`, when present. Metadata only.
    pub max_width: Option<u64>,
    /// `AdaptationSet@minHeight`, when present. Metadata only.
    pub min_height: Option<u64>,
    /// `AdaptationSet@maxHeight`, when present. Metadata only.
    pub max_height: Option<u64>,
    /// `AdaptationSet@minFrameRate`, when present (e.g. `"30000/1001"`). Metadata only.
    pub min_frame_rate: Option<String>,
    /// `AdaptationSet@maxFrameRate`, when present (e.g. `"30000/1001"`). Metadata only.
    pub max_frame_rate: Option<String>,
    /// Resolved `SubRepresentation` entries under this adaptation set's representations.
    pub sub_tracks: Vec<SubTrackInfo>,
    /// DASH accessibility descriptors as `(schemeIdUri, value)` pairs.
    pub accessibility: Vec<TrackDescriptor>,
    /// `EssentialProperty` descriptors aggregated from the adaptation set and its children.
    pub essential_properties: Vec<TrackDescriptor>,
    /// `SupplementalProperty` descriptors aggregated from the adaptation set and its children.
    pub supplemental_properties: Vec<TrackDescriptor>,
    /// `AdaptationSet/Label` entries (plus `GroupLabel` when present).
    pub labels: Vec<ContentLabel>,
    /// `Rating` descriptors from the adaptation set and its content components.
    pub ratings: Vec<TrackDescriptor>,
    /// `Representation/Label` entries keyed by representation index within the adaptation set.
    pub representation_labels: Vec<(usize, Vec<ContentLabel>)>,
    /// Period adaptation indices that may be switched to (adaptation-set switching / DVB
    /// fallback), excluding this track's own index.
    pub switchable_adaptation_indices: Vec<usize>,
    /// `AdaptationSet@id` values corresponding to [`Self::switchable_adaptation_indices`].
    pub switchable_adaptation_set_ids: Vec<String>,
}
