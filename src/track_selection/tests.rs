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
    assert!(selected[0].info.sub_tracks.is_empty());
}

#[test]
fn adaptation_and_representation_labels_and_ratings_are_surfaced() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="video" contentType="video" mimeType="video/mp4">
                  <Rating schemeIdUri="urn:mpeg:dash:rating:2011" value="PG"/>
                  <Label id="v" lang="en">Main</Label>
                  <ContentComponent id="0" contentType="video">
                    <Rating schemeIdUri="urn:org:example:rating" value="TV-14"/>
                  </ContentComponent>
                  <Representation id="r0" bandwidth="1000000">
                    <Label lang="en">720p</Label>
                  </Representation>
                </AdaptationSet>
            </Period></MPD>"#,
    );

    let selected = select_adaptation_sets(&period, &TrackSelection::default());
    assert_eq!(selected.len(), 1);
    let info = &selected[0].info;
    assert_eq!(info.labels.len(), 1);
    assert_eq!(info.labels[0].content, "Main");
    assert_eq!(
        info.ratings,
        vec![
            TrackDescriptor::new("urn:mpeg:dash:rating:2011", "PG"),
            TrackDescriptor::new("urn:org:example:rating", "TV-14"),
        ]
    );
    assert_eq!(info.representation_labels.len(), 1);
    assert_eq!(info.representation_labels[0].0, 0);
    assert_eq!(info.representation_labels[0].1[0].content, "720p");
}

#[test]
fn sub_representation_codecs_participate_in_preference_ranking() {
    let period = period(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"><Period>
                <AdaptationSet id="hevc" mimeType="video/mp4" contentType="video">
                  <Representation id="h" bandwidth="1000000" codecs="hvc1.1.6.L93.B0"/>
                </AdaptationSet>
                <AdaptationSet id="avc" mimeType="video/mp4" width="640" height="480">
                  <ContentComponent id="0" contentType="video"/>
                  <ContentComponent id="1" contentType="audio" lang="en"/>
                  <Representation id="mux" bandwidth="512000">
                    <SubRepresentation level="0" contentComponent="0" bandwidth="128000"
                                       codecs="avc1.4D401E" maxPlayoutRate="4"/>
                    <SubRepresentation level="2" contentComponent="1" bandwidth="64000"
                                       codecs="mp4a.40"/>
                  </Representation>
                </AdaptationSet>
            </Period></MPD>"#,
    );
    let video = TrackPreference::default().codec("avc1").max_tracks(1);

    let selected = select_adaptation_sets(&period, &TrackSelection::default().with_video(video));
    assert_eq!(selected[0].info.id.as_deref(), Some("avc"));
    assert_eq!(selected[0].info.codecs, vec!["avc1.4D401E", "mp4a.40"]);
    assert_eq!(selected[0].info.sub_tracks.len(), 2);
    assert_eq!(selected[0].info.sub_tracks[0].level, Some(0));
    assert_eq!(selected[0].info.sub_tracks[0].max_playout_rate, Some(4.0));
    assert_eq!(
        selected[0].info.sub_tracks[1].language.as_deref(),
        Some("en")
    );
    assert_eq!(selected[0].info.sub_tracks[0].width, Some(640));
    assert_eq!(selected[0].info.sub_tracks[0].height, Some(480));
}

#[test]
fn preselection_element_selects_partial_adaptation_sets() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="1" contentType="audio" mimeType="audio/mp4" lang="en">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                  <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
                </AdaptationSet>
                <AdaptationSet id="2" contentType="audio" mimeType="audio/mp4">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                </AdaptationSet>
                <AdaptationSet id="v" contentType="video" mimeType="video/mp4"/>
                <Preselection id="ps1" preselectionComponents="1 2" lang="en" codecs="mp4a.40.2" tag="1">
                  <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
                </Preselection>
            </Period></MPD>"#,
    );

    let selected = select_adaptation_sets(
        &period,
        &TrackSelection::default().with_audio(TrackPreference::default().max_tracks(1)),
    );
    assert_eq!(
        selected
            .iter()
            .map(|track| track.info.id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("1"), Some("2"), Some("v")]
    );
}

#[test]
fn preselection_language_preference_picks_matching_bundle() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="1" contentType="audio" mimeType="audio/mp4" lang="en">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                </AdaptationSet>
                <AdaptationSet id="2" contentType="audio" mimeType="audio/mp4">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                </AdaptationSet>
                <AdaptationSet id="3" contentType="audio" mimeType="audio/mp4" lang="fr">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                </AdaptationSet>
                <AdaptationSet id="4" contentType="audio" mimeType="audio/mp4">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                </AdaptationSet>
                <Preselection id="en" preselectionComponents="1 2" lang="en" tag="1"/>
                <Preselection id="fr" preselectionComponents="3 4" lang="fr" tag="2"/>
            </Period></MPD>"#,
    );
    let audio = TrackPreference::default().language("fr").max_tracks(1);

    let selected = select_adaptation_sets(&period, &TrackSelection::default().with_audio(audio));
    assert_eq!(
        selected
            .iter()
            .map(|track| track.info.id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("3"), Some("4")]
    );
}

