//! Period continuity / connectivity signalling (ISO/IEC 23009-1 §5.3.2.4).
//!
//! Adaptation Sets may declare that they continue a previous Period via SupplementalProperty
//! schemes `urn:mpeg:dash:period-continuity:2015` or `urn:mpeg:dash:period-connectivity:2015`.
//! Continuity implies continuous sample timelines; connectivity implies equivalent Initialization
//! Segments with timelines that may jump (adjusted by `@presentationTimeOffset`).

use std::collections::BTreeSet;

use dash_mpd::{AdaptationSet, MPD, Period, SupplementalProperty};

/// How two adjacent Periods relate for a matched Adaptation Set pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PeriodLink {
    /// Sample timeline continuous; init may be reused.
    Continuous,
    /// Init equivalent; presentation times may be discontinuous (PTO-adjusted).
    Connected,
    /// No continuity/connectivity; treat as a hard period boundary.
    Discontinuous,
}

impl PeriodLink {
    /// Whether the link permits reusing Init and overlap dedup across the boundary.
    pub(crate) fn allows_soft_transition(self) -> bool {
        matches!(self, Self::Continuous | Self::Connected)
    }
}

const PERIOD_CONTINUITY_SCHEME: &str = "urn:mpeg:dash:period-continuity:2015";
const PERIOD_CONNECTIVITY_SCHEME: &str = "urn:mpeg:dash:period-connectivity:2015";

/// Strongest link declared between Periods `from_idx` and `to_idx` for any matching AS pair.
///
/// Prefers Continuity over Connectivity when both appear. Returns [`PeriodLink::Discontinuous`]
/// when either index is out of range or no Adaptation Sets link.
pub(crate) fn period_link(mpd: &MPD, from_idx: usize, to_idx: usize) -> PeriodLink {
    let Some(prev) = mpd.periods.get(from_idx) else {
        return PeriodLink::Discontinuous;
    };
    let Some(next) = mpd.periods.get(to_idx) else {
        return PeriodLink::Discontinuous;
    };
    let prev_id = prev.id.as_deref();

    let mut best = PeriodLink::Discontinuous;
    for next_as in &next.adaptations {
        for prev_as in &prev.adaptations {
            let link = adaptation_set_period_link(prev_as, prev_id, next_as);
            best = strongest_link(best, link);
        }
    }

    if best == PeriodLink::Discontinuous && asset_identifiers_equal(prev, next) {
        // Spec suggestion: identical AssetIdentifiers imply associated Periods; when AS `@id`
        // values match, treat as Continuous for soft transitions.
        for next_as in &next.adaptations {
            for prev_as in &prev.adaptations {
                if adaptation_sets_match_ids(prev_as, next_as) {
                    return PeriodLink::Continuous;
                }
            }
        }
    }

    best
}

/// Link declared on `next` toward `prev_period_id` for a specific Adaptation Set pair.
pub(crate) fn adaptation_set_period_link(
    prev: &AdaptationSet,
    prev_period_id: Option<&str>,
    next: &AdaptationSet,
) -> PeriodLink {
    if !adaptation_sets_match_ids(prev, next) {
        return PeriodLink::Discontinuous;
    }
    let Some(prev_id) = prev_period_id else {
        return PeriodLink::Discontinuous;
    };

    let mut best = PeriodLink::Discontinuous;
    for property in &next.supplemental_property {
        let Some(link) = descriptor_link(property, prev_id) else {
            continue;
        };
        best = strongest_link(best, link);
    }
    best
}

/// Whether `next` is linked to `prev` under the given link kind (descriptor + id matching).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn adaptation_sets_linked(
    prev: &AdaptationSet,
    prev_period_id: Option<&str>,
    next: &AdaptationSet,
    kind: PeriodLink,
) -> bool {
    if !kind.allows_soft_transition() {
        return false;
    }
    let actual = adaptation_set_period_link(prev, prev_period_id, next);
    match kind {
        PeriodLink::Continuous => actual == PeriodLink::Continuous,
        PeriodLink::Connected => matches!(actual, PeriodLink::Connected | PeriodLink::Continuous),
        PeriodLink::Discontinuous => false,
    }
}

fn descriptor_link(property: &SupplementalProperty, prev_period_id: &str) -> Option<PeriodLink> {
    let value = property.value.as_deref()?;
    if value != prev_period_id {
        return None;
    }
    if scheme_eq(&property.schemeIdUri, PERIOD_CONTINUITY_SCHEME) {
        return Some(PeriodLink::Continuous);
    }
    if scheme_eq(&property.schemeIdUri, PERIOD_CONNECTIVITY_SCHEME) {
        return Some(PeriodLink::Connected);
    }
    None
}

fn adaptation_sets_match_ids(prev: &AdaptationSet, next: &AdaptationSet) -> bool {
    match (&prev.id, &next.id) {
        (Some(a), Some(b)) if a == b => representation_ids(prev) == representation_ids(next),
        _ => false,
    }
}

fn representation_ids(adaptation_set: &AdaptationSet) -> BTreeSet<&str> {
    adaptation_set
        .representations
        .iter()
        .filter_map(|r| r.id.as_deref())
        .collect()
}

