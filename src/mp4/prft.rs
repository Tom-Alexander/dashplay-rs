//! ISO BMFF `prft` (Producer Reference Time) parsing for in-band clock correction.
//!
//! See ISO/IEC 14496-12 §8.16.5 and DASH-IF low-latency §9.X.4.3 (v) for
//! `ProducerReferenceTime@inband=true` re-verification.

use std::sync::Mutex;

use dash_mpd::{AdaptationSet, Period, Representation};

use crate::clock::{resync, utc_timing};
use crate::manifest;

use super::{box_type_at, read_box_size};
use resync::ProducerReferenceAnchor;

/// Parsed `prft` box payload (ISO/IEC 14496-12 §8.16.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedPrft {
    pub reference_track_id: u32,
    pub ntp_timestamp: u64,
    pub media_time: u64,
}

/// Scan `data` for all `prft` boxes (walks into ISO BMFF container boxes such as `moof`).
pub(crate) fn scan_prft_boxes(data: &[u8]) -> Vec<ParsedPrft> {
    let mut out = Vec::new();
    scan_prft_boxes_at(data, &mut out);
    out
}

fn scan_prft_boxes_at(data: &[u8], out: &mut Vec<ParsedPrft>) {
    let mut offset = 0usize;
    while offset + 8 <= data.len() {
        let Some((box_size, header_len)) = read_box_size(data, offset) else {
            break;
        };
        if box_size < header_len || offset + box_size > data.len() {
            break;
        }
        let box_end = offset + box_size;
        if box_type_at(data, offset, header_len) == Some(*b"prft") {
            if let Some(parsed) = parse_prft_box(&data[offset + header_len..box_end]) {
                out.push(parsed);
            }
        } else if is_prft_container_box(data, offset, header_len) {
            scan_prft_boxes_at(&data[offset + header_len..box_end], out);
        }
        offset = box_end;
    }
}

fn is_prft_container_box(data: &[u8], offset: usize, header_len: usize) -> bool {
    let Some(ty) = box_type_at(data, offset, header_len) else {
        return false;
    };
    ty == *b"moof" || ty == *b"traf" || ty == *b"moov" || ty == *b"trak" || ty == *b"mdia"
}

fn parse_prft_box(payload: &[u8]) -> Option<ParsedPrft> {
    if payload.len() < 4 {
        return None;
    }
    let version = payload[0];
    let body = &payload[4..];
    match version {
        0 if body.len() >= 16 => Some(ParsedPrft {
            reference_track_id: u32::from_be_bytes(body[0..4].try_into().ok()?),
            ntp_timestamp: u64::from_be_bytes(body[4..12].try_into().ok()?),
            media_time: u64::from(u32::from_be_bytes(body[12..16].try_into().ok()?)),
        }),
        1 if body.len() >= 20 => Some(ParsedPrft {
            reference_track_id: u32::from_be_bytes(body[0..4].try_into().ok()?),
            ntp_timestamp: u64::from_be_bytes(body[4..12].try_into().ok()?),
            media_time: u64::from_be_bytes(body[12..20].try_into().ok()?),
        }),
        _ => None,
    }
}

/// Build a [`ProducerReferenceAnchor`] from a parsed `prft` and representation timescale/PTO.
pub(crate) fn anchor_from_prft(
    prft: ParsedPrft,
    timescale: u64,
    presentation_time_offset: u64,
) -> Option<ProducerReferenceAnchor> {
    let wall_clock_time = utc_timing::ntp_u64_to_utc(prft.ntp_timestamp)?;
    let pta_ticks = prft.media_time.saturating_sub(presentation_time_offset);
    Some(ProducerReferenceAnchor {
        wall_clock_time,
        pta_ticks,
        timescale: timescale.max(1),
    })
}

