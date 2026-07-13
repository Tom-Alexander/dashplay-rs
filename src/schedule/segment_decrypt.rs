//! Widevine media-fragment decryption with key-recovery retry.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::Mutex as AsyncMutex;

use crate::drm::DrmError;
use crate::drm::DrmSessionCoordinator;

#[cfg(feature = "drm")]
use crate::drm::License;

#[cfg(feature = "drm")]
pub(super) async fn decrypt_media_fragment(
    drm: &Arc<AsyncMutex<DrmSessionCoordinator>>,
    period_adaptation_index: usize,
    rep_id: &str,
    init_bytes: &Bytes,
    data: Bytes,
) -> Result<Bytes, DrmError> {
    let license = {
        let guard = drm.lock().await;
        guard.license_for_rep(period_adaptation_index, rep_id)
    };
    let Some(lic) = license else {
        return Ok(data);
    };

    match lic.decrypt(&data, Some(init_bytes)) {
        Ok(decrypted) => Ok(decrypted),
        Err(e) if License::is_likely_missing_key(&e) => {
            let mut guard = drm.lock().await;
            guard
                .recover_from_decrypt_failure(
                    period_adaptation_index,
                    rep_id,
                    init_bytes,
                    data.as_ref(),
                )
                .await?;
            let refreshed = guard.license_for_rep(period_adaptation_index, rep_id);
            drop(guard);
            let Some(new_lic) = refreshed else {
                return Err(DrmError::License(e));
            };
            new_lic
                .decrypt(&data, Some(init_bytes))
                .map_err(DrmError::License)
        }
        Err(e) => {
            let msg = e.to_string().to_ascii_lowercase();
            if msg.contains("not encrypted") || msg.contains("no") && msg.contains("senc") {
                Ok(data)
            } else {
                Err(DrmError::License(e))
            }
        }
    }
}

#[cfg(not(feature = "drm"))]
pub(super) async fn decrypt_media_fragment(
    _drm: &Arc<AsyncMutex<DrmSessionCoordinator>>,
    _period_adaptation_index: usize,
    _rep_id: &str,
    _init_bytes: &Bytes,
    data: Bytes,
) -> Result<Bytes, DrmError> {
    Ok(data)
}
