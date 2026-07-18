//! Synchronous segment scheduling: decides what to download before any I/O.
//!
//! [`SegmentPlan`] captures the scheduler output (segment index, representation, init
//! requirement, byte range) consumed by fetch, decrypt, and emit stages. Keeping this
//! synchronous makes buffer-target scheduling (P7) testable without HTTP mocks.

use std::collections::HashMap;

use bytes::Bytes;
use dash_mpd::AdaptationSet;

use crate::abr::{
    AbrController, DroppedFramesHistory, QualityConstraints, apply_dropped_frames_cap,
    clamp_quality_index,
};
use crate::manifest::{
    self, ByteRange, SegmentAvailability, SwitchingHint, TimelineBuildContext, TimelineSegment,
    is_switch_opportunity, switching_hints_for,
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
    /// Period adaptation index of the primary (selected) adaptation set.
    pub primary_period_adaptation_index: usize,
    /// Primary and switch/fallback peers keyed by period adaptation index.
    pub adaptation_sets: &'a HashMap<usize, AdaptationSet>,
    /// `@bitstreamSwitching` (or equivalent) per period adaptation index.
    #[allow(dead_code)]
    pub bitstream_switching: &'a HashMap<usize, bool>,
    /// Merged segment + BaseURL availability for this adaptation set.
    pub set_availability: SegmentAvailability,
    pub timeline_ctx: &'a TimelineBuildContext,
    /// Cached init segments keyed by `(period_adaptation_index, representation_id)`.
    pub cached_inits: &'a HashMap<(usize, String), Bytes>,
    /// Previous ABR quality index; used to hold switches until a `Switching` opportunity.
    pub last_quality_index: Option<usize>,
    /// User quality constraints applied after each ABR decision (fixed quality / autoswitch).
    pub quality_constraints: QualityConstraints,
    /// Host-reported dropped-frame history for the ABR down-switch rule.
    pub dropped_frames: Option<&'a DroppedFramesHistory>,
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
    /// Period adaptation index that owns the selected representation.
    #[allow(dead_code)]
    pub period_adaptation_index: usize,
    /// Init segment for the selected representation is not yet cached.
    #[allow(dead_code)]
    pub init_needed: bool,
    /// Media byte range when known at plan time (e.g. timeline or sidecar index metadata).
    pub media_range: Option<ByteRange>,
    /// Segment is published as LL-DASH chunks requiring per-chunk HTTP transfer.
    pub chunked: bool,
}

/// Plan the initialization-segment fetch for a track that has not yet emitted init.
pub(crate) fn plan_init(
    abr: &mut dyn AbrController,
    buffer_s: f64,
    constraints: &QualityConstraints,
    dropped_frames: Option<&DroppedFramesHistory>,
) -> InitPlan {
    abr.update_buffer(buffer_s);
    let decided = clamp_quality_index(abr.decide().quality_index, abr.rung_count(), constraints);
    let quality_index =
        apply_dropped_frames_cap(decided, abr.rung_count(), constraints, dropped_frames);
    InitPlan { quality_index }
}

