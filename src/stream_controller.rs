//! Orchestrates manifest refresh, period selection, and parallel adaptation-set streams
//! (dash.js: `StreamController` coordinating multiple `Stream` instances).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::future::join_all;
use reqwest::Client;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::sleep;
use url::Url;

use super::drm::coordinator::DrmSessionCoordinator;

use super::PlayerError;
use super::dash_stream::{AdaptationStreamContext, run_adaptation_stream};
use super::delivered_segments::DeliveredSegmentTracker;
use super::manifest;
use super::playback_control::{PlaybackController, PlaybackState};
use super::segment_blacklist::SegmentBlacklist;
use super::track_selection::{TrackSelection, select_adaptation_sets};
use super::types::PlayerEvent;
use super::utc_timing;

pub(crate) struct PlaybackLoopState {
    pub client: Client,
    pub manifest_uri: Url,
    pub drm: DrmSessionCoordinator,
    pub playback: PlaybackController,
    pub track_selection: TrackSelection,
}

impl PlaybackLoopState {
    pub async fn run(self, tracks: Vec<super::types::PlayerTrack>) -> Result<(), PlayerError> {
        let PlaybackLoopState {
            client,
            manifest_uri,
            drm,
            playback,
            track_selection,
        } = self;

        let mut manifest = None;
        let mut mpd_xml = None;
        let mut last_period_idx = None;
        let mut seek_target_override: Option<std::time::Duration> = None;

        let have_init: Arc<Vec<AtomicBool>> =
            Arc::new((0..tracks.len()).map(|_| AtomicBool::new(false)).collect());
        let delivered: Arc<Vec<Arc<Mutex<DeliveredSegmentTracker>>>> = Arc::new(
            (0..tracks.len())
                .map(|_| Arc::new(Mutex::new(DeliveredSegmentTracker::default())))
                .collect(),
        );
        let blacklist = SegmentBlacklist::new();
        let drm = Arc::new(AsyncMutex::new(drm));

        let run_result: Result<(), PlayerError> = async {
            loop {
                if playback.is_stopped() {
                    break;
                }
                if tracks.iter().all(|t| t.receiver_count() == 0) {
                    break;
                }

                playback.set_state(PlaybackState::LoadingManifest);

                let resp = client.get(manifest_uri.clone()).send().await?;
                let text = resp.text().await?;
                manifest = Some(dash_mpd::parse(&text)?);
                mpd_xml = Some(text);

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
                    utc_timing::wall_clock_utc(&client, mpd_ref, Some(&manifest_uri)).await;

                if let Some(seek) = playback.take_seek_target() {
                    seek_target_override = Some(seek);
                }

                let target_time = if let Some(seek) = seek_target_override {
                    Some(seek)
                } else {
                    manifest::target_presentation_time_at(mpd_ref, wall_now)?
                };

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
                    let segment_base_ctx = manifest::SegmentBaseContext {
                        manifest_uri: manifest_uri.clone(),
                        mpd_base_urls: mpd_ref.base_url.clone(),
                        period_base_urls: period.BaseURL.clone(),
                    };

                    let since_ast = manifest::since_availability_start_at(mpd_ref, wall_now)?;
                    let timeline_ctx = manifest::TimelineBuildContext {
                        is_dynamic,
                        period_window: current_window,
                        period_duration: period.duration,
                        media_presentation_duration: mpd_ref.mediaPresentationDuration,
                        time_shift_buffer_depth: mpd_ref.timeShiftBufferDepth,
                        since_availability_start: since_ast,
                    };

                    let adaptation_sets = select_adaptation_sets(&period, &track_selection);

                    let mut streams = Vec::new();
                    for (track_idx, selected) in adaptation_sets.into_iter().enumerate() {
                        if track_idx >= tracks.len() {
                            break;
                        }
                        let adaptation_set = selected.adaptation_set.clone();
                        let period_adaptation_index = selected.info.period_adaptation_index;

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

                        let period = period.clone();
                        streams.push(async move {
                            run_adaptation_stream(AdaptationStreamContext {
                                client,
                                segment_base_ctx,
                                target_time,
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
                            })
                            .await
                        });
                    }

                    for result in join_all(streams).await {
                        result?;
                    }

                    if playback.seek_generation() != seek_generation_at_start {
                        seek_interrupted = true;
                        reset_for_seek(&have_init, &delivered);
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
) {
    for flag in have_init.iter() {
        flag.store(false, Ordering::Release);
    }
    for tracker in delivered.iter() {
        if let Ok(mut t) = tracker.lock() {
            t.reset();
        }
    }
}
