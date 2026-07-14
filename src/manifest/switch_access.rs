//! `AdaptationSet`/`Representation` `Switching` and `RandomAccess` (SISSI).
//!
//! ISO/IEC 23009-1 §5.3.3.4 / §5.3.5.5: `@interval` (in `@timescale` ticks) marks
//! switch-to and random-access opportunities. `Switching@type` is `media` or `bitstream`;
//! `RandomAccess@type` is `closed`, `open`, or `gradual` (unknown types are ignored).
//!
//! `dash-mpd` deserializes `Switching` but not `RandomAccess`; the latter is recovered from
//! raw MPD XML (same approach as [`super::end_numbers`]).

use std::time::Duration;

use dash_mpd::{AdaptationSet, Representation, Switching};
use roxmltree::{Document, Node};

use super::types::TimelineSegment;
use crate::manifest::ManifestError;

/// `Switching@type` strategies (ISO/IEC 23009-1 Table 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SwitchingStrategy {
    /// Re-initialize the switch-to Representation at the switch point.
    Media,
    /// Concatenate bitstreams without re-initialization.
    Bitstream,
}

/// Parsed `Switching` hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SwitchingHint {
    /// `@interval` in `@timescale` ticks.
    pub interval_ticks: u64,
    pub strategy: SwitchingStrategy,
}

/// `RandomAccess@type` strategies (ISO/IEC 23009-1 Table 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RandomAccessStrategy {
    /// Closed-GOP RAP (SAP type 1 or 2).
    Closed,
    /// Open-GOP RAP (SAP type 1–3).
    Open,
    /// Gradual decoder refresh (SAP type 1–4).
    Gradual,
}

/// Parsed `RandomAccess` hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RandomAccessHint {
    /// `@interval` in `@timescale` ticks.
    pub interval_ticks: u64,
    pub strategy: RandomAccessStrategy,
    pub min_buffer_time: Option<Duration>,
    pub bandwidth: Option<u64>,
}

/// `RandomAccess` elements recovered from raw MPD XML (`dash-mpd` omits them).
#[derive(Debug, Clone, Default)]
pub(crate) struct RandomAccessSupplements {
    periods: Vec<PeriodRandomAccess>,
}

#[derive(Debug, Clone, Default)]
struct PeriodRandomAccess {
    adaptation_sets: Vec<AdaptationSetRandomAccess>,
}

#[derive(Debug, Clone, Default)]
struct AdaptationSetRandomAccess {
    entries: Vec<RandomAccessHint>,
    representations: Vec<Vec<RandomAccessHint>>,
}

fn xml_element_name(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn parse_duration_attr(raw: &str) -> Option<Duration> {
    // Accept xs:duration forms already used elsewhere via dash-mpd (PT…S).
    // Minimal fallback: numeric seconds.
    if let Ok(secs) = raw.parse::<f64>() {
        if secs.is_finite() && secs >= 0.0 {
            return Some(Duration::from_secs_f64(secs));
        }
    }
    parse_xs_duration(raw)
}

fn parse_xs_duration(raw: &str) -> Option<Duration> {
    let s = raw.trim();
    if !s.starts_with('P') && !s.starts_with('p') {
        return None;
    }
    // Only need PT#H#M#S / PT#S for MPD buffer times.
    let t = s.find('T').or_else(|| s.find('t'))?;
    let time = &s[t + 1..];
    let mut total = 0.0_f64;
    let mut num = String::new();
    for ch in time.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
            continue;
        }
        let v: f64 = num.parse().ok()?;
        num.clear();
        match ch {
            'H' | 'h' => total += v * 3600.0,
            'M' | 'm' => total += v * 60.0,
            'S' | 's' => total += v,
            _ => return None,
        }
    }
    if !num.is_empty() {
        return None;
    }
    Some(Duration::from_secs_f64(total.max(0.0)))
}

fn parse_random_access_node(node: Node<'_, '_>) -> Option<RandomAccessHint> {
    let interval_ticks: u64 = node.attribute("interval")?.parse().ok()?;
    if interval_ticks == 0 {
        return None;
    }
    let strategy = match node.attribute("type").unwrap_or("closed") {
        s if s.eq_ignore_ascii_case("closed") => RandomAccessStrategy::Closed,
        s if s.eq_ignore_ascii_case("open") => RandomAccessStrategy::Open,
        s if s.eq_ignore_ascii_case("gradual") => RandomAccessStrategy::Gradual,
        _ => return None, // unknown → ignore element (ISO/IEC 23009-1)
    };
    let min_buffer_time = node
        .attribute("minBufferTime")
        .and_then(parse_duration_attr);
    let bandwidth = node.attribute("bandwidth").and_then(|v| v.parse().ok());
    Some(RandomAccessHint {
        interval_ticks,
        strategy,
        min_buffer_time,
        bandwidth,
    })
}

