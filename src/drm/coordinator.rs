//! Coordinates Widevine license acquisition, key rotation, and renewal during playback.

use bytes::Bytes;
use reqwest::Client;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use url::Url;

use super::mp4::{InBandDrmInfo, extract_in_band_drm};
use super::mpd::{MpdDrmInfo, parse_mpd_drm_info};
use super::widevine::{License, WidevineLicenseManager, WidevineSessionKey};
use crate::PlayerError;

pub type AdaptationLicenseSessions<'a> = (
    &'a [Option<Arc<License>>],
    &'a [HashMap<String, Arc<License>>],
);

/// Async license fetch invoked instead of the default `reqwest` POST when set.
pub type WidevineLicenseFetcher = Arc<
    dyn Fn(Url, Vec<u8>) -> Pin<Box<dyn Future<Output = Result<Bytes, PlayerError>> + Send>>
        + Send
        + Sync,
>;

/// Manages Widevine sessions across manifest refresh and key rotation.
pub struct DrmSessionCoordinator {
    manager: WidevineLicenseManager,
    fallback_license_uri: Option<Url>,
    client: Client,
    license_fetch: Option<WidevineLicenseFetcher>,
    adaptation_wv_sessions: Vec<Option<Arc<License>>>,
    adaptation_wv_sessions_by_rep: Vec<HashMap<String, Arc<License>>>,
    /// License URLs from the last MPD sync, indexed by adaptation-set position in DRM info.
    cached_as_license_urls: HashMap<usize, Vec<String>>,
    /// License URLs used for each active PSSH session key.
    session_license_urls: HashMap<WidevineSessionKey, Url>,
}

impl DrmSessionCoordinator {
    pub fn new(
        client: Client,
        fallback_license_uri: Option<Url>,
        license_fetch: Option<WidevineLicenseFetcher>,
    ) -> Self {
        Self {
            manager: WidevineLicenseManager::new(),
            fallback_license_uri,
            client,
            license_fetch,
            adaptation_wv_sessions: Vec::new(),
            adaptation_wv_sessions_by_rep: Vec::new(),
            cached_as_license_urls: HashMap::new(),
            session_license_urls: HashMap::new(),
        }
    }

    pub fn license_for_adaptation(&self, aset_idx: usize) -> Option<Arc<License>> {
        self.adaptation_wv_sessions
            .get(aset_idx)
            .and_then(|session| session.clone())
    }

    pub fn license_for_rep(&self, aset_idx: usize, rep_id: &str) -> Option<Arc<License>> {
        self.adaptation_wv_sessions_by_rep
            .get(aset_idx)
            .and_then(|map| map.get(rep_id).cloned())
            .or_else(|| self.license_for_adaptation(aset_idx))
    }

