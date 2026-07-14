mod common;

use std::time::Duration;

use common::{FixtureServer, init_payload, recv_matching, segment_payloads};
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
        recv_matching(&mut rx, TIMEOUT, |ev| matches!(ev, PlayerEvent::Init(_))).await,
        Some(PlayerEvent::Init(_))
    ));
    assert!(matches!(
        recv_matching(&mut rx, TIMEOUT, |ev| matches!(
            ev,
            PlayerEvent::Segment { .. }
        ))
        .await,
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
        recv_matching(&mut rx, TIMEOUT, |ev| matches!(ev, PlayerEvent::Init(_))).await,
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
        recv_matching(&mut rx, TIMEOUT, |ev| matches!(ev, PlayerEvent::Init(_))).await,
        Some(PlayerEvent::Init(_))
    ));
    assert_eq!(
        segment_payloads(&[recv_matching(&mut rx, TIMEOUT, |ev| matches!(
            ev,
            PlayerEvent::Segment { .. }
        ))
        .await
        .expect("segment")]),
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
async fn presentation_time_tracks_delivery_and_seek() {
    let server = FixtureServer::spawn("vod_time").await;
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let mut rx = outputs.subscribe(0).expect("one track");

    assert!(matches!(
        recv_matching(&mut rx, TIMEOUT, |ev| matches!(ev, PlayerEvent::Init(_))).await,
        Some(PlayerEvent::Init(_))
    ));
    assert!(matches!(
        recv_matching(&mut rx, TIMEOUT, |ev| matches!(
            ev,
            PlayerEvent::Segment { .. }
        ))
        .await,
        Some(PlayerEvent::Segment { .. })
    ));
    assert_eq!(
        outputs.presentation_time(),
        Some(Duration::from_secs(0)),
        "first segment starts at t=0"
    );

    outputs.seek(Duration::from_secs(5)).expect("seek");
    assert_eq!(
        outputs.presentation_time(),
        Some(Duration::from_secs(5)),
        "seek target is exposed immediately"
    );

    let mut saw_playhead_event = false;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if let Some(ev) = recv_event(&mut rx, Duration::from_millis(500)).await {
            if matches!(
                ev,
                PlayerEvent::PlayheadUpdated {
                    presentation_time: Some(t),
                } if t == Duration::from_secs(4)
            ) {
                saw_playhead_event = true;
            }
            if matches!(ev, PlayerEvent::End) {
                break;
            }
        }
    }
    assert_eq!(
        outputs.presentation_time(),
        Some(Duration::from_secs(4)),
        "after SAP-aligned delivery, playhead follows segment start"
    );
    assert!(
        saw_playhead_event,
        "expected PlayheadUpdated when delivery realigns playhead"
    );

    outputs.join.await.unwrap().expect("join");
}

#[tokio::test]
async fn set_track_selection_switches_audio_language() {
    let server = FixtureServer::spawn("vod_multi_audio").await;
    let selection = dashplayrs::TrackSelection::default().with_audio(
        dashplayrs::TrackPreference::default()
            .language("en")
            .max_tracks(1),
    );
    let player = dashplayrs::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(selection);
    let outputs = player.start_tracks().await.expect("start");
    assert_eq!(outputs.track_count(), 2);

    let audio_idx = outputs
        .tracks
        .iter()
        .position(|t| t.info().kind == dashplayrs::TrackKind::Audio)
        .expect("audio track");
    let mut rx = outputs.subscribe(audio_idx).expect("audio rx");

    assert_eq!(
        outputs.tracks[audio_idx].info().language.as_deref(),
        Some("en")
    );
    assert!(matches!(
        recv_matching(&mut rx, TIMEOUT, |ev| matches!(ev, PlayerEvent::Init(_))).await,
        Some(PlayerEvent::Init(_))
    ));
    assert_eq!(
        segment_payloads(&[recv_matching(&mut rx, TIMEOUT, |ev| matches!(
            ev,
            PlayerEvent::Segment { .. }
        ))
        .await
        .expect("en segment")]),
        vec![b"dashplay-audio-en-1".to_vec()]
    );

    let switched = dashplayrs::TrackSelection::default().with_audio(
        dashplayrs::TrackPreference::default()
            .language("fr")
            .max_tracks(1),
    );
    outputs.set_track_selection(switched).expect("switch audio");

    let mut saw_track_changed = false;
    let mut fr_init = false;
    let mut fr_segments = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match recv_event(&mut rx, Duration::from_millis(500)).await {
            Some(PlayerEvent::TrackChanged { info }) => {
                assert_eq!(info.language.as_deref(), Some("fr"));
                saw_track_changed = true;
            }
            Some(PlayerEvent::Init(data)) => {
                assert_eq!(&data[..], b"dashplay-audio-fr-init");
                fr_init = true;
            }
            Some(PlayerEvent::Segment { data, .. }) => fr_segments.push(data.to_vec()),
            Some(PlayerEvent::End) => break,
            _ => {}
        }
        if fr_init && !fr_segments.is_empty() && saw_track_changed {
            break;
        }
    }

    assert!(saw_track_changed, "expected TrackChanged for French audio");
    assert!(fr_init, "expected French init after switch");
    assert!(
        fr_segments
            .iter()
            .any(|s| s.as_slice() == b"dashplay-audio-fr-1"
                || s.as_slice() == b"dashplay-audio-fr-2"),
        "expected French media after switch, got {fr_segments:?}"
    );
    assert_eq!(
        outputs.tracks[audio_idx].info().language.as_deref(),
        Some("fr")
    );

    outputs.stop().expect("stop");
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
