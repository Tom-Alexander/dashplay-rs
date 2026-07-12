//! ISO BMFF `emsg` (Event Message) box parsing.

use super::{box_type_at, read_box_size};

/// Parsed `emsg` box payload (ISO/IEC 23009-1 §5.10.3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedEmsg {
    pub scheme_id_uri: String,
    pub value: Option<String>,
    pub timescale: u64,
    pub presentation_time: u64,
    pub event_duration: Option<u64>,
    pub id: Option<u64>,
    pub message_data: Vec<u8>,
}

/// Scan `data` for all top-level `emsg` boxes.
pub(crate) fn scan_emsg_boxes(data: &[u8]) -> Vec<ParsedEmsg> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 8 <= data.len() {
        let Some((box_size, header_len)) = read_box_size(data, offset) else {
            break;
        };
        if box_size < header_len || offset + box_size > data.len() {
            offset = offset.saturating_add(1);
            continue;
        }
        let box_end = offset + box_size;
        if box_type_at(data, offset, header_len) == Some(*b"emsg") {
            if let Some(parsed) = parse_emsg_box(&data[offset + header_len..box_end]) {
                out.push(parsed);
            }
        }
        offset = box_end;
    }
    out
}

fn parse_emsg_box(payload: &[u8]) -> Option<ParsedEmsg> {
    if payload.len() < 4 {
        return None;
    }
    let version = payload[0];
    match version {
        0 => parse_emsg_v0(payload),
        1 => parse_emsg_v1(payload),
        _ => None,
    }
}

fn parse_emsg_v0(payload: &[u8]) -> Option<ParsedEmsg> {
    let mut offset = 4usize;
    let (scheme_id_uri, next) = read_c_string(payload, offset)?;
    offset = next;
    let (value, next) = read_c_string(payload, offset)?;
    offset = next;
    if offset + 16 > payload.len() {
        return None;
    }
    let timescale = u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let presentation_time_delta = u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let event_duration = u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let id = u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let message_data = payload.get(offset..)?.to_vec();
    Some(ParsedEmsg {
        scheme_id_uri,
        value: if value.is_empty() { None } else { Some(value) },
        timescale: u64::from(timescale.max(1)),
        presentation_time: u64::from(presentation_time_delta),
        event_duration: (event_duration > 0).then_some(u64::from(event_duration)),
        id: (id > 0).then_some(u64::from(id)),
        message_data,
    })
}

fn parse_emsg_v1(payload: &[u8]) -> Option<ParsedEmsg> {
    let mut offset = 4usize;
    if offset + 20 > payload.len() {
        return None;
    }
    let timescale = u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let presentation_time = u64::from_be_bytes(payload[offset..offset + 8].try_into().ok()?);
    offset += 8;
    let event_duration = u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let id = u32::from_be_bytes(payload[offset..offset + 4].try_into().ok()?);
    offset += 4;
    let (scheme_id_uri, next) = read_c_string(payload, offset)?;
    offset = next;
    let (value, next) = read_c_string(payload, offset)?;
    offset = next;
    let message_data = payload.get(offset..)?.to_vec();
    Some(ParsedEmsg {
        scheme_id_uri,
        value: if value.is_empty() { None } else { Some(value) },
        timescale: u64::from(timescale.max(1)),
        presentation_time,
        event_duration: (event_duration > 0).then_some(u64::from(event_duration)),
        id: (id > 0).then_some(u64::from(id)),
        message_data,
    })
}

fn read_c_string(data: &[u8], mut offset: usize) -> Option<(String, usize)> {
    let start = offset;
    while offset < data.len() {
        if data[offset] == 0 {
            let s = std::str::from_utf8(&data[start..offset]).ok()?.to_string();
            return Some((s, offset + 1));
        }
        offset += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_emsg_v0(
        scheme: &str,
        value: &str,
        timescale: u32,
        delta: u32,
        duration: u32,
        id: u32,
        message: &[u8],
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8, 0, 0, 0]);
        payload.extend_from_slice(scheme.as_bytes());
        payload.push(0);
        payload.extend_from_slice(value.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&timescale.to_be_bytes());
        payload.extend_from_slice(&delta.to_be_bytes());
        payload.extend_from_slice(&duration.to_be_bytes());
        payload.extend_from_slice(&id.to_be_bytes());
        payload.extend_from_slice(message);

        let mut out = Vec::new();
        let box_size = (8 + payload.len()) as u32;
        out.extend_from_slice(&box_size.to_be_bytes());
        out.extend_from_slice(b"emsg");
        out.extend_from_slice(&payload);
        out
    }

    #[test]
    fn scan_emsg_v0_box() {
        let emsg = build_emsg_v0("urn:mpeg:dash:event:2012", "1", 1, 500, 0, 7, b"hello");
        let parsed = scan_emsg_boxes(&emsg);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].scheme_id_uri, "urn:mpeg:dash:event:2012");
        assert_eq!(parsed[0].value.as_deref(), Some("1"));
        assert_eq!(parsed[0].presentation_time, 500);
        assert_eq!(parsed[0].message_data, b"hello");
    }
}
