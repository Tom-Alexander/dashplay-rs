//! Orchestrates manifest refresh, period selection, and parallel adaptation-set streams
//! (dash.js: `StreamController` coordinating multiple `Stream` instances).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::future::join_all;
use reqwest::Client;
use tokio::time::sleep;
use url::Url;

use super::drm::License;

use super::PlayerError;
use super::dash_stream::{AdaptationStreamContext, run_adaptation_stream};
use super::manifest::{self, MimeType};
use super::segment_blacklist::SegmentBlacklist;
use super::types::PlayerEvent;
use super::utc_timing;

pub(crate) struct PlaybackLoopState {
    pub client: Client,
    pub manifest_uri: Url,
    pub manifest: Option<dash_mpd::MPD>,
    pub adaptation_wv_sessions: Vec<Option<Arc<License>>>,
    pub adaptation_wv_sessions_by_rep: Vec<HashMap<String, Arc<License>>>,
    pub(crate) last_period_idx: Option<usize>,
}

impl PlaybackLoopState {
    pub async fn fetch_manifest(&mut self) -> Result<(), PlayerError> {
        let mpd = manifest::fetch_mpd(&self.client, &self.manifest_uri).await?;
        self.manifest = Some(mpd);
        Ok(())
    }

    pub async fn run(mut self, tracks: Vec<super::types::PlayerTrack>) -> Result<(), PlayerError> {
        let have_init: Arc<Vec<AtomicBool>> =
            Arc::new((0..tracks.len()).map(|_| AtomicBool::new(false)).collect());
        let blacklist = SegmentBlacklist::new();

        loop {
            if tracks.iter().all(|t| t.receiver_count() == 0) {
                break;
            }

            self.fetch_manifest().await?;

            let mpd_ref = manifest::mpd(&self.manifest)?;
            let min_update = mpd_ref
                .minimumUpdatePeriod
                .unwrap_or(std::time::Duration::ZERO);

            let is_dynamic = manifest::is_dynamic_mpd(mpd_ref);
            let wall_now =
                utc_timing::wall_clock_utc(&self.client, mpd_ref, Some(&self.manifest_uri)).await;
            let target_time = manifest::target_presentation_time_at(mpd_ref, wall_now)?;
            let period_windows = manifest::period_windows(mpd_ref)?;
            let periods_to_play: Vec<manifest::PeriodWindow> = if is_dynamic {
                vec![manifest::current_period_window_at(mpd_ref, wall_now)?]
            } else {
                period_windows
            };

            for current_window in periods_to_play {
                if self.last_period_idx != Some(current_window.idx) {
                    for flag in have_init.iter() {
                        flag.store(false, Ordering::Release);
                    }
                }
                self.last_period_idx = Some(current_window.idx);
                let period_start = current_window.start;
                let period = mpd_ref.periods[current_window.idx].clone();
                let segment_base_ctx = manifest::SegmentBaseContext {
                    manifest_uri: self.manifest_uri.clone(),
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

                let adaptation_sets: Vec<dash_mpd::AdaptationSet> = period
                    .adaptations
                    .iter()
                    .filter(|adaptation_set| {
                        let mime = adaptation_set.mimeType.as_deref();
                        matches!(
                            mime,
                            Some(m) if m == MimeType::Audio.as_str()
                                || m == MimeType::Video.as_str()
                        )
                    })
                    .cloned()
                    .collect();

                let mut tasks = Vec::new();
                for (aset_idx, adaptation_set) in adaptation_sets.into_iter().enumerate() {
                    if aset_idx >= tracks.len() {
                        break;
                    }

                    let tx = tracks[aset_idx].tx.clone();
                    let have_init = have_init.clone();
                    let client = self.client.clone();
                    let segment_base_ctx = segment_base_ctx.clone();
                    let blacklist = blacklist.clone();
                    let license = self
                        .adaptation_wv_sessions
                        .get(aset_idx)
                        .and_then(|x| x.clone());
                    let wv_by_rep = self
                        .adaptation_wv_sessions_by_rep
                        .get(aset_idx)
                        .cloned()
                        .unwrap_or_default();
                    let buffer_rx = tracks[aset_idx].buffer_rx.clone();

                    let period = period.clone();
                    tasks.push(tokio::spawn(async move {
                        run_adaptation_stream(AdaptationStreamContext {
                            client,
                            segment_base_ctx,
                            target_time,
                            period_start,
                            period,
                            timeline_ctx,
                            adaptation_set,
                            aset_idx,
                            tx,
                            have_init,
                            blacklist,
                            license,
                            wv_by_rep,
                            buffer_rx,
                        })
                        .await
                    }));
                }

                let results = join_all(tasks).await;
                for inner in results.into_iter().filter_map(Result::ok) {
                    inner?;
                }
            }

            if min_update.is_zero() {
                for t in &tracks {
                    let _ = t.tx.send(PlayerEvent::End);
                }
                break;
            }

            sleep(min_update).await;
        }

        Ok(())
    }
}
