//! CTA-5004 CMCD header encoding (header transmission mode).

use super::keys::{CmcdObjectType, CmcdRequestContext, CmcdStreamType};

/// Header names defined by CTA-5004 for header-mode transmission.
pub const CMCD_REQUEST: &str = "CMCD-Request";
pub const CMCD_OBJECT: &str = "CMCD-Object";
pub const CMCD_STATUS: &str = "CMCD-Status";
pub const CMCD_SESSION: &str = "CMCD-Session";

/// Encoded CMCD headers ready to attach to an [`HttpRequest`](crate::HttpRequest).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CmcdHeaders {
    pub request: Option<String>,
    pub object: Option<String>,
    pub status: Option<String>,
    pub session: Option<String>,
}

impl CmcdHeaders {
    /// Non-empty `(name, value)` pairs suitable for [`HttpRequest::header`](crate::HttpRequest::header).
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            (CMCD_REQUEST, self.request.as_deref()),
            (CMCD_OBJECT, self.object.as_deref()),
            (CMCD_STATUS, self.status.as_deref()),
            (CMCD_SESSION, self.session.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|v| (name, v)))
    }
}

/// Encode a CMCD request context into the four CTA-5004 headers.
///
/// Keys are omitted when unavailable. Within each header, keys are sequenced in
/// alphabetical order per CTA-5004 guidance.
pub fn encode_headers(ctx: &CmcdRequestContext) -> CmcdHeaders {
    CmcdHeaders {
        request: encode_request(ctx),
        object: encode_object(ctx),
        status: encode_status(ctx),
        session: encode_session(ctx),
    }
}

fn encode_request(ctx: &CmcdRequestContext) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(bl_ms) = ctx.buffer_length_ms {
        parts.push(format!("bl={bl_ms}"));
    }
    if let Some(mtp_kbps) = ctx.measured_throughput_kbps {
        let rounded = ((mtp_kbps as f64) / 100.0).round() as u64 * 100;
        parts.push(format!("mtp={rounded}"));
    }
    if let Some(ref nor) = ctx.next_object_request {
        parts.push(format!("nor={}", quote_string(nor)));
    }
    if let Some(ref nrr) = ctx.next_range_request {
        parts.push(format!("nrr={}", quote_string(nrr)));
    }
    if ctx.startup {
        parts.push("su".to_string());
    }
    join_parts(parts)
}

fn encode_object(ctx: &CmcdRequestContext) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(br_kbps) = ctx.encoded_bitrate_kbps {
        parts.push(format!("br={br_kbps}"));
    }
    if let Some(d_ms) = ctx.object_duration_ms {
        parts.push(format!("d={d_ms}"));
    }
    parts.push(format!("ot={}", ctx.object_type.as_token()));
    join_parts(parts)
}

fn encode_status(ctx: &CmcdRequestContext) -> Option<String> {
    let mut parts = Vec::new();
    if ctx.buffer_starvation {
        parts.push("bs".to_string());
    }
    join_parts(parts)
}

fn encode_session(ctx: &CmcdRequestContext) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(ref cid) = ctx.content_id {
        parts.push(format!("cid={}", quote_string(cid)));
    }
    parts.push("sf=d".to_string());
    parts.push(format!("sid={}", quote_string(&ctx.session_id)));
    parts.push(format!("st={}", ctx.stream_type.as_token()));
    join_parts(parts)
}

fn join_parts(parts: Vec<String>) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(","))
    }
}

/// Quote and escape a CMCD string value (CTA-5004 / RFC 8941 style).
pub fn quote_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

impl CmcdObjectType {
    pub(crate) fn as_token(self) -> &'static str {
        match self {
            Self::Manifest => "m",
            Self::Audio => "a",
            Self::Video => "v",
            Self::Muxed => "av",
            Self::Init => "i",
            Self::Caption => "c",
            Self::TimedText => "tt",
            Self::Key => "k",
            Self::Other => "o",
        }
    }
}

impl CmcdStreamType {
    pub(crate) fn as_token(self) -> &'static str {
        match self {
            Self::Vod => "v",
            Self::Live => "l",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmcd::keys::CmcdRequestContext;

    fn base_ctx() -> CmcdRequestContext {
        CmcdRequestContext {
            session_id: "6e2fb550-c457-11e9-bb97-0800200c9a66".into(),
            content_id: None,
            stream_type: CmcdStreamType::Vod,
            object_type: CmcdObjectType::Video,
            encoded_bitrate_kbps: None,
            object_duration_ms: None,
            buffer_length_ms: None,
            measured_throughput_kbps: None,
            startup: false,
            buffer_starvation: false,
            next_object_request: None,
            next_range_request: None,
        }
    }

    #[test]
    fn encodes_session_id_only() {
        let headers = encode_headers(&base_ctx());
        assert_eq!(headers.request, None);
        assert_eq!(headers.object.as_deref(), Some("ot=v"));
        assert_eq!(headers.status, None);
        assert_eq!(
            headers.session.as_deref(),
            Some(r#"sf=d,sid="6e2fb550-c457-11e9-bb97-0800200c9a66",st=v"#)
        );
    }

    #[test]
    fn encodes_cta_example_keys() {
        let mut ctx = base_ctx();
        ctx.content_id = Some("faec5fc2-ac30-11ea-bb37-0242ac130002".into());
        ctx.encoded_bitrate_kbps = Some(3200);
        ctx.object_duration_ms = Some(4004);
        ctx.buffer_length_ms = Some(21300);
        ctx.measured_throughput_kbps = Some(48100);
        ctx.startup = true;
        ctx.buffer_starvation = true;
        ctx.next_object_request = Some("../300kbps/track.m4v".into());
        ctx.next_range_request = Some("12323-48763".into());

        let headers = encode_headers(&ctx);
        assert_eq!(
            headers.request.as_deref(),
            Some(r#"bl=21300,mtp=48100,nor="../300kbps/track.m4v",nrr="12323-48763",su"#)
        );
        assert_eq!(headers.object.as_deref(), Some("br=3200,d=4004,ot=v"));
        assert_eq!(headers.status.as_deref(), Some("bs"));
        assert_eq!(
            headers.session.as_deref(),
            Some(
                r#"cid="faec5fc2-ac30-11ea-bb37-0242ac130002",sf=d,sid="6e2fb550-c457-11e9-bb97-0800200c9a66",st=v"#
            )
        );
    }

    #[test]
    fn rounds_mtp_to_nearest_100() {
        let mut ctx = base_ctx();
        ctx.measured_throughput_kbps = Some(25450);
        let headers = encode_headers(&ctx);
        assert!(
            headers
                .request
                .as_deref()
                .unwrap_or_default()
                .contains("mtp=25500")
        );
    }

    #[test]
    fn quotes_and_escapes_strings() {
        assert_eq!(quote_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
