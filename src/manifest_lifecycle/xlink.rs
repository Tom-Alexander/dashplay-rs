//! Period `xlink:href` resolution (ISO/IEC 23009-1 §5.5).
//!
//! Supports:
//! - `Period@xlink:href` remote element entities (one or more `Period`s)
//! - remote entities that are full MPD documents (their `Period` children are embedded)
//! - `urn:mpeg:dash:resolve-to-zero:2013` placeholders (element removed, no fetch)

use roxmltree::{Document, Node};
use thiserror::Error;
use url::Url;

use crate::http::{
    HttpRequest, HttpRequestKind, HttpRetryConfig, SharedHttpClient, send_with_retry,
};
use crate::manifest::merge_base_url;

const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const MPD_NS: &str = "urn:mpeg:dash:schema:mpd:2011";
const RESOLVE_TO_ZERO: &str = "urn:mpeg:dash:resolve-to-zero:2013";
const MAX_RESOLUTION_PASSES: usize = 5;

#[derive(Debug, Error)]
pub(crate) enum XlinkError {
    #[error("malformed MPD XML: {0}")]
    Malformed(String),
    #[error("invalid Period xlink:href URL: {0}")]
    InvalidHref(String),
    #[error("failed to fetch Period xlink:href {url}: {source}")]
    Fetch {
        url: String,
        #[source]
        source: crate::http::HttpError,
    },
    #[error("Period xlink:href {url} returned HTTP {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("remote xlink entity at {url} has no Period content")]
    EmptyRemote { url: String },
    #[error("remote xlink entity at {url} is not a valid Period or MPD document: {detail}")]
    InappropriateTarget { url: String, detail: String },
}

/// Resolve `Period@xlink:href` references in `mpd_xml`.
///
/// Both `onLoad` and `onRequest` Periods are resolved here so the returned document is ready
/// for timeline expansion and playback. Nested remote entities are resolved up to
/// [`MAX_RESOLUTION_PASSES`] times.
pub(crate) async fn resolve_period_xlinks(
    client: &SharedHttpClient,
    manifest_uri: &Url,
    mpd_xml: &str,
    http_retry: &HttpRetryConfig,
) -> Result<String, XlinkError> {
    let mut xml = mpd_xml.to_string();
    for _ in 0..MAX_RESOLUTION_PASSES {
        let (next, changed) =
            resolve_period_xlinks_once(client, manifest_uri, &xml, http_retry).await?;
        xml = next;
        if !changed {
            break;
        }
    }
    // Any remaining Period with onLoad xlink indicates a circular / unresolved remote entity:
    // strip xlink attributes per ISO/IEC 23009-1 §5.5.3 rule 3.
    strip_unresolved_onload_period_xlinks(&xml)
}

async fn resolve_period_xlinks_once(
    client: &SharedHttpClient,
    manifest_uri: &Url,
    mpd_xml: &str,
    http_retry: &HttpRetryConfig,
) -> Result<(String, bool), XlinkError> {
    let doc =
        Document::parse(mpd_xml).map_err(|e| XlinkError::Malformed(format!("MPD XML: {e}")))?;

    let targets: Vec<PeriodXlinkTarget> = doc
        .descendants()
        .filter(|n| n.is_element() && local_name(*n) == "Period")
        .filter_map(|n| period_xlink_target(n))
        .collect();

    if targets.is_empty() {
        return Ok((mpd_xml.to_string(), false));
    }

    // Apply replacements from the end so earlier byte ranges stay valid.
    let mut xml = mpd_xml.to_string();
    for target in targets.into_iter().rev() {
        let replacement = if is_resolve_to_zero(&target.href) {
            String::new()
        } else {
            fetch_period_replacement(client, manifest_uri, &target.href, http_retry).await?
        };
        xml = replace_range(&xml, target.range, &replacement);
    }
    Ok((xml, true))
}

#[derive(Debug)]
struct PeriodXlinkTarget {
    href: String,
    range: std::ops::Range<usize>,
}

fn period_xlink_target(node: Node<'_, '_>) -> Option<PeriodXlinkTarget> {
    let href = xlink_href(node)?.to_string();
    if href.is_empty() {
        return None;
    }
    Some(PeriodXlinkTarget {
        href,
        range: node.range(),
    })
}

fn xlink_href<'a, 'input>(node: Node<'a, 'input>) -> Option<&'a str> {
    node.attribute((XLINK_NS, "href"))
        .or_else(|| node.attribute("href"))
}

fn xlink_actuate<'a, 'input>(node: Node<'a, 'input>) -> Option<&'a str> {
    node.attribute((XLINK_NS, "actuate"))
        .or_else(|| node.attribute("actuate"))
}

