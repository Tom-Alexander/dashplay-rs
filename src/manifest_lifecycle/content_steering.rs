//! DASH content steering (DCSM) client support.

use std::time::Duration;

use dash_mpd::BaseURL;
use roxmltree::Document;
use url::Url;

use crate::PlayerError;
use crate::http::{HttpRequest, SharedHttpClient};
use crate::manifest::ManifestError;
use crate::manifest::merge_base_url;
use crate::platform::Instant;

/// Hints for DCSM request query parameters (ETSI TS 103 998 §7.7).
#[derive(Debug, Clone, Default)]
pub(crate) struct SteeringSyncHints {
    /// Currently selected `serviceLocation`, if known.
    pub pathway: Option<String>,
    /// Measured download throughput in bits per second, if known.
    pub throughput_bps: Option<u64>,
}

/// Parsed DASH Content Steering Manifest (DCSM).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ContentSteeringManifest {
    pub service_location_priority: Vec<String>,
    pub reload_uri: Option<String>,
    pub ttl: Duration,
}

/// Steering configuration discovered in the MPD XML.
#[derive(Debug, Clone)]
pub(crate) struct ContentSteeringConfig {
    pub server_uri: Url,
    pub default_service_location: Option<String>,
    pub query_before_start: bool,
}

/// Active steering state carried across manifest refreshes.
#[derive(Debug, Clone, Default)]
pub(crate) struct ContentSteeringState {
    pub manifest: Option<ContentSteeringManifest>,
    pub config: Option<ContentSteeringConfig>,
    reload_uri: Option<Url>,
    fetched_at: Option<Instant>,
    /// Preferred / applied pathway after the last DCSM (or default before first response).
    current_pathway: Option<String>,
}

impl ContentSteeringState {
    pub(crate) fn service_location_priority(&self) -> &[String] {
        self.manifest
            .as_ref()
            .map(|m| m.service_location_priority.as_slice())
            .unwrap_or(&[])
    }

    /// Preferred pathway for BaseURL selection and `_DASH_pathway` reporting.
    pub(crate) fn current_pathway(&self) -> Option<&str> {
        self.current_pathway.as_deref().or_else(|| {
            self.config
                .as_ref()
                .and_then(|c| c.default_service_location.as_deref())
        })
    }

    /// Time until the next TTL-gated DCSM reload, if steering is active.
    pub(crate) fn ttl_remaining(&self) -> Option<Duration> {
        let manifest = self.manifest.as_ref()?;
        let fetched_at = self.fetched_at?;
        let deadline = fetched_at + manifest.ttl;
        let now = Instant::now();
        Some(deadline.saturating_duration_since(now))
    }

    pub(crate) async fn sync_from_mpd_xml(
        &mut self,
        client: &SharedHttpClient,
        mpd_xml: &str,
        manifest_uri: &Url,
        hints: &SteeringSyncHints,
    ) -> Result<(), PlayerError> {
        let Some(config) = parse_content_steering_config(mpd_xml, manifest_uri)? else {
            self.clear();
            return Ok(());
        };

        let server_uri_changed = self
            .config
            .as_ref()
            .is_some_and(|prev| prev.server_uri != config.server_uri);

        if self.current_pathway.is_none() {
            self.current_pathway = config.default_service_location.clone();
        }
        self.config = Some(config.clone());

        if !self.needs_reload(server_uri_changed) {
            return Ok(());
        }

        let base_fetch_uri = self
            .reload_uri
            .clone()
            .unwrap_or_else(|| config.server_uri.clone());
        let fetch_uri = self.build_fetch_uri(base_fetch_uri.clone(), &config, hints);
        let body = client.send(HttpRequest::get(fetch_uri)).await?.text()?;
        let manifest = parse_dcsm(&body)?;

        // Resolve RELOAD-URI against the DCSM URL without client query params.
        self.reload_uri = manifest
            .reload_uri
            .as_deref()
            .map(|u| merge_base_url(&base_fetch_uri, u))
            .transpose()?;
        if let Some(preferred) = manifest.service_location_priority.first() {
            self.current_pathway = Some(preferred.clone());
        }
        self.fetched_at = Some(Instant::now());
        self.config = Some(config);
        self.manifest = Some(manifest);
        Ok(())
    }

