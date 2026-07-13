use dash_mpd::{RepresentationIndex, SegmentBase, SegmentTemplate};

use crate::manifest::ManifestError;

use super::types::parse_byte_range;
use super::types::{ByteRange, TimelineSegment};
use parser::SidxBox;

mod parser;

fn parse_sidx_from_representation_index_bytes(
    index_bytes: &[u8],
    ri: &RepresentationIndex,
) -> Result<(SidxBox, usize), ManifestError> {
    if let Some(range) = ri.range.as_deref() {
        parse_sidx_from_index_bytes(index_bytes, range)
    } else {
        let sidx = SidxBox::parse(index_bytes)?;
        Ok((sidx, 0))
    }
}

/// Parse `sidx` index bytes referenced by `SegmentBase@indexRange`.
pub(crate) fn parse_sidx_index(
    sb: &SegmentBase,
    index_bytes: &[u8],
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let index_range = sb
        .indexRange
        .as_deref()
        .ok_or(ManifestError::MissingSegmentBaseIndexRange)?;
    let br = parse_byte_range(index_range)?;
    // Media starts after the contiguous index range; each `sidx@first_offset` is relative to this.
    let media_origin = br.end.saturating_add(1);
    let (sidx, sidx_offset) = parse_sidx_from_index_bytes(index_bytes, index_range)?;
    let timescale = sb.timescale.unwrap_or(u64::from(sidx.timescale));
    let presentation_time_offset = sb.presentationTimeOffset.unwrap_or(0);
    timeline_segments_from_index_blob(
        index_bytes,
        sidx_offset,
        timescale,
        presentation_time_offset,
        1,
        media_origin,
    )
}

/// Parse `sidx` index bytes from `SegmentBase` (`@indexRange` or `RepresentationIndex`).
pub(crate) fn parse_sidx_index_for_segment_base(
    sb: &SegmentBase,
    index_bytes: &[u8],
) -> Result<Vec<TimelineSegment>, ManifestError> {
    if let Some(ri) = &sb.representation_index {
        return parse_sidx_index_from_representation_index_base(sb, ri, index_bytes);
    }
    parse_sidx_index(sb, index_bytes)
}

pub(crate) fn parse_sidx_index_from_representation_index_base(
    sb: &SegmentBase,
    ri: &RepresentationIndex,
    index_bytes: &[u8],
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let (sidx, sidx_offset) = parse_sidx_from_representation_index_bytes(index_bytes, ri)?;
    let timescale = sb.timescale.unwrap_or(u64::from(sidx.timescale));
    let presentation_time_offset = sb.presentationTimeOffset.unwrap_or(0);
    // Separate index document: `first_offset` is relative to the media file origin.
    timeline_segments_from_index_blob(
        index_bytes,
        sidx_offset,
        timescale,
        presentation_time_offset,
        1,
        0,
    )
}

fn parse_sidx_from_index_bytes(
    index_bytes: &[u8],
    index_range: &str,
) -> Result<(SidxBox, usize), ManifestError> {
    let br = parse_byte_range(index_range)?;
    let expected_len = br.end.saturating_sub(br.start).saturating_add(1) as usize;
    let (slice, offset_in_buf) = if index_bytes.len() == expected_len {
        (index_bytes, 0usize)
    } else {
        let slice = index_bytes
            .get(br.start as usize..=br.end as usize)
            .ok_or_else(|| ManifestError::InvalidByteRange(index_range.to_string()))?;
        (slice, br.start as usize)
    };
    let sidx = SidxBox::parse(slice)?;
    Ok((sidx, offset_in_buf))
}

pub(crate) fn parse_sidx_index_from_template(
    st: &SegmentTemplate,
    index_bytes: &[u8],
) -> Result<Vec<TimelineSegment>, ManifestError> {
    if let Some(ri) = &st.representation_index {
        return parse_sidx_index_from_template_representation_index(st, ri, index_bytes);
    }
    let index_range = st
        .indexRange
        .as_deref()
        .ok_or(ManifestError::MissingSegmentTemplateIndexRange)?;
    let (sidx, sidx_offset) = parse_sidx_from_index_bytes(index_bytes, index_range)?;
    let timescale = st.timescale.unwrap_or(u64::from(sidx.timescale));
    let presentation_time_offset = st.presentationTimeOffset.unwrap_or(0);
    let start_number = st.startNumber.unwrap_or(1);
    timeline_segments_from_index_blob(
        index_bytes,
        sidx_offset,
        timescale,
        presentation_time_offset,
        start_number,
        0,
    )
}

