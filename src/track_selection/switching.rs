//! Adaptation-set switching and DVB fallback SupplementalProperty resolution.

use std::collections::{HashMap, HashSet};

use dash_mpd::{AdaptationSet, Period};

/// MPEG-DASH adaptation-set switching scheme (ISO/IEC 23009-1).
pub const ADAPTATION_SET_SWITCHING_SCHEME: &str = "urn:mpeg:dash:adaptation-set-switching:2016";

/// Legacy DASH-IF adaptation-set switching scheme.
pub const DASHIF_ADAPTATION_SET_SWITCHING_SCHEME: &str =
    "http://dashif.org/guidelines/AdaptationSetSwitching";

/// DVB low-bitrate fallback Adaptation Set (ETSI TS 103 285).
///
/// Present on the fallback Adaptation Set; `@value` equals the primary
/// `AdaptationSet@id` for which it provides fallback.
pub const DVB_FALLBACK_ADAPTATION_SET_SCHEME: &str = "urn:dvb:dash:fallback_adaptation_set:2014";

fn scheme_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn is_adaptation_set_switching_scheme(scheme_id_uri: &str) -> bool {
    scheme_eq(scheme_id_uri, ADAPTATION_SET_SWITCHING_SCHEME)
        || scheme_eq(scheme_id_uri, DASHIF_ADAPTATION_SET_SWITCHING_SCHEME)
}

fn is_dvb_fallback_scheme(scheme_id_uri: &str) -> bool {
    scheme_eq(scheme_id_uri, DVB_FALLBACK_ADAPTATION_SET_SCHEME)
}

/// Parse a comma-separated list of Adaptation Set `@id` values.
pub(crate) fn parse_adaptation_set_id_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

fn descriptor_scheme_and_value<'a>(
    scheme_id_uri: &'a str,
    value: Option<&'a String>,
) -> Option<(&'a str, &'a str)> {
    Some((scheme_id_uri, value?.as_str()))
}

/// Collect `(schemeIdUri, value)` pairs from Essential and Supplemental descriptors on
/// the adaptation set and its representations / sub-representations.
fn collect_descriptor_values(adaptation_set: &AdaptationSet) -> Vec<(&str, &str)> {
    let mut out = Vec::new();

    for property in &adaptation_set.essential_property {
        if let Some(pair) =
            descriptor_scheme_and_value(&property.schemeIdUri, property.value.as_ref())
        {
            out.push(pair);
        }
    }
    for property in &adaptation_set.supplemental_property {
        if let Some(pair) =
            descriptor_scheme_and_value(&property.schemeIdUri, property.value.as_ref())
        {
            out.push(pair);
        }
    }
    for representation in &adaptation_set.representations {
        for property in &representation.essential_property {
            if let Some(pair) =
                descriptor_scheme_and_value(&property.schemeIdUri, property.value.as_ref())
            {
                out.push(pair);
            }
        }
        for property in &representation.supplemental_property {
            if let Some(pair) =
                descriptor_scheme_and_value(&property.schemeIdUri, property.value.as_ref())
            {
                out.push(pair);
            }
        }
        for sub in &representation.SubRepresentation {
            for property in &sub.essential_property {
                if let Some(pair) =
                    descriptor_scheme_and_value(&property.schemeIdUri, property.value.as_ref())
                {
                    out.push(pair);
                }
            }
            for property in &sub.supplemental_property {
                if let Some(pair) =
                    descriptor_scheme_and_value(&property.schemeIdUri, property.value.as_ref())
                {
                    out.push(pair);
                }
            }
        }
    }
    out
}

/// Target Adaptation Set `@id`s from adaptation-set-switching descriptors on this set.
pub(crate) fn switching_target_ids(adaptation_set: &AdaptationSet) -> Vec<String> {
    let mut ids = Vec::new();
    for (scheme, value) in collect_descriptor_values(adaptation_set) {
        if !is_adaptation_set_switching_scheme(scheme) {
            continue;
        }
        for id in parse_adaptation_set_id_list(value) {
            if !ids
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&id))
            {
                ids.push(id);
            }
        }
    }
    ids
}

