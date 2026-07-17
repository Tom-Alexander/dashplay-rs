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
async fn quality_constraints_max_bitrate_selects_low_representation()
-> Result<(), dashplay::PlayerError> {
    use dashplay::{Player, QualityConstraints};

    let server = FixtureServer::spawn("vod_abr").await;
    let player = Player::new(server.manifest_url.as_str(), None)?
        .with_quality_constraints(QualityConstraints::default().max_bitrate_bps(200_000));
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

#[tokio::test]
async fn quality_constraints_data_saver_selects_lowest_representation()
-> Result<(), dashplay::PlayerError> {
    use dashplay::{Player, QualityConstraints};

    let server = FixtureServer::spawn("vod_abr").await;
    let player = Player::new(server.manifest_url.as_str(), None)?
        .with_quality_constraints(QualityConstraints::default().data_saver(true));
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
    assert!(has_end(&events));
    Ok(())
}

#[tokio::test]
async fn quality_constraints_fixed_quality_pins_representation() -> Result<(), dashplay::PlayerError>
{
    use dashplay::{Player, QualityConstraints};

    let server = FixtureServer::spawn("vod_abr").await;
    // Ladder is low(0) then high(1); pin to low even with a full buffer.
    let player = Player::new(server.manifest_url.as_str(), None)?
        .with_quality_constraints(QualityConstraints::default().fixed_quality(0));
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
async fn dropped_frames_cap_forces_quality_downswitch() -> Result<(), dashplay::PlayerError> {
    use dashplay::{PlaybackQualitySample, Player, PlayerEvent};
    use std::time::Duration as StdDuration;

    let server = FixtureServer::spawn("vod_abr").await;
    // Pin ABR to the high rung so only the dropped-frames rule can force a down-switch.
    let player = Player::new(server.manifest_url.as_str(), None)?
        .with_abr_factory(dashplay::shared_abr_factory(FixedHighAbrFactory));
    let outputs = player.start_tracks().await?;
    let buffer_feedback = outputs.buffer_feedback(0).expect("one track");
    let quality_feedback = outputs.playback_quality_feedback(0).expect("one track");
    // Hold buffer at/above the high-water mark so the second segment cannot schedule
    // until after we report dropped frames (avoids racing the planner).
    let hold = {
        let bf = buffer_feedback.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(StdDuration::from_millis(50));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if bf.report(30.0).is_err() {
                    break;
                }
            }
        })
    };
    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("one track")
        .into_receiver();

    let mut events = Vec::new();
    let mut saw_first_media = false;
    let mut _drain = None;
    let deadline = tokio::time::Instant::now() + ABR_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) => {
                let is_media = matches!(ev, PlayerEvent::Segment { .. });
                events.push(ev);
                if is_media && !saw_first_media {
                    saw_first_media = true;
                    let _ = quality_feedback.report(PlaybackQualitySample {
                        dropped_video_frames: 0,
                        total_video_frames: 0,
                    });
                    let _ = quality_feedback.report(PlaybackQualitySample {
                        dropped_video_frames: 100,
                        total_video_frames: 400,
                    });
                    hold.abort();
                    let _ = buffer_feedback.report(5.0);
                    _drain = Some(common::spawn_playback_buffer_simulation(
                        buffer_feedback.clone(),
                        5.0,
                    ));
                }
                if matches!(
                    events.last(),
                    Some(PlayerEvent::End | PlayerEvent::PlaybackEnded | PlayerEvent::Error(_))
                ) {
                    break;
                }
            }
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    hold.abort();
    outputs.join.await.unwrap()?;

    assert!(
        saw_first_media,
        "expected at least one media segment before reporting drops"
    );
    let segments = segment_payloads(&events);
    assert!(
        segments
            .iter()
            .any(|seg| seg.starts_with(b"dashplay-abr-high-seg-")),
        "expected high-rep segment before dropped-frames cap, got {segments:?}"
    );
    assert!(
        segments
            .iter()
            .any(|seg| seg.starts_with(b"dashplay-abr-low-seg-")),
        "expected low-rep segment after dropped-frames cap, got {segments:?}"
    );
    assert!(has_end(&events));
    Ok(())
}

struct FixedHighAbrFactory;

struct FixedHighAbrController {
    rungs: Vec<dashplay::QualityRung>,
}

impl dashplay::AbrFactory for FixedHighAbrFactory {
    fn create(
        &self,
        adaptation_set: &dash_mpd::AdaptationSet,
        ctx: &dashplay::AbrCreateContext<'_>,
    ) -> Option<Box<dyn dashplay::AbrController>> {
        let rungs = if let Some(ladder) = ctx.quality_ladder {
            ladder.to_vec()
        } else {
            dashplay::quality_ladder_from_adaptation_set(adaptation_set)
        };
        if rungs.is_empty() {
            return None;
        }
        Some(Box::new(FixedHighAbrController { rungs }))
    }
}

impl dashplay::AbrController for FixedHighAbrController {
    fn update_buffer(&mut self, _buffer_s: f64) {}

    fn observe_segment_download(
        &mut self,
        _throughput_bps: f64,
        _downloaded_bytes: usize,
        _quality_index: usize,
    ) {
    }

    fn decide(&mut self) -> dashplay::AbrDecision {
        let quality_index = self.rungs.len().saturating_sub(1);
        dashplay::AbrDecision {
            quality_index,
            bitrate_bps: self.rungs[quality_index].bitrate_bps,
        }
    }

