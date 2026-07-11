//! Deterministic selection and description of DASH adaptation-set tracks.

use dash_mpd::{AdaptationSet, Period};

use super::descriptors::{
    adaptation_descriptor_metadata, is_playback_adaptation_set, is_thumbnail_tile_adaptation_set,
    is_trick_play_adaptation_set, thumbnail_tile_layout,
};

/// The media kind carried by a selected DASH adaptation set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    /// An audio adaptation set.
    Audio,
    /// A video adaptation set.
    Video,
    /// A subtitle or caption adaptation set (`text/vtt`, TTML, or in-band fMP4 text tracks).
    Text,
    /// A trick-play video adaptation set (`http://dashif.org/guidelines/trickmode`).
    TrickPlay,
    /// A thumbnail image adaptation set (`image/jpeg`, often with `thumbnail_tile`).
    Image,
}

/// A DASH descriptor used for track metadata and accessibility matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDescriptor {
    /// Descriptor scheme URI, such as `urn:mpeg:dash:role:2011`.
    pub scheme_id_uri: String,
    /// Optional descriptor value. When absent, any value under the scheme matches.
    pub value: Option<String>,
}

impl TrackDescriptor {
    /// Create a preference that matches any descriptor value under `scheme_id_uri`.
    pub fn scheme(scheme_id_uri: impl Into<String>) -> Self {
        Self {
            scheme_id_uri: scheme_id_uri.into(),
            value: None,
        }
    }

    /// Create a preference that matches both a descriptor scheme and value.
    pub fn new(scheme_id_uri: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            scheme_id_uri: scheme_id_uri.into(),
            value: Some(value.into()),
        }
    }
}

/// Ordered preferences and output limit for one media kind.
///
/// Preference lists are fallback lists: earlier entries rank ahead of later entries, and
/// adaptation sets that match none rank last. An empty list does not affect ranking.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackPreference {
    /// Preferred RFC 5646 language ranges, in priority order.
    pub languages: Vec<String>,
    /// Preferred DASH `Role@value` values, in priority order.
    pub roles: Vec<String>,
    /// Preferred RFC 6381 codec names or prefixes, in priority order.
    pub codecs: Vec<String>,
    /// Preferred DASH accessibility descriptors, in priority order.
    pub accessibility: Vec<TrackDescriptor>,
    /// Maximum number of tracks of this kind. `None` retains every compatible track.
    pub max_tracks: Option<usize>,
}

impl TrackPreference {
    /// Add a preferred RFC 5646 language range.
    pub fn language(mut self, language: impl Into<String>) -> Self {
        self.languages.push(language.into());
        self
    }

    /// Add a preferred DASH role value.
    pub fn role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Add a preferred RFC 6381 codec name or prefix.
    pub fn codec(mut self, codec: impl Into<String>) -> Self {
        self.codecs.push(codec.into());
        self
    }

    /// Add a preferred accessibility descriptor.
    pub fn accessibility(mut self, descriptor: TrackDescriptor) -> Self {
        self.accessibility.push(descriptor);
        self
    }

    /// Limit how many tracks of this media kind are selected.
    pub fn max_tracks(mut self, max_tracks: usize) -> Self {
        self.max_tracks = Some(max_tracks);
        self
    }
}

/// User preferences for selecting audio, video, text, trick-play, and image adaptation sets.
///
/// The default retains all audio and video tracks and **no** text, trick-play, or image tracks.
/// Use [`TrackPreference::max_tracks`] on [`Self::text`], [`Self::trick_play`], or [`Self::image`]
/// to enable auxiliary delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSelection {
    /// Audio-track preferences. Set `max_tracks` above one to retain multiple preferred languages
    /// or roles.
    pub audio: TrackPreference,
    /// Video-track preferences.
    pub video: TrackPreference,
    /// Subtitle and caption preferences. Disabled by default (`max_tracks(0)`).
    pub text: TrackPreference,
    /// Trick-play video preferences. Disabled by default (`max_tracks(0)`).
    pub trick_play: TrackPreference,
    /// Thumbnail image preferences. Disabled by default (`max_tracks(0)`).
    pub image: TrackPreference,
}