/// Primary Adaptation Set `@id` this set falls back for, when it carries the DVB fallback
/// SupplementalProperty.
pub(crate) fn dvb_fallback_primary_id(adaptation_set: &AdaptationSet) -> Option<String> {
    for (scheme, value) in collect_descriptor_values(adaptation_set) {
        if is_dvb_fallback_scheme(scheme) {
            let id = value.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Returns whether this adaptation set is a DVB fallback for another adaptation set.
pub(crate) fn is_dvb_fallback_adaptation_set(adaptation_set: &AdaptationSet) -> bool {
    dvb_fallback_primary_id(adaptation_set).is_some()
}

fn id_index_map(period: &Period) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for (idx, aset) in period.adaptations.iter().enumerate() {
        if let Some(id) = aset.id.as_ref() {
            // First occurrence wins; duplicate ids are unusual.
            map.entry(id.clone()).or_insert(idx);
        }
    }
    map
}

fn find_id_index(map: &HashMap<String, usize>, id: &str) -> Option<usize> {
    map.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(id))
        .map(|(_, idx)| *idx)
}

/// Union-find for undirected switch/fallback groups within a Period.
#[derive(Debug, Clone)]
pub(crate) struct SwitchingGroups {
    parent: Vec<usize>,
}

impl SwitchingGroups {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
        }
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[rb] = ra;
        }
    }

    /// All period indices in the same connected component as `index` (including itself).
    pub(crate) fn component_of(&mut self, index: usize) -> Vec<usize> {
        if index >= self.parent.len() {
            return Vec::new();
        }
        let root = self.find(index);
        let mut members: Vec<usize> = (0..self.parent.len())
            .filter(|&i| self.find(i) == root)
            .collect();
        members.sort_unstable();
        members
    }
}

/// Build undirected connectivity from adaptation-set switching and DVB fallback links.
pub(crate) fn resolve_switching_groups(period: &Period) -> SwitchingGroups {
    let n = period.adaptations.len();
    let mut groups = SwitchingGroups::new(n);
    let id_map = id_index_map(period);

    for (idx, aset) in period.adaptations.iter().enumerate() {
        for target_id in switching_target_ids(aset) {
            if let Some(target_idx) = find_id_index(&id_map, &target_id) {
                groups.union(idx, target_idx);
            }
        }
        if let Some(primary_id) = dvb_fallback_primary_id(aset) {
            if let Some(primary_idx) = find_id_index(&id_map, &primary_id) {
                groups.union(idx, primary_idx);
            }
        }
    }

    groups
}