    fn rung_for_quality_index(&self, quality_index: usize) -> &dashplay::QualityRung {
        &self.rungs[quality_index]
    }

    fn rung_count(&self) -> usize {
        self.rungs.len()
    }
}

#[tokio::test]
async fn custom_abr_factory_selects_fixed_representation() -> Result<(), dashplay::PlayerError> {
    use dash_mpd::AdaptationSet;
    use dashplay::{
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
        fn create(
            &self,
            adaptation_set: &AdaptationSet,
            ctx: &dashplay::AbrCreateContext<'_>,
        ) -> Option<Box<dyn AbrController>> {
            let rungs = if let Some(ladder) = ctx.quality_ladder {
                ladder.to_vec()
            } else {
                quality_ladder_from_adaptation_set(adaptation_set)
            };
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

        fn decide(&mut self) -> AbrDecision {
            AbrDecision {
                quality_index: self.quality_index,
                bitrate_bps: self.rungs[self.quality_index].bitrate_bps,
            }
        }

        fn rung_for_quality_index(&self, quality_index: usize) -> &QualityRung {
            &self.rungs[quality_index]
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

#[tokio::test]
async fn lol_plus_abr_factory_plays_fixture() -> Result<(), dashplay::PlayerError> {
    use dashplay::{LolPlusAbrFactory, Player, shared_abr_factory};

    let server = FixtureServer::spawn("vod_abr").await;
    let player = Player::new(server.manifest_url.as_str(), None)?.with_abr_factory(
        shared_abr_factory(LolPlusAbrFactory {
            // Prefer the low rung until throughput is observed, then adapt.
            ..LolPlusAbrFactory::default()
        }),
    );
    let outputs = player.start_tracks().await?;
    let buffer_feedback = outputs.buffer_feedback(0).expect("one track");
    let _ = buffer_feedback.report(5.0);
    let _drain = common::spawn_playback_buffer_simulation(buffer_feedback, 5.0);
    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("one track")
        .into_receiver();
    let events = common::collect_events(&mut rx, ABR_TIMEOUT).await;
    outputs.join.await.unwrap()?;

    assert!(
        init_payload(&events).is_some(),
        "expected at least one init"
    );
    assert!(!segment_payloads(&events).is_empty());
    assert!(has_end(&events));
    Ok(())
}

#[tokio::test]
async fn adaptation_set_switching_merges_ladder_and_fetches_peer_as()
-> Result<(), dashplay::PlayerError> {
    use dash_mpd::AdaptationSet;
    use dashplay::{
        AbrController, AbrDecision, AbrFactory, Player, QualityRung, TrackInfo, TrackKind,
        quality_ladder_from_adaptation_set, shared_abr_factory,
    };

    struct PeerQualityAbrFactory {
        /// Prefer the highest rung (peer HEVC AS).
        prefer_highest: bool,
    }

    struct PeerQualityAbrController {
        rungs: Vec<QualityRung>,
        quality_index: usize,
    }

    impl AbrFactory for PeerQualityAbrFactory {
        fn create(
            &self,
            adaptation_set: &AdaptationSet,
            ctx: &dashplay::AbrCreateContext<'_>,
        ) -> Option<Box<dyn AbrController>> {
            let rungs = if let Some(ladder) = ctx.quality_ladder {
                ladder.to_vec()
            } else {
                quality_ladder_from_adaptation_set(adaptation_set)
            };
            if rungs.is_empty() {
                return None;
            }
            let quality_index = if self.prefer_highest {
                rungs.len() - 1
            } else {
                0
            };
            Some(Box::new(PeerQualityAbrController {
                rungs,
                quality_index,
            }))
        }
    }

    impl AbrController for PeerQualityAbrController {
        fn update_buffer(&mut self, _buffer_s: f64) {}

        fn observe_segment_download(
            &mut self,
            _throughput_bps: f64,
            _downloaded_bytes: usize,
            _quality_index: usize,
        ) {
        }

        fn decide(&mut self) -> AbrDecision {
            AbrDecision {
                quality_index: self.quality_index,
                bitrate_bps: self.rungs[self.quality_index].bitrate_bps,
            }
        }

        fn rung_for_quality_index(&self, quality_index: usize) -> &QualityRung {
            &self.rungs[quality_index]
        }

        fn rung_count(&self) -> usize {
            self.rungs.len()
        }
    }

    let server = FixtureServer::spawn("vod_as_switching").await;
    let player = Player::new(server.manifest_url.as_str(), None)?.with_abr_factory(
        shared_abr_factory(PeerQualityAbrFactory {
            prefer_highest: true,
        }),
    );
    let outputs = player.start_tracks().await?;
    assert_eq!(outputs.tracks.len(), 1);
    let info: TrackInfo = outputs.tracks[0].info();
    assert_eq!(info.kind, TrackKind::Video);
    assert_eq!(info.id.as_deref(), Some("264"));
    assert_eq!(info.switchable_adaptation_set_ids, vec!["265".to_string()]);

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
        Some(b"dashplay-as-hevc-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-as-hevc-seg-1".to_vec(),
            b"dashplay-as-hevc-seg-2".to_vec(),
        ]
    );
    assert!(has_end(&events));
    Ok(())
}
