//! DASH-IF IOP conformance integration tests.
//!
//! Each case uses a local fixture whose MPD structure is adapted from a published
//! DASH-IF test vector (see <https://testassets.dashif.org/>). Stub media bytes
//! stand in for real encoded segments so tests stay offline and deterministic.

mod common;
mod conformance;

use common::drm::{
    play_single_track_with_license_fetcher, spawn_drm_fixture_with_mock_license,
    static_license_fetcher,
};
use common::{
    AdvancingLiveServer, FixtureServer, MultiPeriodLiveServer, has_end, init_payload,
    init_payloads, play_all_tracks, play_single_track, play_single_track_live, read_fixture,
    segment_numbers, segment_payloads,
};
use conformance::validate_iop;
use dashplay::drm::mpd::parse_mpd_drm_info;

const VOD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const LIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(800);
const LIVE_REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Fixtures with DASH-IF IOP `@profiles` that must pass schematron validation.
const IOP_FIXTURES: &[&str] = &[
    "dashif_simple",
    "dashif_bandwidth",
    "dashif_rep_template",
    "dashif_subtitle",
    "dashif_trick_play",
    "dashif_drm_encrypted",
];

/// Local fixtures whose MPDs must parse with `dash-mpd`.
const PARSE_FIXTURES: &[&str] = &[
    "dashif_simple",
    "dashif_bandwidth",
    "dashif_rep_template",
    "dashif_subtitle",
    "dashif_trick_play",
    "dashif_drm_encrypted",
    "vod_single",
    "vod_period_template",
    "vod_timeline",
    "vod_time",
    "vod_segment_base",
    "vod_segment_list",
    "vod_multi_period",
    "vod_av",
    "vod_abr",
    "vod_rep_fallback",
    "base_url_failover",
    "live_duration",
    "live_timeline",
    "drm_widevine",
];

/// Published DASH-IF vectors fetched over the network (manifest parse only).
const REMOTE_MPD_URLS: &[(&str, &str)] = &[
    (
        "TestCases/1a/qualcomm/1/MultiRate.mpd",
        "https://dash.akamaized.net/dash264/TestCases/1a/qualcomm/1/MultiRate.mpd",
    ),
    (
        "TestCases/4b/qualcomm/1/ED_OnDemand_5SecSeg_Subtitles.mpd",
        "https://dash.akamaized.net/dash264/TestCases/4b/qualcomm/1/ED_OnDemand_5SecSeg_Subtitles.mpd",
    ),
    (
        "TestCases/9a/qualcomm/1/MultiRate.mpd",
        "https://dash.akamaized.net/dash264/TestCases/9a/qualcomm/1/MultiRate.mpd",
    ),
    (
        "TestCases/5a/nomor/1.mpd",
        "https://dash.akamaized.net/dash264/TestCases/5a/nomor/1.mpd",
    ),
];

/// livesim2 `testpic_2s/Manifest_endNumber.mpd` — static VOD, `endNumber`, `$RepresentationID$`.
#[tokio::test]
async fn dashif_simple_end_number_segment_template() {
    let server = FixtureServer::spawn("dashif_simple").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-dashif-init".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 4);
    assert!(has_end(&events));
}

/// TestCases/1a — on-demand multi-rate with `$Bandwidth$` / `$RepresentationID$` templates.
#[tokio::test]
async fn dashif_bandwidth_template_variables() {
    let server = FixtureServer::spawn("dashif_bandwidth").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-abr-low-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-abr-low-seg-1".to_vec(),
            b"dashplay-abr-low-seg-2".to_vec(),
        ]
    );
    assert!(has_end(&events));
}

/// TestCases/5a/nomor/1.mpd — representation-level `SegmentTemplate` with `presentationTimeOffset`.
#[tokio::test]
async fn dashif_representation_level_segment_template() {
    let server = FixtureServer::spawn("dashif_rep_template").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    let init = init_payload(&events).expect("init");
    assert!(
        init == b"dashplay-abr-low-init" || init == b"dashplay-abr-high-init",
        "unexpected init payload: {init:?}"
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// TestCases/1c — live profile, `SegmentTemplate` + `SegmentTimeline`.
#[tokio::test]
async fn dashif_vod_segment_timeline() {
    let server = FixtureServer::spawn("vod_timeline").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-timeline-init".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// livesim2 `segtimeline_1/testpic_6s` — `$Time$` media template with `SegmentTimeline`.
#[tokio::test]
async fn dashif_time_template_addressing() {
    let server = FixtureServer::spawn("vod_time").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-time-0".to_vec(), b"dashplay-time-4000".to_vec(),]
    );
    assert!(has_end(&events));
}

/// TestCases/1a — single-file `SegmentBase` + `indexRange` (sidx-indexed).
#[tokio::test]
async fn dashif_segment_base_index_range() {
    let server = FixtureServer::spawn("vod_segment_base").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// `SegmentBase@indexRangeExact="true"` — index window is exact; media starts after it.
#[tokio::test]
async fn dashif_segment_base_index_range_exact() {
    let server = FixtureServer::spawn("vod_segment_base_index_range_exact").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"SEGMENT-1!!".to_vec(), b"SEGMENT-2!!".to_vec()]
    );
    assert!(has_end(&events));
}

/// `SegmentBase@indexRangeExact="false"` — Index Segment may extend past `@indexRange`.
#[tokio::test]
async fn dashif_segment_base_index_range_inexact() {
    let server = FixtureServer::spawn("vod_segment_base_index_range_inexact").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"SEGMENT-1!!".to_vec(), b"SEGMENT-2!!".to_vec()]
    );
    assert!(has_end(&events));
}

