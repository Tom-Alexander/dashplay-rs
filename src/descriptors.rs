//! DASH `EssentialProperty` and `SupplementalProperty` descriptor handling.

use dash_mpd::{AdaptationSet, EssentialProperty, SupplementalProperty};

use super::track_selection::TrackDescriptor;

/// MPEG-DASH role descriptor scheme.
pub const ROLE_SCHEME: &str = "urn:mpeg:dash:role:2011";

/// EssentialProperty schemes that mark auxiliary adaptation sets (trick play, thumbnails).
const AUXILIARY_ESSENTIAL_SCHEMES: &[&str] = &[
    "http://dashif.org/guidelines/trickmode",
    "http://dashif.org/guidelines/thumbnail_tile",
];

/// EssentialProperty schemes understood for segment delivery without extra processing.
const SUPPORTED_ESSENTIAL_SCHEMES: &[&str] = &["urn:mpeg:dash:adaptation-set-switching:2016"];

fn scheme_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn is_auxiliary_essential_scheme(scheme_id_uri: &str) -> bool {
    AUXILIARY_ESSENTIAL_SCHEMES
        .iter()
        .any(|scheme| scheme_eq(scheme_id_uri, scheme))
}

fn is_supported_essential_scheme(scheme_id_uri: &str) -> bool {
    SUPPORTED_ESSENTIAL_SCHEMES
        .iter()
        .any(|scheme| scheme_eq(scheme_id_uri, scheme))
}

fn to_descriptor(property: &EssentialProperty) -> TrackDescriptor {
    TrackDescriptor {
        scheme_id_uri: property.schemeIdUri.clone(),
        value: property.value.clone(),
    }
}

fn supplemental_to_descriptor(property: &SupplementalProperty) -> TrackDescriptor {
    TrackDescriptor {
        scheme_id_uri: property.schemeIdUri.clone(),
        value: property.value.clone(),
    }
}

fn collect_essential_properties(adaptation_set: &AdaptationSet) -> Vec<TrackDescriptor> {
    adaptation_set
        .essential_property
        .iter()
        .chain(
            adaptation_set
                .representations
                .iter()
                .flat_map(|representation| representation.essential_property.iter()),
        )
        .map(to_descriptor)
        .collect()
}

fn collect_supplemental_properties(adaptation_set: &AdaptationSet) -> Vec<TrackDescriptor> {
    adaptation_set
        .supplemental_property
        .iter()
        .chain(
            adaptation_set
                .representations
                .iter()
                .flat_map(|representation| representation.supplemental_property.iter()),
        )
        .map(supplemental_to_descriptor)
        .collect()
}

fn supplemental_roles(adaptation_set: &AdaptationSet) -> Vec<String> {
    let mut roles = Vec::new();
    for descriptor in collect_supplemental_properties(adaptation_set) {
        if !scheme_eq(&descriptor.scheme_id_uri, ROLE_SCHEME) {
            continue;
        }
        let Some(value) = descriptor.value else {
            continue;
        };
        if !roles
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&value))
        {
            roles.push(value);
        }
    }
    roles
}

/// Returns whether every `EssentialProperty` on this adaptation set can be used for playback.
///
/// Adaptation sets that declare auxiliary essential schemes (trick play, thumbnail tiles) or
/// unknown essential schemes are excluded from default track selection.
pub fn is_playback_adaptation_set(adaptation_set: &AdaptationSet) -> bool {
    adaptation_set
        .essential_property
        .iter()
        .all(|property| essential_property_supported(&property.schemeIdUri))
}

/// Returns whether a representation's essential properties are supported for segment delivery.
pub fn is_playback_representation(representation: &dash_mpd::Representation) -> bool {
    representation
        .essential_property
        .iter()
        .all(|property| essential_property_supported(&property.schemeIdUri))
}

fn essential_property_supported(scheme_id_uri: &str) -> bool {
    !is_auxiliary_essential_scheme(scheme_id_uri) && is_supported_essential_scheme(scheme_id_uri)
}

pub(crate) fn adaptation_descriptor_metadata(
    adaptation_set: &AdaptationSet,
) -> (Vec<TrackDescriptor>, Vec<TrackDescriptor>, Vec<String>) {
    (
        collect_essential_properties(adaptation_set),
        collect_supplemental_properties(adaptation_set),
        supplemental_roles(adaptation_set),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adaptation_set(xml: &str) -> AdaptationSet {
        dash_mpd::parse(xml)
            .expect("valid mpd")
            .periods
            .into_iter()
            .next()
            .expect("period")
            .adaptations
            .into_iter()
            .next()
            .expect("adaptation set")
    }

    #[test]
    fn trick_mode_essential_property_excludes_playback() {
        let aset = adaptation_set(
            r#"<MPD><Period><AdaptationSet mimeType="video/mp4" contentType="video">
                <EssentialProperty schemeIdUri="http://dashif.org/guidelines/trickmode" value="1"/>
            </AdaptationSet></Period></MPD>"#,
        );
        assert!(!is_playback_adaptation_set(&aset));
    }

    #[test]
    fn adaptation_set_switching_essential_property_is_supported() {
        let aset = adaptation_set(
            r#"<MPD><Period><AdaptationSet mimeType="video/mp4" contentType="video">
                <EssentialProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="2"/>
            </AdaptationSet></Period></MPD>"#,
        );
        assert!(is_playback_adaptation_set(&aset));
    }

    #[test]
    fn unknown_essential_property_excludes_playback() {
        let aset = adaptation_set(
            r#"<MPD><Period><AdaptationSet mimeType="video/mp4" contentType="video">
                <EssentialProperty schemeIdUri="urn:example:unsupported:2020" value="x"/>
            </AdaptationSet></Period></MPD>"#,
        );
        assert!(!is_playback_adaptation_set(&aset));
    }

    #[test]
    fn supplemental_role_values_are_collected() {
        let aset = adaptation_set(
            r#"<MPD><Period><AdaptationSet mimeType="audio/mp4" contentType="audio">
                <SupplementalProperty schemeIdUri="urn:mpeg:dash:role:2011" value="dub"/>
            </AdaptationSet></Period></MPD>"#,
        );
        assert_eq!(supplemental_roles(&aset), vec!["dub"]);
    }
}
