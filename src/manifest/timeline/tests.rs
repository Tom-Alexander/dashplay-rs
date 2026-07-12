use std::time::Duration;

use dash_mpd::{AdaptationSet, Representation, S, SegmentTemplate, SegmentTimeline};

use crate::PlayerError;
use crate::manifest::{
    PeriodWindow, TemplateVars, TimelineBuildContext, TimelineSegment, align_start_index_to_resync,
    align_start_index_to_sap, interpolate_template, mid_segment_resync_alignment,
    template_vars_for_representation, timeline_segments,
};
use crate::resync::ResyncHints;

fn static_ctx(period_end: Option<Duration>) -> TimelineBuildContext {
    TimelineBuildContext {
        is_dynamic: false,
        period_window: PeriodWindow {
            idx: 0,
            start: Duration::ZERO,
            end: period_end,
        },
        period_duration: None,
        media_presentation_duration: None,
        time_shift_buffer_depth: None,
        since_availability_start: None,
        resync_hints: None,
    }
}

#[test]
fn align_start_index_to_resync_snaps_to_grid() {
    let segments = vec![
        TimelineSegment {
            number: 1,
            presentation_time_s: 0.0,
            duration_s: 2.0,
            ..default_timeline_segment()
        },
        TimelineSegment {
            number: 2,
            presentation_time_s: 2.0,
            duration_s: 2.0,
            ..default_timeline_segment()
        },
        TimelineSegment {
            number: 3,
            presentation_time_s: 5.0,
            duration_s: 2.0,
            ..default_timeline_segment()
        },
    ];
    let hints = ResyncHints {
        chunk_duration_s: None,
        random_access_interval_s: Some(2.0),
        random_access_markers: false,
        random_access_within_segment: false,
    };
    assert_eq!(align_start_index_to_resync(&segments, 2, hints), 1);
    assert_eq!(align_start_index_to_resync(&segments, 1, hints), 1);
}

#[test]
fn mid_segment_resync_alignment_snaps_to_in_segment_grid() {
    let segments = vec![
        TimelineSegment {
            number: 1,
            presentation_time_s: 0.0,
            duration_s: 4.0,
            ..default_timeline_segment()
        },
        TimelineSegment {
            number: 2,
            presentation_time_s: 4.0,
            duration_s: 4.0,
            ..default_timeline_segment()
        },
    ];
    let hints = ResyncHints {
        chunk_duration_s: None,
        random_access_interval_s: Some(0.5),
        random_access_markers: false,
        random_access_within_segment: true,
    };
    let (idx, chunk) = mid_segment_resync_alignment(&segments, 1, 5.2, hints);
    assert_eq!(idx, 1);
    assert_eq!(chunk, Some(3)); // 4.0 + 2*0.5 = 5.0s resync point → chunk 3
}

#[test]
fn mid_segment_resync_alignment_at_segment_start_uses_first_chunk() {
    let segments = vec![TimelineSegment {
        number: 1,
        presentation_time_s: 4.0,
        duration_s: 4.0,
        ..default_timeline_segment()
    }];
    let hints = ResyncHints {
        chunk_duration_s: None,
        random_access_interval_s: Some(0.5),
        random_access_markers: false,
        random_access_within_segment: true,
    };
    let (idx, chunk) = mid_segment_resync_alignment(&segments, 0, 4.0, hints);
    assert_eq!(idx, 0);
    assert_eq!(chunk, Some(1));
}

fn default_timeline_segment() -> TimelineSegment {
    TimelineSegment {
        number: 0,
        time: 0,
        duration: 0,
        duration_s: 0.0,
        presentation_time_s: 0.0,
        sub_number: None,
        resync_start_chunk: None,
        media_url: None,
        media_range: None,
    }
}

