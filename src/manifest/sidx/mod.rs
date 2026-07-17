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

fn index_range_is_exact(exact: Option<bool>) -> bool {
    exact == Some(true)
}

/// WebM/Matroska indexes (`Cues`, EBML header) share `SegmentBase@indexRange` in some
/// multi-codec MPDs (e.g. Shaka Angel One) but are not ISO BMFF `sidx` boxes.
fn looks_like_ebml_index(data: &[u8]) -> bool {
    // EBML header element ID, or Matroska/WebM `Cues` (`0x1C53BB6B`).
    data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) || data.starts_with(&[0x1C, 0x53, 0xBB, 0x6B])
}

fn reject_unsupported_index_container(index_bytes: &[u8]) -> Result<(), ManifestError> {
    if looks_like_ebml_index(index_bytes) {
        return Err(ManifestError::SidxParse(
            "WebM/Matroska SegmentBase index (EBML) is not supported; select an ISO BMFF (mp4) representation"
                .into(),
        ));
    }
    Ok(())
}

/// Read an ISO BMFF box header at `off`. Returns `(box_size, box_type)`.
fn read_box_header(data: &[u8], off: usize) -> Result<(usize, [u8; 4]), ManifestError> {
    if data.len().saturating_sub(off) < 8 {
        return Err(ManifestError::SidxParse(
            "truncated box header in index bytes".into(),
        ));
    }
    let raw_size = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    let mut typ = [0u8; 4];
    typ.copy_from_slice(&data[off + 4..off + 8]);
    let box_size = if raw_size == 0 {
        data.len().saturating_sub(off)
    } else if raw_size == 1 {
        return Err(ManifestError::SidxParse(
            "64-bit box largesize is not supported".into(),
        ));
    } else {
        raw_size as usize
    };
    if box_size < 8 {
        return Err(ManifestError::SidxParse("invalid ISO BMFF box size".into()));
    }
    Ok((box_size, typ))
}

/// Locate the first `sidx` within range-relative `index_bytes`.
///
/// When `exact` is true, the Index Segment must begin at the first byte (ISO/IEC 23009-1
/// `@indexRangeExact`). When false, boxes before the `sidx` are skipped (CMAF “scan” case).
fn find_sidx_offset(
    index_bytes: &[u8],
    exact: bool,
    range_start: u64,
) -> Result<usize, ManifestError> {
    if exact {
        let (box_size, typ) = match read_box_header(index_bytes, 0) {
            Ok(h) => h,
            Err(_) => {
                // Not even an 8-byte header: ask for at least that much.
                return Err(ManifestError::IncompleteSidxIndex {
                    need_end: range_start.saturating_add(7),
                });
            }
        };
        if typ != *b"sidx" {
            return Err(ManifestError::SidxParse(
                "indexRangeExact requires sidx at the start of @indexRange".into(),
            ));
        }
        if box_size > index_bytes.len() {
            // Exact ranges must contain the full Index Segment; if the box is larger than the
            // fetched exact window this is a content error (do not extend past `@indexRange`).
            return Err(ManifestError::SidxParse(
                "sidx box size exceeds exact @indexRange".into(),
            ));
        }
        return Ok(0);
    }

    let mut off = 0usize;
    while off < index_bytes.len() {
        let (box_size, typ) = match read_box_header(index_bytes, off) {
            Ok(h) => h,
            Err(_) => {
                return Err(ManifestError::IncompleteSidxIndex {
                    need_end: range_start.saturating_add(off as u64).saturating_add(7),
                });
            }
        };
        if typ == *b"sidx" {
            if off + box_size > index_bytes.len() {
                return Err(ManifestError::IncompleteSidxIndex {
                    need_end: range_start
                        .saturating_add(off as u64)
                        .saturating_add(box_size as u64)
                        .saturating_sub(1),
                });
            }
            return Ok(off);
        }
        // Non-sidx "boxes" with absurd sizes are almost always non-ISO containers
        // (e.g. EBML) misread as BMFF — do not ask to download hundreds of MB.
        const MAX_SCAN_SKIP: usize = 16 * 1024 * 1024;
        if box_size > MAX_SCAN_SKIP || !typ.iter().all(|b| b.is_ascii_graphic()) {
            return Err(ManifestError::SidxParse(
                "sidx box not found; index bytes do not look like ISO BMFF".into(),
            ));
        }
        if off + box_size > index_bytes.len() {
            // Need more bytes to skip past this box before scanning further.
            return Err(ManifestError::IncompleteSidxIndex {
                need_end: range_start
                    .saturating_add(off as u64)
                    .saturating_add(box_size as u64)
                    .saturating_sub(1),
            });
        }
        off += box_size;
    }
    Err(ManifestError::SidxParse(
        "sidx box not found within fetched @indexRange bytes".into(),
    ))
}

