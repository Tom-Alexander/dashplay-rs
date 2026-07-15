//! Deterministic adaptation-set selection.

use dash_mpd::{AdaptationSet, Period};

use super::descriptors::{
    adaptation_descriptor_metadata, is_playback_adaptation_set, is_thumbnail_tile_adaptation_set,
    is_trick_play_adaptation_set, thumbnail_tile_layout,
};
use super::info::TrackInfo;
use super::kind::{TrackDescriptor, TrackKind, TrackPreference, TrackSelection};
use super::preselection::{ResolvedPreselection, resolve_preselections};
use super::sub_representation::{resolve_sub_tracks, sub_representation_codec_values};
use super::switching::{collapse_selected_into_switch_groups, is_dvb_fallback_adaptation_set};
use crate::manifest::content_label_from_dash;

pub(crate) struct SelectedAdaptationSet<'a> {
    pub adaptation_set: &'a AdaptationSet,
    pub info: TrackInfo,
    /// Peer adaptation sets in the same switch / DVB-fallback group (excluding the primary).
    pub switch_peers: Vec<(usize, &'a AdaptationSet)>,
}

pub(crate) fn track_kind(adaptation_set: &AdaptationSet) -> Option<TrackKind> {
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
    effective_mime_type(adaptation_set).is_some_and(|mime_type| is_image_mime_type(&mime_type))
        || is_thumbnail_tile_adaptation_set(adaptation_set)
}

/// `image/*` MIME types used for thumbnail / still-image adaptation sets.
fn is_image_mime_type(mime_type: &str) -> bool {
    mime_type.to_ascii_lowercase().starts_with("image/")
}

fn effective_mime_type(adaptation_set: &AdaptationSet) -> Option<String> {
    adaptation_set.mimeType.clone().or_else(|| {
        adaptation_set
            .representations
            .iter()
            .find_map(|representation| representation.mimeType.clone())
    })
}

pub(crate) fn effective_language(adaptation_set: &AdaptationSet) -> Option<String> {
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

pub(crate) fn codec_values(adaptation_set: &AdaptationSet) -> Vec<String> {
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
    for codec in sub_representation_codec_values(adaptation_set) {
        if !codecs
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&codec))
        {
            codecs.push(codec);
        }
    }
    codecs
}

pub(crate) fn role_values(
    adaptation_set: &AdaptationSet,
    supplemental_roles: &[String],
) -> Vec<String> {
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

    let sub_tracks = resolve_sub_tracks(adaptation_set);
    let mut accessibility: Vec<TrackDescriptor> = adaptation_set
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
        .collect();
    for sub_track in &sub_tracks {
        for descriptor in &sub_track.accessibility {
            if !accessibility.iter().any(|existing| existing == descriptor) {
                accessibility.push(descriptor.clone());
            }
        }
    }

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
        sub_tracks,
        accessibility,
        essential_properties,
        supplemental_properties,
        labels: adaptation_labels(adaptation_set),
        ratings: adaptation_ratings(adaptation_set),
        representation_labels: representation_labels(adaptation_set),
        switchable_adaptation_indices: Vec::new(),
        switchable_adaptation_set_ids: Vec::new(),
    }
}

fn adaptation_labels(adaptation_set: &AdaptationSet) -> Vec<crate::manifest::ContentLabel> {
    adaptation_set
        .Label
        .iter()
        .chain(adaptation_set.GroupLabel.iter())
        .map(content_label_from_dash)
        .collect()
}

fn adaptation_ratings(adaptation_set: &AdaptationSet) -> Vec<TrackDescriptor> {
    adaptation_set
        .Rating
        .iter()
        .chain(
            adaptation_set
                .ContentComponent
                .iter()
                .flat_map(|component| component.Rating.iter()),
        )
        .map(|rating| TrackDescriptor {
            scheme_id_uri: rating.schemeIdUri.clone(),
            value: rating.value.clone(),
        })
        .collect()
}

