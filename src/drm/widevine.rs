use super::decrypt;
use bytes::Bytes;
use mp4decrypt::Ap4CencDecryptingProcessor;
use pssh_box::{PsshBox, ToBytes};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use widevine::{CdmLicenseRequest, Key, KeyType};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct WidevineSessionKey(Vec<u8>);

impl WidevineSessionKey {
    pub fn from_pssh(pssh: &PsshBox) -> Self {
        Self(pssh.to_bytes())
    }
}

pub struct License {
    request: CdmLicenseRequest,
    processor: Option<Arc<Ap4CencDecryptingProcessor>>,
}

impl License {
    pub fn new_from_pssh(pssh: &PsshBox) -> Result<Self, LicenseError> {
        let request = decrypt::create_license_request(pssh)?;
        Ok(Self {
            request,
            processor: None,
        })
    }

    pub fn challenge(&self) -> Result<Vec<u8>, LicenseError> {
        self.request.challenge().map_err(LicenseError::Widevine)
    }

    pub fn set_license(&mut self, license_message: &[u8]) -> Result<(), LicenseError> {
        let key_set = self
            .request
            .get_keys(license_message)
            .map_err(LicenseError::Widevine)?;

        let content_keys: Vec<&Key> = key_set.of_type(KeyType::CONTENT).collect();
        if content_keys.is_empty() {
            return Err(LicenseError::WidevineNoContentKeys);
        }

        let mut builder = Ap4CencDecryptingProcessor::new();
        for key in content_keys {
            let kid_hex = hex::encode(key.kid);
            let key_hex = hex::encode(&key.key);
            builder = builder
                .key(&kid_hex, &key_hex)
                .map_err(LicenseError::Mp4Decrypt)?;
        }

        let built = builder.build().map_err(LicenseError::Mp4Decrypt)?;
        self.processor = Some(Arc::new(built));
        Ok(())
    }

    pub fn decrypt(&self, ciphertext: &Bytes, init: Option<&Bytes>) -> Result<Bytes, LicenseError> {
        let processor = self.processor.as_ref().ok_or(LicenseError::LicenseNotSet)?;
        let init_ref = init.map(|b| b.as_ref());
        let decrypted = processor
            .decrypt(ciphertext.as_ref(), init_ref)
            .map_err(LicenseError::Mp4Decrypt)?;
        Ok(Bytes::from(decrypted))
    }
}

#[derive(Error, Debug)]
pub enum LicenseError {
    #[error("set the license before decrypting")]
    LicenseNotSet,
    #[error("Widevine returned no content keys")]
    WidevineNoContentKeys,
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
}
