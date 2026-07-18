//! Manifest refresh: `Location`, MPD patch, and content steering integration.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use dash_mpd::MPD;
use url::Url;

use crate::PlayerError;
use crate::cmcd::{CmcdObjectType, CmcdSession, parse_cmsd_headers};
use crate::http::{
    HttpError, HttpRequest, HttpRequestKind, HttpRetryConfig, SharedHttpClient, send_with_retry,
};
use crate::manifest::ManifestError;
use crate::manifest::merge_base_url;
use crate::platform;

use super::content_steering::{ContentSteeringState, SteeringSyncHints};
use super::patch::{self, MpdPatchError};
use super::xlink::{self, XlinkError};

/// Result of a successful manifest refresh (initial load or live update).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ManifestRefreshOutcome {
    /// Set when one or more patch attempts failed and the player fell back to a full MPD.
    pub patch_fallback: Option<String>,
}

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
        cmcd: Option<&CmcdSession>,
        http_retry: &HttpRetryConfig,
    ) -> Result<ManifestRefreshOutcome, PlayerError> {
        let fetch_uri = self.resolve_fetch_uri(initial_uri)?;
        let (xml, parsed, patch_fallback) = self
            .fetch_manifest_body(client, &fetch_uri, cmcd, http_retry)
            .await?;
        if let Some(session) = cmcd {
            session.set_stream_type(crate::cmcd::CmcdStreamType::from_dynamic(
                crate::manifest::is_dynamic_mpd(&parsed),
            ));
        }
        self.current_uri = Some(resolve_location_uri(&parsed, &fetch_uri)?);
        self.mpd_xml = Some(xml);
        self.parsed = Some(parsed);
        Ok(ManifestRefreshOutcome { patch_fallback })
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
        hints: &SteeringSyncHints,
    ) -> Result<(), PlayerError> {
        let xml = self.xml()?.to_string();
        let uri = self.manifest_uri()?.clone();
        self.steering
            .sync_from_mpd_xml(client, &xml, &uri, hints)
            .await
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
        cmcd: Option<&CmcdSession>,
        http_retry: &HttpRetryConfig,
    ) -> Result<(String, MPD, Option<String>), PlayerError> {
        let patch_uris = self.patch_fetch_uris(fetch_uri, platform::utc_now())?;
        let mut last_patch_err: Option<String> = None;
        for patch_uri in &patch_uris {
            match self
                .try_fetch_patch(client, fetch_uri, patch_uri, cmcd, http_retry)
                .await
            {
                Ok((xml, parsed)) => return Ok((xml, parsed, None)),
                Err(err) => {
                    last_patch_err = Some(err.to_string());
                }
            }
        }

        let patch_fallback = last_patch_err;
        let req = manifest_http_request(fetch_uri.clone(), cmcd);
        let resp =
            send_with_retry(client, req, http_retry, HttpRequestKind::Manifest, false).await?;
        if let Some(cmsd) =
            parse_cmsd_headers(resp.headers().iter().map(|(k, v)| (k.as_str(), v.as_str())))
            && let Some(session) = cmcd
        {
            session.record_cmsd(cmsd);
        }
        let text = resp.text()?;
        let resolved = xlink::resolve_period_xlinks(client, fetch_uri, &text, http_retry)
            .await
            .map_err(map_xlink_error)?;
        let parsed = dash_mpd::parse(&resolved)?;
        Ok((resolved, parsed, patch_fallback))
    }

    fn patch_fetch_uris(
        &self,
        fetch_uri: &Url,
        now: DateTime<Utc>,
    ) -> Result<Vec<Url>, ManifestError> {
        let Some(_xml) = self.mpd_xml.as_deref() else {
            return Ok(Vec::new());
        };
        let Some(mpd) = self.parsed.as_ref() else {
            return Ok(Vec::new());
        };
        resolve_active_patch_uris(mpd, fetch_uri, now)
    }

    async fn try_fetch_patch(
        &self,
        client: &SharedHttpClient,
        manifest_uri: &Url,
        patch_uri: &Url,
        cmcd: Option<&CmcdSession>,
        http_retry: &HttpRetryConfig,
    ) -> Result<(String, MPD), PlayerError> {
        let base_xml = self.mpd_xml.as_deref().ok_or(ManifestError::NotLoaded)?;
        let resp = send_with_retry(
            client,
            manifest_http_request(patch_uri.clone(), cmcd),
            http_retry,
            HttpRequestKind::Manifest,
            false,
        )
        .await?;
        if !resp.is_success() {
            return Err(
                HttpError::Transport(format!("patch HTTP status {}", resp.status())).into(),
            );
        }
        if let Some(cmsd) =
            parse_cmsd_headers(resp.headers().iter().map(|(k, v)| (k.as_str(), v.as_str())))
            && let Some(session) = cmcd
        {
            session.record_cmsd(cmsd);
        }
        let patch_xml = resp.text()?;
        let updated = patch::apply_mpd_patch(base_xml, &patch_xml).map_err(map_patch_error)?;
        let resolved = xlink::resolve_period_xlinks(client, manifest_uri, &updated, http_retry)
            .await
            .map_err(map_xlink_error)?;
        let parsed = dash_mpd::parse(&resolved)?;
        Ok((resolved, parsed))
    }
}

