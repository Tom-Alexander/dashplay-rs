use dash_mpd::{SegmentBase, SegmentTemplate};

use crate::PlayerError;

use super::types::parse_byte_range;
use super::types::{ByteRange, TimelineSegment};

pub(crate) fn timeline_segments_from_sidx(
    sb: &SegmentBase,
    sidx: &dash_mpd::sidx::SidxBox,
    index_start: u64,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let timescale = sb.timescale.unwrap_or(u64::from(sidx.timescale));
    let presentation_time_offset = sb.presentationTimeOffset.unwrap_or(0);
    timeline_segments_from_sidx_values(timescale, presentation_time_offset, 1, sidx, index_start)
}

fn timeline_segments_from_sidx_values(
    timescale: u64,
    presentation_time_offset: u64,
    start_number: u64,
    sidx: &dash_mpd::sidx::SidxBox,
    media_start: u64,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    if timescale == 0 {
        return Err(PlayerError::ZeroTimescale);
    }

    let mut segments = Vec::with_capacity(sidx.references.len());
    let mut current_pos = media_start;
    let mut presentation_time = sidx.earliest_presentation_time;

    for (i, sref) in sidx.references.iter().enumerate() {
        if sref.reference_type != 0 {
            return Err(PlayerError::HierarchicalSidxNotSupported);
        }
        let start = current_pos;
        let end = start
            .saturating_add(u64::from(sref.referenced_size))
            .saturating_sub(1);
        let duration_ticks = u64::from(sref.subsegment_duration);
        let duration_s = duration_ticks as f64 / timescale as f64;
        let presentation_time_s =
            presentation_time.saturating_sub(presentation_time_offset) as f64 / timescale as f64;

        segments.push(TimelineSegment {
            number: start_number.saturating_add(i as u64),
            time: presentation_time,
            duration: duration_ticks,
            duration_s,
            presentation_time_s,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: Some(ByteRange { start, end }),
        });

        current_pos += u64::from(sref.referenced_size);
        presentation_time = presentation_time.saturating_add(duration_ticks);
    }

    Ok(segments)
}

/// Parse `sidx` index bytes referenced by `SegmentBase@indexRange`.
pub(crate) fn parse_sidx_index(
    sb: &SegmentBase,
    index_bytes: &[u8],
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let index_range = sb
        .indexRange
        .as_deref()
        .ok_or(PlayerError::MissingSegmentBaseIndexRange)?;
    let br = parse_byte_range(index_range)?;
    let index_start = br.end.saturating_add(1);
    let sidx = parse_sidx_from_index_bytes(index_bytes, index_range)?;
    timeline_segments_from_sidx(sb, &sidx, index_start)
}

fn parse_sidx_from_index_bytes(
    index_bytes: &[u8],
    index_range: &str,
) -> Result<dash_mpd::sidx::SidxBox, PlayerError> {
    let br = parse_byte_range(index_range)?;
    let expected_len = br.end.saturating_sub(br.start).saturating_add(1) as usize;
    let slice = if index_bytes.len() == expected_len {
        index_bytes
    } else {
        index_bytes
            .get(br.start as usize..=br.end as usize)
            .ok_or_else(|| PlayerError::InvalidByteRange(index_range.to_string()))?
    };
    dash_mpd::sidx::SidxBox::parse(slice).map_err(|e| PlayerError::SidxParse(e.to_string()))
}
pub(crate) fn parse_sidx_index_from_template(
    st: &SegmentTemplate,
    index_bytes: &[u8],
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let index_range = st
        .indexRange
        .as_deref()
        .ok_or(PlayerError::MissingSegmentTemplateIndexRange)?;
    let sidx = parse_sidx_from_index_bytes(index_bytes, index_range)?;
    let timescale = st.timescale.unwrap_or(u64::from(sidx.timescale));
    let presentation_time_offset = st.presentationTimeOffset.unwrap_or(0);
    let start_number = st.startNumber.unwrap_or(1);
    timeline_segments_from_sidx_values(
        timescale,
        presentation_time_offset,
        start_number,
        &sidx,
        sidx.first_offset,
    )
}
