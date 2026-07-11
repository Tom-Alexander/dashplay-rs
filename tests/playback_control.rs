mod common;

use std::time::Duration;

use common::{FixtureServer, init_payload, segment_payloads};
use dashplayrs::{PlaybackState, PlayerEvent};

const TIMEOUT: Duration = Duration::from_secs(10);

async fn recv_event(
    rx: &mut tokio::sync::broadcast::Receiver<PlayerEvent>,
    timeout: Duration,
) -> Option<PlayerEvent> {
    tokio::time::timeout(timeout, rx.recv())
        .await
        .ok()
        .and_then(Result::ok)
}

#[tokio::test]
async fn stop_halts_segment_delivery() {
    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let mut rx = outputs.subscribe(0).expect("one track");

    assert!(matches!(
        recv_event(&mut rx, TIMEOUT).await,
        Some(PlayerEvent::Init(_))
    ));
    assert!(matches!(
        recv_event(&mut rx, TIMEOUT).await,
        Some(PlayerEvent::Segment { .. })
    ));

    outputs.stop().expect("stop");
    assert_eq!(outputs.playback_state(), PlaybackState::Ended);

    let mut saw_end = false;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Some(PlayerEvent::End) = recv_event(&mut rx, Duration::from_millis(500)).await {
            saw_end = true;
            break;
        }
    }
    assert!(saw_end, "expected End after stop");

    outputs.join.await.unwrap().expect("join");
}

#[tokio::test]
async fn pause_and_resume_delay_delivery() {
    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    outputs.pause().expect("pause");
    let mut rx = outputs.subscribe(0).expect("one track");

    assert!(matches!(
        recv_event(&mut rx, TIMEOUT).await,
        Some(PlayerEvent::Init(_))
    ));
    assert_eq!(outputs.playback_state(), PlaybackState::Paused);

    assert!(
        recv_event(&mut rx, Duration::from_millis(300))
            .await
            .is_none(),
        "no segments should arrive while paused"
    );

    outputs.resume().expect("resume");

    let mut segments = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match recv_event(&mut rx, Duration::from_millis(500)).await {
            Some(PlayerEvent::Segment { data, .. }) => segments.push(data),
            Some(PlayerEvent::End) => break,
            _ => {}
        }
    }
    assert_eq!(segments.len(), 2, "expected both VOD segments after resume");

    outputs.join.await.unwrap().expect("join");
}

#[tokio::test]
async fn seek_repositions_to_later_segment() {
    let server = FixtureServer::spawn("vod_time").await;
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let mut rx = outputs.subscribe(0).expect("one track");

    assert!(matches!(
        recv_event(&mut rx, TIMEOUT).await,
        Some(PlayerEvent::Init(_))
    ));
    assert_eq!(
        segment_payloads(&[recv_event(&mut rx, TIMEOUT).await.expect("segment")]),
        vec![b"dashplay-time-0".to_vec()]
    );

    outputs.seek(Duration::from_secs(5)).expect("seek");

    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Some(ev) = recv_event(&mut rx, Duration::from_millis(500)).await {
            if matches!(ev, PlayerEvent::End) {
                events.push(ev);
                break;
            }
            events.push(ev);
        }
    }

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref()),
        "seek should re-emit init"
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-time-4000".to_vec()],
        "seek to 5s should deliver only the second segment"
    );

    outputs.join.await.unwrap().expect("join");
}

#[tokio::test]
async fn control_errors_when_stopped() {
    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    outputs.stop().expect("stop");
    assert!(outputs.pause().is_err());
    assert!(outputs.seek(Duration::from_secs(1)).is_err());
    outputs.join.await.unwrap().expect("join");
}
