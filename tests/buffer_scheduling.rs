mod common;

use common::FixtureServer;
use dashplayrs::{Player, PlayerEvent};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn buffer_target_throttles_second_segment_when_buffer_full() {
    let server = FixtureServer::spawn("vod_single").await;
    let player = Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let buffer_feedback = outputs.buffer_feedback(0).expect("track");
    buffer_feedback.report(30.0).expect("initial buffer report");

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("track")
        .into_receiver();

    let deadline = tokio::time::Instant::now() + TIMEOUT;

    // Init is not subject to media-segment buffer throttling.
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(PlayerEvent::Init(_))) => break,
            Ok(Ok(_)) => continue,
            _ => panic!("timed out waiting for init"),
        }
    }

    // First media segment is always scheduled (startup bootstrap).
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(PlayerEvent::Segment { .. })) => break,
            Ok(Ok(_)) => continue,
            _ => panic!("timed out waiting for first segment"),
        }
    }

    // With a full buffer, the second segment must not be prefetched yet.
    match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
        Ok(Ok(PlayerEvent::Segment { .. })) => {
            panic!("second segment arrived before buffer dropped below high-water mark")
        }
        Ok(Ok(_)) => {}
        Ok(Err(_)) | Err(_) => {}
    }

    buffer_feedback.report(5.0).expect("buffer drop report");

    let mut got_second_segment = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(PlayerEvent::Segment { .. })) => {
                got_second_segment = true;
                break;
            }
            Ok(Ok(PlayerEvent::End)) => break,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert!(
        got_second_segment,
        "expected second segment after buffer dropped below high-water mark"
    );

    drop(rx);
    outputs.join.await.unwrap().expect("join");
}

#[tokio::test]
async fn buffer_target_rebuffer_recovery_fetches_when_below_min_buffer_time() {
    let server = FixtureServer::spawn("vod_single").await;
    let player = Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let buffer_feedback = outputs.buffer_feedback(0).expect("track");
    buffer_feedback.report(30.0).expect("initial buffer report");

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("track")
        .into_receiver();

    let deadline = tokio::time::Instant::now() + TIMEOUT;

    // Skip init and first segment.
    for _ in 0..2 {
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(PlayerEvent::Init(_))) | Ok(Ok(PlayerEvent::Segment { .. })) => break,
                Ok(Ok(_)) => continue,
                _ => panic!("timed out waiting for startup events"),
            }
        }
    }

    // Rebuffer: below MPD minBufferTime (PT2S) must resume prefetch immediately.
    buffer_feedback.report(0.5).expect("rebuffer report");

    let mut got_second_segment = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(PlayerEvent::Segment { .. })) => {
                got_second_segment = true;
                break;
            }
            Ok(Ok(PlayerEvent::End)) => break,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert!(
        got_second_segment,
        "expected rebuffer recovery to fetch when buffer is below minBufferTime"
    );

    drop(rx);
    outputs.join.await.unwrap().expect("join");
}
