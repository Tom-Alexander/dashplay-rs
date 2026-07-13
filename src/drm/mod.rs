#[cfg(feature = "drm")]
pub mod cdm;
#[cfg(feature = "drm")]
pub mod coordinator;
#[cfg(feature = "drm")]
pub mod decrypt;
#[cfg(feature = "drm")]
pub mod mp4;
#[cfg(feature = "drm")]
pub mod mpd;
#[cfg(feature = "drm")]
mod playback_error;
#[cfg(feature = "drm")]
mod renewal;
#[cfg(feature = "drm")]
pub mod widevine;

#[cfg(not(feature = "drm"))]
mod stub;

#[cfg(feature = "drm")]
pub use coordinator::{DrmSessionCoordinator, WidevineLicenseFetcher};
#[cfg(feature = "drm")]
pub use playback_error::DrmError;
#[cfg(feature = "drm")]
pub use widevine::{License, LicenseError, WidevineLicenseManager, WidevineSessionKey};

#[cfg(not(feature = "drm"))]
pub use stub::{DrmSessionCoordinator, WidevineLicenseFetcher};
