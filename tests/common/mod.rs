//! Local HTTP server and playback helpers for integration tests.

#![allow(dead_code)]

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use url::Url;

#[derive(Clone)]
struct AppState {
    files: Arc<HashMap<String, Vec<u8>>>,
    not_found_prefixes: Arc<HashSet<String>>,
}

pub struct FixtureServer {
    pub manifest_url: Url,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl FixtureServer {
    pub async fn spawn(fixture: &str) -> Self {
        Self::spawn_with_options(fixture, &[]).await
    }

    /// URL path prefixes (e.g. `"/bad"`) that always return HTTP 404.
    pub async fn spawn_with_options(fixture: &str, not_found_prefixes: &[&str]) -> Self {
        let root = fixture_dir(fixture);
        let files = Arc::new(load_fixture_files(&root));
        let not_found_prefixes = Arc::new(
            not_found_prefixes
                .iter()
                .map(|p| p.trim_end_matches('/').to_string())
                .collect::<HashSet<_>>(),
        );

        let state = AppState {
            files,
            not_found_prefixes,
        };

        let app = Router::new()
            .route("/{*path}", get(serve_path))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("serve");
        });

        let manifest_url =
            Url::parse(&format!("http://{addr}/manifest.mpd")).expect("manifest url");

        Self {
            manifest_url,
            shutdown: Some(shutdown_tx),
            handle,
        }
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

#[derive(Clone)]
struct AdvancingLiveState {
    files: Arc<HashMap<String, Vec<u8>>>,
    fetch_count: Arc<AtomicUsize>,
}

/// Serves a dynamic live MPD whose pinned `UTCTiming` advances on each manifest fetch.
pub struct AdvancingLiveServer {
    pub manifest_url: Url,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl AdvancingLiveServer {
    pub async fn spawn() -> Self {
        let root = fixture_dir("live_refresh");
        let files = Arc::new(load_fixture_files(&root));
        let state = AdvancingLiveState {
            files,
            fetch_count: Arc::new(AtomicUsize::new(0)),
        };

        let app = Router::new()
            .route("/manifest.mpd", get(serve_advancing_manifest))
            .route("/{*path}", get(serve_advancing_path))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("serve");
        });

        let manifest_url =
            Url::parse(&format!("http://{addr}/manifest.mpd")).expect("manifest url");

        Self {
            manifest_url,
            shutdown: Some(shutdown_tx),
            handle,
        }
    }
}

