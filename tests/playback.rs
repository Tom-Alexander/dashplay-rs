mod common;

use common::{
    FixtureServer, has_end, init_payload, init_payloads, play_all_tracks, play_single_track,
    segment_payloads, trim_payload,
};
use dashplay::{PeriodTransitionKind, PlayerEvent};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn vod_single_track_emits_init_segments_and_end() {
    let server = FixtureServer::spawn("vod_single").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-seg-1".to_vec(), b"dashplay-seg-2".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_mp2t_delivers_ts_segments_without_init() {
    let server = FixtureServer::spawn("vod_mp2t").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert!(
        init_payloads(&events).is_empty(),
        "mp2t profiles omit Initialization; no Init event expected"
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-ts-1".to_vec(), b"dashplay-ts-2".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_webm_delivers_init_and_webm_segments() {
    let server = FixtureServer::spawn("vod_webm").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-webm-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-webm-1".to_vec(), b"dashplay-webm-2".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_ac4_profile_playback() {
    let server = FixtureServer::spawn("vod_ac4").await;
    let selection = dashplay::TrackSelection::default()
        .with_video(dashplay::TrackPreference::default().max_tracks(0))
        .with_audio(
            dashplay::TrackPreference::default()
                .codec("ac-4")
                .max_tracks(1),
        );
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(selection);
    let outputs = player.start_tracks().await.expect("start");
    assert_eq!(outputs.track_count(), 1);
    assert_eq!(outputs.tracks[0].info().codecs, vec!["ac-4.02.01.00"]);

    let all = play_all_tracks_with_outputs(outputs, TIMEOUT)
        .await
        .expect("playback");
    let events = &all[0];
    assert_eq!(
        init_payload(events).as_deref(),
        Some(b"dashplay-ac4-init".as_ref())
    );
    assert_eq!(
        segment_payloads(events),
        vec![
            b"dashplay-ac4-seg-1".to_vec(),
            b"dashplay-ac4-seg-2".to_vec()
        ]
    );
    assert!(has_end(events));
}

#[tokio::test]
async fn vod_mha1_profile_playback() {
    let server = FixtureServer::spawn("vod_mha1").await;
    let selection = dashplay::TrackSelection::default()
        .with_video(dashplay::TrackPreference::default().max_tracks(0))
        .with_audio(
            dashplay::TrackPreference::default()
                .codec("mha1")
                .max_tracks(1),
        );
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(selection);
    let outputs = player.start_tracks().await.expect("start");
    assert_eq!(outputs.tracks[0].info().codecs, vec!["mha1.0.4.L3.C"]);

    let all = play_all_tracks_with_outputs(outputs, TIMEOUT)
        .await
        .expect("playback");
    let events = &all[0];
    assert_eq!(
        init_payload(events).as_deref(),
        Some(b"dashplay-mha1-init".as_ref())
    );
    assert_eq!(
        segment_payloads(events),
        vec![
            b"dashplay-mha1-seg-1".to_vec(),
            b"dashplay-mha1-seg-2".to_vec()
        ]
    );
    assert!(has_end(events));
}

#[tokio::test]
async fn vod_vp9_hdr_profile_playback() {
    let server = FixtureServer::spawn("vod_vp9_hdr").await;
    let selection = dashplay::TrackSelection::default().with_video(
        dashplay::TrackPreference::default()
            .codec("vp09")
            .max_tracks(1),
    );
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(selection);
    let outputs = player.start_tracks().await.expect("start");
    assert_eq!(
        outputs.tracks[0].info().codecs,
        vec!["vp09.02.10.10.01.09.16.09.01"]
    );

    let all = play_all_tracks_with_outputs(outputs, TIMEOUT)
        .await
        .expect("playback");
    let events = &all[0];
    assert_eq!(
        init_payload(events).as_deref(),
        Some(b"dashplay-vp9-init".as_ref())
    );
    assert_eq!(
        segment_payloads(events),
        vec![
            b"dashplay-vp9-seg-1".to_vec(),
            b"dashplay-vp9-seg-2".to_vec()
        ]
    );
    assert!(has_end(events));
}

#[tokio::test]
async fn vod_dvb_hbbtv_profile_selects_primary_not_fallback() {
    let server = FixtureServer::spawn("vod_dvb_hbbtv").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-seg-1".to_vec(), b"dashplay-seg-2".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_template_width_height_frame_rate_and_ext() {
    let server = FixtureServer::spawn("vod_template_vars").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-seg-1".to_vec(), b"dashplay-seg-2".to_vec(),]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_segment_list_playback() {
    let server = FixtureServer::spawn("vod_segment_list").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-seg-1".to_vec(), b"dashplay-seg-2".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_segment_list_byte_range_playback() {
    let server = FixtureServer::spawn("vod_segment_list_ranges").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-seg-1".to_vec(), b"dashplay-seg-2".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_segment_timeline_playback() {
    let server = FixtureServer::spawn("vod_timeline").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-timeline-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-timeline-seg-1".to_vec(),
            b"dashplay-timeline-seg-2".to_vec()
        ]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_segment_base_playback() {
    let server = FixtureServer::spawn("vod_segment_base").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"SEGMENT-1!!".to_vec(), b"SEGMENT-2!!".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_segment_base_presentation_duration_playback() {
    let server = FixtureServer::spawn("vod_segment_base_presentation_duration").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"INIT!!!WHOLE-FILE!!".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_segment_base_whole_file_playback() {
    let server = FixtureServer::spawn("vod_segment_base_whole_file").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    // No Initialization: same as WebVTT — no Init event, BaseURL delivered as one Segment.
    assert!(init_payload(&events).is_none());
    assert_eq!(segment_payloads(&events), vec![b"WHOLE-FILE!!".to_vec()]);
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_template_sidecar_index_playback() {
    let server = FixtureServer::spawn("vod_template_index").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"SEGMENT-1!!".to_vec(), b"SEGMENT-2!!".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_template_sidecar_index_range_exact_playback() {
    let server = FixtureServer::spawn("vod_template_index_range_exact").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"SEGMENT-1!!".to_vec(), b"SEGMENT-2!!".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_template_sidecar_index_range_inexact_playback() {
    let server = FixtureServer::spawn("vod_template_index_range_inexact").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"SEGMENT-1!!".to_vec(), b"SEGMENT-2!!".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_template_representation_index_playback() {
    let server = FixtureServer::spawn("vod_template_representation_index").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"SEGMENT-1!!".to_vec(), b"SEGMENT-2!!".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_segment_base_representation_index_playback() {
    let server = FixtureServer::spawn("vod_segment_base_representation_index").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payload(&events).as_deref(), Some(b"INIT!!!".as_ref()));
    assert_eq!(
        segment_payloads(&events),
        vec![b"SEGMENT-1!!".to_vec(), b"SEGMENT-2!!".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_audio_video_parallel_tracks() {
    let server = FixtureServer::spawn("vod_av").await;
    let tracks = play_all_tracks(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(tracks.len(), 2);

    let video = &tracks[0];
    assert_eq!(
        init_payload(video).as_deref(),
        Some(b"dashplay-video-init".as_ref())
    );
    assert_eq!(
        segment_payloads(video),
        vec![
            b"dashplay-video-seg-1".to_vec(),
            b"dashplay-video-seg-2".to_vec()
        ]
    );
    assert!(has_end(video));

    let audio = &tracks[1];
    assert_eq!(
        init_payload(audio).as_deref(),
        Some(b"dashplay-audio-init".as_ref())
    );
    assert_eq!(
        segment_payloads(audio),
        vec![
            b"dashplay-audio-seg-1".to_vec(),
            b"dashplay-audio-seg-2".to_vec()
        ]
    );
    assert!(has_end(audio));
}

#[tokio::test]
async fn track_preferences_limit_outputs_and_expose_metadata() {
    let server = FixtureServer::spawn("vod_av").await;
    let selection = dashplay::TrackSelection::default()
        .with_video(dashplay::TrackPreference::default().max_tracks(0))
        .with_audio(
            dashplay::TrackPreference::default()
                .codec("mp4a")
                .max_tracks(1),
        );
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(selection);
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 1);
    assert_eq!(outputs.tracks[0].info().kind, dashplay::TrackKind::Audio);
    assert_eq!(outputs.tracks[0].info().codecs, vec!["mp4a.40.2"]);

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("audio")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.expect("join").expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-audio-init".as_ref())
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_multi_period_emits_inits_and_segments_in_order() {
    let server = FixtureServer::spawn("vod_multi_period").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert_eq!(inits.len(), 2, "expected init per period, got {inits:?}");
    assert_eq!(inits[0], b"dashplay-period1-init".to_vec());
    assert_eq!(inits[1], b"dashplay-period2-init".to_vec());

    let segments = segment_payloads(&events);
    assert_eq!(
        segments,
        vec![
            b"dashplay-period1-seg-1".to_vec(),
            b"dashplay-period1-seg-2".to_vec(),
            b"dashplay-period2-seg-1".to_vec(),
            b"dashplay-period2-seg-2".to_vec(),
        ]
    );
    assert!(has_end(&events));

    let period_events: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            PlayerEvent::PeriodChanged {
                period_index,
                transition,
                gap_before,
                ..
            } => Some((*period_index, *transition, *gap_before)),
            _ => None,
        })
        .collect();
    assert_eq!(
        period_events,
        vec![
            (0, PeriodTransitionKind::Discontinuous, None),
            (1, PeriodTransitionKind::Discontinuous, None),
        ]
    );
}

#[tokio::test]
async fn vod_period_gap_signals_presentation_hole() {
    let server = FixtureServer::spawn("vod_period_gap").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert_eq!(
        inits.len(),
        2,
        "gap periods remain discontinuous, got {inits:?}"
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-period1-seg-1".to_vec(),
            b"dashplay-period1-seg-2".to_vec(),
            b"dashplay-period2-seg-1".to_vec(),
            b"dashplay-period2-seg-2".to_vec(),
        ]
    );
    assert!(has_end(&events));

    let period_events: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            PlayerEvent::PeriodChanged {
                period_index,
                start,
                end,
                transition,
                gap_before,
            } => Some((*period_index, *start, *end, *transition, *gap_before)),
            _ => None,
        })
        .collect();
    assert_eq!(
        period_events,
        vec![
            (
                0,
                Duration::ZERO,
                Some(Duration::from_secs(8)),
                PeriodTransitionKind::Discontinuous,
                None,
            ),
            (
                1,
                Duration::from_secs(12),
                Some(Duration::from_secs(20)),
                PeriodTransitionKind::Discontinuous,
                Some(Duration::from_secs(4)),
            ),
        ]
    );

    let p2_seg_times: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            PlayerEvent::Segment {
                presentation_time,
                data,
                ..
            } if trim_payload(data.as_ref()) == b"dashplay-period2-seg-1" => {
                Some(*presentation_time)
            }
            _ => None,
        })
        .collect();
    assert_eq!(p2_seg_times, vec![Duration::from_secs(12)]);
}

#[tokio::test]
async fn vod_period_continuity_skips_init_reemission() {
    let server = FixtureServer::spawn("vod_period_continuity").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert_eq!(
        inits.len(),
        1,
        "continuous periods should reuse Init, got {inits:?}"
    );
    assert_eq!(inits[0], b"dashplay-shared-init".to_vec());
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-p1-seg-1".to_vec(),
            b"dashplay-p1-seg-2".to_vec(),
            b"dashplay-p2-seg-1".to_vec(),
            b"dashplay-p2-seg-2".to_vec(),
        ]
    );
    assert!(has_end(&events));

    let transitions: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            PlayerEvent::PeriodChanged { transition, .. } => Some(*transition),
            _ => None,
        })
        .collect();
    assert_eq!(
        transitions,
        vec![
            PeriodTransitionKind::Discontinuous,
            PeriodTransitionKind::Continuous,
        ]
    );
}

