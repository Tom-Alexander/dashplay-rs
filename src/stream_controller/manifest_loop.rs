//! Manifest refresh tick: fetch, steering sync, and period window selection.

use std::time::Duration;

use chrono::{DateTime, Utc};
use dash_mpd::MPD;
use url::Url;

use super::super::PlayerError;
use super::super::clock::utc_timing;
use super::super::http::SharedHttpClient;
use super::super::manifest::{self, SegmentTemplateEndNumbers};
use super::super::manifest_lifecycle::{ContentSteeringState, ManifestSession, SteeringSyncHints};
use super::super::types::{PlayerEvent, PlayerTrack};

/// Parsed manifest state produced by one refresh cycle.
pub(crate) struct ManifestTick<'a> {
    pub mpd: &'a MPD,
    pub xml: &'a str,
    pub active_manifest_uri: Url,
    pub steering: &'a ContentSteeringState,
    pub min_update_period: Duration,
    pub is_dynamic: bool,
    pub wall_now: DateTime<Utc>,
    pub template_end_numbers: Option<SegmentTemplateEndNumbers>,
    pub random_access: Option<crate::manifest::RandomAccessSupplements>,
}

pub(crate) async fn refresh_manifest(
    session: &mut ManifestSession,
    client: &SharedHttpClient,
    manifest_uri: &Url,
    cmcd: Option<&crate::cmcd::CmcdSession>,
    http_retry: &crate::http::HttpRetryConfig,
    steering_hints: &SteeringSyncHints,
) -> Result<(), PlayerError> {
    session
        .refresh(client, manifest_uri, cmcd, http_retry)
        .await?;
    session.sync_steering(client, steering_hints).await
}

pub(crate) async fn manifest_tick<'a>(
    session: &'a ManifestSession,
    client: &SharedHttpClient,
) -> Result<ManifestTick<'a>, PlayerError> {
    let mpd = session.parsed()?;
    let xml = session.xml()?;
    let active_manifest_uri = session.manifest_uri()?.clone();
    let min_update_period = mpd.minimumUpdatePeriod.unwrap_or(Duration::ZERO);
    let is_dynamic = manifest::is_dynamic_mpd(mpd);
    let wall_now = utc_timing::wall_clock_utc(client, mpd, Some(&active_manifest_uri)).await;
    let template_end_numbers = Some(manifest::parse_segment_template_end_numbers(xml)?);
    let random_access = Some(manifest::parse_random_access_supplements(xml)?);

    Ok(ManifestTick {
        mpd,
        xml,
        active_manifest_uri,
        steering: &session.steering,
        min_update_period,
        is_dynamic,
        wall_now,
        template_end_numbers,
        random_access,
    })
}

pub(crate) fn periods_to_play(
    mpd: &MPD,
    is_dynamic: bool,
    wall_now: DateTime<Utc>,
    sync_depth_s: f64,
) -> Result<Vec<manifest::PeriodWindow>, PlayerError> {
    let period_windows = manifest::period_windows(mpd)?;
    if !is_dynamic {
        return Ok(period_windows);
    }

    let current = manifest::current_period_window_at(mpd, wall_now)?;
    let mut out = vec![current];

    // Include the upcoming Period when wall clock is within the sync-buffer depth of its start
    // and it is Continuous/Connected with the current Period.
    if let Some(since_ast) = manifest::since_availability_start_at(mpd, wall_now)? {
        let since_s = since_ast.as_secs_f64();
        if let Some(next) = period_windows.iter().find(|w| w.idx == current.idx + 1) {
            let link = manifest::period_link(mpd, current.idx, next.idx);
            let near_next = since_s + 1e-9 >= next.start.as_secs_f64() - sync_depth_s.max(0.0);
            if link.allows_soft_transition() && near_next {
                out.push(*next);
            }
        }
    }

    Ok(out)
}

/// Whether a dynamic presentation with known `@mediaPresentationDuration` has finished
/// (wall clock at or past AST + duration). Static MPDs always report finished once played.
pub(crate) fn dynamic_presentation_has_ended(
    mpd: &MPD,
    is_dynamic: bool,
    wall_now: DateTime<Utc>,
) -> Result<bool, PlayerError> {
    if !is_dynamic {
        return Ok(true);
    }
    let Some(duration) = mpd.mediaPresentationDuration.filter(|d| !d.is_zero()) else {
        return Ok(false);
    };
    let Some(since_ast) = manifest::since_availability_start_at(mpd, wall_now)? else {
        return Ok(false);
    };
    Ok(since_ast >= duration)
}

/// End playback after this tick when there is nothing left to fetch/update.
pub(crate) fn should_end_after_tick(
    mpd: &MPD,
    is_dynamic: bool,
    wall_now: DateTime<Utc>,
    min_update_period: Duration,
) -> Result<bool, PlayerError> {
    if min_update_period.is_zero() {
        // No `minimumUpdatePeriod`: static VOD, or dynamic live-to-VoD transition
        // (`mediaPresentationDuration` set, MUP removed — DASH-IF live2vod).
        if !is_dynamic {
            return Ok(true);
        }
        return Ok(mpd.mediaPresentationDuration.is_some());
    }
    // Dynamic with MUP still present: stop once the known presentation duration elapses.
    dynamic_presentation_has_ended(mpd, is_dynamic, wall_now)
}

pub(crate) fn broadcast_manifest_loaded(tracks: &[PlayerTrack], mpd: &MPD, mpd_xml: &str) {
    let is_dynamic = manifest::is_dynamic_mpd(mpd);
    let metadata = manifest::ManifestMetadata::from_mpd(mpd, Some(mpd_xml));
    for t in tracks {
        let _ = t.tx.send(PlayerEvent::ManifestLoaded {
            is_dynamic,
            media_presentation_duration: mpd.mediaPresentationDuration,
            metadata: metadata.clone(),
        });
    }
}
