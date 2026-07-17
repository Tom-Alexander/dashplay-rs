use bytes::Bytes;
use futures::future::join_all;
use futures_util::StreamExt;
use std::pin::Pin;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;

use super::abr::SharedAbrFactory;
use super::cmcd::CmcdConfig;
use super::http::{HttpRetryConfig, SharedHttpClient};
use super::manifest::ManifestMetadata;
use super::media_player::{MediaPlayer, WidevineLicenseFetcher};
use super::metrics::TrackMetrics;
use super::playback_control::{PlaybackControlError, PlaybackController, PlaybackState};
use super::track_selection::{TrackInfo, TrackSelection};
use super::types::{BufferFeedback, PlaybackQualityFeedback};
use super::{PlayerError, PlayerEvent, PlayerOutputs, PlayerTrack};

type MergedByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

pub struct Player {
    media_player: MediaPlayer,
}

impl Player {
    pub fn new(manifest_uri: &str, license_uri: Option<&str>) -> Result<Self, PlayerError> {
        Ok(Self {
            media_player: MediaPlayer::new(manifest_uri, license_uri)?,
        })
    }

    /// Same as [`MediaPlayer::with_license_fetcher`]: custom Widevine license HTTP handling.
    pub fn with_license_fetcher(self, fetcher: WidevineLicenseFetcher) -> Self {
        Self {
            media_player: self.media_player.with_license_fetcher(fetcher),
        }
    }

    /// Configure deterministic audio, video, and text adaptation-set selection.
    pub fn with_track_selection(self, selection: TrackSelection) -> Self {
        Self {
            media_player: self.media_player.with_track_selection(selection),
        }
    }

    /// Use a custom [`HttpClient`](crate::HttpClient) for manifest, segment, and clock-sync requests.
    pub fn with_http_client(self, client: SharedHttpClient) -> Self {
        Self {
            media_player: self.media_player.with_http_client(client),
        }
    }

    /// Use a custom [`AbrFactory`](crate::AbrFactory) for representation selection.
    pub fn with_abr_factory(self, factory: SharedAbrFactory) -> Self {
        Self {
            media_player: self.media_player.with_abr_factory(factory),
        }
    }

    /// Constrain ABR selection (min/max bitrate, fixed quality, data-saver).
    pub fn with_quality_constraints(self, constraints: crate::QualityConstraints) -> Self {
        Self {
            media_player: self.media_player.with_quality_constraints(constraints),
        }
    }

    /// Enable CTA-5004 CMCD request headers and CTA-5006 CMSD response parsing.
    pub fn with_cmcd(self, config: CmcdConfig) -> Self {
        Self {
            media_player: self.media_player.with_cmcd(config),
        }
    }

    /// Configure fixed-delay HTTP retry for transient failures (dash.js-style).
    pub fn with_http_retry(self, config: HttpRetryConfig) -> Self {
        Self {
            media_player: self.media_player.with_http_retry(config),
        }
    }

