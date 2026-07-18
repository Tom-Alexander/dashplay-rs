//! MPEG-DASH `SubRepresentation` resolution.
//!
//! A SubRepresentation describes properties of one or more media content components
//! embedded in a Representation (ISO/IEC 23009-1 §5.3.6). Unset common attributes
//! inherit from the containing Representation, then the Adaptation Set (§5.3.7).
//! `@contentComponent` links to Adaptation Set `ContentComponent@id` values for
//! language, role, and content-type metadata used in track selection.

use dash_mpd::{AdaptationSet, ContentComponent, Representation, SubRepresentation};

use super::kind::TrackDescriptor;

/// Public metadata for one MPD `SubRepresentation` after common-attribute inheritance.
#[derive(Debug, Clone, PartialEq)]
pub struct SubTrackInfo {
    /// `SubRepresentation@level`. When set, a Subsegment Index assigns this level.
    pub level: Option<u32>,
    /// Parsed `SubRepresentation@dependencyLevel` values.
    pub dependency_levels: Vec<u32>,
    /// `ContentComponent@id` values from `@contentComponent`.
    pub content_component_ids: Vec<String>,
    /// Effective MIME type after Representation → AdaptationSet inheritance.
    pub mime_type: Option<String>,
    /// Effective `@contentType`, falling back to linked ContentComponents.
    pub content_type: Option<String>,
    /// Sub-representation bandwidth. Not inherited from the parent Representation.
    pub bandwidth: Option<u64>,
    /// Effective RFC 6381 codec strings after inheritance and comma-splitting.
    pub codecs: Vec<String>,
    /// Effective width after Representation → AdaptationSet inheritance.
    pub width: Option<u64>,
    /// Effective height after Representation → AdaptationSet inheritance.
    pub height: Option<u64>,
    /// Effective frame rate after Representation → AdaptationSet inheritance.
    pub frame_rate: Option<String>,
    /// Language from the first linked ContentComponent that declares `@lang`.
    pub language: Option<String>,
    /// Role values from linked ContentComponents.
    pub roles: Vec<String>,
    /// Accessibility descriptors from linked ContentComponents.
    pub accessibility: Vec<TrackDescriptor>,
    /// Maximum supported playout rate after SubRepresentation → Representation →
    /// AdaptationSet inheritance.
    pub max_playout_rate: Option<f64>,
    /// Whether samples have coding dependencies after the same inheritance chain.
    /// Metadata only; not used for trick-play selection.
    pub coding_dependency: Option<bool>,
}

fn parse_whitespace_ids(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_dependency_levels(value: &str) -> Vec<u32> {
    value
        .split_whitespace()
        .filter_map(|token| token.parse().ok())
        .collect()
}

fn codecs_from_string(codecs: &str) -> Vec<String> {
    codecs
        .split(',')
        .map(str::trim)
        .filter(|codec| !codec.is_empty())
        .map(str::to_string)
        .collect()
}

fn linked_content_components<'a>(
    adaptation_set: &'a AdaptationSet,
    content_component_ids: &[String],
) -> Vec<&'a ContentComponent> {
    content_component_ids
        .iter()
        .filter_map(|id| {
            adaptation_set
                .ContentComponent
                .iter()
                .find(|component| component.id.as_deref() == Some(id.as_str()))
        })
        .collect()
}

/// ISO/IEC 23009-1 Table 13 / DASH-IF schematron: `@bandwidth` is required when `@level` is set.
fn is_valid_sub_representation(sub: &SubRepresentation) -> bool {
    sub.level.is_none() || sub.bandwidth.is_some()
}

