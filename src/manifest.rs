use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dash_mpd::{
    AdaptationSet, BaseURL, MPD, Period, Representation, SegmentBase, SegmentList, SegmentTemplate,
};
use url::Url;

use super::PlayerError;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PeriodWindow {
    pub idx: usize,
    pub start: Duration,
    pub end: Option<Duration>,
}

/// Wall-clock and MPD metadata for `SegmentTemplate@duration` (dynamic window) and for filtering
/// explicit `SegmentTimeline` on dynamic MPDs (time-shift buffer vs `availabilityStartTime`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct TimelineBuildContext {
    pub is_dynamic: bool,
    pub period_window: PeriodWindow,
    /// `Period@duration` when present.
    pub period_duration: Option<Duration>,
    pub media_presentation_duration: Option<Duration>,
    pub time_shift_buffer_depth: Option<Duration>,
    pub since_availability_start: Option<Duration>,
    pub resync_hints: Option<super::resync::ResyncHints>,
}

impl TimelineBuildContext {
    pub(crate) fn period_length_secs(self) -> Option<f64> {
        if let Some(end) = self.period_window.end {
            let d = end.saturating_sub(self.period_window.start);
            if !d.is_zero() {
                return Some(d.as_secs_f64());
            }
        }
        if let Some(d) = self.period_duration {
            if !d.is_zero() {
                return Some(d.as_secs_f64());
            }
        }
        self.media_presentation_duration
            .filter(|d| !d.is_zero())
            .map(|d| d.as_secs_f64())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TimelineSegment {
    pub number: u64,
    /// MPD anchor for `$Time$` (for `S@k`>1, earliest presentation time of the whole sequence).
    pub time: u64,
    /// Segment duration in MPD timescale ticks (mirrors `S@d`; playback uses `duration_s`).
    #[allow(dead_code)]
    pub duration: u64,
    pub duration_s: f64,
    /// Segment start time in seconds relative to the Period start.
    pub presentation_time_s: f64,
    /// When `S@k`>1: 1-based index within the segment sequence (`$SubNumber$`). Otherwise `None`.
    pub sub_number: Option<u64>,
    /// 1-based CMAF chunk index to start emitting from after mid-segment resync (`Resync@type` 2/3).
    pub resync_start_chunk: Option<u64>,
    /// Explicit `SegmentURL@media` when using `SegmentList` addressing (may be rep-specific).
    pub media_url: Option<String>,
    /// Inclusive byte range for `SegmentBase@indexRange` / `Initialization@range` addressing.
    pub media_range: Option<ByteRange>,
}

/// Inclusive byte range (`start`..=`end`) for HTTP Range requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// Relative path plus optional byte range for a segment or init fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentFetchTarget {
    pub path: String,
    pub range: Option<ByteRange>,
}

/// Parse a DASH range specifier (`start-end`, inclusive).
pub(crate) fn parse_byte_range(range: &str) -> Result<ByteRange, PlayerError> {
    let parts: Vec<&str> = range.split('-').collect();
    if parts.len() != 2 {
        return Err(PlayerError::InvalidByteRange(range.to_string()));
    }
    let start: u64 = parts[0]
        .parse()
        .map_err(|_| PlayerError::InvalidByteRange(range.to_string()))?;
    let end: u64 = parts[1]
        .parse()
        .map_err(|_| PlayerError::InvalidByteRange(range.to_string()))?;
    if end < start {
        return Err(PlayerError::InvalidByteRange(range.to_string()));
    }
    Ok(ByteRange { start, end })
}

/// Rewind a timeline index so playback begins at a DASH random-access point aligned with
/// `startWithSAP` semantics on the segment (not an interior `k`-split chunk).
///
/// For `S@k`>1, only the first subsegment shares the segment's SAP boundary at `S@t` unless
/// `AdaptationSet@subsegmentStartsWithSAP` is ≥1, in which case every subsegment is declared
/// to start with SAP.
pub(crate) fn align_start_index_to_sap(
    segments: &[TimelineSegment],
    start_idx: usize,
    adaptation_set: &AdaptationSet,
) -> usize {
    if segments.is_empty() {
        return 0;
    }
    let mut i = start_idx.min(segments.len() - 1);

    if adaptation_set
        .subsegmentStartsWithSAP
        .is_some_and(|v| v >= 1)
    {
        return i;
    }

    while i > 0 {
        match segments[i].sub_number {
            Some(n) if n > 1 => i -= 1,
            _ => break,
        }
    }
    i
}

/// When [`super::resync::ResyncHints::random_access_interval_s`] is set, snap `start_idx` to the
/// nearest segment on the resync grid (DASH-IF IOP §9.X.6.2.8).
pub(crate) fn align_start_index_to_resync(
    segments: &[TimelineSegment],
    start_idx: usize,
    hints: super::resync::ResyncHints,
) -> usize {
    let Some(interval_s) = hints
        .random_access_interval_s
        .filter(|x| x.is_finite() && *x > 0.0)
    else {
        return start_idx;
    };
    if segments.is_empty() {
        return 0;
    }
    let anchor_t = segments[start_idx.min(segments.len() - 1)].presentation_time_s;
    let grid_t = (anchor_t / interval_s).floor() * interval_s;
    segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.presentation_time_s <= grid_t + 1e-6)
        .max_by(|(_, a), (_, b)| {
            a.presentation_time_s
                .partial_cmp(&b.presentation_time_s)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(start_idx)
}

/// Presentation-time seconds (relative to Period start) where a segment sequence begins.
fn segment_sequence_start_presentation_s(seg: &TimelineSegment) -> f64 {
    if let Some(sub) = seg.sub_number {
        let prior = sub.saturating_sub(1) as f64;
        seg.presentation_time_s - prior * seg.duration_s
    } else {
        seg.presentation_time_s
    }
}

/// Align seek/recovery to the nearest in-segment resync point (`Resync@type` 2/3).
///
/// Returns the timeline index and an optional 1-based CMAF chunk index to start emitting from
/// within the first segment of the trimmed playback window.
pub(crate) fn mid_segment_resync_alignment(
    segments: &[TimelineSegment],
    start_idx: usize,
    target_presentation_time_s: f64,
    hints: super::resync::ResyncHints,
) -> (usize, Option<u64>) {
    let Some(interval_s) = hints
        .random_access_interval_s
        .filter(|x| x.is_finite() && *x > 0.0)
    else {
        return (start_idx, None);
    };
    if segments.is_empty() {
        return (0, None);
    }

    let idx = start_idx.min(segments.len() - 1);
    let grid_t = (target_presentation_time_s / interval_s).floor() * interval_s;

    let aligned_idx = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            let start = segment_sequence_start_presentation_s(s);
            start <= grid_t + 1e-6 && start + s.duration_s > grid_t - 1e-6
        })
        .max_by(|(_, a), (_, b)| {
            segment_sequence_start_presentation_s(a)
                .partial_cmp(&segment_sequence_start_presentation_s(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(idx);

    let seq_start = segment_sequence_start_presentation_s(&segments[aligned_idx]);
    let offset_s = (grid_t - seq_start).max(0.0);
    let chunk = (offset_s / interval_s).floor() as u64 + 1;

    (aligned_idx, Some(chunk.max(1)))
}

pub(crate) fn mpd(manifest: &Option<MPD>) -> Result<&MPD, PlayerError> {
    manifest.as_ref().ok_or(PlayerError::ManifestNotLoaded)
}

/// Elapsed time since `MPD@availabilityStartTime` using a wall clock (from [`super::utc_timing`] or local UTC).
pub(crate) fn since_availability_start_at(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
) -> Result<Option<Duration>, PlayerError> {
    let Some(ast) = mpd.availabilityStartTime else {
        return Ok(None);
    };

    let since_ast: Duration = wall_now
        .signed_duration_since(ast)
        .to_std()
        .unwrap_or(Duration::ZERO);

    Ok(Some(since_ast))
}

pub(crate) fn period_windows(mpd: &MPD) -> Result<Vec<PeriodWindow>, PlayerError> {
    if mpd.periods.is_empty() {
        return Err(PlayerError::NoPeriod);
    }

    let mut acc_start = Duration::ZERO;
    let mut windows = Vec::with_capacity(mpd.periods.len());

    for (idx, period) in mpd.periods.iter().enumerate() {
        let start = period.start.unwrap_or(acc_start);

        let end = if let Some(d) = period.duration {
            Some(start.saturating_add(d))
        } else {
            // If the next period has an explicit start, infer this one's end.
            mpd.periods.get(idx + 1).and_then(|p| p.start)
        };

        // Advance accumulated start time if we can.
        if let Some(e) = end {
            acc_start = e;
        }

        windows.push(PeriodWindow { idx, start, end });
    }

    Ok(windows)
}

pub(crate) fn is_dynamic_mpd(mpd: &MPD) -> bool {
    mpd.mpdtype.as_deref() == Some("dynamic")
}

pub(crate) fn current_period_window_at(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
) -> Result<PeriodWindow, PlayerError> {
    let windows = period_windows(mpd)?;

    // Static VOD has no availability timeline; playback starts at the first Period.
    let Some(since_ast) = since_availability_start_at(mpd, wall_now)? else {
        return Ok(windows[0]);
    };

    for w in windows {
        let in_range = since_ast >= w.start && w.end.is_none_or(|e| since_ast < e);
        if in_range {
            return Ok(w);
        }
    }

    Ok(PeriodWindow {
        idx: mpd.periods.len().saturating_sub(1),
        start: mpd
            .periods
            .last()
            .and_then(|p| p.start)
            .unwrap_or(Duration::ZERO),
        end: None,
    })
}

/// Hierarchical inputs for resolving segment URLs (ISO/IEC 23009-1 §5.6).
#[derive(Debug, Clone)]
pub(crate) struct SegmentBaseContext {
    pub manifest_uri: Url,
    pub mpd_base_urls: Vec<BaseURL>,
    pub period_base_urls: Vec<BaseURL>,
    pub service_location_priority: Vec<String>,
    pub default_service_location: Option<String>,
}

fn is_absolute_base(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("file://")
        || t.starts_with("ftp://")
}

/// Merge a document base with a `BaseURL@` value (RFC 3986); preserves manifest query when absent on the child (dash-mpd semantics).
pub(crate) fn merge_base_url(current: &Url, new: &str) -> Result<Url, PlayerError> {
    let new = new.trim();
    if new.is_empty() {
        return Ok(current.clone());
    }
    if is_absolute_base(new) {
        return Ok(Url::parse(new)?);
    }
    let mut merged = current.join(new)?;
    if merged.query().is_none() {
        merged.set_query(current.query());
    }
    Ok(merged)
}

fn sorted_base_url_layer(layer: &[BaseURL]) -> Vec<&BaseURL> {
    let mut v: Vec<_> = layer.iter().collect();
    v.sort_by_key(|bu| bu.priority.unwrap_or(u64::MAX));
    v
}

/// Expand one hierarchical level: each incoming base × each alternative `BaseURL` at this level.
fn expand_base_layer(bases: Vec<Url>, layer: &[BaseURL]) -> Result<Vec<Url>, PlayerError> {
    if layer.is_empty() {
        return Ok(bases);
    }
    let sorted = sorted_base_url_layer(layer);
    let alts: Vec<&str> = sorted
        .iter()
        .map(|bu| bu.base.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if alts.is_empty() {
        return Ok(bases);
    }
    let mut next = Vec::with_capacity(bases.len().saturating_mul(alts.len()));
    for b in bases {
        for s in &alts {
            next.push(merge_base_url(&b, s)?);
        }
    }
    Ok(next)
}

fn dedupe_urls(mut bases: Vec<Url>) -> Vec<Url> {
    let mut seen = HashSet::new();
    bases.retain(|u| seen.insert(u.as_str().to_string()));
    bases
}

/// Absolute segment bases for `(AdaptationSet, Representation)` after MPD → Period → AdaptationSet → Representation `BaseURL` expansion.
pub(crate) fn segment_bases_for_representation(
    ctx: &SegmentBaseContext,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<Vec<Url>, PlayerError> {
    let mut bases = vec![ctx.manifest_uri.clone()];
    let mpd_base_urls = super::content_steering::order_base_urls_for_steering(
        &ctx.mpd_base_urls,
        &ctx.service_location_priority,
        ctx.default_service_location.as_deref(),
    );
    bases = expand_base_layer(bases, &mpd_base_urls)?;
    bases = expand_base_layer(bases, &ctx.period_base_urls)?;
    bases = expand_base_layer(bases, &adaptation_set.BaseURL)?;
    bases = expand_base_layer(bases, &representation.BaseURL)?;
    Ok(dedupe_urls(bases))
}

/// Merge two `SegmentTemplate` nodes: `child` attributes override `parent` when present.
fn merge_segment_template(parent: &SegmentTemplate, child: &SegmentTemplate) -> SegmentTemplate {
    SegmentTemplate {
        media: child.media.clone().or_else(|| parent.media.clone()),
        index: child.index.clone().or_else(|| parent.index.clone()),
        initialization: child
            .initialization
            .clone()
            .or_else(|| parent.initialization.clone()),
        bitstreamSwitching: child
            .bitstreamSwitching
            .clone()
            .or_else(|| parent.bitstreamSwitching.clone()),
        indexRange: child
            .indexRange
            .clone()
            .or_else(|| parent.indexRange.clone()),
        indexRangeExact: child.indexRangeExact.or(parent.indexRangeExact),
        startNumber: child.startNumber.or(parent.startNumber),
        duration: child.duration.or(parent.duration),
        timescale: child.timescale.or(parent.timescale),
        eptDelta: child.eptDelta.or(parent.eptDelta),
        pbDelta: child.pbDelta.or(parent.pbDelta),
        presentationTimeOffset: child
            .presentationTimeOffset
            .or(parent.presentationTimeOffset),
        availabilityTimeOffset: child
            .availabilityTimeOffset
            .or(parent.availabilityTimeOffset),
        availabilityTimeComplete: child
            .availabilityTimeComplete
            .or(parent.availabilityTimeComplete),
        Initialization: child
            .Initialization
            .clone()
            .or_else(|| parent.Initialization.clone()),
        representation_index: child
            .representation_index
            .clone()
            .or_else(|| parent.representation_index.clone()),
        failover_content: child
            .failover_content
            .clone()
            .or_else(|| parent.failover_content.clone()),
        SegmentTimeline: child
            .SegmentTimeline
            .clone()
            .or_else(|| parent.SegmentTimeline.clone()),
        BitstreamSwitching: child
            .BitstreamSwitching
            .clone()
            .or_else(|| parent.BitstreamSwitching.clone()),
    }
}

fn merge_segment_template_chain(templates: &[Option<&SegmentTemplate>]) -> Option<SegmentTemplate> {
    templates.iter().filter_map(|t| *t).fold(None, |acc, st| {
        Some(match acc {
            None => st.clone(),
            Some(parent) => merge_segment_template(&parent, st),
        })
    })
}

fn segment_template_has_timeline_source(st: &SegmentTemplate) -> bool {
    st.SegmentTimeline.is_some() || st.duration.is_some()
}

/// Effective `SegmentTemplate` for timeline expansion on an adaptation set (Period → AdaptationSet,
/// supplementing from the first representation that carries timeline or duration when needed).
pub(crate) fn segment_template_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentTemplate, PlayerError> {
    let mut merged = merge_segment_template_chain(&[
        period.SegmentTemplate.as_ref(),
        adaptation_set.SegmentTemplate.as_ref(),
    ]);

    if merged
        .as_ref()
        .is_none_or(|st| !segment_template_has_timeline_source(st))
    {
        for rep in &adaptation_set.representations {
            if let Some(rep_st) = &rep.SegmentTemplate {
                if segment_template_has_timeline_source(rep_st) {
                    merged = Some(match merged {
                        None => rep_st.clone(),
                        Some(parent) => merge_segment_template(&parent, rep_st),
                    });
                    break;
                }
            }
        }
    }

    merged.ok_or(PlayerError::MissingSegmentTemplate)
}

/// Resolved segment addressing mode after Period → AdaptationSet → Representation inheritance.
#[derive(Debug, Clone)]
pub(crate) enum SegmentAddressing {
    Template(SegmentTemplate),
    List(SegmentList),
    Base(SegmentBase),
}

fn has_segment_list_in_chain(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: Option<&Representation>,
) -> bool {
    period.SegmentList.is_some()
        || adaptation_set.SegmentList.is_some()
        || representation.is_some_and(|r| r.SegmentList.is_some())
}

fn adaptation_set_uses_segment_list(period: &Period, adaptation_set: &AdaptationSet) -> bool {
    period.SegmentList.is_some()
        || adaptation_set.SegmentList.is_some()
        || adaptation_set
            .representations
            .iter()
            .any(|r| r.SegmentList.is_some())
}

fn adaptation_set_uses_segment_template(period: &Period, adaptation_set: &AdaptationSet) -> bool {
    period.SegmentTemplate.is_some()
        || adaptation_set.SegmentTemplate.is_some()
        || adaptation_set
            .representations
            .iter()
            .any(|r| r.SegmentTemplate.is_some())
}

fn adaptation_set_uses_segment_base(period: &Period, adaptation_set: &AdaptationSet) -> bool {
    period.SegmentBase.is_some()
        || adaptation_set.SegmentBase.is_some()
        || adaptation_set
            .representations
            .iter()
            .any(|r| r.SegmentBase.is_some())
}

fn has_segment_template_in_chain(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: Option<&Representation>,
) -> bool {
    period.SegmentTemplate.is_some()
        || adaptation_set.SegmentTemplate.is_some()
        || representation.is_some_and(|r| r.SegmentTemplate.is_some())
}

fn has_segment_base_in_chain(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: Option<&Representation>,
) -> bool {
    period.SegmentBase.is_some()
        || adaptation_set.SegmentBase.is_some()
        || representation.is_some_and(|r| r.SegmentBase.is_some())
}

/// Merge two `SegmentBase` nodes: `child` attributes override `parent` when present.
fn merge_segment_base(parent: &SegmentBase, child: &SegmentBase) -> SegmentBase {
    SegmentBase {
        timescale: child.timescale.or(parent.timescale),
        presentationTimeOffset: child
            .presentationTimeOffset
            .or(parent.presentationTimeOffset),
        indexRange: child
            .indexRange
            .clone()
            .or_else(|| parent.indexRange.clone()),
        indexRangeExact: child.indexRangeExact.or(parent.indexRangeExact),
        availabilityTimeOffset: child
            .availabilityTimeOffset
            .or(parent.availabilityTimeOffset),
        availabilityTimeComplete: child
            .availabilityTimeComplete
            .or(parent.availabilityTimeComplete),
        presentationDuration: child.presentationDuration.or(parent.presentationDuration),
        eptDelta: child.eptDelta.or(parent.eptDelta),
        pbDelta: child.pbDelta.or(parent.pbDelta),
        Initialization: child
            .Initialization
            .clone()
            .or_else(|| parent.Initialization.clone()),
        representation_index: child
            .representation_index
            .clone()
            .or_else(|| parent.representation_index.clone()),
        failover_content: child
            .failover_content
            .clone()
            .or_else(|| parent.failover_content.clone()),
    }
}

fn merge_segment_base_chain(bases: &[Option<&SegmentBase>]) -> Option<SegmentBase> {
    bases.iter().filter_map(|sb| *sb).fold(None, |acc, sb| {
        Some(match acc {
            None => sb.clone(),
            Some(parent) => merge_segment_base(&parent, sb),
        })
    })
}

/// Effective `SegmentBase` for timeline expansion on an adaptation set.
pub(crate) fn segment_base_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentBase, PlayerError> {
    merge_segment_base_chain(&[
        period.SegmentBase.as_ref(),
        adaptation_set.SegmentBase.as_ref(),
    ])
    .or_else(|| {
        adaptation_set
            .representations
            .iter()
            .find_map(|r| r.SegmentBase.as_ref())
            .cloned()
    })
    .ok_or(PlayerError::MissingSegmentBase)
}

/// Effective `SegmentBase` for fetching init/media of one representation.
pub(crate) fn segment_base_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentBase, PlayerError> {
    merge_segment_base_chain(&[
        period.SegmentBase.as_ref(),
        adaptation_set.SegmentBase.as_ref(),
        representation.SegmentBase.as_ref(),
    ])
    .ok_or(PlayerError::MissingSegmentBase)
}

/// Build timeline segments from a parsed ISOBMFF `sidx` box and `SegmentBase@indexRange`.
pub(crate) fn timeline_segments_from_sidx(
    sb: &SegmentBase,
    sidx: &dash_mpd::sidx::SidxBox,
    index_start: u64,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let timescale = sb.timescale.unwrap_or(u64::from(sidx.timescale));
    if timescale == 0 {
        return Err(PlayerError::ZeroTimescale);
    }
    let presentation_time_offset = sb.presentationTimeOffset.unwrap_or(0);

    let mut segments = Vec::with_capacity(sidx.references.len());
    let mut current_pos = index_start;
    let mut presentation_time = sidx.earliest_presentation_time;

    for (i, sref) in sidx.references.iter().enumerate() {
        if sref.reference_type != 0 {
            return Err(PlayerError::HierarchicalSidxNotSupported);
        }
        let start = current_pos;
        let end = current_pos.saturating_sub(1) + u64::from(sref.referenced_size);
        let duration_ticks = u64::from(sref.subsegment_duration);
        let duration_s = duration_ticks as f64 / timescale as f64;
        let presentation_time_s =
            presentation_time.saturating_sub(presentation_time_offset) as f64 / timescale as f64;

        segments.push(TimelineSegment {
            number: (i as u64).saturating_add(1),
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
    let sidx = dash_mpd::sidx::SidxBox::parse(index_bytes)
        .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
    timeline_segments_from_sidx(sb, &sidx, index_start)
}

/// Init fetch target for merged `SegmentBase` addressing.
pub(crate) fn segment_base_init_target(
    sb: &SegmentBase,
    vars: &TemplateVars<'_>,
) -> Result<SegmentFetchTarget, PlayerError> {
    let init = sb
        .Initialization
        .as_ref()
        .ok_or(PlayerError::MissingInitializationTemplate)?;
    let path = init
        .sourceURL
        .as_deref()
        .map(|s| interpolate_template(s, vars))
        .unwrap_or_default();
    let range = init.range.as_deref().map(parse_byte_range).transpose()?;
    Ok(SegmentFetchTarget { path, range })
}

/// Media fetch target for one timeline segment under `SegmentBase` addressing.
pub(crate) fn segment_base_media_target(
    _sb: &SegmentBase,
    seg: &TimelineSegment,
    vars: &TemplateVars<'_>,
) -> Result<SegmentFetchTarget, PlayerError> {
    let path = seg
        .media_url
        .as_deref()
        .map(|s| {
            interpolate_template(
                s,
                &TemplateVars {
                    representation_id: vars.representation_id,
                    bandwidth: vars.bandwidth,
                    number: Some(seg.number),
                    time: Some(seg.time),
                    sub_number: seg.sub_number,
                },
            )
        })
        .unwrap_or_default();
    Ok(SegmentFetchTarget {
        path,
        range: seg.media_range,
    })
}

/// Merge two `SegmentList` nodes: `child` attributes override `parent` when present.
fn merge_segment_list(parent: &SegmentList, child: &SegmentList) -> SegmentList {
    SegmentList {
        duration: child.duration.or(parent.duration),
        timescale: child.timescale.or(parent.timescale),
        indexRange: child
            .indexRange
            .clone()
            .or_else(|| parent.indexRange.clone()),
        indexRangeExact: child.indexRangeExact.or(parent.indexRangeExact),
        href: child.href.clone().or_else(|| parent.href.clone()),
        actuate: child.actuate.clone().or_else(|| parent.actuate.clone()),
        sltype: child.sltype.clone().or_else(|| parent.sltype.clone()),
        show: child.show.clone().or_else(|| parent.show.clone()),
        Initialization: child
            .Initialization
            .clone()
            .or_else(|| parent.Initialization.clone()),
        SegmentTimeline: child
            .SegmentTimeline
            .clone()
            .or_else(|| parent.SegmentTimeline.clone()),
        BitstreamSwitching: child
            .BitstreamSwitching
            .clone()
            .or_else(|| parent.BitstreamSwitching.clone()),
        segment_urls: if child.segment_urls.is_empty() {
            parent.segment_urls.clone()
        } else {
            child.segment_urls.clone()
        },
    }
}

fn merge_segment_list_chain(lists: &[Option<&SegmentList>]) -> Option<SegmentList> {
    lists.iter().filter_map(|sl| *sl).fold(None, |acc, sl| {
        Some(match acc {
            None => sl.clone(),
            Some(parent) => merge_segment_list(&parent, sl),
        })
    })
}

fn segment_list_has_timeline_source(sl: &SegmentList) -> bool {
    sl.SegmentTimeline.is_some() || sl.duration.is_some()
}

/// Effective `SegmentList` for timeline expansion on an adaptation set.
pub(crate) fn segment_list_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentList, PlayerError> {
    let mut merged = merge_segment_list_chain(&[
        period.SegmentList.as_ref(),
        adaptation_set.SegmentList.as_ref(),
    ]);

    if merged
        .as_ref()
        .is_none_or(|sl| !segment_list_has_timeline_source(sl))
    {
        for rep in &adaptation_set.representations {
            if let Some(rep_sl) = &rep.SegmentList {
                if segment_list_has_timeline_source(rep_sl) {
                    merged = Some(match merged {
                        None => rep_sl.clone(),
                        Some(parent) => merge_segment_list(&parent, rep_sl),
                    });
                    break;
                }
            }
        }
    }

    merged.ok_or(PlayerError::MissingSegmentList)
}

/// Effective `SegmentList` for fetching init/media of one representation.
pub(crate) fn segment_list_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentList, PlayerError> {
    merge_segment_list_chain(&[
        period.SegmentList.as_ref(),
        adaptation_set.SegmentList.as_ref(),
        representation.SegmentList.as_ref(),
    ])
    .ok_or(PlayerError::MissingSegmentList)
}

/// Effective segment addressing for timeline expansion on an adaptation set.
pub(crate) fn segment_addressing_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentAddressing, PlayerError> {
    if adaptation_set_uses_segment_list(period, adaptation_set) {
        return Ok(SegmentAddressing::List(segment_list_for_timeline(
            period,
            adaptation_set,
        )?));
    }
    if adaptation_set_uses_segment_template(period, adaptation_set) {
        return Ok(SegmentAddressing::Template(segment_template_for_timeline(
            period,
            adaptation_set,
        )?));
    }
    if adaptation_set_uses_segment_base(period, adaptation_set) {
        return Ok(SegmentAddressing::Base(segment_base_for_timeline(
            period,
            adaptation_set,
        )?));
    }
    Err(PlayerError::MissingSegmentTemplate)
}

/// Effective segment addressing for fetching init/media of one representation.
pub(crate) fn segment_addressing_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentAddressing, PlayerError> {
    if has_segment_list_in_chain(period, adaptation_set, Some(representation)) {
        return Ok(SegmentAddressing::List(segment_list_for_representation(
            period,
            adaptation_set,
            representation,
        )?));
    }
    if has_segment_template_in_chain(period, adaptation_set, Some(representation)) {
        return Ok(SegmentAddressing::Template(
            segment_template_for_representation(period, adaptation_set, representation)?,
        ));
    }
    if has_segment_base_in_chain(period, adaptation_set, Some(representation)) {
        return Ok(SegmentAddressing::Base(segment_base_for_representation(
            period,
            adaptation_set,
            representation,
        )?));
    }
    Err(PlayerError::MissingSegmentTemplate)
}

/// `SegmentList@Initialization@sourceURL` for the effective merged list.
pub(crate) fn segment_list_init_source(sl: &SegmentList) -> Result<&str, PlayerError> {
    sl.Initialization
        .as_ref()
        .and_then(|init| init.sourceURL.as_deref())
        .ok_or(PlayerError::MissingInitializationTemplate)
}

/// Media path for a segment index under `SegmentList` addressing (1-based segment number).
pub(crate) fn segment_list_media_for_index(
    sl: &SegmentList,
    segment_index: usize,
) -> Result<&str, PlayerError> {
    let su = sl
        .segment_urls
        .get(segment_index)
        .ok_or(PlayerError::EmptySegmentList)?;
    su.media.as_deref().ok_or(PlayerError::MissingMediaTemplate)
}

pub(crate) fn timeline_segments_for_addressing(
    addressing: &SegmentAddressing,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    match addressing {
        SegmentAddressing::Template(st) => timeline_segments(st, ctx),
        SegmentAddressing::List(sl) => timeline_segments_from_list(sl, ctx),
        SegmentAddressing::Base(sb) => timeline_segments_from_segment_base(sb, ctx),
    }
}

fn timeline_segments_from_segment_base(
    sb: &SegmentBase,
    _ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    if sb.indexRange.is_some() {
        return Err(PlayerError::SegmentBaseIndexNotLoaded);
    }

    let timescale = sb.timescale.unwrap_or(1);
    if timescale == 0 {
        return Err(PlayerError::ZeroTimescale);
    }

    let duration_ticks = sb
        .presentationDuration
        .filter(|d| *d > 0)
        .ok_or(PlayerError::MissingSegmentDuration)?;
    let duration_s = duration_ticks as f64 / timescale as f64;

    Ok(vec![TimelineSegment {
        number: 1,
        time: 0,
        duration: duration_ticks,
        duration_s,
        presentation_time_s: 0.0,
        sub_number: None,
        resync_start_chunk: None,
        media_url: None,
        media_range: None,
    }])
}

fn timeline_segments_from_list(
    sl: &SegmentList,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let segments = if let Some(timeline) = sl.SegmentTimeline.as_ref() {
        segments_from_list_timeline(sl, timeline, ctx)?
    } else if !sl.segment_urls.is_empty() {
        segments_from_list_urls(sl)?
    } else {
        return Err(PlayerError::EmptySegmentList);
    };

    if ctx.is_dynamic && sl.SegmentTimeline.is_some() {
        filter_explicit_timeline_for_dynamic_window(segments, ctx)
    } else {
        Ok(segments)
    }
}

fn segments_from_list_timeline(
    sl: &SegmentList,
    timeline: &dash_mpd::SegmentTimeline,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let pseudo_st = SegmentTemplate {
        timescale: sl.timescale,
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(timeline.clone()),
        ..Default::default()
    };
    let mut segments = segments_from_explicit_timeline(&pseudo_st, timeline, ctx)?;

    if !sl.segment_urls.is_empty() && sl.segment_urls.len() != segments.len() {
        return Err(PlayerError::SegmentListUrlTimelineMismatch);
    }

    for (seg, su) in segments.iter_mut().zip(sl.segment_urls.iter()) {
        seg.media_url = su.media.clone();
    }

    Ok(segments)
}

fn segments_from_list_urls(sl: &SegmentList) -> Result<Vec<TimelineSegment>, PlayerError> {
    let duration_ticks = sl
        .duration
        .filter(|d| *d > 0)
        .ok_or(PlayerError::MissingSegmentDuration)?;
    let timescale = sl.timescale.unwrap_or(1);
    if timescale == 0 {
        return Err(PlayerError::ZeroTimescale);
    }
    let duration_s = duration_ticks as f64 / timescale as f64;

    Ok(sl
        .segment_urls
        .iter()
        .enumerate()
        .map(|(i, su)| TimelineSegment {
            number: (i as u64).saturating_add(1),
            time: (i as u64).saturating_mul(duration_ticks),
            duration: duration_ticks,
            duration_s,
            presentation_time_s: i as f64 * duration_s,
            sub_number: None,
            resync_start_chunk: None,
            media_url: su.media.clone(),
            media_range: None,
        })
        .collect())
}

/// Effective `SegmentTemplate` for fetching init/media of one representation.
pub(crate) fn segment_template_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentTemplate, PlayerError> {
    merge_segment_template_chain(&[
        period.SegmentTemplate.as_ref(),
        adaptation_set.SegmentTemplate.as_ref(),
        representation.SegmentTemplate.as_ref(),
    ])
    .ok_or(PlayerError::MissingSegmentTemplate)
}

/// Build template substitution values for one representation (init/media URL construction).
pub(crate) fn template_vars_for_representation(rep: &Representation) -> TemplateVars<'_> {
    TemplateVars {
        representation_id: rep.id.as_deref().unwrap_or_default(),
        bandwidth: rep.bandwidth,
        number: None,
        time: None,
        sub_number: None,
    }
}

/// Substitution values for DASH `SegmentTemplate` URL identifiers (ISO 23009-1 §5.3.9.4.4).
#[derive(Debug, Clone, Copy)]
pub(crate) struct TemplateVars<'a> {
    pub representation_id: &'a str,
    pub bandwidth: Option<u64>,
    pub number: Option<u64>,
    pub time: Option<u64>,
    pub sub_number: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateIdent {
    RepId,
    Number,
    Bandwidth,
    Time,
    SubNumber,
}

/// Parse `%0[width]d` width from a DASH format tag; defaults to 1 per spec when absent or invalid.
fn dash_format_width(format: Option<&str>) -> usize {
    let Some(fmt) = format else {
        return 1;
    };
    if !fmt.starts_with("%0") || !fmt.ends_with('d') || fmt.len() < 4 {
        return 1;
    }
    let width_str = &fmt[2..fmt.len() - 1];
    if width_str.is_empty() {
        return 1;
    }
    width_str.parse::<usize>().unwrap_or(1).max(1)
}

fn format_dash_integer(value: u64, width: usize) -> String {
    format!("{:0width$}", value, width = width.max(1))
}

fn parse_template_ident(token: &str) -> Option<(TemplateIdent, Option<&str>)> {
    let (name, format) = match token.find('%') {
        Some(pos) => (&token[..pos], Some(&token[pos..])),
        None => (token, None),
    };
    let ident = match name {
        "RepresentationID" => TemplateIdent::RepId,
        "Number" => TemplateIdent::Number,
        "Bandwidth" => TemplateIdent::Bandwidth,
        "Time" => TemplateIdent::Time,
        "SubNumber" => TemplateIdent::SubNumber,
        _ => return None,
    };
    if ident == TemplateIdent::RepId && format.is_some() {
        return None;
    }
    Some((ident, format))
}

fn resolve_template_ident(
    ident: TemplateIdent,
    format: Option<&str>,
    vars: &TemplateVars<'_>,
) -> Option<String> {
    match ident {
        TemplateIdent::RepId => Some(vars.representation_id.to_string()),
        TemplateIdent::Number => vars
            .number
            .map(|n| format_dash_integer(n, dash_format_width(format))),
        TemplateIdent::Bandwidth => vars
            .bandwidth
            .map(|bw| format_dash_integer(bw, dash_format_width(format))),
        TemplateIdent::Time => vars
            .time
            .map(|t| format_dash_integer(t, dash_format_width(format))),
        TemplateIdent::SubNumber => Some(format_dash_integer(
            vars.sub_number.unwrap_or(1),
            dash_format_width(format),
        )),
    }
}

/// DASH `$...$` template interpolation (§5.3.9.4.4), including `$SubNumber$` (§5.3.9.6.5).
pub(crate) fn interpolate_template(template: &str, vars: &TemplateVars<'_>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while !rest.is_empty() {
        let Some(dollar_pos) = rest.find('$') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..dollar_pos]);
        rest = &rest[dollar_pos..];

        if rest.starts_with("$$") {
            out.push('$');
            rest = &rest[2..];
            continue;
        }

        let Some(close) = rest[1..].find('$') else {
            out.push('$');
            rest = &rest[1..];
            continue;
        };

        let token = &rest[1..=close];
        let consumed = close + 2;
        if let Some((ident, format)) = parse_template_ident(token) {
            if let Some(value) = resolve_template_ident(ident, format, vars) {
                out.push_str(&value);
                rest = &rest[consumed..];
                continue;
            }
        }

        out.push('$');
        rest = &rest[1..];
    }
    out
}

pub(crate) fn timeline_segments(
    st: &dash_mpd::SegmentTemplate,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let segments = if let Some(timeline) = st.SegmentTimeline.as_ref() {
        segments_from_explicit_timeline(st, timeline, ctx)?
    } else {
        segments_from_duration_template(st, ctx)?
    };

    if ctx.is_dynamic && st.SegmentTimeline.is_some() {
        filter_explicit_timeline_for_dynamic_window(segments, ctx)
    } else {
        Ok(segments)
    }
}

/// ISO/IEC 23009-1 §5.3.9.6 — `S@t` / `S@d` / `S@r` / `S@k` (segment sequences) / `S@n`.
fn segments_from_explicit_timeline(
    st: &dash_mpd::SegmentTemplate,
    timeline: &dash_mpd::SegmentTimeline,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let timescale = st.timescale.unwrap_or(1);
    if timescale == 0 {
        return Err(PlayerError::ZeroTimescale);
    }

    let presentation_time_offset = st.presentationTimeOffset.unwrap_or(0);
    let mut segments = Vec::new();

    let mut current_number = st.startNumber.unwrap_or(1);
    let mut current_time: Option<u64> = None;

    let period_start_s = ctx.period_window.start.as_secs_f64();
    const MAX_EXPANSION: usize = 1_000_000;

    for (seg_idx, s) in timeline.segments.iter().enumerate() {
        if s.d == 0 {
            return Err(PlayerError::ZeroTimelineSegmentDuration);
        }

        let k = s.k.unwrap_or(1);
        if k == 0 {
            return Err(PlayerError::InvalidTimelineSegmentK);
        }
        if k > 1 && s.d % k != 0 {
            return Err(PlayerError::TimelineDNotDivisibleByK);
        }

        if let Some(t) = s.t {
            current_time = Some(t);
        }

        if let Some(n) = s.n {
            current_number = n;
        }

        let repeat_count = s.r.unwrap_or(0);
        let mut t = current_time.unwrap_or(0);

        if repeat_count >= 0 {
            let mut seq_start = t;
            for _ in 0..=(repeat_count as u64) {
                if segments.len().saturating_add(k as usize) > MAX_EXPANSION {
                    return Err(PlayerError::UnboundedSegmentTimelineRepeat);
                }
                emit_segment_sequence(
                    &mut segments,
                    seq_start,
                    current_number,
                    s.d,
                    k,
                    timescale,
                    presentation_time_offset,
                )?;
                current_number += 1;
                seq_start = seq_start.saturating_add(s.d);
            }
            t = seq_start;
        } else {
            // §5.3.9.6: negative @r repeats until the next S, Period end, or (dynamic) next MPD update.
            let end = negative_r_repeat_end(seg_idx, timeline, ctx, period_start_s)?;
            loop {
                if segments.len().saturating_add(k as usize) > MAX_EXPANSION {
                    return Err(PlayerError::UnboundedSegmentTimelineRepeat);
                }
                match &end {
                    NegativeRepeatEnd::NextSegmentT(t_cap) => {
                        if t >= *t_cap {
                            break;
                        }
                    }
                    NegativeRepeatEnd::MpdSeconds(end_s) => {
                        let abs_start_s = period_start_s
                            + (t.saturating_sub(presentation_time_offset) as f64)
                                / (timescale as f64);
                        if abs_start_s >= *end_s - 1e-9 {
                            break;
                        }
                    }
                }
                emit_segment_sequence(
                    &mut segments,
                    t,
                    current_number,
                    s.d,
                    k,
                    timescale,
                    presentation_time_offset,
                )?;
                current_number += 1;
                t = t.saturating_add(s.d);
            }
        }

        current_time = Some(t);
    }

    Ok(segments)
}

fn emit_segment_sequence(
    segments: &mut Vec<TimelineSegment>,
    seq_start_t: u64,
    sequence_number: u64,
    d_total: u64,
    k: u64,
    timescale: u64,
    presentation_time_offset: u64,
) -> Result<(), PlayerError> {
    let d_per = d_total / k;
    let ts = timescale as f64;
    for sub in 1..=k {
        let chunk_start = seq_start_t.saturating_add((sub - 1).saturating_mul(d_per));
        let presentation_time_s =
            (chunk_start.saturating_sub(presentation_time_offset) as f64) / ts;
        let duration_s = d_per as f64 / ts;
        segments.push(TimelineSegment {
            number: sequence_number,
            time: seq_start_t,
            duration: d_per,
            duration_s,
            presentation_time_s,
            sub_number: if k > 1 { Some(sub) } else { None },
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        });
    }
    Ok(())
}

enum NegativeRepeatEnd {
    /// Stop before the first segment of the next `S` (exclusive `S@t`).
    NextSegmentT(u64),
    /// Stop when segment MPD start time (s) reaches or passes this bound.
    MpdSeconds(f64),
}

fn negative_r_repeat_end(
    seg_idx: usize,
    timeline: &dash_mpd::SegmentTimeline,
    ctx: &TimelineBuildContext,
    period_start_s: f64,
) -> Result<NegativeRepeatEnd, PlayerError> {
    for s2 in timeline.segments.iter().skip(seg_idx + 1) {
        if let Some(t2) = s2.t {
            return Ok(NegativeRepeatEnd::NextSegmentT(t2));
        }
    }

    if let Some(end_s) = ctx.period_window.end.map(|e| e.as_secs_f64()) {
        return Ok(NegativeRepeatEnd::MpdSeconds(end_s));
    }
    if let Some(dur) = ctx.period_duration {
        return Ok(NegativeRepeatEnd::MpdSeconds(
            period_start_s + dur.as_secs_f64(),
        ));
    }

    if ctx.is_dynamic {
        let Some(since) = ctx.since_availability_start else {
            return Err(PlayerError::MissingAvailabilityStartForDynamicTemplate);
        };
        return Ok(NegativeRepeatEnd::MpdSeconds(since.as_secs_f64()));
    }

    Err(PlayerError::UnboundedSegmentTimelineRepeat)
}

/// For dynamic MPDs with `SegmentTimeline`, keep segments in the time-shift buffer (same idea as
/// `segments_from_duration_template`): MPD media time in `[now - TSBD, now]`.
fn filter_explicit_timeline_for_dynamic_window(
    segments: Vec<TimelineSegment>,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let Some(since_ast) = ctx.since_availability_start else {
        return Err(PlayerError::MissingAvailabilityStartForDynamicTemplate);
    };
    let period_start_s = ctx.period_window.start.as_secs_f64();
    let now_s = since_ast.as_secs_f64();
    let tsbd_s = ctx
        .time_shift_buffer_depth
        .map(|x| x.as_secs_f64())
        .filter(|x| x.is_finite() && *x > 0.0)
        .unwrap_or(120.0);
    let window_start_s = (now_s - tsbd_s).max(period_start_s);
    let window_end_s = now_s;

    Ok(segments
        .into_iter()
        .filter(|s| {
            let abs_s = period_start_s + s.presentation_time_s;
            abs_s <= window_end_s + 1e-6 && abs_s >= window_start_s - 1e-6
        })
        .collect())
}

/// SegmentTemplate with `@duration` / `@timescale` / `@startNumber` but no `SegmentTimeline`.
fn segments_from_duration_template(
    st: &dash_mpd::SegmentTemplate,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, PlayerError> {
    let d = st
        .duration
        .filter(|x| *x > 0.0)
        .ok_or(PlayerError::MissingSegmentDuration)?;
    let timescale = st.timescale.unwrap_or(1);
    if timescale == 0 {
        return Err(PlayerError::ZeroTimescale);
    }
    let presentation_time_offset = st.presentationTimeOffset.unwrap_or(0);
    let start_number = st.startNumber.unwrap_or(1);
    let duration_s = d / timescale as f64;
    let duration_ticks = d.round().max(1.0) as u64;

    if ctx.is_dynamic {
        let Some(since_ast) = ctx.since_availability_start else {
            return Err(PlayerError::MissingAvailabilityStartForDynamicTemplate);
        };
        let period_start_s = ctx.period_window.start.as_secs_f64();
        let t_in_period = (since_ast.as_secs_f64() - period_start_s).max(0.0);
        let end_n = start_number + (t_in_period / duration_s).floor() as u64;

        let tsbd_s = ctx
            .time_shift_buffer_depth
            .map(|x| x.as_secs_f64())
            .filter(|x| x.is_finite() && *x > 0.0)
            .unwrap_or(120.0);
        let span = ((tsbd_s / duration_s).ceil() as u64)
            .saturating_add(2)
            .max(1);
        let start_n = end_n.saturating_sub(span).max(start_number);

        let mut segments = Vec::new();
        for n in start_n..=end_n {
            let t = presentation_time_offset as f64 + (n - start_number) as f64 * d;
            let t_u64 = t.max(0.0) as u64;
            let presentation_time_s = (n - start_number) as f64 * d / timescale as f64;
            segments.push(TimelineSegment {
                number: n,
                time: t_u64,
                duration: duration_ticks,
                duration_s,
                presentation_time_s,
                sub_number: None,
                resync_start_chunk: None,
                media_url: None,
                media_range: None,
            });
        }
        Ok(segments)
    } else {
        let period_duration_s = ctx
            .period_length_secs()
            .filter(|x| x.is_finite() && *x > 0.0)
            .ok_or(PlayerError::MissingPeriodExtentForStaticTemplate)?;
        let count = ((period_duration_s / duration_s).ceil() as u64).max(1);
        let mut segments = Vec::with_capacity(count as usize);
        for i in 0..count {
            let n = start_number + i;
            let t = presentation_time_offset as f64 + i as f64 * d;
            let t_u64 = t.max(0.0) as u64;
            let presentation_time_s = i as f64 * d / timescale as f64;
            segments.push(TimelineSegment {
                number: n,
                time: t_u64,
                duration: duration_ticks,
                duration_s,
                presentation_time_s,
                sub_number: None,
                resync_start_chunk: None,
                media_url: None,
                media_range: None,
            });
        }
        Ok(segments)
    }
}

/// Low-latency segment availability attributes from merged segment addressing (ISO 23009-1 §5.3.9.6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SegmentAvailability {
    /// `@availabilityTimeOffset` in seconds; `None` in XML means zero.
    pub availability_time_offset_s: Option<f64>,
    /// `@availabilityTimeComplete`; absent means `true`.
    pub availability_time_complete: bool,
}

impl SegmentAvailability {
    pub(crate) fn from_addressing(addressing: &SegmentAddressing) -> Self {
        match addressing {
            SegmentAddressing::Template(st) => Self {
                availability_time_offset_s: st.availabilityTimeOffset,
                availability_time_complete: st.availabilityTimeComplete.unwrap_or(true),
            },
            SegmentAddressing::List(_sl) => Self {
                availability_time_offset_s: None,
                availability_time_complete: true,
            },
            SegmentAddressing::Base(sb) => Self {
                availability_time_offset_s: sb.availabilityTimeOffset,
                availability_time_complete: sb.availabilityTimeComplete.unwrap_or(true),
            },
        }
    }
}

/// `@target` from the first [`ServiceDescription::Latency`] entry (milliseconds per DASH-IF IOP).
pub(crate) fn target_latency_from_mpd(mpd: &MPD) -> Option<Duration> {
    for sd in &mpd.ServiceDescription {
        for lat in &sd.Latency {
            let target_ms = lat.target?;
            if target_ms.is_finite() && target_ms >= 0.0 {
                return Some(Duration::from_secs_f64(target_ms / 1000.0));
            }
        }
    }
    None
}

/// MPD media-timeline seconds (from `availabilityStartTime`) when a segment sequence starts.
pub(crate) fn segment_sequence_start_s(period_start: Duration, seg: &TimelineSegment) -> f64 {
    let period_start_s = period_start.as_secs_f64();
    let seq_start_s = if let Some(sub) = seg.sub_number {
        let prior = sub.saturating_sub(1) as f64;
        seg.presentation_time_s - prior * seg.duration_s
    } else {
        seg.presentation_time_s
    };
    period_start_s + seq_start_s
}

/// Whether a segment is published and fetchable at `since_availability_start`.
pub(crate) fn segment_is_available(
    seg: &TimelineSegment,
    period_start: Duration,
    since_availability_start: Duration,
    availability: &SegmentAvailability,
) -> bool {
    let ato = availability.availability_time_offset_s.unwrap_or(0.0);
    if ato.is_infinite() {
        return ato.is_sign_positive();
    }
    if ato.is_nan() {
        return true;
    }

    let now_s = since_availability_start.as_secs_f64();

    if !availability.availability_time_complete {
        if seg.sub_number.is_some() {
            return now_s + 1e-6 >= period_start.as_secs_f64() + seg.presentation_time_s;
        }
        let sap_s = segment_sequence_start_s(period_start, seg);
        return now_s + 1e-6 >= sap_s;
    }

    let sap_s = segment_sequence_start_s(period_start, seg);
    now_s + 1e-6 >= sap_s + ato.max(0.0)
}

/// `@availabilityTimeComplete=false` on a whole segment (no `S@k` sub-number): fetch via chunked HTTP.
pub(crate) fn uses_chunked_segment_transfer(
    availability: &SegmentAvailability,
    seg: &TimelineSegment,
) -> bool {
    !availability.availability_time_complete && seg.sub_number.is_none()
}

/// Drop segments that are not yet published on dynamic MPDs.
pub(crate) fn filter_segments_by_availability(
    segments: Vec<TimelineSegment>,
    is_dynamic: bool,
    period_start: Duration,
    since_availability_start: Option<Duration>,
    addressing: &SegmentAddressing,
) -> Vec<TimelineSegment> {
    if !is_dynamic {
        return segments;
    }
    let Some(since) = since_availability_start else {
        return segments;
    };
    let availability = SegmentAvailability::from_addressing(addressing);
    segments
        .into_iter()
        .filter(|s| segment_is_available(s, period_start, since, &availability))
        .collect()
}

pub(crate) fn target_presentation_time_from_since(mpd: &MPD, since_ast: Duration) -> Duration {
    let mut t = since_ast;
    if let Some(latency) = target_latency_from_mpd(mpd) {
        t = t.saturating_sub(latency);
    } else if let Some(delay) = mpd.suggestedPresentationDelay {
        t = t.saturating_sub(delay);
    }
    t
}

pub(crate) fn target_presentation_time_at(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
) -> Result<Option<Duration>, PlayerError> {
    let Some(since_ast) = since_availability_start_at(mpd, wall_now)? else {
        return Ok(None);
    };
    Ok(Some(target_presentation_time_from_since(mpd, since_ast)))
}

#[cfg(test)]
mod timeline_tests {
    use super::*;
    use dash_mpd::{AdaptationSet, S, SegmentTemplate, SegmentTimeline};

    fn static_ctx(period_end: Option<Duration>) -> TimelineBuildContext {
        TimelineBuildContext {
            is_dynamic: false,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: period_end,
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
        }
    }

    #[test]
    fn align_start_index_to_resync_snaps_to_grid() {
        let segments = vec![
            TimelineSegment {
                number: 1,
                presentation_time_s: 0.0,
                duration_s: 2.0,
                ..default_timeline_segment()
            },
            TimelineSegment {
                number: 2,
                presentation_time_s: 2.0,
                duration_s: 2.0,
                ..default_timeline_segment()
            },
            TimelineSegment {
                number: 3,
                presentation_time_s: 5.0,
                duration_s: 2.0,
                ..default_timeline_segment()
            },
        ];
        let hints = super::super::resync::ResyncHints {
            chunk_duration_s: None,
            random_access_interval_s: Some(2.0),
            random_access_markers: false,
            random_access_within_segment: false,
        };
        assert_eq!(align_start_index_to_resync(&segments, 2, hints), 1);
        assert_eq!(align_start_index_to_resync(&segments, 1, hints), 1);
    }

    #[test]
    fn mid_segment_resync_alignment_snaps_to_in_segment_grid() {
        let segments = vec![
            TimelineSegment {
                number: 1,
                presentation_time_s: 0.0,
                duration_s: 4.0,
                ..default_timeline_segment()
            },
            TimelineSegment {
                number: 2,
                presentation_time_s: 4.0,
                duration_s: 4.0,
                ..default_timeline_segment()
            },
        ];
        let hints = super::super::resync::ResyncHints {
            chunk_duration_s: None,
            random_access_interval_s: Some(0.5),
            random_access_markers: false,
            random_access_within_segment: true,
        };
        let (idx, chunk) = mid_segment_resync_alignment(&segments, 1, 5.2, hints);
        assert_eq!(idx, 1);
        assert_eq!(chunk, Some(3)); // 4.0 + 2*0.5 = 5.0s resync point → chunk 3
    }

    #[test]
    fn mid_segment_resync_alignment_at_segment_start_uses_first_chunk() {
        let segments = vec![TimelineSegment {
            number: 1,
            presentation_time_s: 4.0,
            duration_s: 4.0,
            ..default_timeline_segment()
        }];
        let hints = super::super::resync::ResyncHints {
            chunk_duration_s: None,
            random_access_interval_s: Some(0.5),
            random_access_markers: false,
            random_access_within_segment: true,
        };
        let (idx, chunk) = mid_segment_resync_alignment(&segments, 0, 4.0, hints);
        assert_eq!(idx, 0);
        assert_eq!(chunk, Some(1));
    }

    fn default_timeline_segment() -> TimelineSegment {
        TimelineSegment {
            number: 0,
            time: 0,
            duration: 0,
            duration_s: 0.0,
            presentation_time_s: 0.0,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        }
    }

    #[test]
    fn segment_timeline_implicit_t_chains_previous_s_end() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            SegmentTimeline: Some(SegmentTimeline {
                segments: vec![
                    S {
                        t: Some(0),
                        d: 2000,
                        r: Some(0),
                        ..Default::default()
                    },
                    S {
                        t: None,
                        d: 1000,
                        r: Some(1),
                        ..Default::default()
                    },
                ],
            }),
            ..Default::default()
        };
        let segs = timeline_segments(&st, &static_ctx(Some(Duration::from_secs(10)))).unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].time, 0);
        assert_eq!(segs[1].time, 2000);
        assert_eq!(segs[2].time, 3000);
    }

    #[test]
    fn segment_timeline_s_n_sets_first_segment_number() {
        let st = SegmentTemplate {
            timescale: Some(1),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            SegmentTimeline: Some(SegmentTimeline {
                segments: vec![S {
                    t: Some(10),
                    d: 1,
                    r: Some(0),
                    n: Some(99),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };
        let segs = timeline_segments(&st, &static_ctx(None)).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].number, 99);
        assert_eq!(segs[0].time, 10);
    }

    #[test]
    fn segment_timeline_negative_r_stops_before_next_s_t() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            SegmentTimeline: Some(SegmentTimeline {
                segments: vec![
                    S {
                        t: Some(0),
                        d: 500,
                        r: Some(-1),
                        ..Default::default()
                    },
                    S {
                        t: Some(2000),
                        d: 100,
                        r: Some(0),
                        ..Default::default()
                    },
                ],
            }),
            ..Default::default()
        };
        let segs = timeline_segments(&st, &static_ctx(None)).unwrap();
        assert_eq!(segs.len(), 5);
        assert_eq!(
            segs.iter().map(|s| s.time).collect::<Vec<_>>(),
            vec![0, 500, 1000, 1500, 2000]
        );
    }

    #[test]
    fn segment_timeline_negative_r_unbounded_errors() {
        let st = SegmentTemplate {
            timescale: Some(1),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            SegmentTimeline: Some(SegmentTimeline {
                segments: vec![S {
                    t: Some(0),
                    d: 1,
                    r: Some(-1),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };
        let ctx = static_ctx(None);
        let err = timeline_segments(&st, &ctx).unwrap_err();
        assert!(matches!(err, PlayerError::UnboundedSegmentTimelineRepeat));
    }

    #[test]
    fn dynamic_segment_timeline_filtered_to_time_shift_buffer() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            SegmentTimeline: Some(SegmentTimeline {
                segments: vec![
                    S {
                        t: Some(1000),
                        d: 1000,
                        r: Some(9),
                        ..Default::default()
                    }, // 1s..10s
                ],
            }),
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: true,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: None,
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: Some(Duration::from_secs(2)),
            since_availability_start: Some(Duration::from_secs(5)),
            resync_hints: None,
        };
        let segs = timeline_segments(&st, &ctx).unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(
            segs.iter().map(|s| s.time).collect::<Vec<_>>(),
            vec![3000, 4000, 5000]
        );
    }

    #[test]
    fn segment_timeline_k_sequence_subnumbers_and_shared_time() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            SegmentTimeline: Some(SegmentTimeline {
                segments: vec![S {
                    t: Some(0),
                    d: 3000,
                    r: Some(0),
                    k: Some(3),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };
        let segs = timeline_segments(&st, &static_ctx(None)).unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].number, 1);
        assert_eq!(segs[1].number, 1);
        assert_eq!(segs[2].number, 1);
        assert_eq!(segs[0].time, 0);
        assert_eq!(segs[1].time, 0);
        assert_eq!(segs[2].time, 0);
        assert_eq!(
            segs.iter().map(|s| s.sub_number).collect::<Vec<_>>(),
            vec![Some(1), Some(2), Some(3)]
        );
        assert_eq!(segs[0].duration, 1000);
        assert!((segs[0].presentation_time_s - 0.0).abs() < 1e-9);
        assert!((segs[1].presentation_time_s - 1.0).abs() < 1e-9);
        assert!((segs[2].presentation_time_s - 2.0).abs() < 1e-9);
    }

    #[test]
    fn segment_timeline_k_with_repeat_r_counts_sequences() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            SegmentTimeline: Some(SegmentTimeline {
                segments: vec![S {
                    t: Some(0),
                    d: 4000,
                    r: Some(1),
                    k: Some(2),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };
        let segs = timeline_segments(&st, &static_ctx(None)).unwrap();
        assert_eq!(segs.len(), 4);
        assert_eq!(
            segs.iter().map(|s| s.number).collect::<Vec<_>>(),
            vec![1, 1, 2, 2]
        );
        assert_eq!(segs[2].time, 4000);
        assert_eq!(segs[2].presentation_time_s, 4.0);
    }

    #[test]
    fn segment_timeline_k_must_divide_d() {
        let st = SegmentTemplate {
            timescale: Some(1),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            SegmentTimeline: Some(SegmentTimeline {
                segments: vec![S {
                    t: Some(0),
                    d: 10,
                    r: Some(0),
                    k: Some(3),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };
        let err = timeline_segments(&st, &static_ctx(None)).unwrap_err();
        assert!(matches!(err, PlayerError::TimelineDNotDivisibleByK));
    }

    #[test]
    fn interpolate_template_subnumber() {
        let vars = TemplateVars {
            representation_id: "A",
            bandwidth: None,
            number: Some(7),
            time: Some(42),
            sub_number: Some(3),
        };
        let out = interpolate_template(
            "v-$RepresentationID$-$Number$-$Time$-$SubNumber$.m4s",
            &vars,
        );
        assert_eq!(out, "v-A-7-42-3.m4s");
    }

    #[test]
    fn interpolate_template_subnumber_defaults_to_one() {
        let vars = TemplateVars {
            representation_id: "id",
            bandwidth: None,
            number: None,
            time: None,
            sub_number: None,
        };
        let out = interpolate_template("x-$SubNumber$.m4s", &vars);
        assert_eq!(out, "x-1.m4s");
    }

    #[test]
    fn interpolate_template_number_and_time_format_width() {
        let vars = TemplateVars {
            representation_id: "1",
            bandwidth: Some(1_100_000),
            number: Some(7),
            time: Some(42),
            sub_number: None,
        };
        let out = interpolate_template(
            "chunk-$RepresentationID$-$Number%05d$-$Time%010d$-$Bandwidth%07d$.m4s",
            &vars,
        );
        assert_eq!(out, "chunk-1-00007-0000000042-1100000.m4s");
    }

    #[test]
    fn interpolate_template_bandwidth_without_format() {
        let vars = TemplateVars {
            representation_id: "v",
            bandwidth: Some(500_000),
            number: None,
            time: None,
            sub_number: None,
        };
        let out = interpolate_template("seg-$Bandwidth$.m4s", &vars);
        assert_eq!(out, "seg-500000.m4s");
    }

    #[test]
    fn interpolate_template_dollar_escape() {
        let vars = TemplateVars {
            representation_id: "id",
            bandwidth: None,
            number: Some(1),
            time: None,
            sub_number: None,
        };
        let out = interpolate_template("pre$$-$Number$-post", &vars);
        assert_eq!(out, "pre$-1-post");
    }

    #[test]
    fn interpolate_template_leaves_missing_number_unsubstituted() {
        let vars = TemplateVars {
            representation_id: "id",
            bandwidth: None,
            number: None,
            time: None,
            sub_number: None,
        };
        let out = interpolate_template("seg-$Number%05d$.m4s", &vars);
        assert_eq!(out, "seg-$Number%05d$.m4s");
    }

    #[test]
    fn align_start_index_rewinds_to_first_subsegment_without_subsegment_sap() {
        let aset = AdaptationSet {
            startWithSAP: Some(1),
            ..Default::default()
        };
        let segs = vec![
            TimelineSegment {
                number: 1,
                time: 0,
                duration: 1000,
                duration_s: 1.0,
                presentation_time_s: 0.0,
                sub_number: Some(1),
                resync_start_chunk: None,
                media_url: None,
                media_range: None,
            },
            TimelineSegment {
                number: 1,
                time: 0,
                duration: 1000,
                duration_s: 1.0,
                presentation_time_s: 1.0,
                sub_number: Some(2),
                resync_start_chunk: None,
                media_url: None,
                media_range: None,
            },
            TimelineSegment {
                number: 1,
                time: 0,
                duration: 1000,
                duration_s: 1.0,
                presentation_time_s: 2.0,
                sub_number: Some(3),
                resync_start_chunk: None,
                media_url: None,
                media_range: None,
            },
        ];
        assert_eq!(align_start_index_to_sap(&segs, 2, &aset), 0);
    }

    #[test]
    fn align_start_index_keeps_interior_subsegment_when_subsegment_starts_with_sap() {
        let aset = AdaptationSet {
            startWithSAP: Some(1),
            subsegmentStartsWithSAP: Some(1),
            ..Default::default()
        };
        let segs = vec![
            TimelineSegment {
                number: 1,
                time: 0,
                duration: 1000,
                duration_s: 1.0,
                presentation_time_s: 0.0,
                sub_number: Some(1),
                resync_start_chunk: None,
                media_url: None,
                media_range: None,
            },
            TimelineSegment {
                number: 1,
                time: 0,
                duration: 1000,
                duration_s: 1.0,
                presentation_time_s: 1.0,
                sub_number: Some(2),
                resync_start_chunk: None,
                media_url: None,
                media_range: None,
            },
        ];
        assert_eq!(align_start_index_to_sap(&segs, 1, &aset), 1);
    }
}

#[cfg(test)]
mod manifest_logic_tests {
    use super::*;
    use chrono::TimeZone;
    use dash_mpd::{
        AdaptationSet, Period, Representation, SegmentBase, SegmentList, SegmentTemplate,
        SegmentURL,
    };

    #[test]
    fn merge_base_url_relative_and_absolute() {
        let base = Url::parse("https://cdn.example/vod/?token=abc").unwrap();
        let rel = merge_base_url(&base, "segments/").unwrap();
        assert_eq!(rel.as_str(), "https://cdn.example/vod/segments/?token=abc");

        let abs = merge_base_url(&base, "https://alt.example/").unwrap();
        assert_eq!(abs.as_str(), "https://alt.example/");
    }

    #[test]
    fn segment_bases_expand_hierarchy_and_dedupe() {
        let ctx = SegmentBaseContext {
            manifest_uri: Url::parse("https://example.com/manifest.mpd?sig=1").unwrap(),
            mpd_base_urls: vec![BaseURL {
                base: "mpd/".into(),
                ..Default::default()
            }],
            period_base_urls: vec![BaseURL {
                base: "period/".into(),
                ..Default::default()
            }],
            service_location_priority: Vec::new(),
            default_service_location: None,
        };
        let adaptation_set = AdaptationSet {
            BaseURL: vec![BaseURL {
                base: "as/".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let representation = Representation {
            BaseURL: vec![
                BaseURL {
                    base: "rep-a/".into(),
                    ..Default::default()
                },
                BaseURL {
                    base: "rep-a/".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let bases =
            segment_bases_for_representation(&ctx, &adaptation_set, &representation).unwrap();
        assert_eq!(bases.len(), 1);
        assert!(bases[0].as_str().contains("/rep-a"));
        assert!(bases[0].as_str().contains("/as/"));
        assert_eq!(bases[0].query(), Some("sig=1"));
    }

    #[test]
    fn period_windows_chain_period_starts() {
        let mpd = MPD {
            periods: vec![
                Period {
                    duration: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
                Period {
                    start: Some(Duration::from_secs(10)),
                    duration: Some(Duration::from_secs(5)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let windows = period_windows(&mpd).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].start, Duration::ZERO);
        assert_eq!(windows[0].end, Some(Duration::from_secs(10)));
        assert_eq!(windows[1].start, Duration::from_secs(10));
        assert_eq!(windows[1].end, Some(Duration::from_secs(15)));
    }

    #[test]
    fn current_period_window_static_mpd_starts_at_first_period() {
        let mpd = MPD {
            mpdtype: Some("static".into()),
            periods: vec![
                Period {
                    duration: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
                Period {
                    start: Some(Duration::from_secs(10)),
                    duration: Some(Duration::from_secs(5)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        assert_eq!(current_period_window_at(&mpd, now).unwrap().idx, 0);
    }

    #[test]
    fn current_period_window_selects_by_availability_time() {
        let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        let mpd = MPD {
            availabilityStartTime: Some(ast),
            periods: vec![
                Period {
                    duration: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
                Period {
                    start: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let in_first = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 5).unwrap();
        assert_eq!(current_period_window_at(&mpd, in_first).unwrap().idx, 0);

        let in_second = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 12).unwrap();
        assert_eq!(current_period_window_at(&mpd, in_second).unwrap().idx, 1);
    }

    #[test]
    fn target_presentation_time_applies_suggested_delay() {
        let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        let mpd = MPD {
            availabilityStartTime: Some(ast),
            suggestedPresentationDelay: Some(Duration::from_secs(2)),
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 10).unwrap();
        assert_eq!(
            target_presentation_time_at(&mpd, now).unwrap(),
            Some(Duration::from_secs(8))
        );
    }

    #[test]
    fn target_presentation_time_prefers_service_description_latency() {
        use dash_mpd::{Latency, ServiceDescription};

        let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        let mpd = MPD {
            availabilityStartTime: Some(ast),
            suggestedPresentationDelay: Some(Duration::from_secs(2)),
            ServiceDescription: vec![ServiceDescription {
                Latency: vec![Latency {
                    target: Some(3500.0),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 10).unwrap();
        assert_eq!(
            target_presentation_time_at(&mpd, now).unwrap(),
            Some(Duration::from_secs_f64(6.5))
        );
    }

    #[test]
    fn segment_availability_waits_for_availability_time_offset_when_complete() {
        let seg = TimelineSegment {
            number: 3,
            time: 8000,
            duration: 4000,
            duration_s: 4.0,
            presentation_time_s: 8.0,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        };
        let availability = SegmentAvailability {
            availability_time_offset_s: Some(7.0),
            availability_time_complete: true,
        };
        assert!(!segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(14),
            &availability,
        ));
        assert!(segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(15),
            &availability,
        ));
    }

    #[test]
    fn segment_availability_partial_whole_segment_available_at_sap() {
        let seg = TimelineSegment {
            number: 3,
            time: 8000,
            duration: 4000,
            duration_s: 4.0,
            presentation_time_s: 8.0,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        };
        let availability = SegmentAvailability {
            availability_time_offset_s: Some(7.0),
            availability_time_complete: false,
        };
        assert!(!segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(7),
            &availability,
        ));
        assert!(segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(8),
            &availability,
        ));
    }

    #[test]
    fn segment_availability_uses_sequence_start_for_subsegments() {
        let seg = TimelineSegment {
            number: 1,
            time: 0,
            duration: 1000,
            duration_s: 1.0,
            presentation_time_s: 2.0,
            sub_number: Some(3),
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        };
        assert!((segment_sequence_start_s(Duration::ZERO, &seg) - 0.0).abs() < 1e-6);
        let availability = SegmentAvailability {
            availability_time_offset_s: Some(5.0),
            availability_time_complete: true,
        };
        assert!(!segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(4),
            &availability,
        ));
        assert!(segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(5),
            &availability,
        ));
    }

    #[test]
    fn filter_segments_by_availability_drops_unpublished_complete_live_edge() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            startNumber: Some(1),
            availabilityTimeOffset: Some(7.0),
            availabilityTimeComplete: Some(true),
            ..Default::default()
        };
        let addressing = SegmentAddressing::Template(st.clone());
        let ctx = TimelineBuildContext {
            is_dynamic: true,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: None,
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: Some(Duration::from_secs(20)),
            since_availability_start: Some(Duration::from_secs(12)),
            resync_hints: None,
        };
        let segments = timeline_segments(&st, &ctx).unwrap();
        let filtered = filter_segments_by_availability(
            segments,
            true,
            Duration::ZERO,
            ctx.since_availability_start,
            &addressing,
        );
        let numbers: Vec<_> = filtered.iter().map(|s| s.number).collect();
        assert_eq!(numbers, vec![1, 2]);
    }

    #[test]
    fn filter_segments_by_availability_includes_partial_live_edge_at_sap() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            startNumber: Some(1),
            availabilityTimeOffset: Some(7.0),
            availabilityTimeComplete: Some(false),
            ..Default::default()
        };
        let addressing = SegmentAddressing::Template(st.clone());
        let ctx = TimelineBuildContext {
            is_dynamic: true,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: None,
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: Some(Duration::from_secs(20)),
            since_availability_start: Some(Duration::from_secs(11)),
            resync_hints: None,
        };
        let segments = timeline_segments(&st, &ctx).unwrap();
        let filtered = filter_segments_by_availability(
            segments,
            true,
            Duration::ZERO,
            ctx.since_availability_start,
            &addressing,
        );
        let numbers: Vec<_> = filtered.iter().map(|s| s.number).collect();
        assert_eq!(numbers, vec![1, 2, 3]);
    }

    #[test]
    fn segment_template_inheritance_merges_period_and_adaptation_set() {
        let period = Period {
            SegmentTemplate: Some(SegmentTemplate {
                timescale: Some(1000),
                duration: Some(4000.0),
                startNumber: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let adaptation_set = AdaptationSet {
            SegmentTemplate: Some(SegmentTemplate {
                initialization: Some("init.mp4".into()),
                media: Some("seg-$Number$.m4s".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let timeline = segment_template_for_timeline(&period, &adaptation_set).unwrap();
        assert_eq!(timeline.timescale, Some(1000));
        assert_eq!(timeline.duration, Some(4000.0));
        assert_eq!(timeline.startNumber, Some(1));
        assert_eq!(timeline.initialization.as_deref(), Some("init.mp4"));
        assert_eq!(timeline.media.as_deref(), Some("seg-$Number$.m4s"));
    }

    #[test]
    fn segment_template_inheritance_supplements_timeline_from_representation() {
        let period = Period {
            SegmentTemplate: Some(SegmentTemplate {
                timescale: Some(90000),
                startNumber: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let adaptation_set = AdaptationSet {
            representations: vec![Representation {
                SegmentTemplate: Some(SegmentTemplate {
                    initialization: Some("i.mp4".into()),
                    media: Some("m$Number$.mp4".into()),
                    SegmentTimeline: Some(dash_mpd::SegmentTimeline {
                        segments: vec![dash_mpd::S {
                            t: Some(0),
                            d: 180000,
                            r: Some(1),
                            ..Default::default()
                        }],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        let timeline = segment_template_for_timeline(&period, &adaptation_set).unwrap();
        assert_eq!(timeline.timescale, Some(90000));
        assert!(timeline.SegmentTimeline.is_some());
        assert_eq!(timeline.initialization.as_deref(), Some("i.mp4"));

        let rep = &adaptation_set.representations[0];
        let rep_tpl = segment_template_for_representation(&period, &adaptation_set, rep).unwrap();
        assert_eq!(rep_tpl.media.as_deref(), Some("m$Number$.mp4"));
    }

    #[test]
    fn static_duration_template_emits_expected_segment_count() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: false,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: Some(Duration::from_secs(8)),
            },
            period_duration: None,
            media_presentation_duration: Some(Duration::from_secs(8)),
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
        };

        let segs = timeline_segments(&st, &ctx).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].number, 1);
        assert_eq!(segs[1].number, 2);
    }

    #[test]
    fn dynamic_duration_template_limits_window_to_time_shift_buffer() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: true,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: None,
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: Some(Duration::from_secs(8)),
            since_availability_start: Some(Duration::from_secs(20)),
            resync_hints: None,
        };

        let segs = timeline_segments(&st, &ctx).unwrap();
        assert_eq!(segs.first().map(|s| s.number), Some(2));
        assert_eq!(segs.last().map(|s| s.number), Some(6));
    }

    #[test]
    fn segment_list_explicit_urls_builds_timeline() {
        let sl = SegmentList {
            timescale: Some(1000),
            duration: Some(4000),
            Initialization: Some(dash_mpd::Initialization {
                sourceURL: Some("init.mp4".into()),
                ..Default::default()
            }),
            segment_urls: vec![
                SegmentURL {
                    media: Some("seg-1.m4s".into()),
                    ..Default::default()
                },
                SegmentURL {
                    media: Some("seg-2.m4s".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: false,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: Some(Duration::from_secs(8)),
            },
            period_duration: None,
            media_presentation_duration: Some(Duration::from_secs(8)),
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
        };

        let segs = timeline_segments_from_list(&sl, &ctx).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].media_url.as_deref(), Some("seg-1.m4s"));
        assert_eq!(segs[1].media_url.as_deref(), Some("seg-2.m4s"));
        assert!((segs[0].duration_s - 4.0).abs() < 1e-9);
        assert!((segs[1].presentation_time_s - 4.0).abs() < 1e-9);
    }

    #[test]
    fn segment_list_inheritance_merges_period_and_representation() {
        let period = Period {
            SegmentList: Some(SegmentList {
                timescale: Some(1000),
                duration: Some(2000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let adaptation_set = AdaptationSet {
            representations: vec![Representation {
                SegmentList: Some(SegmentList {
                    Initialization: Some(dash_mpd::Initialization {
                        sourceURL: Some("rep-init.mp4".into()),
                        ..Default::default()
                    }),
                    segment_urls: vec![SegmentURL {
                        media: Some("seg.m4s".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };

        let rep = &adaptation_set.representations[0];
        let merged = segment_list_for_representation(&period, &adaptation_set, rep).unwrap();
        assert_eq!(merged.timescale, Some(1000));
        assert_eq!(merged.duration, Some(2000));
        assert_eq!(
            merged.Initialization.as_ref().unwrap().sourceURL.as_deref(),
            Some("rep-init.mp4")
        );
        assert_eq!(merged.segment_urls.len(), 1);

        let addressing =
            segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
        assert!(matches!(addressing, SegmentAddressing::List(_)));
    }

    #[test]
    fn segment_addressing_prefers_list_over_template() {
        let period = Period::default();
        let adaptation_set = AdaptationSet {
            SegmentTemplate: Some(SegmentTemplate {
                media: Some("tpl-$Number$.m4s".into()),
                ..Default::default()
            }),
            representations: vec![Representation {
                SegmentList: Some(SegmentList {
                    duration: Some(1000),
                    segment_urls: vec![SegmentURL {
                        media: Some("list.m4s".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rep = &adaptation_set.representations[0];
        let addressing =
            segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
        match addressing {
            SegmentAddressing::List(sl) => {
                assert_eq!(sl.segment_urls[0].media.as_deref(), Some("list.m4s"));
            }
            SegmentAddressing::Template(_) => panic!("expected SegmentList addressing"),
            SegmentAddressing::Base(_) => panic!("expected SegmentList addressing"),
        }
    }

    #[test]
    fn parse_byte_range_accepts_inclusive_specifier() {
        let br = parse_byte_range("7-62").unwrap();
        assert_eq!(br.start, 7);
        assert_eq!(br.end, 62);
        assert!(parse_byte_range("bad").is_err());
        assert!(parse_byte_range("10-5").is_err());
    }

    #[test]
    fn segment_addressing_prefers_template_over_base() {
        let period = Period {
            SegmentBase: Some(SegmentBase {
                indexRange: Some("0-10".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let adaptation_set = AdaptationSet {
            representations: vec![Representation {
                SegmentTemplate: Some(SegmentTemplate {
                    media: Some("seg-$Number$.m4s".into()),
                    initialization: Some("init.mp4".into()),
                    duration: Some(4000.0),
                    timescale: Some(1000),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let rep = &adaptation_set.representations[0];
        let addressing =
            segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
        assert!(matches!(addressing, SegmentAddressing::Template(_)));
    }

    #[test]
    fn segment_base_init_target_uses_range_on_base_url() {
        let sb = SegmentBase {
            Initialization: Some(dash_mpd::Initialization {
                range: Some("0-6".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let vars = TemplateVars {
            representation_id: "1",
            bandwidth: None,
            number: None,
            time: None,
            sub_number: None,
        };
        let target = segment_base_init_target(&sb, &vars).unwrap();
        assert_eq!(target.path, "");
        assert_eq!(target.range, Some(ByteRange { start: 0, end: 6 }));
    }

    fn minimal_sidx_bytes(seg_sizes: &[(u32, u32)], timescale: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(0); // version
        body.extend_from_slice(&[0, 0, 0]); // flags
        body.extend_from_slice(&1u32.to_be_bytes()); // reference_id
        body.extend_from_slice(&timescale.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes()); // ept
        body.extend_from_slice(&0u32.to_be_bytes()); // first_offset
        body.extend_from_slice(&0u16.to_be_bytes()); // reserved
        body.extend_from_slice(&(seg_sizes.len() as u16).to_be_bytes());
        for &(size, dur) in seg_sizes {
            body.extend_from_slice(&(size & 0x7FFF_FFFF).to_be_bytes());
            body.extend_from_slice(&dur.to_be_bytes());
            body.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        let mut out = (8 + body.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b"sidx");
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn parse_sidx_index_builds_timeline_with_byte_ranges() {
        let seg1_len = 11u32;
        let seg2_len = 11u32;
        let init_len = 7usize;
        let sidx = minimal_sidx_bytes(&[(seg1_len, 2000), (seg2_len, 2000)], 1000);
        let index_start = init_len;
        let index_end = init_len + sidx.len() - 1;
        let sb = SegmentBase {
            timescale: Some(1000),
            indexRange: Some(format!("{index_start}-{index_end}")),
            ..Default::default()
        };
        let segs = parse_sidx_index(&sb, &sidx).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(
            segs[0].media_range,
            Some(ByteRange {
                start: (index_end + 1) as u64,
                end: (index_end + 1 + seg1_len as usize - 1) as u64,
            })
        );
        assert!((segs[0].duration_s - 2.0).abs() < 1e-9);
        assert!((segs[1].presentation_time_s - 2.0).abs() < 1e-9);
    }
}
