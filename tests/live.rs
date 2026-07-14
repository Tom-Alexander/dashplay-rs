mod common;

use common::{
    AdvancingLiveServer, FixtureServer, InbandProducerReferenceLiveServer, LatencyLiveServer,
    PartialLiveServer, ProducerReferenceLiveServer, assert_no_duplicate_segments, has_end,
    init_payload, init_payloads, partial_segment_payloads, play_single_track_live,
    playback_rate_suggestions, segment_numbers, segment_payloads,
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

#[tokio::test]
async fn live_manifest_refresh_does_not_reemit_segments() {
    let server = AdvancingLiveServer::spawn().await;
    let events = play_single_track_live(&server.manifest_url, REFRESH_TIMEOUT)
        .await
        .expect("playback");

    assert!(
        segment_payloads(&events).len() >= 2,
        "expected segments across manifest refreshes, got {:?}",
        segment_payloads(&events)
    );
    assert_no_duplicate_segments(&events);
    let numbers = segment_numbers(&events);
    assert!(
        numbers.iter().any(|&n| n >= 4),
        "expected live edge segments after refresh, got {numbers:?}"
    );
}

#[tokio::test]
async fn live_multi_period_transition_re_emits_init() {
    let server = common::MultiPeriodLiveServer::spawn().await;
    let events = play_single_track_live(&server.manifest_url, std::time::Duration::from_secs(2))
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert!(
        inits.len() >= 2,
        "expected init re-emission on period change, got {inits:?}"
    );
    assert_eq!(inits[0], b"dashplay-period1-init".to_vec());
    assert!(
        inits.iter().any(|init| init == b"dashplay-period2-init"),
        "expected period-2 init after transition, got {inits:?}"
    );

    let segments = segment_payloads(&events);
    assert!(
        segments
            .iter()
            .any(|seg| seg.starts_with(b"dashplay-period1-seg-")),
        "expected period-1 segments"
    );
    assert!(
        segments
            .iter()
            .any(|seg| seg.starts_with(b"dashplay-period2-seg-")),
        "expected period-2 segments after manifest refresh, got {segments:?}"
    );
    assert!(!has_end(&events));
}

#[tokio::test]
async fn live_producer_reference_time_overrides_utc_timing_for_window() {
    let server = ProducerReferenceLiveServer::spawn().await;
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
    // UTCTiming alone (20s since AST) would start at seg-5/seg-6; PRT anchor (4s media) starts earlier.
    assert_eq!(
        &segments[..2],
        [
            b"dashplay-live-seg-1".as_ref(),
            b"dashplay-live-seg-2".as_ref(),
        ],
        "live window must follow ProducerReferenceTime, not UTCTiming"
    );
    let numbers = segment_numbers(&events);
    assert_eq!(&numbers[..2], [1, 2]);
    assert!(!has_end(&events));
}

#[tokio::test]
async fn live_inband_prft_producer_reference_time_selects_correct_window() {
    let server = InbandProducerReferenceLiveServer::spawn().await;
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
    assert!(
        segments[0].windows(4).any(|w| w == b"prft"),
        "expected in-band prft box in segment payload"
    );
    let numbers = segment_numbers(&events);
    assert_eq!(
        &numbers[..2],
        [1, 2],
        "live window must follow in-band prft / ProducerReferenceTime, not UTCTiming alone"
    );
    assert!(!has_end(&events));
}

#[tokio::test]
async fn live_partial_segment_transfer_emits_chunked_cmaf_fragments() {
    let server = PartialLiveServer::spawn().await;
    let events = play_single_track_live(&server.manifest_url, LIVE_TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );

    let partials = partial_segment_payloads(&events);
    assert!(
        partials.len() >= 4,
        "expected multiple partial chunks, got {partials:?}"
    );
    assert!(
        partials.iter().any(
            |(meta, payload)| meta.is_some_and(|p| p.index == 1 && !p.is_final)
                && payload.ends_with(b"partial-seg-5.m4s-a")
        ),
        "expected first chunk of seg-5, got {partials:?}"
    );
    assert!(
        partials
            .iter()
            .any(|(meta, payload)| meta.is_some_and(|p| p.is_final)
                && payload.ends_with(b"partial-seg-5.m4s-b")),
        "expected final chunk of seg-5, got {partials:?}"
    );
    assert!(!has_end(&events));
}

#[tokio::test]
async fn live_service_description_latency_suggests_catch_up_rate() {
    let server = LatencyLiveServer::spawn().await;
    let events = play_single_track_live(&server.manifest_url, LIVE_TIMEOUT)
        .await
        .expect("playback");

    assert!(
        !segment_payloads(&events).is_empty(),
        "expected live segments, got {:?}",
        segment_payloads(&events)
    );
    let suggestions = playback_rate_suggestions(&events);
    assert!(
        !suggestions.is_empty(),
        "expected PlaybackRateSuggested events, got none (events={events:?})"
    );
    assert!(
        suggestions.iter().any(|(rate, _)| *rate > 1.0),
        "expected catch-up rate > 1.0, got {suggestions:?}"
    );
    assert!(
        suggestions
            .iter()
            .all(|(rate, _)| *rate >= 0.96 && *rate <= 1.04),
        "rates must stay within PlaybackRate bounds, got {suggestions:?}"
    );
}
