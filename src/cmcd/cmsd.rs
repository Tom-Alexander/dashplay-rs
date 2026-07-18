//! CTA-5006 CMSD response header parsing.

/// Parsed value from a CMSD key/value pair.
#[derive(Debug, Clone, PartialEq)]
pub enum CmsdValue {
    /// Boolean true (key present without `=value`).
    True,
    /// Integer (signed).
    Integer(i64),
    /// Decimal / floating value.
    Decimal(f64),
    /// Quoted string or bare token.
    String(String),
}

/// One hop / server entry from `CMSD-Dynamic` (list item + parameters).
#[derive(Debug, Clone, PartialEq)]
pub struct CmsdHop {
    /// Server identifier (`n`) when present, otherwise the bare list item token.
    pub name: Option<String>,
    /// Parameters attached to this hop (`etp`, `rtt`, …).
    pub params: Vec<(String, CmsdValue)>,
}

/// Parsed CMSD-Static and CMSD-Dynamic data from one HTTP response.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CmsdSnapshot {
    /// Keys from the `CMSD-Static` dictionary header.
    pub static_keys: Vec<(String, CmsdValue)>,
    /// Hops from the `CMSD-Dynamic` list header (oldest/first to newest).
    pub dynamic_hops: Vec<CmsdHop>,
}

impl CmsdSnapshot {
    /// Whether any CMSD data was present.
    pub fn is_empty(&self) -> bool {
        self.static_keys.is_empty() && self.dynamic_hops.is_empty()
    }
}

/// Parse `CMSD-Static` / `CMSD-Dynamic` response headers when present.
pub fn parse_cmsd_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<CmsdSnapshot> {
    let mut snapshot = CmsdSnapshot::default();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("CMSD-Static") {
            snapshot.static_keys = parse_static(value);
        } else if name.eq_ignore_ascii_case("CMSD-Dynamic") {
            snapshot.dynamic_hops = parse_dynamic(value);
        }
    }
    if snapshot.is_empty() {
        None
    } else {
        Some(snapshot)
    }
}

fn parse_static(input: &str) -> Vec<(String, CmsdValue)> {
    split_top_level(input, ',')
        .into_iter()
        .filter_map(|member| parse_key_value(member.trim()))
        .collect()
}

fn parse_dynamic(input: &str) -> Vec<CmsdHop> {
    split_top_level(input, ',')
        .into_iter()
        .filter_map(|item| parse_hop(item.trim()))
        .collect()
}

fn parse_hop(item: &str) -> Option<CmsdHop> {
    if item.is_empty() {
        return None;
    }
    let parts = split_top_level(item, ';');
    let mut iter = parts.into_iter();
    let first = iter.next()?.trim();
    let (name, mut params) = match parse_key_value(first) {
        Some((k, v)) if k == "n" => (Some(value_as_string(v)), Vec::new()),
        Some((k, v)) => (None, vec![(k, v)]),
        None => {
            // Bare token list item (server id without `n=`).
            if first.is_empty() {
                return None;
            }
            (Some(unquote_token(first)), Vec::new())
        }
    };
    for part in iter {
        if let Some(kv) = parse_key_value(part.trim()) {
            if kv.0 == "n" && name.is_none() {
                // Prefer explicit n= if we somehow got here second.
            }
            params.push(kv);
        }
    }
    // If first param was n, already handled; also lift n from params if needed.
    let (name, params) = if name.is_some() {
        (name, params)
    } else if let Some(idx) = params.iter().position(|(k, _)| k == "n") {
        let n = value_as_string(params.remove(idx).1);
        (Some(n), params)
    } else {
        (name, params)
    };
    Some(CmsdHop { name, params })
}

fn parse_key_value(input: &str) -> Option<(String, CmsdValue)> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Some((key, raw)) = input.split_once('=') {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        Some((key.to_string(), parse_value(raw.trim())))
    } else {
        // Boolean true
        Some((input.to_string(), CmsdValue::True))
    }
}

fn parse_value(raw: &str) -> CmsdValue {
    if raw.starts_with('"') {
        return CmsdValue::String(unquote_string(raw));
    }
    if let Ok(i) = raw.parse::<i64>() {
        return CmsdValue::Integer(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return CmsdValue::Decimal(f);
    }
    CmsdValue::String(raw.to_string())
}

fn value_as_string(value: CmsdValue) -> String {
    match value {
        CmsdValue::String(s) => s,
        CmsdValue::Integer(i) => i.to_string(),
        CmsdValue::Decimal(f) => f.to_string(),
        CmsdValue::True => "true".into(),
    }
}

fn unquote_token(raw: &str) -> String {
    if raw.starts_with('"') {
        unquote_string(raw)
    } else {
        raw.to_string()
    }
}

fn unquote_string(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return raw.to_string();
    }
    let mut out = String::new();
    let mut chars = raw[1..raw.len() - 1].chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Split on `delim` ignoring delimiters inside quoted strings.
fn split_top_level(input: &str, delim: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut escaped = false;
    for (idx, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            c if c == delim && !in_quotes => {
                parts.push(&input[start..idx]);
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_static_dictionary() {
        let snap =
            parse_cmsd_headers([("CMSD-Static", r#"ot=v,sf=h,st=v,n="OriginReels",d=4004"#)])
                .expect("cmsd");
        assert_eq!(snap.static_keys.len(), 5);
        assert_eq!(
            snap.static_keys
                .iter()
                .find(|(k, _)| k == "ot")
                .map(|(_, v)| v),
            Some(&CmsdValue::String("v".into()))
        );
        assert_eq!(
            snap.static_keys
                .iter()
                .find(|(k, _)| k == "d")
                .map(|(_, v)| v),
            Some(&CmsdValue::Integer(4004))
        );
    }

    #[test]
    fn parses_multi_hop_dynamic() {
        let snap = parse_cmsd_headers([(
            "CMSD-Dynamic",
            r#"n="CDN1-A";etp=76;rtt=32,n="CDN1-B";etp=96;rtt=8"#,
        )])
        .expect("cmsd");
        assert_eq!(snap.dynamic_hops.len(), 2);
        assert_eq!(snap.dynamic_hops[0].name.as_deref(), Some("CDN1-A"));
        assert_eq!(
            snap.dynamic_hops[0].params,
            vec![
                ("etp".into(), CmsdValue::Integer(76)),
                ("rtt".into(), CmsdValue::Integer(32)),
            ]
        );
        assert_eq!(snap.dynamic_hops[1].name.as_deref(), Some("CDN1-B"));
    }

    #[test]
    fn returns_none_when_absent() {
        assert!(parse_cmsd_headers([("Content-Type", "video/mp4")]).is_none());
    }
}