fn random_access_children(parent: Node<'_, '_>) -> Vec<RandomAccessHint> {
    parent
        .children()
        .filter(|n| xml_element_name(*n, "RandomAccess"))
        .filter_map(parse_random_access_node)
        .collect()
}

fn parse_adaptation_set_random_access(as_node: Node<'_, '_>) -> AdaptationSetRandomAccess {
    let entries = random_access_children(as_node);
    let representations = as_node
        .children()
        .filter(|n| xml_element_name(*n, "Representation"))
        .map(random_access_children)
        .collect();
    AdaptationSetRandomAccess {
        entries,
        representations,
    }
}

fn parse_period_random_access(period_node: Node<'_, '_>) -> PeriodRandomAccess {
    let adaptation_sets = period_node
        .children()
        .filter(|n| xml_element_name(*n, "AdaptationSet"))
        .map(parse_adaptation_set_random_access)
        .collect();
    PeriodRandomAccess { adaptation_sets }
}

/// Parse `RandomAccess` elements from raw MPD XML (indexed like `Period.adaptations`).
pub(crate) fn parse_random_access_supplements(
    mpd_xml: &str,
) -> Result<RandomAccessSupplements, ManifestError> {
    let doc = Document::parse(mpd_xml)
        .map_err(|e| ManifestError::Parse(dash_mpd::DashMpdError::Parsing(e.to_string())))?;
    let periods = doc
        .root_element()
        .children()
        .filter(|n| xml_element_name(*n, "Period"))
        .map(parse_period_random_access)
        .collect();
    Ok(RandomAccessSupplements { periods })
}

impl RandomAccessSupplements {
    /// Merge AdaptationSet- and Representation-level `RandomAccess` for a ladder member.
    pub(crate) fn hints_for(
        &self,
        period_idx: usize,
        adaptation_idx: usize,
        representation_idx: Option<usize>,
    ) -> Vec<RandomAccessHint> {
        let Some(period) = self.periods.get(period_idx) else {
            return Vec::new();
        };
        let Some(aset) = period.adaptation_sets.get(adaptation_idx) else {
            return Vec::new();
        };
        let mut out = aset.entries.clone();
        if let Some(rep_idx) = representation_idx {
            if let Some(rep_hints) = aset.representations.get(rep_idx) {
                out.extend(rep_hints.iter().copied());
            }
        }
        out
    }
}

fn parse_switching(sw: &Switching) -> Option<SwitchingHint> {
    let interval_ticks = sw.interval.filter(|i| *i > 0)?;
    let strategy = match sw.stype.as_deref().unwrap_or("media") {
        s if s.eq_ignore_ascii_case("bitstream") => SwitchingStrategy::Bitstream,
        s if s.eq_ignore_ascii_case("media") => SwitchingStrategy::Media,
        _ => SwitchingStrategy::Media,
    };
    Some(SwitchingHint {
        interval_ticks,
        strategy,
    })
}

/// Collect `Switching` hints from an Adaptation Set and optional Representation.
pub(crate) fn switching_hints_for(
    adaptation_set: &AdaptationSet,
    representation: Option<&Representation>,
) -> Vec<SwitchingHint> {
    let mut out: Vec<SwitchingHint> = adaptation_set
        .Switching
        .iter()
        .filter_map(parse_switching)
        .collect();
    if let Some(rep) = representation {
        out.extend(rep.Switching.iter().filter_map(parse_switching));
    }
    out
}

/// Earliest presentation time of `seg` in `@timescale` ticks (SegmentTimeline / `$Time$` domain).
pub(crate) fn earliest_presentation_ticks(seg: &TimelineSegment) -> u64 {
    match seg.sub_number {
        Some(sub) if sub > 1 => {
            let prior = sub.saturating_sub(1);
            seg.time.saturating_add(prior.saturating_mul(seg.duration))
        }
        _ => seg.time,
    }
}

/// Whether `seg` is an opportunity for an `@interval` grid (ISO/IEC 23009-1 SISSI).
pub(crate) fn is_interval_opportunity(seg: &TimelineSegment, interval_ticks: u64) -> bool {
    if interval_ticks == 0 {
        return true;
    }
    earliest_presentation_ticks(seg) % interval_ticks == 0
}

/// True when any `Switching` hint marks `seg` as a switch-to opportunity.
pub(crate) fn is_switch_opportunity(seg: &TimelineSegment, hints: &[SwitchingHint]) -> bool {
    if hints.is_empty() {
        return true;
    }
    hints
        .iter()
        .any(|h| is_interval_opportunity(seg, h.interval_ticks))
}

/// `Switching@type=bitstream` opportunity for `seg`, if any.
pub(crate) fn bitstream_switch_opportunity(seg: &TimelineSegment, hints: &[SwitchingHint]) -> bool {
    hints.iter().any(|h| {
        h.strategy == SwitchingStrategy::Bitstream && is_interval_opportunity(seg, h.interval_ticks)
    })
}

