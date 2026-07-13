//! WASM bindings for browser playback via Media Source Extensions.

mod fetch_client;

use std::cell::RefCell;
use std::rc::Rc;

use dashplayrs::{BufferFeedback, MediaPlayer, PlayerError, PlayerEvent, TrackKind, shared};
use fetch_client::FetchClient;
use js_sys::Function;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

/// Install a panic hook that logs Rust panics to the browser console.
///
/// Runs automatically when the module is instantiated so aborts surface a readable
/// message instead of a bare `RuntimeError: unreachable`.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

#[derive(Serialize)]
struct TrackInfoJs {
    index: usize,
    mime_type: Option<String>,
    codecs: Vec<String>,
    kind: &'static str,
    language: Option<String>,
}

fn kind_label(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Audio => "audio",
        TrackKind::Video => "video",
        TrackKind::Text => "text",
        TrackKind::TrickPlay => "trickplay",
        TrackKind::Image => "image",
    }
}

#[wasm_bindgen]
pub struct DashPlayer {
    manifest_url: String,
    on_track: Option<Function>,
    on_fragment: Option<Function>,
    on_status: Option<Function>,
    on_error: Option<Function>,
    started: Rc<RefCell<bool>>,
}

#[wasm_bindgen]
impl DashPlayer {
    #[wasm_bindgen(constructor)]
    pub fn new(manifest_url: String) -> Self {
        Self {
            manifest_url,
            on_track: None,
            on_fragment: None,
            on_status: None,
            on_error: None,
            started: Rc::new(RefCell::new(false)),
        }
    }

    pub fn on_track(&mut self, callback: Function) {
        self.on_track = Some(callback);
    }

    pub fn on_fragment(&mut self, callback: Function) {
        self.on_fragment = Some(callback);
    }

    pub fn on_status(&mut self, callback: Function) {
        self.on_status = Some(callback);
    }

    pub fn on_error(&mut self, callback: Function) {
        self.on_error = Some(callback);
    }

    pub fn start(&mut self) -> Result<(), JsValue> {
        if *self.started.borrow() {
            return Err(JsValue::from_str("playback already started"));
        }
        *self.started.borrow_mut() = true;

        let manifest_url = self.manifest_url.clone();
        let on_track = self.on_track.clone();
        let on_fragment = self.on_fragment.clone();
        let on_status = self.on_status.clone();
        let on_error = self.on_error.clone();

        spawn_local(async move {
            if let Err(err) = run_playback(
                &manifest_url,
                on_track.as_ref(),
                on_fragment.as_ref(),
                on_status.as_ref(),
                on_error.as_ref(),
            )
            .await
            {
                emit_error(on_error.as_ref(), &err);
            }
        });

        Ok(())
    }
}

async fn run_playback(
    manifest_url: &str,
    on_track: Option<&Function>,
    on_fragment: Option<&Function>,
    on_status: Option<&Function>,
    on_error: Option<&Function>,
) -> Result<(), PlayerError> {
    let _ = on_error;
    emit_status(on_status, "loading manifest");

    let client = shared(FetchClient::default());
    let media_player = MediaPlayer::new(manifest_url, None)?.with_http_client(client);
    let outputs = media_player.start().await?;

    let track_count = outputs.tracks.len();
    emit_status(
        on_status,
        &format!("loaded manifest with {track_count} track(s)"),
    );

    let mut receivers = Vec::with_capacity(track_count);
    let mut buffer_feedbacks = Vec::with_capacity(track_count);

    for (index, track) in outputs.tracks.iter().enumerate() {
        let info = TrackInfoJs {
            index,
            mime_type: track.mime_type.clone(),
            codecs: track.info.codecs.clone(),
            kind: kind_label(track.info.kind),
            language: track.info.language.clone(),
        };

        if let Some(callback) = on_track {
            let value = serde_wasm_bindgen::to_value(&info).map_err(|err| {
                PlayerError::Segment(dashplayrs::SegmentError::Request(
                    dashplayrs::HttpError::Transport(err.to_string()),
                ))
            })?;
            let _ = callback.call1(callback, &value);
        }

        receivers.push(track.subscribe());
        buffer_feedbacks.push(track.buffer_feedback());
    }

    let playback_task = outputs.run();
    let event_task = consume_track_events(receivers, buffer_feedbacks, on_fragment, on_status);

    let (playback_result, event_result) =
        futures_util::future::join(playback_task, event_task).await;
    playback_result?;
    event_result?;

    emit_status(on_status, "playback ended");
    Ok(())
}

async fn consume_track_events(
    receivers: Vec<tokio::sync::broadcast::Receiver<PlayerEvent>>,
    buffer_feedbacks: Vec<BufferFeedback>,
    on_fragment: Option<&Function>,
    on_status: Option<&Function>,
) -> Result<(), PlayerError> {
    let tasks = receivers
        .into_iter()
        .enumerate()
        .map(|(index, mut rx)| {
            let buffer = buffer_feedbacks[index].clone();
            let on_fragment = on_fragment.cloned();
            let on_status = on_status.cloned();
            async move {
                let mut segments_seen = 0u32;
                loop {
                    match rx.recv().await {
                        Ok(PlayerEvent::Init(data)) => {
                            emit_fragment(on_fragment.as_ref(), index, "init", &data);
                        }
                        Ok(PlayerEvent::Segment { data, .. }) => {
                            emit_fragment(on_fragment.as_ref(), index, "segment", &data);
                            segments_seen = segments_seen.saturating_add(1);
                            let estimated_buffer = (segments_seen as f64 * 2.0).min(20.0);
                            let _ = buffer.report(estimated_buffer);
                        }
                        Ok(PlayerEvent::BitrateChanged { to_bitrate_bps, .. }) => {
                            emit_status(
                                on_status.as_ref(),
                                &format!("track {index} bitrate {to_bitrate_bps} bps"),
                            );
                        }
                        Ok(PlayerEvent::End | PlayerEvent::PlaybackEnded) => break,
                        Ok(PlayerEvent::Error(err)) => {
                            return Err(PlayerError::Segment(dashplayrs::SegmentError::Request(
                                dashplayrs::HttpError::Transport(err.0),
                            )));
                        }
                        Ok(_) => {}
                        Err(RecvError::Lagged(_)) => {
                            return Err(PlayerError::Segment(dashplayrs::SegmentError::Request(
                                dashplayrs::HttpError::Transport(
                                    "event receiver lagged; increase consumer throughput".into(),
                                ),
                            )));
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
                Ok::<(), PlayerError>(())
            }
        })
        .collect::<Vec<_>>();

    for result in futures_util::future::join_all(tasks).await {
        result?;
    }

    Ok(())
}

fn emit_fragment(callback: Option<&Function>, track_index: usize, kind: &str, data: &bytes::Bytes) {
    let Some(callback) = callback else {
        return;
    };
    let array = js_sys::Uint8Array::from(data.as_ref());
    let _ = callback.call3(
        callback,
        &JsValue::from_f64(track_index as f64),
        &JsValue::from_str(kind),
        &array,
    );
}

fn emit_status(callback: Option<&Function>, message: &str) {
    let Some(callback) = callback else {
        return;
    };
    let _ = callback.call1(callback, &JsValue::from_str(message));
}

fn emit_error(callback: Option<&Function>, err: &PlayerError) {
    let message = err.to_string();
    web_sys::console::error_1(&JsValue::from_str(&message));
    if let Some(callback) = callback {
        let _ = callback.call1(callback, &JsValue::from_str(&message));
    }
}
