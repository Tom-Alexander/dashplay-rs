use thiserror::Error;

/// Errors from manifest parsing, timeline expansion, and URL resolution.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("parse: {0}")]
    Parse(#[from] dash_mpd::DashMpdError),
    #[error("url: {0}")]
    Url(#[from] url::ParseError),
    #[error("manifest not loaded")]
    NotLoaded,
    #[error("MPD has no Period")]
    NoPeriod,
    #[error("missing SegmentTemplate")]
    MissingSegmentTemplate,
    #[error("missing SegmentList")]
    MissingSegmentList,
    #[error("missing SegmentBase")]
    MissingSegmentBase,
    /// ISO/IEC 23009-1 §5.3.9: at most one of `SegmentTemplate`, `SegmentList`, or
    /// `SegmentBase` may appear on the same Period, AdaptationSet, or Representation.
    #[error(
        "conflicting segment addressing modes at {0}: at most one of SegmentTemplate, SegmentList, or SegmentBase may appear at the same hierarchy level"
    )]
    ConflictingSegmentAddressing(&'static str),
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
    #[error("missing RepresentationIndex@sourceURL")]
    MissingRepresentationIndexSourceUrl,
    #[error("SegmentTemplate@index sidecar timeline requires fetched sidx index")]
    SegmentTemplateIndexNotLoaded,
    #[error("SegmentTemplate@index with $Number$ or $Time$ requires segment number or time")]
    MissingSegmentTemplateIndexVars,
    #[error("failed to parse sidx index: {0}")]
    SidxParse(String),
    /// `@indexRangeExact` is false (or absent) and the Index Segment extends past the fetched
    /// bytes. `need_end` is the inclusive absolute file offset that must still be fetched.
    #[error("sidx index is incomplete; need bytes through offset {need_end}")]
    IncompleteSidxIndex { need_end: u64 },
    #[error(
        "hierarchical sidx reference is outside the fetched index bytes (interleaved same-file nest not fetched)"
    )]
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
    #[error("segment duration exceeds MPD@maxSegmentDuration")]
    SegmentDurationExceedsMaxSegmentDuration,
    #[error(
        "SegmentTimeline S@r<0 needs a following S@t, Period end, or (for dynamic MPD) availabilityStartTime"
    )]
    UnboundedSegmentTimelineRepeat,
}
