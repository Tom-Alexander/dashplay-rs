//! Live DVR window and period selection for backward seeks (dash.js: `getDvrWindow`).

use std::time::Duration;

use chrono::{DateTime, Utc};
use dash_mpd::MPD;

use super::ManifestError;
use super::availability::target_presentation_time_from_since;
use super::period::{
    current_period_window_at, is_dynamic_mpd, period_windows, since_availability_start_at,
};
use super::period_connectivity::period_link;
use super::types::PeriodWindow;

/// Seekable presentation-time range for a dynamic live MPD (dash.js: `getDvrWindow`).
///
/// `start` / `end` are seconds from `availabilityStartTime` on the media presentation timeline.
/// For static MPDs, spans the full resolved presentation when duration is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvrWindow {
    pub start: Duration,
    pub end: Duration,
}

/// Default `timeShiftBufferDepth` when absent on a dynamic MPD (ISO 23009-1 default).
const DEFAULT_TIME_SHIFT_BUFFER_DEPTH: Duration = Duration::from_secs(120);

/// Compute the DVR seek window at `wall_now`.
pub(crate) fn dvr_window_at(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
) -> Result<Option<DvrWindow>, ManifestError> {
    if !is_dynamic_mpd(mpd) {
        let windows = period_windows(mpd)?;
        let start = windows.first().map(|w| w.start).unwrap_or(Duration::ZERO);
        let end = mpd
            .mediaPresentationDuration
            .or_else(|| windows.last().and_then(|w| w.end))
            .filter(|d| *d > start);
        return Ok(end.map(|end| DvrWindow { start, end }));
    }

    let Some(since_ast) = since_availability_start_at(mpd, wall_now)? else {
        return Ok(None);
    };

    let tsbd = mpd
        .timeShiftBufferDepth
        .filter(|d| !d.is_zero())
        .unwrap_or(DEFAULT_TIME_SHIFT_BUFFER_DEPTH);

    let windows = period_windows(mpd)?;
    let first_start = windows.first().map(|w| w.start).unwrap_or(Duration::ZERO);

    let mut end = target_presentation_time_from_since(mpd, since_ast);
    if let Some(mpd_end) = mpd.mediaPresentationDuration.filter(|d| !d.is_zero()) {
        end = end.min(mpd_end);
    }

    let mut start = since_ast.saturating_sub(tsbd).max(first_start);
    if let Some(last_end) = windows.last().and_then(|w| w.end) {
        start = start.min(last_end);
    }

    if end <= start {
        return Ok(None);
    }

    Ok(Some(DvrWindow { start, end }))
}

/// Period window containing `presentation_time`, if any.
pub(crate) fn period_at_presentation_time(
    windows: &[PeriodWindow],
    presentation_time: Duration,
) -> Option<PeriodWindow> {
    windows
        .iter()
        .copied()
        .find(|w| presentation_time >= w.start && w.end.is_none_or(|e| presentation_time < e))
}

/// Dynamic periods to fetch for the current manifest tick.
///
/// Without a seek target, returns the current Period (and optionally the next when near a soft
/// transition). With a backward seek, includes every Period from the seek target through the
/// current live Period so DVR rewind can cross a sliding multi-period window.
pub(crate) fn live_periods_for_playback(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
    sync_depth_s: f64,
    seek_target: Option<Duration>,
) -> Result<Vec<PeriodWindow>, ManifestError> {
    let period_windows = period_windows(mpd)?;
    let current = current_period_window_at(mpd, wall_now)?;

    let first_idx = if let Some(seek) = seek_target {
        period_at_presentation_time(&period_windows, seek)
            .map(|w| w.idx)
            .unwrap_or(current.idx)
    } else {
        current.idx
    };

    let mut out: Vec<PeriodWindow> = period_windows
        .iter()
        .copied()
        .filter(|w| w.idx >= first_idx && w.idx <= current.idx)
        .collect();

    if out.is_empty() {
        out.push(current);
    }

    append_upcoming_period_if_near_transition(mpd, wall_now, sync_depth_s, &current, &mut out)?;

    Ok(out)
}

fn append_upcoming_period_if_near_transition(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
    sync_depth_s: f64,
    current: &PeriodWindow,
    out: &mut Vec<PeriodWindow>,
) -> Result<(), ManifestError> {
    let Some(since_ast) = since_availability_start_at(mpd, wall_now)? else {
        return Ok(());
    };
    let since_s = since_ast.as_secs_f64();
    let period_windows = period_windows(mpd)?;
    let Some(next) = period_windows.iter().find(|w| w.idx == current.idx + 1) else {
        return Ok(());
    };
    let link = period_link(mpd, current.idx, next.idx);
    let near_next = since_s + 1e-9 >= next.start.as_secs_f64() - sync_depth_s.max(0.0);
    if link.allows_soft_transition() && near_next && !out.iter().any(|w| w.idx == next.idx) {
        out.push(*next);
    }
    Ok(())
}

/// Presentation time to use when starting an adaptation stream in `window` after a seek.
pub(crate) fn period_seek_target(window: PeriodWindow, seek: Duration) -> Option<Duration> {
    if seek < window.start {
        return None;
    }
    if window.end.is_some_and(|e| seek >= e) {
        return None;
    }
    Some(seek)
}

/// Whether this Period lies entirely before `seek` and should be skipped.
pub(crate) fn period_is_before_seek(window: PeriodWindow, seek: Duration) -> bool {
    window.end.is_some_and(|e| e <= seek)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use dash_mpd::MPD;

    #[test]
    fn dvr_window_uses_time_shift_buffer_and_suggested_delay() {
        let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        let mpd = MPD {
            mpdtype: Some("dynamic".to_string()),
            availabilityStartTime: Some(ast),
            timeShiftBufferDepth: Some(Duration::from_secs(20)),
            suggestedPresentationDelay: Some(Duration::from_secs(4)),
            periods: vec![dash_mpd::Period::default()],
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 20).unwrap();
        let window = dvr_window_at(&mpd, now).unwrap().expect("dvr");
        assert_eq!(window.start, Duration::ZERO);
        assert_eq!(window.end, Duration::from_secs(16));
    }

    #[test]
    fn live_periods_for_backward_seek_includes_earlier_period() {
        let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        let mpd = MPD {
            mpdtype: Some("dynamic".to_string()),
            availabilityStartTime: Some(ast),
            minimumUpdatePeriod: Some(Duration::from_millis(500)),
            periods: vec![
                dash_mpd::Period {
                    duration: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
                dash_mpd::Period {
                    start: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 12).unwrap();
        let windows =
            live_periods_for_playback(&mpd, now, 2.0, Some(Duration::from_secs(4))).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].idx, 0);
        assert_eq!(windows[1].idx, 1);
    }

    #[test]
    fn period_seek_target_only_when_seek_falls_in_period() {
        let p1 = PeriodWindow {
            idx: 0,
            start: Duration::ZERO,
            end: Some(Duration::from_secs(10)),
        };
        let p2 = PeriodWindow {
            idx: 1,
            start: Duration::from_secs(10),
            end: None,
        };
        assert_eq!(
            period_seek_target(p1, Duration::from_secs(4)),
            Some(Duration::from_secs(4))
        );
        assert_eq!(period_seek_target(p2, Duration::from_secs(4)), None);
        assert!(period_is_before_seek(p1, Duration::from_secs(12)));
    }
}
