use dash_mpd::{SegmentBase, SegmentList, SegmentTemplate, SegmentURL};

use crate::manifest::ManifestError;

use super::addressing::SegmentAddressing;
use super::addressing::segment_template_uses_global_sidecar_index;
use super::types::{ByteRange, TimelineBuildContext, TimelineSegment, parse_byte_range};

fn segment_url_media_range(su: &SegmentURL) -> Result<Option<ByteRange>, ManifestError> {
    su.mediaRange.as_deref().map(parse_byte_range).transpose()
}

/// ISO/IEC 23009-1 §5.3.9.5.3 — the last `@duration` segment in a Period may be shorter.
fn bound_last_static_duration_segment(
    segments: &mut [TimelineSegment],
    period_length_s: f64,
    timescale: u64,
) {
    let Some(last) = segments.last_mut() else {
        return;
    };
    let remaining_s = (period_length_s - last.presentation_time_s).max(0.0);
    if remaining_s + 1e-9 < last.duration_s {
        last.duration_s = remaining_s;
        last.duration = (remaining_s * timescale as f64).round().max(1.0) as u64;
    }
}

fn static_duration_segment_count(
    start_number: u64,
    duration_s: f64,
    end_number: Option<u64>,
    ctx: &TimelineBuildContext,
) -> Result<u64, ManifestError> {
    if let Some(end_num) = end_number {
        if end_num < start_number {
            return Err(ManifestError::InvalidSegmentTemplateEndNumber);
        }
        return Ok(end_num - start_number + 1);
    }

    let period_duration_s = ctx
        .period_length_secs()
        .filter(|x| x.is_finite() && *x > 0.0)
        .ok_or(ManifestError::MissingPeriodExtentForStaticTemplate)?;
    Ok(((period_duration_s / duration_s).ceil() as u64).max(1))
}
pub(crate) fn timeline_segments_for_addressing(
    addressing: &SegmentAddressing,
    ctx: &TimelineBuildContext,
    end_number: Option<u64>,
) -> Result<Vec<TimelineSegment>, ManifestError> {
    match addressing {
        SegmentAddressing::Template(st) if segment_template_uses_global_sidecar_index(st) => {
            Err(ManifestError::SegmentTemplateIndexNotLoaded)
        }
        SegmentAddressing::Template(st) => timeline_segments(st, ctx, end_number),
        SegmentAddressing::List(sl) => timeline_segments_from_list(sl, ctx),
        SegmentAddressing::Base(sb) => timeline_segments_from_segment_base(sb, ctx),
    }
}

