//! Capture a Widevine license response for DRM integration tests.
//!
//! Usage:
//! ```text
//! DEVICE_PATH=/path/to/device.wvd cargo run --example capture_widevine_license -- \
//!   --pssh-b64 <base64-pssh> \
//!   --license-url https://license.example/wv \
//!   --output tests/fixtures/dashif_drm_encrypted/license-response.bin
//! ```

use base64::Engine;
use std::path::PathBuf;

#[derive(Debug)]
struct Args {
    pssh_b64: String,
    license_url: String,
    output: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut pssh_b64 = None;
    let mut license_url = None;
    let mut output = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--pssh-b64" => pssh_b64 = it.next(),
            "--license-url" => license_url = it.next(),
            "--output" => output = it.next().map(PathBuf::from),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args {
        pssh_b64: pssh_b64.ok_or("missing --pssh-b64")?,
        license_url: license_url.ok_or("missing --license-url")?,
        output: output.ok_or("missing --output")?,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args =
        parse_args().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let pssh_bytes = base64::engine::general_purpose::STANDARD.decode(args.pssh_b64.trim())?;
    let boxes = pssh_box::from_bytes(&pssh_bytes).map_err(|e| format!("parse pssh: {e:?}"))?;
    let pssh = boxes.into_iter().next().ok_or("PSSH box list is empty")?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let license = dashplayrs::drm::License::new_from_pssh(&pssh)?;
        let challenge = license.challenge()?;
        let client = reqwest::Client::new();
        let response = client
            .post(&args.license_url)
            .header("Content-Type", "application/octet-stream")
            .header("Accept", "application/octet-stream")
            .body(challenge)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        std::fs::write(&args.output, &response)?;
        println!(
            "Wrote {} bytes to {}",
            response.len(),
            args.output.display()
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}
