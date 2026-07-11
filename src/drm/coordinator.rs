//! Coordinates Widevine license acquisition, key rotation, and renewal during playback.

use bytes::Bytes;
use reqwest::Client;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use url::Url;

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
        }
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

    /// Check active sessions for upcoming license renewal (phase 3).
    pub async fn poll_renewals(&mut self) -> Result<(), PlayerError> {
        let now = Instant::now();
        for license in self.manager_sessions() {
            if license.renewal_needs_action(now)? {
                license.mark_renewal_attempt(now)?;
                // Renewal challenge generation is not yet exposed by the widevine crate.
            }
        }
        Ok(())
    }

    fn manager_sessions(&self) -> Vec<Arc<License>> {
        let mut seen = HashMap::new();
        for license in self.adaptation_wv_sessions.iter().flatten() {
            let ptr = Arc::as_ptr(license) as usize;
            seen.entry(ptr).or_insert_with(|| license.clone());
        }
        for map in &self.adaptation_wv_sessions_by_rep {
            for license in map.values() {
                let ptr = Arc::as_ptr(license) as usize;
                seen.entry(ptr).or_insert_with(|| license.clone());
            }
        }
        seen.into_values().collect()
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
                self.manager.insert_arc(session_key, existing.clone());
                existing
            }
            None => {
                new_session.apply_license(bytes.as_ref())?;
                self.manager.insert_ready(session_key, new_session)
            }
        };

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
mod tests {
    use super::*;
    use crate::drm::mpd::parse_mpd_drm_info;

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
}
