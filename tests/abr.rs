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

#[tokio::test]
async fn custom_abr_factory_selects_fixed_representation() -> Result<(), dashplayrs::PlayerError> {
    use dash_mpd::AdaptationSet;
    use dashplayrs::{
        AbrController, AbrDecision, AbrFactory, Player, QualityRung,
        quality_ladder_from_adaptation_set, shared_abr_factory,
    };

    struct FixedQualityAbrFactory {
        quality_index: usize,
    }

    struct FixedQualityAbrController {
        rungs: Vec<QualityRung>,
        quality_index: usize,
    }

    impl AbrFactory for FixedQualityAbrFactory {
        fn create(&self, adaptation_set: &AdaptationSet) -> Option<Box<dyn AbrController>> {
            let rungs = quality_ladder_from_adaptation_set(adaptation_set);
            if rungs.is_empty() {
                return None;
            }
            let quality_index = self.quality_index.min(rungs.len() - 1);
            Some(Box::new(FixedQualityAbrController {
                rungs,
                quality_index,
            }))
        }
    }

    impl AbrController for FixedQualityAbrController {
        fn update_buffer(&mut self, _buffer_s: f64) {}

        fn observe_segment_download(
            &mut self,
            _throughput_bps: f64,
            _downloaded_bytes: usize,
            _quality_index: usize,
        ) {
        }

        fn decide(&self) -> AbrDecision {
            AbrDecision {
                quality_index: self.quality_index,
                bitrate_bps: self.rungs[self.quality_index].bitrate_bps,
            }
        }

        fn representation_index_for_quality_index(&self, quality_index: usize) -> usize {
            self.rungs[quality_index].representation_index
        }

        fn bitrate_bps_for_quality_index(&self, quality_index: usize) -> f64 {
            self.rungs[quality_index].bitrate_bps
        }

        fn rung_count(&self) -> usize {
            self.rungs.len()
        }
    }

    let server = FixtureServer::spawn("vod_abr").await;
    let player = Player::new(server.manifest_url.as_str(), None)?.with_abr_factory(
        shared_abr_factory(FixedQualityAbrFactory { quality_index: 0 }),
    );
    let outputs = player.start_tracks().await?;
    let buffer_feedback = outputs.buffer_feedback(0).expect("one track");
    let _ = buffer_feedback.report(25.0);
    let _drain = common::spawn_playback_buffer_simulation(buffer_feedback, 25.0);
    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("one track")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.unwrap()?;

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-abr-low-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-abr-low-seg-1".to_vec(),
            b"dashplay-abr-low-seg-2".to_vec(),
        ]
    );
    assert!(has_end(&events));
    Ok(())
}