fn resolve_one(
    adaptation_set: &AdaptationSet,
    representation: &Representation,
    sub: &SubRepresentation,
) -> Option<SubTrackInfo> {
    if !is_valid_sub_representation(sub) {
        return None;
    }

    let content_component_ids = sub
        .contentComponent
        .as_deref()
        .map(parse_whitespace_ids)
        .unwrap_or_default();
    let linked = linked_content_components(adaptation_set, &content_component_ids);

    let codecs_raw = sub
        .codecs
        .as_deref()
        .or(representation.codecs.as_deref())
        .or(adaptation_set.codecs.as_deref());
    let codecs = codecs_raw.map(codecs_from_string).unwrap_or_default();

    let language = linked.iter().find_map(|component| component.lang.clone());
    let roles: Vec<String> = linked
        .iter()
        .flat_map(|component| component.Role.iter())
        .filter_map(|role| role.value.clone())
        .collect();
    let accessibility: Vec<TrackDescriptor> = linked
        .iter()
        .flat_map(|component| component.Accessibility.iter())
        .map(|descriptor| TrackDescriptor {
            scheme_id_uri: descriptor.schemeIdUri.clone(),
            value: descriptor.value.clone(),
        })
        .collect();

    let content_type = sub
        .contentType
        .clone()
        .or_else(|| {
            linked
                .iter()
                .find_map(|component| component.contentType.clone())
        })
        .or_else(|| adaptation_set.contentType.clone());

    Some(SubTrackInfo {
        level: sub.level,
        dependency_levels: sub
            .dependencyLevel
            .as_deref()
            .map(parse_dependency_levels)
            .unwrap_or_default(),
        content_component_ids,
        mime_type: sub
            .mimeType
            .clone()
            .or_else(|| representation.mimeType.clone())
            .or_else(|| adaptation_set.mimeType.clone()),
        content_type,
        bandwidth: sub.bandwidth,
        codecs,
        width: sub.width.or(representation.width).or(adaptation_set.width),
        height: sub
            .height
            .or(representation.height)
            .or(adaptation_set.height),
        frame_rate: sub
            .frameRate
            .clone()
            .or_else(|| representation.frameRate.clone())
            .or_else(|| adaptation_set.frameRate.clone()),
        language,
        roles,
        accessibility,
        max_playout_rate: sub
            .maxPlayoutRate
            .filter(|v| v.is_finite() && *v > 0.0)
            .or_else(|| {
                representation
                    .maxPlayoutRate
                    .filter(|v| v.is_finite() && *v > 0.0)
            })
            .or_else(|| {
                adaptation_set
                    .maxPlayoutRate
                    .filter(|v| v.is_finite() && *v > 0.0)
            }),
        coding_dependency: sub
            .codingDependency
            .or(representation.codingDependency)
            .or(adaptation_set.codingDependency),
    })
}

/// Resolve every valid `SubRepresentation` under all Representations in the Adaptation Set.
pub(crate) fn resolve_sub_tracks(adaptation_set: &AdaptationSet) -> Vec<SubTrackInfo> {
    adaptation_set
        .representations
        .iter()
        .flat_map(|representation| {
            representation
                .SubRepresentation
                .iter()
                .filter_map(|sub| resolve_one(adaptation_set, representation, sub))
        })
        .collect()
}