/// Ensure `index_bytes` covers the full Index Segment rooted at `sidx_off` (including
/// hierarchical type-1 children that follow the parent in the index).
fn ensure_index_segment_complete(
    index_bytes: &[u8],
    sidx_off: usize,
    range_start: u64,
) -> Result<(), ManifestError> {
    fn walk(
        data: &[u8],
        off: usize,
        range_start: u64,
        depth: usize,
    ) -> Result<usize, ManifestError> {
        const MAX_DEPTH: usize = 32;
        if depth > MAX_DEPTH {
            return Err(ManifestError::SidxParse(
                "sidx hierarchy exceeds maximum depth".into(),
            ));
        }
        let (box_size, typ) = match read_box_header(data, off) {
            Ok(h) => h,
            Err(_) => {
                return Err(ManifestError::IncompleteSidxIndex {
                    need_end: range_start.saturating_add(off as u64).saturating_add(7),
                });
            }
        };
        if typ != *b"sidx" {
            return Err(ManifestError::SidxParse(
                "expected sidx box in Index Segment".into(),
            ));
        }
        if off + box_size > data.len() {
            return Err(ManifestError::IncompleteSidxIndex {
                need_end: range_start
                    .saturating_add(off as u64)
                    .saturating_add(box_size as u64)
                    .saturating_sub(1),
            });
        }
        let sidx = SidxBox::parse(&data[off..off + box_size])?;
        let mut cursor = off.saturating_add(sidx.box_size);
        let mut end = cursor;
        for sref in &sidx.references {
            if sref.reference_type == 0 {
                continue;
            }
            let nested_size = sref.referenced_size as usize;
            if nested_size == 0 {
                return Err(ManifestError::SidxParse(
                    "hierarchical sidx reference has zero size".into(),
                ));
            }
            if cursor + nested_size > data.len() {
                return Err(ManifestError::IncompleteSidxIndex {
                    need_end: range_start
                        .saturating_add(cursor as u64)
                        .saturating_add(nested_size as u64)
                        .saturating_sub(1),
                });
            }
            let nested_end = walk(data, cursor, range_start, depth + 1)?;
            end = end.max(nested_end);
            cursor = cursor.saturating_add(nested_size);
        }
        Ok(end.max(off + box_size))
    }

    walk(index_bytes, sidx_off, range_start, 0).map(|_| ())
}

/// Parse `sidx` index bytes referenced by `SegmentBase@indexRange`.
///
/// `index_bytes` must be contiguous file content starting at `@indexRange` start (HTTP Range
/// body, possibly shorter than the declared window on the first attempt, or longer when the
/// Index Segment extends past `@indexRange` because `@indexRangeExact` is false). Callers
/// should loop on [`ManifestError::IncompleteSidxIndex`].
pub(crate) fn parse_sidx_index(
    sb: &SegmentBase,
    index_bytes: &[u8],
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let index_range = sb
        .indexRange
        .as_deref()
        .ok_or(ManifestError::MissingSegmentBaseIndexRange)?;
    let br = parse_byte_range(index_range)?;
    let exact = index_range_is_exact(sb.indexRangeExact);
    let file_base = br.start;

    reject_unsupported_index_container(index_bytes)?;
    let sidx_off = find_sidx_offset(index_bytes, exact, file_base)?;
    ensure_index_segment_complete(index_bytes, sidx_off, file_base)?;

    let sidx = SidxBox::parse(&index_bytes[sidx_off..])?;
    // `@indexRangeExact=true`: media begins immediately after the declared prefix.
    // Otherwise: `first_offset` is relative to the first byte after the `sidx` box.
    let media_origin = if exact {
        br.end.saturating_add(1)
    } else {
        file_base
            .saturating_add(sidx_off as u64)
            .saturating_add(sidx.box_size as u64)
    };
    let timescale = sb.timescale.unwrap_or(u64::from(sidx.timescale));
    let presentation_time_offset = sb.presentationTimeOffset.unwrap_or(0);
    timeline_segments_from_index_blob(
        index_bytes,
        sidx_off,
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

/// Parse `sidx` index bytes from a `SegmentTemplate@index` sidecar (`@indexRange`).
///
/// `index_bytes` must be contiguous sidecar content starting at `@indexRange` start (HTTP Range
/// body). When `@indexRangeExact` is false/absent, the Index Segment may extend past
/// `@indexRange` — callers should loop on [`ManifestError::IncompleteSidxIndex`]. Media byte
/// ranges use separate-index semantics (`first_offset` relative to the media file origin).
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
    let br = parse_byte_range(index_range)?;
    let exact = index_range_is_exact(st.indexRangeExact);
    let file_base = br.start;

    reject_unsupported_index_container(index_bytes)?;
    let sidx_off = find_sidx_offset(index_bytes, exact, file_base)?;
    ensure_index_segment_complete(index_bytes, sidx_off, file_base)?;

    let sidx = SidxBox::parse(&index_bytes[sidx_off..])?;
    let timescale = st.timescale.unwrap_or(u64::from(sidx.timescale));
    let presentation_time_offset = st.presentationTimeOffset.unwrap_or(0);
    let start_number = st.startNumber.unwrap_or(1);
    timeline_segments_from_index_blob(
        index_bytes,
        sidx_off,
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
