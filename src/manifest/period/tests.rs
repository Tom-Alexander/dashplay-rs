use std::time::Duration;

use chrono::{TimeZone, Utc};
use dash_mpd::{MPD, Period};

use super::{current_period_window_at, period_windows};

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
