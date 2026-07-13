use dash_mpd::{RepresentationIndex, SegmentBase, SegmentList, SegmentTemplate};

use crate::manifest::ManifestError;

use super::addressing::segment_template_index_uses_segment_identifiers;
use super::sidx::{
    parse_sidx_index_from_template, parse_sidx_index_from_template_representation_index,
};
use super::template::{TemplateVars, interpolate_template};
use super::types::parse_byte_range;
use super::types::{ByteRange, SegmentFetchTarget, TimelineSegment};

pub(crate) fn representation_index_fetch_target(
    ri: &RepresentationIndex,
    vars: &TemplateVars<'_>,
) -> Result<SegmentFetchTarget, ManifestError> {
    let source = ri
        .sourceURL
        .as_deref()
        .ok_or(ManifestError::MissingRepresentationIndexSourceUrl)?;
    let path = interpolate_template(source, vars);
    let range = ri.range.as_deref().map(parse_byte_range).transpose()?;
    Ok(SegmentFetchTarget { path, range })
}

pub(crate) fn segment_base_index_target(
    sb: &SegmentBase,
    vars: &TemplateVars<'_>,
) -> Result<SegmentFetchTarget, ManifestError> {
    if let Some(ri) = &sb.representation_index {
        return representation_index_fetch_target(ri, vars);
    }
    let index_range = sb
        .indexRange
        .as_deref()
        .ok_or(ManifestError::MissingSegmentBaseIndexRange)?;
    let br = parse_byte_range(index_range)?;
    Ok(SegmentFetchTarget {
        path: String::new(),
        range: Some(br),
    })
}

pub(crate) fn segment_template_index_target(
    st: &SegmentTemplate,
    vars: &TemplateVars<'_>,
) -> Result<SegmentFetchTarget, ManifestError> {
    if let Some(ri) = &st.representation_index {
        if segment_template_index_uses_segment_identifiers(st)
            && vars.number.is_none()
            && vars.time.is_none()
        {
            return Err(ManifestError::MissingSegmentTemplateIndexVars);
        }
        return representation_index_fetch_target(ri, vars);
    }
    let index_tpl = st
        .index
        .as_deref()
        .ok_or(ManifestError::MissingSegmentTemplateIndex)?;
    if segment_template_index_uses_segment_identifiers(st)
        && vars.number.is_none()
        && vars.time.is_none()
    {
        return Err(ManifestError::MissingSegmentTemplateIndexVars);
    }
    let index_range = st
        .indexRange
        .as_deref()
        .ok_or(ManifestError::MissingSegmentTemplateIndexRange)?;
    let br = parse_byte_range(index_range)?;
    Ok(SegmentFetchTarget {
        path: interpolate_template(index_tpl, vars),
        range: Some(br),
    })
}

/// Inclusive media byte range for one per-segment sidecar index (`@index` / `RepresentationIndex`
/// with `$Number$` or `$Time$`).
pub(crate) fn media_range_from_per_segment_index(
    st: &SegmentTemplate,
    index_bytes: &[u8],
) -> Result<ByteRange, ManifestError> {
    let segs = if let Some(ri) = &st.representation_index {
        parse_sidx_index_from_template_representation_index(st, ri, index_bytes)?
    } else {
        parse_sidx_index_from_template(st, index_bytes)?
    };
    let Some(first) = segs.first() else {
        return Err(ManifestError::SidxParse("empty sidx index".into()));
    };
    let Some(first_range) = first.media_range else {
        return Err(ManifestError::SidxParse(
            "sidx index missing media range".into(),
        ));
    };
    let last_range = segs
        .last()
        .and_then(|s| s.media_range)
        .unwrap_or(first_range);
    Ok(ByteRange {
        start: first_range.start,
        end: last_range.end,
    })
}
pub(crate) fn segment_base_init_target(
    sb: &SegmentBase,
    vars: &TemplateVars<'_>,
) -> Result<SegmentFetchTarget, ManifestError> {
    let init = sb
        .Initialization
        .as_ref()
        .ok_or(ManifestError::MissingInitializationTemplate)?;
    let path = init
        .sourceURL
        .as_deref()
        .map(|s| interpolate_template(s, vars))
        .unwrap_or_default();
    let range = init.range.as_deref().map(parse_byte_range).transpose()?;
    Ok(SegmentFetchTarget { path, range })
}

/// `SegmentList` initialization fetch: optional `sourceURL` plus optional `@range` on BaseURL.
pub(crate) fn segment_list_init_target(
    sl: &SegmentList,
    vars: &TemplateVars<'_>,
) -> Result<Option<SegmentFetchTarget>, ManifestError> {
    let Some(init) = sl.Initialization.as_ref() else {
        return Ok(None);
    };
    let path = init
        .sourceURL
        .as_deref()
        .map(|s| interpolate_template(s, vars))
        .unwrap_or_default();
    let range = init.range.as_deref().map(parse_byte_range).transpose()?;
    if path.is_empty() && range.is_none() {
        return Err(ManifestError::MissingInitializationTemplate);
    }
    Ok(Some(SegmentFetchTarget { path, range }))
}

/// Media fetch target for one timeline segment under `SegmentList` addressing.
///
/// When `SegmentURL@media` is absent, the relative path is empty and the request uses the
/// representation BaseURL (byte-range-only list addressing).
pub(crate) fn segment_list_media_target(
    sl: &SegmentList,
    seg: &TimelineSegment,
    list_idx: usize,
) -> Result<SegmentFetchTarget, ManifestError> {
    let path = if let Some(url) = seg.media_url.as_deref() {
        url.to_string()
    } else {
        sl.segment_urls
            .get(list_idx)
            .and_then(|su| su.media.clone())
            .unwrap_or_default()
    };
    if path.is_empty() && seg.media_range.is_none() {
        return Err(ManifestError::MissingMediaTemplate);
    }
    Ok(SegmentFetchTarget {
        path,
        range: seg.media_range,
    })
}

/// Media fetch target for one timeline segment under `SegmentBase` addressing.
pub(crate) fn segment_base_media_target(
    _sb: &SegmentBase,
    seg: &TimelineSegment,
    vars: &TemplateVars<'_>,
) -> Result<SegmentFetchTarget, ManifestError> {
    let path = seg
        .media_url
        .as_deref()
        .map(|s| {
            interpolate_template(
                s,
                &TemplateVars {
                    number: Some(seg.number),
                    time: Some(seg.time),
                    sub_number: seg.sub_number,
                    ..*vars
                },
            )
        })
        .unwrap_or_default();
    Ok(SegmentFetchTarget {
        path,
        range: seg.media_range,
    })
}

#[cfg(test)]
mod tests;
