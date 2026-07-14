//! MPEG-DASH `Preselection` element and descriptor handling.
//!
//! A Preselection is a Period-level personalization bundle: selecting it means jointly
//! delivering the Adaptation Sets (or Content Components) listed in
//! `@preselectionComponents`. Partial Adaptation Sets marked with an Essential
//! `urn:mpeg:dash:preselection:2016` descriptor are only selectable via a Preselection.

use dash_mpd::{AdaptationSet, Period, Preselection};

use super::kind::{TrackDescriptor, TrackKind};
use super::select::{codec_values, effective_language, role_values, track_kind};

/// Scheme URI for the Preselection descriptor (ISO/IEC 23009-1 clause 5.3.11.2).
pub const PRESELECTION_SCHEME: &str = "urn:mpeg:dash:preselection:2016";

/// A resolved Preselection ready for preference ranking and AdaptationSet expansion.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPreselection {
    /// Indices into `Period.adaptations` in `@preselectionComponents` order.
    pub adaptation_indices: Vec<usize>,
    /// Track kind of the Main Adaptation Set (first resolved component).
    pub main_kind: TrackKind,
    pub language: Option<String>,
    pub roles: Vec<String>,
    pub codecs: Vec<String>,
    pub accessibility: Vec<TrackDescriptor>,
    pub selection_priority: u64,
    /// Document order among Preselections in the Period (elements before descriptors).
    pub document_index: usize,
}

fn scheme_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

/// Returns whether this Adaptation Set is only consumable as part of a Preselection.
///
/// True when `EssentialProperty` carries `urn:mpeg:dash:preselection:2016`
/// (ISO/IEC 23009-1: Essential ⇒ partial; Supplemental ⇒ also independently playable).
pub(crate) fn is_partial_preselection_adaptation_set(adaptation_set: &AdaptationSet) -> bool {
    adaptation_set
        .essential_property
        .iter()
        .any(|property| scheme_eq(&property.schemeIdUri, PRESELECTION_SCHEME))
}

fn parse_component_ids(value: &str) -> Vec<&str> {
    value
        .split_whitespace()
        .filter(|id| !id.is_empty())
        .collect()
}

/// Parse a Preselection Descriptor `@value` of the form `tag,id1 id2 …`.
fn parse_descriptor_value(value: &str) -> Option<(String, Vec<&str>)> {
    let (tag, ids) = value.split_once(',')?;
    let tag = tag.trim();
    if tag.is_empty() {
        return None;
    }
    let components = parse_component_ids(ids);
    if components.is_empty() {
        return None;
    }
    Some((tag.to_string(), components))
}

fn adaptation_matches_component_id(adaptation_set: &AdaptationSet, component_id: &str) -> bool {
    adaptation_set
        .id
        .as_deref()
        .is_some_and(|id| id == component_id)
        || adaptation_set
            .ContentComponent
            .iter()
            .any(|component| component.id.as_deref() == Some(component_id))
}

fn resolve_component_ids(period: &Period, component_ids: &[&str]) -> Option<Vec<usize>> {
    if component_ids.is_empty() {
        return None;
    }
    let mut indices = Vec::with_capacity(component_ids.len());
    for component_id in component_ids {
        let index = period.adaptations.iter().position(|adaptation_set| {
            adaptation_matches_component_id(adaptation_set, component_id)
        })?;
        if !indices.contains(&index) {
            indices.push(index);
        }
    }
    Some(indices)
}

fn codecs_from_string(codecs: &str) -> Vec<String> {
    codecs
        .split(',')
        .map(str::trim)
        .filter(|codec| !codec.is_empty())
        .map(str::to_string)
        .collect()
}

fn preselection_language(preselection: &Preselection) -> Option<String> {
    preselection.lang.clone().or_else(|| {
        preselection
            .languages
            .iter()
            .find_map(|language| language.content.clone())
    })
}

fn preselection_roles(preselection: &Preselection) -> Vec<String> {
    preselection
        .roles
        .iter()
        .filter_map(|role| role.value.clone())
        .collect()
}

fn preselection_accessibility(preselection: &Preselection) -> Vec<TrackDescriptor> {
    preselection
        .accessibilities
        .iter()
        .map(|descriptor| TrackDescriptor {
            scheme_id_uri: descriptor.schemeIdUri.clone(),
            value: descriptor.value.clone(),
        })
        .collect()
}

fn resolve_from_element(
    period: &Period,
    preselection: &Preselection,
    document_index: usize,
) -> Option<ResolvedPreselection> {
    let component_ids = parse_component_ids(&preselection.preselectionComponents);
    let adaptation_indices = resolve_component_ids(period, &component_ids)?;
    let main = period.adaptations.get(adaptation_indices[0])?;
    let main_kind = track_kind(main)?;
    Some(ResolvedPreselection {
        adaptation_indices,
        main_kind,
        language: preselection_language(preselection).or_else(|| effective_language(main)),
        roles: {
            let roles = preselection_roles(preselection);
            if roles.is_empty() {
                role_values(main, &[])
            } else {
                roles
            }
        },
        codecs: {
            let codecs = codecs_from_string(&preselection.codecs);
            if codecs.is_empty() {
                codec_values(main)
            } else {
                codecs
            }
        },
        accessibility: {
            let accessibility = preselection_accessibility(preselection);
            if accessibility.is_empty() {
                main.Accessibility
                    .iter()
                    .chain(
                        main.ContentComponent
                            .iter()
                            .flat_map(|component| component.Accessibility.iter()),
                    )
                    .map(|descriptor| TrackDescriptor {
                        scheme_id_uri: descriptor.schemeIdUri.clone(),
                        value: descriptor.value.clone(),
                    })
                    .collect()
            } else {
                accessibility
            }
        },
        selection_priority: preselection.selectionPriority.unwrap_or(1),
        document_index,
    })
}

