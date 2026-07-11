mod common;

use common::{
    FixtureServer, has_end, init_payload, init_payloads, play_single_track, segment_payloads,
};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const ABR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[tokio::test]
async fn abr_starts_at_high_representation_with_full_buffer() {
    let server = FixtureServer::spawn("vod_abr").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-abr-high-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-abr-high-seg-1".to_vec(),
            b"dashplay-abr-high-seg-2".to_vec(),
        ]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn representation_fallback_uses_lower_rep_when_higher_segment_missing() {
    let server = FixtureServer::spawn("vod_rep_fallback").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-abr-high-init".as_ref())
    );
    let inits = init_payloads(&events);
    assert!(
        inits.iter().any(|init| init == b"dashplay-abr-low-init"),
        "expected low-rep init after segment fallback, got {inits:?}"
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

#[tokio::test]
async fn abr_downgrades_and_re_emits_init_when_buffer_drains() {
    let server = FixtureServer::spawn_with_delays("vod_abr", &["/high"]).await;
    let events = play_single_track(&server.manifest_url, ABR_TIMEOUT)
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert!(
        inits.len() >= 2,
        "expected init re-emission after rep switch, got {inits:?}"
    );
    assert_eq!(inits[0], b"dashplay-abr-high-init".to_vec());
    assert!(
        inits.iter().any(|init| init == b"dashplay-abr-low-init"),
        "expected low-rep init after downgrade, got {inits:?}"
    );

    let segments = segment_payloads(&events);
    assert!(
        segments
            .iter()
            .any(|seg| seg.starts_with(b"dashplay-abr-high-seg-")),
        "expected high-rep segments before downgrade"
    );
    assert!(
        segments
            .iter()
            .any(|seg| seg.starts_with(b"dashplay-abr-low-seg-")),
        "expected low-rep segments after downgrade, got {segments:?}"
    );
    assert!(has_end(&events));
}