#[test]
fn segment_timeline_implicit_t_chains_previous_s_end() {
    let st = SegmentTemplate {
        timescale: Some(1000),
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(SegmentTimeline {
            segments: vec![
                S {
                    t: Some(0),
                    d: 2000,
                    r: Some(0),
                    ..Default::default()
                },
                S {
                    t: None,
                    d: 1000,
                    r: Some(1),
                    ..Default::default()
                },
            ],
        }),
        ..Default::default()
    };
    let segs = timeline_segments(&st, &static_ctx(Some(Duration::from_secs(10))), None).unwrap();
    assert_eq!(segs.len(), 3);
    assert_eq!(segs[0].time, 0);
    assert_eq!(segs[1].time, 2000);
    assert_eq!(segs[2].time, 3000);
}

#[test]
fn segment_timeline_s_n_sets_first_segment_number() {
    let st = SegmentTemplate {
        timescale: Some(1),
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(SegmentTimeline {
            segments: vec![S {
                t: Some(10),
                d: 1,
                r: Some(0),
                n: Some(99),
                ..Default::default()
            }],
        }),
        ..Default::default()
    };
    let segs = timeline_segments(&st, &static_ctx(None), None).unwrap();
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].number, 99);
    assert_eq!(segs[0].time, 10);
}

#[test]
fn segment_timeline_negative_r_stops_before_next_s_t() {
    let st = SegmentTemplate {
        timescale: Some(1000),
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(SegmentTimeline {
            segments: vec![
                S {
                    t: Some(0),
                    d: 500,
                    r: Some(-1),
                    ..Default::default()
                },
                S {
                    t: Some(2000),
                    d: 100,
                    r: Some(0),
                    ..Default::default()
                },
            ],
        }),
        ..Default::default()
    };
    let segs = timeline_segments(&st, &static_ctx(None), None).unwrap();
    assert_eq!(segs.len(), 5);
    assert_eq!(
        segs.iter().map(|s| s.time).collect::<Vec<_>>(),
        vec![0, 500, 1000, 1500, 2000]
    );
}

#[test]
fn segment_timeline_negative_r_unbounded_errors() {
    let st = SegmentTemplate {
        timescale: Some(1),
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(SegmentTimeline {
            segments: vec![S {
                t: Some(0),
                d: 1,
                r: Some(-1),
                ..Default::default()
            }],
        }),
        ..Default::default()
    };
    let ctx = static_ctx(None);
    let err = timeline_segments(&st, &ctx, None).unwrap_err();
    assert!(matches!(err, PlayerError::UnboundedSegmentTimelineRepeat));
}

#[test]
fn dynamic_segment_timeline_filtered_to_time_shift_buffer() {
    let st = SegmentTemplate {
        timescale: Some(1000),
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(SegmentTimeline {
            segments: vec![
                S {
                    t: Some(1000),
                    d: 1000,
                    r: Some(9),
                    ..Default::default()
                }, // 1s..10s
            ],
        }),
        ..Default::default()
    };
    let ctx = TimelineBuildContext {
        is_dynamic: true,
        period_window: PeriodWindow {
            idx: 0,
            start: Duration::ZERO,
            end: None,
        },
        period_duration: None,
        media_presentation_duration: None,
        time_shift_buffer_depth: Some(Duration::from_secs(2)),
        since_availability_start: Some(Duration::from_secs(5)),
        resync_hints: None,
    };
    let segs = timeline_segments(&st, &ctx, None).unwrap();
    assert_eq!(segs.len(), 3);
    assert_eq!(
        segs.iter().map(|s| s.time).collect::<Vec<_>>(),
        vec![3000, 4000, 5000]
    );
}

#[test]
fn segment_timeline_k_sequence_subnumbers_and_shared_time() {
    let st = SegmentTemplate {
        timescale: Some(1000),
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(SegmentTimeline {
            segments: vec![S {
                t: Some(0),
                d: 3000,
                r: Some(0),
                k: Some(3),
                ..Default::default()
            }],
        }),
        ..Default::default()
    };
    let segs = timeline_segments(&st, &static_ctx(None), None).unwrap();
    assert_eq!(segs.len(), 3);
    assert_eq!(segs[0].number, 1);
    assert_eq!(segs[1].number, 1);
    assert_eq!(segs[2].number, 1);
    assert_eq!(segs[0].time, 0);
    assert_eq!(segs[1].time, 0);
    assert_eq!(segs[2].time, 0);
    assert_eq!(
        segs.iter().map(|s| s.sub_number).collect::<Vec<_>>(),
        vec![Some(1), Some(2), Some(3)]
    );
    assert_eq!(segs[0].duration, 1000);
    assert!((segs[0].presentation_time_s - 0.0).abs() < 1e-9);
    assert!((segs[1].presentation_time_s - 1.0).abs() < 1e-9);
    assert!((segs[2].presentation_time_s - 2.0).abs() < 1e-9);
}

