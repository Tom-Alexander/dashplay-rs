//! Top-level DASH client facade (dash.js: `MediaPlayer`).

use tokio::sync::broadcast;
use tokio::sync::watch;
use url::Url;

use super::PlayerError;
use super::abr::{BolaAbrFactory, SharedAbrFactory, shared as shared_abr_factory};
use super::cmcd::{CmcdConfig, CmcdObjectType, CmcdSession, CmcdStreamType, parse_cmsd_headers};
#[cfg(feature = "reqwest-http")]
use super::http::ReqwestClient;
#[cfg(not(feature = "reqwest-http"))]
use super::http::UnconfiguredHttpClient;
use super::http::{HttpRequest, SharedHttpClient, shared};
use super::manifest::{self, ManifestError};
use super::playback_control::PlaybackController;
use super::stream_controller::PlaybackLoopState;
use super::track_selection::{TrackSelection, select_adaptation_sets};
use super::types::PlayerOutputs;
use crate::clock::utc_timing;

pub use super::drm::{DrmSessionCoordinator, WidevineLicenseFetcher};

/// DASH MPD client and playback coordinator (dash.js: `MediaPlayer`).
pub struct MediaPlayer {
    client: SharedHttpClient,
    manifest_uri: Url,
    #[allow(dead_code)]
    license_uri: Option<Url>,
    license_fetch: Option<WidevineLicenseFetcher>,
    manifest: Option<dash_mpd::MPD>,
    mpd_xml: Option<String>,
    drm: DrmSessionCoordinator,
    track_selection: TrackSelection,
    abr_factory: SharedAbrFactory,
    cmcd: Option<CmcdSession>,
}

impl MediaPlayer {
    pub fn new(uri: &str, license_uri: Option<&str>) -> Result<Self, PlayerError> {
        let license_uri = license_uri.map(Url::parse).transpose()?;
        #[cfg(feature = "reqwest-http")]
        let client = shared(ReqwestClient::default());
        #[cfg(not(feature = "reqwest-http"))]
        let client = shared(UnconfiguredHttpClient::default());
        Ok(Self {
            client: client.clone(),
            manifest_uri: Url::parse(uri)?,
            license_uri: license_uri.clone(),
            license_fetch: None,
            manifest: None,
            mpd_xml: None,
            drm: DrmSessionCoordinator::new(client, license_uri, None),
            track_selection: TrackSelection::default(),
            abr_factory: shared_abr_factory(BolaAbrFactory::default()),
            cmcd: None,
        })
    }

    /// Use a custom [`HttpClient`](crate::HttpClient) for manifest, segment, and clock-sync requests.
    pub fn with_http_client(mut self, client: SharedHttpClient) -> Self {
        self.client = client.clone();
        self.drm = DrmSessionCoordinator::new(
            client,
            self.license_uri.clone(),
            self.license_fetch.clone(),
        );
        self
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

    /// Configure deterministic audio, video, and text adaptation-set selection.
    pub fn with_track_selection(mut self, selection: TrackSelection) -> Self {
        self.track_selection = selection;
        self
    }

    /// Use a custom [`AbrFactory`](crate::AbrFactory) for representation selection.
    ///
    /// The default is [`BolaAbrFactory`].
    pub fn with_abr_factory(mut self, factory: SharedAbrFactory) -> Self {
        self.abr_factory = factory;
        self
    }

    /// Enable CTA-5004 CMCD request headers and CTA-5006 CMSD response parsing.
    ///
    /// CMCD keys are sent on the four `CMCD-*` headers (header mode only). Parsed CMSD
    /// is exposed via metrics and [`crate::PlayerEvent::CmsdUpdated`] and does not drive ABR.
    pub fn with_cmcd(mut self, config: CmcdConfig) -> Self {
        self.cmcd = Some(CmcdSession::new(config));
        self
    }

    pub async fn fetch_manifest(&mut self) -> Result<(), PlayerError> {
        let mut req = HttpRequest::get(self.manifest_uri.clone());
        if let Some(session) = self.cmcd.as_ref() {
            let ctx = session.context_for(CmcdObjectType::Manifest, None, None, None, None, None);
            req = session.apply(req, &ctx);
        }
        let resp = self.client.send(req).await?;
        if let Some(cmsd) =
            parse_cmsd_headers(resp.headers().iter().map(|(k, v)| (k.as_str(), v.as_str())))
        {
            if let Some(session) = self.cmcd.as_ref() {
                session.record_cmsd(cmsd);
            }
        }
        let text = resp.text()?;
        let resolved = crate::manifest_lifecycle::resolve_period_xlinks(
            &self.client,
            &self.manifest_uri,
            &text,
        )
        .await
        .map_err(|e| ManifestError::Xlink(e.to_string()))?;
        let mpd = dash_mpd::parse(&resolved)?;
        if let Some(session) = self.cmcd.as_ref() {
            session.set_stream_type(CmcdStreamType::from_dynamic(manifest::is_dynamic_mpd(&mpd)));
        }
        self.manifest = Some(mpd);
        self.mpd_xml = Some(resolved);
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
    /// Each receiver sees lifecycle events ([`PlayerEvent::ManifestLoaded`], [`PlayerEvent::BufferUpdated`],
    /// [`PlayerEvent::BitrateChanged`], [`PlayerEvent::PlaybackStarted`]/[`PlayerEvent::PlaybackEnded`],
    /// [`PlayerEvent::Error`]) plus [`PlayerEvent::Init`] then [`PlayerEvent::Segment`], then
    /// [`PlayerEvent::End`] when the manifest window is exhausted (no `minimumUpdatePeriod`
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

        let playback = PlaybackController::new();
        playback.mark_started();

        let mut tracks = Vec::with_capacity(adaptation_sets.len());
        for (track_idx, selected) in adaptation_sets.into_iter().enumerate() {
            let (tx, _rx) = broadcast::channel(32);
            let (buffer_tx, buffer_rx) = watch::channel(0.0);
            let metrics = super::metrics::TrackMetrics::new();
            tracks.push(super::types::PlayerTrack::new(
                selected.info,
                tx.clone(),
                super::types::BufferFeedback::new(
                    buffer_tx.clone(),
                    metrics.clone(),
                    tx,
                    playback.clone(),
                    track_idx,
                ),
                buffer_tx,
                buffer_rx,
                metrics,
            ));
        }

        let manifest_metadata = manifest::ManifestMetadata::from_mpd(mpd, self.mpd_xml.as_deref());

        let loop_state = PlaybackLoopState {
            client: self.client,
            manifest_uri: self.manifest_uri,
            drm: self.drm,
            playback: playback.clone(),
            track_selection: self.track_selection,
            abr_factory: self.abr_factory,
            cmcd: self.cmcd,
        };

        Ok(PlayerOutputs {
            tracks,
            playback,
            manifest_metadata,
            loop_state,
        })
    }
}
