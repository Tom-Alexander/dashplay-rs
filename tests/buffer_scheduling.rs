mod common;

use common::{FixtureServer, recv_matching};
use dashplayrs::{PlaybackState, Player, PlayerEvent};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

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

#[tokio::test]
async fn parallel_segment_prefetch_downloads_concurrently() {
    let (server, peak) = FixtureServer::spawn_with_concurrency_probe(
        "dashif_simple",
        &["/V300/"],
        std::time::Duration::from_millis(250),
    )
    .await;
    let player = Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let buffer_feedback = outputs.buffer_feedback(0).expect("track");
    // Keep buffer below high-water so later segments may be prefetched together.
    buffer_feedback.report(5.0).expect("buffer report");

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("track")
        .into_receiver();

    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut segments = 0usize;
    while tokio::time::Instant::now() < deadline && segments < 4 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(PlayerEvent::Segment { .. })) => segments += 1,
            Ok(Ok(PlayerEvent::End)) => break,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert!(
        segments >= 3,
        "expected several media segments, got {segments}"
    );
    assert!(
        peak.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "expected concurrent media segment downloads, peak={}",
        peak.load(std::sync::atomic::Ordering::SeqCst)
    );

    drop(rx);
    outputs.join.await.unwrap().expect("join");
}

#[tokio::test]
async fn automatic_buffer_estimate_emits_buffer_updated_without_report() {
    let server = FixtureServer::spawn("vod_single").await;
    let player = Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let metrics = outputs.metrics(0).expect("metrics");
    let mut rx = outputs.subscribe(0).expect("track");

    let _ = recv_matching(&mut rx, TIMEOUT, |ev| {
        matches!(ev, PlayerEvent::Segment { .. })
    })
    .await
    .expect("first segment");

    let buffer_updated = recv_matching(
        &mut rx,
        TIMEOUT,
        |ev| matches!(ev, PlayerEvent::BufferUpdated { buffer_s } if *buffer_s > 0.0),
    )
    .await
    .expect("automatic BufferUpdated");
    assert!(
        matches!(
            buffer_updated,
            PlayerEvent::BufferUpdated { buffer_s } if buffer_s > 0.0
        ),
        "expected positive automatic buffer estimate"
    );

    let snap = metrics.snapshot();
    assert!(
        snap.buffer_s > 0.0,
        "metrics should reflect automatic buffer estimate, got {}",
        snap.buffer_s
    );

    drop(rx);
    outputs.join.await.unwrap().expect("join");
}

#[tokio::test]
async fn automatic_rebuffer_when_media_clock_underruns() {
    // Delay the second segment so the 4s first segment drains before it arrives.
    let (server, _peak) = FixtureServer::spawn_with_concurrency_probe(
        "vod_single",
        &["/seg-2"],
        std::time::Duration::from_secs(6),
    )
    .await;
    let player = Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let metrics = outputs.metrics(0).expect("metrics");
    let mut state_rx = outputs.subscribe_playback_state();
    let mut rx = outputs.subscribe(0).expect("track");

    let _ = recv_matching(&mut rx, TIMEOUT, |ev| {
        matches!(ev, PlayerEvent::Segment { .. })
    })
    .await
    .expect("first segment");

    let deadline = tokio::time::Instant::now() + TIMEOUT;
    let mut saw_buffering = false;
    while tokio::time::Instant::now() < deadline {
        if *state_rx.borrow() == PlaybackState::Buffering
            && !metrics.snapshot().rebuffer_events.is_empty()
        {
            saw_buffering = true;
            break;
        }
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(200), state_rx.changed()).await;
    }
    assert!(
        saw_buffering,
        "expected PlaybackState::Buffering and a rebuffer metric without BufferFeedback::report; \
         state={:?} rebuffers={}",
        outputs.playback_state(),
        metrics.snapshot().rebuffer_events.len()
    );

    drop(rx);
    let _ = outputs.stop();
    let _ = outputs.join.await;
}

#[tokio::test]
async fn pause_freezes_automatic_buffer_drain() {
    let server = FixtureServer::spawn("vod_single").await;
    let player = Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let metrics = outputs.metrics(0).expect("metrics");
    let mut rx = outputs.subscribe(0).expect("track");

    let _ = recv_matching(&mut rx, TIMEOUT, |ev| {
        matches!(ev, PlayerEvent::Segment { .. })
    })
    .await
    .expect("first segment");
    // Let the media clock advance briefly so buffer is below the delivered end.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let before = metrics.snapshot().buffer_s;
    assert!(before > 0.0, "expected automatic buffer before pause");

    outputs.pause().expect("pause");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let during_pause = metrics.snapshot().buffer_s;
    assert!(
        (during_pause - before).abs() < 0.75,
        "pause should freeze media-clock drain: before={before} during={during_pause}"
    );

    outputs.resume().expect("resume");
    drop(rx);
    let _ = outputs.stop();
    let _ = outputs.join.await;
}

#[tokio::test]
async fn buffer_feedback_report_overrides_automatic_estimate() {
    let server = FixtureServer::spawn("vod_single").await;
    let player = Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let metrics = outputs.metrics(0).expect("metrics");
    let buffer_feedback = outputs.buffer_feedback(0).expect("track");
    let mut rx = outputs.subscribe(0).expect("track");

    let _ = recv_matching(&mut rx, TIMEOUT, |ev| {
        matches!(ev, PlayerEvent::Segment { .. })
    })
    .await
    .expect("first segment");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(metrics.snapshot().buffer_s > 0.0);

    buffer_feedback.report(1.25).expect("override report");
    assert!(
        (metrics.snapshot().buffer_s - 1.25).abs() < 1e-6,
        "report should snap metrics buffer to the consumer value"
    );

    drop(rx);
    let _ = outputs.stop();
    let _ = outputs.join.await;
}
