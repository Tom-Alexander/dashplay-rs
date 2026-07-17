//! WASM bindings for browser playback via Media Source Extensions.

use std::cell::RefCell;
use std::rc::Rc;

use dashplay::{
    BufferFeedback, MediaPlayer, PlayerError, PlayerEvent, TrackKind, TrackPreference,
    TrackSelection, set_widevine_device_bytes,
};
use js_sys::Function;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

// Bento4 / wasi-libc is linked for CENC decrypt. Without an explicit
// `__wasm_call_ctors` call, wasm-ld wraps every export as a WASI *command*
// entrypoint (`*.command_export`) that runs `__wasm_call_dtors` on return —
// which hits a null `__funcs_on_exit` slot and traps with `RuntimeError: null
// function`. Calling ctors once opts into reactor-style linking instead.
unsafe extern "C" {
    fn __wasm_call_ctors();
}

/// Install a panic hook that logs Rust panics to the browser console.
///
/// Runs automatically when the module is instantiated so aborts surface a readable
/// message instead of a bare `RuntimeError: unreachable`.
#[wasm_bindgen(start)]
pub fn start() {
    unsafe {
        __wasm_call_ctors();
    }
    console_error_panic_hook::set_once();
}

#[derive(Serialize)]
struct TrackInfoJs {
    index: usize,
    mime_type: Option<String>,
    codecs: Vec<String>,
    kind: &'static str,
    language: Option<String>,
    is_dynamic: bool,
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
    license_url: Option<String>,
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
            license_url: None,
            on_track: None,
            on_fragment: None,
            on_status: None,
            on_error: None,
            started: Rc::new(RefCell::new(false)),
        }
    }

    /// Optional license server URL override (otherwise taken from the MPD).
    pub fn set_license_url(&mut self, license_url: Option<String>) {
        self.license_url = license_url.filter(|s| !s.is_empty());
    }

    /// Load a Widevine `.wvd` device (pywidevine format) before [`Self::start`].
    ///
    /// Required for encrypted playback; clear streams may omit this.
    pub fn set_widevine_device(&mut self, device_wvd: &[u8]) -> Result<(), JsValue> {
        set_widevine_device_bytes(device_wvd.to_vec())
            .map_err(|err| JsValue::from_str(&err.to_string()))
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
        let license_url = self.license_url.clone();
        let on_track = self.on_track.clone();
        let on_fragment = self.on_fragment.clone();
        let on_status = self.on_status.clone();
        let on_error = self.on_error.clone();

        spawn_local(async move {
            if let Err(err) = run_playback(
                &manifest_url,
                license_url.as_deref(),
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
    license_url: Option<&str>,
    on_track: Option<&Function>,
    on_fragment: Option<&Function>,
    on_status: Option<&Function>,
    on_error: Option<&Function>,
) -> Result<(), PlayerError> {
    let _ = on_error;
    emit_status(on_status, "loading manifest");

    // Angel One and similar demos ship parallel WebM AdaptationSets; their
    // SegmentBase@indexRange is EBML Cues, not ISO BMFF sidx. Prefer fMP4.
    let track_selection = TrackSelection::default()
        .with_video(
            TrackPreference::default()
                .codec("avc1")
                .codec("hev1")
                .codec("hvc1")
                .max_tracks(1),
        )
        .with_audio(
            TrackPreference::default()
                .codec("mp4a")
                .language("en")
                .max_tracks(1),
        );

    let media_player =
        MediaPlayer::new(manifest_url, license_url)?.with_track_selection(track_selection);
    let outputs = media_player.start().await?;

    let track_count = outputs.tracks.len();
    let is_dynamic = outputs.is_dynamic;
    emit_status(
        on_status,
        &format!("loaded manifest with {track_count} track(s)"),
    );

    if is_dynamic {
        emit_status(on_status, "manifest:live");
    } else {
        emit_status(on_status, "manifest:vod");
    }

    let mut receivers = Vec::with_capacity(track_count);
    let mut buffer_feedbacks = Vec::with_capacity(track_count);

    for (index, track) in outputs.tracks.iter().enumerate() {
        let info = TrackInfoJs {
            index,
            mime_type: track.mime_type(),
            codecs: track.info().codecs.clone(),
            kind: kind_label(track.info().kind),
            language: track.info().language.clone(),
            is_dynamic,
        };

        if let Some(callback) = on_track {
            let value = serde_wasm_bindgen::to_value(&info).map_err(|err| {
                PlayerError::Segment(dashplay::SegmentError::Request(
                    dashplay::HttpError::Transport(err.to_string()),
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
                // Safari/WebKit MSE is unreliable when fed dozens of tiny CMAF chunks
                // (`appendBuffer` per moof+mdat). The HTTP body is already complete before
                // fragments are emitted, so coalesce back into one MSE append per segment.
                let mut pending_media = bytes::BytesMut::new();
                loop {
                    match rx.recv().await {
                        Ok(PlayerEvent::Init(data)) => {
                            if !pending_media.is_empty() {
                                emit_fragment(
                                    on_fragment.as_ref(),
                                    index,
                                    "segment",
                                    &pending_media.split().freeze(),
                                );
                            }
                            emit_fragment(on_fragment.as_ref(), index, "init", &data);
                        }
                        Ok(PlayerEvent::Segment { data, partial, .. }) => {
                            pending_media.extend_from_slice(&data);
                            let complete = partial.is_none_or(|p| p.is_final);
                            if complete {
                                let coalesced = pending_media.split().freeze();
                                emit_fragment(on_fragment.as_ref(), index, "segment", &coalesced);
                                segments_seen = segments_seen.saturating_add(1);
                                let estimated_buffer = (segments_seen as f64 * 2.0).min(20.0);
                                let _ = buffer.report(estimated_buffer);
                            }
                        }
                        Ok(PlayerEvent::ManifestLoaded { .. }) => {}
                        Ok(PlayerEvent::BitrateChanged { to_bitrate_bps, .. }) => {
                            emit_status(
                                on_status.as_ref(),
                                &format!("track {index} bitrate {to_bitrate_bps} bps"),
                            );
                        }
                        Ok(PlayerEvent::End | PlayerEvent::PlaybackEnded) => {
                            if !pending_media.is_empty() {
                                emit_fragment(
                                    on_fragment.as_ref(),
                                    index,
                                    "segment",
                                    &pending_media.split().freeze(),
                                );
                            }
                            break;
                        }
                        Ok(PlayerEvent::Error(err)) => {
                            return Err(PlayerError::Segment(dashplay::SegmentError::Request(
                                dashplay::HttpError::Transport(err.0),
                            )));
                        }
                        Ok(_) => {}
                        Err(RecvError::Lagged(_)) => {
                            return Err(PlayerError::Segment(dashplay::SegmentError::Request(
                                dashplay::HttpError::Transport(
                                    "event receiver lagged; increase consumer throughput".into(),
                                ),
                            )));
                        }
                        Err(RecvError::Closed) => {
                            if !pending_media.is_empty() {
                                emit_fragment(
                                    on_fragment.as_ref(),
                                    index,
                                    "segment",
                                    &pending_media.split().freeze(),
                                );
                            }
                            break;
                        }
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