/// Collect RFC 6381 codec tokens declared on SubRepresentations (before inheritance).
pub(crate) fn sub_representation_codec_values(adaptation_set: &AdaptationSet) -> Vec<String> {
    let mut codecs = Vec::new();
    for representation in &adaptation_set.representations {
        for sub in &representation.SubRepresentation {
            if !is_valid_sub_representation(sub) {
                continue;
            }
            let Some(value) = sub.codecs.as_deref() else {
                continue;
            };
            for codec in codecs_from_string(value) {
                if !codecs
                    .iter()
                    .any(|existing: &String| existing.eq_ignore_ascii_case(&codec))
                {
                    codecs.push(codec);
                }
            }
        }
    }
    codecs
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
    fn inherits_dimensions_and_links_content_components() {
        let aset = adaptation_set(
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011">
                <Period>
                  <AdaptationSet mimeType="video/mp4" codecs="avc1.4D401E,mp4a.40"
                                 width="640" height="480" frameRate="30" lang="en">
                    <ContentComponent id="0" contentType="video"/>
                    <ContentComponent id="1" contentType="audio" lang="fr">
                      <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
                    </ContentComponent>
                    <Representation id="tag0" bandwidth="512000">
                      <SubRepresentation level="0" contentComponent="0" bandwidth="128000"
                                         codecs="avc1.4D401E" maxPlayoutRate="4"/>
                      <SubRepresentation level="2" contentComponent="1" bandwidth="64000"
                                         codecs="mp4a.40"/>
                    </Representation>
                  </AdaptationSet>
                </Period>
              </MPD>"#,
        );

        let subs = resolve_sub_tracks(&aset);
        assert_eq!(subs.len(), 2);

        assert_eq!(subs[0].level, Some(0));
        assert_eq!(subs[0].bandwidth, Some(128000));
        assert_eq!(subs[0].codecs, vec!["avc1.4D401E"]);
        assert_eq!(subs[0].width, Some(640));
        assert_eq!(subs[0].height, Some(480));
        assert_eq!(subs[0].frame_rate.as_deref(), Some("30"));
        assert_eq!(subs[0].content_type.as_deref(), Some("video"));
        assert_eq!(subs[0].max_playout_rate, Some(4.0));
        assert_eq!(subs[0].coding_dependency, None);

        assert_eq!(subs[1].level, Some(2));
        assert_eq!(subs[1].content_component_ids, vec!["1"]);
        assert_eq!(subs[1].content_type.as_deref(), Some("audio"));
        assert_eq!(subs[1].language.as_deref(), Some("fr"));
        assert_eq!(subs[1].roles, vec!["main"]);
        assert_eq!(subs[1].codecs, vec!["mp4a.40"]);
        assert_eq!(subs[1].max_playout_rate, None);
        assert_eq!(subs[1].coding_dependency, None);
    }

    #[test]
    fn skips_level_without_bandwidth() {
        let aset = adaptation_set(
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011">
                <Period>
                  <AdaptationSet mimeType="video/mp4" contentType="video">
                    <Representation id="1" bandwidth="100000">
                      <SubRepresentation level="0" codecs="avc1.4D401E"/>
                      <SubRepresentation codecs="mp4a.40.2"/>
                    </Representation>
                  </AdaptationSet>
                </Period>
              </MPD>"#,
        );
        let subs = resolve_sub_tracks(&aset);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].codecs, vec!["mp4a.40.2"]);
        assert_eq!(subs[0].level, None);
    }

    #[test]
    fn inherits_coding_dependency_from_representation() {
        let aset = adaptation_set(
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011">
                <Period>
                  <AdaptationSet mimeType="video/mp4" codingDependency="true">
                    <Representation id="1" bandwidth="500000" codingDependency="false">
                      <SubRepresentation level="0" bandwidth="320000" codecs="avc1.4D401E"/>
                    </Representation>
                  </AdaptationSet>
                </Period>
              </MPD>"#,
        );
        let subs = resolve_sub_tracks(&aset);
        assert_eq!(subs[0].coding_dependency, Some(false));
        assert_eq!(subs[0].max_playout_rate, None);
    }

    #[test]
    fn dependency_levels_parse_whitespace_list() {
        let aset = adaptation_set(
            r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011">
                <Period>
                  <AdaptationSet mimeType="video/mp4">
                    <Representation id="1" bandwidth="500000">
                      <SubRepresentation level="1" dependencyLevel="0 2" bandwidth="320000"
                                         codecs="avc2.4D401E"/>
                    </Representation>
                  </AdaptationSet>
                </Period>
              </MPD>"#,
        );
        let subs = resolve_sub_tracks(&aset);
        assert_eq!(subs[0].dependency_levels, vec![0, 2]);
    }
}