    fn clear(&mut self) {
        self.config = None;
        self.manifest = None;
        self.reload_uri = None;
        self.fetched_at = None;
        self.current_pathway = None;
    }

    fn needs_reload(&self, server_uri_changed: bool) -> bool {
        if server_uri_changed {
            return true;
        }
        let Some(manifest) = &self.manifest else {
            return true;
        };
        let Some(fetched_at) = self.fetched_at else {
            return true;
        };
        Instant::now() >= fetched_at + manifest.ttl
    }

    fn build_fetch_uri(
        &self,
        mut url: Url,
        config: &ContentSteeringConfig,
        hints: &SteeringSyncHints,
    ) -> Url {
        let first_query_before_start = config.query_before_start && self.manifest.is_none();
        if first_query_before_start {
            return url;
        }

        let pathway = hints
            .pathway
            .as_deref()
            .or(self.current_pathway.as_deref())
            .or(config.default_service_location.as_deref());
        let throughput = hints.throughput_bps.filter(|&bps| bps > 0);

        {
            let mut pairs = url.query_pairs_mut();
            if let Some(pathway) = pathway {
                pairs.append_pair("_DASH_pathway", &format!("\"{pathway}\""));
            }
            if let Some(throughput) = throughput {
                pairs.append_pair("_DASH_throughput", &throughput.to_string());
            }
        }
        url
    }
}

/// Sleep duration until the next manifest/steering refresh.
///
/// When content steering is active, wake at the earlier of `minimumUpdatePeriod` and DCSM TTL.
pub(crate) fn next_refresh_sleep(
    min_update_period: Duration,
    steering_ttl_remaining: Option<Duration>,
) -> Duration {
    match steering_ttl_remaining {
        Some(ttl) if !min_update_period.is_zero() => min_update_period.min(ttl),
        _ => min_update_period,
    }
}

/// Extract the first `ContentSteering` element (MPD or `ServiceDescription` scope).
pub(crate) fn parse_content_steering_config(
    mpd_xml: &str,
    manifest_uri: &Url,
) -> Result<Option<ContentSteeringConfig>, ManifestError> {
    let doc = Document::parse(mpd_xml)
        .map_err(|e| ManifestError::Parse(dash_mpd::DashMpdError::Parsing(e.to_string())))?;
    let node = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "ContentSteering");
    let Some(node) = node else {
        return Ok(None);
    };

    let url_text = node.text().unwrap_or("").trim();
    if url_text.is_empty() {
        return Ok(None);
    }

    let server_uri = merge_base_url(manifest_uri, url_text)?;
    Ok(Some(ContentSteeringConfig {
        server_uri,
        default_service_location: node.attribute("defaultServiceLocation").map(str::to_string),
        query_before_start: node
            .attribute("queryBeforeStart")
            .is_some_and(|v| v == "true"),
    }))
}

pub(crate) fn parse_dcsm(body: &str) -> Result<ContentSteeringManifest, ManifestError> {
    let version = extract_json_number(body, "VERSION").ok_or_else(|| {
        ManifestError::Parse(dash_mpd::DashMpdError::Parsing(
            "DCSM VERSION missing".into(),
        ))
    })?;
    if version != 1 {
        return Err(ManifestError::Parse(dash_mpd::DashMpdError::Parsing(
            format!("unsupported DCSM VERSION {version}"),
        )));
    }

    let ttl_secs = extract_json_number(body, "TTL").unwrap_or(300);
    let reload_uri = extract_json_string(body, "RELOAD-URI");
    let pathway_priority =
        extract_json_string_array(body, "PATHWAY-PRIORITY").filter(|v| !v.is_empty());
    let service_location_priority = pathway_priority.unwrap_or_else(|| {
        extract_json_string_array(body, "SERVICE-LOCATION-PRIORITY").unwrap_or_default()
    });

    Ok(ContentSteeringManifest {
        service_location_priority,
        reload_uri,
        ttl: Duration::from_secs(ttl_secs),
    })
}