/// Smallest `RandomAccess@interval` in ticks, when present.
pub(crate) fn random_access_interval_ticks(hints: &[RandomAccessHint]) -> Option<u64> {
    hints.iter().map(|h| h.interval_ticks).min()
}

/// Snap `start_idx` back to the nearest prior segment on a `RandomAccess@interval` grid.
pub(crate) fn align_start_index_to_random_access(
    segments: &[TimelineSegment],
    start_idx: usize,
    hints: &[RandomAccessHint],
) -> usize {
    let Some(interval) = random_access_interval_ticks(hints).filter(|i| *i > 0) else {
        return start_idx;
    };
    if segments.is_empty() {
        return 0;
    }
    let idx = start_idx.min(segments.len() - 1);
    segments[..idx + 1]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, s)| is_interval_opportunity(s, interval))
        .map(|(i, _)| i)
        .unwrap_or(idx)
}

/// Whether any collected `Switching` declares bitstream strategy (may replace `@bitstreamSwitching`).
pub(crate) fn switching_declares_bitstream(hints: &[SwitchingHint]) -> bool {
    hints
        .iter()
        .any(|h| h.strategy == SwitchingStrategy::Bitstream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_mpd::AdaptationSet;

    fn seg(time: u64, duration: u64) -> TimelineSegment {
        TimelineSegment {
            number: 1,
            time,
            duration,
            duration_s: duration as f64 / 1000.0,
            presentation_time_s: time as f64 / 1000.0,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        }
    }

    #[test]
    fn switch_opportunity_on_interval_grid() {
        let hints = [SwitchingHint {
            interval_ticks: 4000,
            strategy: SwitchingStrategy::Media,
        }];
        assert!(is_switch_opportunity(&seg(0, 2000), &hints));
        assert!(!is_switch_opportunity(&seg(2000, 2000), &hints));
        assert!(is_switch_opportunity(&seg(4000, 2000), &hints));
    }

    #[test]
    fn empty_switching_allows_any_segment() {
        assert!(is_switch_opportunity(&seg(123, 1000), &[]));
    }

    #[test]
    fn bitstream_opportunity_requires_bitstream_type() {
        let media = [SwitchingHint {
            interval_ticks: 2000,
            strategy: SwitchingStrategy::Media,
        }];
        let bitstream = [SwitchingHint {
            interval_ticks: 2000,
            strategy: SwitchingStrategy::Bitstream,
        }];
        assert!(!bitstream_switch_opportunity(&seg(2000, 2000), &media));
        assert!(bitstream_switch_opportunity(&seg(2000, 2000), &bitstream));
    }

    #[test]
    fn switching_hints_from_adaptation_set() {
        let aset = AdaptationSet {
            Switching: vec![Switching {
                interval: Some(8000),
                stype: Some("bitstream".into()),
            }],
            ..Default::default()
        };
        let hints = switching_hints_for(&aset, None);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].interval_ticks, 8000);
        assert_eq!(hints[0].strategy, SwitchingStrategy::Bitstream);
        assert!(switching_declares_bitstream(&hints));
    }

    #[test]
    fn parse_random_access_from_mpd_xml() {
        let xml = r#"<?xml version="1.0"?>
        <MPD xmlns="urn:mpeg:dash:schema:mpd:2011">
          <Period>
            <AdaptationSet mimeType="video/mp4">
              <RandomAccess interval="4000" type="closed"/>
              <RandomAccess interval="2000" type="unknown"/>
              <Representation id="1" bandwidth="100000">
                <RandomAccess interval="8000" type="open" bandwidth="90000"
                              minBufferTime="PT1.5S"/>
              </Representation>
            </AdaptationSet>
          </Period>
        </MPD>"#;
        let supplements = parse_random_access_supplements(xml).expect("parse");
        let as_hints = supplements.hints_for(0, 0, None);
        assert_eq!(as_hints.len(), 1);
        assert_eq!(as_hints[0].interval_ticks, 4000);
        assert_eq!(as_hints[0].strategy, RandomAccessStrategy::Closed);

        let merged = supplements.hints_for(0, 0, Some(0));
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].interval_ticks, 8000);
        assert_eq!(merged[1].strategy, RandomAccessStrategy::Open);
        assert_eq!(merged[1].bandwidth, Some(90_000));
        assert_eq!(
            merged[1].min_buffer_time,
            Some(Duration::from_secs_f64(1.5))
        );
    }

    #[test]
    fn align_start_to_random_access_rewinds_to_grid() {
        let hints = [RandomAccessHint {
            interval_ticks: 4000,
            strategy: RandomAccessStrategy::Closed,
            min_buffer_time: None,
            bandwidth: None,
        }];
        let segments = vec![
            seg(0, 2000),
            seg(2000, 2000),
            seg(4000, 2000),
            seg(6000, 2000),
        ];
        assert_eq!(align_start_index_to_random_access(&segments, 3, &hints), 2);
        assert_eq!(align_start_index_to_random_access(&segments, 1, &hints), 0);
    }
}
