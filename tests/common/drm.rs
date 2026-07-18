//! Widevine license mock server and DRM playback helpers for integration tests.

use super::{collect_events, fixture_dir, spawn_playback_buffer_simulation};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use dashplay::Player;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use url::Url;

#[derive(Clone)]
struct LicenseServerState {
    license_response: Arc<Vec<u8>>,
    files: Arc<HashMap<String, Vec<u8>>>,
    license_post_count: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct RotatingDrmState {
    manifest_v1: Arc<String>,
    manifest_v2: Arc<String>,
    files: Arc<HashMap<String, Vec<u8>>>,
    license_response: Arc<Vec<u8>>,
    manifest_fetch_count: Arc<AtomicUsize>,
    license_post_count: Arc<AtomicUsize>,
}

/// Serves fixture media and returns a fixed Widevine license response on POST `/license`.
pub struct LicenseMockServer {
    pub manifest_url: Url,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl LicenseMockServer {
    pub async fn spawn(fixture: &str, license_response: Vec<u8>) -> Self {
        let root = fixture_dir(fixture);
        let files = Arc::new(super::load_fixture_files_public(&root));
        let state = LicenseServerState {
            license_response: Arc::new(license_response),
            files,
            license_post_count: Arc::new(AtomicUsize::new(0)),
        };

        let app = Router::new()
            .route("/license", post(serve_license))
            .route("/{*path}", get(serve_media))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind license mock server");
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

impl Drop for LicenseMockServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_license(
    State(state): State<LicenseServerState>,
    body: axum::body::Bytes,
) -> Response {
    if body.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    state.license_post_count.fetch_add(1, Ordering::Relaxed);
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .body(Body::from(state.license_response.as_ref().clone()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_media(
    State(state): State<LicenseServerState>,
    axum::extract::Path(path): axum::extract::Path<String>,
    uri: axum::http::Uri,
) -> Response {
    let url_path = uri.path().trim_end_matches('/').to_string();
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

/// Play a single track using a custom Widevine license fetcher.
pub async fn play_single_track_with_license_fetcher(
    manifest_url: &Url,
    timeout: std::time::Duration,
    fetcher: dashplay::WidevineLicenseFetcher,
) -> Result<Vec<dashplay::PlayerEvent>, dashplay::PlayerError> {
    let player = Player::new(manifest_url.as_str(), None)?.with_license_fetcher(fetcher);
    let outputs = player.start_tracks().await?;
    let buffer_feedback = outputs.buffer_feedback(0).expect("one track");
    let drain = spawn_playback_buffer_simulation(buffer_feedback, 25.0);
    let mut rx = outputs
        .tracks
        .into_iter()
        .next()
        .expect("one track")
        .into_receiver();
    let events = collect_events(&mut rx, timeout).await;
    drop(rx);
    drain.abort();
    outputs.join.await.unwrap()?;
    Ok(events)
}

/// Build a license fetcher that always returns `response` regardless of URL/challenge.
pub fn static_license_fetcher(response: Vec<u8>) -> dashplay::WidevineLicenseFetcher {
    let bytes = Bytes::from(response);
    Arc::new(move |_url: Url, _challenge: Vec<u8>| {
        let payload = bytes.clone();
        Box::pin(async move { Ok(payload) })
            as Pin<Box<dyn Future<Output = Result<Bytes, dashplay::DrmError>> + Send>>
    })
}

/// Spawn a fixture server and rewrite manifest license URL to hit `/license` on the same host.
pub async fn spawn_drm_fixture_with_mock_license(
    fixture: &str,
    license_response: Vec<u8>,
) -> LicenseMockServer {
    LicenseMockServer::spawn(fixture, license_response).await
}

/// Live server that serves `manifest_v1.mpd` then `manifest_v2.mpd` after the first refresh.
pub struct RotatingDrmMockServer {
    pub manifest_url: Url,
    pub license_post_count: Arc<AtomicUsize>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl RotatingDrmMockServer {
    pub async fn spawn(fixture: &str, license_response: Vec<u8>) -> Self {
        let root = fixture_dir(fixture);
        let files = Arc::new(super::load_fixture_files_public(&root));
        let manifest_v1 =
            Arc::new(std::fs::read_to_string(root.join("manifest_v1.mpd")).expect("manifest_v1"));
        let manifest_v2 =
            Arc::new(std::fs::read_to_string(root.join("manifest_v2.mpd")).expect("manifest_v2"));
        let license_post_count = Arc::new(AtomicUsize::new(0));
        let state = RotatingDrmState {
            manifest_v1,
            manifest_v2,
            files,
            license_response: Arc::new(license_response),
            manifest_fetch_count: Arc::new(AtomicUsize::new(0)),
            license_post_count: license_post_count.clone(),
        };

        let app = Router::new()
            .route("/manifest.mpd", get(serve_rotating_manifest))
            .route("/license", post(serve_rotating_license))
            .route("/{*path}", get(serve_rotating_media))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind rotating drm server");
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
            license_post_count,
            shutdown: Some(shutdown_tx),
            handle,
        }
    }
}

impl Drop for RotatingDrmMockServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_rotating_manifest(State(state): State<RotatingDrmState>) -> Response {
    let n = state.manifest_fetch_count.fetch_add(1, Ordering::Relaxed);
    let body = if n == 0 {
        state.manifest_v1.as_str()
    } else {
        state.manifest_v2.as_str()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/dash+xml")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_rotating_license(
    State(state): State<RotatingDrmState>,
    body: axum::body::Bytes,
) -> Response {
    if body.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    state.license_post_count.fetch_add(1, Ordering::Relaxed);
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .body(Body::from(state.license_response.as_ref().clone()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_rotating_media(
    State(state): State<RotatingDrmState>,
    axum::extract::Path(path): axum::extract::Path<String>,
    uri: axum::http::Uri,
) -> Response {
    let url_path = uri.path().trim_end_matches('/').to_string();
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

/// Counting license fetcher backed by a shared atomic counter.
pub fn counting_license_fetcher(
    response: Vec<u8>,
    counter: Arc<AtomicUsize>,
) -> dashplay::WidevineLicenseFetcher {
    let bytes = Bytes::from(response);
    Arc::new(move |_url: Url, _challenge: Vec<u8>| {
        let payload = bytes.clone();
        let counter = counter.clone();
        Box::pin(async move {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(payload)
        }) as Pin<Box<dyn Future<Output = Result<Bytes, dashplay::DrmError>> + Send>>
    })
}
