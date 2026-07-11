mod common;

use common::{
    AdvancingLiveServer, FixtureServer, has_end, init_payload, play_single_track_live,
    segment_numbers, segment_payloads,
};

const LIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(800);
const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[tokio::test]
async fn live_duration_template_emits_init_and_segments_without_end() {
    let server = FixtureServer::spawn("live_duration").await;
    let events = play_single_track_live(&server.manifest_url, LIVE_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    let segments = segment_payloads(&events);
    assert!(
        segments.len() >= 2,
        "expected live segments, got {segments:?}"
    );
    assert_eq!(
        &segments[..2],
        [
            b"dashplay-live-seg-5".as_ref(),
            b"dashplay-live-seg-6".as_ref(),
        ]
    );
    assert!(
        !has_end(&events),
        "live stream should not emit End while active"
    );
}

#[tokio::test]
async fn live_segment_timeline_respects_time_shift_buffer() {
    let server = FixtureServer::spawn("live_timeline").await;
    let events = play_single_track_live(&server.manifest_url, LIVE_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    let segments = segment_payloads(&events);
    assert!(
        segments.len() >= 2,
        "expected live segments, got {segments:?}"
    );
    assert_eq!(
        &segments[..2],
        [
            b"dashplay-live-timeline-seg-5".as_ref(),
            b"dashplay-live-timeline-seg-6".as_ref(),
        ]
    );
    let numbers = segment_numbers(&events);
    assert!(numbers.len() >= 2);
    assert_eq!(&numbers[..2], [5, 6]);
    assert!(!has_end(&events));
}

#[tokio::test]
async fn live_manifest_refresh_advances_playback_window() {
    let server = AdvancingLiveServer::spawn().await;
    let events = play_single_track_live(&server.manifest_url, REFRESH_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert!(
        segment_payloads(&events).len() >= 2,
        "expected segments across manifest refreshes, got {:?}",
        segment_payloads(&events)
    );
    let numbers = segment_numbers(&events);
    assert!(
        numbers.iter().any(|&n| n >= 4),
        "expected live edge segments after refresh, got {numbers:?}"
    );
    assert!(!has_end(&events));
}
