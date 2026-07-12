use dash_mpd::Period;

use super::kind::TrackDescriptor;
use super::{TrackKind, TrackPreference, TrackSelection, select_adaptation_sets};

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

    let selected = select_adaptation_sets(&period, &TrackSelection::default().with_audio(audio));
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

    let selected = select_adaptation_sets(&period, &TrackSelection::default().with_audio(audio));
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
    let selection = TrackSelection::default().with_text(TrackPreference::default().max_tracks(1));

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
    let selection = TrackSelection::default().with_text(TrackPreference::default().max_tracks(1));

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
    let selection = TrackSelection::default().with_image(TrackPreference::default().max_tracks(1));

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

    let selected = select_adaptation_sets(&period, &TrackSelection::default().with_audio(audio));
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

    let selected = select_adaptation_sets(&period, &TrackSelection::default().with_video(video));
    assert_eq!(selected[0].info.kind, TrackKind::Video);
    assert_eq!(selected[0].info.mime_type.as_deref(), Some("video/mp4"));
    assert_eq!(selected[0].info.codecs, vec!["avc1.4d401f"]);
}