fn is_resolve_to_zero(href: &str) -> bool {
    href.trim() == RESOLVE_TO_ZERO
}

fn local_name<'a, 'input>(node: Node<'a, 'input>) -> &'a str {
    node.tag_name().name()
}

async fn fetch_period_replacement(
    client: &SharedHttpClient,
    manifest_uri: &Url,
    href: &str,
    http_retry: &HttpRetryConfig,
) -> Result<String, XlinkError> {
    let url = resolve_xlink_url(manifest_uri, href)?;
    let resp = send_with_retry(
        client,
        HttpRequest::get(url.clone())
            .header("Accept", "application/dash+xml,video/vnd.mpeg.dash.mpd,*/*"),
        http_retry,
        HttpRequestKind::Xlink,
        false,
    )
    .await
    .map_err(|source| XlinkError::Fetch {
        url: url.to_string(),
        source,
    })?;
    if !resp.is_success() {
        return Err(XlinkError::HttpStatus {
            url: url.to_string(),
            status: resp.status(),
        });
    }
    let remote = resp.text().map_err(|source| XlinkError::Fetch {
        url: url.to_string(),
        source,
    })?;
    extract_period_xml_from_remote(&remote, &url)
}

fn resolve_xlink_url(manifest_uri: &Url, href: &str) -> Result<Url, XlinkError> {
    merge_base_url(manifest_uri, href).map_err(|e| XlinkError::InvalidHref(e.to_string()))
}

/// Turn a remote xlink entity into one or more serialized `Period` elements.
fn extract_period_xml_from_remote(remote_xml: &str, url: &Url) -> Result<String, XlinkError> {
    let body = skip_xml_preamble(remote_xml).trim();
    if body.is_empty() {
        return Err(XlinkError::EmptyRemote {
            url: url.to_string(),
        });
    }

    // Multiple top-level elements are allowed and are not themselves a well-formed XML document.
    let wrapped = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><wrapper xmlns="{MPD_NS}" xmlns:xlink="{XLINK_NS}">{body}</wrapper>"#
    );
    let doc = Document::parse(&wrapped).map_err(|e| XlinkError::InappropriateTarget {
        url: url.to_string(),
        detail: e.to_string(),
    })?;
    let wrapper = doc.root_element();

    let children: Vec<Node<'_, '_>> = wrapper.children().filter(|n| n.is_element()).collect();
    if children.is_empty() {
        return Err(XlinkError::EmptyRemote {
            url: url.to_string(),
        });
    }

    // Remote entity is a full MPD document: embed its Period children.
    if children.len() == 1 && local_name(children[0]) == "MPD" {
        let periods = serialize_period_children(children[0], &wrapped)?;
        if periods.is_empty() {
            return Err(XlinkError::EmptyRemote {
                url: url.to_string(),
            });
        }
        return Ok(periods);
    }

    if children.iter().all(|n| local_name(*n) == "Period") {
        let mut out = String::new();
        for child in children {
            out.push_str(node_slice(&wrapped, child));
        }
        return Ok(out);
    }

    Err(XlinkError::InappropriateTarget {
        url: url.to_string(),
        detail: "expected Period element(s) or an MPD document".into(),
    })
}

fn serialize_period_children(mpd: Node<'_, '_>, wrapped_xml: &str) -> Result<String, XlinkError> {
    let mut out = String::new();
    for child in mpd
        .children()
        .filter(|n| n.is_element() && local_name(*n) == "Period")
    {
        out.push_str(node_slice(wrapped_xml, child));
    }
    Ok(out)
}

fn node_slice<'a>(xml: &'a str, node: Node<'_, '_>) -> &'a str {
    &xml[node.range()]
}

fn replace_range(xml: &str, range: std::ops::Range<usize>, replacement: &str) -> String {
    let mut out = String::with_capacity(xml.len() - range.len() + replacement.len());
    out.push_str(&xml[..range.start]);
    out.push_str(replacement);
    out.push_str(&xml[range.end..]);
    out
}

fn skip_xml_preamble(input: &str) -> &str {
    let trimmed = input.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<?xml") {
        if let Some(end) = rest.find("?>") {
            return rest[end + 2..].trim_start();
        }
    }
    trimmed
}

