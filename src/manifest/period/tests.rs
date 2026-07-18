use std::time::Duration;

use chrono::{TimeZone, Utc};
use dash_mpd::{MPD, Period};

use super::{current_period_window_at, period_windows, presentation_gap_before};

#[test]
fn period_windows_chain_period_starts() {
    let mpd = MPD {
        periods: vec![
            Period {
                duration: Some(Duration::from_secs(10)),
                ..Default::default()
            },
            Period {
                start: Some(Duration::from_secs(10)),
                duration: Some(Duration::from_secs(5)),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let windows = period_windows(&mpd).unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].start, Duration::ZERO);
    assert_eq!(windows[0].end, Some(Duration::from_secs(10)));
    assert_eq!(windows[1].start, Duration::from_secs(10));
    assert_eq!(windows[1].end, Some(Duration::from_secs(15)));
    assert_eq!(
        presentation_gap_before(windows[0].end, windows[1].start),
        None
    );
}

#[test]
fn period_windows_preserve_explicit_timeline_gap() {
    let mpd = MPD {
        mediaPresentationDuration: Some(Duration::from_secs(20)),
        periods: vec![
            Period {
                duration: Some(Duration::from_secs(8)),
                ..Default::default()
            },
            Period {
                start: Some(Duration::from_secs(12)),
                duration: Some(Duration::from_secs(8)),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let windows = period_windows(&mpd).unwrap();
    assert_eq!(windows[0].end, Some(Duration::from_secs(8)));
    assert_eq!(windows[1].start, Duration::from_secs(12));
    assert_eq!(
        presentation_gap_before(windows[0].end, windows[1].start),
        Some(Duration::from_secs(4))
    );
    assert_eq!(
        super::gap_before_period(&mpd, 1),
        Some(Duration::from_secs(4))
    );
    assert_eq!(super::gap_before_period(&mpd, 0), None);
}

#[test]
fn presentation_gap_before_ignores_overlap() {
    assert_eq!(
        presentation_gap_before(Some(Duration::from_secs(10)), Duration::from_secs(8)),
        None
    );
    assert_eq!(
        presentation_gap_before(Some(Duration::from_secs(8)), Duration::from_secs(8)),
        None
    );
}

#[test]
fn current_period_window_static_mpd_starts_at_first_period() {
    let mpd = MPD {
        mpdtype: Some("static".into()),
        periods: vec![
            Period {
                duration: Some(Duration::from_secs(10)),
                ..Default::default()
            },
            Period {
                start: Some(Duration::from_secs(10)),
                duration: Some(Duration::from_secs(5)),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
    assert_eq!(current_period_window_at(&mpd, now).unwrap().idx, 0);
}

#[test]
fn current_period_window_selects_by_availability_time() {
    let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
    let mpd = MPD {
        availabilityStartTime: Some(ast),
        periods: vec![
            Period {
                duration: Some(Duration::from_secs(10)),
                ..Default::default()
            },
            Period {
                start: Some(Duration::from_secs(10)),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let in_first = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 5).unwrap();
    assert_eq!(current_period_window_at(&mpd, in_first).unwrap().idx, 0);

    let in_second = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 12).unwrap();
    assert_eq!(current_period_window_at(&mpd, in_second).unwrap().idx, 1);
}

#[test]
fn period_windows_apply_media_presentation_duration_to_last_period() {
    let mpd = MPD {
        mediaPresentationDuration: Some(Duration::from_secs(16)),
        periods: vec![Period {
            id: Some("p0".into()),
            ..Default::default()
        }],
        ..Default::default()
    };

    let windows = period_windows(&mpd).unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].start, Duration::ZERO);
    assert_eq!(windows[0].end, Some(Duration::from_secs(16)));
}

#[test]
fn current_period_window_past_presentation_end_keeps_last_period_extent() {
    let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
    let mpd = MPD {
        mpdtype: Some("dynamic".into()),
        availabilityStartTime: Some(ast),
        mediaPresentationDuration: Some(Duration::from_secs(16)),
        periods: vec![Period {
            id: Some("p0".into()),
            ..Default::default()
        }],
        ..Default::default()
    };

    let after_end = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 20).unwrap();
    let window = current_period_window_at(&mpd, after_end).unwrap();
    assert_eq!(window.idx, 0);
    assert_eq!(window.end, Some(Duration::from_secs(16)));
}