fn switching_hints_for_rung(
    ctx: &SegmentPlanContext<'_>,
    period_adaptation_index: usize,
    representation_index: usize,
) -> Vec<SwitchingHint> {
    let Some(adaptation_set) = ctx
        .adaptation_sets
        .get(&period_adaptation_index)
        .or_else(|| {
            ctx.adaptation_sets
                .get(&ctx.primary_period_adaptation_index)
        })
    else {
        return Vec::new();
    };
    let rep = adaptation_set.representations.get(representation_index);
    let mut hints = switching_hints_for(adaptation_set, rep);
    // Cross-AS switching: when the target has no Switching of its own, use group signalling.
    if hints.is_empty() {
        for aset in ctx.adaptation_sets.values() {
            hints.extend(switching_hints_for(aset, None));
        }
    }
    hints
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
    let mut quality_index = clamp_quality_index(
        abr.decide().quality_index,
        abr.rung_count(),
        &ctx.quality_constraints,
    );
    quality_index = apply_dropped_frames_cap(
        quality_index,
        abr.rung_count(),
        &ctx.quality_constraints,
        ctx.dropped_frames,
    );

    // ISO/IEC 23009-1 §5.3.3.4: when `Switching` is present, only change representation
    // at switch-to opportunities for the target.
    if let Some(prev_q) = ctx.last_quality_index {
        if prev_q != quality_index {
            let rung = abr.rung_for_quality_index(quality_index);
            let hints = switching_hints_for_rung(
                ctx,
                rung.period_adaptation_index,
                rung.representation_index,
            );
            if !hints.is_empty() && !is_switch_opportunity(segment, &hints) {
                quality_index = prev_q;
            }
        }
    }

    let rung = abr.rung_for_quality_index(quality_index);
    let period_adaptation_index = rung.period_adaptation_index;
    let representation_index = rung.representation_index;
    let adaptation_set = ctx
        .adaptation_sets
        .get(&period_adaptation_index)
        .or_else(|| {
            ctx.adaptation_sets
                .get(&ctx.primary_period_adaptation_index)
        })
        .expect("primary adaptation set present");
    let rep = &adaptation_set.representations[representation_index];
    let rep_id = rep.id.as_deref().unwrap_or_default();
    let cache_key = (period_adaptation_index, rep_id.to_string());
    // Always load the Init for the selected Representation. `@bitstreamSwitching` allows
    // seamless switches at SAP points; it does not imply Initialization Segments are shared.
    let init_needed = !ctx.cached_inits.contains_key(&cache_key);

    let set_availability = ctx.set_availability;
    let chunked = ctx.timeline_ctx.is_dynamic
        && manifest::uses_chunked_segment_transfer(&set_availability, segment);

    SegmentPlan {
        list_index: ctx.segment_start_index + local_index,
        local_index,
        media_range: segment.media_range,
        segment: segment.clone(),
        quality_index,
        representation_index,
        period_adaptation_index,
        init_needed,
        chunked,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dash_mpd::{AdaptationSet, Representation};

    use crate::abr::{
        AbrController, AbrDecision, AbrFactory, BolaAbrFactory, QualityConstraints, QualityRung,
    };
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
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
            must_cover_presentation_s: None,
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

    fn single_set_map(aset: AdaptationSet) -> HashMap<usize, AdaptationSet> {
        let mut map = HashMap::new();
        map.insert(0, aset);
        map
    }

    fn no_bitstream() -> HashMap<usize, bool> {
        HashMap::new()
    }

    struct FixedAbr {
        quality_index: usize,
        rung: QualityRung,
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

        fn decide(&mut self) -> AbrDecision {
            AbrDecision {
                quality_index: self.quality_index,
                bitrate_bps: self.rung.bitrate_bps,
            }
        }

        fn rung_for_quality_index(&self, quality_index: usize) -> &QualityRung {
            // Single-rung fixtures ignore quality_index; multi-rep tests build a matching rung.
            let _ = quality_index;
            &self.rung
        }

        fn rung_count(&self) -> usize {
            1
        }
    }

    fn fixed_abr(quality_index: usize, rep_index: usize) -> FixedAbr {
        FixedAbr {
            quality_index,
            rung: QualityRung {
                period_adaptation_index: 0,
                representation_index: rep_index,
                label: String::new(),
                bitrate_bps: 1_000_000.0,
                quality_ranking: None,
                max_playout_rate: None,
                coding_dependency: None,
            },
        }
    }

    /// ABR that maps quality index to representation index 1:1 for Switching tests.
    struct IndexedAbr {
        desired_quality: usize,
        rungs: Vec<QualityRung>,
    }

    impl AbrController for IndexedAbr {
        fn update_buffer(&mut self, _buffer_s: f64) {}

        fn observe_segment_download(
            &mut self,
            _throughput_bps: f64,
            _downloaded_bytes: usize,
            _quality_index: usize,
        ) {
        }

        fn decide(&mut self) -> AbrDecision {
            let q = self.desired_quality.min(self.rungs.len().saturating_sub(1));
            AbrDecision {
                quality_index: q,
                bitrate_bps: self.rungs[q].bitrate_bps,
            }
        }

        fn rung_for_quality_index(&self, quality_index: usize) -> &QualityRung {
            &self.rungs[quality_index.min(self.rungs.len() - 1)]
        }

        fn rung_count(&self) -> usize {
            self.rungs.len()
        }
    }

    #[test]
    fn plan_init_uses_abr_decision() {
        let set = adaptation_set_with_id("v1");
        let mut abr = BolaAbrFactory::default()
            .create(&set, &crate::abr::AbrCreateContext::default())
            .expect("controller");
        abr.update_buffer(10.0);
        let plan = plan_init(abr.as_mut(), 10.0, &QualityConstraints::default(), None);
        assert_eq!(plan.quality_index, abr.decide().quality_index);
    }

    #[test]
    fn plan_segment_marks_init_needed_when_rep_not_cached() {
        let set = adaptation_set_with_id("v1");
        let sets = single_set_map(set);
        let bitstream = no_bitstream();
        let mut abr = Box::new(fixed_abr(0, 0)) as Box<dyn AbrController>;
        let cached = HashMap::new();
        let timeline = timeline_ctx(false);
        let addressing = SegmentAddressing::Template(Default::default());
        let ctx = SegmentPlanContext {
            segment_start_index: 10,
            primary_period_adaptation_index: 0,
            adaptation_sets: &sets,
            bitstream_switching: &bitstream,
            set_availability: SegmentAvailability::for_representation(
                &addressing,
                &SegmentAvailability::default(),
            ),
            timeline_ctx: &timeline,
            cached_inits: &cached,
            last_quality_index: None,
            quality_constraints: QualityConstraints::default(),
            dropped_frames: None,
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
        let sets = single_set_map(set);
        let bitstream = no_bitstream();
        let mut cached = HashMap::new();
        cached.insert((0, "v1".to_string()), Bytes::new());
        let mut abr = Box::new(fixed_abr(0, 0)) as Box<dyn AbrController>;
        let timeline = timeline_ctx(false);
        let addressing = SegmentAddressing::Template(Default::default());
        let ctx = SegmentPlanContext {
            segment_start_index: 0,
            primary_period_adaptation_index: 0,
            adaptation_sets: &sets,
            bitstream_switching: &bitstream,
            set_availability: SegmentAvailability::for_representation(
                &addressing,
                &SegmentAvailability::default(),
            ),
            timeline_ctx: &timeline,
            cached_inits: &cached,
            last_quality_index: None,
            quality_constraints: QualityConstraints::default(),
            dropped_frames: None,
        };
        let plan = plan_segment(abr.as_mut(), 5.0, &segment(1), 0, &ctx);
        assert!(!plan.init_needed);
    }

    #[test]
    fn plan_segment_marks_init_needed_when_switching_representation() {
        let set = AdaptationSet {
            representations: vec![
                Representation {
                    id: Some("v1".into()),
                    bandwidth: Some(500_000),
                    ..Default::default()
                },
                Representation {
                    id: Some("v2".into()),
                    bandwidth: Some(1_000_000),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let sets = single_set_map(set);
        let mut bitstream = HashMap::new();
        bitstream.insert(0, true);
        let mut cached = HashMap::new();
        cached.insert((0, "v1".to_string()), Bytes::from_static(b"init"));
        let mut abr = Box::new(fixed_abr(1, 1)) as Box<dyn AbrController>;
        let timeline = timeline_ctx(false);
        let addressing = SegmentAddressing::Template(Default::default());
        let ctx = SegmentPlanContext {
            segment_start_index: 0,
            primary_period_adaptation_index: 0,
            adaptation_sets: &sets,
            bitstream_switching: &bitstream,
            set_availability: SegmentAvailability::for_representation(
                &addressing,
                &SegmentAvailability::default(),
            ),
            timeline_ctx: &timeline,
            cached_inits: &cached,
            last_quality_index: None,
            quality_constraints: QualityConstraints::default(),
            dropped_frames: None,
        };
        let plan = plan_segment(abr.as_mut(), 5.0, &segment(1), 0, &ctx);
        assert!(plan.init_needed);
        assert_eq!(plan.representation_index, 1);
    }

    #[test]
    fn plan_segment_holds_quality_until_switching_opportunity() {
        let set = AdaptationSet {
            Switching: vec![dash_mpd::Switching {
                interval: Some(4000),
                stype: Some("media".into()),
            }],
            representations: vec![
                Representation {
                    id: Some("lo".into()),
                    bandwidth: Some(100_000),
                    ..Default::default()
                },
                Representation {
                    id: Some("hi".into()),
                    bandwidth: Some(500_000),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let sets = single_set_map(set);
        let bitstream = no_bitstream();
        let mut cached = HashMap::new();
        cached.insert((0, "lo".to_string()), Bytes::from_static(b"init"));
        cached.insert((0, "hi".to_string()), Bytes::from_static(b"init2"));
        let mut abr = Box::new(IndexedAbr {
            desired_quality: 1,
            rungs: vec![
                QualityRung {
                    period_adaptation_index: 0,
                    representation_index: 0,
                    label: "lo".into(),
                    bitrate_bps: 100_000.0,
                    quality_ranking: None,
                    max_playout_rate: None,
                    coding_dependency: None,
                },
                QualityRung {
                    period_adaptation_index: 0,
                    representation_index: 1,
                    label: "hi".into(),
                    bitrate_bps: 500_000.0,
                    quality_ranking: None,
                    max_playout_rate: None,
                    coding_dependency: None,
                },
            ],
        }) as Box<dyn AbrController>;
        let timeline = timeline_ctx(false);
        let addressing = SegmentAddressing::Template(Default::default());
        let ctx = SegmentPlanContext {
            segment_start_index: 0,
            primary_period_adaptation_index: 0,
            adaptation_sets: &sets,
            bitstream_switching: &bitstream,
            set_availability: SegmentAvailability::for_representation(
                &addressing,
                &SegmentAvailability::default(),
            ),
            timeline_ctx: &timeline,
            cached_inits: &cached,
            last_quality_index: Some(0),
            quality_constraints: QualityConstraints::default(),
            dropped_frames: None,
        };
        let held = plan_segment(abr.as_mut(), 5.0, &segment_at(2000), 0, &ctx);
        assert_eq!(held.quality_index, 0);
        let switched = plan_segment(abr.as_mut(), 5.0, &segment_at(4000), 1, &ctx);
        assert_eq!(switched.quality_index, 1);
    }

    #[test]
    fn plan_segment_applies_dropped_frames_cap() {
        use crate::abr::{DroppedFramesHistory, DroppedFramesParams};

        let set = AdaptationSet {
            representations: vec![
                Representation {
                    id: Some("low".into()),
                    bandwidth: Some(100_000),
                    ..Default::default()
                },
                Representation {
                    id: Some("high".into()),
                    bandwidth: Some(500_000),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let sets = single_set_map(set);
        let bitstream = no_bitstream();
        let mut abr = Box::new(IndexedAbr {
            desired_quality: 1,
            rungs: vec![
                QualityRung {
                    period_adaptation_index: 0,
                    representation_index: 0,
                    label: "low".into(),
                    bitrate_bps: 100_000.0,
                    quality_ranking: None,
                    max_playout_rate: None,
                    coding_dependency: None,
                },
                QualityRung {
                    period_adaptation_index: 0,
                    representation_index: 1,
                    label: "high".into(),
                    bitrate_bps: 500_000.0,
                    quality_ranking: None,
                    max_playout_rate: None,
                    coding_dependency: None,
                },
            ],
        }) as Box<dyn AbrController>;
        let cached = HashMap::new();
        let timeline = timeline_ctx(false);
        let addressing = SegmentAddressing::Template(Default::default());
        let history = DroppedFramesHistory::with_params(DroppedFramesParams {
            minimum_sample_size: 10,
            dropped_frames_percentage_threshold: 0.15,
        });
        history.set_active_quality(1);
        history.push(0, 0);
        history.push(40, 100);
        assert_eq!(history.quality_cap(), Some(0));
        let ctx = SegmentPlanContext {
            segment_start_index: 0,
            primary_period_adaptation_index: 0,
            adaptation_sets: &sets,
            bitstream_switching: &bitstream,
            set_availability: SegmentAvailability::for_representation(
                &addressing,
                &SegmentAvailability::default(),
            ),
            timeline_ctx: &timeline,
            cached_inits: &cached,
            last_quality_index: None,
            quality_constraints: QualityConstraints::default(),
            dropped_frames: Some(&history),
        };
        let plan = plan_segment(abr.as_mut(), 20.0, &segment(1), 0, &ctx);
        assert_eq!(plan.quality_index, 0);
        assert_eq!(plan.representation_index, 0);
    }

    fn segment_at(time: u64) -> TimelineSegment {
        TimelineSegment {
            number: 1,
            time,
            duration: 2000,
            duration_s: 2.0,
            presentation_time_s: time as f64 / 1000.0,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        }
    }
}
