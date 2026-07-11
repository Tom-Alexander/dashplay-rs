//! Tracks media segments already emitted on an adaptation-set stream so live manifest
//! refreshes cannot re-deliver or skip fragments.

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

/// Per-stream record of delivered fragments, reset on period transitions.
#[derive(Debug, Default)]
pub(crate) struct DeliveredSegmentTracker {
    keys: HashSet<SegmentKey>,
    /// Period-relative end time of the last delivered segment (seconds).
    last_end_s: f64,
}

impl DeliveredSegmentTracker {
    pub(crate) fn reset(&mut self) {
        self.keys.clear();
        self.last_end_s = 0.0;
    }

    pub(crate) fn is_delivered(&self, seg: &TimelineSegment) -> bool {
        self.keys.contains(&SegmentKey::from(seg)) || seg_end_s(seg) <= self.last_end_s + 1e-9
    }

    /// Advance a timeline start index past segments already delivered on prior manifest snapshots.
    pub(crate) fn advance_start_index(
        &self,
        segments: &[TimelineSegment],
        start_idx: usize,
    ) -> usize {
        let mut i = start_idx.min(segments.len());
        while i < segments.len() && self.is_delivered(&segments[i]) {
            i += 1;
        }
        i
    }

    pub(crate) fn mark_delivered(&mut self, seg: &TimelineSegment) {
        self.keys.insert(SegmentKey::from(seg));
        self.last_end_s = self.last_end_s.max(seg_end_s(seg));
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
            media_url: None,
            media_range: None,
        }
    }

    #[test]
    fn advance_start_index_skips_delivered_segments() {
        let mut tracker = DeliveredSegmentTracker::default();
        let segments = vec![seg(1, 0.0, 4.0), seg(2, 4.0, 4.0), seg(3, 8.0, 4.0)];
        tracker.mark_delivered(&segments[0]);
        tracker.mark_delivered(&segments[1]);

        assert_eq!(tracker.advance_start_index(&segments, 0), 2);
        assert_eq!(tracker.advance_start_index(&segments, 1), 2);
    }

    #[test]
    fn reset_clears_delivery_state() {
        let mut tracker = DeliveredSegmentTracker::default();
        let s = seg(1, 0.0, 4.0);
        tracker.mark_delivered(&s);
        tracker.reset();
        assert!(!tracker.is_delivered(&s));
    }
}
