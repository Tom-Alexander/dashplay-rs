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
use super::manifest::{self, MimeType};
use super::segment_blacklist::SegmentBlacklist;
use super::types::PlayerEvent;
use super::utc_timing;

pub(crate) struct PlaybackLoopState {
    pub client: Client,
    pub manifest_uri: Url,
    pub drm: DrmSessionCoordinator,
}

impl PlaybackLoopState {
    pub async fn run(self, tracks: Vec<super::types::PlayerTrack>) -> Result<(), PlayerError> {
        let PlaybackLoopState {
            client,
            manifest_uri,
            drm,
            ..
        } = self;

        let mut manifest;
        let mut mpd_xml;
        let mut last_period_idx = None;

        let have_init: Arc<Vec<AtomicBool>> =
            Arc::new((0..tracks.len()).map(|_| AtomicBool::new(false)).collect());
        let delivered: Arc<Vec<Arc<Mutex<DeliveredSegmentTracker>>>> = Arc::new(
            (0..tracks.len())
                .map(|_| Arc::new(Mutex::new(DeliveredSegmentTracker::default())))
                .collect(),
        );
        let blacklist = SegmentBlacklist::new();
        let drm = Arc::new(AsyncMutex::new(drm));

        loop {
            if tracks.iter().all(|t| t.receiver_count() == 0) {
                break;
            }

            let resp = client.get(manifest_uri.clone()).send().await?;
            let text = resp.text().await?;
            manifest = Some(dash_mpd::parse(&text)?);
            mpd_xml = Some(text);

            let mpd_ref = manifest::mpd(&manifest)?;
            let min_update = mpd_ref
                .minimumUpdatePeriod
                .unwrap_or(std::time::Duration::ZERO);

            let is_dynamic = manifest::is_dynamic_mpd(mpd_ref);
            let wall_now = utc_timing::wall_clock_utc(&client, mpd_ref, Some(&manifest_uri)).await;
            let target_time = manifest::target_presentation_time_at(mpd_ref, wall_now)?;
            let period_windows = manifest::period_windows(mpd_ref)?;
            let periods_to_play: Vec<manifest::PeriodWindow> = if is_dynamic {
                vec![manifest::current_period_window_at(mpd_ref, wall_now)?]
            } else {
                period_windows
            };

            for current_window in periods_to_play {
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
                    let client = client.clone();
                    let segment_base_ctx = segment_base_ctx.clone();
                    let blacklist = blacklist.clone();
                    let drm = drm.clone();
                    let buffer_rx = tracks[aset_idx].buffer_rx.clone();
                    let delivered = delivered[aset_idx].clone();

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
                            delivered,
                            blacklist,
                            drm,
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