/// Resolve non-expired `PatchLocation` URLs in document order (dash.js `getPatchLocation`).
///
/// Requires `MPD@publishTime`. Locations without `@ttl` remain valid; with `@ttl` (seconds),
/// valid while `publishTime + ttl > now`.
pub(crate) fn resolve_active_patch_uris(
    mpd: &MPD,
    fetch_uri: &Url,
    now: DateTime<Utc>,
) -> Result<Vec<Url>, ManifestError> {
    let Some(publish_time) = mpd.publishTime else {
        return Ok(Vec::new());
    };

    let mut uris = Vec::new();
    for patch in &mpd.PatchLocation {
        let path = patch.content.trim();
        if path.is_empty() {
            continue;
        }
        if !patch_location_ttl_valid(publish_time, patch.ttl, now) {
            continue;
        }
        uris.push(merge_base_url(fetch_uri, path)?);
    }
    Ok(uris)
}

fn patch_location_ttl_valid(
    publish_time: DateTime<Utc>,
    ttl_secs: Option<f64>,
    now: DateTime<Utc>,
) -> bool {
    let Some(ttl_secs) = ttl_secs else {
        return true;
    };
    if !ttl_secs.is_finite() || ttl_secs < 0.0 {
        return false;
    }
    let ttl_ms = (ttl_secs * 1000.0).round().min(i64::MAX as f64) as i64;
    publish_time
        .checked_add_signed(ChronoDuration::milliseconds(ttl_ms))
        .is_some_and(|expiry| expiry > now)
}

fn map_patch_error(err: MpdPatchError) -> ManifestError {
    ManifestError::Parse(dash_mpd::DashMpdError::Parsing(err.to_string()))
}

fn map_xlink_error(err: XlinkError) -> ManifestError {
    ManifestError::Xlink(err.to_string())
}

