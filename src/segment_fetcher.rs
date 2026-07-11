use reqwest::Client;
use url::Url;

use super::PlayerError;
use super::segment_blacklist::SegmentBlacklist;

/// Low-level segment / fragment HTTP fetch (dash.js: FragmentLoader / HTTPLoader).
///
/// Failing HTTP responses record the full segment URL in `blacklist` so callers can try the next
/// alternative `BaseURL` (ISO/IEC 23009-1 §5.6.5) without repeating the same request.
pub async fn fetch_bytes(
    client: &Client,
    url: Url,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<u8>, PlayerError> {
    if blacklist.contains_url(&url) {
        return Err(PlayerError::SegmentBlacklisted(url.to_string()));
    }

    let resp = client.get(url.clone()).send().await?;
    let status = resp.status();
    if !status.is_success() {
        blacklist.insert_url(&url);
        return Err(PlayerError::SegmentRequestFailed {
            status: status.as_u16(),
            url: url.to_string(),
        });
    }
    let b = resp.bytes().await?;
    Ok(b.to_vec())
}

/// Try each resolved absolute base with the same relative segment path (multi-CDN / redundant hosts).
pub async fn fetch_bytes_with_base_failover(
    client: &Client,
    bases: &[Url],
    relative_path: &str,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<u8>, PlayerError> {
    let mut last_err: Option<PlayerError> = None;
    for base in bases {
        let url = base.join(relative_path)?;
        match fetch_bytes(client, url, blacklist).await {
            Ok(b) => return Ok(b),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or(PlayerError::SegmentExhaustedRepresentations))
}
