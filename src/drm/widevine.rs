use super::decrypt;
use bytes::Bytes;
use mp4decrypt::Ap4CencDecryptingProcessor;
use pssh_box::{PsshBox, ToBytes};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use thiserror::Error;
use widevine::{CdmLicenseRequest, Key, KeyType};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct WidevineSessionKey(Vec<u8>);

impl WidevineSessionKey {
    pub fn from_pssh(pssh: &PsshBox) -> Self {
        Self(pssh.to_bytes())
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

struct LicenseInner {
    request: CdmLicenseRequest,
    processor: Option<Arc<Ap4CencDecryptingProcessor>>,
    content_keys: HashMap<[u8; 16], Vec<u8>>,
    renewal: RenewalState,
}

/// Widevine license session with a decryptor that accumulates content keys across rotations.
#[derive(Clone)]
pub struct License {
    inner: Arc<RwLock<LicenseInner>>,
}

impl License {
    pub fn new_from_pssh(pssh: &PsshBox) -> Result<Self, LicenseError> {
        let request = decrypt::create_license_request(pssh)?;
        Ok(Self {
            inner: Arc::new(RwLock::new(LicenseInner {
                request,
                processor: None,
                content_keys: HashMap::new(),
                renewal: RenewalState::default(),
            })),
        })
    }

    pub fn challenge(&self) -> Result<Vec<u8>, LicenseError> {
        let inner = self.inner.read().map_err(|_| LicenseError::LockPoisoned)?;
        inner.request.challenge().map_err(LicenseError::Widevine)
    }

    /// Apply a license response; merge CONTENT keys into the decryptor.
    pub fn apply_license(&self, license_message: &[u8]) -> Result<(), LicenseError> {
        let key_set = {
            let inner = self.inner.read().map_err(|_| LicenseError::LockPoisoned)?;
            inner
                .request
                .get_keys(license_message)
                .map_err(LicenseError::Widevine)?
        };
        self.integrate_key_set(key_set)
    }

    /// Merge CONTENT keys from a license response acquired via `source` into this session.
    pub(crate) fn merge_keys_from_session(
        &self,
        license_message: &[u8],
        source: &License,
    ) -> Result<(), LicenseError> {
        let key_set = {
            let source_inner = source
                .inner
                .read()
                .map_err(|_| LicenseError::LockPoisoned)?;
            source_inner
                .request
                .get_keys(license_message)
                .map_err(LicenseError::Widevine)?
        };
        self.integrate_key_set(key_set)
    }

    fn integrate_key_set(&self, key_set: widevine::KeySet) -> Result<(), LicenseError> {
        let mut inner = self.inner.write().map_err(|_| LicenseError::LockPoisoned)?;

        for key in key_set.of_type(KeyType::KEY_CONTROL) {
            inner.renewal.update_from_key_control(key);
        }

        let mut added = false;
        for key in key_set.of_type(KeyType::CONTENT) {
            if inner
                .content_keys
                .insert(key.kid, key.key.clone())
                .is_none()
            {
                added = true;
            }
        }

        if inner.content_keys.is_empty() {
            return Err(LicenseError::WidevineNoContentKeys);
        }

        if added || inner.processor.is_none() {
            inner.processor = Some(rebuild_processor(&inner.content_keys)?);
        }

        Ok(())
    }

    /// Keys currently loaded (for tests / diagnostics).
    pub fn loaded_kids(&self) -> Result<Vec<[u8; 16]>, LicenseError> {
        let inner = self.inner.read().map_err(|_| LicenseError::LockPoisoned)?;
        let mut kids: Vec<[u8; 16]> = inner.content_keys.keys().copied().collect();
        kids.sort_unstable();
        Ok(kids)
    }

    /// Backward-compatible alias for [`Self::apply_license`].
    pub fn set_license(&self, license_message: &[u8]) -> Result<(), LicenseError> {
        self.apply_license(license_message)
    }

    pub fn decrypt(&self, ciphertext: &Bytes, init: Option<&Bytes>) -> Result<Bytes, LicenseError> {
        let inner = self.inner.read().map_err(|_| LicenseError::LockPoisoned)?;
        let processor = inner
            .processor
            .as_ref()
            .ok_or(LicenseError::LicenseNotSet)?;
        let init_ref = init.map(|b| b.as_ref());
        let decrypted = processor
            .decrypt(ciphertext.as_ref(), init_ref)
            .map_err(LicenseError::Mp4Decrypt)?;
        Ok(Bytes::from(decrypted))
    }

    pub fn has_kid(&self, kid: &[u8; 16]) -> Result<bool, LicenseError> {
        let inner = self.inner.read().map_err(|_| LicenseError::LockPoisoned)?;
        Ok(inner.content_keys.contains_key(kid))
    }

    /// Returns true when decrypt failed in a way that may be fixed by acquiring more keys.
    pub fn is_likely_missing_key(err: &LicenseError) -> bool {
        matches!(
            err,
            LicenseError::Mp4Decrypt(mp4decrypt::Error::DecryptionFailed(_))
        )
    }

    pub(crate) fn renewal_needs_action(&self, now: Instant) -> Result<bool, LicenseError> {
        let inner = self.inner.read().map_err(|_| LicenseError::LockPoisoned)?;
        Ok(inner.renewal.needs_renewal(now))
    }

    pub(crate) fn mark_renewal_attempt(&self, now: Instant) -> Result<(), LicenseError> {
        let mut inner = self.inner.write().map_err(|_| LicenseError::LockPoisoned)?;
        inner.renewal.last_attempt = Some(now);
        Ok(())
    }
}

fn rebuild_processor(
    content_keys: &HashMap<[u8; 16], Vec<u8>>,
) -> Result<Arc<Ap4CencDecryptingProcessor>, LicenseError> {
    let mut builder = Ap4CencDecryptingProcessor::new();
    let mut kids: Vec<[u8; 16]> = content_keys.keys().copied().collect();
    kids.sort_unstable();
    for kid in kids {
        let key_bytes = content_keys
            .get(&kid)
            .expect("kid present in content_keys map");
        let kid_hex = hex::encode(kid);
        let key_hex = hex::encode(key_bytes);
        builder = builder
            .key(&kid_hex, &key_hex)
            .map_err(LicenseError::Mp4Decrypt)?;
    }
    let built = builder.build().map_err(LicenseError::Mp4Decrypt)?;
    Ok(Arc::new(built))
}

#[derive(Default)]
struct RenewalState {
    /// Wall-clock instant before which renewal must complete (phase 3).
    #[allow(dead_code)]
    renew_after: Option<Instant>,
    /// Last renewal attempt (backoff on failure).
    last_attempt: Option<Instant>,
}

impl RenewalState {
    fn update_from_key_control(&mut self, _key: &Key) {
        // Phase 3: parse KEY_CONTROL block for renewal deadline.
    }

    fn needs_renewal(&self, _now: Instant) -> bool {
        // Phase 3: renewal challenge generation is not yet exposed by the widevine crate.
        false
    }
}

#[derive(Error, Debug)]
pub enum LicenseError {
    #[error("set the license before decrypting")]
    LicenseNotSet,
    #[error("Widevine returned no content keys")]
    WidevineNoContentKeys,
    #[error("license state lock poisoned")]
    LockPoisoned,
    #[error("Widevine: {0}")]
    Widevine(#[from] widevine::Error),
    #[error("create license request: {0}")]
    LicenseRequest(#[from] anyhow::Error),
    #[error("MP4 decrypt: {0}")]
    Mp4Decrypt(#[from] mp4decrypt::Error),
}

/// dash.js-like: manages multiple sessions (e.g. key rotation / per-stream DRM).
pub struct WidevineLicenseManager {
    sessions: HashMap<WidevineSessionKey, Arc<License>>,
}

impl Default for WidevineLicenseManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WidevineLicenseManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    pub fn get(&self, key: &WidevineSessionKey) -> Option<Arc<License>> {
        self.sessions.get(key).cloned()
    }

    pub fn insert_ready(&mut self, key: WidevineSessionKey, license: License) -> Arc<License> {
        let arc = Arc::new(license);
        self.sessions.insert(key, arc.clone());
        arc
    }

    pub fn insert_arc(&mut self, key: WidevineSessionKey, license: Arc<License>) -> Arc<License> {
        self.sessions.insert(key, license.clone());
        license
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_processor_accepts_multiple_kids() {
        let mut keys = HashMap::new();
        keys.insert([0x01; 16], vec![0xAA; 16]);
        keys.insert([0x02; 16], vec![0xBB; 16]);
        let processor = rebuild_processor(&keys).expect("build processor");
        assert!(Arc::strong_count(&processor) >= 1);
    }
}
