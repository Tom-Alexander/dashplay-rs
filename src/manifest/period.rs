use std::time::Duration;

use chrono::{DateTime, Utc};
use dash_mpd::MPD;

use crate::PlayerError;

use super::types::PeriodWindow;

pub(crate) fn mpd(manifest: &Option<MPD>) -> Result<&MPD, PlayerError> {
    manifest.as_ref().ok_or(PlayerError::ManifestNotLoaded)
}

/// Elapsed time since `MPD@availabilityStartTime` using a wall clock (from [`super::utc_timing`] or local UTC).
pub(crate) fn since_availability_start_at(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
) -> Result<Option<Duration>, PlayerError> {
    let Some(ast) = mpd.availabilityStartTime else {
        return Ok(None);
    };

    let since_ast: Duration = wall_now
        .signed_duration_since(ast)
        .to_std()
        .unwrap_or(Duration::ZERO);

    Ok(Some(since_ast))
}

pub(crate) fn period_windows(mpd: &MPD) -> Result<Vec<PeriodWindow>, PlayerError> {
    if mpd.periods.is_empty() {
        return Err(PlayerError::NoPeriod);
    }

    let mut acc_start = Duration::ZERO;
    let mut windows = Vec::with_capacity(mpd.periods.len());

    for (idx, period) in mpd.periods.iter().enumerate() {
        let start = period.start.unwrap_or(acc_start);

        let end = if let Some(d) = period.duration {
            Some(start.saturating_add(d))
        } else {
            // If the next period has an explicit start, infer this one's end.
            mpd.periods.get(idx + 1).and_then(|p| p.start)
        };

        // Advance accumulated start time if we can.
        if let Some(e) = end {
            acc_start = e;
        }

        windows.push(PeriodWindow { idx, start, end });
    }

    Ok(windows)
}

pub(crate) fn is_dynamic_mpd(mpd: &MPD) -> bool {
    mpd.mpdtype.as_deref() == Some("dynamic")
}

pub(crate) fn current_period_window_at(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
) -> Result<PeriodWindow, PlayerError> {
    let windows = period_windows(mpd)?;

    // Static VOD has no availability timeline; playback starts at the first Period.
    let Some(since_ast) = since_availability_start_at(mpd, wall_now)? else {
        return Ok(windows[0]);
    };

    for w in windows {
        let in_range = since_ast >= w.start && w.end.is_none_or(|e| since_ast < e);
        if in_range {
            return Ok(w);
        }
    }

    Ok(PeriodWindow {
        idx: mpd.periods.len().saturating_sub(1),
        start: mpd
            .periods
            .last()
            .and_then(|p| p.start)
            .unwrap_or(Duration::ZERO),
        end: None,
    })
}
