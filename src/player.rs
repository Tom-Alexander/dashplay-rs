use bytes::Bytes;
use futures::future::join_all;
use futures_util::StreamExt;
use std::pin::Pin;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;

use super::media_player::{MediaPlayer, WidevineLicenseFetcher};
use super::metrics::TrackMetrics;
use super::playback_control::{PlaybackControlError, PlaybackController, PlaybackState};
use super::track_selection::{TrackInfo, TrackSelection};
use super::types::BufferFeedback;
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

    /// Configure deterministic audio and video adaptation-set selection.
    pub fn with_track_selection(self, selection: TrackSelection) -> Self {
        Self {
            media_player: self.media_player.with_track_selection(selection),
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
            loop_state,
            playback: _playback,
        } = self.media_player.start().await?;

        let (out_tx, out_rx) = mpsc::channel::<Result<Bytes, PlayerError>>(256);
        let senders: Vec<_> = tracks.iter().map(|t| t.tx.clone()).collect();

        let join = tokio::spawn(async move {
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
        let join = outputs.spawn();

        let mut outs = Vec::with_capacity(tracks.len());
        let senders = tracks.clone();
        for (i, t) in tracks.iter().enumerate() {
            outs.push(PlayerTrackOutput {
                track_index: i,
                mime_type: t.mime_type.clone(),
                info: t.info.clone(),
                rx: t.tx.subscribe(),
                tx: t.tx.clone(),
                buffer_feedback: t.buffer_feedback(),
                metrics: t.metrics(),
            });
        }

        Ok(PlayerTrackOutputs {
            tracks: outs,
            join,
            playback,
            senders,
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
    senders: Vec<PlayerTrack>,
}

impl PlayerTrackOutputs {
    /// Current playback lifecycle state.
    pub fn playback_state(&self) -> PlaybackState {
        self.playback.state()
    }

    /// Watch playback lifecycle state transitions.
    pub fn subscribe_playback_state(&self) -> watch::Receiver<PlaybackState> {
        self.playback.subscribe_state()
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
            senders,
        } = self;
        (tracks, senders, join)
    }
}

pub struct PlayerTrackOutput {
    pub track_index: usize,
    pub mime_type: Option<String>,
    /// Language, roles, codecs, accessibility, and other selected-track metadata.
    pub info: TrackInfo,
    rx: broadcast::Receiver<PlayerEvent>,
    tx: broadcast::Sender<PlayerEvent>,
    buffer_feedback: BufferFeedback,
    metrics: TrackMetrics,
}

impl PlayerTrackOutput {
    pub fn into_receiver(self) -> broadcast::Receiver<PlayerEvent> {
        self.rx
    }

    /// Report buffer occupancy for this track's adaptive bitrate controller.
    pub fn buffer_feedback(&self) -> BufferFeedback {
        self.buffer_feedback.clone()
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
