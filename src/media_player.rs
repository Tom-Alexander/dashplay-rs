//! Top-level DASH client facade (dash.js: `MediaPlayer`).

use bytes::Bytes;
use reqwest::Client;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use url::Url;

use super::drm::mpd::{MpdDrmInfo, parse_mpd_drm_info};
use super::drm::widevine::{License, WidevineLicenseManager, WidevineSessionKey};

use super::PlayerError;
use super::manifest;
use super::stream_controller::PlaybackLoopState;
use super::types::PlayerOutputs;
use super::utc_timing;

/// Async license fetch invoked instead of the default `reqwest` POST when set.
pub type WidevineLicenseFetcher = Arc<
    dyn Fn(Url, Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Bytes, PlayerError>> + Send>>
        + Send
        + Sync,
>;

/// DASH MPD client and playback coordinator (dash.js: `MediaPlayer`).
pub struct MediaPlayer {
    client: Client,
    manifest_uri: Url,
    #[allow(dead_code)]
    license_uri: Option<Url>,
    /// When `Some`, used for Widevine license POSTs instead of [`MediaPlayer`]'s `reqwest` client.
    license_fetch: Option<WidevineLicenseFetcher>,
    manifest: Option<dash_mpd::MPD>,
    mpd_xml: Option<String>,
    drm_info: Option<MpdDrmInfo>,
    license_manager: WidevineLicenseManager,
    /// Widevine sessions available to each adaptation-set (index-aligned).
    adaptation_wv_sessions: Vec<Option<Arc<License>>>,
    /// Representation-specific sessions for each adaptation set (repId -> session).
    adaptation_wv_sessions_by_rep: Vec<HashMap<String, Arc<License>>>,
}

impl MediaPlayer {
    pub fn new(uri: &str, license_uri: Option<&str>) -> Result<Self, PlayerError> {
        Ok(Self {
            client: Client::new(),
            manifest_uri: Url::parse(uri)?,
            license_uri: license_uri.map(Url::parse).transpose()?,
            license_fetch: None,
            manifest: None,
            mpd_xml: None,
            drm_info: None,
            license_manager: WidevineLicenseManager::new(),
            adaptation_wv_sessions: Vec::new(),
            adaptation_wv_sessions_by_rep: Vec::new(),
        })
    }

    /// Use a custom async fetcher for Widevine license requests (e.g. extra headers, cookies, or a proxy).
    pub fn with_license_fetcher(mut self, fetcher: WidevineLicenseFetcher) -> Self {
        self.license_fetch = Some(fetcher);
        self
    }

    async fn fetch_widevine_license(
        &self,
        license_url: &Url,
        challenge: Vec<u8>,
    ) -> Result<Bytes, PlayerError> {
        if let Some(fetch) = self.license_fetch.as_ref() {
            return fetch(license_url.clone(), challenge).await;
        }
        let req = self
            .client
            .post(license_url.clone())
            .header("Content-Type", "application/octet-stream")
            .header("Accept", "application/octet-stream");
        let resp = req.body(challenge).send().await?;
        let resp = resp.error_for_status()?;
        let raw = resp.bytes().await?;
        Ok(raw)
    }

    pub async fn fetch_manifest(&mut self) -> Result<(), PlayerError> {
        let resp = self.client.get(self.manifest_uri.clone()).send().await?;
        let text = resp.text().await?;
        let mpd = dash_mpd::parse(&text)?;
        self.manifest = Some(mpd);
        self.mpd_xml = Some(text.clone());

        self.drm_info = Some(parse_mpd_drm_info(&text).map_err(PlayerError::DrmMpd)?);

        Ok(())
    }

    /// Attach source and start the stream controller (dash.js: initialize + attachSource).
    ///
    /// Subscribe to **every** `outputs.tracks[i]` you care about before relying on delivery:
    /// each adaptation set runs in parallel, and a broadcast with no receivers drops events.
    ///
    /// Each receiver sees `PlayerEvent::Init` then `PlayerEvent::Segment`, then
    /// `PlayerEvent::End` when the manifest window is exhausted (no `minimumUpdatePeriod`
    /// refresh). For live manifests, `End` is not sent until the controller stops.
    /// Drop all `Sender`s after awaiting `outputs.join` if you need `RecvError::Closed`.
    pub async fn start(mut self) -> Result<PlayerOutputs, PlayerError> {
        self.fetch_manifest().await?;
        let mpd = manifest::mpd(&self.manifest)?;
        let wall_now =
            utc_timing::wall_clock_utc(&self.client, mpd, Some(&self.manifest_uri)).await;
        let current_window = manifest::current_period_window_at(mpd, wall_now)?;
        let period = &mpd.periods[current_window.idx];

        // Build per-adaptation Widevine sessions for the *current* Period,
        // using DASH DRM inheritance: Rep + AS + Period + MPD.
        self.adaptation_wv_sessions.clear();
        self.adaptation_wv_sessions_by_rep.clear();
        if let Some(drm) = self.drm_info.as_ref() {
            if let Some(p) = drm.periods.get(current_window.idx) {
                let fallback_license_uri = self.license_uri.clone();
                // Use adaptation-set effective DRM (rep overrides are handled later when fetching init).
                self.adaptation_wv_sessions = vec![None; p.adaptation_sets.len()];
                self.adaptation_wv_sessions_by_rep = vec![HashMap::new(); p.adaptation_sets.len()];
                for (idx, aset) in p.adaptation_sets.iter().enumerate() {
                    let Some(pssh) = aset.effective.widevine_pssh.first() else {
                        continue;
                    };
                    let session_key = WidevineSessionKey::from_pssh(pssh);
                    if let Some(existing) = self.license_manager.get(&session_key) {
                        self.adaptation_wv_sessions[idx] = Some(existing);
                        continue;
                    }
                    let license_url = aset
                        .effective
                        .license_urls
                        .iter()
                        .find_map(|s| Url::parse(s).ok())
                        .or_else(|| fallback_license_uri.clone());
                    let Some(license_url) = license_url else {
                        continue;
                    };
                    let mut license = License::new_from_pssh(pssh)?;
                    let challenge = license.challenge()?;
                    let bytes = self.fetch_widevine_license(&license_url, challenge).await?;
                    license.set_license(bytes.as_ref())?;
                    let arc = self.license_manager.insert_ready(session_key, license);
                    self.adaptation_wv_sessions[idx] = Some(arc);

                    // Also establish sessions for Representation-effective DRM when it differs.
                    for rep in &aset.representations {
                        let Some(rep_id) = rep.id.as_deref() else {
                            continue;
                        };
                        let Some(rep_pssh) = rep.effective.widevine_pssh.first() else {
                            continue;
                        };
                        let rep_key = WidevineSessionKey::from_pssh(rep_pssh);
                        let rep_arc = if let Some(existing) = self.license_manager.get(&rep_key) {
                            existing
                        } else {
                            let license_url = rep
                                .effective
                                .license_urls
                                .iter()
                                .find_map(|s| Url::parse(s).ok())
                                .or_else(|| fallback_license_uri.clone())
                                .unwrap_or_else(|| license_url.clone());
                            let mut lic = License::new_from_pssh(rep_pssh)?;
                            let challenge = lic.challenge()?;
                            let bytes =
                                self.fetch_widevine_license(&license_url, challenge).await?;
                            lic.set_license(bytes.as_ref())?;
                            self.license_manager.insert_ready(rep_key, lic)
                        };
                        self.adaptation_wv_sessions_by_rep[idx].insert(rep_id.to_string(), rep_arc);
                    }
                }
            }
        }

        let adaptation_sets: Vec<&dash_mpd::AdaptationSet> = period
            .adaptations
            .iter()
            .filter(|adaptation_set| {
                let mime = adaptation_set.mimeType.as_deref();
                matches!(
                    mime,
                    Some(m) if m == manifest::MimeType::Audio.as_str()
                        || m == manifest::MimeType::Video.as_str()
                )
            })
            .collect();

        let mut tracks = Vec::with_capacity(adaptation_sets.len());
        for aset in &adaptation_sets {
            let (tx, _rx) = broadcast::channel(32);
            tracks.push(super::types::PlayerTrack {
                mime_type: aset.mimeType.clone(),
                tx,
            });
        }

        let loop_state = PlaybackLoopState {
            client: self.client,
            manifest_uri: self.manifest_uri,
            manifest: self.manifest,
            adaptation_wv_sessions: self.adaptation_wv_sessions,
            adaptation_wv_sessions_by_rep: self.adaptation_wv_sessions_by_rep,
            last_period_idx: None,
        };

        let tracks_for_task = tracks.clone();
        let join: JoinHandle<Result<(), PlayerError>> =
            tokio::spawn(async move { loop_state.run(tracks_for_task).await });

        Ok(PlayerOutputs { tracks, join })
    }
}
