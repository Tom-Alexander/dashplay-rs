//! Integration tests for MPD and in-band media events.

mod common;

use common::{FixtureServer, play_single_track, recv_matching};
use dashplay::{MediaEventSource, PlayerEvent};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn media_events(events: &[PlayerEvent]) -> Vec<&dashplay::MediaEvent> {
    events
        .iter()
        .filter_map(|ev| match ev {
            PlayerEvent::MediaEvent(event) => Some(event),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn mpd_and_inband_media_events_are_emitted() {
    let server = FixtureServer::spawn("events_vod").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    let media = media_events(&events);
    assert!(
        media
            .iter()
            .any(|e| matches!(e.source, MediaEventSource::Mpd) && e.is_scte35()),
        "expected MPD SCTE-35 event, got {media:?}"
    );
    assert!(
        media.iter().any(|e| {
            matches!(
                e.source,
                MediaEventSource::InBand {
                    segment_number: 1,
                    ..
                }
            ) && e.message_data.as_ref() == b"inband-event"
        }),
        "expected in-band emsg event on segment 1, got {media:?}"
    );
    assert!(common::has_end(&events));
}

#[tokio::test]
async fn mpd_media_event_arrives_before_first_segment() {
    let server = FixtureServer::spawn("events_vod").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let mut rx = outputs.subscribe(0).expect("track");

    let mpd_event = recv_matching(&mut rx, TIMEOUT, |ev| {
        matches!(
            ev,
            PlayerEvent::MediaEvent(dashplay::MediaEvent {
                source: MediaEventSource::Mpd,
                ..
            })
        )
    })
    .await
    .expect("mpd media event");
    assert!(matches!(mpd_event, PlayerEvent::MediaEvent(e) if e.is_scte35()));

    let _ = recv_matching(&mut rx, TIMEOUT, |ev| matches!(ev, PlayerEvent::Init(_)))
        .await
        .expect("init");

    outputs.join.await.unwrap().expect("join");
}