fn strip_unresolved_onload_period_xlinks(mpd_xml: &str) -> Result<String, XlinkError> {
    let doc =
        Document::parse(mpd_xml).map_err(|e| XlinkError::Malformed(format!("MPD XML: {e}")))?;
    let mut removals: Vec<(std::ops::Range<usize>, String)> = Vec::new();

    for period in doc
        .descendants()
        .filter(|n| n.is_element() && local_name(*n) == "Period")
    {
        let Some(href) = xlink_href(period) else {
            continue;
        };
        if is_resolve_to_zero(href) {
            // Deferred resolve-to-zero that somehow survived (should not); remove the Period.
            removals.push((period.range(), String::new()));
            continue;
        }
        let actuate = xlink_actuate(period).unwrap_or("onRequest");
        if !actuate.eq_ignore_ascii_case("onLoad") {
            continue;
        }
        // Circular / unresolved onLoad: drop only the xlink attributes.
        let stripped = strip_xlink_attrs_from_open_tag(mpd_xml, period)?;
        removals.push((period.range(), stripped));
    }

    if removals.is_empty() {
        return Ok(mpd_xml.to_string());
    }

    let mut xml = mpd_xml.to_string();
    for (range, replacement) in removals.into_iter().rev() {
        xml = replace_range(&xml, range, &replacement);
    }
    Ok(xml)
}

fn strip_xlink_attrs_from_open_tag(xml: &str, period: Node<'_, '_>) -> Result<String, XlinkError> {
    let range = period.range();
    let element = &xml[range.clone()];
    let open_end = element
        .find('>')
        .ok_or_else(|| XlinkError::Malformed("Period open tag".into()))?;
    let open = &element[..=open_end];
    let rest = &element[open_end + 1..];

    let stripped_open = strip_xlink_attr_tokens(open);
    let mut out = String::with_capacity(element.len());
    out.push_str(&stripped_open);
    out.push_str(rest);
    Ok(out)
}