#[test]
fn preselection_descriptor_defines_selectable_bundle() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="1" contentType="audio" mimeType="audio/mp4" lang="de">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016" value="bundle,1 2"/>
                </AdaptationSet>
                <AdaptationSet id="2" contentType="audio" mimeType="audio/mp4">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
                </AdaptationSet>
            </Period></MPD>"#,
    );

    let selected = select_adaptation_sets(
        &period,
        &TrackSelection::default().with_audio(TrackPreference::default().max_tracks(1)),
    );
    assert_eq!(
        selected
            .iter()
            .map(|track| track.info.id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("1"), Some("2")]
    );
}

#[test]
fn partial_preselection_adaptation_set_is_not_selected_alone() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="main" contentType="audio" mimeType="audio/mp4" lang="en"/>
                <AdaptationSet id="partial" contentType="audio" mimeType="audio/mp4">
                  <EssentialProperty schemeIdUri="urn:mpeg:dash:preselection:2016"/>
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
fn adaptation_set_switching_collapses_to_one_track_with_peers() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="264" mimeType="video/mp4" contentType="video" codecs="avc1.42E01E">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="265"/>
                  <Representation id="avc" bandwidth="500000"/>
                </AdaptationSet>
                <AdaptationSet id="265" mimeType="video/mp4" contentType="video" codecs="hev1.1.6.L93.B0">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="264"/>
                  <Representation id="hevc" bandwidth="800000"/>
                </AdaptationSet>
            </Period></MPD>"#,
    );

    let selected = select_adaptation_sets(
        &period,
        &TrackSelection::default().with_video(TrackPreference::default().max_tracks(2)),
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].info.id.as_deref(), Some("264"));
    assert_eq!(selected[0].info.switchable_adaptation_indices, vec![1]);
    assert_eq!(
        selected[0].info.switchable_adaptation_set_ids,
        vec!["265".to_string()]
    );
    assert_eq!(selected[0].switch_peers.len(), 1);
    assert!(
        selected[0]
            .info
            .codecs
            .iter()
            .any(|c| c.starts_with("hev1"))
    );
}

#[test]
fn max_tracks_one_still_attaches_switchable_peer() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="1" mimeType="video/mp4" contentType="video" codecs="avc1">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="2"/>
                  <Representation id="a" bandwidth="100000"/>
                </AdaptationSet>
                <AdaptationSet id="2" mimeType="video/mp4" contentType="video" codecs="hev1">
                  <SupplementalProperty schemeIdUri="urn:mpeg:dash:adaptation-set-switching:2016" value="1"/>
                  <Representation id="b" bandwidth="200000"/>
                </AdaptationSet>
            </Period></MPD>"#,
    );

    let selected = select_adaptation_sets(
        &period,
        &TrackSelection::default().with_video(TrackPreference::default().max_tracks(1)),
    );
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].switch_peers.len(), 1);
}

#[test]
fn dvb_fallback_is_peer_not_standalone_track() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="main" mimeType="audio/mp4" contentType="audio" lang="en">
                  <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
                  <Representation id="hi" bandwidth="128000"/>
                </AdaptationSet>
                <AdaptationSet id="fb" mimeType="audio/mp4" contentType="audio" lang="en">
                  <Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
                  <SupplementalProperty schemeIdUri="urn:dvb:dash:fallback_adaptation_set:2014" value="main"/>
                  <Representation id="lo" bandwidth="48000"/>
                </AdaptationSet>
            </Period></MPD>"#,
    );

    let selected = select_adaptation_sets(&period, &TrackSelection::default());
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].info.id.as_deref(), Some("main"));
    assert_eq!(selected[0].info.switchable_adaptation_indices, vec![1]);
    assert_eq!(selected[0].switch_peers.len(), 1);
}

