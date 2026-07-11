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

pub(crate) fn is_auxiliary_essential_scheme(scheme_id_uri: &str) -> bool {
    AUXILIARY_ESSENTIAL_SCHEMES
        .iter()
        .any(|scheme| scheme_eq(scheme_id_uri, scheme))
}

pub(crate) fn is_trick_play_adaptation_set(adaptation_set: &AdaptationSet) -> bool {
    const TRICK_MODE_SCHEME: &str = "http://dashif.org/guidelines/trickmode";

    adaptation_set
        .essential_property
        .iter()
        .any(|property| scheme_eq(&property.schemeIdUri, TRICK_MODE_SCHEME))
        || adaptation_set.representations.iter().any(|representation| {
            representation
                .essential_property
                .iter()
                .any(|property| scheme_eq(&property.schemeIdUri, TRICK_MODE_SCHEME))
        })
}

/// Returns whether this adaptation set carries thumbnail-tile `EssentialProperty` descriptors.
pub(crate) fn is_thumbnail_tile_adaptation_set(adaptation_set: &AdaptationSet) -> bool {
    const THUMBNAIL_TILE_SCHEME: &str = "http://dashif.org/guidelines/thumbnail_tile";

    adaptation_set
        .essential_property
        .iter()
        .any(|property| scheme_eq(&property.schemeIdUri, THUMBNAIL_TILE_SCHEME))
        || adaptation_set.representations.iter().any(|representation| {
            representation
                .essential_property
                .iter()
                .any(|property| scheme_eq(&property.schemeIdUri, THUMBNAIL_TILE_SCHEME))
        })
}

/// Parse `thumbnail_tile` tile layout as `(horizontal_tiles, vertical_tiles)`.
pub(crate) fn thumbnail_tile_layout(adaptation_set: &AdaptationSet) -> Option<(u32, u32)> {
    const THUMBNAIL_TILE_SCHEME: &str = "http://dashif.org/guidelines/thumbnail_tile";

    let property = adaptation_set
        .essential_property
        .iter()
        .chain(
            adaptation_set
                .representations
                .iter()
                .flat_map(|representation| representation.essential_property.iter()),
        )
        .find(|property| scheme_eq(&property.schemeIdUri, THUMBNAIL_TILE_SCHEME))?;
    let value = property.value.as_deref()?;
    let mut parts = value.split('x');
    let horizontal = parts.next()?.parse().ok()?;
    let vertical = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((horizontal, vertical))
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

/// Returns whether a representation can be delivered, including auxiliary trick-play and thumbnail
/// descriptors.
pub(crate) fn is_delivery_representation(representation: &dash_mpd::Representation) -> bool {
    representation.essential_property.iter().all(|property| {
        is_auxiliary_essential_scheme(&property.schemeIdUri)
            || essential_property_supported(&property.schemeIdUri)
    })
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
    fn thumbnail_tile_layout_parses_horizontal_and_vertical_counts() {
        let aset = adaptation_set(
            r#"<MPD><Period><AdaptationSet mimeType="image/jpeg" contentType="image">
                <EssentialProperty schemeIdUri="http://dashif.org/guidelines/thumbnail_tile" value="10x5"/>
            </AdaptationSet></Period></MPD>"#,
        );
        assert_eq!(thumbnail_tile_layout(&aset), Some((10, 5)));
    }

    #[test]
    fn delivery_representation_allows_thumbnail_tile_essential_property() {
        let aset = adaptation_set(
            r#"<MPD><Period><AdaptationSet mimeType="image/jpeg" contentType="image">
                <Representation id="thumb" bandwidth="1000">
                  <EssentialProperty schemeIdUri="http://dashif.org/guidelines/thumbnail_tile" value="4x3"/>
                </Representation>
            </AdaptationSet></Period></MPD>"#,
        );
        assert!(is_delivery_representation(&aset.representations[0]));
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