impl Drop for AdvancingLiveServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_advancing_manifest(State(state): State<AdvancingLiveState>) -> Response {
    let fetch = state.fetch_count.fetch_add(1, Ordering::SeqCst);
    // 12s base + 4s per manifest refresh simulates the live edge moving forward.
    let elapsed_secs = 12 + fetch * 4;
    let wall_now = format!("2020-05-01T12:00:{elapsed_secs:02}Z");
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     type="dynamic"
     minimumUpdatePeriod="PT0.5S"
     availabilityStartTime="2020-05-01T12:00:00Z"
     timeShiftBufferDepth="PT20S"
     suggestedPresentationDelay="PT2S"
     minBufferTime="PT2S">
  <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014" value="{wall_now}"/>
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <SegmentTemplate timescale="1000" duration="4000" initialization="init.mp4" media="seg-$Number$.m4s" startNumber="1"/>
      <Representation id="1" bandwidth="100000" codecs="avc1.42E01E" width="640" height="360"/>
    </AdaptationSet>
  </Period>
</MPD>
"#
    );

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/dash+xml")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_advancing_path(
    State(state): State<AdvancingLiveState>,
    Path(path): Path<String>,
    uri: Uri,
) -> Response {
    let url_path = uri.path().trim_end_matches('/').to_string();
    if url_path.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let key = if path.is_empty() {
        url_path
    } else {
        format!("/{path}")
    };

    let Some(bytes) = state.files.get(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(bytes.clone()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn read_fixture(name: &str, relative: &str) -> String {
    std::fs::read_to_string(fixture_dir(name).join(relative))
        .unwrap_or_else(|e| panic!("read fixture {name}/{relative}: {e}"))
}

async fn serve_path(State(state): State<AppState>, Path(path): Path<String>, uri: Uri) -> Response {
    let url_path = uri.path().trim_end_matches('/').to_string();
    if url_path.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }

    for prefix in state.not_found_prefixes.iter() {
        if url_path == *prefix || url_path.starts_with(&format!("{prefix}/")) {
            return StatusCode::NOT_FOUND.into_response();
        }
    }

    let key = if path.is_empty() {
        url_path
    } else {
        format!("/{path}")
    };

    let Some(bytes) = state.files.get(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(bytes.clone()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn load_fixture_files(root: &FsPath) -> HashMap<String, Vec<u8>> {
    let mut files = HashMap::new();
    collect_files(root, root, &mut files);
    files
}

fn collect_files(root: &FsPath, dir: &FsPath, out: &mut HashMap<String, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .expect("strip prefix")
            .to_string_lossy()
            .replace('\\', "/");
        let key = format!("/{rel}");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        out.insert(key, bytes);
    }
}

async fn collect_events(
    rx: &mut tokio::sync::broadcast::Receiver<dashplayrs::PlayerEvent>,
    timeout: std::time::Duration,
) -> Vec<dashplayrs::PlayerEvent> {
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(dashplayrs::PlayerEvent::End)) => {
                events.push(dashplayrs::PlayerEvent::End);
                break;
            }
            Ok(Ok(ev)) => events.push(ev),
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    events
}

pub async fn play_single_track(
    manifest_url: &Url,
    timeout: std::time::Duration,
) -> Result<Vec<dashplayrs::PlayerEvent>, dashplayrs::PlayerError> {
    play_single_track_with_options(manifest_url, timeout, false).await
}

/// Like [`play_single_track`], but drops the event receiver before awaiting the controller so
/// dynamic live streams (long `minimumUpdatePeriod`) do not block test shutdown.
pub async fn play_single_track_live(
    manifest_url: &Url,
    timeout: std::time::Duration,
) -> Result<Vec<dashplayrs::PlayerEvent>, dashplayrs::PlayerError> {
    play_single_track_with_options(manifest_url, timeout, true).await
}

async fn play_single_track_with_options(
    manifest_url: &Url,
    timeout: std::time::Duration,
    drop_receiver_before_join: bool,
) -> Result<Vec<dashplayrs::PlayerEvent>, dashplayrs::PlayerError> {
    let player = dashplayrs::Player::new(manifest_url.as_str(), None)?;
    let outputs = player.start_tracks().await?;
    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("one track")
        .into_receiver();
    let events = collect_events(&mut rx, timeout).await;
    if drop_receiver_before_join {
        drop(rx);
    }
    outputs.join.await.unwrap()?;
    Ok(events)
}

pub async fn play_all_tracks(
    manifest_url: &Url,
    timeout: std::time::Duration,
) -> Result<Vec<Vec<dashplayrs::PlayerEvent>>, dashplayrs::PlayerError> {
    let player = dashplayrs::Player::new(manifest_url.as_str(), None)?;
    let outputs = player.start_tracks().await?;
    let track_count = outputs.tracks.len();
    let mut receivers: Vec<_> = outputs
        .tracks
        .into_iter()
        .map(|t| t.into_receiver())
        .collect();

    let mut all_events = Vec::with_capacity(track_count);
    for rx in receivers.iter_mut() {
        all_events.push(collect_events(rx, timeout).await);
    }

    outputs.join.await.unwrap()?;
    Ok(all_events)
}

pub fn init_payload(events: &[dashplayrs::PlayerEvent]) -> Option<Vec<u8>> {
    events.iter().find_map(|ev| match ev {
        dashplayrs::PlayerEvent::Init(data) => Some(trim_payload(data.as_ref())),
        _ => None,
    })
}

pub fn segment_payloads(events: &[dashplayrs::PlayerEvent]) -> Vec<Vec<u8>> {
    events
        .iter()
        .filter_map(|ev| match ev {
            dashplayrs::PlayerEvent::Segment { data, .. } => Some(trim_payload(data.as_ref())),
            _ => None,
        })
        .collect()
}

fn trim_payload(bytes: &[u8]) -> Vec<u8> {
    let end = bytes
        .iter()
        .rposition(|b| *b != b'\n' && *b != b'\r')
        .map(|i| i + 1)
        .unwrap_or(0);
    bytes[..end].to_vec()
}

pub fn has_end(events: &[dashplayrs::PlayerEvent]) -> bool {
    events
        .iter()
        .any(|ev| matches!(ev, dashplayrs::PlayerEvent::End))
}

pub fn segment_numbers(events: &[dashplayrs::PlayerEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|ev| match ev {
            dashplayrs::PlayerEvent::Segment { number, .. } => Some(*number),
            _ => None,
        })
        .collect()
}