pub(crate) fn parse_sidx_index_from_template_representation_index(
    st: &SegmentTemplate,
    ri: &RepresentationIndex,
    index_bytes: &[u8],
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let (sidx, sidx_offset) = parse_sidx_from_representation_index_bytes(index_bytes, ri)?;
    let timescale = st.timescale.unwrap_or(u64::from(sidx.timescale));
    let presentation_time_offset = st.presentationTimeOffset.unwrap_or(0);
    let start_number = st.startNumber.unwrap_or(1);
    timeline_segments_from_index_blob(
        index_bytes,
        sidx_offset,
        timescale,
        presentation_time_offset,
        start_number,
        0,
    )
}

struct SidxExpandCtx<'a> {
    index_bytes: &'a [u8],
    timescale: u64,
    presentation_time_offset: u64,
    media_origin: u64,
    next_number: &'a mut u64,
    segments: &'a mut Vec<TimelineSegment>,
}

fn timeline_segments_from_index_blob(
    index_bytes: &[u8],
    sidx_offset: usize,
    timescale: u64,
    presentation_time_offset: u64,
    start_number: u64,
    media_origin: u64,
) -> Result<Vec<TimelineSegment>, ManifestError> {
    if timescale == 0 {
        return Err(ManifestError::ZeroTimescale);
    }
    let mut segments = Vec::new();
    let mut next_number = start_number;
    let mut ctx = SidxExpandCtx {
        index_bytes,
        timescale,
        presentation_time_offset,
        media_origin,
        next_number: &mut next_number,
        segments: &mut segments,
    };
    expand_sidx(&mut ctx, sidx_offset, 0)?;
    Ok(segments)
}

/// Flatten a `sidx` (and any nested `reference_type = 1` entries) into leaf media segments.
///
/// Uses ISO BMFF separate-index semantics: nested index boxes live in `index_bytes` immediately
/// after their parent (advanced by `referenced_size`), while media byte ranges are placed via each
/// box's `first_offset` relative to `media_origin`. Nested boxes that fall outside the fetched
/// index blob are rejected — interleaved same-file hierarchies that require further range fetches
/// are not resolved here.
fn expand_sidx(
    ctx: &mut SidxExpandCtx<'_>,
    sidx_offset: usize,
    depth: usize,
) -> Result<(), ManifestError> {
    const MAX_DEPTH: usize = 32;
    if depth > MAX_DEPTH {
        return Err(ManifestError::SidxParse(
            "sidx hierarchy exceeds maximum depth".into(),
        ));
    }

    let sidx_slice = ctx.index_bytes.get(sidx_offset..).ok_or_else(|| {
        ManifestError::SidxParse("sidx offset is outside fetched index bytes".into())
    })?;
    let sidx = SidxBox::parse(sidx_slice)?;

    // Index references (type 1) start at the first byte after this box in the index document.
    let mut index_cursor = sidx_offset.saturating_add(sidx.box_size);
    // Media references (type 0) start at media_origin + first_offset.
    let mut media_pos = ctx.media_origin.saturating_add(sidx.first_offset);
    let mut presentation_time = sidx.earliest_presentation_time;

    for sref in &sidx.references {
        if sref.reference_type != 0 {
            let nested_size = sref.referenced_size as usize;
            if nested_size == 0 {
                return Err(ManifestError::SidxParse(
                    "hierarchical sidx reference has zero size".into(),
                ));
            }
            let nested_end = index_cursor.saturating_add(nested_size);
            if nested_end > ctx.index_bytes.len() {
                return Err(ManifestError::HierarchicalSidxNotSupported);
            }
            // Nested box must begin at index_cursor; validate it looks like a sidx before recurse.
            let nested_probe = &ctx.index_bytes[index_cursor..nested_end];
            let nested = SidxBox::parse(nested_probe)
                .map_err(|_| ManifestError::HierarchicalSidxNotSupported)?;
            if nested.box_size > nested_size {
                return Err(ManifestError::SidxParse(
                    "nested sidx box size exceeds parent referenced_size".into(),
                ));
            }
            expand_sidx(ctx, index_cursor, depth + 1)?;
            index_cursor = nested_end;
            presentation_time =
                presentation_time.saturating_add(u64::from(sref.subsegment_duration));
            continue;
        }

        let start = media_pos;
        let end = start
            .saturating_add(u64::from(sref.referenced_size))
            .saturating_sub(1);
        let duration_ticks = u64::from(sref.subsegment_duration);
        let duration_s = duration_ticks as f64 / ctx.timescale as f64;
        let presentation_time_s = presentation_time.saturating_sub(ctx.presentation_time_offset)
            as f64
            / ctx.timescale as f64;

        ctx.segments.push(TimelineSegment {
            number: *ctx.next_number,
            time: presentation_time,
            duration: duration_ticks,
            duration_s,
            presentation_time_s,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: Some(ByteRange { start, end }),
        });
        *ctx.next_number = ctx.next_number.saturating_add(1);

        media_pos = media_pos.saturating_add(u64::from(sref.referenced_size));
        presentation_time = presentation_time.saturating_add(duration_ticks);
    }

    Ok(())
}

#[cfg(test)]
mod tests;