fn manifest_http_request(uri: Url, cmcd: Option<&CmcdSession>) -> HttpRequest {
    let mut req = HttpRequest::get(uri);
    if let Some(session) = cmcd {
        let ctx = session.context_for(CmcdObjectType::Manifest, None, None, None, None, None);
        req = session.apply(req, &ctx);
    }
    req
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
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;

    use crate::http::{HttpClient, HttpFuture, HttpRequest, HttpResponse, shared};

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

    #[test]
    fn patch_ttl_filters_expired_and_keeps_valid() {
        let mpd_xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
     publishTime="2020-05-01T12:00:00Z">
  <PatchLocation ttl="30">expired.mpp</PatchLocation>
  <PatchLocation ttl="120">valid.mpp</PatchLocation>
  <PatchLocation>no-ttl.mpp</PatchLocation>
</MPD>"#;
        let mpd = dash_mpd::parse(mpd_xml).expect("parse");
        let base = Url::parse("https://example.com/live/manifest.mpd").unwrap();
        let now = DateTime::parse_from_rfc3339("2020-05-01T12:01:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let uris = resolve_active_patch_uris(&mpd, &base, now).unwrap();
        assert_eq!(
            uris.iter().map(Url::as_str).collect::<Vec<_>>(),
            vec![
                "https://example.com/live/valid.mpp",
                "https://example.com/live/no-ttl.mpp",
            ]
        );
    }

    #[test]
    fn patch_locations_require_publish_time() {
        let mpd_xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic">
  <PatchLocation ttl="60">patch.mpp</PatchLocation>
</MPD>"#;
        let mpd = dash_mpd::parse(mpd_xml).expect("parse");
        let base = Url::parse("https://example.com/live/manifest.mpd").unwrap();
        let now = Utc::now();
        let uris = resolve_active_patch_uris(&mpd, &base, now).unwrap();
        assert!(uris.is_empty());
    }

    #[test]
    fn patch_ttl_exact_expiry_is_expired() {
        let publish = DateTime::parse_from_rfc3339("2020-05-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = DateTime::parse_from_rfc3339("2020-05-01T12:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!patch_location_ttl_valid(publish, Some(60.0), now));
        assert!(patch_location_ttl_valid(publish, Some(61.0), now));
    }

    #[derive(Clone, Default)]
    struct RecordingMockClient {
        responses: Arc<Mutex<HashMap<String, HttpResponse>>>,
        hits: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingMockClient {
        fn with_response(self, url: &str, response: HttpResponse) -> Self {
            self.responses
                .lock()
                .expect("lock")
                .insert(url.to_string(), response);
            self
        }
    }

    impl HttpClient for RecordingMockClient {
        fn send<'a>(
            &'a self,
            request: HttpRequest,
        ) -> HttpFuture<'a, Result<HttpResponse, crate::http::HttpError>> {
            let url = request.url().to_string();
            let responses = self.responses.clone();
            let hits = self.hits.clone();
            Box::pin(async move {
                hits.lock().expect("lock").push(url.clone());
                responses
                    .lock()
                    .expect("lock")
                    .get(&url)
                    .cloned()
                    .ok_or_else(|| crate::http::HttpError::Transport(format!("no mock for {url}")))
            })
        }
    }

    const BASE_MPD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     id="patch-live"
     type="dynamic"
     publishTime="2020-05-01T12:00:12Z"
     minimumUpdatePeriod="PT0.5S">
  <PatchLocation ttl="315360000">patch.mpp</PatchLocation>
  <Period id="P0"/>
</MPD>"#;

    const FULL_MPD_AFTER: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     id="patch-live"
     type="dynamic"
     publishTime="2020-05-01T12:00:16Z"
     minimumUpdatePeriod="PT0.5S">
  <Period id="P0"/>
</MPD>"#;

    const GOOD_PATCH: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Patch xmlns="urn:mpeg:dash:schema:mpd-patch:2020"
     mpdId="patch-live"
     originalPublishTime="2020-05-01T12:00:12Z"
     publishTime="2020-05-01T12:00:16Z">
  <replace sel="/MPD/@publishTime">2020-05-01T12:00:16Z</replace>
</Patch>"#;

    const BAD_PATCH: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Patch xmlns="urn:mpeg:dash:schema:mpd-patch:2020"
     mpdId="patch-live"
     originalPublishTime="1999-01-01T00:00:00Z"
     publishTime="2020-05-01T12:00:16Z">
  <replace sel="/MPD/@publishTime">2020-05-01T12:00:16Z</replace>
</Patch>"#;

    fn seed_session(xml: &str) -> (ManifestSession, Url) {
        let uri = Url::parse("https://example.com/live/manifest.mpd").unwrap();
        let mut session = ManifestSession::default();
        session.initialize(uri.clone());
        session.mpd_xml = Some(xml.to_string());
        session.parsed = Some(dash_mpd::parse(xml).expect("parse seed"));
        session.current_uri = Some(uri.clone());
        (session, uri)
    }

    fn xml_response(body: &str) -> HttpResponse {
        HttpResponse::new(200, vec![], Bytes::from(body.to_string()))
    }

    #[tokio::test]
    async fn refresh_applies_valid_patch() {
        let (mut session, uri) = seed_session(BASE_MPD);
        let client = shared(RecordingMockClient::default().with_response(
            "https://example.com/live/patch.mpp",
            xml_response(GOOD_PATCH),
        ));
        let outcome = session
            .refresh(&client, &uri, None, &HttpRetryConfig::default())
            .await
            .expect("refresh");
        assert!(outcome.patch_fallback.is_none());
        assert_eq!(
            session.parsed().unwrap().publishTime.unwrap().to_rfc3339(),
            "2020-05-01T12:00:16+00:00"
        );
    }

    #[tokio::test]
    async fn refresh_falls_back_after_invalid_patch() {
        let (mut session, uri) = seed_session(BASE_MPD);
        let client = shared(
            RecordingMockClient::default()
                .with_response(
                    "https://example.com/live/patch.mpp",
                    xml_response(BAD_PATCH),
                )
                .with_response(
                    "https://example.com/live/manifest.mpd",
                    xml_response(FULL_MPD_AFTER),
                ),
        );
        let outcome = session
            .refresh(&client, &uri, None, &HttpRetryConfig::default())
            .await
            .expect("refresh");
        let reason = outcome.patch_fallback.expect("patch fallback");
        assert!(
            reason.contains("originalPublishTime") || reason.contains("validation"),
            "reason={reason}"
        );
        assert_eq!(
            session.parsed().unwrap().publishTime.unwrap().to_rfc3339(),
            "2020-05-01T12:00:16+00:00"
        );
    }

    #[tokio::test]
    async fn refresh_tries_next_patch_location_after_http_failure() {
        let seed = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     id="patch-live"
     type="dynamic"
     publishTime="2020-05-01T12:00:12Z"
     minimumUpdatePeriod="PT0.5S">
  <PatchLocation ttl="315360000">bad.mpp</PatchLocation>
  <PatchLocation ttl="315360000">good.mpp</PatchLocation>
  <Period id="P0"/>
</MPD>"#;
        let (mut session, uri) = seed_session(seed);
        let mock = RecordingMockClient::default()
            .with_response(
                "https://example.com/live/bad.mpp",
                HttpResponse::new(404, vec![], Bytes::from_static(b"missing")),
            )
            .with_response(
                "https://example.com/live/good.mpp",
                xml_response(GOOD_PATCH),
            );
        let hits = mock.hits.clone();
        let client = shared(mock);
        let outcome = session
            .refresh(&client, &uri, None, &HttpRetryConfig::default())
            .await
            .expect("refresh");
        assert!(outcome.patch_fallback.is_none());
        let hits = hits.lock().expect("lock").clone();
        assert!(hits.iter().any(|u| u.contains("bad.mpp")), "hits={hits:?}");
        assert!(hits.iter().any(|u| u.contains("good.mpp")), "hits={hits:?}");
        assert_eq!(
            session.parsed().unwrap().publishTime.unwrap().to_rfc3339(),
            "2020-05-01T12:00:16+00:00"
        );
    }

    #[tokio::test]
    async fn refresh_skips_expired_patch_ttl_without_fallback_event() {
        let seed = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     id="patch-live"
     type="dynamic"
     publishTime="2020-05-01T12:00:12Z"
     minimumUpdatePeriod="PT0.5S">
  <PatchLocation ttl="60">patch.mpp</PatchLocation>
  <Period id="P0"/>
</MPD>"#;
        let (mut session, uri) = seed_session(seed);
        let mock = RecordingMockClient::default()
            .with_response(
                "https://example.com/live/manifest.mpd",
                xml_response(FULL_MPD_AFTER),
            )
            .with_response(
                "https://example.com/live/patch.mpp",
                xml_response(GOOD_PATCH),
            );
        let hits = mock.hits.clone();
        let client = shared(mock);
        let outcome = session
            .refresh(&client, &uri, None, &HttpRetryConfig::default())
            .await
            .expect("refresh");
        assert!(outcome.patch_fallback.is_none());
        let hits = hits.lock().expect("lock").clone();
        assert!(
            !hits.iter().any(|u| u.contains("patch.mpp")),
            "expired ttl must skip patch fetch, hits={hits:?}"
        );
        assert!(
            hits.iter().any(|u| u.contains("manifest.mpd")),
            "expected full MPD fetch, hits={hits:?}"
        );
    }
}