#[test]
fn segment_timeline_k_with_repeat_r_counts_sequences() {
    let st = SegmentTemplate {
        timescale: Some(1000),
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(SegmentTimeline {
            segments: vec![S {
                t: Some(0),
                d: 4000,
                r: Some(1),
                k: Some(2),
                ..Default::default()
            }],
        }),
        ..Default::default()
    };
    let segs = timeline_segments(&st, &static_ctx(None), None).unwrap();
    assert_eq!(segs.len(), 4);
    assert_eq!(
        segs.iter().map(|s| s.number).collect::<Vec<_>>(),
        vec![1, 1, 2, 2]
    );
    assert_eq!(segs[2].time, 4000);
    assert_eq!(segs[2].presentation_time_s, 4.0);
}

#[test]
fn segment_timeline_k_must_divide_d() {
    let st = SegmentTemplate {
        timescale: Some(1),
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(SegmentTimeline {
            segments: vec![S {
                t: Some(0),
                d: 10,
                r: Some(0),
                k: Some(3),
                ..Default::default()
            }],
        }),
        ..Default::default()
    };
    let err = timeline_segments(&st, &static_ctx(None), None).unwrap_err();
    assert!(matches!(err, PlayerError::TimelineDNotDivisibleByK));
}

#[test]
fn interpolate_template_subnumber() {
    let vars = TemplateVars {
        representation_id: "A",
        number: Some(7),
        time: Some(42),
        sub_number: Some(3),
        ..Default::default()
    };
    let out = interpolate_template(
        "v-$RepresentationID$-$Number$-$Time$-$SubNumber$.m4s",
        &vars,
    );
    assert_eq!(out, "v-A-7-42-3.m4s");
}

#[test]
fn interpolate_template_subnumber_defaults_to_one() {
    let vars = TemplateVars {
        representation_id: "id",
        ..Default::default()
    };
    let out = interpolate_template("x-$SubNumber$.m4s", &vars);
    assert_eq!(out, "x-1.m4s");
}

#[test]
fn interpolate_template_number_and_time_format_width() {
    let vars = TemplateVars {
        representation_id: "1",
        bandwidth: Some(1_100_000),
        number: Some(7),
        time: Some(42),
        ..Default::default()
    };
    let out = interpolate_template(
        "chunk-$RepresentationID$-$Number%05d$-$Time%010d$-$Bandwidth%07d$.m4s",
        &vars,
    );
    assert_eq!(out, "chunk-1-00007-0000000042-1100000.m4s");
}

#[test]
fn interpolate_template_bandwidth_without_format() {
    let vars = TemplateVars {
        representation_id: "v",
        bandwidth: Some(500_000),
        ..Default::default()
    };
    let out = interpolate_template("seg-$Bandwidth$.m4s", &vars);
    assert_eq!(out, "seg-500000.m4s");
}

#[test]
fn interpolate_template_dollar_escape() {
    let vars = TemplateVars {
        representation_id: "id",
        number: Some(1),
        ..Default::default()
    };
    let out = interpolate_template("pre$$-$Number$-post", &vars);
    assert_eq!(out, "pre$-1-post");
}

#[test]
fn interpolate_template_leaves_missing_number_unsubstituted() {
    let vars = TemplateVars {
        representation_id: "id",
        ..Default::default()
    };
    let out = interpolate_template("seg-$Number%05d$.m4s", &vars);
    assert_eq!(out, "seg-$Number%05d$.m4s");
}

