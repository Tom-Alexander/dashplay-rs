//! Manifest processing: inheritance, timeline expansion, and URL resolution.

mod addressing;
mod alignment;
mod availability;
mod base_url;
mod end_numbers;
pub mod error;
mod fetch;
mod inheritance;
mod period;
mod sidx;
mod template;
mod timeline;
mod types;

pub use error::ManifestError;

pub(crate) use addressing::{
    SegmentAddressing, end_number_for_timeline, resolved_initialization_path,
    segment_addressing_for_representation, segment_addressing_for_timeline,
    segment_base_for_representation, segment_base_uses_sidx_index, segment_list_init_source,
    segment_list_media_for_index, segment_template_for_representation,
    segment_template_uses_global_sidecar_index, segment_template_uses_per_segment_index,
};
pub(crate) use alignment::{
    align_start_index_to_resync, align_start_index_to_sap, mid_segment_resync_alignment,
};
pub(crate) use availability::{
    SegmentAvailability, filter_segments_by_availability, target_presentation_time_at,
    target_presentation_time_from_since, uses_chunked_segment_transfer,
};
pub(crate) use base_url::{SegmentBaseContext, merge_base_url, segment_bases_for_representation};
pub(crate) use end_numbers::{SegmentTemplateEndNumbers, parse_segment_template_end_numbers};
pub(crate) use fetch::{
    media_range_from_per_segment_index, segment_base_index_target, segment_base_init_target,
    segment_base_media_target, segment_template_index_target,
};
pub(crate) use period::{
    current_period_window_at, is_dynamic_mpd, mpd, period_windows, since_availability_start_at,
};
pub(crate) use sidx::{parse_sidx_index_for_segment_base, parse_sidx_index_from_template};
pub(crate) use template::{TemplateVars, interpolate_template, template_vars_for_representation};
pub(crate) use timeline::timeline_segments_for_addressing;
pub(crate) use types::{
    ByteRange, PeriodWindow, SegmentFetchTarget, TimelineBuildContext, TimelineSegment,
};
