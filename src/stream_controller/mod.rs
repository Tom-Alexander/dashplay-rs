//! Orchestrates manifest refresh, period selection, and parallel adaptation-set streams
//! (dash.js: `StreamController` coordinating multiple `Stream` instances).

mod manifest_loop;
mod mpd_events;
mod period_context;
mod sync_prefetch;

use std::sync::Arc;

use futures::future::join_all;
use tokio::sync::Mutex as AsyncMutex;

use crate::platform::sleep;
use dash_mpd::AdaptationSet;

use super::drm::DrmSessionCoordinator;

use super::PlayerError;
use super::abr::SharedAbrFactory;
use super::clock::latency_control::LatencyPolicy;
use super::http::SharedHttpClient;
use super::manifest::{self, PeriodLink};
use super::manifest_lifecycle::{ManifestSession, SteeringSyncHints, next_refresh_sleep};
use super::playback_control::{PlaybackController, PlaybackState};
use super::schedule::{AdaptationStreamContext, BufferTarget, run_adaptation_stream};
use super::segment_blacklist::SegmentBlacklist;
use super::track_selection::{SelectedAdaptationSet, TrackSelection};
use super::track_session::TrackSessionState;
use super::types::{PeriodTransitionKind, PlayerEvent};

use manifest_loop::{
    broadcast_manifest_loaded, broadcast_manifest_patch_failed, manifest_tick, periods_to_play,
    refresh_manifest, should_end_after_tick,
};
use mpd_events::MpdEventDedup;
use period_context::{
    PeriodContextInputs, TimelineContextInputs, build_period_context, build_timeline_context,
};
use sync_prefetch::{SyncPrefetchInputs, build_sync_prefetch_plans};

pub(crate) struct PlaybackLoopState {
    pub client: SharedHttpClient,
    pub manifest_uri: url::Url,
    pub drm: DrmSessionCoordinator,
    pub playback: PlaybackController,
    pub track_selection: TrackSelection,
    pub abr_factory: SharedAbrFactory,
    pub cmcd: Option<crate::cmcd::CmcdSession>,
    pub http_retry: crate::http::HttpRetryConfig,
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
            cmcd,
            http_retry,
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

                let steering_hints = steering_sync_hints(&tracks, &manifest_session);
                let refresh = refresh_manifest(
                    &mut manifest_session,
                    &client,
                    &manifest_uri,
                    cmcd.as_ref(),
                    &http_retry,
                    &steering_hints,
                )
                .await?;
                if let Some(reason) = refresh.patch_fallback.as_deref() {
                    broadcast_manifest_patch_failed(&tracks, reason);
                }
                let tick = manifest_tick(&manifest_session, &client).await?;
                broadcast_manifest_loaded(&tracks, tick.mpd, tick.xml);

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

                let buffer_target = BufferTarget::from_mpd(tick.mpd);
                playback.set_min_buffer_s(buffer_target.min_buffer_s);
                let periods_to_play = periods_to_play(
                    tick.mpd,
                    tick.is_dynamic,
                    tick.wall_now,
                    buffer_target.min_buffer_s,
                )?;
                let seek_generation_at_start = playback.seek_generation();
                let mut seek_interrupted = false;
                let mut held_prefetch: Vec<Vec<PlayerEvent>> =
                    (0..tracks.len()).map(|_| Vec::new()).collect();

