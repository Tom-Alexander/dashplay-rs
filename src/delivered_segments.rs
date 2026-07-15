//! Tracks media segments already emitted on an adaptation-set stream so live manifest
//! refreshes cannot re-deliver or skip fragments.
//!
//! Absolute presentation frontiers survive soft period transitions so boundary-overlapping
//! segments in period-connected Adaptation Sets are not presented twice.

use std::collections::HashSet;

use super::manifest::TimelineSegment;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SegmentKey {
    number: u64,
    time: u64,
    sub_number: Option<u64>,
}

impl From<&TimelineSegment> for SegmentKey {
    fn from(seg: &TimelineSegment) -> Self {
        Self {
            number: seg.number,
            time: seg.time,
            sub_number: seg.sub_number,
        }
    }
}

/// Per-stream record of delivered fragments.
#[derive(Debug, Default)]
pub(crate) struct DeliveredSegmentTracker {
    keys: HashSet<SegmentKey>,
    /// Media URLs already emitted (survives soft period transitions for boundary dedup).
    media_urls: HashSet<String>,
    /// Period-relative end time of the last delivered segment (seconds).
    last_end_s: f64,
    /// Absolute presentation end of the last delivered segment (PeriodStart + relative end).
    last_abs_end_s: f64,
    /// `PeriodStart` for which [`Self::last_end_s`] / [`Self::keys`] are valid.
    active_period_start_s: f64,
}

impl DeliveredSegmentTracker {
    pub(crate) fn reset(&mut self) {
        self.keys.clear();
        self.media_urls.clear();
        self.last_end_s = 0.0;
        self.last_abs_end_s = 0.0;
        self.active_period_start_s = 0.0;
    }

    /// Soft period transition: drop period-local keys while keeping absolute frontier and URLs.
    pub(crate) fn soft_reset(&mut self) {
        self.keys.clear();
        self.last_end_s = 0.0;
    }

    pub(crate) fn last_abs_end_s(&self) -> f64 {
        self.last_abs_end_s
    }

    pub(crate) fn is_delivered(&self, seg: &TimelineSegment, period_start_s: f64) -> bool {
        if let Some(url) = seg.media_url.as_deref()
            && self.media_urls.contains(url)
        {
            return true;
        }
        let abs_start = period_start_s + seg.presentation_time_s;
        if abs_start + 1e-9 < self.last_abs_end_s {
            return true;
        }
        // Period-relative keys/`last_end_s` apply only within the same PeriodStart.
        if (period_start_s - self.active_period_start_s).abs() > 1e-9 {
            return false;
        }
        self.keys.contains(&SegmentKey::from(seg)) || seg_end_s(seg) <= self.last_end_s + 1e-9
    }

    /// Advance a timeline start index past segments already delivered on prior manifest snapshots.
    pub(crate) fn advance_start_index(
        &self,
        segments: &[TimelineSegment],
        start_idx: usize,
        period_start_s: f64,
    ) -> usize {
        let mut i = start_idx.min(segments.len());
        while i < segments.len() && self.is_delivered(&segments[i], period_start_s) {
            i += 1;
        }
        i
    }

    pub(crate) fn mark_delivered(&mut self, seg: &TimelineSegment, period_start_s: f64) {
        if (period_start_s - self.active_period_start_s).abs() > 1e-9 {
            self.keys.clear();
            self.last_end_s = 0.0;
            self.active_period_start_s = period_start_s;
        }
        self.keys.insert(SegmentKey::from(seg));
        if let Some(url) = seg.media_url.clone() {
            self.media_urls.insert(url);
        }
        let rel_end = seg_end_s(seg);
        self.last_end_s = self.last_end_s.max(rel_end);
        self.last_abs_end_s = self.last_abs_end_s.max(period_start_s + rel_end);
    }
}

fn seg_end_s(seg: &TimelineSegment) -> f64 {
    seg.presentation_time_s + seg.duration_s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(number: u64, start_s: f64, duration_s: f64) -> TimelineSegment {
        TimelineSegment {
            number,
            time: 0,
            duration: 0,
            duration_s,
            presentation_time_s: start_s,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        }
    }

    #[test]
    fn advance_start_index_skips_delivered_segments() {
        let mut tracker = DeliveredSegmentTracker::default();
        let segments = vec![seg(1, 0.0, 4.0), seg(2, 4.0, 4.0), seg(3, 8.0, 4.0)];
        tracker.mark_delivered(&segments[0], 0.0);
        tracker.mark_delivered(&segments[1], 0.0);

        assert_eq!(tracker.advance_start_index(&segments, 0, 0.0), 2);
        assert_eq!(tracker.advance_start_index(&segments, 1, 0.0), 2);
    }

    #[test]
    fn reset_clears_delivery_state() {
        let mut tracker = DeliveredSegmentTracker::default();
        let s = seg(1, 0.0, 4.0);
        tracker.mark_delivered(&s, 0.0);
        tracker.reset();
        assert!(!tracker.is_delivered(&s, 0.0));
        assert_eq!(tracker.last_abs_end_s(), 0.0);
    }

    #[test]
    fn soft_reset_keeps_absolute_frontier_for_overlap_dedup() {
        let mut tracker = DeliveredSegmentTracker::default();
        // Period 0: segment covering [4, 8) absolute when period starts at 0.
        tracker.mark_delivered(&seg(2, 4.0, 4.0), 0.0);
        assert!((tracker.last_abs_end_s() - 8.0).abs() < 1e-9);

        tracker.soft_reset();
        // Period 1 starts at 8s; overlapping boundary segment relative [0, 4) → abs [8, 12).
        // A segment that starts before last_abs_end is skipped; one at the boundary is kept.
        let overlap = seg(1, 0.0, 4.0);
        // abs start 6 (< 8) when period_start is 6 — overlap with previous end.
        assert!(tracker.is_delivered(&overlap, 6.0));
        // Fresh period at 8s with relative 0 is not overlapping.
        assert!(!tracker.is_delivered(&seg(1, 0.0, 4.0), 8.0));
    }

    #[test]
    fn soft_reset_keeps_media_url_dedup_for_boundary_segments() {
        let mut tracker = DeliveredSegmentTracker::default();
        let mut boundary = seg(2, 4.0, 4.0);
        boundary.media_url = Some("boundary.m4s".into());
        tracker.mark_delivered(&boundary, 0.0);
        tracker.soft_reset();

        let mut again = seg(1, 0.0, 4.0);
        again.media_url = Some("boundary.m4s".into());
        assert!(tracker.is_delivered(&again, 8.0));
    }
}