#[tokio::test]
async fn vod_period_connectivity_dedups_boundary_segment() {
    let server = FixtureServer::spawn("vod_period_connectivity").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    let inits = init_payloads(&events);
    assert_eq!(
        inits.len(),
        1,
        "connected periods should reuse Init, got {inits:?}"
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-p1-seg-1".to_vec(),
            b"dashplay-boundary".to_vec(),
            b"dashplay-p2-seg-2".to_vec(),
        ],
        "boundary media should be emitted once"
    );
    assert!(has_end(&events));

    let transitions: Vec<_> = events
        .iter()
        .filter_map(|ev| match ev {
            PlayerEvent::PeriodChanged { transition, .. } => Some(*transition),
            _ => None,
        })
        .collect();
    assert_eq!(
        transitions,
        vec![
            PeriodTransitionKind::Discontinuous,
            PeriodTransitionKind::Connected,
        ]
    );
}

#[tokio::test]
async fn vod_period_sync_buffer_prefetches_next_period_media() {
    let server = common::SyncBufferServer::spawn("vod_period_sync_buffer").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(init_payloads(&events).len(), 1);
    assert!(has_end(&events));
    assert!(
        server.p2_fetch_started_before_p1_seg2_emit(),
        "next-period media fetch should begin before the last period-1 segment is emitted ({})",
        server.request_debug()
    );
}

