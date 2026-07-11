use pssh_box::{PsshBox, ToBytes};
use std::{env, fs::File, io::BufReader};

use super::cdm::{Cdm, CdmLicenseRequest, Device, LicenseType, Pssh};

pub fn get_cdm() -> anyhow::Result<Cdm> {
    let path = env::var("DEVICE_PATH").map_err(|_| anyhow::anyhow!("DEVICE_PATH is not set"))?;
    let device = Device::read_wvd(BufReader::new(File::open(&path)?))
        .map_err(|e| anyhow::anyhow!("read wvd at {path}: {e}"))?;
    Ok(Cdm::new(device))
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