impl Default for TrackSelection {
    fn default() -> Self {
        Self {
            audio: TrackPreference::default(),
            video: TrackPreference::default(),
            text: TrackPreference::default().max_tracks(0),
            trick_play: TrackPreference::default().max_tracks(0),
            image: TrackPreference::default().max_tracks(0),
        }
    }
}

impl TrackSelection {
    /// Replace the audio-track preferences.
    pub fn with_audio(mut self, audio: TrackPreference) -> Self {
        self.audio = audio;
        self
    }

    /// Replace the video-track preferences.
    pub fn with_video(mut self, video: TrackPreference) -> Self {
        self.video = video;
        self
    }

    /// Replace subtitle and caption preferences.
    pub fn with_text(mut self, text: TrackPreference) -> Self {
        self.text = text;
        self
    }

    /// Replace trick-play video preferences.
    pub fn with_trick_play(mut self, trick_play: TrackPreference) -> Self {
        self.trick_play = trick_play;
        self
    }

    /// Replace thumbnail image preferences.
    pub fn with_image(mut self, image: TrackPreference) -> Self {
        self.image = image;
        self
    }
}

/// Public metadata for one selected adaptation-set track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// Index of this adaptation set within the containing `Period.adaptations` list.
    pub period_adaptation_index: usize,
    /// `AdaptationSet@id`, when present.
    pub id: Option<String>,
    /// Whether this is an audio, video, or text track.
    pub kind: TrackKind,
    /// Subtitle or caption format when [`Self::kind`] is [`TrackKind::Text`].
    pub subtitle_type: Option<dash_mpd::SubtitleType>,
    /// Thumbnail tile layout `(horizontal_tiles, vertical_tiles)` when [`Self::kind`] is
    /// [`TrackKind::Image`] and the manifest declares `thumbnail_tile`.
    pub thumbnail_tile: Option<(u32, u32)>,
    /// Effective MIME type from the adaptation set or one of its representations.
    pub mime_type: Option<String>,
    /// Effective RFC 5646 language from the adaptation set, content component, or representation.
    pub language: Option<String>,
    /// DASH `Role@value` values.
    pub roles: Vec<String>,
    /// Effective RFC 6381 codec strings advertised by the adaptation set or representations.
    pub codecs: Vec<String>,
    /// DASH accessibility descriptors as `(schemeIdUri, value)` pairs.
    pub accessibility: Vec<TrackDescriptor>,
    /// `EssentialProperty` descriptors aggregated from the adaptation set and its children.
    pub essential_properties: Vec<TrackDescriptor>,
    /// `SupplementalProperty` descriptors aggregated from the adaptation set and its children.
    pub supplemental_properties: Vec<TrackDescriptor>,
}

pub(crate) struct SelectedAdaptationSet<'a> {
    pub adaptation_set: &'a AdaptationSet,
    pub info: TrackInfo,
}

fn track_kind(adaptation_set: &AdaptationSet) -> Option<TrackKind> {
    if is_trick_play_adaptation_set(adaptation_set) {
        return Some(TrackKind::TrickPlay);
    }
    if is_image_adaptation_set(adaptation_set) {
        return Some(TrackKind::Image);
    }
    if dash_mpd::is_audio_adaptation(&adaptation_set) {
        return Some(TrackKind::Audio);
    }
    if dash_mpd::is_video_adaptation(&adaptation_set) {
        return Some(TrackKind::Video);
    }
    if dash_mpd::is_subtitle_adaptation(&adaptation_set) {
        return Some(TrackKind::Text);
    }
    None
}

fn is_image_adaptation_set(adaptation_set: &AdaptationSet) -> bool {
    effective_mime_type(adaptation_set)
        .is_some_and(|mime_type| mime_type.eq_ignore_ascii_case("image/jpeg"))
        || is_thumbnail_tile_adaptation_set(adaptation_set)
}

