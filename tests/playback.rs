mod common;

use common::{
    FixtureServer, has_end, init_payload, play_all_tracks, play_single_track, segment_payloads,
};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn vod_single_track_emits_init_segments_and_end() {
    let server = FixtureServer::spawn("vod_single").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
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

#[tokio::test]
async fn vod_segment_list_playback() {
    let server = FixtureServer::spawn("vod_segment_list").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
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

#[tokio::test]
async fn vod_segment_timeline_playback() {
    let server = FixtureServer::spawn("vod_timeline").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-timeline-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-timeline-seg-1".to_vec(),
            b"dashplay-timeline-seg-2".to_vec()
        ]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_audio_video_parallel_tracks() {
    let server = FixtureServer::spawn("vod_av").await;
    let tracks = play_all_tracks(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(tracks.len(), 2);

    let video = &tracks[0];
    assert_eq!(
        init_payload(video).as_deref(),
        Some(b"dashplay-video-init".as_ref())
    );
    assert_eq!(
        segment_payloads(video),
        vec![
            b"dashplay-video-seg-1".to_vec(),
            b"dashplay-video-seg-2".to_vec()
        ]
    );
    assert!(has_end(video));

    let audio = &tracks[1];
    assert_eq!(
        init_payload(audio).as_deref(),
        Some(b"dashplay-audio-init".as_ref())
    );
    assert_eq!(
        segment_payloads(audio),
        vec![
            b"dashplay-audio-seg-1".to_vec(),
            b"dashplay-audio-seg-2".to_vec()
        ]
    );
    assert!(has_end(audio));
}

#[tokio::test]
async fn vod_period_level_segment_template_inheritance() {
    let server = FixtureServer::spawn("vod_period_template").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
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

#[tokio::test]
async fn base_url_failover_uses_secondary_host() {
    let server = FixtureServer::spawn_with_options("base_url_failover", &["/bad"]).await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-failover-init".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

#[tokio::test]
async fn missing_segment_surfaces_request_error() {
    let server = FixtureServer::spawn("vod_missing_segment").await;

    let err = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect_err("expected segment fetch failure");

    assert!(
        matches!(
            err,
            dashplayrs::PlayerError::SegmentRequestFailed { status: 404, .. }
        ),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn player_rejects_invalid_manifest_url() {
    let err = dashplayrs::Player::new("not-a-valid-url", None)
        .err()
        .expect("invalid url");
    assert!(matches!(err, dashplayrs::PlayerError::Url(_)));
}

#[tokio::test]
async fn vod_time_template_addressing_playback() {
    let server = FixtureServer::spawn("vod_time").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
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

#[tokio::test]
async fn dashif_simple_segment_template_smoke_test() {
    // Structure adapted from DASH-IF livesim2 testpic_2s Manifest_endNumber.mpd.
    let server = FixtureServer::spawn("dashif_simple").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-dashif-init".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 4);
    assert!(has_end(&events));
}

#[tokio::test]
async fn all_base_urls_fail_surfaces_segment_error() {
    let server = FixtureServer::spawn_with_options("base_url_all_bad", &["/a", "/b"]).await;

    let err = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect_err("expected all CDN bases to fail");

    assert!(
        matches!(
            err,
            dashplayrs::PlayerError::SegmentRequestFailed { status: 404, .. }
        ),
        "unexpected error: {err:?}"
    );
}
