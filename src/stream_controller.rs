//! Orchestrates manifest refresh, period selection, and parallel adaptation-set streams
//! (dash.js: `StreamController` coordinating multiple `Stream` instances).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::future::join_all;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::sleep;
use url::Url;

use super::drm::coordinator::DrmSessionCoordinator;

use super::PlayerError;
use super::abr::SharedAbrFactory;
use super::dash_stream::{AdaptationStreamContext, run_adaptation_stream};
use super::delivered_segments::DeliveredSegmentTracker;
use super::http::SharedHttpClient;
use super::manifest;
use super::manifest_update;
use super::media_events;
use super::playback_control::{PlaybackController, PlaybackState};
use super::segment_blacklist::SegmentBlacklist;
use super::track_selection::{TrackSelection, select_adaptation_sets};
use super::types::PlayerEvent;
use super::utc_timing;

pub(crate) struct PlaybackLoopState {
    pub client: SharedHttpClient,
    pub manifest_uri: Url,
    pub drm: DrmSessionCoordinator,
    pub playback: PlaybackController,
    pub track_selection: TrackSelection,
    pub abr_factory: SharedAbrFactory,
}

impl PlaybackLoopState {
    pub async fn run(self, tracks: Vec<super::types::PlayerTrack>) -> Result<(), PlayerError> {
        let PlaybackLoopState {
            client,
            manifest_uri,
            drm,
            playback,
            track_selection,
            abr_factory,
        } = self;

        let mut manifest = None;
        let mut mpd_xml = None;
        let mut manifest_session = manifest_update::ManifestSession::default();
        manifest_session.initialize(manifest_uri.clone());
        let mut last_period_idx = None;
        let mut seek_target_override: Option<std::time::Duration> = None;
        let mut emitted_mpd_events: std::collections::HashSet<(String, u64, u64)> =
            std::collections::HashSet::new();

        let have_init: Arc<Vec<AtomicBool>> =
            Arc::new((0..tracks.len()).map(|_| AtomicBool::new(false)).collect());
        let delivered: Arc<Vec<Arc<Mutex<DeliveredSegmentTracker>>>> = Arc::new(
            (0..tracks.len())
                .map(|_| Arc::new(Mutex::new(DeliveredSegmentTracker::default())))
                .collect(),
        );
        let blacklist = SegmentBlacklist::new();
        let drm = Arc::new(AsyncMutex::new(drm));
        let inband_prt_anchors: Arc<
            Vec<Arc<Mutex<Option<super::resync::ProducerReferenceAnchor>>>>,
        > = Arc::new(
            (0..tracks.len())
                .map(|_| Arc::new(Mutex::new(None)))
                .collect(),
        );

        let run_result: Result<(), PlayerError> = async {
            loop {
                if playback.is_stopped() {
                    break;
                }
                if tracks.iter().all(|t| t.receiver_count() == 0) {
                    break;
                }

                playback.set_state(PlaybackState::LoadingManifest);

                manifest_session.refresh(&client, &manifest_uri).await?;
                manifest_session.sync_steering(&client).await?;
                manifest = Some(manifest_session.parsed.clone().expect("parsed"));
                mpd_xml = Some(manifest_session.xml()?.to_string());
                let active_manifest_uri = manifest_session.manifest_uri()?.clone();

                let mpd_ref = manifest::mpd(&manifest)?;
                let min_update = mpd_ref
                    .minimumUpdatePeriod
                    .unwrap_or(std::time::Duration::ZERO);

                for t in &tracks {
                    let _ = t.tx.send(PlayerEvent::ManifestLoaded {
                        is_dynamic: manifest::is_dynamic_mpd(mpd_ref),
                        media_presentation_duration: mpd_ref.mediaPresentationDuration,
                    });
                }

                let is_dynamic = manifest::is_dynamic_mpd(mpd_ref);
                let wall_now =
                    utc_timing::wall_clock_utc(&client, mpd_ref, Some(&active_manifest_uri)).await;

                if let Some(seek) = playback.take_seek_target() {
                    seek_target_override = Some(seek);
                    let playhead = playback.presentation_time();
                    for t in &tracks {
                        let _ = t.tx.send(PlayerEvent::PlayheadUpdated {
                            presentation_time: playhead,
                        });
                    }
                }

                let period_windows = manifest::period_windows(mpd_ref)?;
                let periods_to_play: Vec<manifest::PeriodWindow> = if is_dynamic {
                    vec![manifest::current_period_window_at(mpd_ref, wall_now)?]
                } else {
                    period_windows
                };

                let seek_generation_at_start = playback.seek_generation();
                let mut seek_interrupted = false;

                for current_window in periods_to_play {
                    if playback.is_stopped() {
                        break;
                    }

                    if last_period_idx != Some(current_window.idx) {
                        for flag in have_init.iter() {
                            flag.store(false, Ordering::Release);
                        }
                        for tracker in delivered.iter() {
                            if let Ok(mut t) = tracker.lock() {
                                t.reset();
                            }
                        }
                        for anchor in inband_prt_anchors.iter() {
                            if let Ok(mut a) = anchor.lock() {
                                *a = None;
                            }
                        }
                    }
                    last_period_idx = Some(current_window.idx);

                    if let Some(xml) = mpd_xml.as_deref() {
                        drm.lock()
                            .await
                            .sync_from_mpd(xml, current_window.idx)
                            .await?;
                    }
                    drm.lock().await.poll_renewals().await?;

                    let period_start = current_window.start;
                    let period = mpd_ref.periods[current_window.idx].clone();
                    let mpd_events = media_events::mpd_events_for_period(&period);
                    for event in mpd_events {
                        let key = (
                            event.scheme_id_uri.clone(),
                            event.presentation_time,
                            event.id.unwrap_or(0),
                        );
                        if !emitted_mpd_events.insert(key) {
                            continue;
                        }
                        for t in &tracks {
                            let _ = t.tx.send(PlayerEvent::MediaEvent(event.clone()));
                        }
                    }

                    let steering = &manifest_session.steering;
                    let segment_base_ctx = manifest::SegmentBaseContext {
                        manifest_uri: active_manifest_uri.clone(),
                        mpd_base_urls: mpd_ref.base_url.clone(),
                        period_base_urls: period.BaseURL.clone(),
                        service_location_priority: steering.service_location_priority().to_vec(),
                        default_service_location: steering
                            .config
                            .as_ref()
                            .and_then(|c| c.default_service_location.clone()),
                    };

                    let since_ast_utc = manifest::since_availability_start_at(mpd_ref, wall_now)?;
                    let adaptation_sets = select_adaptation_sets(&period, &track_selection);
                    let prt_reference_id = super::resync::latency_reference_id(mpd_ref);

                    let reference_since_ast = adaptation_sets
                        .first()
                        .and_then(|selected| {
                            selected
                                .adaptation_set
                                .representations
                                .first()
                                .and_then(|rep| {
                                    let inband = inband_prt_anchors
                                        .first()
                                        .and_then(|a| a.lock().ok().and_then(|g| *g));
                                    super::resync::resync_corrected_since_ast(
                                        mpd_ref,
                                        wall_now,
                                        &period,
                                        period_start,
                                        selected.adaptation_set,
                                        rep,
                                        inband,
                                    )
                                })
                        })
                        .or(since_ast_utc);

                    let period_target_time = if let Some(seek) = seek_target_override {
                        Some(seek)
                    } else if let Some(s) = reference_since_ast {
                        Some(manifest::target_presentation_time_from_since(mpd_ref, s))
                    } else {
                        manifest::target_presentation_time_at(mpd_ref, wall_now)?
                    };

                    let mut streams = Vec::new();
                    for (track_idx, selected) in adaptation_sets.into_iter().enumerate() {
                        if track_idx >= tracks.len() {
                            break;
                        }
                        let adaptation_set = selected.adaptation_set.clone();
                        let period_adaptation_index = selected.info.period_adaptation_index;
                        let rep = adaptation_set.representations.first();
                        let since_ast = rep
                            .and_then(|r| {
                                let inband = inband_prt_anchors
                                    .get(track_idx)
                                    .and_then(|a| a.lock().ok().and_then(|g| *g));
                                super::resync::resync_corrected_since_ast(
                                    mpd_ref,
                                    wall_now,
                                    &period,
                                    period_start,
                                    &adaptation_set,
                                    r,
                                    inband,
                                )
                            })
                            .or(since_ast_utc);
                        let resync_hints = rep
                            .and_then(|r| super::resync::resync_hints(&period, &adaptation_set, r));
                        let timeline_ctx = manifest::TimelineBuildContext {
                            is_dynamic,
                            period_window: current_window,
                            period_duration: period.duration,
                            media_presentation_duration: mpd_ref.mediaPresentationDuration,
                            time_shift_buffer_depth: mpd_ref.timeShiftBufferDepth,
                            since_availability_start: since_ast,
                            resync_hints,
                        };

                        let tx = tracks[track_idx].tx.clone();
                        let have_init = have_init.clone();
                        let client = client.clone();
                        let segment_base_ctx = segment_base_ctx.clone();
                        let blacklist = blacklist.clone();
                        let drm = drm.clone();
                        let buffer_rx = tracks[track_idx].buffer_rx.clone();
                        let metrics = tracks[track_idx].metrics.clone();
                        let delivered = delivered[track_idx].clone();
                        let playback = playback.clone();
                        let abr_factory = abr_factory.clone();
                        let inband_prt_anchor = inband_prt_anchors[track_idx].clone();
                        let prt_reference_id = prt_reference_id.clone();

                        let period = period.clone();
                        streams.push(async move {
                            run_adaptation_stream(AdaptationStreamContext {
                                client,
                                segment_base_ctx,
                                target_time: period_target_time,
                                period_start,
                                period,
                                timeline_ctx,
                                adaptation_set,
                                track_idx,
                                period_adaptation_index,
                                tx,
                                have_init,
                                delivered,
                                blacklist,
                                drm,
                                buffer_rx,
                                metrics,
                                playback,
                                abr_factory,
                                inband_prt_anchor,
                                prt_reference_id,
                            })
                            .await
                        });
                    }

                    for result in join_all(streams).await {
                        result?;
                    }

                    if playback.seek_generation() != seek_generation_at_start {
                        seek_interrupted = true;
                        reset_for_seek(&have_init, &delivered, &inband_prt_anchors);
                        break;
                    }
                }

                if playback.is_stopped() {
                    send_playback_ended(&tracks);
                    playback.set_state(PlaybackState::Ended);
                    break;
                }

                if seek_interrupted {
                    continue;
                }

                if min_update.is_zero() {
                    send_playback_ended(&tracks);
                    playback.set_state(PlaybackState::Ended);
                    break;
                }

                seek_target_override = None;

                playback.wait_while_paused().await;
                if playback.is_stopped() {
                    send_playback_ended(&tracks);
                    playback.set_state(PlaybackState::Ended);
                    break;
                }

                sleep(min_update).await;
            }

            Ok(())
        }
        .await;

        if let Err(ref err) = run_result {
            let event_err = super::types::PlayerEventError::from(err);
            for t in &tracks {
                let _ = t.tx.send(PlayerEvent::Error(event_err.clone()));
            }
            playback.mark_error();
        }
        run_result
    }
}

fn send_playback_ended(tracks: &[super::types::PlayerTrack]) {
    for t in tracks {
        let _ = t.tx.send(PlayerEvent::PlaybackEnded);
        let _ = t.tx.send(PlayerEvent::End);
    }
}

fn reset_for_seek(
    have_init: &Arc<Vec<AtomicBool>>,
    delivered: &Arc<Vec<Arc<Mutex<DeliveredSegmentTracker>>>>,
    inband_prt_anchors: &Arc<Vec<Arc<Mutex<Option<super::resync::ProducerReferenceAnchor>>>>>,
) {
    for flag in have_init.iter() {
        flag.store(false, Ordering::Release);
    }
    for tracker in delivered.iter() {
        if let Ok(mut t) = tracker.lock() {
            t.reset();
        }
    }
    for anchor in inband_prt_anchors.iter() {
        if let Ok(mut a) = anchor.lock() {
            *a = None;
        }
    }
}