fn effective_mime_type(adaptation_set: &AdaptationSet) -> Option<String> {
    adaptation_set.mimeType.clone().or_else(|| {
        adaptation_set
            .representations
            .iter()
            .find_map(|representation| representation.mimeType.clone())
    })
}

fn effective_language(adaptation_set: &AdaptationSet) -> Option<String> {
    adaptation_set
        .lang
        .clone()
        .or_else(|| {
            adaptation_set
                .ContentComponent
                .iter()
                .find_map(|component| component.lang.clone())
        })
        .or_else(|| {
            adaptation_set
                .representations
                .iter()
                .find_map(|representation| representation.lang.clone())
        })
}

fn codec_values(adaptation_set: &AdaptationSet) -> Vec<String> {
    let mut codecs = Vec::new();
    for value in std::iter::once(adaptation_set.codecs.as_deref())
        .chain(
            adaptation_set
                .representations
                .iter()
                .map(|representation| representation.codecs.as_deref()),
        )
        .flatten()
    {
        for codec in value
            .split(',')
            .map(str::trim)
            .filter(|codec| !codec.is_empty())
        {
            if !codecs
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(codec))
            {
                codecs.push(codec.to_string());
            }
        }
    }
    codecs
}

fn role_values(adaptation_set: &AdaptationSet, supplemental_roles: &[String]) -> Vec<String> {
    let mut roles: Vec<String> = adaptation_set
        .Role
        .iter()
        .chain(
            adaptation_set
                .ContentComponent
                .iter()
                .flat_map(|component| component.Role.iter()),
        )
        .filter_map(|role| role.value.clone())
        .collect();

    for role in supplemental_roles {
        if !roles
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(role))
        {
            roles.push(role.clone());
        }
    }
    roles
}

fn subtitle_type_for(adaptation_set: &AdaptationSet) -> dash_mpd::SubtitleType {
    dash_mpd::subtitle_type(&adaptation_set)
}

fn track_info(
    adaptation_set: &AdaptationSet,
    period_adaptation_index: usize,
    kind: TrackKind,
) -> TrackInfo {
    let (essential_properties, supplemental_properties, supplemental_roles) =
        adaptation_descriptor_metadata(adaptation_set);

    TrackInfo {
        period_adaptation_index,
        id: adaptation_set.id.clone(),
        kind,
        subtitle_type: (kind == TrackKind::Text).then(|| subtitle_type_for(adaptation_set)),
        thumbnail_tile: if kind == TrackKind::Image {
            thumbnail_tile_layout(adaptation_set)
        } else {
            None
        },
        mime_type: effective_mime_type(adaptation_set),
        language: effective_language(adaptation_set),
        roles: role_values(adaptation_set, &supplemental_roles),
        codecs: codec_values(adaptation_set),
        accessibility: adaptation_set
            .Accessibility
            .iter()
            .chain(
                adaptation_set
                    .ContentComponent
                    .iter()
                    .flat_map(|component| component.Accessibility.iter()),
            )
            .map(|descriptor| TrackDescriptor {
                scheme_id_uri: descriptor.schemeIdUri.clone(),
                value: descriptor.value.clone(),
            })
            .collect(),
        essential_properties,
        supplemental_properties,
    }
}

fn language_matches(language: &str, range: &str) -> bool {
    language.eq_ignore_ascii_case(range)
        || language
            .get(..range.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(range))
            && language.as_bytes().get(range.len()) == Some(&b'-')
}

fn codec_matches(codec: &str, preference: &str) -> bool {
    codec.eq_ignore_ascii_case(preference)
        || codec
            .get(..preference.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(preference))
            && matches!(codec.as_bytes().get(preference.len()), Some(b'.' | b'-'))
}

fn descriptor_matches(candidate: &TrackDescriptor, preference: &TrackDescriptor) -> bool {
    candidate
        .scheme_id_uri
        .eq_ignore_ascii_case(&preference.scheme_id_uri)
        && preference.value.as_ref().is_none_or(|preferred_value| {
            candidate
                .value
                .as_ref()
                .is_some_and(|value| value.eq_ignore_ascii_case(preferred_value))
        })
}

