//! Manifest refresh tick: fetch, steering sync, and period window selection.

use std::time::Duration;

use chrono::{DateTime, Utc};
use dash_mpd::MPD;
use url::Url;

use super::super::PlayerError;
use super::super::clock::utc_timing;
use super::super::http::SharedHttpClient;
use super::super::manifest::{self, SegmentTemplateEndNumbers};
use super::super::manifest_lifecycle::{ContentSteeringState, ManifestSession};
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
}

pub(crate) async fn refresh_manifest(
    session: &mut ManifestSession,
    client: &SharedHttpClient,
    manifest_uri: &Url,
) -> Result<(), PlayerError> {
    session.refresh(client, manifest_uri).await?;
    session.sync_steering(client).await
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

    Ok(ManifestTick {
        mpd,
        xml,
        active_manifest_uri,
        steering: &session.steering,
        min_update_period,
        is_dynamic,
        wall_now,
        template_end_numbers,
    })
}

pub(crate) fn periods_to_play(
    mpd: &MPD,
    is_dynamic: bool,
    wall_now: DateTime<Utc>,
) -> Result<Vec<manifest::PeriodWindow>, PlayerError> {
    let period_windows = manifest::period_windows(mpd)?;
    if is_dynamic {
        Ok(vec![manifest::current_period_window_at(mpd, wall_now)?])
    } else {
        Ok(period_windows)
    }
}

pub(crate) fn broadcast_manifest_loaded(tracks: &[PlayerTrack], mpd: &MPD) {
    let is_dynamic = manifest::is_dynamic_mpd(mpd);
    for t in tracks {
        let _ = t.tx.send(PlayerEvent::ManifestLoaded {
            is_dynamic,
            media_presentation_duration: mpd.mediaPresentationDuration,
        });
    }
}