fn representation_labels(
    adaptation_set: &AdaptationSet,
) -> Vec<(usize, Vec<crate::manifest::ContentLabel>)> {
    adaptation_set
        .representations
        .iter()
        .enumerate()
        .filter_map(|(idx, rep)| {
            if rep.Label.is_empty() {
                return None;
            }
            Some((idx, rep.Label.iter().map(content_label_from_dash).collect()))
        })
        .collect()
}

fn with_switch_peers(mut info: TrackInfo, peers: &[(usize, &AdaptationSet)]) -> TrackInfo {
    info.switchable_adaptation_indices = peers.iter().map(|(idx, _)| *idx).collect();
    info.switchable_adaptation_set_ids = peers
        .iter()
        .filter_map(|(_, aset)| aset.id.clone())
        .collect();
    // Surface peer codecs on the primary track for preference / discovery.
    for (_, peer) in peers {
        for codec in codec_values(peer) {
            if !info
                .codecs
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&codec))
            {
                info.codecs.push(codec);
            }
        }
    }
    info
}

type CollapsedCandidate<'a> = (
    usize,
    &'a AdaptationSet,
    TrackInfo,
    Vec<(usize, &'a AdaptationSet)>,
);

/// Collapse switch/fallback groups within an already-ranked candidate list (best first).
fn collapse_kind_candidates<'a>(
    period: &'a Period,
    candidates: Vec<(usize, &'a AdaptationSet, TrackInfo)>,
) -> Vec<CollapsedCandidate<'a>> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let indices: Vec<usize> = candidates.iter().map(|(idx, _, _)| *idx).collect();
    let collapsed = collapse_selected_into_switch_groups(period, &indices);
    let mut by_index = std::collections::HashMap::new();
    for (idx, aset, info) in candidates {
        by_index.entry(idx).or_insert((aset, info));
    }

    collapsed
        .into_iter()
        .filter_map(|(primary_idx, peer_idxs)| {
            let (adaptation_set, info) = by_index.remove(&primary_idx)?;
            let peers: Vec<(usize, &'a AdaptationSet)> = peer_idxs
                .into_iter()
                .filter_map(|peer_idx| {
                    period
                        .adaptations
                        .get(peer_idx)
                        .map(|aset| (peer_idx, aset))
                })
                .collect();
            let info = with_switch_peers(info, &peers);
            Some((primary_idx, adaptation_set, info, peers))
        })
        .collect()
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

#[allow(clippy::large_enum_variant)] // TrackInfo carries MPD metadata; boxing is not worth it here.
enum KindCandidate<'a> {
    Standalone {
        document_index: usize,
        adaptation_set: &'a AdaptationSet,
        info: TrackInfo,
    },
    Preselection {
        preselection: ResolvedPreselection,
    },
}

fn rank_kind_candidate(
    candidate: &KindCandidate<'_>,
    preference: &TrackPreference,
) -> (
    usize,
    usize,
    usize,
    usize,
    std::cmp::Reverse<u64>,
    u8,
    usize,
) {
    match candidate {
        KindCandidate::Standalone {
            document_index,
            adaptation_set,
            info,
        } => (
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
            // Prefer Preselection bundles over an otherwise-equivalent standalone set so
            // partial Adaptation Sets are retained under `max_tracks(1)`.
            1,
            *document_index,
        ),
        KindCandidate::Preselection { preselection } => (
            match_rank(
                &preference.languages,
                preselection.language.iter(),
                language_matches,
            ),
            match_rank(&preference.roles, &preselection.roles, |role, preferred| {
                role.eq_ignore_ascii_case(preferred)
            }),
            match_rank(&preference.codecs, &preselection.codecs, codec_matches),
            descriptor_rank(&preference.accessibility, &preselection.accessibility),
            std::cmp::Reverse(preselection.selection_priority),
            0,
            preselection.document_index,
        ),
    }
}

