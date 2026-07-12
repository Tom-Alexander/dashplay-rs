use dash_mpd::AdaptationSet;

use super::types::TimelineSegment;

/// Rewind a timeline index so playback begins at a DASH random-access point aligned with
/// `startWithSAP` semantics on the segment (not an interior `k`-split chunk).
///
/// For `S@k`>1, only the first subsegment shares the segment's SAP boundary at `S@t` unless
/// `AdaptationSet@subsegmentStartsWithSAP` is ≥1, in which case every subsegment is declared
/// to start with SAP.
pub(crate) fn align_start_index_to_sap(
    segments: &[TimelineSegment],
    start_idx: usize,
    adaptation_set: &AdaptationSet,
) -> usize {
    if segments.is_empty() {
        return 0;
    }
    let mut i = start_idx.min(segments.len() - 1);

    if adaptation_set
        .subsegmentStartsWithSAP
        .is_some_and(|v| v >= 1)
    {
        return i;
    }

    while i > 0 {
        match segments[i].sub_number {
            Some(n) if n > 1 => i -= 1,
            _ => break,
        }
    }
    i
}

/// When [`crate::clock::resync::ResyncHints::random_access_interval_s`] is set, snap `start_idx` to the
/// nearest segment on the resync grid (DASH-IF IOP §9.X.6.2.8).
pub(crate) fn align_start_index_to_resync(
    segments: &[TimelineSegment],
    start_idx: usize,
    hints: crate::clock::resync::ResyncHints,
) -> usize {
    let Some(interval_s) = hints
        .random_access_interval_s
        .filter(|x| x.is_finite() && *x > 0.0)
    else {
        return start_idx;
    };
    if segments.is_empty() {
        return 0;
    }
    let anchor_t = segments[start_idx.min(segments.len() - 1)].presentation_time_s;
    let grid_t = (anchor_t / interval_s).floor() * interval_s;
    segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.presentation_time_s <= grid_t + 1e-6)
        .max_by(|(_, a), (_, b)| {
            a.presentation_time_s
                .partial_cmp(&b.presentation_time_s)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(start_idx)
}

/// Presentation-time seconds (relative to Period start) where a segment sequence begins.
fn segment_sequence_start_presentation_s(seg: &TimelineSegment) -> f64 {
    if let Some(sub) = seg.sub_number {
        let prior = sub.saturating_sub(1) as f64;
        seg.presentation_time_s - prior * seg.duration_s
    } else {
        seg.presentation_time_s
    }
}

/// Align seek/recovery to the nearest in-segment resync point (`Resync@type` 2/3).
///
/// Returns the timeline index and an optional 1-based CMAF chunk index to start emitting from
/// within the first segment of the trimmed playback window.
pub(crate) fn mid_segment_resync_alignment(
    segments: &[TimelineSegment],
    start_idx: usize,
    target_presentation_time_s: f64,
    hints: crate::clock::resync::ResyncHints,
) -> (usize, Option<u64>) {
    let Some(interval_s) = hints
        .random_access_interval_s
        .filter(|x| x.is_finite() && *x > 0.0)
    else {
        return (start_idx, None);
    };
    if segments.is_empty() {
        return (0, None);
    }

    let idx = start_idx.min(segments.len() - 1);
    let grid_t = (target_presentation_time_s / interval_s).floor() * interval_s;

    let aligned_idx = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            let start = segment_sequence_start_presentation_s(s);
            start <= grid_t + 1e-6 && start + s.duration_s > grid_t - 1e-6
        })
        .max_by(|(_, a), (_, b)| {
            segment_sequence_start_presentation_s(a)
                .partial_cmp(&segment_sequence_start_presentation_s(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(idx);

    let seq_start = segment_sequence_start_presentation_s(&segments[aligned_idx]);
    let offset_s = (grid_t - seq_start).max(0.0);
    let chunk = (offset_s / interval_s).floor() as u64 + 1;

    (aligned_idx, Some(chunk.max(1)))
}
