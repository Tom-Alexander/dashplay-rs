//! DASH timed events from MPD `EventStream` and in-band `emsg` boxes.

use std::collections::HashSet;

use bytes::Bytes;
use dash_mpd::{AdaptationSet, Event, EventStream, InbandEventStream, Period, Representation};

/// Origin of a DASH timed media event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaEventSource {
    /// Declared in the MPD `EventStream` at Period level.
    Mpd,
    /// Carried in an ISO BMFF `emsg` box inside a media segment.
    InBand {
        /// DASH segment sequence number (`$Number$`).
        segment_number: u64,
        /// MPD timeline anchor for the segment (`$Time$`).
        segment_time: u64,
        /// 1-based subsegment index when `S@k` > 1.
        segment_sub_number: Option<u64>,
    },
}

/// Raw SCTE-35 splice information section bytes extracted from an event payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scte35Cue {
    pub binary: Bytes,
}

/// A DASH timed event (`EventStream` / `Event` or in-band `emsg`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaEvent {
    pub source: MediaEventSource,
    pub scheme_id_uri: String,
    pub value: Option<String>,
    pub timescale: u64,
    /// Presentation time in [`Self::timescale`] ticks on the MPD media timeline.
    pub presentation_time: u64,
    pub duration: Option<u64>,
    pub id: Option<u64>,
    pub message_data: Bytes,
}

impl MediaEvent {
    /// Returns `true` when the event scheme identifies SCTE-35 signalling.
    pub fn is_scte35(&self) -> bool {
        is_scte35_scheme(&self.scheme_id_uri)
    }

    /// Extracts base64-encoded SCTE-35 binary from MPD event content or raw message data.
    pub fn scte35_cue(&self) -> Option<Scte35Cue> {
        if !self.is_scte35() {
            return None;
        }
        decode_scte35_payload(&self.message_data)
    }
}

/// Collect MPD `EventStream` events for one Period.
pub(crate) fn mpd_events_for_period(period: &Period) -> Vec<MediaEvent> {
    let mut out = Vec::new();
    for stream in &period.event_streams {
        push_mpd_event_stream(&mut out, stream);
    }
    out
}

