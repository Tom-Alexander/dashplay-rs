//! Synchronous segment scheduling: decides what to download before any I/O.
//!
//! [`SegmentPlan`] captures the scheduler output (segment index, representation, init
//! requirement, byte range) consumed by fetch, decrypt, and emit stages. Keeping this
//! synchronous makes buffer-target scheduling (P7) testable without HTTP mocks.

use std::collections::HashMap;

use bytes::Bytes;
use dash_mpd::AdaptationSet;

use crate::abr::AbrController;
use crate::manifest::{
    self, ByteRange, SegmentAddressing, SegmentAvailability, TimelineBuildContext, TimelineSegment,
};

/// First-segment init fetch decision for a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InitPlan {
    /// ABR quality rung for the initialization segment fetch.
    pub quality_index: usize,
}

/// Inputs shared across segment plans for one adaptation-set stream.
pub(crate) struct SegmentPlanContext<'a> {
    pub segment_start_index: usize,
    pub adaptation_set: &'a AdaptationSet,
    pub addressing: &'a SegmentAddressing,
    pub timeline_ctx: &'a TimelineBuildContext,
    pub cached_inits: &'a HashMap<String, Bytes>,
}

/// Download plan for one media segment, produced synchronously before fetch/decrypt/emit.
#[derive(Debug, Clone)]
pub(crate) struct SegmentPlan {
    /// Timeline position in the adaptation-set segment list.
    pub list_index: usize,
    /// Index within the current scheduling slice (`segments[start..]`).
    pub local_index: usize,
    /// Segment identity and timing from the timeline engine.
    pub segment: TimelineSegment,
    /// ABR quality rung selected for this segment.
    pub quality_index: usize,
    /// `AdaptationSet.representations` index for the selected rung.
    #[allow(dead_code)]
    pub representation_index: usize,
    /// Init segment for the selected representation is not yet cached.
    #[allow(dead_code)]
    pub init_needed: bool,
    /// Media byte range when known at plan time (e.g. timeline or sidecar index metadata).
    pub media_range: Option<ByteRange>,
    /// Segment is published as LL-DASH chunks requiring per-chunk HTTP transfer.
    pub chunked: bool,
}

/// Plan the initialization-segment fetch for a track that has not yet emitted init.
pub(crate) fn plan_init(abr: &mut dyn AbrController, buffer_s: f64) -> InitPlan {
    abr.update_buffer(buffer_s);
    InitPlan {
        quality_index: abr.decide().quality_index,
    }
}

/// Plan the next media segment download from ABR state and cached init segments.
pub(crate) fn plan_segment(
    abr: &mut dyn AbrController,
    buffer_s: f64,
    segment: &TimelineSegment,
    local_index: usize,
    ctx: &SegmentPlanContext<'_>,
) -> SegmentPlan {
    abr.update_buffer(buffer_s);
    let quality_index = abr.decide().quality_index;
    let representation_index = abr.representation_index_for_quality_index(quality_index);
    let rep = &ctx.adaptation_set.representations[representation_index];
    let rep_id = rep.id.as_deref().unwrap_or_default();
    let init_needed = !ctx.cached_inits.contains_key(rep_id);

    let set_availability = SegmentAvailability::from_addressing(ctx.addressing);
    let chunked = ctx.timeline_ctx.is_dynamic
        && manifest::uses_chunked_segment_transfer(&set_availability, segment);

    SegmentPlan {
        list_index: ctx.segment_start_index + local_index,
        local_index,
        media_range: segment.media_range,
        segment: segment.clone(),
        quality_index,
        representation_index,
        init_needed,
        chunked,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dash_mpd::{AdaptationSet, Representation};

    use crate::abr::{AbrController, AbrDecision, AbrFactory, BolaAbrFactory};
    use crate::manifest::{PeriodWindow, SegmentAddressing, TimelineBuildContext, TimelineSegment};

    use super::*;

    fn timeline_ctx(is_dynamic: bool) -> TimelineBuildContext {
        TimelineBuildContext {
            is_dynamic,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: None,
            },
            period_duration: None,
            media_presentation_duration: None,
            max_segment_duration: None,
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
        }
    }

    fn segment(number: u64) -> TimelineSegment {
        TimelineSegment {
            number,
            time: 0,
            duration: 0,
            duration_s: 4.0,
            presentation_time_s: 0.0,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        }
    }

    fn adaptation_set_with_id(id: &str) -> AdaptationSet {
        AdaptationSet {
            representations: vec![Representation {
                id: Some(id.to_string()),
                bandwidth: Some(1_000_000),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    struct FixedAbr {
        quality_index: usize,
        rep_index: usize,
    }

    impl AbrController for FixedAbr {
        fn update_buffer(&mut self, _buffer_s: f64) {}

        fn observe_segment_download(
            &mut self,
            _throughput_bps: f64,
            _downloaded_bytes: usize,
            _quality_index: usize,
        ) {
        }

        fn decide(&self) -> AbrDecision {
            AbrDecision {
                quality_index: self.quality_index,
                bitrate_bps: 1_000_000.0,
            }
        }

        fn representation_index_for_quality_index(&self, _quality_index: usize) -> usize {
            self.rep_index
        }

        fn bitrate_bps_for_quality_index(&self, _quality_index: usize) -> f64 {
            1_000_000.0
        }

        fn rung_count(&self) -> usize {
            1
        }
    }

    #[test]
    fn plan_init_uses_abr_decision() {
        let set = adaptation_set_with_id("v1");
        let mut abr = BolaAbrFactory::default().create(&set).expect("controller");
        abr.update_buffer(10.0);
        let plan = plan_init(abr.as_mut(), 10.0);
        assert_eq!(plan.quality_index, abr.decide().quality_index);
    }

    #[test]
    fn plan_segment_marks_init_needed_when_rep_not_cached() {
        let set = adaptation_set_with_id("v1");
        let mut abr = Box::new(FixedAbr {
            quality_index: 0,
            rep_index: 0,
        }) as Box<dyn AbrController>;
        let cached = HashMap::new();
        let timeline = timeline_ctx(false);
        let addressing = SegmentAddressing::Template(Default::default());
        let ctx = SegmentPlanContext {
            segment_start_index: 10,
            adaptation_set: &set,
            addressing: &addressing,
            timeline_ctx: &timeline,
            cached_inits: &cached,
        };
        let plan = plan_segment(abr.as_mut(), 5.0, &segment(1), 2, &ctx);
        assert_eq!(plan.list_index, 12);
        assert_eq!(plan.local_index, 2);
        assert_eq!(plan.quality_index, 0);
        assert_eq!(plan.representation_index, 0);
        assert!(plan.init_needed);
        assert!(!plan.chunked);
    }

    #[test]
    fn plan_segment_init_not_needed_when_cached() {
        let set = adaptation_set_with_id("v1");
        let mut cached = HashMap::new();
        cached.insert("v1".to_string(), Bytes::new());
        let mut abr = Box::new(FixedAbr {
            quality_index: 0,
            rep_index: 0,
        }) as Box<dyn AbrController>;
        let timeline = timeline_ctx(false);
        let addressing = SegmentAddressing::Template(Default::default());
        let ctx = SegmentPlanContext {
            segment_start_index: 0,
            adaptation_set: &set,
            addressing: &addressing,
            timeline_ctx: &timeline,
            cached_inits: &cached,
        };
        let plan = plan_segment(abr.as_mut(), 5.0, &segment(1), 0, &ctx);
        assert!(!plan.init_needed);
    }
}