/// Reorder a `BaseURL` layer to prefer steered service locations.
pub(crate) fn order_base_urls_for_steering(
    layer: &[BaseURL],
    priorities: &[String],
    default_service_location: Option<&str>,
) -> Vec<BaseURL> {
    if layer.is_empty() {
        return Vec::new();
    }

    if priorities.is_empty() {
        if let Some(default_loc) = default_service_location {
            let preferred: Vec<BaseURL> = layer
                .iter()
                .filter(|bu| bu.serviceLocation.as_deref() == Some(default_loc))
                .cloned()
                .collect();
            if !preferred.is_empty() {
                let rest: Vec<BaseURL> = layer
                    .iter()
                    .filter(|bu| bu.serviceLocation.as_deref() != Some(default_loc))
                    .cloned()
                    .collect();
                return preferred.into_iter().chain(rest).collect();
            }
        }
        return layer.to_vec();
    }

    let mut ordered = Vec::with_capacity(layer.len());
    let mut used = vec![false; layer.len()];
    for loc in priorities {
        for (idx, bu) in layer.iter().enumerate() {
            if used[idx] {
                continue;
            }
            if bu.serviceLocation.as_deref() == Some(loc.as_str()) {
                ordered.push(bu.clone());
                used[idx] = true;
            }
        }
    }
    for (idx, bu) in layer.iter().enumerate() {
        if !used[idx] {
            ordered.push(bu.clone());
        }
    }
    ordered
}

