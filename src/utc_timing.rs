//! Clock synchronization via MPD `UTCTiming` (ISO/IEC 23009-1 §5.8.4.11, §5.8.5.7).
//!
//! Implements the normative `schemeIdUri` values under `urn:mpeg:dash:utc:*` used for
//! `UTCTiming@schemeIdUri` / `@value`, in manifest order (first is highest preference).
//! Unsupported or failing sources are skipped; if none succeed, falls back to `Utc::now()`.

use std::net::UdpSocket;
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use dash_mpd::{MPD, UTCTiming};
use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest::Client;
use url::Url;

const NTP_UNIX_OFFSET: i64 = 2_208_988_800;

/// Resolve wall-clock UTC using MPD `UTCTiming` entries (and nested `ProducerReferenceTime`
/// timing), then PRT chain. Falls back to the local clock when no entry succeeds.
pub(crate) async fn wall_clock_utc(
    client: &Client,
    mpd: &MPD,
    manifest_uri: Option<&Url>,
) -> DateTime<Utc> {
    for u in utc_timing_chain(mpd) {
        if let Some(t) = resolve_utc_timing(client, u, manifest_uri).await {
            return t;
        }
    }
    Utc::now()
}

fn utc_timing_chain(mpd: &MPD) -> Vec<&UTCTiming> {
    let mut out: Vec<&UTCTiming> = mpd.UTCTiming.iter().collect();
    for period in &mpd.periods {
        for ad in &period.adaptations {
            for prt in &ad.ProducerReferenceTime {
                if let Some(ref u) = prt.UTCTiming {
                    out.push(u);
                }
            }
            for rep in &ad.representations {
                for prt in &rep.ProducerReferenceTime {
                    if let Some(ref u) = prt.UTCTiming {
                        out.push(u);
                    }
                }
            }
        }
    }
    out
}

fn scheme_base(scheme_id_uri: &str) -> Option<&str> {
    let u = scheme_id_uri.trim();
    let u = u.strip_prefix("urn:mpeg:dash:utc:")?;
    let u = u.strip_prefix("UTC:").unwrap_or(u);
    Some(trim_trailing_year_suffix(u))
}

fn trim_trailing_year_suffix(s: &str) -> &str {
    if let Some((head, tail)) = s.rsplit_once(':') {
        if tail.len() == 4 && tail.chars().all(|c| c.is_ascii_digit()) {
            return head;
        }
    }
    s
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UtcScheme {
    Direct,
    HttpIso,
    HttpXsDate,
    HttpHead,
    Head,
    HttpNtp,
    Ntp,
    NtpServer,
    Sntp,
    WebSocket,
}

fn classify_scheme(scheme_id_uri: &str) -> Option<UtcScheme> {
    let base = scheme_base(scheme_id_uri)?.to_ascii_lowercase();
    Some(match base.as_str() {
        "direct" => UtcScheme::Direct,
        "http-iso" => UtcScheme::HttpIso,
        "http-xsdate" => UtcScheme::HttpXsDate,
        "http-head" => UtcScheme::HttpHead,
        "head" => UtcScheme::Head,
        "http-ntp" => UtcScheme::HttpNtp,
        "ntp" => UtcScheme::Ntp,
        "ntp-server" => UtcScheme::NtpServer,
        "sntp" => UtcScheme::Sntp,
        "websocket" => UtcScheme::WebSocket,
        _ => return None,
    })
}

async fn resolve_utc_timing(
    client: &Client,
    u: &UTCTiming,
    manifest_uri: Option<&Url>,
) -> Option<DateTime<Utc>> {
    let kind = classify_scheme(&u.schemeIdUri)?;
    let value = u
        .value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;

    match kind {
        UtcScheme::Direct => parse_xs_datetime_loose(value),
        UtcScheme::HttpIso | UtcScheme::HttpXsDate => {
            let url = resolve_http_url(manifest_uri, value)?;
            http_datetime_body(client, &url, kind == UtcScheme::HttpXsDate).await
        }
        UtcScheme::HttpHead | UtcScheme::Head => {
            let url = resolve_http_url(manifest_uri, value)?;
            http_date_header(client, &url).await
        }
        UtcScheme::HttpNtp => {
            let url = resolve_http_url(manifest_uri, value)?;
            http_ntp_body(client, &url).await
        }
        UtcScheme::Ntp | UtcScheme::NtpServer | UtcScheme::Sntp => {
            let (host, port) = parse_host_port(value, 123)?;
            (tokio::task::spawn_blocking(move || sntp_query(&host, port)).await).unwrap_or_default()
        }
        UtcScheme::WebSocket => None,
    }
}

fn resolve_http_url(manifest_uri: Option<&Url>, value: &str) -> Option<String> {
    let v = value.trim();
    if v.starts_with("http://") || v.starts_with("https://") {
        return Some(v.to_string());
    }
    if v.starts_with("//") {
        return Some(format!("https:{v}"));
    }
    manifest_uri
        .and_then(|m| m.join(v).ok())
        .map(|u| u.to_string())
}

fn parse_host_port(s: &str, default_port: u16) -> Option<(String, u16)> {
    let s = s.trim();
    if let Some((host, port_s)) = s.rsplit_once(':') {
        if !host.contains(']') && port_s.parse::<u16>().is_ok() {
            let port: u16 = port_s.parse().ok()?;
            return Some((host.to_string(), port));
        }
    }
    Some((s.to_string(), default_port))
}

fn parse_xs_datetime_loose(s: &str) -> Option<DateTime<Utc>> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Some(dt.with_timezone(&Utc));
    }
    let fmts = [
        "%Y-%m-%dT%H:%M:%S%.fZ",
        "%Y-%m-%dT%H:%M:%SZ",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ];
    for f in fmts {
        if let Ok(n) = NaiveDateTime::parse_from_str(t, f) {
            return Some(n.and_utc());
        }
    }
    if let Ok(n) = NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S") {
        return Some(n.and_utc());
    }
    None
}

