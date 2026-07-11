use bytes::Bytes;
use futures_util::StreamExt;
use std::pin::Pin;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_stream::Stream;
use tokio_stream::wrappers::ReceiverStream;

use super::media_player::{MediaPlayer, WidevineLicenseFetcher};
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

    /// Start the underlying `MediaPlayer` and return a **single merged byte stream**.
    ///
    /// Notes:
    /// - Each adaptation set emits `Init` then `Segment` fragments (decrypted when DRM is present).
    /// - This merged stream simply forwards fragment bytes in arrival order.
    /// - If you need per-track separation (e.g. audio + video as separate inputs), use
    ///   `start_tracks()` instead.
    #[allow(dead_code)]
    pub async fn start_merged(self) -> Result<PlayerMergedOutput, PlayerError> {
        let PlayerOutputs { tracks, join } = self.media_player.start().await?;

        let (out_tx, out_rx) = mpsc::channel::<Result<Bytes, PlayerError>>(256);
        let mut forwarders: Vec<JoinHandle<()>> = Vec::with_capacity(tracks.len());

        for t in &tracks {
            let mut rx = t.tx.subscribe();
            let out_tx = out_tx.clone();
            forwarders.push(tokio::spawn(async move {
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
                        Ok(PlayerEvent::End) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }));
        }

        // If the receiver is dropped, all `out_tx` clones will fail and forwarders will exit.
        drop(out_tx);

        Ok(PlayerMergedOutput {
            stream: ReceiverStream::new(out_rx),
            join,
            _tracks: tracks.into_iter().map(|t| t.tx).collect::<Vec<_>>(),
            _forwarders: forwarders,
        })
    }

    /// Start the underlying `MediaPlayer` and return one stream per track.
    pub async fn start_tracks(self) -> Result<PlayerTrackOutputs, PlayerError> {
        let PlayerOutputs { tracks, join } = self.media_player.start().await?;

        let mut outs = Vec::with_capacity(tracks.len());
        let senders = tracks.clone();
        for (i, t) in tracks.iter().enumerate() {
            outs.push(PlayerTrackOutput {
                track_index: i,
                mime_type: t.mime_type.clone(),
                rx: t.tx.subscribe(),
            });
        }

        Ok(PlayerTrackOutputs {
            tracks: outs,
            join,
            senders,
        })
    }
}

#[allow(dead_code)]
pub struct PlayerMergedOutput {
    /// Merged stream of init + media fragments.
    pub stream: ReceiverStream<Result<Bytes, PlayerError>>,
    /// Join handle for the underlying stream-controller task.
    pub join: JoinHandle<Result<(), PlayerError>>,
    // Hold senders so broadcasts don't close prematurely.
    _tracks: Vec<broadcast::Sender<PlayerEvent>>,
    // Hold forwarders so they live as long as the output.
    _forwarders: Vec<JoinHandle<()>>,
}

#[allow(dead_code)]
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
            _forwarders: self._forwarders,
        }
    }
}

#[allow(dead_code)]
pub struct PlayerMergedAsyncRead {
    pub reader: tokio_util::io::StreamReader<MergedByteStream, Bytes>,
    pub join: JoinHandle<Result<(), PlayerError>>,
    _tracks: Vec<broadcast::Sender<PlayerEvent>>,
    _forwarders: Vec<JoinHandle<()>>,
}

pub struct PlayerTrackOutputs {
    pub tracks: Vec<PlayerTrackOutput>,
    pub join: JoinHandle<Result<(), PlayerError>>,
    senders: Vec<PlayerTrack>,
}

impl PlayerTrackOutputs {
    #[allow(dead_code)]
    pub fn track_count(&self) -> usize {
        self.senders.len()
    }

    #[allow(dead_code)]
    pub fn subscribe(&self, idx: usize) -> Option<broadcast::Receiver<PlayerEvent>> {
        self.senders.get(idx).map(|t| t.subscribe())
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
            senders,
        } = self;
        (tracks, senders, join)
    }
}

pub struct PlayerTrackOutput {
    pub track_index: usize,
    pub mime_type: Option<String>,
    rx: broadcast::Receiver<PlayerEvent>,
}

impl PlayerTrackOutput {
    pub fn into_receiver(self) -> broadcast::Receiver<PlayerEvent> {
        self.rx
    }

    /// Stream events for a single adaptation set (init + segments).
    #[allow(dead_code)]
    pub fn events(
        &mut self,
    ) -> impl Stream<
        Item = Result<PlayerEvent, tokio_stream::wrappers::errors::BroadcastStreamRecvError>,
    > + '_ {
        tokio_stream::wrappers::BroadcastStream::new(self.rx.resubscribe())
    }
}