fn extract_json_number(body: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{key}\"");
    let start = body.find(&pattern)? + pattern.len();
    let rest = body[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_json_string(body: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{key}\"");
    let start = body.find(&pattern)? + pattern.len();
    let rest = body[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_json_string_array(body: &str, key: &str) -> Option<Vec<String>> {
    let pattern = format!("\"{key}\"");
    let start = body.find(&pattern)? + pattern.len();
    let rest = body[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('[')?;
    let end = rest.find(']')?;
    let inner = &rest[..end];
    let mut values = Vec::new();
    for part in inner.split(',') {
        let part = part.trim().trim_matches('"');
        if !part.is_empty() {
            values.push(part.to_string());
        }
    }
    Some(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpClient, HttpError, HttpFuture, HttpResponse, shared};
    use std::sync::Mutex;

    #[test]
    fn parse_dcsm_priority_list() {
        let json = r#"{"VERSION":1,"TTL":300,"SERVICE-LOCATION-PRIORITY":["alpha","beta"]}"#;
        let dcsm = parse_dcsm(json).expect("parse");
        assert_eq!(dcsm.service_location_priority, ["alpha", "beta"]);
        assert_eq!(dcsm.ttl, Duration::from_secs(300));
    }

    #[test]
    fn parse_dcsm_pathway_priority() {
        let json = r#"{"VERSION":1,"TTL":60,"PATHWAY-PRIORITY":["cdn-a","cdn-b"]}"#;
        let dcsm = parse_dcsm(json).expect("parse");
        assert_eq!(dcsm.service_location_priority, ["cdn-a", "cdn-b"]);
    }

    #[test]
    fn parse_dcsm_prefers_pathway_priority_over_service_location() {
        let json =
            r#"{"VERSION":1,"TTL":60,"PATHWAY-PRIORITY":["a"],"SERVICE-LOCATION-PRIORITY":["b"]}"#;
        let dcsm = parse_dcsm(json).expect("parse");
        assert_eq!(dcsm.service_location_priority, ["a"]);
    }

    #[test]
    fn parse_dcsm_reload_uri() {
        let json = r#"{"VERSION":1,"TTL":10,"RELOAD-URI":"next.json","PATHWAY-PRIORITY":["x"]}"#;
        let dcsm = parse_dcsm(json).expect("parse");
        assert_eq!(dcsm.reload_uri.as_deref(), Some("next.json"));
    }

    #[test]
    fn parse_content_steering_from_mpd_xml() {
        let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic">
  <ContentSteering defaultServiceLocation="alpha">steering.json</ContentSteering>
</MPD>"#;
        let base = Url::parse("http://127.0.0.1/manifest.mpd").unwrap();
        let cfg = parse_content_steering_config(mpd, &base)
            .expect("parse")
            .expect("config");
        assert_eq!(cfg.server_uri.as_str(), "http://127.0.0.1/steering.json");
        assert_eq!(cfg.default_service_location.as_deref(), Some("alpha"));
    }

    #[test]
    fn order_base_urls_by_steering_priority() {
        let layer = vec![
            BaseURL {
                serviceLocation: Some("beta".into()),
                base: "https://beta.example/".into(),
                ..Default::default()
            },
            BaseURL {
                serviceLocation: Some("alpha".into()),
                base: "https://alpha.example/".into(),
                ..Default::default()
            },
        ];
        let ordered = order_base_urls_for_steering(&layer, &["alpha".into(), "beta".into()], None);
        assert_eq!(ordered[0].serviceLocation.as_deref(), Some("alpha"));
        assert_eq!(ordered[1].serviceLocation.as_deref(), Some("beta"));
    }

    #[test]
    fn parse_content_steering_from_live_test_manifest() {
        let mpd = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     type="dynamic"
     minimumUpdatePeriod="PT3600S"
     availabilityStartTime="2020-05-01T12:00:00Z"
     timeShiftBufferDepth="PT20S"
     suggestedPresentationDelay="PT2S"
     minBufferTime="PT2S">
  <BaseURL serviceLocation="alpha">alpha/</BaseURL>
  <BaseURL serviceLocation="beta">beta/</BaseURL>
  <ContentSteering defaultServiceLocation="alpha">steering.json</ContentSteering>
</MPD>"#;
        let base = Url::parse("http://127.0.0.1/manifest.mpd").unwrap();
        let cfg = parse_content_steering_config(mpd, &base)
            .expect("parse")
            .expect("config");
        assert_eq!(cfg.server_uri.as_str(), "http://127.0.0.1/steering.json");
    }

    #[test]
    fn needs_reload_respects_ttl() {
        let mut state = ContentSteeringState {
            manifest: Some(ContentSteeringManifest {
                service_location_priority: vec!["alpha".into()],
                reload_uri: None,
                ttl: Duration::from_secs(300),
            }),
            fetched_at: Some(Instant::now()),
            ..Default::default()
        };
        assert!(!state.needs_reload(false));

        state.fetched_at = Some(Instant::now() - Duration::from_secs(301));
        assert!(state.needs_reload(false));
        assert!(state.needs_reload(true));
    }

    #[test]
    fn ttl_remaining_decreases_after_fetch() {
        let state = ContentSteeringState {
            manifest: Some(ContentSteeringManifest {
                service_location_priority: vec!["alpha".into()],
                reload_uri: None,
                ttl: Duration::from_secs(100),
            }),
            fetched_at: Some(Instant::now()),
            ..Default::default()
        };
        let remaining = state.ttl_remaining().expect("ttl");
        assert!(remaining <= Duration::from_secs(100));
        assert!(remaining > Duration::from_secs(90));
    }

    #[test]
    fn build_fetch_uri_appends_pathway_and_throughput() {
        let state = ContentSteeringState {
            current_pathway: Some("beta".into()),
            manifest: Some(ContentSteeringManifest {
                service_location_priority: vec!["beta".into()],
                reload_uri: None,
                ttl: Duration::from_secs(30),
            }),
            ..Default::default()
        };
        let config = ContentSteeringConfig {
            server_uri: Url::parse("http://127.0.0.1/steering.json").unwrap(),
            default_service_location: Some("alpha".into()),
            query_before_start: false,
        };
        let hints = SteeringSyncHints {
            pathway: None,
            throughput_bps: Some(1_500_000),
        };
        let url = state.build_fetch_uri(config.server_uri.clone(), &config, &hints);
        let query = url.query().unwrap_or("");
        assert!(query.contains("_DASH_pathway=%22beta%22"), "query={query}");
        assert!(query.contains("_DASH_throughput=1500000"));
    }

    #[test]
    fn build_fetch_uri_omits_params_on_first_query_before_start() {
        let state = ContentSteeringState::default();
        let config = ContentSteeringConfig {
            server_uri: Url::parse("http://127.0.0.1/steering.json").unwrap(),
            default_service_location: Some("alpha".into()),
            query_before_start: true,
        };
        let hints = SteeringSyncHints {
            pathway: Some("alpha".into()),
            throughput_bps: Some(1000),
        };
        let url = state.build_fetch_uri(config.server_uri.clone(), &config, &hints);
        assert!(url.query().is_none());
    }

    #[test]
    fn next_refresh_sleep_takes_earlier_of_mup_and_ttl() {
        assert_eq!(
            next_refresh_sleep(Duration::from_secs(10), Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        assert_eq!(
            next_refresh_sleep(Duration::from_secs(2), Some(Duration::from_secs(5))),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_refresh_sleep(Duration::from_secs(5), None),
            Duration::from_secs(5)
        );
        assert_eq!(
            next_refresh_sleep(Duration::ZERO, Some(Duration::from_secs(3))),
            Duration::ZERO
        );
    }

    #[derive(Clone)]
    struct RecordingClient {
        hits: std::sync::Arc<Mutex<Vec<Url>>>,
        body: String,
    }

    impl HttpClient for RecordingClient {
        fn send<'a>(
            &'a self,
            request: HttpRequest,
        ) -> HttpFuture<'a, Result<HttpResponse, HttpError>> {
            let hits = self.hits.clone();
            let body = self.body.clone();
            let url = request.url().clone();
            Box::pin(async move {
                hits.lock().expect("lock").push(url);
                Ok(HttpResponse::new(200, vec![], bytes::Bytes::from(body)))
            })
        }
    }

    #[tokio::test]
    async fn sync_ttl_gates_reload_uri_and_query_params() {
        let hits = std::sync::Arc::new(Mutex::new(Vec::new()));
        let body = r#"{"VERSION":1,"TTL":300,"PATHWAY-PRIORITY":["beta"],"RELOAD-URI":"next-steering.json"}"#;
        let client = shared(RecordingClient {
            hits: hits.clone(),
            body: body.to_string(),
        });
        let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic">
  <ContentSteering defaultServiceLocation="alpha">steering.json</ContentSteering>
</MPD>"#;
        let base = Url::parse("http://127.0.0.1/manifest.mpd").unwrap();
        let mut state = ContentSteeringState::default();

        state
            .sync_from_mpd_xml(&client, mpd, &base, &SteeringSyncHints::default())
            .await
            .expect("first sync");
        assert_eq!(hits.lock().expect("lock").len(), 1);
        assert!(
            hits.lock().expect("lock")[0]
                .path()
                .ends_with("/steering.json")
        );
        assert_eq!(state.current_pathway.as_deref(), Some("beta"));
        assert_eq!(
            state.reload_uri.as_ref().map(|u| u.as_str()),
            Some("http://127.0.0.1/next-steering.json")
        );

        // Within TTL: no second fetch.
        state
            .sync_from_mpd_xml(&client, mpd, &base, &SteeringSyncHints::default())
            .await
            .expect("cached sync");
        assert_eq!(hits.lock().expect("lock").len(), 1);

        // Expire TTL and report pathway/throughput on the next request.
        state.fetched_at = Some(Instant::now() - Duration::from_secs(301));
        let hints = SteeringSyncHints {
            pathway: Some("beta".into()),
            throughput_bps: Some(2_000_000),
        };
        state
            .sync_from_mpd_xml(&client, mpd, &base, &hints)
            .await
            .expect("ttl reload");
        let urls = hits.lock().expect("lock");
        assert_eq!(urls.len(), 2);
        assert!(urls[1].path().ends_with("/next-steering.json"));
        let query = urls[1].query().unwrap_or("");
        assert!(query.contains("_DASH_pathway=%22beta%22"), "query={query}");
        assert!(query.contains("_DASH_throughput=2000000"), "query={query}");
    }
}