fn match_rank<T>(
    preferences: &[T],
    candidates: impl IntoIterator<Item = impl AsRef<str>>,
    matches: impl Fn(&str, &str) -> bool,
) -> usize
where
    T: AsRef<str>,
{
    let candidates: Vec<_> = candidates.into_iter().collect();
    preferences
        .iter()
        .position(|preference| {
            candidates
                .iter()
                .any(|candidate| matches(candidate.as_ref(), preference.as_ref()))
        })
        .unwrap_or(preferences.len())
}

fn descriptor_rank(preferences: &[TrackDescriptor], candidates: &[TrackDescriptor]) -> usize {
    preferences
        .iter()
        .position(|preference| {
            candidates
                .iter()
                .any(|candidate| descriptor_matches(candidate, preference))
        })
        .unwrap_or(preferences.len())
}

fn select_kind(
    candidates: &mut Vec<(usize, &AdaptationSet, TrackInfo)>,
    preference: &TrackPreference,
) {
    candidates.sort_by_key(|(document_index, adaptation_set, info)| {
        (
            match_rank(
                &preference.languages,
                info.language.iter(),
                language_matches,
            ),
            match_rank(&preference.roles, &info.roles, |role, preferred| {
                role.eq_ignore_ascii_case(preferred)
            }),
            match_rank(&preference.codecs, &info.codecs, codec_matches),
            descriptor_rank(&preference.accessibility, &info.accessibility),
            std::cmp::Reverse(adaptation_set.selectionPriority.unwrap_or(1)),
            *document_index,
        )
    });
    if let Some(max_tracks) = preference.max_tracks {
        candidates.truncate(max_tracks);
    }
}