                for (window_pos, current_window) in periods_to_play.iter().copied().enumerate() {
                    if playback.is_stopped() {
                        break;
                    }

                    let link = last_period_idx
                        .map(|prev| manifest::period_link(tick.mpd, prev, current_window.idx))
                        .unwrap_or(PeriodLink::Discontinuous);
                    let transition = period_transition_kind(link);
                    let entering_new_period = last_period_idx != Some(current_window.idx);

                    if entering_new_period {
                        let gap_before = manifest::gap_before_period(tick.mpd, current_window.idx);
                        emit_period_changed(&tracks, current_window, transition, gap_before);
                        on_period_change(
                            &mut last_period_idx,
                            current_window.idx,
                            &track_sessions,
                            link.allows_soft_transition(),
                        );
                        for (track_idx, events) in held_prefetch.iter_mut().enumerate() {
                            if track_idx >= tracks.len() {
                                break;
                            }
                            for event in events.drain(..) {
                                let _ = tracks[track_idx].tx.send(event);
                            }
                        }
                    }

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

                    playback.set_latency_control(
                        LatencyPolicy::from_mpd(tick.mpd),
                        period_ctx.reference_since_ast(),
                    );

                    apply_track_selection_updates(&tracks, &adaptation_sets);

                    let next_window = periods_to_play
                        .get(window_pos + 1)
                        .copied()
                        .or_else(|| peek_next_period_window(tick.mpd, current_window.idx));
                    let sync_plans = next_window.and_then(|next_win| {
                        build_sync_prefetch_plans(SyncPrefetchInputs {
                            mpd: tick.mpd,
                            current_window,
                            next_window: next_win,
                            sync_depth_s: buffer_target.min_buffer_s,
                            current_sets: &adaptation_sets,
                            period_ctx: &period_ctx,
                            wall_now: tick.wall_now,
                            is_dynamic: tick.is_dynamic,
                            template_end_numbers: tick.template_end_numbers.as_ref(),
                            track_sessions: &track_sessions,
                        })
                    });

                    let mut streams = Vec::new();
                    for (track_idx, selected) in adaptation_sets.into_iter().enumerate() {
                        if track_idx >= tracks.len() {
                            break;
                        }
                        let adaptation_set = selected.adaptation_set.clone();
                        let period_adaptation_index = selected.info.period_adaptation_index;
                        let switch_peers: std::collections::HashMap<usize, AdaptationSet> =
                            selected
                                .switch_peers
                                .iter()
                                .map(|(idx, aset)| (*idx, (*aset).clone()))
                                .collect();
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
                        let buffer_tx = tracks[track_idx].buffer_tx.clone();
                        let metrics = tracks[track_idx].metrics.clone();
                        let track_kind = tracks[track_idx].info().kind;
                        let playback = playback.clone();
                        let abr_factory = abr_factory.clone();
                        let cmcd_for_track = cmcd.clone();
                        let http_retry_for_track = http_retry.clone();
                        let prt_reference_id = period_ctx.prt_reference_id.clone();
                        let operating =
                            crate::clock::service_description::OperatingConstraints::from_mpd(
                                tick.mpd,
                            );
                        let media_type = adaptation_set.contentType.as_deref().or_else(|| {
                            adaptation_set
                                .mimeType
                                .as_deref()
                                .and_then(|m| m.split('/').next())
                        });
                        let operating_constraints = operating
                            .as_ref()
                            .map(|ops| ops.resolve_for_media(media_type))
                            .filter(|c| !c.is_empty());
                        let template_end_numbers = tick.template_end_numbers.clone();
                        let random_access = tick.random_access.clone();
                        let period_idx = current_window.idx;
                        let sync_prefetch = sync_plans
                            .as_ref()
                            .and_then(|plans| plans.get(&track_idx).cloned());

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
                                random_access,
                                period_idx,
                                adaptation_set,
                                switch_peers,
                                track_idx,
                                period_adaptation_index,
                                tx,
                                session,
                                blacklist,
                                drm,
                                buffer_rx,
                                buffer_tx,
                                buffer_target,
                                metrics,
                                playback,
                                abr_factory,
                                prt_reference_id,
                                operating_constraints,
                                cmcd: cmcd_for_track,
                                http_retry: http_retry_for_track,
                                track_kind,
                                sync_prefetch,
                            })
                            .await
                        });
                    }

                    for result in join_all(streams).await {
                        let prefetch = result?;
                        if let Some((track_idx, events)) = prefetch
                            && track_idx < held_prefetch.len()
                        {
                            held_prefetch[track_idx] = events;
                        }
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

                if should_end_after_tick(
                    tick.mpd,
                    tick.is_dynamic,
                    tick.wall_now,
                    tick.min_update_period,
                )? {
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

                if tracks.iter().all(|t| t.receiver_count() == 0) {
                    break;
                }

                sleep(next_refresh_sleep(
                    tick.min_update_period,
                    tick.steering.ttl_remaining(),
                ))
                .await;
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

fn steering_sync_hints(
    tracks: &[super::types::PlayerTrack],
    session: &ManifestSession,
) -> SteeringSyncHints {
    let pathway = session.steering.current_pathway().map(str::to_string);
    let throughput_bps = tracks
        .iter()
        .map(|t| t.metrics.snapshot().throughput_bps)
        .filter(|bps| bps.is_finite() && *bps > 0.0)
        .map(|bps| bps.round() as u64)
        .max();
    SteeringSyncHints {
        pathway,
        throughput_bps,
    }
}

fn emit_period_changed(
    tracks: &[super::types::PlayerTrack],
    window: manifest::PeriodWindow,
    transition: PeriodTransitionKind,
    gap_before: Option<std::time::Duration>,
) {
    for t in tracks {
        let _ = t.tx.send(PlayerEvent::PeriodChanged {
            period_index: window.idx,
            start: window.start,
            end: window.end,
            transition,
            gap_before,
        });
    }
}

fn period_transition_kind(link: PeriodLink) -> PeriodTransitionKind {
    match link {
        PeriodLink::Continuous => PeriodTransitionKind::Continuous,
        PeriodLink::Connected => PeriodTransitionKind::Connected,
        PeriodLink::Discontinuous => PeriodTransitionKind::Discontinuous,
    }
}

fn peek_next_period_window(
    mpd: &dash_mpd::MPD,
    current_idx: usize,
) -> Option<manifest::PeriodWindow> {
    let windows = manifest::period_windows(mpd).ok()?;
    windows.into_iter().find(|w| w.idx == current_idx + 1)
}

fn on_period_change(
    last_period_idx: &mut Option<usize>,
    current_idx: usize,
    track_sessions: &Arc<Vec<Arc<TrackSessionState>>>,
    soft: bool,
) {
    if *last_period_idx == Some(current_idx) {
        return;
    }
    *last_period_idx = Some(current_idx);
    if soft {
        for session in track_sessions.iter() {
            session.soft_reset();
        }
    } else {
        reset_track_sessions(track_sessions);
    }
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
