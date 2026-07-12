//! Deterministic adaptation-set selection.

use dash_mpd::{AdaptationSet, Period};

use super::descriptors::{
    adaptation_descriptor_metadata, is_playback_adaptation_set, is_thumbnail_tile_adaptation_set,
    is_trick_play_adaptation_set, thumbnail_tile_layout,
};

use super::info::TrackInfo;
use super::kind::{TrackDescriptor, TrackKind, TrackPreference, TrackSelection};

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
