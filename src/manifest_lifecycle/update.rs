//! Manifest refresh: `Location`, MPD patch, and content steering integration.

use dash_mpd::MPD;
use url::Url;

use crate::PlayerError;
use crate::http::{HttpRequest, SharedHttpClient};
use crate::manifest::ManifestError;
use crate::manifest::merge_base_url;

use super::content_steering::ContentSteeringState;
use super::patch::{self, MpdPatchError};
use super::xlink::{self, XlinkError};

/// Cached manifest state used across live refreshes.
#[derive(Debug, Default)]
pub(crate) struct ManifestSession {
    pub current_uri: Option<Url>,
    pub mpd_xml: Option<String>,
    pub parsed: Option<MPD>,
    pub steering: ContentSteeringState,
}

impl ManifestSession {
    pub(crate) fn initialize(&mut self, manifest_uri: Url) {
        self.current_uri = Some(manifest_uri);
    }

    pub(crate) async fn refresh(
        &mut self,
        client: &SharedHttpClient,
        initial_uri: &Url,
    ) -> Result<(), PlayerError> {
        let fetch_uri = self.resolve_fetch_uri(initial_uri)?;
        let (xml, parsed) = self.fetch_manifest_body(client, &fetch_uri).await?;
        self.current_uri = Some(resolve_location_uri(&parsed, &fetch_uri)?);
        self.mpd_xml = Some(xml);
        self.parsed = Some(parsed);
        Ok(())
    }

    pub(crate) fn xml(&self) -> Result<&str, ManifestError> {
        self.mpd_xml.as_deref().ok_or(ManifestError::NotLoaded)
    }

    pub(crate) fn parsed(&self) -> Result<&MPD, ManifestError> {
        self.parsed.as_ref().ok_or(ManifestError::NotLoaded)
    }

    pub(crate) fn manifest_uri(&self) -> Result<&Url, ManifestError> {
        self.current_uri.as_ref().ok_or(ManifestError::NotLoaded)
    }

    pub(crate) async fn sync_steering(
        &mut self,
        client: &SharedHttpClient,
    ) -> Result<(), PlayerError> {
        let xml = self.xml()?.to_string();
        let uri = self.manifest_uri()?.clone();
        self.steering.sync_from_mpd_xml(client, &xml, &uri).await
    }

    fn resolve_fetch_uri(&self, initial_uri: &Url) -> Result<Url, ManifestError> {
        Ok(self
            .current_uri
            .clone()
            .unwrap_or_else(|| initial_uri.clone()))
    }

    async fn fetch_manifest_body(
        &mut self,
        client: &SharedHttpClient,
        fetch_uri: &Url,
    ) -> Result<(String, MPD), PlayerError> {
        if let Some(patch_uri) = self.patch_fetch_uri(fetch_uri)? {
            match self.try_fetch_patch(client, fetch_uri, &patch_uri).await {
                Ok(result) => return Ok(result),
                Err(_err) => {}
            }
        }

        let resp = client.send(HttpRequest::get(fetch_uri.clone())).await?;
        let text = resp.text()?;
        let resolved = xlink::resolve_period_xlinks(client, fetch_uri, &text)
            .await
            .map_err(map_xlink_error)?;
        let parsed = dash_mpd::parse(&resolved)?;
        Ok((resolved, parsed))
    }

    fn patch_fetch_uri(&self, fetch_uri: &Url) -> Result<Option<Url>, ManifestError> {
        let Some(_xml) = self.mpd_xml.as_deref() else {
            return Ok(None);
        };
        let Some(mpd) = self.parsed.as_ref() else {
            return Ok(None);
        };
        if mpd.PatchLocation.is_empty() {
            return Ok(None);
        }
        let patch = &mpd.PatchLocation[0];
        let path = patch.content.trim();
        if path.is_empty() {
            return Ok(None);
        }
        Ok(Some(merge_base_url(fetch_uri, path)?))
    }

    async fn try_fetch_patch(
        &self,
        client: &SharedHttpClient,
        manifest_uri: &Url,
        patch_uri: &Url,
    ) -> Result<(String, MPD), PlayerError> {
        let base_xml = self.mpd_xml.as_deref().ok_or(ManifestError::NotLoaded)?;
        let resp = client.send(HttpRequest::get(patch_uri.clone())).await?;
        let patch_xml = resp.text()?;
        let updated = patch::apply_mpd_patch(base_xml, &patch_xml).map_err(map_patch_error)?;
        let resolved = xlink::resolve_period_xlinks(client, manifest_uri, &updated)
            .await
            .map_err(map_xlink_error)?;
        let parsed = dash_mpd::parse(&resolved)?;
        Ok((resolved, parsed))
    }
}

fn map_patch_error(err: MpdPatchError) -> ManifestError {
    ManifestError::Parse(dash_mpd::DashMpdError::Parsing(err.to_string()))
}

fn map_xlink_error(err: XlinkError) -> ManifestError {
    ManifestError::Xlink(err.to_string())
}

/// Resolve the manifest URI for the next refresh from the latest `Location` element.
pub(crate) fn resolve_location_uri(mpd: &MPD, base_uri: &Url) -> Result<Url, ManifestError> {
    let Some(location) = mpd.locations.last() else {
        return Ok(base_uri.clone());
    };
    let url = location.url.trim();
    if url.is_empty() {
        return Ok(base_uri.clone());
    }
    merge_base_url(base_uri, url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_location_from_mpd() {
        let mpd_xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic">
  <Location>alt/manifest.mpd</Location>
</MPD>"#;
        let mpd = dash_mpd::parse(mpd_xml).expect("parse");
        let base = Url::parse("https://example.com/live/manifest.mpd").unwrap();
        let resolved = resolve_location_uri(&mpd, &base).unwrap();
        assert_eq!(
            resolved.as_str(),
            "https://example.com/live/alt/manifest.mpd"
        );
    }
}