#[test]
fn prefers_ac4_mha1_and_vp09_codec_prefixes() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="aac" mimeType="audio/mp4" contentType="audio" lang="en">
                  <Representation id="a" bandwidth="96000" codecs="mp4a.40.2"/>
                </AdaptationSet>
                <AdaptationSet id="ac4" mimeType="audio/mp4" contentType="audio" lang="en">
                  <Representation id="b" bandwidth="128000" codecs="ac-4.02.01.00"/>
                </AdaptationSet>
                <AdaptationSet id="mha" mimeType="audio/mp4" contentType="audio" lang="en">
                  <Representation id="c" bandwidth="256000" codecs="mha1.0.4.L3.C"/>
                </AdaptationSet>
                <AdaptationSet id="avc" mimeType="video/mp4" contentType="video">
                  <Representation id="d" bandwidth="800000" codecs="avc1.4d401f"/>
                </AdaptationSet>
                <AdaptationSet id="vp9" mimeType="video/mp4" contentType="video">
                  <EssentialProperty schemeIdUri="urn:dvb:dash:hdr-dmi" value="HDR10"/>
                  <Representation id="e" bandwidth="2000000" codecs="vp09.02.10.10.01.09.16.09.01"/>
                </AdaptationSet>
            </Period></MPD>"#,
    );

    let audio = select_adaptation_sets(
        &period,
        &TrackSelection::default()
            .with_video(TrackPreference::default().max_tracks(0))
            .with_audio(TrackPreference::default().codec("ac-4").max_tracks(1)),
    );
    assert_eq!(audio[0].info.id.as_deref(), Some("ac4"));

    let mpeg_h = select_adaptation_sets(
        &period,
        &TrackSelection::default()
            .with_video(TrackPreference::default().max_tracks(0))
            .with_audio(TrackPreference::default().codec("mha1").max_tracks(1)),
    );
    assert_eq!(mpeg_h[0].info.id.as_deref(), Some("mha"));

    let video = select_adaptation_sets(
        &period,
        &TrackSelection::default()
            .with_audio(TrackPreference::default().max_tracks(0))
            .with_video(TrackPreference::default().codec("vp09").max_tracks(1)),
    );
    assert_eq!(video[0].info.id.as_deref(), Some("vp9"));
}

#[test]
fn selects_mp2t_video_adaptation_set() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="ts" mimeType="video/mp2t" contentType="video">
                  <Representation id="1" bandwidth="1000000" codecs="avc1.4d401f"/>
                </AdaptationSet>
            </Period></MPD>"#,
    );
    let selected = select_adaptation_sets(&period, &TrackSelection::default());
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].info.kind, TrackKind::Video);
    assert_eq!(selected[0].info.mime_type.as_deref(), Some("video/mp2t"));
}

#[test]
fn selects_webm_video_and_audio_adaptation_sets() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="v" mimeType="video/webm" contentType="video">
                  <Representation id="1" bandwidth="1000000" codecs="vp9"/>
                </AdaptationSet>
                <AdaptationSet id="a" mimeType="audio/webm" contentType="audio" lang="en">
                  <Representation id="2" bandwidth="128000" codecs="opus"/>
                </AdaptationSet>
            </Period></MPD>"#,
    );
    let selected = select_adaptation_sets(&period, &TrackSelection::default());
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].info.kind, TrackKind::Video);
    assert_eq!(selected[0].info.mime_type.as_deref(), Some("video/webm"));
    assert_eq!(selected[1].info.kind, TrackKind::Audio);
    assert_eq!(selected[1].info.mime_type.as_deref(), Some("audio/webm"));
}

#[test]
fn image_png_and_bmp_tracks_are_selected_when_enabled() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="png" mimeType="image/png" contentType="image">
                  <Representation id="tiles" bandwidth="1000" width="320" height="180"/>
                </AdaptationSet>
                <AdaptationSet id="bmp" mimeType="image/bmp" contentType="image">
                  <EssentialProperty schemeIdUri="http://dashif.org/thumbnail_tile" value="3x1"/>
                  <Representation id="tiles" bandwidth="1000" width="160" height="90"/>
                </AdaptationSet>
            </Period></MPD>"#,
    );
    let selection = TrackSelection::default().with_image(TrackPreference::default().max_tracks(2));

    let selected = select_adaptation_sets(&period, &selection);
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].info.kind, TrackKind::Image);
    assert_eq!(selected[0].info.mime_type.as_deref(), Some("image/png"));
    assert_eq!(selected[1].info.mime_type.as_deref(), Some("image/bmp"));
    assert_eq!(selected[1].info.thumbnail_tile, Some((3, 1)));
}

#[test]
fn track_info_exposes_adaptation_set_range_attributes() {
    let period = period(
        r#"<MPD><Period>
                <AdaptationSet id="v" contentType="video"
                               minBandwidth="500000" maxBandwidth="3000000"
                               minWidth="640" maxWidth="1920"
                               minHeight="360" maxHeight="1080"
                               minFrameRate="24" maxFrameRate="30000/1001">
                  <Representation id="low" bandwidth="800000" width="1280" height="720"/>
                  <Representation id="high" bandwidth="5000000" width="3840" height="2160"/>
                </AdaptationSet>
            </Period></MPD>"#,
    );

    let selected = select_adaptation_sets(&period, &TrackSelection::default());
    assert_eq!(selected.len(), 1);
    let info = &selected[0].info;
    assert_eq!(info.min_bandwidth_bps, Some(500_000));
    assert_eq!(info.max_bandwidth_bps, Some(3_000_000));
    assert_eq!(info.min_width, Some(640));
    assert_eq!(info.max_width, Some(1920));
    assert_eq!(info.min_height, Some(360));
    assert_eq!(info.max_height, Some(1080));
    assert_eq!(info.min_frame_rate.as_deref(), Some("24"));
    assert_eq!(info.max_frame_rate.as_deref(), Some("30000/1001"));
}
