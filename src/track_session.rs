//! Per-track session state shared between the playback loop and adaptation streams.

use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::clock::resync::ProducerReferenceAnchor;
use crate::delivered_segments::DeliveredSegmentTracker;

/// Mutable per-track state reset on period changes and seeks.
#[derive(Debug, Default)]
pub(crate) struct TrackSessionState {
    have_init: AtomicBool,
    delivered: Mutex<DeliveredSegmentTracker>,
    inband_prt_anchor: Mutex<Option<ProducerReferenceAnchor>>,
}

impl TrackSessionState {
    /// Hard period boundary: clear init, delivery bookkeeping, and PRT anchor.
    pub(crate) fn reset(&self) {
        self.have_init.store(false, Ordering::Release);
        if let Ok(mut delivered) = self.delivered.lock() {
            delivered.reset();
        }
        if let Ok(mut anchor) = self.inband_prt_anchor.lock() {
            *anchor = None;
        }
    }

    /// Soft period transition (continuity / connectivity): reuse Init and absolute delivery frontier.
    pub(crate) fn soft_reset(&self) {
        if let Ok(mut delivered) = self.delivered.lock() {
            delivered.soft_reset();
        }
        if let Ok(mut anchor) = self.inband_prt_anchor.lock() {
            *anchor = None;
        }
    }

    /// Returns `true` when this track should fetch its init segment.
    pub(crate) fn try_take_init(&self) -> bool {
        self.have_init
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn release_init(&self) {
        self.have_init.store(false, Ordering::Release);
    }

    pub(crate) fn lock_delivered(&self) -> MutexGuard<'_, DeliveredSegmentTracker> {
        self.delivered.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn inband_prt_anchor(&self) -> &Mutex<Option<ProducerReferenceAnchor>> {
        &self.inband_prt_anchor
    }

    pub(crate) fn inband_anchor(&self) -> Option<ProducerReferenceAnchor> {
        self.inband_prt_anchor.lock().ok().and_then(|guard| *guard)
    }

    pub(crate) fn last_abs_end_s(&self) -> f64 {
        self.lock_delivered().last_abs_end_s()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_clears_init_delivery_and_anchor() {
        let session = TrackSessionState::default();
        assert!(session.try_take_init());
        {
            let mut delivered = session.lock_delivered();
            delivered.mark_delivered(
                &crate::manifest::TimelineSegment {
                    number: 1,
                    time: 0,
                    duration: 0,
                    duration_s: 4.0,
                    presentation_time_s: 0.0,
                    sub_number: None,
                    resync_start_chunk: None,
                    media_url: None,
                    media_range: None,
                },
                0.0,
            );
        }
        if let Ok(mut anchor) = session.inband_prt_anchor().lock() {
            *anchor = Some(ProducerReferenceAnchor {
                wall_clock_time: chrono::Utc::now(),
                pta_ticks: 1,
                timescale: 1,
            });
        }

        session.reset();

        assert!(session.try_take_init());
        assert!(!session.lock_delivered().is_delivered(
            &crate::manifest::TimelineSegment {
                number: 1,
                time: 0,
                duration: 0,
                duration_s: 4.0,
                presentation_time_s: 0.0,
                sub_number: None,
                resync_start_chunk: None,
                media_url: None,
                media_range: None,
            },
            0.0,
        ));
        assert!(session.inband_anchor().is_none());
    }

    #[test]
    fn soft_reset_keeps_init_and_absolute_frontier() {
        let session = TrackSessionState::default();
        assert!(session.try_take_init());
        {
            let mut delivered = session.lock_delivered();
            delivered.mark_delivered(
                &crate::manifest::TimelineSegment {
                    number: 2,
                    time: 0,
                    duration: 0,
                    duration_s: 4.0,
                    presentation_time_s: 4.0,
                    sub_number: None,
                    resync_start_chunk: None,
                    media_url: None,
                    media_range: None,
                },
                0.0,
            );
        }

        session.soft_reset();

        assert!(!session.try_take_init(), "init should remain taken");
        assert!((session.last_abs_end_s() - 8.0).abs() < 1e-9);
    }
}