    pub fn adaptation_sessions(&self) -> AdaptationLicenseSessions<'_> {
        (
            &self.adaptation_wv_sessions,
            &self.adaptation_wv_sessions_by_rep,
        )
    }

    /// Parse DRM from refreshed MPD XML, acquire missing licenses, and update session handles.
    pub async fn sync_from_mpd(
        &mut self,
        mpd_xml: &str,
        period_idx: usize,
    ) -> Result<(), PlayerError> {
        let drm = parse_mpd_drm_info(mpd_xml).map_err(PlayerError::DrmMpd)?;
        self.sync_from_drm_info(&drm, period_idx).await
    }

    pub(crate) async fn sync_from_drm_info(
        &mut self,
        drm: &MpdDrmInfo,
        period_idx: usize,
    ) -> Result<(), PlayerError> {
        let Some(period) = drm.periods.get(period_idx) else {
            self.adaptation_wv_sessions.clear();
            self.adaptation_wv_sessions_by_rep.clear();
            return Ok(());
        };

        let as_count = period.adaptation_sets.len();
        if self.adaptation_wv_sessions.len() != as_count {
            self.adaptation_wv_sessions.resize(as_count, None);
            self.adaptation_wv_sessions_by_rep
                .resize(as_count, HashMap::new());
        }

        for (idx, aset) in period.adaptation_sets.iter().enumerate() {
            self.cached_as_license_urls
                .insert(idx, aset.effective.license_urls.clone());

            if let Some(pssh) = aset.effective.widevine_pssh.first() {
                let session = self
                    .acquire_or_merge_session(
                        pssh,
                        &aset.effective.license_urls,
                        self.adaptation_wv_sessions[idx].clone(),
                    )
                    .await?;
                self.adaptation_wv_sessions[idx] = Some(session);
            } else {
                self.adaptation_wv_sessions[idx] = None;
            }

            let mut rep_sessions = HashMap::new();
            for rep in &aset.representations {
                let Some(rep_id) = rep.id.as_deref() else {
                    continue;
                };
                let Some(rep_pssh) = rep.effective.widevine_pssh.first() else {
                    continue;
                };
                let as_session = self.adaptation_wv_sessions[idx].clone();
                let rep_session = self
                    .acquire_or_merge_session(rep_pssh, &rep.effective.license_urls, as_session)
                    .await?;
                rep_sessions.insert(rep_id.to_string(), rep_session);
            }
            self.adaptation_wv_sessions_by_rep[idx] = rep_sessions;
        }

        Ok(())
    }

    /// Acquire licenses for Widevine PSSH discovered in-band (init segment or `emsg`).
    pub async fn ensure_in_band_drm(
        &mut self,
        aset_idx: usize,
        rep_id: &str,
        info: &InBandDrmInfo,
    ) -> Result<Option<Arc<License>>, PlayerError> {
        if !info.has_widevine_pssh() {
            return Ok(None);
        }

        let license_urls = self
            .cached_as_license_urls
            .get(&aset_idx)
            .cloned()
            .unwrap_or_default();
        let mut updated = None;

        if self.adaptation_wv_sessions.len() <= aset_idx {
            self.adaptation_wv_sessions.resize(aset_idx + 1, None);
            self.adaptation_wv_sessions_by_rep
                .resize(aset_idx + 1, HashMap::new());
        }

        for pssh in info.all_widevine_pssh() {
            let session_key = WidevineSessionKey::from_pssh(pssh);
            if let Some(existing) = self.manager.get(&session_key) {
                self.adaptation_wv_sessions[aset_idx] = Some(existing.clone());
                rep_sessions_mut(&mut self.adaptation_wv_sessions_by_rep, aset_idx)
                    .insert(rep_id.to_string(), existing.clone());
                updated = Some(existing);
                continue;
            }
            let accumulating = self.adaptation_wv_sessions[aset_idx].clone();
            let session = self
                .acquire_or_merge_session(pssh, &license_urls, accumulating)
                .await?;
            self.adaptation_wv_sessions[aset_idx] = Some(session.clone());
            rep_sessions_mut(&mut self.adaptation_wv_sessions_by_rep, aset_idx)
                .insert(rep_id.to_string(), session.clone());
            updated = Some(session);
        }

        Ok(updated)
    }

    /// Parse fragment bytes and acquire any newly discovered in-band Widevine PSSH.
    pub async fn ensure_from_fragments(
        &mut self,
        aset_idx: usize,
        rep_id: &str,
        init: &[u8],
        media: Option<&[u8]>,
    ) -> Result<Option<Arc<License>>, PlayerError> {
        let info = extract_in_band_drm(init, media).map_err(PlayerError::InBandDrm)?;
        self.ensure_in_band_drm(aset_idx, rep_id, &info).await
    }

    /// On decrypt failure, scan init/media for new PSSH and retry key acquisition.
    pub async fn recover_from_decrypt_failure(
        &mut self,
        aset_idx: usize,
        rep_id: &str,
        init: &[u8],
        media: &[u8],
    ) -> Result<Option<Arc<License>>, PlayerError> {
        self.ensure_from_fragments(aset_idx, rep_id, init, Some(media))
            .await
    }

    /// Check active sessions for upcoming license renewal (phase 3).
    pub async fn poll_renewals(&mut self) -> Result<(), PlayerError> {
        let now = Instant::now();
        let sessions = self.manager_sessions_with_urls();
        for (session_key, license) in sessions {
            if !license.renewal_can_renew()? || !license.renewal_needs_action(now)? {
                continue;
            }

            let license_url = license
                .renewal_server_url()?
                .and_then(|url| Url::parse(&url).ok())
                .or_else(|| self.session_license_urls.get(&session_key).cloned())
                .or_else(|| self.fallback_license_uri.clone())
                .ok_or_else(|| {
                    PlayerError::WidevineLicenseHttp(
                        "license renewal required but no license URL is available".into(),
                    )
                })?;

            license.mark_renewal_attempt(now)?;

            let challenge = license.renewal_challenge().map_err(|err| {
                let _ = license.mark_renewal_failure();
                PlayerError::License(err)
            })?;

            match self.fetch_widevine_license(&license_url, challenge).await {
                Ok(bytes) => {
                    if let Err(err) = license.apply_license(bytes.as_ref()) {
                        let _ = license.mark_renewal_failure();
                        if license.renewal_is_expired(now)? {
                            return Err(PlayerError::License(err));
                        }
                    } else {
                        license.mark_renewal_success()?;
                    }
                }
                Err(err) => {
                    let _ = license.mark_renewal_failure();
                    if license.renewal_is_expired(now)? {
                        return Err(err);
                    }
                }
            }
        }
        Ok(())
    }

    fn manager_sessions_with_urls(&self) -> Vec<(WidevineSessionKey, Arc<License>)> {
        self.session_license_urls
            .iter()
            .filter_map(|(session_key, _url)| {
                self.manager
                    .get(session_key)
                    .map(|license| (session_key.clone(), license))
            })
            .collect()
    }

    async fn acquire_or_merge_session(
        &mut self,
        pssh: &pssh_box::PsshBox,
        license_urls: &[String],
        accumulating: Option<Arc<License>>,
    ) -> Result<Arc<License>, PlayerError> {
        let session_key = WidevineSessionKey::from_pssh(pssh);
        if let Some(existing) = self.manager.get(&session_key) {
            return Ok(existing);
        }

        let license_url = license_urls
            .iter()
            .find_map(|s| Url::parse(s).ok())
            .or_else(|| self.fallback_license_uri.clone())
            .ok_or_else(|| {
                PlayerError::WidevineLicenseHttp(format!(
                    "no license URL for PSSH session {}",
                    hex::encode(session_key.as_bytes())
                ))
            })?;

        let new_session = License::new_from_pssh(pssh)?;
        let challenge = new_session.challenge()?;
        let bytes = self.fetch_widevine_license(&license_url, challenge).await?;

        let arc = match accumulating {
            Some(existing) => {
                existing.merge_keys_from_session(bytes.as_ref(), &new_session)?;
                self.manager
                    .insert_arc(session_key.clone(), existing.clone());
                existing
            }
            None => {
                new_session.apply_license(bytes.as_ref())?;
                self.manager.insert_ready(session_key.clone(), new_session)
            }
        };

        self.session_license_urls.insert(session_key, license_url);
        Ok(arc)
    }

    async fn fetch_widevine_license(
        &self,
        license_url: &Url,
        challenge: Vec<u8>,
    ) -> Result<Bytes, PlayerError> {
        if let Some(fetch) = self.license_fetch.as_ref() {
            return fetch(license_url.clone(), challenge).await;
        }
        let resp = self
            .client
            .post(license_url.clone())
            .header("Content-Type", "application/octet-stream")
            .header("Accept", "application/octet-stream")
            .body(challenge)
            .send()
            .await?;
        let resp = resp.error_for_status()?;
        Ok(resp.bytes().await?)
    }
}

