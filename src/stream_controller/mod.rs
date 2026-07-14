//! Orchestrates manifest refresh, period selection, and parallel adaptation-set streams
//! (dash.js: `StreamController` coordinating multiple `Stream` instances).

mod manifest_loop;
mod mpd_events;
mod period_context;

use std::sync::Arc;

use futures::future::join_all;
use tokio::sync::Mutex as AsyncMutex;

use crate::platform::sleep;

use super::drm::DrmSessionCoordinator;

use super::PlayerError;
use super::abr::SharedAbrFactory;
use super::http::SharedHttpClient;
use super::manifest_lifecycle::ManifestSession;
use super::playback_control::{PlaybackController, PlaybackState};
use super::schedule::{AdaptationStreamContext, BufferTarget, run_adaptation_stream};
use super::segment_blacklist::SegmentBlacklist;
use super::track_selection::{SelectedAdaptationSet, TrackSelection};
use super::track_session::TrackSessionState;
use super::types::PlayerEvent;

use manifest_loop::{broadcast_manifest_loaded, manifest_tick, periods_to_play, refresh_manifest};
use mpd_events::MpdEventDedup;
use period_context::{
    PeriodContextInputs, TimelineContextInputs, build_period_context, build_timeline_context,
};

pub(crate) struct PlaybackLoopState {
    pub client: SharedHttpClient,
    pub manifest_uri: url::Url,
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
            mut track_selection,
            abr_factory,
        } = self;

        let mut manifest_session = ManifestSession::default();
        manifest_session.initialize(manifest_uri.clone());
        let mut last_period_idx = None;
        let mut seek_target_override: Option<std::time::Duration> = None;
        let mut mpd_event_dedup = MpdEventDedup::default();

        let track_sessions: Arc<Vec<Arc<TrackSessionState>>> = Arc::new(
            (0..tracks.len())
                .map(|_| Arc::new(TrackSessionState::default()))
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

                refresh_manifest(&mut manifest_session, &client, &manifest_uri).await?;
                let tick = manifest_tick(&manifest_session, &client).await?;
                broadcast_manifest_loaded(&tracks, tick.mpd);

                if let Some(selection) = playback.take_track_selection() {
                    track_selection = selection;
                }

                if let Some(seek) = playback.take_seek_target() {
                    seek_target_override = Some(seek);
                    let playhead = playback.presentation_time();
                    for t in &tracks {
                        let _ = t.tx.send(PlayerEvent::PlayheadUpdated {
                            presentation_time: playhead,
                        });
                    }
                }

                let periods_to_play = periods_to_play(tick.mpd, tick.is_dynamic, tick.wall_now)?;
                let seek_generation_at_start = playback.seek_generation();
                let mut seek_interrupted = false;

                let buffer_target = BufferTarget::from_mpd(tick.mpd);

                for current_window in periods_to_play {
                    if playback.is_stopped() {
                        break;
                    }

                    on_period_change(&mut last_period_idx, current_window.idx, &track_sessions);

                    drm.lock()
                        .await
                        .sync_from_mpd(tick.xml, current_window.idx)
                        .await?;
                    drm.lock().await.poll_renewals().await?;

                    let period = tick.mpd.periods[current_window.idx].clone();
                    mpd_event_dedup.emit_new_events(&period, &tracks);

                    let (period_ctx, adaptation_sets) =
                        build_period_context(PeriodContextInputs {
                            mpd: tick.mpd,
                            wall_now: tick.wall_now,
                            current_window,
                            period: &period,
                            manifest_uri: &tick.active_manifest_uri,
                            steering: tick.steering,
                            seek_target_override,
                            track_selection: &track_selection,
                            track_sessions: &track_sessions,
                        })?;

                    apply_track_selection_updates(&tracks, &adaptation_sets);

                    let mut streams = Vec::new();
                    for (track_idx, selected) in adaptation_sets.into_iter().enumerate() {
                        if track_idx >= tracks.len() {
                            break;
                        }
                        let adaptation_set = selected.adaptation_set.clone();
                        let period_adaptation_index = selected.info.period_adaptation_index;
                        let timeline_ctx = build_timeline_context(TimelineContextInputs {
                            mpd: tick.mpd,
                            wall_now: tick.wall_now,
                            is_dynamic: tick.is_dynamic,
                            period_ctx: &period_ctx,
                            current_window,
                            period: &period,
                            adaptation_set: &adaptation_set,
                            track_idx,
                            track_sessions: &track_sessions,
                        });

                        let tx = tracks[track_idx].tx.clone();
                        let session = track_sessions[track_idx].clone();
                        let client = client.clone();
                        let segment_base_ctx = period_ctx.segment_base_ctx.clone();
                        let blacklist = blacklist.clone();
                        let drm = drm.clone();
                        let buffer_rx = tracks[track_idx].buffer_rx.clone();
                        let metrics = tracks[track_idx].metrics.clone();
                        let playback = playback.clone();
                        let abr_factory = abr_factory.clone();
                        let prt_reference_id = period_ctx.prt_reference_id.clone();
                        let template_end_numbers = tick.template_end_numbers.clone();
                        let period_idx = current_window.idx;

                        let period = period.clone();
                        streams.push(async move {
                            run_adaptation_stream(AdaptationStreamContext {
                                client,
                                segment_base_ctx,
                                target_time: period_ctx.period_target_time,
                                period_start: current_window.start,
                                period,
                                timeline_ctx,
                                template_end_numbers,
                                period_idx,
                                adaptation_set,
                                track_idx,
                                period_adaptation_index,
                                tx,
                                session,
                                blacklist,
                                drm,
                                buffer_rx,
                                buffer_target,
                                metrics,
                                playback,
                                abr_factory,
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
                        reset_track_sessions(&track_sessions);
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

                if tick.min_update_period.is_zero() {
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

                sleep(tick.min_update_period).await;
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

fn on_period_change(
    last_period_idx: &mut Option<usize>,
    current_idx: usize,
    track_sessions: &Arc<Vec<Arc<TrackSessionState>>>,
) {
    if *last_period_idx == Some(current_idx) {
        return;
    }
    *last_period_idx = Some(current_idx);
    reset_track_sessions(track_sessions);
}

fn reset_track_sessions(track_sessions: &Arc<Vec<Arc<TrackSessionState>>>) {
    for session in track_sessions.iter() {
        session.reset();
    }
}

fn apply_track_selection_updates(
    tracks: &[super::types::PlayerTrack],
    adaptation_sets: &[SelectedAdaptationSet<'_>],
) {
    for (track_idx, selected) in adaptation_sets.iter().enumerate() {
        if track_idx >= tracks.len() {
            break;
        }
        let previous = tracks[track_idx].info();
        if previous == selected.info {
            continue;
        }
        tracks[track_idx].replace_track_info(selected.info.clone());
        let _ = tracks[track_idx].tx.send(PlayerEvent::TrackChanged {
            info: selected.info.clone(),
        });
    }
}