#[tokio::test]
async fn vod_end_number_bounds_segments_without_mpd_duration() {
    let server = FixtureServer::spawn("vod_end_number").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-seg-1".to_vec(), b"dashplay-seg-2".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vod_period_level_segment_template_inheritance() {
    let server = FixtureServer::spawn("vod_period_template").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-seg-1".to_vec(), b"dashplay-seg-2".to_vec()]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn base_url_failover_uses_secondary_host() {
    let server = FixtureServer::spawn_with_options("base_url_failover", &["/bad"]).await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-failover-init".as_ref())
    );
    assert_eq!(segment_payloads(&events).len(), 2);
    assert!(has_end(&events));
}

#[tokio::test]
async fn missing_segment_surfaces_request_error() {
    let server = FixtureServer::spawn("vod_missing_segment").await;

    let err = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect_err("expected segment fetch failure");

    assert!(
        matches!(
            err,
            dashplay::PlayerError::Segment(dashplay::SegmentError::RequestFailed {
                status: 404,
                ..
            })
        ),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn player_rejects_invalid_manifest_url() {
    let err = dashplay::Player::new("not-a-valid-url", None)
        .err()
        .expect("invalid url");
    assert!(matches!(
        err,
        dashplay::PlayerError::Manifest(dashplay::ManifestError::Url(_))
    ));
}

#[tokio::test]
async fn vod_time_template_addressing_playback() {
    let server = FixtureServer::spawn("vod_time").await;
    let events = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-time-0".to_vec(), b"dashplay-time-4000".to_vec(),]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn all_base_urls_fail_surfaces_segment_error() {
    let server = FixtureServer::spawn_with_options("base_url_all_bad", &["/a", "/b"]).await;

    let err = play_single_track(&server.manifest_url, TIMEOUT)
        .await
        .expect_err("expected all CDN bases to fail");

    assert!(
        matches!(
            err,
            dashplay::PlayerError::Segment(dashplay::SegmentError::RequestFailed {
                status: 404,
                ..
            })
        ),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn vod_merged_stream_emits_fragments() {
    use futures_util::StreamExt;

    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None).expect("player");
    let mut merged = player.start_merged().await.expect("start merged");

    let mut chunks = Vec::new();
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), merged.stream.next())
            .await
        {
            Ok(Some(Ok(bytes))) => chunks.push(bytes.to_vec()),
            Ok(Some(Err(e))) => panic!("stream error: {e}"),
            Ok(None) => break,
            Err(_) if !chunks.is_empty() => break,
            Err(_) => continue,
        }
    }

    merged.join.await.unwrap().expect("join");

    let chunks: Vec<_> = chunks.iter().map(|c| trim_payload(c)).collect();
    assert_eq!(chunks.len(), 3, "expected init + 2 segments");
    assert_eq!(chunks[0], b"dashplay-init-v1".to_vec());
    assert_eq!(chunks[1], b"dashplay-seg-1".to_vec());
    assert_eq!(chunks[2], b"dashplay-seg-2".to_vec());
}