fn resolve_from_descriptor(
    period: &Period,
    adaptation_set: &AdaptationSet,
    component_ids: &[&str],
    document_index: usize,
) -> Option<ResolvedPreselection> {
    let adaptation_indices = resolve_component_ids(period, component_ids)?;
    let main = period.adaptations.get(adaptation_indices[0])?;
    let main_kind = track_kind(main)?;
    // Descriptor-defined Preselections inherit ranking metadata from the carrying /
    // Main Adaptation Set (the descriptor does not carry lang/role separately).
    let from_carrier = adaptation_set.id.as_deref() == main.id.as_deref();
    let meta_set = if from_carrier { adaptation_set } else { main };
    Some(ResolvedPreselection {
        adaptation_indices,
        main_kind,
        language: effective_language(meta_set),
        roles: role_values(meta_set, &[]),
        codecs: codec_values(meta_set),
        accessibility: meta_set
            .Accessibility
            .iter()
            .chain(
                meta_set
                    .ContentComponent
                    .iter()
                    .flat_map(|component| component.Accessibility.iter()),
            )
            .map(|descriptor| TrackDescriptor {
                scheme_id_uri: descriptor.schemeIdUri.clone(),
                value: descriptor.value.clone(),
            })
            .collect(),
        selection_priority: meta_set.selectionPriority.unwrap_or(1),
        document_index,
    })
}

/// Collect Period `Preselection` elements and describing Preselection descriptors.
pub(crate) fn resolve_preselections(period: &Period) -> Vec<ResolvedPreselection> {
    let mut resolved = Vec::new();
    let mut document_index = 0usize;

    for preselection in &period.pre_selections {
        if let Some(item) = resolve_from_element(period, preselection, document_index) {
            resolved.push(item);
            document_index += 1;
        }
    }

    for adaptation_set in &period.adaptations {
        let descriptor_values = adaptation_set
            .essential_property
            .iter()
            .filter(|property| scheme_eq(&property.schemeIdUri, PRESELECTION_SCHEME))
            .filter_map(|property| property.value.as_deref())
            .chain(
                adaptation_set
                    .supplemental_property
                    .iter()
                    .filter(|property| scheme_eq(&property.schemeIdUri, PRESELECTION_SCHEME))
                    .filter_map(|property| property.value.as_deref()),
            );
        for value in descriptor_values {
            let Some((_tag, components)) = parse_descriptor_value(value) else {
                continue;
            };
            if let Some(item) =
                resolve_from_descriptor(period, adaptation_set, &components, document_index)
            {
                resolved.push(item);
                document_index += 1;
            }
        }
    }

    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn period(xml: &str) -> Period {
        dash_mpd::parse(xml)
            .expect("valid MPD")
            .periods
            .into_iter()
            .next()
            .expect("period")
    }

    #[test]
    fn resolves_preselection_element_components() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="1" contentType="audio" mimeType="audio/mp4" lang="en"/>
                <AdaptationSet id="2" contentType="audio" mimeType="audio/mp4">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                </AdaptationSet>
                <Preselection id="ps1" preselectionComponents="1 2" lang="en" codecs="mp4a.40.2" tag="1">
                  <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
                </Preselection>
            </Period></MPD>"#,
        );
        let resolved = resolve_preselections(&period);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].adaptation_indices, vec![0, 1]);
        assert_eq!(resolved[0].main_kind, TrackKind::Audio);
        assert_eq!(resolved[0].language.as_deref(), Some("en"));
        assert_eq!(resolved[0].roles, vec!["main"]);
        assert!(is_partial_preselection_adaptation_set(
            &period.adaptations[1]
        ));
        assert!(!is_partial_preselection_adaptation_set(
            &period.adaptations[0]
        ));
    }

    #[test]
    fn resolves_descriptor_defined_preselection() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="1" contentType="audio" mimeType="audio/mp4" lang="fr">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016" value="bundle,1 2"/>
                </AdaptationSet>
                <AdaptationSet id="2" contentType="audio" mimeType="audio/mp4">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        let resolved = resolve_preselections(&period);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].adaptation_indices, vec![0, 1]);
        assert_eq!(resolved[0].language.as_deref(), Some("fr"));
    }

    #[test]
    fn resolves_content_component_ids_to_containing_adaptation_set() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="as1" contentType="audio" mimeType="audio/mp4">
                  <ContentComponent id="10" contentType="audio" lang="en"/>
                </AdaptationSet>
                <AdaptationSet id="as2" contentType="audio" mimeType="audio/mp4">
                  <ContentComponent id="20" contentType="audio"/>
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                </AdaptationSet>
                <Preselection id="ps" preselectionComponents="10 20" tag="1"/>
            </Period></MPD>"#,
        );
        let resolved = resolve_preselections(&period);
        assert_eq!(resolved[0].adaptation_indices, vec![0, 1]);
    }
}