/// Remove `xlink:href` / `xlink:actuate` / `xlink:type` / `xlink:show` (and unprefixed
/// aliases) from a Period start tag.
fn strip_xlink_attr_tokens(open_tag: &str) -> String {
    const DROP: &[&str] = &["href", "actuate", "type", "show"];
    let mut out = String::with_capacity(open_tag.len());
    let bytes = open_tag.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if !bytes[i].is_ascii_whitespace() {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }

        // Preserve one space, then decide whether the following attribute is dropped.
        let ws_start = i;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' || (bytes[i] == b'/' && i + 1 < bytes.len()) {
            out.push_str(&open_tag[ws_start..i]);
            continue;
        }

        let name_start = i;
        while i < bytes.len()
            && bytes[i] != b'='
            && bytes[i] != b'>'
            && !bytes[i].is_ascii_whitespace()
        {
            i += 1;
        }
        let name = &open_tag[name_start..i];
        let local = name.rsplit_once(':').map_or(name, |(_, l)| l);
        let is_xlink_attr = name.starts_with("xlink:") || !name.contains(':');
        let drop = is_xlink_attr && DROP.contains(&local);

        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                while i < bytes.len()
                    && !bytes[i].is_ascii_whitespace()
                    && bytes[i] != b'>'
                    && bytes[i] != b'/'
                {
                    i += 1;
                }
            }
        }

        if drop {
            continue;
        }
        out.push(' ');
        out.push_str(&open_tag[name_start..i]);
    }

    out.replace(" >", ">").replace(" />", "/>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpClient, HttpError, HttpFuture, HttpResponse, shared};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockClient {
        responses: Mutex<HashMap<String, HttpResponse>>,
    }

    impl MockClient {
        fn with_response(self, url: &str, body: &str) -> Self {
            self.responses.lock().expect("lock").insert(
                url.to_string(),
                HttpResponse::new(200, vec![], bytes::Bytes::from(body.to_string())),
            );
            self
        }
    }

    impl HttpClient for MockClient {
        fn send<'a>(
            &'a self,
            request: HttpRequest,
        ) -> HttpFuture<'a, Result<HttpResponse, HttpError>> {
            let url = request.url.to_string();
            Box::pin(async move {
                self.responses
                    .lock()
                    .expect("lock")
                    .get(&url)
                    .cloned()
                    .ok_or_else(|| HttpError::Transport(format!("no mock for {url}")))
            })
        }
    }

    fn base() -> Url {
        Url::parse("https://example.com/live/manifest.mpd").unwrap()
    }

    #[tokio::test]
    async fn resolve_to_zero_removes_period() {
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" xmlns:xlink="http://www.w3.org/1999/xlink"
     type="static" mediaPresentationDuration="PT8S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period id="keep" duration="PT8S">
    <AdaptationSet mimeType="video/mp4">
      <Representation id="1" bandwidth="1000"/>
    </AdaptationSet>
  </Period>
  <Period id="gone" xlink:href="urn:mpeg:dash:resolve-to-zero:2013" xlink:actuate="onLoad"/>
</MPD>"#;
        let client = shared(MockClient::default());
        let out = resolve_period_xlinks(&client, &base(), xml, &HttpRetryConfig::disabled())
            .await
            .expect("resolve");
        let mpd = dash_mpd::parse(&out).expect("parse");
        assert_eq!(mpd.periods.len(), 1);
        assert_eq!(mpd.periods[0].id.as_deref(), Some("keep"));
    }

    #[tokio::test]
    async fn period_xlink_fetches_remote_period() {
        let remote = r#"<Period id="remote" duration="PT4S">
  <AdaptationSet mimeType="audio/mp4">
    <Representation id="a1" bandwidth="64000"/>
  </AdaptationSet>
</Period>"#;
        let client =
            shared(MockClient::default().with_response("https://example.com/live/ad.mpd", remote));
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" xmlns:xlink="http://www.w3.org/1999/xlink"
     type="static" mediaPresentationDuration="PT4S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period xlink:href="ad.mpd" xlink:actuate="onLoad"/>
</MPD>"#;
        let out = resolve_period_xlinks(&client, &base(), xml, &HttpRetryConfig::disabled())
            .await
            .expect("resolve");
        let mpd = dash_mpd::parse(&out).expect("parse");
        assert_eq!(mpd.periods.len(), 1);
        assert_eq!(mpd.periods[0].id.as_deref(), Some("remote"));
        assert!(mpd.periods[0].href.is_none());
        assert_eq!(mpd.periods[0].adaptations.len(), 1);
    }

    #[tokio::test]
    async fn period_xlink_expands_multiple_periods() {
        let remote = r#"<Period id="p1" duration="PT2S">
  <AdaptationSet mimeType="video/mp4"><Representation id="1" bandwidth="1"/></AdaptationSet>
</Period>
<Period id="p2" duration="PT2S">
  <AdaptationSet mimeType="video/mp4"><Representation id="2" bandwidth="2"/></AdaptationSet>
</Period>"#;
        let client = shared(
            MockClient::default().with_response("https://example.com/live/multi.mpd", remote),
        );
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" xmlns:xlink="http://www.w3.org/1999/xlink"
     type="static" mediaPresentationDuration="PT4S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period xlink:href="multi.mpd" xlink:actuate="onRequest"/>
</MPD>"#;
        let out = resolve_period_xlinks(&client, &base(), xml, &HttpRetryConfig::disabled())
            .await
            .expect("resolve");
        let mpd = dash_mpd::parse(&out).expect("parse");
        assert_eq!(mpd.periods.len(), 2);
        assert_eq!(mpd.periods[0].id.as_deref(), Some("p1"));
        assert_eq!(mpd.periods[1].id.as_deref(), Some("p2"));
    }

    #[tokio::test]
    async fn period_xlink_remote_mpd_document() {
        let remote = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" minBufferTime="PT1S"
     mediaPresentationDuration="PT4S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period id="from-mpd" duration="PT4S">
    <AdaptationSet mimeType="video/mp4"><Representation id="1" bandwidth="1"/></AdaptationSet>
  </Period>
</MPD>"#;
        let client =
            shared(MockClient::default().with_response("https://cdn.example/remote.mpd", remote));
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" xmlns:xlink="http://www.w3.org/1999/xlink"
     type="static" mediaPresentationDuration="PT4S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period xlink:href="https://cdn.example/remote.mpd" xlink:actuate="onLoad"/>
</MPD>"#;
        let out = resolve_period_xlinks(&client, &base(), xml, &HttpRetryConfig::disabled())
            .await
            .expect("resolve");
        let mpd = dash_mpd::parse(&out).expect("parse");
        assert_eq!(mpd.periods.len(), 1);
        assert_eq!(mpd.periods[0].id.as_deref(), Some("from-mpd"));
    }

    #[tokio::test]
    async fn nested_onload_xlink_is_resolved() {
        let inner = r#"<Period id="inner" duration="PT2S">
  <AdaptationSet mimeType="video/mp4"><Representation id="1" bandwidth="1"/></AdaptationSet>
</Period>"#;
        let outer = r#"<Period xlink:href="inner.mpd" xlink:actuate="onLoad"/>"#;
        let client = shared(
            MockClient::default()
                .with_response("https://example.com/live/outer.mpd", outer)
                .with_response("https://example.com/live/inner.mpd", inner),
        );
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" xmlns:xlink="http://www.w3.org/1999/xlink"
     type="static" mediaPresentationDuration="PT2S" minBufferTime="PT1S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period xlink:href="outer.mpd" xlink:actuate="onLoad"/>
</MPD>"#;
        let out = resolve_period_xlinks(&client, &base(), xml, &HttpRetryConfig::disabled())
            .await
            .expect("resolve");
        let mpd = dash_mpd::parse(&out).expect("parse");
        assert_eq!(mpd.periods.len(), 1);
        assert_eq!(mpd.periods[0].id.as_deref(), Some("inner"));
        assert!(mpd.periods[0].href.is_none());
    }
}