#[tokio::test]
async fn vod_merged_async_read_pipes_bytes() {
    use tokio::io::AsyncReadExt;

    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None).expect("player");
    let merged = player.start_merged().await.expect("start merged");
    let mut async_read = merged.into_async_read();

    let mut buf = Vec::new();
    async_read
        .reader
        .read_to_end(&mut buf)
        .await
        .expect("read merged output");

    async_read.join.await.unwrap().expect("join");

    let expected = [
        b"dashplay-init-v1\n".as_slice(),
        b"dashplay-seg-1\n".as_slice(),
        b"dashplay-seg-2\n".as_slice(),
    ]
    .concat();
    assert_eq!(buf, expected);
}

#[tokio::test]
async fn track_subscription_helpers_expose_receivers() {
    use futures_util::StreamExt;

    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 1);

    let mut subscribe_rx = outputs.subscribe(0).expect("track");
    let first = common::recv_matching(&mut subscribe_rx, TIMEOUT, |ev| {
        matches!(ev, dashplay::PlayerEvent::Init(_))
    })
    .await
    .expect("init from subscribe");
    assert!(
        matches!(first, dashplay::PlayerEvent::Init(_)),
        "subscribe should receive init"
    );

    let track_out = outputs.tracks.first().expect("one track output");
    let mut segment_stream = track_out.events().filter(|res| {
        futures::future::ready(match res {
            Ok(ev) => matches!(ev, dashplay::PlayerEvent::Segment { .. }),
            Err(_) => false,
        })
    });
    let next_from_stream = tokio::time::timeout(TIMEOUT, segment_stream.next())
        .await
        .expect("segment from events stream")
        .expect("stream item")
        .expect("event");
    assert!(
        matches!(next_from_stream, dashplay::PlayerEvent::Segment { .. }),
        "events() should receive segments after init"
    );

    if let Some(feedback) = outputs.buffer_feedback(0) {
        let _ = feedback.report(25.0);
    }
    while tokio::time::timeout(std::time::Duration::from_millis(500), subscribe_rx.recv())
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|ev| !ev.is_terminal())
    {}

    outputs.join.await.unwrap().expect("join");
}