/// Collapse a set of selected period indices into unique group roots, preferring the
/// first-seen (best-ranked) member as primary for each connected component.
///
/// Returns `(primary_index, peer_indices)` for each collapsed group, in encounter order.
pub(crate) fn collapse_selected_into_switch_groups(
    period: &Period,
    selected_indices: &[usize],
) -> Vec<(usize, Vec<usize>)> {
    let mut groups = resolve_switching_groups(period);
    let mut claimed_roots = HashSet::new();
    let mut collapsed = Vec::new();

    for &primary in selected_indices {
        let root = groups.find(primary);
        if !claimed_roots.insert(root) {
            continue;
        }
        let peers: Vec<usize> = groups
            .component_of(primary)
            .into_iter()
            .filter(|&i| i != primary)
            .collect();
        collapsed.push((primary, peers));
    }
    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_mpd::Period;

    fn period(xml: &str) -> Period {
        dash_mpd::parse(xml)
            .expect("valid mpd")
            .periods
            .into_iter()
            .next()
            .expect("period")
    }

    fn switch_peer_indices(period: &Period, primary_index: usize) -> Vec<usize> {
        let mut groups = resolve_switching_groups(period);
        groups
            .component_of(primary_index)
            .into_iter()
            .filter(|&i| i != primary_index)
            .collect()
    }

    fn dvb_fallback_indices_for_primary(period: &Period, primary_index: usize) -> Vec<usize> {
        let Some(primary_id) = period
            .adaptations
            .get(primary_index)
            .and_then(|a| a.id.as_ref())
        else {
            return Vec::new();
        };
        period
            .adaptations
            .iter()
            .enumerate()
            .filter_map(|(idx, aset)| {
                let fallback_for = dvb_fallback_primary_id(aset)?;
                if fallback_for.eq_ignore_ascii_case(primary_id) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn parse_adaptation_set_id_list_trims_and_splits() {
        assert_eq!(
            parse_adaptation_set_id_list(" 2, 3 ,4 "),
            vec!["2", "3", "4"]
        );
        assert!(parse_adaptation_set_id_list("").is_empty());
    }

    #[test]
    fn mutual_switching_forms_one_group() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="264" mimeType="video/mp4" contentType="video">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="265"/>
                  <Representation id="a" bandwidth="100000"/>
                </AdaptationSet>
                <AdaptationSet id="265" mimeType="video/mp4" contentType="video">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="264"/>
                  <Representation id="b" bandwidth="200000"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        let peers = switch_peer_indices(&period, 0);
        assert_eq!(peers, vec![1]);
        let peers = switch_peer_indices(&period, 1);
        assert_eq!(peers, vec![0]);
    }

    #[test]
    fn one_way_switching_still_joins_group() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="1" mimeType="video/mp4" contentType="video">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="2"/>
                  <Representation id="a" bandwidth="100000"/>
                </AdaptationSet>
                <AdaptationSet id="2" mimeType="video/mp4" contentType="video">
                  <Representation id="b" bandwidth="200000"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        assert_eq!(switch_peer_indices(&period, 0), vec![1]);
        assert_eq!(switch_peer_indices(&period, 1), vec![0]);
    }

    #[test]
    fn dashif_switching_scheme_is_recognized() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="a" mimeType="video/mp4" contentType="video">
                  <SupplementalProperty schemeIdUri="http://dashif.org/guidelines/AdaptationSetSwitching" value="b"/>
                  <Representation id="1" bandwidth="100000"/>
                </AdaptationSet>
                <AdaptationSet id="b" mimeType="video/mp4" contentType="video">
                  <Representation id="2" bandwidth="200000"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        assert_eq!(switch_peer_indices(&period, 0), vec![1]);
    }

    #[test]
    fn dvb_fallback_joins_primary_group() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="main" mimeType="audio/mp4" contentType="audio">
                  <Representation id="hi" bandwidth="128000"/>
                </AdaptationSet>
                <AdaptationSet id="fb" mimeType="audio/mp4" contentType="audio">
                  <SupplementalProperty schemeIdUri="urn:dvb:dash:fallback_adaptation_set:2014" value="main"/>
                  <Representation id="lo" bandwidth="48000"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        assert!(is_dvb_fallback_adaptation_set(&period.adaptations[1]));
        assert_eq!(dvb_fallback_indices_for_primary(&period, 0), vec![1]);
        assert_eq!(switch_peer_indices(&period, 0), vec![1]);
    }

    #[test]
    fn collapse_selected_keeps_first_as_primary() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="1" mimeType="video/mp4" contentType="video">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="2"/>
                  <Representation id="a" bandwidth="100000"/>
                </AdaptationSet>
                <AdaptationSet id="2" mimeType="video/mp4" contentType="video">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="1"/>
                  <Representation id="b" bandwidth="200000"/>
                </AdaptationSet>
                <AdaptationSet id="3" mimeType="audio/mp4" contentType="audio">
                  <Representation id="c" bandwidth="96000"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        let collapsed = collapse_selected_into_switch_groups(&period, &[1, 0, 2]);
        assert_eq!(collapsed.len(), 2);
        assert_eq!(collapsed[0].0, 1);
        assert_eq!(collapsed[0].1, vec![0]);
        assert_eq!(collapsed[1].0, 2);
        assert!(collapsed[1].1.is_empty());
    }
}
