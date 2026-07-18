//! Widevine CDM device loading for license challenges.
//!
//! Native builds may load a `.wvd` from [`DEVICE_PATH`](std::env::var). Browser / WASM
//! builds inject device bytes via [`set_widevine_device_bytes`] before starting playback.

use pssh_box::{PsshBox, ToBytes};
use std::io::Cursor;
use std::sync::RwLock;

#[cfg(not(target_arch = "wasm32"))]
use std::{env, fs::File, io::BufReader};

use super::cdm::{Cdm, CdmLicenseRequest, Device, LicenseType, Pssh};

static DEVICE_WVD: RwLock<Option<Vec<u8>>> = RwLock::new(None);

/// Install Widevine device bytes (`.wvd` / pywidevine format) for subsequent CDM use.
///
/// Call before creating licenses (e.g. from JavaScript `fetch` or a file picker).
/// Replaces any previously configured device. Returns an error if the bytes are not a
/// valid `.wvd`.
pub fn set_widevine_device_bytes(bytes: Vec<u8>) -> Result<(), anyhow::Error> {
    Device::read_wvd(Cursor::new(&bytes))
        .map_err(|e| anyhow::anyhow!("invalid widevine device (.wvd): {e}"))?;
    let mut slot = DEVICE_WVD
        .write()
        .map_err(|_| anyhow::anyhow!("widevine device lock poisoned"))?;
    *slot = Some(bytes);
    Ok(())
}

/// Whether a Widevine device has been configured via [`set_widevine_device_bytes`].
pub fn widevine_device_configured() -> bool {
    DEVICE_WVD.read().map(|g| g.is_some()).unwrap_or(false)
}

pub fn get_cdm() -> anyhow::Result<Cdm> {
    {
        let slot = DEVICE_WVD
            .read()
            .map_err(|_| anyhow::anyhow!("widevine device lock poisoned"))?;
        if let Some(bytes) = slot.as_ref() {
            let device = Device::read_wvd(Cursor::new(bytes.as_slice()))
                .map_err(|e| anyhow::anyhow!("read injected wvd: {e}"))?;
            return Ok(Cdm::new(device));
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        return Err(anyhow::anyhow!(
            "Widevine device not set; call set_widevine_device_bytes before DRM playback"
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path =
            env::var("DEVICE_PATH").map_err(|_| anyhow::anyhow!("DEVICE_PATH is not set"))?;
        let device = Device::read_wvd(BufReader::new(File::open(&path)?))
            .map_err(|e| anyhow::anyhow!("read wvd at {path}: {e}"))?;
        Ok(Cdm::new(device))
    }
}

pub fn create_license_request(pssh: &PsshBox) -> anyhow::Result<CdmLicenseRequest> {
    let cdm = get_cdm()?;
    let pssh_bytes = pssh.to_bytes();
    let pssh =
        Pssh::from_bytes(&pssh_bytes).map_err(|e| anyhow::anyhow!("parse widevine pssh: {e:?}"))?;
    let request = cdm
        .open()
        .get_license_request(pssh, LicenseType::STREAMING)
        .map_err(|e| anyhow::anyhow!("widevine license request: {e:?}"))?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_device_bytes() {
        let err = set_widevine_device_bytes(b"not-a-wvd".to_vec()).unwrap_err();
        assert!(err.to_string().contains("invalid widevine device"));
    }
}