fn select_kind_with_preselections<'a>(
    period: &'a Period,
    kind: TrackKind,
    preference: &TrackPreference,
    standalone: Vec<(usize, &'a AdaptationSet, TrackInfo)>,
    preselections: &[ResolvedPreselection],
) -> Vec<(usize, &'a AdaptationSet, TrackInfo)> {
    let kind_preselections: Vec<ResolvedPreselection> = preselections
        .iter()
        .filter(|preselection| preselection.main_kind == kind)
        .cloned()
        .collect();

    if kind_preselections.is_empty() {
        let mut candidates = standalone;
        select_kind(&mut candidates, preference);
        return candidates;
    }

    let mut candidates: Vec<KindCandidate<'a>> = standalone
        .into_iter()
        .map(
            |(document_index, adaptation_set, info)| KindCandidate::Standalone {
                document_index,
                adaptation_set,
                info,
            },
        )
        .chain(
            kind_preselections
                .into_iter()
                .map(|preselection| KindCandidate::Preselection { preselection }),
        )
        .collect();

    candidates.sort_by_key(|candidate| rank_kind_candidate(candidate, preference));
    if let Some(max_tracks) = preference.max_tracks {
        candidates.truncate(max_tracks);
    }

    let mut selected = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for candidate in candidates {
        match candidate {
            KindCandidate::Standalone {
                document_index,
                adaptation_set,
                info,
            } => {
                if seen.insert(document_index) {
                    selected.push((document_index, adaptation_set, info));
                }
            }
            KindCandidate::Preselection { preselection } => {
                for document_index in preselection.adaptation_indices {
                    if !seen.insert(document_index) {
                        continue;
                    }
                    let Some(adaptation_set) = period.adaptations.get(document_index) else {
                        continue;
                    };
                    let Some(component_kind) = track_kind(adaptation_set) else {
                        continue;
                    };
                    // Expand only components that match the selected kind; other media
                    // kinds keep their own selection pass.
                    if component_kind != kind {
                        continue;
                    }
                    selected.push((
                        document_index,
                        adaptation_set,
                        track_info(adaptation_set, document_index, kind),
                    ));
                }
            }
        }
    }
    selected
}

pub(crate) fn select_adaptation_sets<'a>(
    period: &'a Period,
    selection: &TrackSelection,
) -> Vec<SelectedAdaptationSet<'a>> {
    let preselections = resolve_preselections(period);
    let mut audio = Vec::new();
    let mut video = Vec::new();
    let mut text = Vec::new();
    let mut trick_play = Vec::new();
    let mut image = Vec::new();

    for (document_index, adaptation_set) in period.adaptations.iter().enumerate() {
        let Some(kind) = track_kind(adaptation_set) else {
            continue;
        };
        if matches!(kind, TrackKind::Audio | TrackKind::Video | TrackKind::Text) {
            // Partial Preselection sets are delivered only when their Preselection is chosen.
            if super::preselection::is_partial_preselection_adaptation_set(adaptation_set) {
                continue;
            }
            // DVB fallback sets are delivered only as peers of their primary.
            if is_dvb_fallback_adaptation_set(adaptation_set) {
                continue;
            }
            if !is_playback_adaptation_set(adaptation_set) {
                continue;
            }
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

    let audio = collapse_kind_candidates(
        period,
        select_kind_with_preselections(
            period,
            TrackKind::Audio,
            &selection.audio,
            audio,
            &preselections,
        ),
    );
    let video = collapse_kind_candidates(
        period,
        select_kind_with_preselections(
            period,
            TrackKind::Video,
            &selection.video,
            video,
            &preselections,
        ),
    );
    let text = collapse_kind_candidates(
        period,
        select_kind_with_preselections(
            period,
            TrackKind::Text,
            &selection.text,
            text,
            &preselections,
        ),
    );
    select_kind(&mut trick_play, &selection.trick_play);
    select_kind(&mut image, &selection.image);
    let trick_play = collapse_kind_candidates(period, trick_play);
    let image = collapse_kind_candidates(period, image);

    let mut selected: Vec<_> = audio
        .into_iter()
        .chain(video)
        .chain(text)
        .chain(trick_play)
        .chain(image)
        .collect();
    selected.sort_by_key(|(document_index, _, _, _)| *document_index);
    selected
        .into_iter()
        .map(
            |(_, adaptation_set, info, switch_peers)| SelectedAdaptationSet {
                adaptation_set,
                info,
                switch_peers,
            },
        )
        .collect()
}