/// Explicit `SegmentList` / `SegmentURL` addressing.
#[tokio::test]
async fn dashif_segment_list() {
    let server = FixtureServer::spawn("vod_segment_list").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// `SegmentList` with `SegmentURL@mediaRange` / `Initialization@range` (byte-range-only).
#[tokio::test]
async fn dashif_segment_list_byte_ranges() {
    let server = FixtureServer::spawn("vod_segment_list_ranges").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-seg-1".to_vec(), b"dashplay-seg-2".to_vec()]
    );
    assert!(has_end(&events));
}

/// TestCases/5a — static multi-period VOD with per-period templates.
#[tokio::test]
async fn dashif_multi_period_vod() {
    let server = FixtureServer::spawn("vod_multi_period").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert_eq!(inits.len(), 2);
    assert_eq!(segment_payloads(&events).len(), 4);
    assert!(has_end(&events));
}

/// TestCases/3b — separate video and audio adaptation sets.
#[tokio::test]
async fn dashif_audio_video_adaptation_sets() {
    let server = FixtureServer::spawn("vod_av").await;
    let tracks = play_all_tracks(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(tracks.len(), 2);
    assert!(has_end(&tracks[0]));
    assert!(has_end(&tracks[1]));
    assert_eq!(init_payloads(&tracks[0]).len(), 1);
    assert_eq!(init_payloads(&tracks[1]).len(), 1);
    assert_eq!(segment_payloads(&tracks[0]).len(), 2);
    assert_eq!(segment_payloads(&tracks[1]).len(), 2);
}

/// livesim2 `testpic_2s/Manifest.mpd` — dynamic `@duration` template with `UTCTiming`.
#[tokio::test]
async fn dashif_live_duration_template() {
    let server = FixtureServer::spawn("live_duration").await;
    let events = play_single_track_live(&server.manifest_url, LIVE_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert!(segment_payloads(&events).len() >= 2);
    assert!(!has_end(&events));
}

/// livesim2 `segtimeline_1` — live `SegmentTimeline` bounded by `timeShiftBufferDepth`.
#[tokio::test]
async fn dashif_live_segment_timeline_tsbd() {
    let server = FixtureServer::spawn("live_timeline").await;
    let events = play_single_track_live(&server.manifest_url, LIVE_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    let numbers = segment_numbers(&events);
    assert!(numbers.len() >= 2);
    assert_eq!(&numbers[..2], [5, 6]);
    assert!(!has_end(&events));
}

/// TestCases/1a — basic on-demand `SegmentTemplate@duration`.
#[tokio::test]
async fn dashif_on_demand_duration_template() {
    let server = FixtureServer::spawn("vod_single").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// IOP — `SegmentTemplate` declared at Period level, merged with AdaptationSet fields.
#[tokio::test]
async fn dashif_period_level_segment_template() {
    let server = FixtureServer::spawn("vod_period_template").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// livesim2 `baseurl_*` — multiple `BaseURL` elements with priority-based failover.
#[tokio::test]
async fn dashif_multi_base_url_failover() {
    let server = FixtureServer::spawn_with_options("base_url_failover", &["/bad"]).await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-failover-init".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// TestCases/1a — multi-representation ABR selects highest feasible representation.
#[tokio::test]
async fn dashif_multi_representation_abr() {
    let server = FixtureServer::spawn("vod_abr").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-abr-high-init".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// IOP — representation fallback when the active representation's segment is unavailable.
#[tokio::test]
async fn dashif_representation_fallback() {
    let server = FixtureServer::spawn("vod_rep_fallback").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert!(
        inits.iter().any(|init| init == b"dashplay-abr-low-init"),
        "expected low-rep init after fallback, got {inits:?}"
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// livesim2 — dynamic MPD refresh advances the live window (`minimumUpdatePeriod`).
#[tokio::test]
async fn dashif_live_manifest_refresh() {
    let server = AdvancingLiveServer::spawn().await;
    let events = play_single_track_live(&server.manifest_url, LIVE_REFRESH_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    let numbers = segment_numbers(&events);
    assert!(
        numbers.iter().any(|&n| n >= 4),
        "expected segments after manifest refresh, got {numbers:?}"
    );
    assert!(!has_end(&events));
}

/// livesim2 `periods_*` — live multi-period transition re-emits initialization.
#[tokio::test]
async fn dashif_live_multi_period_transition() {
    let server = MultiPeriodLiveServer::spawn().await;
    let events = play_single_track_live(&server.manifest_url, LIVE_REFRESH_TIMEOUT)
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert!(inits.len() >= 2, "expected init per period, got {inits:?}");
    assert!(
        inits.iter().any(|init| init == b"dashplay-period2-init"),
        "expected period-2 init, got {inits:?}"
    );
    assert!(!has_end(&events));
}

/// Axinom / DASH-IF DRM vectors — Widevine `ContentProtection` is extracted from the MPD.
#[test]
fn dashif_widevine_drm_mpd_parses() {
    let xml = read_fixture("drm_widevine", "manifest.mpd");
    let info = parse_mpd_drm_info(&xml).expect("parse drm");
    let aset = &info.periods[0].adaptation_sets[0];
    assert!(!aset.effective.widevine_pssh.is_empty());
    assert_eq!(
        aset.effective.license_urls,
        vec!["https://license.example/wv".to_string()]
    );
}

/// DASH-IF DRM vectors — playback requires an external Widevine CDM device.
#[tokio::test]
async fn dashif_drm_playback_requires_device() {
    if std::env::var("DEVICE_PATH").is_ok() {
        return;
    }

    let server = FixtureServer::spawn("drm_widevine").await;
    let err = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect_err("expected DRM setup failure without CDM device");

    assert!(
        matches!(
            err,
            dashplay::PlayerError::Drm(dashplay::DrmError::License(_))
        ),
        "unexpected error: {err:?}"
    );
}

/// TestCases/4b — TTML subtitle adaptation sets parse; main video track still plays.
#[test]
fn dashif_subtitle_mpd_parses() {
    let xml = read_fixture("dashif_subtitle", "manifest.mpd");
    let mpd = dash_mpd::parse(&xml).expect("parse mpd");
    assert_eq!(mpd.periods.len(), 1);
    assert_eq!(mpd.periods[0].adaptations.len(), 2);
    assert!(
        mpd.periods[0]
            .adaptations
            .iter()
            .any(|aset| aset.mimeType.as_deref() == Some("application/ttml+xml"))
    );
}

#[tokio::test]
async fn dashif_subtitle_mpd_plays_main_video() {
    let server = FixtureServer::spawn("dashif_subtitle").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

/// TestCases/9a — trick-mode `EssentialProperty` parses; main video adaptation set plays.
#[test]
fn dashif_trick_play_mpd_parses() {
    let xml = read_fixture("dashif_trick_play", "manifest.mpd");
    let mpd = dash_mpd::parse(&xml).expect("parse mpd");
    assert_eq!(mpd.periods.len(), 1);
    assert_eq!(mpd.periods[0].adaptations.len(), 2);
    let trick = mpd.periods[0]
        .adaptations
        .iter()
        .find(|aset| {
            aset.essential_property
                .iter()
                .any(|p| p.schemeIdUri.contains("trickmode"))
        })
        .expect("trick-mode adaptation set");
    assert_eq!(trick.representations[0].maxPlayoutRate, Some(24.0));
    let ladder = dashplay::quality_ladder_from_adaptation_set(trick);
    assert_eq!(ladder.len(), 1);
    assert_eq!(ladder[0].max_playout_rate, Some(24.0));
    assert_eq!(ladder[0].coding_dependency, None);
}

#[tokio::test]
async fn dashif_trick_play_plays_main_video_track() {
    let server = FixtureServer::spawn("dashif_trick_play").await;
    let events = play_single_track(&server.manifest_url, VOD_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

#[tokio::test]
async fn dashif_trick_play_track_delivers_when_enabled() {
    let server = FixtureServer::spawn("dashif_trick_play").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(
            dashplay::TrackSelection::default()
                .with_video(dashplay::TrackPreference::default().max_tracks(0))
                .with_trick_play(dashplay::TrackPreference::default().max_tracks(1)),
        );
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(
        outputs.tracks[0].info().kind,
        dashplay::TrackKind::TrickPlay
    );
    let mut rx = outputs.tracks.into_iter().next().unwrap().into_receiver();
    let events = common::collect_events(&mut rx, VOD_TIMEOUT).await;
    outputs.join.await.unwrap().expect("join");

    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

#[test]
fn dashif_local_fixture_mpds_parse() {
    for fixture in PARSE_FIXTURES {
        let xml = read_fixture(fixture, "manifest.mpd");
        let mpd = dash_mpd::parse(&xml).unwrap_or_else(|e| panic!("parse {fixture}: {e}"));
        assert!(
            !mpd.periods.is_empty(),
            "{fixture}: expected at least one Period"
        );
    }
}

#[test]
fn dashif_iop_schematron_local_fixtures() {
    for fixture in IOP_FIXTURES {
        let xml = read_fixture(fixture, "manifest.mpd");
        validate_iop(&xml).unwrap_or_else(|violations| {
            panic!("IOP schematron failed for {fixture}: {violations:#?}")
        });
    }
}

/// Encrypted CENC vector — MPD and media layout conform to DASH-IF IOP.
#[test]
fn dashif_encrypted_vector_mpd_parses_and_validates_iop() {
    let xml = read_fixture("dashif_drm_encrypted", "manifest.mpd");
    validate_iop(&xml).expect("IOP validation");
    let info = parse_mpd_drm_info(&xml).expect("drm parse");
    assert!(
        !info.periods[0].adaptation_sets[0]
            .effective
            .widevine_pssh
            .is_empty()
    );
    assert_eq!(
        info.periods[0].adaptation_sets[0]
            .effective
            .protection_schemes,
        vec![dashplay::drm::CommonEncryptionScheme::Cenc]
    );
}

/// Full Widevine decrypt playback when `DEVICE_PATH` and a captured `license-response.bin` exist.
#[tokio::test]
async fn dashif_widevine_encrypted_playback_local() {
    let device_path = match std::env::var("DEVICE_PATH") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let _ = device_path;

    let license_path = common::fixture_dir("dashif_drm_encrypted").join("license-response.bin");
    if !license_path.exists() {
        return;
    }
    let license_bytes = std::fs::read(&license_path).expect("read license-response.bin");

    let server =
        spawn_drm_fixture_with_mock_license("dashif_drm_encrypted", license_bytes.clone()).await;
    let fetcher = static_license_fetcher(license_bytes);
    let events = play_single_track_with_license_fetcher(&server.manifest_url, VOD_TIMEOUT, fetcher)
        .await
        .expect("encrypted playback");

    assert!(init_payload(&events).is_some(), "expected decrypted init");
    assert!(
        !segment_payloads(&events).is_empty(),
        "expected decrypted segment(s)"
    );
    assert!(has_end(&events));
}

/// Axinom Widevine test vector — requires network, CDM device, and license server credentials.
#[tokio::test]
#[ignore = "requires DEVICE_PATH, WV_LICENSE_URL, and network"]
async fn dashif_remote_widevine_encrypted_playback() {
    let _device = std::env::var("DEVICE_PATH").expect("DEVICE_PATH");
    let license_url = std::env::var("WV_LICENSE_URL").expect("WV_LICENSE_URL");
    let manifest_url = std::env::var("DASHIF_DRM_MANIFEST_URL").unwrap_or_else(|_| {
        "https://media.axprod.net/TestVectors/v7-MultiDRM-SingleKey/Manifest_1080p.mpd".to_string()
    });

    let player = dashplay::Player::new(&manifest_url, Some(&license_url)).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let buffer_feedback = outputs.buffer_feedback(0).expect("track");
    let drain = common::spawn_playback_buffer_simulation(buffer_feedback, 25.0);
    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("track")
        .into_receiver();
    let events = common::collect_events(&mut rx, VOD_TIMEOUT).await;
    drain.abort();
    outputs.join.await.unwrap().expect("join");
    assert!(init_payload(&events).is_some());
    assert!(!segment_payloads(&events).is_empty());
}

/// Fetch published DASH-IF vectors and verify manifest parse (`cargo test --ignored`).
#[tokio::test]
#[ignore = "requires network; run with `cargo test --test dashif -- --ignored`"]
async fn dashif_remote_vectors_parse() {
    let client = reqwest::Client::builder()
        .timeout(VOD_TIMEOUT)
        .build()
        .expect("http client");

    for (name, url) in REMOTE_MPD_URLS {
        let body = client
            .get(*url)
            .send()
            .await
            .unwrap_or_else(|e| panic!("fetch {name}: {e}"))
            .error_for_status()
            .unwrap_or_else(|e| panic!("HTTP {name}: {e}"))
            .text()
            .await
            .unwrap_or_else(|e| panic!("body {name}: {e}"));

        let mpd = dash_mpd::parse(&body).unwrap_or_else(|e| panic!("parse {name}: {e}"));
        assert!(
            !mpd.periods.is_empty(),
            "{name}: remote MPD must contain at least one Period"
        );
    }
}
