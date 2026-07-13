//! No-op DRM coordinator used when the `drm` feature is disabled.

use url::Url;

use crate::drm::DrmError;
use crate::http::SharedHttpClient;

/// Async license fetch hook (unused without the `drm` feature).
pub type WidevineLicenseFetcher = crate::platform::LicenseFetcher;

/// DRM coordinator stub: passes encrypted bytes through unchanged.
pub struct DrmSessionCoordinator {
    _client: SharedHttpClient,
}

impl DrmSessionCoordinator {
    pub fn new(
        client: SharedHttpClient,
        _fallback_license_uri: Option<Url>,
        _license_fetch: Option<WidevineLicenseFetcher>,
    ) -> Self {
        Self { _client: client }
    }

    pub async fn sync_from_mpd(
        &mut self,
        _mpd_xml: &str,
        _period_idx: usize,
    ) -> Result<(), DrmError> {
        Ok(())
    }

    pub async fn ensure_from_fragments(
        &mut self,
        _aset_idx: usize,
        _rep_id: &str,
        _init: &[u8],
        _media: Option<&[u8]>,
    ) -> Result<(), DrmError> {
        Ok(())
    }

    pub async fn recover_from_decrypt_failure(
        &mut self,
        _aset_idx: usize,
        _rep_id: &str,
        _init: &[u8],
        _media: &[u8],
    ) -> Result<(), DrmError> {
        Ok(())
    }

    pub async fn poll_renewals(&mut self) -> Result<(), DrmError> {
        Ok(())
    }
}