#[tokio::test]
async fn track_metrics_collect_playback_observations() {
    use common::recv_matching;
    use dashplay::PlayerEvent;

    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let metrics = outputs.metrics(0).expect("track metrics");

    let mut rx = outputs.subscribe(0).expect("track");
    let _ = recv_matching(&mut rx, TIMEOUT, |ev| matches!(ev, PlayerEvent::Init(_)))
        .await
        .expect("init");
    let _ = recv_matching(&mut rx, TIMEOUT, |ev| {
        matches!(ev, PlayerEvent::Segment { .. })
    })
    .await
    .expect("segment");

    if let Some(feedback) = outputs.buffer_feedback(0) {
        let _ = feedback.report(25.0);
        feedback.report(2.0).expect("buffer report");
    }

    while tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .ok()
        .and_then(Result::ok)
        .is_some_and(|ev| !ev.is_terminal())
    {}

    outputs.join.await.unwrap().expect("join");

    let snap = metrics.snapshot();
    assert!(snap.startup_delay.is_some());
    assert!(!snap.throughput_history.is_empty());
    assert!(snap.throughput_bps > 0.0);
    assert!(!snap.buffer_history.is_empty());
    assert_eq!(snap.rebuffer_events.len(), 1);
}

#[tokio::test]
async fn richer_lifecycle_events_are_emitted() {
    use common::recv_matching;
    use dashplay::PlayerEvent;

    let server = FixtureServer::spawn("vod_single").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let mut rx = outputs.subscribe(0).expect("track");

    let manifest_loaded = recv_matching(&mut rx, TIMEOUT, |ev| {
        matches!(ev, PlayerEvent::ManifestLoaded { .. })
    })
    .await
    .expect("manifest loaded");
    assert!(
        matches!(
            manifest_loaded,
            PlayerEvent::ManifestLoaded {
                is_dynamic: false,
                media_presentation_duration: Some(_),
                ..
            }
        ),
        "expected static VOD manifest metadata"
    );

    let _ = recv_matching(&mut rx, TIMEOUT, |ev| matches!(ev, PlayerEvent::Init(_)))
        .await
        .expect("init");

    if let Some(feedback) = outputs.buffer_feedback(0) {
        let buffer_updated = recv_matching(&mut rx, TIMEOUT, |ev| {
            matches!(ev, PlayerEvent::BufferUpdated { buffer_s: 12.0 })
        });
        feedback.report(12.0).expect("buffer report");
        assert!(
            buffer_updated.await.is_some(),
            "buffer feedback should emit BufferUpdated"
        );
    }

    let _ = recv_matching(&mut rx, TIMEOUT, |ev| {
        matches!(ev, PlayerEvent::PlaybackStarted)
    })
    .await
    .expect("playback started");

    let _ = recv_matching(&mut rx, TIMEOUT, |ev| {
        matches!(ev, PlayerEvent::PlaybackEnded)
    })
    .await
    .expect("playback ended");

    assert!(
        recv_matching(&mut rx, TIMEOUT, |ev| matches!(ev, PlayerEvent::End))
            .await
            .is_some(),
        "expected End after PlaybackEnded"
    );

    outputs.join.await.unwrap().expect("join");
}