#[cfg(test)]
impl DrmSessionCoordinator {
    fn test_register_session(
        &mut self,
        session_key: WidevineSessionKey,
        license: License,
        license_url: Url,
    ) -> Arc<License> {
        let arc = self.manager.insert_ready(session_key.clone(), license);
        self.session_license_urls.insert(session_key, license_url);
        self.adaptation_wv_sessions = vec![Some(arc.clone())];
        arc
    }
}

fn rep_sessions_mut(
    sessions: &mut Vec<HashMap<String, Arc<License>>>,
    aset_idx: usize,
) -> &mut HashMap<String, Arc<License>> {
    if sessions.len() <= aset_idx {
        sessions.resize(aset_idx + 1, HashMap::new());
    }
    &mut sessions[aset_idx]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drm::mpd::parse_mpd_drm_info;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn read_fixture(name: &str, file: &str) -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(root.join(file)).expect("read fixture")
    }

    #[test]
    fn sync_detects_new_pssh_in_rotated_mpd() {
        let xml_v1 = read_fixture("drm_widevine_rotate", "manifest_v1.mpd");
        let xml_v2 = read_fixture("drm_widevine_rotate", "manifest_v2.mpd");
        let drm_v1 = parse_mpd_drm_info(&xml_v1).expect("parse v1");
        let drm_v2 = parse_mpd_drm_info(&xml_v2).expect("parse v2");

        let pssh_v1 = drm_v1.periods[0].adaptation_sets[0].effective.widevine_pssh[0].clone();
        let pssh_v2 = drm_v2.periods[0].adaptation_sets[0].effective.widevine_pssh[0].clone();

        assert_ne!(
            WidevineSessionKey::from_pssh(&pssh_v1),
            WidevineSessionKey::from_pssh(&pssh_v2)
        );
    }

    #[tokio::test]
    async fn poll_renewals_posts_renewal_challenge_when_due() {
        let device_path = match std::env::var("DEVICE_PATH") {
            Ok(v) if !v.is_empty() => v,
            _ => return,
        };
        let _ = device_path;

        let license_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dashif_drm_encrypted/license-response.bin");
        if !license_path.exists() {
            return;
        }
        let license_bytes = std::fs::read(&license_path).expect("read license");

        let xml = read_fixture("drm_widevine", "manifest.mpd");
        let drm = parse_mpd_drm_info(&xml).expect("parse drm");
        let pssh = &drm.periods[0].adaptation_sets[0].effective.widevine_pssh[0];
        let session_key = WidevineSessionKey::from_pssh(pssh);

        let posts = Arc::new(AtomicUsize::new(0));
        let counter = posts.clone();
        let response = license_bytes.clone();
        let fetcher: WidevineLicenseFetcher = Arc::new(move |_url, _challenge| {
            let payload = Bytes::from(response.clone());
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, Ordering::Relaxed);
                Ok(payload)
            })
        });

        let license_url = Url::parse("https://license.example/wv").expect("license url");
        let mut coordinator =
            DrmSessionCoordinator::new(Client::new(), Some(license_url.clone()), Some(fetcher));

        let license = License::new_from_pssh(pssh).expect("license");
        license.apply_license(&license_bytes).expect("apply");
        let license = coordinator.test_register_session(session_key, license, license_url);

        license
            .test_force_renewal_due(Instant::now())
            .expect("force renewal");

        coordinator.poll_renewals().await.expect("poll renewals");
        assert_eq!(
            posts.load(Ordering::Relaxed),
            1,
            "expected one renewal license POST"
        );
    }
}
