//! DASH content steering (DCSM) client support.

use std::time::Duration;

use dash_mpd::BaseURL;
use roxmltree::Document;
use url::Url;

use super::PlayerError;
use super::http::{HttpRequest, SharedHttpClient};
use super::manifest::merge_base_url;

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
}

impl ContentSteeringState {
    pub(crate) fn service_location_priority(&self) -> &[String] {
        self.manifest
            .as_ref()
            .map(|m| m.service_location_priority.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) async fn sync_from_mpd_xml(
        &mut self,
        client: &SharedHttpClient,
        mpd_xml: &str,
        manifest_uri: &Url,
    ) -> Result<(), PlayerError> {
        let Some(config) = parse_content_steering_config(mpd_xml, manifest_uri)? else {
            self.config = None;
            self.manifest = None;
            self.reload_uri = None;
            return Ok(());
        };

        self.config = Some(config.clone());

        if config.query_before_start && self.manifest.is_some() {
            return Ok(());
        }

        let fetch_uri = self
            .reload_uri
            .clone()
            .unwrap_or_else(|| config.server_uri.clone());
        let body = client
            .send(HttpRequest::get(fetch_uri.clone()))
            .await?
            .text()?;
        let manifest = parse_dcsm(&body)?;

        self.reload_uri = manifest
            .reload_uri
            .as_deref()
            .map(|u| merge_base_url(&fetch_uri, u))
            .transpose()?;
        self.config = Some(config);
        self.manifest = Some(manifest);
        Ok(())
    }
}

/// Extract the first `ContentSteering` element (MPD or `ServiceDescription` scope).
pub(crate) fn parse_content_steering_config(
    mpd_xml: &str,
    manifest_uri: &Url,
) -> Result<Option<ContentSteeringConfig>, PlayerError> {
    let doc = Document::parse(mpd_xml)
        .map_err(|e| PlayerError::Manifest(dash_mpd::DashMpdError::Parsing(e.to_string())))?;
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

pub(crate) fn parse_dcsm(body: &str) -> Result<ContentSteeringManifest, PlayerError> {
    let version = extract_json_number(body, "VERSION").ok_or_else(|| {
        PlayerError::Manifest(dash_mpd::DashMpdError::Parsing(
            "DCSM VERSION missing".into(),
        ))
    })?;
    if version != 1 {
        return Err(PlayerError::Manifest(dash_mpd::DashMpdError::Parsing(
            format!("unsupported DCSM VERSION {version}"),
        )));
    }

    let ttl_secs = extract_json_number(body, "TTL").unwrap_or(300);
    let reload_uri = extract_json_string(body, "RELOAD-URI");
    let service_location_priority =
        extract_json_string_array(body, "SERVICE-LOCATION-PRIORITY").unwrap_or_default();

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

    #[test]
    fn parse_dcsm_priority_list() {
        let json = r#"{"VERSION":1,"TTL":300,"SERVICE-LOCATION-PRIORITY":["alpha","beta"]}"#;
        let dcsm = parse_dcsm(json).expect("parse");
        assert_eq!(dcsm.service_location_priority, ["alpha", "beta"]);
        assert_eq!(dcsm.ttl, Duration::from_secs(300));
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
}
