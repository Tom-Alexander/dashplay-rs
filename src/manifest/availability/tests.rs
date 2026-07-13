use std::time::Duration;

use chrono::{TimeZone, Utc};
use dash_mpd::{Latency, MPD, SegmentTemplate, ServiceDescription};

use crate::manifest::{
    PeriodWindow, SegmentAddressing, SegmentAvailability, TimelineBuildContext, TimelineSegment,
    filter_segments_by_availability, segment_is_available, segment_sequence_start_s,
    target_presentation_time_at, timeline_segments,
};

#[test]
fn target_presentation_time_applies_suggested_delay() {
    let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
    let mpd = MPD {
        availabilityStartTime: Some(ast),
        suggestedPresentationDelay: Some(Duration::from_secs(2)),
        ..Default::default()
    };
    let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 10).unwrap();
    assert_eq!(
        target_presentation_time_at(&mpd, now).unwrap(),
        Some(Duration::from_secs(8))
    );
}

#[test]
fn target_presentation_time_prefers_service_description_latency() {
    let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
    let mpd = MPD {
        availabilityStartTime: Some(ast),
        suggestedPresentationDelay: Some(Duration::from_secs(2)),
        ServiceDescription: vec![ServiceDescription {
            Latency: vec![Latency {
                target: Some(3500.0),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 10).unwrap();
    assert_eq!(
        target_presentation_time_at(&mpd, now).unwrap(),
        Some(Duration::from_secs_f64(6.5))
    );
}

#[test]
fn segment_availability_waits_for_availability_time_offset_when_complete() {
    let seg = TimelineSegment {
        number: 3,
        time: 8000,
        duration: 4000,
        duration_s: 4.0,
        presentation_time_s: 8.0,
        sub_number: None,
        resync_start_chunk: None,
        media_url: None,
        media_range: None,
    };
    let availability = SegmentAvailability {
        availability_time_offset_s: Some(7.0),
        availability_time_complete: true,
    };
    assert!(!segment_is_available(
        &seg,
        Duration::ZERO,
        Duration::from_secs(14),
        &availability,
    ));
    assert!(segment_is_available(
        &seg,
        Duration::ZERO,
        Duration::from_secs(15),
        &availability,
    ));
}

#[test]
fn segment_availability_partial_whole_segment_available_at_sap() {
    let seg = TimelineSegment {
        number: 3,
        time: 8000,
        duration: 4000,
        duration_s: 4.0,
        presentation_time_s: 8.0,
        sub_number: None,
        resync_start_chunk: None,
        media_url: None,
        media_range: None,
    };
    let availability = SegmentAvailability {
        availability_time_offset_s: Some(7.0),
        availability_time_complete: false,
    };
    assert!(!segment_is_available(
        &seg,
        Duration::ZERO,
        Duration::from_secs(7),
        &availability,
    ));
    assert!(segment_is_available(
        &seg,
        Duration::ZERO,
        Duration::from_secs(8),
        &availability,
    ));
}

#[test]
fn segment_availability_uses_sequence_start_for_subsegments() {
    let seg = TimelineSegment {
        number: 1,
        time: 0,
        duration: 1000,
        duration_s: 1.0,
        presentation_time_s: 2.0,
        sub_number: Some(3),
        resync_start_chunk: None,
        media_url: None,
        media_range: None,
    };
    assert!((segment_sequence_start_s(Duration::ZERO, &seg) - 0.0).abs() < 1e-6);
    let availability = SegmentAvailability {
        availability_time_offset_s: Some(5.0),
        availability_time_complete: true,
    };
    assert!(!segment_is_available(
        &seg,
        Duration::ZERO,
        Duration::from_secs(4),
        &availability,
    ));
    assert!(segment_is_available(
        &seg,
        Duration::ZERO,
        Duration::from_secs(5),
        &availability,
    ));
}

#[test]
fn filter_segments_by_availability_drops_unpublished_complete_live_edge() {
    let st = SegmentTemplate {
        timescale: Some(1000),
        duration: Some(4000.0),
        startNumber: Some(1),
        availabilityTimeOffset: Some(7.0),
        availabilityTimeComplete: Some(true),
        ..Default::default()
    };
    let addressing = SegmentAddressing::Template(st.clone());
    let ctx = TimelineBuildContext {
        is_dynamic: true,
        period_window: PeriodWindow {
            idx: 0,
            start: Duration::ZERO,
            end: None,
        },
        period_duration: None,
        media_presentation_duration: None,
        max_segment_duration: None,
        time_shift_buffer_depth: Some(Duration::from_secs(20)),
        since_availability_start: Some(Duration::from_secs(12)),
        resync_hints: None,
    };
    let segments = timeline_segments(&st, &ctx, None).unwrap();
    let filtered = filter_segments_by_availability(
        segments,
        true,
        Duration::ZERO,
        ctx.since_availability_start,
        &addressing,
    );
    let numbers: Vec<_> = filtered.iter().map(|s| s.number).collect();
    assert_eq!(numbers, vec![1, 2]);
}

#[test]
fn filter_segments_by_availability_includes_partial_live_edge_at_sap() {
    let st = SegmentTemplate {
        timescale: Some(1000),
        duration: Some(4000.0),
        startNumber: Some(1),
        availabilityTimeOffset: Some(7.0),
        availabilityTimeComplete: Some(false),
        ..Default::default()
    };
    let addressing = SegmentAddressing::Template(st.clone());
    let ctx = TimelineBuildContext {
        is_dynamic: true,
        period_window: PeriodWindow {
            idx: 0,
            start: Duration::ZERO,
            end: None,
        },
        period_duration: None,
        media_presentation_duration: None,
        max_segment_duration: None,
        time_shift_buffer_depth: Some(Duration::from_secs(20)),
        since_availability_start: Some(Duration::from_secs(11)),
        resync_hints: None,
    };
    let segments = timeline_segments(&st, &ctx, None).unwrap();
    let filtered = filter_segments_by_availability(
        segments,
        true,
        Duration::ZERO,
        ctx.since_availability_start,
        &addressing,
    );
    let numbers: Vec<_> = filtered.iter().map(|s| s.number).collect();
    assert_eq!(numbers, vec![1, 2, 3]);
}
