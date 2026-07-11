use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dash_mpd::{AdaptationSet, BaseURL, MPD, Period, Representation, SegmentList, SegmentTemplate};
use reqwest::Client;
use url::Url;

use super::PlayerError;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum MimeType {
    Audio,
    Video,
}

impl MimeType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MimeType::Audio => "audio/mp4",
            MimeType::Video => "video/mp4",
        }
    }
}

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
    /// Explicit `SegmentURL@media` when using `SegmentList` addressing (may be rep-specific).
    pub media_url: Option<String>,
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

pub(crate) async fn fetch_mpd(client: &Client, manifest_uri: &Url) -> Result<MPD, PlayerError> {
    let response = client.get(manifest_uri.clone()).send().await?;
    let text = response.text().await?;
    Ok(dash_mpd::parse(&text)?)
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

pub(crate) fn current_period_window_at(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
) -> Result<PeriodWindow, PlayerError> {
    let windows = period_windows(mpd)?;

    // No availabilityStartTime => cannot map wall-clock to a Period reliably.
    // Fall back to the last Period window.
    let Some(since_ast) = since_availability_start_at(mpd, wall_now)? else {
        return Ok(*windows.last().expect("checked non-empty"));
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
    bases = expand_base_layer(bases, &ctx.mpd_base_urls)?;
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
    Ok(SegmentAddressing::Template(segment_template_for_timeline(
        period,
        adaptation_set,
    )?))
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
    Ok(SegmentAddressing::Template(
        segment_template_for_representation(period, adaptation_set, representation)?,
    ))
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
    }
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
            media_url: su.media.clone(),
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

/// Very small subset of DASH `$...$` template interpolation (incl. `$SubNumber$` for §5.3.9.6.5).
pub(crate) fn interpolate_template(
    template: &str,
    representation_id: &str,
    number: Option<u64>,
    time: Option<u64>,
    sub_number: Option<u64>,
) -> String {
    let mut out = template.replace("$RepresentationID$", representation_id);
    if let Some(n) = number {
        out = out.replace("$Number$", &n.to_string());
    }
    if let Some(t) = time {
        out = out.replace("$Time$", &t.to_string());
    }
    if let Some(sn) = sub_number {
        out = out.replace("$SubNumber$", &sn.to_string());
    } else if out.contains("$SubNumber$") {
        // §5.3.9.6.5: first chunk in a sequence is 1; single-chunk sequences use k=1.
        out = out.replace("$SubNumber$", "1");
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
            media_url: None,
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
                media_url: None,
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
                media_url: None,
            });
        }
        Ok(segments)
    }
}

pub(crate) fn target_presentation_time_at(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
) -> Result<Option<Duration>, PlayerError> {
    let Some(mut t) = since_availability_start_at(mpd, wall_now)? else {
        return Ok(None);
    };

    // Target "safe live edge" = now - suggestedPresentationDelay (if present).
    if let Some(delay) = mpd.suggestedPresentationDelay {
        t = t.saturating_sub(delay);
    }

    Ok(Some(t))
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
        let out = interpolate_template(
            "v-$RepresentationID$-$Number$-$Time$-$SubNumber$.m4s",
            "A",
            Some(7),
            Some(42),
            Some(3),
        );
        assert_eq!(out, "v-A-7-42-3.m4s");
    }

    #[test]
    fn interpolate_template_subnumber_defaults_to_one() {
        let out = interpolate_template("x-$SubNumber$.m4s", "id", None, None, None);
        assert_eq!(out, "x-1.m4s");
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
                media_url: None,
            },
            TimelineSegment {
                number: 1,
                time: 0,
                duration: 1000,
                duration_s: 1.0,
                presentation_time_s: 1.0,
                sub_number: Some(2),
                media_url: None,
            },
            TimelineSegment {
                number: 1,
                time: 0,
                duration: 1000,
                duration_s: 1.0,
                presentation_time_s: 2.0,
                sub_number: Some(3),
                media_url: None,
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
                media_url: None,
            },
            TimelineSegment {
                number: 1,
                time: 0,
                duration: 1000,
                duration_s: 1.0,
                presentation_time_s: 1.0,
                sub_number: Some(2),
                media_url: None,
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
        AdaptationSet, Period, Representation, SegmentList, SegmentTemplate, SegmentURL,
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
        }
    }
}