#[test]
fn interpolate_template_width_height_frame_rate_and_ext() {
    let vars = TemplateVars {
        width: Some(1280),
        height: Some(720),
        frame_rate: Some("30000/1001"),
        ext: Some("m4s"),
        number: Some(3),
        ..Default::default()
    };
    let out = interpolate_template("seg-$Width$x$Height$-$FrameRate$-$Number$.$Ext$", &vars);
    assert_eq!(out, "seg-1280x720-30000/1001-3.m4s");
}

#[test]
fn interpolate_template_width_height_format_width() {
    let vars = TemplateVars {
        width: Some(640),
        height: Some(360),
        ..Default::default()
    };
    let out = interpolate_template("v$Width%04d$x$Height%03d$.mp4", &vars);
    assert_eq!(out, "v0640x360.mp4");
}

#[test]
fn interpolate_template_initialization() {
    let vars = TemplateVars {
        initialization: Some("init-640x360.mp4"),
        number: Some(2),
        ext: Some("m4s"),
        ..Default::default()
    };
    let out = interpolate_template("$Initialization$-chunk-$Number$.$Ext$", &vars);
    assert_eq!(out, "init-640x360.mp4-chunk-2.m4s");
}

#[test]
fn interpolate_template_rejects_format_tag_on_string_identifiers() {
    let vars = TemplateVars {
        frame_rate: Some("24"),
        ext: Some("m4s"),
        ..Default::default()
    };
    assert_eq!(
        interpolate_template("x-$FrameRate%02d$.m4s", &vars),
        "x-$FrameRate%02d$.m4s"
    );
    assert_eq!(
        interpolate_template("x-$Ext%02d$.m4s", &vars),
        "x-$Ext%02d$.m4s"
    );
}

#[test]
fn template_vars_for_representation_inherits_adaptation_set_dimensions() {
    let adaptation_set = AdaptationSet {
        width: Some(1920),
        height: Some(1080),
        frameRate: Some("25".into()),
        mimeType: Some("video/mp4".into()),
        ..Default::default()
    };
    let rep = Representation {
        bandwidth: Some(1_000_000),
        ..Default::default()
    };
    let vars = template_vars_for_representation(&rep, &adaptation_set);
    assert_eq!(vars.width, Some(1920));
    assert_eq!(vars.height, Some(1080));
    assert_eq!(vars.frame_rate, Some("25"));
    assert_eq!(vars.ext, Some("m4s"));
}

#[test]
fn align_start_index_rewinds_to_first_subsegment_without_subsegment_sap() {
    let aset = AdaptationSet {
        startWithSAP: Some(1),
        ..Default::default()
    };
    let segs = vec![
        TimelineSegment {
            number: 1,
            time: 0,
            duration: 1000,
            duration_s: 1.0,
            presentation_time_s: 0.0,
            sub_number: Some(1),
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        },
        TimelineSegment {
            number: 1,
            time: 0,
            duration: 1000,
            duration_s: 1.0,
            presentation_time_s: 1.0,
            sub_number: Some(2),
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        },
        TimelineSegment {
            number: 1,
            time: 0,
            duration: 1000,
            duration_s: 1.0,
            presentation_time_s: 2.0,
            sub_number: Some(3),
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        },
    ];
    assert_eq!(align_start_index_to_sap(&segs, 2, &aset), 0);
}

#[test]
fn align_start_index_keeps_interior_subsegment_when_subsegment_starts_with_sap() {
    let aset = AdaptationSet {
        startWithSAP: Some(1),
        subsegmentStartsWithSAP: Some(1),
        ..Default::default()
    };
    let segs = vec![
        TimelineSegment {
            number: 1,
            time: 0,
            duration: 1000,
            duration_s: 1.0,
            presentation_time_s: 0.0,
            sub_number: Some(1),
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        },
        TimelineSegment {
            number: 1,
            time: 0,
            duration: 1000,
            duration_s: 1.0,
            presentation_time_s: 1.0,
            sub_number: Some(2),
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        },
    ];
    assert_eq!(align_start_index_to_sap(&segs, 1, &aset), 1);
}
