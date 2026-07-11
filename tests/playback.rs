mod common;

use common::{
    FixtureServer, has_end, init_payload, init_payloads, play_all_tracks, play_single_track,
    segment_payloads, trim_payload,
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
async fn vod_segment_base_playback() {
    let server = FixtureServer::spawn("vod_segment_base").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"SEGMENT-1!!".to_vec(), b"SEGMENT-2!!".to_vec()]
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
async fn track_preferences_limit_outputs_and_expose_metadata() {
    let server = FixtureServer::spawn("vod_av").await;
    let selection = dashplayrs::TrackSelection::default()
        .with_video(dashplayrs::TrackPreference::default().max_tracks(0))
        .with_audio(
            dashplayrs::TrackPreference::default()
                .codec("mp4a")
                .max_tracks(1),
        );
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(selection);
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 1);
    assert_eq!(outputs.tracks[0].info.kind, dashplayrs::TrackKind::Audio);
    assert_eq!(outputs.tracks[0].info.codecs, vec!["mp4a.40.2"]);

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("audio")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.expect("join").expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-audio-init".as_ref())
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_multi_period_emits_inits_and_segments_in_order() {
    let server = FixtureServer::spawn("vod_multi_period").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert_eq!(inits.len(), 2, "expected init per period, got {inits:?}");
    assert_eq!(inits[0], b"dashplay-period1-init".to_vec());
    assert_eq!(inits[1], b"dashplay-period2-init".to_vec());

    let segments = segment_payloads(&events);
    assert_eq!(
        segments,
        vec![
            b"dashplay-period1-seg-1".to_vec(),
            b"dashplay-period1-seg-2".to_vec(),
            b"dashplay-period2-seg-1".to_vec(),
            b"dashplay-period2-seg-2".to_vec(),
        ]
    );
    assert!(has_end(&events));
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

#[tokio::test]
async fn vod_merged_stream_emits_fragments() {
    use futures_util::StreamExt;

    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None).expect("player");
    let mut merged = player.start_merged().await.expect("start merged");

    let mut chunks = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), merged.stream.next())
            .await
        {
            Ok(Some(Ok(bytes))) => chunks.push(bytes.to_vec()),
            Ok(Some(Err(e))) => panic!("stream error: {e}"),
            Ok(None) => break,
            Err(_) if !chunks.is_empty() => break,
            Err(_) => continue,
        }
    }

    merged.join.await.unwrap().expect("join");

    let chunks: Vec<_> = chunks.iter().map(|c| trim_payload(c)).collect();
    assert_eq!(chunks.len(), 3, "expected init + 2 segments");
    assert_eq!(chunks[0], b"dashplay-init-v1".to_vec());
    assert_eq!(chunks[1], b"dashplay-seg-1".to_vec());
    assert_eq!(chunks[2], b"dashplay-seg-2".to_vec());
}

#[tokio::test]
async fn vod_merged_async_read_pipes_bytes() {
    use tokio::io::AsyncReadExt;

    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None).expect("player");
    let merged = player.start_merged().await.expect("start merged");
    let mut async_read = merged.into_async_read();

    let mut buf = Vec::new();
    async_read
        .reader
        .read_to_end(&mut buf)
        .await
        .expect("read merged output");

    async_read.join.await.unwrap().expect("join");

    let expected = [
        b"dashplay-init-v1\n".as_slice(),
        b"dashplay-seg-1\n".as_slice(),
        b"dashplay-seg-2\n".as_slice(),
    ]
    .concat();
    assert_eq!(buf, expected);
}

#[tokio::test]
async fn track_subscription_helpers_expose_receivers() {
    use futures_util::StreamExt;

    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 1);

    let mut subscribe_rx = outputs.subscribe(0).expect("track");
    let first = tokio::time::timeout(TIMEOUT, subscribe_rx.recv())
        .await
        .expect("init from subscribe")
        .expect("event");
    assert!(
        matches!(first, dashplayrs::PlayerEvent::Init(_)),
        "subscribe should receive init"
    );

    let track_out = outputs.tracks.first().expect("one track output");
    let next_from_stream = tokio::time::timeout(TIMEOUT, track_out.events().next())
        .await
        .expect("segment from events stream")
        .expect("stream item")
        .expect("event");
    assert!(
        matches!(next_from_stream, dashplayrs::PlayerEvent::Segment { .. }),
        "events() should receive segments after init"
    );

    if let Some(feedback) = outputs.buffer_feedback(0) {
        let _ = feedback.report(25.0);
    }
    while tokio::time::timeout(std::time::Duration::from_millis(500), subscribe_rx.recv())
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|ev| !matches!(ev, dashplayrs::PlayerEvent::End))
    {}

    outputs.join.await.unwrap().expect("join");
}