fn text_track_selection() -> dashplay::TrackSelection {
    dashplay::TrackSelection::default()
        .with_video(dashplay::TrackPreference::default().max_tracks(0))
        .with_text(
            dashplay::TrackPreference::default()
                .language("en")
                .max_tracks(1),
        )
}

#[tokio::test]
async fn ttml_subtitle_track_delivers_init_and_segments() {
    let server = FixtureServer::spawn("subtitle_ttml").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(text_track_selection());
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 1);
    assert_eq!(outputs.tracks[0].info().kind, dashplay::TrackKind::Text);
    assert_eq!(
        outputs.tracks[0].info().subtitle_type,
        Some(dashplay::SubtitleType::Ttml)
    );

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("text track")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.unwrap().expect("join");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-ttml-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-ttml-seg-1".to_vec(),
            b"dashplay-ttml-seg-2".to_vec()
        ]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn vtt_subtitle_track_delivers_segments_without_init() {
    let server = FixtureServer::spawn("subtitle_vtt").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(text_track_selection());
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(
        outputs.tracks[0].info().subtitle_type,
        Some(dashplay::SubtitleType::Vtt)
    );

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("text track")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.unwrap().expect("join");

    assert!(init_payload(&events).is_none());
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-vtt-seg-1".to_vec(),
            b"dashplay-vtt-seg-2".to_vec()
        ]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn inband_stpp_subtitle_track_delivers_fragments() {
    let server = FixtureServer::spawn("subtitle_inband_stpp").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(text_track_selection());
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(
        outputs.tracks[0].info().subtitle_type,
        Some(dashplay::SubtitleType::Stpp)
    );
    assert_eq!(
        outputs.tracks[0].info().mime_type.as_deref(),
        Some("application/mp4")
    );

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("text track")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.unwrap().expect("join");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-stpp-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-stpp-seg-1".to_vec(),
            b"dashplay-stpp-seg-2".to_vec()
        ]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn subtitle_and_video_tracks_play_in_parallel() {
    let server = FixtureServer::spawn("subtitle_ttml").await;
    let selection = dashplay::TrackSelection::default().with_text(
        dashplay::TrackPreference::default()
            .language("en")
            .max_tracks(1),
    );
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(selection);
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 2);
    assert_eq!(outputs.tracks[0].info().kind, dashplay::TrackKind::Text);
    assert_eq!(outputs.tracks[1].info().kind, dashplay::TrackKind::Video);

    let all = play_all_tracks_with_outputs(outputs, TIMEOUT)
        .await
        .expect("playback");
    let text = &all[0];
    let video = &all[1];

    assert_eq!(
        init_payload(text).as_deref(),
        Some(b"dashplay-ttml-init".as_ref())
    );
    assert_eq!(
        init_payload(video).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert!(has_end(text));
    assert!(has_end(video));
}

fn trick_play_track_selection() -> dashplay::TrackSelection {
    dashplay::TrackSelection::default()
        .with_video(dashplay::TrackPreference::default().max_tracks(0))
        .with_trick_play(dashplay::TrackPreference::default().max_tracks(1))
}

#[tokio::test]
async fn trick_play_track_delivers_init_and_segments() {
    let server = FixtureServer::spawn("dashif_trick_play").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(trick_play_track_selection());
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 1);
    assert_eq!(
        outputs.tracks[0].info().kind,
        dashplay::TrackKind::TrickPlay
    );
    assert_eq!(
        outputs.tracks[0].info().mime_type.as_deref(),
        Some("video/mp4")
    );

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("trick-play track")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.unwrap().expect("join");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![b"dashplay-seg-1".to_vec(), b"dashplay-seg-2".to_vec()]
    );
    assert!(has_end(&events));
}

fn image_track_selection() -> dashplay::TrackSelection {
    dashplay::TrackSelection::default()
        .with_video(dashplay::TrackPreference::default().max_tracks(0))
        .with_image(dashplay::TrackPreference::default().max_tracks(1))
}