    /// Start the underlying `MediaPlayer` and return a **single merged byte stream**.
    ///
    /// Spawns one Tokio task that runs the stream controller and forwards all track events
    /// into the merged output channel. See [`PlayerOutputs::spawn`] for the lower-level contract.
    ///
    /// Notes:
    /// - Each adaptation set emits `Init` then `Segment` fragments (decrypted when DRM is present).
    /// - This merged stream simply forwards fragment bytes in arrival order.
    /// - If you need per-track separation (e.g. audio + video as separate inputs), use
    ///   `start_tracks()` instead.
    pub async fn start_merged(self) -> Result<PlayerMergedOutput, PlayerError> {
        let PlayerOutputs {
            tracks,
            is_dynamic: _,
            loop_state,
            playback: _playback,
            manifest_metadata: _,
        } = self.media_player.start().await?;

        let (out_tx, out_rx) = mpsc::channel::<Result<Bytes, PlayerError>>(256);
        let senders: Vec<_> = tracks.iter().map(|t| t.tx.clone()).collect();

        let join = crate::platform::spawn(async move {
            let mut forwarders = Vec::with_capacity(tracks.len());
            for t in &tracks {
                let mut rx = t.tx.subscribe();
                let out_tx = out_tx.clone();
                forwarders.push(async move {
                    loop {
                        match rx.recv().await {
                            Ok(PlayerEvent::Init(data)) => {
                                if out_tx.send(Ok(data)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(PlayerEvent::Segment { data, .. }) => {
                                if out_tx.send(Ok(data)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(PlayerEvent::End)
                            | Ok(PlayerEvent::PlaybackEnded)
                            | Ok(PlayerEvent::Error(_)) => break,
                            Ok(_) => {}
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
            }
            drop(out_tx);

            let (_, loop_result) = tokio::join!(join_all(forwarders), loop_state.run(tracks));
            loop_result
        });

        Ok(PlayerMergedOutput {
            stream: ReceiverStream::new(out_rx),
            join,
            _tracks: senders,
        })
    }

    /// Start the underlying `MediaPlayer` and return one stream per track.
    ///
    /// Spawns one Tokio task for the stream controller loop via [`PlayerOutputs::spawn`].
    /// Use [`MediaPlayer::start`] and [`PlayerOutputs::run`] directly when you want the
    /// caller to own the async task.
    pub async fn start_tracks(self) -> Result<PlayerTrackOutputs, PlayerError> {
        let outputs = self.media_player.start().await?;
        let tracks = outputs.tracks.clone();
        let playback = outputs.playback.clone();
        let cmcd = outputs.loop_state.cmcd.clone();
        let is_dynamic = outputs.is_dynamic;
        let manifest_metadata = outputs.manifest_metadata.clone();
        let join = outputs.spawn();

        let mut outs = Vec::with_capacity(tracks.len());
        let senders = tracks.clone();
        for (i, t) in tracks.iter().enumerate() {
            outs.push(PlayerTrackOutput {
                track_index: i,
                rx: t.tx.subscribe(),
                tx: t.tx.clone(),
                buffer_feedback: t.buffer_feedback(),
                playback_quality_feedback: t.playback_quality_feedback(),
                metrics: t.metrics(),
                track: t.clone(),
            });
        }

        Ok(PlayerTrackOutputs {
            tracks: outs,
            join,
            playback,
            is_dynamic,
            manifest_metadata,
            senders,
            cmcd,
        })
    }
}

pub struct PlayerMergedOutput {
    /// Merged stream of init + media fragments.
    pub stream: ReceiverStream<Result<Bytes, PlayerError>>,
    /// Join handle for the playback task (stream controller + event forwarding).
    pub join: JoinHandle<Result<(), PlayerError>>,
    // Hold senders so broadcasts don't close prematurely.
    _tracks: Vec<broadcast::Sender<PlayerEvent>>,
}

impl PlayerMergedOutput {
    /// Convert the merged fragment stream into an `AsyncRead` for piping into a child process
    /// (e.g. `ffmpeg -i pipe:0 ...`).
    pub fn into_async_read(self) -> PlayerMergedAsyncRead {
        let s = self.stream.map(|res| match res {
            Ok(b) => Ok(b),
            Err(e) => Err(std::io::Error::other(e)),
        });
        PlayerMergedAsyncRead {
            reader: tokio_util::io::StreamReader::new(Box::pin(s)),
            join: self.join,
            _tracks: self._tracks,
        }
    }
}

pub struct PlayerMergedAsyncRead {
    pub reader: tokio_util::io::StreamReader<MergedByteStream, Bytes>,
    pub join: JoinHandle<Result<(), PlayerError>>,
    _tracks: Vec<broadcast::Sender<PlayerEvent>>,
}

pub struct PlayerTrackOutputs {
    pub tracks: Vec<PlayerTrackOutput>,
    /// Join handle for the stream controller task spawned by [`Player::start_tracks`].
    pub join: JoinHandle<Result<(), PlayerError>>,
    /// Seek, pause, resume, stop, and lifecycle state for this session.
    pub playback: PlaybackController,
    /// Whether the loaded MPD is dynamic (live / sliding window).
    pub is_dynamic: bool,
    /// Descriptive metadata from the initially loaded MPD.
    pub manifest_metadata: ManifestMetadata,
    senders: Vec<PlayerTrack>,
    cmcd: Option<crate::cmcd::CmcdSession>,
}

impl PlayerTrackOutputs {
    /// Latest CMSD snapshot from any request in this session, when CMCD is enabled.
    ///
    /// CMSD is observational and does not influence ABR or scheduling.
    pub fn last_cmsd(&self) -> Option<crate::cmcd::CmsdSnapshot> {
        self.cmcd.as_ref().and_then(|session| session.last_cmsd())
    }

    /// Current playback lifecycle state.
    pub fn playback_state(&self) -> PlaybackState {
        self.playback.state()
    }

    /// Watch playback lifecycle state transitions.
    pub fn subscribe_playback_state(&self) -> watch::Receiver<PlaybackState> {
        self.playback.subscribe_state()
    }

    /// Current presentation time (seconds from the start of the presentation).
    pub fn presentation_time(&self) -> Option<std::time::Duration> {
        self.playback.presentation_time()
    }

    /// Watch presentation time updates.
    pub fn subscribe_presentation_time(&self) -> watch::Receiver<Option<std::time::Duration>> {
        self.playback.subscribe_presentation_time()
    }

    /// Suggested LL-DASH consumption rate (`1.0` when inactive).
    pub fn suggested_playback_rate(&self) -> f64 {
        self.playback.suggested_playback_rate()
    }

    /// Watch suggested LL-DASH consumption rate updates.
    pub fn subscribe_suggested_playback_rate(&self) -> watch::Receiver<f64> {
        self.playback.subscribe_suggested_playback_rate()
    }

    /// Effective consumption rate (user override + LL catch-up, capped by `@maxPlayoutRate`).
    pub fn playback_rate(&self) -> f64 {
        self.playback.playback_rate()
    }

    /// Set or clear a user playback-rate override.
    ///
    /// See [`PlaybackController::set_playback_rate`].
    pub fn set_playback_rate(&self, rate: Option<f64>) -> Result<(), PlaybackControlError> {
        self.playback.set_playback_rate(rate)
    }

    /// Measured live latency when LL-DASH latency control is active.
    pub fn live_latency(&self) -> Option<std::time::Duration> {
        self.playback.live_latency()
    }

    /// Watch live latency updates.
    pub fn subscribe_live_latency(&self) -> watch::Receiver<Option<std::time::Duration>> {
        self.playback.subscribe_live_latency()
    }

    /// Suspend segment delivery until [`Self::resume`].
    pub fn pause(&self) -> Result<(), PlaybackControlError> {
        self.playback.pause()
    }

    /// Resume delivery after [`Self::pause`].
    pub fn resume(&self) -> Result<(), PlaybackControlError> {
        self.playback.resume()
    }

    /// Seek to a presentation time (seconds from the start of the presentation).
    pub fn seek(&self, presentation_time: std::time::Duration) -> Result<(), PlaybackControlError> {
        self.playback.seek(presentation_time)
    }

    /// Change adaptation-set preferences without restarting playback.
    ///
    /// See [`PlaybackController::set_track_selection`].
    pub fn set_track_selection(
        &self,
        selection: TrackSelection,
    ) -> Result<(), PlaybackControlError> {
        self.playback.set_track_selection(selection)
    }

    /// Current user ABR quality constraints.
    pub fn quality_constraints(&self) -> crate::QualityConstraints {
        self.playback.quality_constraints()
    }

    /// Update user ABR quality constraints.
    ///
    /// See [`PlaybackController::set_quality_constraints`].
    pub fn set_quality_constraints(
        &self,
        constraints: crate::QualityConstraints,
    ) -> Result<(), PlaybackControlError> {
        self.playback.set_quality_constraints(constraints)
    }

    /// Pin a ladder index and disable autoswitch.
    ///
    /// See [`PlaybackController::set_quality_for`].
    pub fn set_quality_for(&self, quality_index: usize) -> Result<(), PlaybackControlError> {
        self.playback.set_quality_for(quality_index)
    }

    /// Enable or disable automatic quality switching.
    ///
    /// See [`PlaybackController::set_auto_switch_bitrate`].
    pub fn set_auto_switch_bitrate(&self, enabled: bool) -> Result<(), PlaybackControlError> {
        self.playback.set_auto_switch_bitrate(enabled)
    }

    /// Stop playback. No further segments are delivered.
    pub fn stop(&self) -> Result<(), PlaybackControlError> {
        self.playback.stop()
    }

    pub fn track_count(&self) -> usize {
        self.senders.len()
    }

    pub fn subscribe(&self, idx: usize) -> Option<broadcast::Receiver<PlayerEvent>> {
        self.senders.get(idx).map(|t| t.subscribe())
    }

    /// Buffer feedback handle for a track (same index as [`Self::tracks`] / [`Self::subscribe`]).
    pub fn buffer_feedback(&self, idx: usize) -> Option<BufferFeedback> {
        self.senders.get(idx).map(|t| t.buffer_feedback())
    }

    /// Playback-quality feedback handle for a track (dropped-frame ABR input).
    pub fn playback_quality_feedback(&self, idx: usize) -> Option<PlaybackQualityFeedback> {
        self.senders.get(idx).map(|t| t.playback_quality_feedback())
    }

    /// Metrics handle for a track (same index as [`Self::tracks`] / [`Self::subscribe`]).
    pub fn metrics(&self, idx: usize) -> Option<TrackMetrics> {
        self.senders.get(idx).map(|t| t.metrics())
    }

    pub fn into_parts(
        self,
    ) -> (
        Vec<PlayerTrackOutput>,
        Vec<PlayerTrack>,
        JoinHandle<Result<(), PlayerError>>,
    ) {
        let Self {
            tracks,
            join,
            playback: _playback,
            is_dynamic: _,
            manifest_metadata: _,
            senders,
            cmcd: _,
        } = self;
        (tracks, senders, join)
    }
}

pub struct PlayerTrackOutput {
    pub track_index: usize,
    rx: broadcast::Receiver<PlayerEvent>,
    tx: broadcast::Sender<PlayerEvent>,
    buffer_feedback: BufferFeedback,
    playback_quality_feedback: PlaybackQualityFeedback,
    metrics: TrackMetrics,
    track: PlayerTrack,
}

impl PlayerTrackOutput {
    /// `AdaptationSet@mimeType` when present (e.g. `video/mp4`, `audio/mp4`).
    ///
    /// Reflects mid-playback switches; prefer this over any value captured at start.
    pub fn mime_type(&self) -> Option<String> {
        self.track.mime_type()
    }

    /// Language, roles, codecs, accessibility, and other selected-track metadata.
    ///
    /// Updated after mid-playback track switching.
    pub fn info(&self) -> TrackInfo {
        self.track.info()
    }

    pub fn into_receiver(self) -> broadcast::Receiver<PlayerEvent> {
        self.rx
    }

    /// Report buffer occupancy for this track's adaptive bitrate controller.
    pub fn buffer_feedback(&self) -> BufferFeedback {
        self.buffer_feedback.clone()
    }

    /// Report decoder / MSE dropped-frame counters for this track's ABR rule.
    pub fn playback_quality_feedback(&self) -> PlaybackQualityFeedback {
        self.playback_quality_feedback.clone()
    }

    /// Playback metrics for this track.
    pub fn metrics(&self) -> TrackMetrics {
        self.metrics.clone()
    }

    /// Stream events for a single adaptation set (init + segments).
    ///
    /// Creates a new broadcast subscription, equivalent to
    /// [`PlayerTrackOutputs::subscribe`] for this track index.
    pub fn events(
        &self,
    ) -> impl Stream<
        Item = Result<PlayerEvent, tokio_stream::wrappers::errors::BroadcastStreamRecvError>,
    > + '_ {
        tokio_stream::wrappers::BroadcastStream::new(self.tx.subscribe())
    }
}
