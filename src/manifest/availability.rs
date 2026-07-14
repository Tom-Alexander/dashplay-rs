use std::time::Duration;

use chrono::{DateTime, Utc};
use dash_mpd::MPD;

use crate::manifest::ManifestError;

use super::addressing::SegmentAddressing;
use super::period::since_availability_start_at;
use super::types::TimelineSegment;

/// Low-latency segment availability attributes from merged segment addressing (ISO 23009-1 §5.3.9.6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SegmentAvailability {
    /// `@availabilityTimeOffset` in seconds; `None` in XML means zero.
    pub availability_time_offset_s: Option<f64>,
    /// `@availabilityTimeComplete`; absent means `true`.
    pub availability_time_complete: bool,
}

impl SegmentAvailability {
    pub(crate) fn from_addressing(addressing: &SegmentAddressing) -> Self {
        match addressing {
            SegmentAddressing::Template(st) => Self {
                availability_time_offset_s: st.availabilityTimeOffset,
                availability_time_complete: st.availabilityTimeComplete.unwrap_or(true),
            },
            SegmentAddressing::List(_sl) => Self {
                availability_time_offset_s: None,
                availability_time_complete: true,
            },
            SegmentAddressing::Base(sb) => Self {
                availability_time_offset_s: sb.availabilityTimeOffset,
                availability_time_complete: sb.availabilityTimeComplete.unwrap_or(true),
            },
        }
    }
}

/// `@target` from the first [`ServiceDescription::Latency`] entry (milliseconds per DASH-IF IOP).
pub(crate) fn target_latency_from_mpd(mpd: &MPD) -> Option<Duration> {
    crate::clock::latency_control::LatencyPolicy::from_mpd(mpd).map(|p| p.target)
}

/// MPD media-timeline seconds (from `availabilityStartTime`) when a segment sequence starts.
pub(crate) fn segment_sequence_start_s(period_start: Duration, seg: &TimelineSegment) -> f64 {
    let period_start_s = period_start.as_secs_f64();
    let seq_start_s = if let Some(sub) = seg.sub_number {
        let prior = sub.saturating_sub(1) as f64;
        seg.presentation_time_s - prior * seg.duration_s
    } else {
        seg.presentation_time_s
    };
    period_start_s + seq_start_s
}

/// Whether a segment is published and fetchable at `since_availability_start`.
pub(crate) fn segment_is_available(
    seg: &TimelineSegment,
    period_start: Duration,
    since_availability_start: Duration,
    availability: &SegmentAvailability,
) -> bool {
    let ato = availability.availability_time_offset_s.unwrap_or(0.0);
    if ato.is_infinite() {
        return ato.is_sign_positive();
    }
    if ato.is_nan() {
        return true;
    }

    let now_s = since_availability_start.as_secs_f64();

    if !availability.availability_time_complete {
        if seg.sub_number.is_some() {
            return now_s + 1e-6 >= period_start.as_secs_f64() + seg.presentation_time_s;
        }
        let sap_s = segment_sequence_start_s(period_start, seg);
        return now_s + 1e-6 >= sap_s;
    }

    let sap_s = segment_sequence_start_s(period_start, seg);
    now_s + 1e-6 >= sap_s + ato.max(0.0)
}

/// `@availabilityTimeComplete=false` on a whole segment (no `S@k` sub-number): fetch via chunked HTTP.
pub(crate) fn uses_chunked_segment_transfer(
    availability: &SegmentAvailability,
    seg: &TimelineSegment,
) -> bool {
    !availability.availability_time_complete && seg.sub_number.is_none()
}

/// Drop segments that are not yet published on dynamic MPDs.
pub(crate) fn filter_segments_by_availability(
    segments: Vec<TimelineSegment>,
    is_dynamic: bool,
    period_start: Duration,
    since_availability_start: Option<Duration>,
    addressing: &SegmentAddressing,
) -> Vec<TimelineSegment> {
    if !is_dynamic {
        return segments;
    }
    let Some(since) = since_availability_start else {
        return segments;
    };
    let availability = SegmentAvailability::from_addressing(addressing);
    segments
        .into_iter()
        .filter(|s| segment_is_available(s, period_start, since, &availability))
        .collect()
}

pub(crate) fn target_presentation_time_from_since(mpd: &MPD, since_ast: Duration) -> Duration {
    let mut t = since_ast;
    if let Some(latency) = target_latency_from_mpd(mpd) {
        t = t.saturating_sub(latency);
    } else if let Some(delay) = mpd.suggestedPresentationDelay {
        t = t.saturating_sub(delay);
    }
    t
}

pub(crate) fn target_presentation_time_at(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
) -> Result<Option<Duration>, ManifestError> {
    let Some(since_ast) = since_availability_start_at(mpd, wall_now)? else {
        return Ok(None);
    };
    Ok(Some(target_presentation_time_from_since(mpd, since_ast)))
}

#[cfg(test)]
mod tests;