/// Effective in-band event stream descriptors for an adaptation set and representation.
pub(crate) fn inband_event_streams_for_representation(
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Vec<InbandFilter> {
    let mut filters = Vec::new();
    for stream in &adaptation_set.InbandEventStream {
        filters.push(InbandFilter::from_descriptor(stream));
    }
    for stream in &representation.InbandEventStream {
        filters.push(InbandFilter::from_descriptor(stream));
    }
    dedupe_inband_filters(filters)
}

/// Parse matching `emsg` boxes from a media segment payload.
pub(crate) fn inband_events_from_segment(
    data: &[u8],
    filters: &[InbandFilter],
    segment_number: u64,
    segment_time: u64,
    segment_sub_number: Option<u64>,
) -> Vec<MediaEvent> {
    let filter_active = !filters.is_empty();
    let mut out = Vec::new();
    for emsg in scan_emsg_boxes(data) {
        if filter_active && !filters.iter().any(|f| f.matches(&emsg)) {
            continue;
        }
        out.push(MediaEvent {
            source: MediaEventSource::InBand {
                segment_number,
                segment_time,
                segment_sub_number,
            },
            scheme_id_uri: emsg.scheme_id_uri,
            value: emsg.value,
            timescale: emsg.timescale,
            presentation_time: emsg.presentation_time,
            duration: emsg.event_duration,
            id: emsg.id,
            message_data: Bytes::copy_from_slice(&emsg.message_data),
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InbandFilter {
    scheme_id_uri: String,
    value: Option<String>,
}

impl InbandFilter {
    fn from_descriptor(stream: &InbandEventStream) -> Self {
        Self {
            scheme_id_uri: stream.schemeIdUri.clone(),
            value: stream.value.clone(),
        }
    }

    fn matches(&self, emsg: &ParsedEmsg) -> bool {
        if self.scheme_id_uri != emsg.scheme_id_uri {
            return false;
        }
        match (&self.value, &emsg.value) {
            (None, _) | (_, None) => true,
            (Some(expected), Some(actual)) => expected == actual,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedEmsg {
    scheme_id_uri: String,
    value: Option<String>,
    timescale: u64,
    presentation_time: u64,
    event_duration: Option<u64>,
    id: Option<u64>,
    message_data: Vec<u8>,
}

fn push_mpd_event_stream(out: &mut Vec<MediaEvent>, stream: &EventStream) {
    let stream_timescale = stream.timescale.unwrap_or(1).max(1);
    let stream_offset = stream.presentationTimeOffset.unwrap_or(0);

    for event in &stream.event {
        let presentation_time = mpd_event_presentation_time(event, stream_offset);

        let duration = event.duration.filter(|d| *d > 0);
        let timescale = event.timescale.unwrap_or(stream_timescale).max(1);
        let message_data = mpd_event_message_data(event);

        out.push(MediaEvent {
            source: MediaEventSource::Mpd,
            scheme_id_uri: stream.schemeIdUri.clone(),
            value: stream.value.clone().or_else(|| event.value.clone()),
            timescale,
            presentation_time,
            duration,
            id: event.id.as_ref().and_then(|s| s.parse().ok()),
            message_data,
        });
    }
}

fn mpd_event_presentation_time(event: &Event, stream_offset: u64) -> u64 {
    let event_time = event.presentationTime.unwrap_or(0);
    let event_offset = event.presentationTimeOffset.unwrap_or(stream_offset);
    event_time.saturating_sub(event_offset)
}

fn mpd_event_message_data(event: &Event) -> Bytes {
    if let Some(data) = event.messageData.as_deref() {
        return Bytes::copy_from_slice(data.as_bytes());
    }
    if let Some(content) = event
        .content
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if event.contentEncoding.as_deref() == Some("base64") || is_scte35_base64_content(content) {
            if let Ok(decoded) =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, content)
            {
                return Bytes::from(decoded);
            }
        }
        return Bytes::copy_from_slice(content.as_bytes());
    }
    Bytes::new()
}

fn is_scte35_base64_content(content: &str) -> bool {
    content.starts_with("/DA") || content.starts_with("/DB") || content.starts_with("/DC")
}

fn decode_scte35_payload(data: &[u8]) -> Option<Scte35Cue> {
    if data.is_empty() {
        return None;
    }
    if data.starts_with(b"/D") || data.starts_with(b"/B") {
        return base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .ok()
            .map(Bytes::from)
            .map(|binary| Scte35Cue { binary });
    }
    Some(Scte35Cue {
        binary: Bytes::copy_from_slice(data),
    })
}

fn is_scte35_scheme(scheme: &str) -> bool {
    scheme.contains("scte35") || scheme.contains("scte:scte35")
}

fn dedupe_inband_filters(filters: Vec<InbandFilter>) -> Vec<InbandFilter> {
    let mut seen = HashSet::new();
    filters
        .into_iter()
        .filter(|f| seen.insert((f.scheme_id_uri.clone(), f.value.clone())))
        .collect()
}

fn scan_emsg_boxes(data: &[u8]) -> Vec<ParsedEmsg> {
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
        if &data[offset + 4..offset + 8] == b"emsg" {
            if let Some(parsed) = parse_emsg_box(&data[offset + header_len..box_end]) {
                out.push(parsed);
            }
        }
        offset = box_end;
    }
    out
}

fn read_box_size(data: &[u8], offset: usize) -> Option<(usize, usize)> {
    if offset + 8 > data.len() {
        return None;
    }
    let size32 = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?);
    if size32 == 1 {
        if offset + 16 > data.len() {
            return None;
        }
        let size64 = u64::from_be_bytes(data[offset + 8..offset + 16].try_into().ok()?);
        let size = usize::try_from(size64).ok()?;
        Some((size, 16))
    } else {
        let size = if size32 == 0 {
            data.len() - offset
        } else {
            usize::try_from(size32).ok()?
        };
        Some((size, 8))
    }
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
    use dash_mpd::{Event, EventStream, InbandEventStream};

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
    fn mpd_event_stream_collects_scte35_binary() {
        let period = Period {
            event_streams: vec![EventStream {
                schemeIdUri: "urn:scte:scte35:2014:xml+bin".into(),
                timescale: Some(1),
                event: vec![Event {
                    id: Some("42".into()),
                    presentationTime: Some(100),
                    duration: Some(30),
                    content: Some("/DAhAAAAAAAAAP/wEAUAAAfPf+9/fgAg9YDAAAAAAAA/APOv".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let events = mpd_events_for_period(&period);
        assert_eq!(events.len(), 1);
        assert!(events[0].is_scte35());
        assert!(events[0].scte35_cue().is_some());
        assert_eq!(events[0].presentation_time, 100);
        assert_eq!(events[0].duration, Some(30));
        assert_eq!(events[0].id, Some(42));
    }

    #[test]
    fn inband_emsg_respects_descriptor_filter() {
        let emsg = build_emsg_v0("urn:mpeg:dash:event:2012", "1", 1, 500, 0, 7, b"hello");
        let filters = vec![InbandFilter {
            scheme_id_uri: "urn:mpeg:dash:event:2012".into(),
            value: Some("1".into()),
        }];
        let events = inband_events_from_segment(&emsg, &filters, 3, 9000, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].presentation_time, 500);
        assert_eq!(events[0].message_data.as_ref(), b"hello");
        assert!(matches!(
            events[0].source,
            MediaEventSource::InBand {
                segment_number: 3,
                segment_time: 9000,
                segment_sub_number: None,
            }
        ));

        let other = vec![InbandFilter {
            scheme_id_uri: "urn:other".into(),
            value: None,
        }];
        assert!(inband_events_from_segment(&emsg, &other, 1, 0, None).is_empty());
    }

    #[test]
    fn inband_emsg_emits_all_when_no_descriptors() {
        let emsg = build_emsg_v0("urn:test", "", 1, 0, 0, 0, b"x");
        let events = inband_events_from_segment(&emsg, &[], 1, 0, None);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn inband_event_streams_merge_adaptation_set_and_representation() {
        let aset = AdaptationSet {
            InbandEventStream: vec![InbandEventStream {
                schemeIdUri: "urn:mpeg:dash:event:2012".into(),
                value: Some("1".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rep = Representation {
            InbandEventStream: vec![InbandEventStream {
                schemeIdUri: "urn:scte:scte35:2014:xml+bin".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let filters = inband_event_streams_for_representation(&aset, &rep);
        assert_eq!(filters.len(), 2);
    }
}