fn asset_identifiers_equal(prev: &Period, next: &Period) -> bool {
    match (&prev.asset_identifier, &next.asset_identifier) {
        (Some(a), Some(b)) => {
            a.schemeIdUri == b.schemeIdUri
                && a.value == b.value
                && a.scte214ContentIdentifiers == b.scte214ContentIdentifiers
        }
        _ => false,
    }
}

fn strongest_link(a: PeriodLink, b: PeriodLink) -> PeriodLink {
    use PeriodLink::*;
    match (a, b) {
        (Continuous, _) | (_, Continuous) => Continuous,
        (Connected, _) | (_, Connected) => Connected,
        _ => Discontinuous,
    }
}

fn scheme_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> MPD {
        dash_mpd::parse(xml).expect("mpd")
    }

    #[test]
    fn period_link_continuity_from_supplemental_property() {
        let mpd = parse(
            r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16S" minBufferTime="PT2S">
  <Period id="p1" duration="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
  <Period id="p2" start="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <SupplementalProperty schemeIdUri="urn:mpeg:dash:period-continuity:2015" value="p1"/>
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
</MPD>"#,
        );
        assert_eq!(period_link(&mpd, 0, 1), PeriodLink::Continuous);
    }

    #[test]
    fn period_link_connectivity_from_supplemental_property() {
        let mpd = parse(
            r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16S" minBufferTime="PT2S">
  <Period id="p1" duration="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
  <Period id="p2" start="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <SupplementalProperty schemeIdUri="urn:mpeg:dash:period-connectivity:2015" value="p1"/>
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
</MPD>"#,
        );
        assert_eq!(period_link(&mpd, 0, 1), PeriodLink::Connected);
    }

    #[test]
    fn period_link_discontinuous_without_signalling() {
        let mpd = parse(
            r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16S" minBufferTime="PT2S">
  <Period id="p1" duration="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
  <Period id="p2" start="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
</MPD>"#,
        );
        assert_eq!(period_link(&mpd, 0, 1), PeriodLink::Discontinuous);
    }

    #[test]
    fn period_link_requires_matching_representation_ids() {
        let mpd = parse(
            r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16S" minBufferTime="PT2S">
  <Period id="p1" duration="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
  <Period id="p2" start="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <SupplementalProperty schemeIdUri="urn:mpeg:dash:period-continuity:2015" value="p1"/>
      <Representation id="r2" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
</MPD>"#,
        );
        assert_eq!(period_link(&mpd, 0, 1), PeriodLink::Discontinuous);
    }

    #[test]
    fn period_link_asset_identifier_implies_continuous() {
        let mpd = parse(
            r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16S" minBufferTime="PT2S">
  <Period id="p1" duration="PT8S">
    <AssetIdentifier schemeIdUri="urn:org:example:asset" value="movie-1"/>
    <AdaptationSet id="1" mimeType="video/mp4">
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
  <Period id="p2" start="PT8S">
    <AssetIdentifier schemeIdUri="urn:org:example:asset" value="movie-1"/>
    <AdaptationSet id="1" mimeType="video/mp4">
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
</MPD>"#,
        );
        assert_eq!(period_link(&mpd, 0, 1), PeriodLink::Continuous);
    }

    #[test]
    fn continuity_preferred_over_connectivity() {
        let mpd = parse(
            r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16S" minBufferTime="PT2S">
  <Period id="p1" duration="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
  <Period id="p2" start="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <SupplementalProperty schemeIdUri="urn:mpeg:dash:period-connectivity:2015" value="p1"/>
      <SupplementalProperty schemeIdUri="urn:mpeg:dash:period-continuity:2015" value="p1"/>
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
</MPD>"#,
        );
        assert_eq!(period_link(&mpd, 0, 1), PeriodLink::Continuous);
    }

    #[test]
    fn adaptation_sets_linked_respects_kind() {
        let mpd = parse(
            r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16S" minBufferTime="PT2S">
  <Period id="p1" duration="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
  <Period id="p2" start="PT8S">
    <AdaptationSet id="1" mimeType="video/mp4">
      <SupplementalProperty schemeIdUri="urn:mpeg:dash:period-connectivity:2015" value="p1"/>
      <Representation id="r1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
</MPD>"#,
        );
        let prev = &mpd.periods[0].adaptations[0];
        let next = &mpd.periods[1].adaptations[0];
        assert!(adaptation_sets_linked(
            prev,
            Some("p1"),
            next,
            PeriodLink::Connected
        ));
        assert!(!adaptation_sets_linked(
            prev,
            Some("p1"),
            next,
            PeriodLink::Continuous
        ));
    }

    #[test]
    fn continuity_fixture_mpd_links() {
        let xml = include_str!("../../tests/fixtures/vod_period_continuity/manifest.mpd");
        let mpd = parse(xml);
        assert_eq!(period_link(&mpd, 0, 1), PeriodLink::Continuous);
        assert_eq!(mpd.periods[0].adaptations[0].id.as_deref(), Some("1"));
        assert_eq!(mpd.periods[1].adaptations[0].id.as_deref(), Some("1"));
    }
}