/// Update stored in-band anchor when `ProducerReferenceTime@inband=true` and a `prft` is present.
pub(crate) fn maybe_update_inband_anchor_from_segment(
    data: &[u8],
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
    reference_id: Option<&str>,
    store: &Mutex<Option<ProducerReferenceAnchor>>,
) {
    if !resync::producer_reference_inband_enabled(adaptation_set, representation, reference_id) {
        return;
    }
    let Ok(addressing) =
        manifest::segment_addressing_for_representation(period, adaptation_set, representation)
    else {
        return;
    };
    let (timescale, pto) = match &addressing {
        manifest::SegmentAddressing::Template(st) => (
            st.timescale.unwrap_or(1).max(1),
            st.presentationTimeOffset.unwrap_or(0),
        ),
        manifest::SegmentAddressing::List(sl) => (sl.timescale.unwrap_or(1).max(1), 0),
        manifest::SegmentAddressing::Base(sb) => (
            sb.timescale.unwrap_or(1).max(1),
            sb.presentationTimeOffset.unwrap_or(0),
        ),
    };
    if let Some(anchor) = inband_anchor_from_segment(data, timescale, pto)
        && let Ok(mut guard) = store.lock()
    {
        *guard = Some(anchor);
    }
}

///
/// When multiple `prft` boxes are present, the last one wins (most recent in the segment).
pub(crate) fn inband_anchor_from_segment(
    data: &[u8],
    timescale: u64,
    presentation_time_offset: u64,
) -> Option<ProducerReferenceAnchor> {
    scan_prft_boxes(data)
        .into_iter()
        .last()
        .and_then(|prft| anchor_from_prft(prft, timescale, presentation_time_offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn build_prft_box(version: u8, reference_track_id: u32, ntp: u64, media_time: u64) -> Vec<u8> {
        let mut payload = vec![version, 0, 0, 0];
        payload.extend_from_slice(&reference_track_id.to_be_bytes());
        payload.extend_from_slice(&ntp.to_be_bytes());
        if version == 0 {
            payload.extend_from_slice(&(media_time as u32).to_be_bytes());
        } else {
            payload.extend_from_slice(&media_time.to_be_bytes());
        }
        let size = (8 + payload.len()) as u32;
        let mut out = Vec::with_capacity(size as usize);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(b"prft");
        out.extend_from_slice(&payload);
        out
    }

    fn wrap_moof(payload: &[u8]) -> Vec<u8> {
        let moof_payload_len = 8 + payload.len();
        let mut out = Vec::with_capacity(8 + moof_payload_len);
        out.extend_from_slice(&((8 + moof_payload_len) as u32).to_be_bytes());
        out.extend_from_slice(b"moof");
        out.extend_from_slice(&8u32.to_be_bytes());
        out.extend_from_slice(b"mfhd");
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn parse_prft_v0_box() {
        let unix = 1_588_334_400i64; // 2020-05-01T12:00:00Z
        let ntp_sec = (unix + 2_208_988_800) as u32;
        let ntp = (ntp_sec as u64) << 32;
        let prft = build_prft_box(0, 1, ntp, 4000);
        let parsed = scan_prft_boxes(&prft);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].reference_track_id, 1);
        assert_eq!(parsed[0].media_time, 4000);

        let anchor = anchor_from_prft(parsed[0], 1000, 0).expect("anchor");
        assert_eq!(
            anchor.wall_clock_time,
            Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap()
        );
        assert_eq!(anchor.pta_ticks, 4000);
        assert_eq!(anchor.timescale, 1000);
    }

    #[test]
    fn parse_prft_v1_inside_moof() {
        let unix = 1_588_334_420i64;
        let ntp_sec = (unix + 2_208_988_800) as u32;
        let ntp = (ntp_sec as u64) << 32;
        let prft = build_prft_box(1, 1, ntp, 8000);
        let segment = wrap_moof(&prft);
        let parsed = scan_prft_boxes(&segment);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].media_time, 8000);
    }

    #[test]
    fn inband_anchor_uses_last_prft_in_segment() {
        let unix = 1_588_334_400i64;
        let ntp_sec = (unix + 2_208_988_800) as u32;
        let ntp = (ntp_sec as u64) << 32;
        let first = build_prft_box(0, 1, ntp, 0);
        let second = build_prft_box(0, 1, ntp, 4000);
        let mut segment = first;
        segment.extend_from_slice(&second);
        let anchor = inband_anchor_from_segment(&segment, 1000, 0).expect("anchor");
        assert_eq!(anchor.pta_ticks, 4000);
    }
}