fn ntp_be_timestamp_to_utc(seconds: u32, fraction: u32) -> Option<DateTime<Utc>> {
    let unix_secs = (seconds as i64).checked_sub(NTP_UNIX_OFFSET)?;
    let nanos = (((fraction as u64) * 1_000_000_000) >> 32) as u32;
    Utc.timestamp_opt(unix_secs, nanos).single()
}

fn parse_http_ntp_bytes(body: &[u8]) -> Option<DateTime<Utc>> {
    if let Ok(s) = std::str::from_utf8(body) {
        let t = s.trim();
        if let Some(dt) = parse_xs_datetime_loose(t) {
            return Some(dt);
        }
        if let Ok(f) = t.parse::<f64>() {
            let secs = f.floor() as i64;
            let nanos = ((f - f.floor()) * 1e9).round() as u32;
            return Utc.timestamp_opt(secs, nanos).single();
        }
    }
    if body.len() >= 8 {
        let sec = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
        let frac = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
        return ntp_be_timestamp_to_utc(sec, frac);
    }
    None
}

async fn http_ntp_body(client: &Client, url: &str) -> Option<DateTime<Utc>> {
    let bytes = client.get(url).send().await.ok()?.bytes().await.ok()?;
    parse_http_ntp_bytes(&bytes)
}

async fn http_datetime_body(client: &Client, url: &str, xsdate: bool) -> Option<DateTime<Utc>> {
    let text = client.get(url).send().await.ok()?.text().await.ok()?;
    let t = text.trim();
    if !xsdate {
        return parse_xs_datetime_loose(t);
    }
    parse_xs_datetime_loose(t).or_else(|| first_datetime_in_xml_or_text(&text))
}

fn first_datetime_in_xml_or_text(xml: &str) -> Option<DateTime<Utc>> {
    if let Some(dt) = parse_xs_datetime_loose(xml) {
        return Some(dt);
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf).ok()? {
            Event::Text(t) => {
                let s = t.decode().ok()?.into_owned();
                if let Some(dt) = parse_xs_datetime_loose(&s) {
                    return Some(dt);
                }
            }
            Event::CData(c) => {
                let s = c.decode().ok()?.into_owned();
                if let Some(dt) = parse_xs_datetime_loose(&s) {
                    return Some(dt);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

async fn http_date_header(client: &Client, url: &str) -> Option<DateTime<Utc>> {
    let mut resp = client.head(url).send().await.ok()?;
    if !resp.status().is_success() || resp.headers().get(reqwest::header::DATE).is_none() {
        resp = client.get(url).send().await.ok()?;
    }
    let date_hdr = resp.headers().get(reqwest::header::DATE)?.to_str().ok()?;
    parse_http_date(date_hdr)
}

fn parse_http_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    DateTime::parse_from_rfc2822(s)
        .map(|d| d.with_timezone(&Utc))
        .ok()
}

fn sntp_query(host: &str, port: u16) -> Option<DateTime<Utc>> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let mut packet = [0u8; 48];
    packet[0] = 0x1b;
    let dest = format!("{host}:{port}");
    socket.send_to(&packet, &dest).ok()?;
    let mut buf = [0u8; 48];
    let n = socket.recv(&mut buf).ok()?;
    if n < 48 {
        return None;
    }
    let seconds = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]);
    let fraction = u32::from_be_bytes([buf[44], buf[45], buf[46], buf[47]]);
    ntp_be_timestamp_to_utc(seconds, fraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_base_trims_year() {
        assert_eq!(
            scheme_base("urn:mpeg:dash:utc:http-iso:2014"),
            Some("http-iso")
        );
        assert_eq!(scheme_base("urn:mpeg:dash:utc:direct:2012"), Some("direct"));
    }

    #[test]
    fn parse_direct_datetime() {
        let t = parse_xs_datetime_loose("2020-05-01T12:00:00Z").unwrap();
        assert_eq!(t.timestamp(), 1_588_334_400);
    }

    #[test]
    fn ntp_bytes_to_utc() {
        let unix = 1_000_000_000i64;
        let ntp_sec = (unix + NTP_UNIX_OFFSET) as u32;
        let dt = ntp_be_timestamp_to_utc(ntp_sec, 0).unwrap();
        assert_eq!(dt.timestamp(), unix);
    }

    #[test]
    fn parse_http_ntp_float_body() {
        let b = b"1234567890.5";
        let dt = parse_http_ntp_bytes(b).unwrap();
        assert_eq!(dt.timestamp(), 1_234_567_890);
    }
}