/// Whole-file `SegmentBase` without `@indexRange` / `RepresentationIndex`: a single media
/// segment at the Representation `BaseURL` (ISO/IEC 23009-1 §5.3.9.2 / §5.3.9.5).
///
/// Duration preference: `@presentationDuration`, else Period / MPD presentation extent.
fn timeline_segments_from_segment_base(
    sb: &SegmentBase,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, ManifestError> {
    if super::addressing::segment_base_uses_sidx_index(sb) {
        return Err(ManifestError::SegmentBaseIndexNotLoaded);
    }

    let timescale = sb.timescale.unwrap_or(1);
    if timescale == 0 {
        return Err(ManifestError::ZeroTimescale);
    }

    let (duration_ticks, duration_s) = if let Some(d) = sb.presentationDuration.filter(|d| *d > 0) {
        (d, d as f64 / timescale as f64)
    } else {
        let duration_s = ctx
            .period_length_secs()
            .filter(|x| x.is_finite() && *x > 0.0)
            .ok_or(ManifestError::MissingPeriodExtentForStaticTemplate)?;
        let duration_ticks = (duration_s * timescale as f64).round().max(1.0) as u64;
        (duration_ticks, duration_s)
    };
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

pub(crate) fn timeline_segments_from_list(
    sl: &SegmentList,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let segments = if let Some(timeline) = sl.SegmentTimeline.as_ref() {
        segments_from_list_timeline(sl, timeline, ctx)?
    } else if !sl.segment_urls.is_empty() {
        segments_from_list_urls(sl)?
    } else {
        return Err(ManifestError::EmptySegmentList);
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
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let pseudo_st = SegmentTemplate {
        timescale: sl.timescale,
        presentationTimeOffset: Some(0),
        startNumber: Some(1),
        SegmentTimeline: Some(timeline.clone()),
        ..Default::default()
    };
    let mut segments = segments_from_explicit_timeline(&pseudo_st, timeline, ctx)?;

    if !sl.segment_urls.is_empty() && sl.segment_urls.len() != segments.len() {
        return Err(ManifestError::SegmentListUrlTimelineMismatch);
    }

    for (seg, su) in segments.iter_mut().zip(sl.segment_urls.iter()) {
        seg.media_url = su.media.clone();
        seg.media_range = segment_url_media_range(su)?;
    }

    Ok(segments)
}

fn segments_from_list_urls(sl: &SegmentList) -> Result<Vec<TimelineSegment>, ManifestError> {
    let duration_ticks = sl
        .duration
        .filter(|d| *d > 0)
        .ok_or(ManifestError::MissingSegmentDuration)?;
    let timescale = sl.timescale.unwrap_or(1);
    if timescale == 0 {
        return Err(ManifestError::ZeroTimescale);
    }
    let duration_s = duration_ticks as f64 / timescale as f64;

    sl.segment_urls
        .iter()
        .enumerate()
        .map(|(i, su)| {
            Ok(TimelineSegment {
                number: (i as u64).saturating_add(1),
                time: (i as u64).saturating_mul(duration_ticks),
                duration: duration_ticks,
                duration_s,
                presentation_time_s: i as f64 * duration_s,
                sub_number: None,
                resync_start_chunk: None,
                media_url: su.media.clone(),
                media_range: segment_url_media_range(su)?,
            })
        })
        .collect()
}
pub(crate) fn timeline_segments(
    st: &dash_mpd::SegmentTemplate,
    ctx: &TimelineBuildContext,
    end_number: Option<u64>,
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let segments = if let Some(timeline) = st.SegmentTimeline.as_ref() {
        segments_from_explicit_timeline(st, timeline, ctx)?
    } else {
        segments_from_duration_template(st, ctx, end_number)?
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
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let timescale = st.timescale.unwrap_or(1);
    if timescale == 0 {
        return Err(ManifestError::ZeroTimescale);
    }

    let presentation_time_offset = st.presentationTimeOffset.unwrap_or(0);
    let mut segments = Vec::new();

    let mut current_number = st.startNumber.unwrap_or(1);
    let mut current_time: Option<u64> = None;

    let period_start_s = ctx.period_window.start.as_secs_f64();
    const MAX_EXPANSION: usize = 1_000_000;

    for (seg_idx, s) in timeline.segments.iter().enumerate() {
        if s.d == 0 {
            return Err(ManifestError::ZeroTimelineSegmentDuration);
        }

        let k = s.k.unwrap_or(1);
        if k == 0 {
            return Err(ManifestError::InvalidTimelineSegmentK);
        }
        if k > 1 && s.d % k != 0 {
            return Err(ManifestError::TimelineDNotDivisibleByK);
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
                    return Err(ManifestError::UnboundedSegmentTimelineRepeat);
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
                    return Err(ManifestError::UnboundedSegmentTimelineRepeat);
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
) -> Result<(), ManifestError> {
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
) -> Result<NegativeRepeatEnd, ManifestError> {
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
            return Err(ManifestError::MissingAvailabilityStartForDynamicTemplate);
        };
        return Ok(NegativeRepeatEnd::MpdSeconds(since.as_secs_f64()));
    }

    Err(ManifestError::UnboundedSegmentTimelineRepeat)
}

/// For dynamic MPDs with `SegmentTimeline`, keep segments in the time-shift buffer (same idea as
/// `segments_from_duration_template`): MPD media time in `[now - TSBD, now]`, capped at the
/// known presentation / Period end when `@mediaPresentationDuration` (or Period end) is set.
fn filter_explicit_timeline_for_dynamic_window(
    segments: Vec<TimelineSegment>,
    ctx: &TimelineBuildContext,
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let Some(since_ast) = ctx.since_availability_start else {
        return Err(ManifestError::MissingAvailabilityStartForDynamicTemplate);
    };
    let period_start_s = ctx.period_window.start.as_secs_f64();
    let now_s = since_ast.as_secs_f64();
    let tsbd_s = ctx
        .time_shift_buffer_depth
        .map(|x| x.as_secs_f64())
        .filter(|x| x.is_finite() && *x > 0.0)
        .unwrap_or(120.0);
    let window_start_s = (now_s - tsbd_s).max(period_start_s);
    let period_end_s = ctx.period_end_secs();

    Ok(segments
        .into_iter()
        .filter(|s| {
            let abs_s = period_start_s + s.presentation_time_s;
            if abs_s < window_start_s - 1e-6 || abs_s > now_s + 1e-6 {
                return false;
            }
            // Known presentation / Period end: exclude segments that start at or after it.
            period_end_s.is_none_or(|end| abs_s + 1e-9 < end)
        })
        .collect())
}

/// SegmentTemplate with `@duration` / `@timescale` / `@startNumber` but no `SegmentTimeline`.
fn segments_from_duration_template(
    st: &dash_mpd::SegmentTemplate,
    ctx: &TimelineBuildContext,
    end_number: Option<u64>,
) -> Result<Vec<TimelineSegment>, ManifestError> {
    let d = st
        .duration
        .filter(|x| *x > 0.0)
        .ok_or(ManifestError::MissingSegmentDuration)?;
    let timescale = st.timescale.unwrap_or(1);
    if timescale == 0 {
        return Err(ManifestError::ZeroTimescale);
    }
    let presentation_time_offset = st.presentationTimeOffset.unwrap_or(0);
    let start_number = st.startNumber.unwrap_or(1);
    let duration_s = d / timescale as f64;
    let duration_ticks = d.round().max(1.0) as u64;

    if ctx.is_dynamic {
        let Some(since_ast) = ctx.since_availability_start else {
            return Err(ManifestError::MissingAvailabilityStartForDynamicTemplate);
        };
        let period_start_s = ctx.period_window.start.as_secs_f64();
        // Cap the live edge at the known presentation / Period end (dynamic + static duration).
        let mut t_in_period = (since_ast.as_secs_f64() - period_start_s).max(0.0);
        if let Some(period_length_s) = ctx.period_length_secs() {
            t_in_period = t_in_period.min(period_length_s);
        }
        let mut end_n = start_number + (t_in_period / duration_s).floor() as u64;
        // Segments start at presentation times < period end; the last open interval may
        // begin at `floor((period_length - epsilon) / duration) * duration`.
        if let Some(period_length_s) = ctx.period_length_secs() {
            if period_length_s > 0.0 {
                let max_start_s = (period_length_s - 1e-9).max(0.0);
                let max_n = start_number + (max_start_s / duration_s).floor() as u64;
                end_n = end_n.min(max_n);
            }
        }
        if let Some(en) = end_number {
            end_n = end_n.min(en);
        }
        if end_n < start_number {
            return Ok(Vec::new());
        }

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
        if let Some(period_length_s) = ctx.period_length_secs() {
            bound_last_static_duration_segment(&mut segments, period_length_s, timescale);
        }
        Ok(segments)
    } else {
        let count = static_duration_segment_count(start_number, duration_s, end_number, ctx)?;
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
        if let Some(period_length_s) = ctx.period_length_secs() {
            bound_last_static_duration_segment(&mut segments, period_length_s, timescale);
        }
        Ok(segments)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
