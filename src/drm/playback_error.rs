use thiserror::Error;

use crate::http::HttpError;

use super::LicenseError;
use super::mp4::Mp4DrmError;
use super::mpd::MpdDrmError;

/// Errors from Widevine license acquisition and segment decryption.
#[derive(Debug, Error)]
pub enum DrmError {
    #[error("widevine license HTTP: {0}")]
    WidevineLicenseHttp(String),
    #[error("license request: {0}")]
    Request(#[from] HttpError),
    #[error("widevine license/decrypt: {0}")]
    License(#[from] LicenseError),
    #[error("mpd drm parse: {0}")]
    Mpd(#[from] MpdDrmError),
    #[error("in-band mp4 drm parse: {0}")]
    InBand(#[from] Mp4DrmError),
}
