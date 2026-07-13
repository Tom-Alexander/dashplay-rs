use std::time::Duration;

use crate::manifest::ManifestError;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PeriodWindow {
    pub idx: usize,
    pub start: Duration,
    pub end: Option<Duration>,
}

/// Wall-clock and MPD metadata for `SegmentTemplate@duration` (dynamic window) and for filtering
/// explicit `SegmentTimeline` on dynamic MPDs (time-shift buffer vs `availabilityStartTime`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimelineBuildContext {
    pub is_dynamic: bool,
    pub period_window: PeriodWindow,
    /// `Period@duration` when present.
    pub period_duration: Option<Duration>,
    pub media_presentation_duration: Option<Duration>,
    /// `MPD@maxSegmentDuration` — reject expanded segments longer than this bound.
    pub max_segment_duration: Option<Duration>,
    pub time_shift_buffer_depth: Option<Duration>,
    pub since_availability_start: Option<Duration>,
    pub resync_hints: Option<crate::clock::resync::ResyncHints>,
}

impl TimelineBuildContext {
    pub(crate) fn period_length_secs(self) -> Option<f64> {
        if let Some(end) = self.period_window.end {
            let d = end.saturating_sub(self.period_window.start);
            if !d.is_zero() {
                return Some(d.as_secs_f64());
            }
        }
        if let Some(d) = self.period_duration {
            if !d.is_zero() {
                return Some(d.as_secs_f64());
            }
        }
        self.media_presentation_duration
            .filter(|d| !d.is_zero())
            .map(|d| d.as_secs_f64())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TimelineSegment {
    pub number: u64,
    /// MPD anchor for `$Time$` (for `S@k`>1, earliest presentation time of the whole sequence).
    pub time: u64,
    /// Segment duration in MPD timescale ticks (mirrors `S@d`; playback uses `duration_s`).
    #[allow(dead_code)]
    pub duration: u64,
    pub duration_s: f64,
    /// Segment start time in seconds relative to the Period start.
    pub presentation_time_s: f64,
    /// When `S@k`>1: 1-based index within the segment sequence (`$SubNumber$`). Otherwise `None`.
    pub sub_number: Option<u64>,
    /// 1-based CMAF chunk index to start emitting from after mid-segment resync (`Resync@type` 2/3).
    pub resync_start_chunk: Option<u64>,
    /// Explicit `SegmentURL@media` when using `SegmentList` addressing (may be rep-specific).
    pub media_url: Option<String>,
    /// Inclusive byte range for media (`SegmentURL@mediaRange`, `SegmentBase`/`sidx`, etc.).
    pub media_range: Option<ByteRange>,
}

/// Inclusive byte range (`start`..=`end`) for HTTP Range requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// Relative path plus optional byte range for a segment or init fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentFetchTarget {
    pub path: String,
    pub range: Option<ByteRange>,
}

/// Parse a DASH range specifier (`start-end`, inclusive).
pub(crate) fn parse_byte_range(range: &str) -> Result<ByteRange, ManifestError> {
    let parts: Vec<&str> = range.split('-').collect();
    if parts.len() != 2 {
        return Err(ManifestError::InvalidByteRange(range.to_string()));
    }
    let start: u64 = parts[0]
        .parse()
        .map_err(|_| ManifestError::InvalidByteRange(range.to_string()))?;
    let end: u64 = parts[1]
        .parse()
        .map_err(|_| ManifestError::InvalidByteRange(range.to_string()))?;
    if end < start {
        return Err(ManifestError::InvalidByteRange(range.to_string()));
    }
    Ok(ByteRange { start, end })
}

#[cfg(test)]
mod tests;
