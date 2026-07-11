//! Top-level DASH client facade (dash.js: `MediaPlayer`).

use reqwest::Client;
use tokio::sync::broadcast;
use tokio::sync::watch;
use url::Url;

use super::drm::coordinator::DrmSessionCoordinator;

use super::PlayerError;
use super::manifest;
use super::playback_control::PlaybackController;
use super::stream_controller::PlaybackLoopState;
use super::track_selection::{TrackSelection, select_adaptation_sets};
use super::types::PlayerOutputs;
use super::utc_timing;

pub use super::drm::coordinator::WidevineLicenseFetcher;

/// DASH MPD client and playback coordinator (dash.js: `MediaPlayer`).
pub struct MediaPlayer {
    client: Client,
    manifest_uri: Url,
    #[allow(dead_code)]
    license_uri: Option<Url>,
    license_fetch: Option<WidevineLicenseFetcher>,
    manifest: Option<dash_mpd::MPD>,
    mpd_xml: Option<String>,
    drm: DrmSessionCoordinator,
    track_selection: TrackSelection,
}

impl MediaPlayer {
    pub fn new(uri: &str, license_uri: Option<&str>) -> Result<Self, PlayerError> {
        let license_uri = license_uri.map(Url::parse).transpose()?;
        Ok(Self {
            client: Client::new(),
            manifest_uri: Url::parse(uri)?,
            license_uri: license_uri.clone(),
            license_fetch: None,
            manifest: None,
            mpd_xml: None,
            drm: DrmSessionCoordinator::new(Client::new(), license_uri, None),
            track_selection: TrackSelection::default(),
        })
    }

    /// Use a custom async fetcher for Widevine license requests (e.g. extra headers, cookies, or a proxy).
    pub fn with_license_fetcher(mut self, fetcher: WidevineLicenseFetcher) -> Self {
        self.license_fetch = Some(fetcher.clone());
        self.drm = DrmSessionCoordinator::new(
            self.client.clone(),
            self.license_uri.clone(),
            Some(fetcher),
        );
        self
    }

    /// Configure deterministic audio and video adaptation-set selection.
    pub fn with_track_selection(mut self, selection: TrackSelection) -> Self {
        self.track_selection = selection;
        self
    }

    pub async fn fetch_manifest(&mut self) -> Result<(), PlayerError> {
        let resp = self.client.get(self.manifest_uri.clone()).send().await?;
        let text = resp.text().await?;
        let mpd = dash_mpd::parse(&text)?;
        self.manifest = Some(mpd);
        self.mpd_xml = Some(text);
        Ok(())
    }

    /// Attach source and prepare the stream controller (dash.js: initialize + attachSource).
    ///
    /// This does **not** spawn a background task. After subscribing to the tracks you need,
    /// call [`PlayerOutputs::run`] on the current task or [`PlayerOutputs::spawn`] for a
    /// separate Tokio task.
    ///
    /// Subscribe to **every** `outputs.tracks[i]` you care about before relying on delivery:
    /// each adaptation set runs in parallel, and a broadcast with no receivers drops events.
    ///
    /// Each receiver sees `PlayerEvent::Init` then `PlayerEvent::Segment`, then
    /// `PlayerEvent::End` when the manifest window is exhausted (no `minimumUpdatePeriod`
    /// refresh). For live manifests, `End` is not sent until the controller stops.
    /// Drop all `Sender`s after the loop finishes if you need `RecvError::Closed`.
    pub async fn start(mut self) -> Result<PlayerOutputs, PlayerError> {
        self.fetch_manifest().await?;
        let mpd = manifest::mpd(&self.manifest)?;
        let wall_now =
            utc_timing::wall_clock_utc(&self.client, mpd, Some(&self.manifest_uri)).await;
        let current_window = manifest::current_period_window_at(mpd, wall_now)?;
        let period = &mpd.periods[current_window.idx];

        if let Some(xml) = self.mpd_xml.as_deref() {
            self.drm.sync_from_mpd(xml, current_window.idx).await?;
        }

        let adaptation_sets = select_adaptation_sets(period, &self.track_selection);

        let mut tracks = Vec::with_capacity(adaptation_sets.len());
        for selected in adaptation_sets {
            let (tx, _rx) = broadcast::channel(32);
            let (buffer_tx, buffer_rx) = watch::channel(0.0);
            let metrics = super::metrics::TrackMetrics::new();
            tracks.push(super::types::PlayerTrack {
                mime_type: selected.info.mime_type.clone(),
                info: selected.info,
                tx,
                buffer_feedback: super::types::BufferFeedback::new(buffer_tx, metrics.clone()),
                buffer_rx,
                metrics,
            });
        }

        let playback = PlaybackController::new();
        playback.mark_started();

        let loop_state = PlaybackLoopState {
            client: self.client,
            manifest_uri: self.manifest_uri,
            drm: self.drm,
            playback: playback.clone(),
            track_selection: self.track_selection,
        };

        Ok(PlayerOutputs {
            tracks,
            playback,
            loop_state,
        })
    }
}