#[tokio::test]
async fn image_thumbnail_track_delivers_init_and_segments() {
    let server = FixtureServer::spawn("thumbnail_jpeg").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(image_track_selection());
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 1);
    assert_eq!(outputs.tracks[0].info().kind, dashplay::TrackKind::Image);
    assert_eq!(outputs.tracks[0].info().thumbnail_tile, Some((4, 2)));
    assert_eq!(
        outputs.tracks[0].info().mime_type.as_deref(),
        Some("image/jpeg")
    );

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("image track")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.unwrap().expect("join");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-thumb-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-thumb-seg-1".to_vec(),
            b"dashplay-thumb-seg-2".to_vec()
        ]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn image_png_thumbnail_track_delivers_via_ext_template() {
    let server = FixtureServer::spawn("thumbnail_png").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(image_track_selection());
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 1);
    assert_eq!(outputs.tracks[0].info().kind, dashplay::TrackKind::Image);
    assert_eq!(outputs.tracks[0].info().thumbnail_tile, Some((4, 2)));
    assert_eq!(
        outputs.tracks[0].info().mime_type.as_deref(),
        Some("image/png")
    );

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("image track")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.unwrap().expect("join");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-png-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-png-seg-1".to_vec(),
            b"dashplay-png-seg-2".to_vec()
        ]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn image_bmp_thumbnail_track_accepts_legacy_tile_scheme() {
    let server = FixtureServer::spawn("thumbnail_bmp").await;
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(image_track_selection());
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 1);
    assert_eq!(outputs.tracks[0].info().kind, dashplay::TrackKind::Image);
    assert_eq!(outputs.tracks[0].info().thumbnail_tile, Some((2, 2)));
    assert_eq!(
        outputs.tracks[0].info().mime_type.as_deref(),
        Some("image/bmp")
    );

    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("image track")
        .into_receiver();
    let events = common::collect_events(&mut rx, TIMEOUT).await;
    outputs.join.await.unwrap().expect("join");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-bmp-init".as_ref())
    );
    assert_eq!(
        segment_payloads(&events),
        vec![
            b"dashplay-bmp-seg-1".to_vec(),
            b"dashplay-bmp-seg-2".to_vec()
        ]
    );
    assert!(has_end(&events));
}

#[tokio::test]
async fn trick_play_and_video_tracks_play_in_parallel() {
    let server = FixtureServer::spawn("dashif_trick_play").await;
    let selection = dashplay::TrackSelection::default()
        .with_trick_play(dashplay::TrackPreference::default().max_tracks(1));
    let player = dashplay::Player::new(server.manifest_url.as_str(), None)
        .expect("player")
        .with_track_selection(selection);
    let outputs = player.start_tracks().await.expect("start");

    assert_eq!(outputs.track_count(), 2);
    assert_eq!(outputs.tracks[0].info().kind, dashplay::TrackKind::Video);
    assert_eq!(
        outputs.tracks[1].info().kind,
        dashplay::TrackKind::TrickPlay
    );

    let all = play_all_tracks_with_outputs(outputs, TIMEOUT)
        .await
        .expect("playback");
    let video = &all[0];
    let trick = &all[1];

    assert_eq!(
        init_payload(video).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert_eq!(
        init_payload(trick).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert!(has_end(video));
    assert!(has_end(trick));
}

async fn play_all_tracks_with_outputs(
    outputs: dashplay::PlayerTrackOutputs,
    timeout: std::time::Duration,
) -> Result<Vec<Vec<dashplay::PlayerEvent>>, dashplay::PlayerError> {
    let track_count = outputs.track_count();
    let mut drains = Vec::with_capacity(track_count);
    for i in 0..track_count {
        if let Some(feedback) = outputs.buffer_feedback(i) {
            drains.push(common::spawn_playback_buffer_simulation(feedback, 25.0));
        }
    }
    let mut receivers: Vec<_> = outputs
        .tracks
        .into_iter()
        .map(|t| t.into_receiver())
        .collect();

    let mut all_events = Vec::with_capacity(track_count);
    for rx in &mut receivers {
        all_events.push(common::collect_events(rx, timeout).await);
    }
    for drain in drains {
        drain.abort();
    }
    outputs.join.await.unwrap()?;
    Ok(all_events)
}