pub(crate) fn select_adaptation_sets<'a>(
    period: &'a Period,
    selection: &TrackSelection,
) -> Vec<SelectedAdaptationSet<'a>> {
    let mut audio = Vec::new();
    let mut video = Vec::new();
    let mut text = Vec::new();
    let mut trick_play = Vec::new();
    let mut image = Vec::new();

    for (document_index, adaptation_set) in period.adaptations.iter().enumerate() {
        let Some(kind) = track_kind(adaptation_set) else {
            continue;
        };
        if matches!(kind, TrackKind::Audio | TrackKind::Video | TrackKind::Text)
            && !is_playback_adaptation_set(adaptation_set)
        {
            continue;
        }
        let candidate = (
            document_index,
            adaptation_set,
            track_info(adaptation_set, document_index, kind),
        );
        match kind {
            TrackKind::Audio => audio.push(candidate),
            TrackKind::Video => video.push(candidate),
            TrackKind::Text => text.push(candidate),
            TrackKind::TrickPlay => trick_play.push(candidate),
            TrackKind::Image => image.push(candidate),
        }
    }

    select_kind(&mut audio, &selection.audio);
    select_kind(&mut video, &selection.video);
    select_kind(&mut text, &selection.text);
    select_kind(&mut trick_play, &selection.trick_play);
    select_kind(&mut image, &selection.image);

    let mut selected: Vec<_> = audio
        .into_iter()
        .chain(video)
        .chain(text)
        .chain(trick_play)
        .chain(image)
        .collect();
    selected.sort_by_key(|(document_index, _, _)| *document_index);
    selected
        .into_iter()
        .map(|(_, adaptation_set, info)| SelectedAdaptationSet {
            adaptation_set,
            info,
        })
        .collect()
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
    fn default_selection_retains_supported_tracks_in_manifest_order() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="v" contentType="video"/>
                <AdaptationSet id="a1" contentType="audio" lang="en"/>
                <AdaptationSet id="text" contentType="text"/>
                <AdaptationSet id="a2" contentType="audio" lang="fr"/>
            </Period></MPD>"#,
        );

        let selected = select_adaptation_sets(&period, &TrackSelection::default());
        assert_eq!(
            selected
                .iter()
                .map(|track| track.info.id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("v"), Some("a1"), Some("a2")]
        );
    }

    #[test]
    fn ordered_preferences_select_language_role_codec_and_accessibility() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="described" contentType="audio" lang="en-GB" codecs="ec-3">
                  <Role schemeIdUri="urn:mpeg:dash:role:2011" value="commentary"/>
                  <Accessibility schemeIdUri="urn:tva:metadata:cs:AudioPurposeCS:2007" value="1"/>
                </AdaptationSet>
                <AdaptationSet id="main" contentType="audio" lang="fr" codecs="mp4a.40.2">
                  <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        let audio = TrackPreference::default()
            .language("en")
            .role("commentary")
            .codec("ec-3")
            .accessibility(TrackDescriptor::new(
                "urn:tva:metadata:cs:AudioPurposeCS:2007",
                "1",
            ))
            .max_tracks(1);

        let selected =
            select_adaptation_sets(&period, &TrackSelection::default().with_audio(audio));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].info.id.as_deref(), Some("described"));
    }

    #[test]
    fn multiple_preferred_audio_tracks_are_supported() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="fr" mimeType="audio/mp4" lang="fr"/>
                <AdaptationSet id="en" mimeType="audio/mp4" lang="en"/>
                <AdaptationSet id="de" mimeType="audio/mp4" lang="de"/>
            </Period></MPD>"#,
        );
        let audio = TrackPreference::default()
            .language("en")
            .language("fr")
            .max_tracks(2);

        let selected =
            select_adaptation_sets(&period, &TrackSelection::default().with_audio(audio));
        assert_eq!(
            selected
                .iter()
                .map(|track| track.info.id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("fr"), Some("en")]
        );
    }

    #[test]
    fn text_tracks_are_excluded_by_default() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="sub" contentType="text" mimeType="application/ttml+xml" lang="en"/>
                <AdaptationSet id="video" contentType="video" mimeType="video/mp4"/>
            </Period></MPD>"#,
        );

        let selected = select_adaptation_sets(&period, &TrackSelection::default());
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].info.kind, TrackKind::Video);
    }

    #[test]
    fn text_tracks_are_selected_when_preferences_enable_them() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="sub" contentType="text" mimeType="application/ttml+xml" lang="en"/>
                <AdaptationSet id="video" contentType="video" mimeType="video/mp4"/>
            </Period></MPD>"#,
        );
        let selection = TrackSelection::default().with_text(
            TrackPreference::default()
                .language("en")
                .role("subtitle")
                .max_tracks(1),
        );

        let selected = select_adaptation_sets(&period, &selection);
        assert_eq!(
            selected
                .iter()
                .map(|track| track.info.id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("sub"), Some("video")]
        );
        assert_eq!(selected[0].info.kind, TrackKind::Text);
        assert_eq!(
            selected[0].info.subtitle_type,
            Some(dash_mpd::SubtitleType::Ttml)
        );
        assert_eq!(
            selected[0].info.mime_type.as_deref(),
            Some("application/ttml+xml")
        );
    }

    #[test]
    fn inband_stpp_codec_is_classified_as_text() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="cc" contentType="text" mimeType="application/mp4">
                  <Representation id="1" bandwidth="100" codecs="stpp.ttml.im1t"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        let selection =
            TrackSelection::default().with_text(TrackPreference::default().max_tracks(1));

        let selected = select_adaptation_sets(&period, &selection);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].info.kind, TrackKind::Text);
        assert_eq!(
            selected[0].info.subtitle_type,
            Some(dash_mpd::SubtitleType::Stpp)
        );
    }

    #[test]
    fn text_vtt_mime_type_is_classified_as_text() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="vtt" mimeType="text/vtt" lang="en"/>
            </Period></MPD>"#,
        );
        let selection =
            TrackSelection::default().with_text(TrackPreference::default().max_tracks(1));

        let selected = select_adaptation_sets(&period, &selection);
        assert_eq!(selected.len(), 1);
        assert_eq!(
            selected[0].info.subtitle_type,
            Some(dash_mpd::SubtitleType::Vtt)
        );
    }

    #[test]
    fn period_adaptation_index_skips_non_playback_sets() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="text" contentType="text" mimeType="application/ttml+xml" lang="en"/>
                <AdaptationSet id="video" contentType="video" mimeType="video/mp4"/>
            </Period></MPD>"#,
        );

        let selected = select_adaptation_sets(&period, &TrackSelection::default());
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].info.id.as_deref(), Some("video"));
        assert_eq!(selected[0].info.period_adaptation_index, 1);
    }

    #[test]
    fn trick_mode_adaptation_set_is_excluded_by_default() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="main" contentType="video" mimeType="video/mp4"/>
                <AdaptationSet id="trick" contentType="video" mimeType="video/mp4">
                  <EssentialProperty schemeIdUri="http://dashif.org/guidelines/trickmode" value="1"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );

        let selected = select_adaptation_sets(&period, &TrackSelection::default());
        assert_eq!(
            selected
                .iter()
                .map(|track| track.info.id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("main")]
        );
    }

    #[test]
    fn trick_play_tracks_are_selected_when_preferences_enable_them() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="main" contentType="video" mimeType="video/mp4"/>
                <AdaptationSet id="trick" contentType="video" mimeType="video/mp4">
                  <EssentialProperty schemeIdUri="http://dashif.org/guidelines/trickmode" value="1"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        let selection = TrackSelection::default()
            .with_video(TrackPreference::default().max_tracks(0))
            .with_trick_play(TrackPreference::default().max_tracks(1));

        let selected = select_adaptation_sets(&period, &selection);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].info.kind, TrackKind::TrickPlay);
        assert_eq!(selected[0].info.id.as_deref(), Some("trick"));
    }

    #[test]
    fn image_jpeg_tracks_are_excluded_by_default() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="thumb" mimeType="image/jpeg" contentType="image"/>
                <AdaptationSet id="video" contentType="video" mimeType="video/mp4"/>
            </Period></MPD>"#,
        );

        let selected = select_adaptation_sets(&period, &TrackSelection::default());
        assert_eq!(
            selected
                .iter()
                .map(|track| track.info.id.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("video")]
        );
    }

    #[test]
    fn image_tracks_are_selected_when_preferences_enable_them() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="thumb" mimeType="image/jpeg" contentType="image">
                  <EssentialProperty schemeIdUri="http://dashif.org/guidelines/thumbnail_tile" value="10x5"/>
                  <Representation id="tiles" bandwidth="1000" width="320" height="180"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        let selection =
            TrackSelection::default().with_image(TrackPreference::default().max_tracks(1));

        let selected = select_adaptation_sets(&period, &selection);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].info.kind, TrackKind::Image);
        assert_eq!(selected[0].info.thumbnail_tile, Some((10, 5)));
        assert_eq!(selected[0].info.mime_type.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn supplemental_role_descriptor_is_used_for_selection() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="dub" contentType="audio" lang="en">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:role:2011" value="dub"/>
                </AdaptationSet>
                <AdaptationSet id="main" contentType="audio" lang="en">
                  <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        let audio = TrackPreference::default().role("dub").max_tracks(1);

        let selected =
            select_adaptation_sets(&period, &TrackSelection::default().with_audio(audio));
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].info.id.as_deref(), Some("dub"));
        assert_eq!(selected[0].info.roles, vec!["dub"]);
    }

    #[test]
    fn representation_metadata_is_used_when_adaptation_metadata_is_absent() {
        let period = period(
            r#"<MPD><Period>
                <AdaptationSet id="video">
                  <Representation id="h264" mimeType="video/mp4" codecs="avc1.4d401f"/>
                </AdaptationSet>
            </Period></MPD>"#,
        );
        let video = TrackPreference::default().codec("avc1").max_tracks(1);

        let selected =
            select_adaptation_sets(&period, &TrackSelection::default().with_video(video));
        assert_eq!(selected[0].info.kind, TrackKind::Video);
        assert_eq!(selected[0].info.mime_type.as_deref(), Some("video/mp4"));
        assert_eq!(selected[0].info.codecs, vec!["avc1.4d401f"]);
    }
}
