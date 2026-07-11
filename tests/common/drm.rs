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
use dashplayrs::Player;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use url::Url;

#[derive(Clone)]
struct LicenseServerState {
    license_response: Arc<Vec<u8>>,
    files: Arc<HashMap<String, Vec<u8>>>,
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
    fetcher: dashplayrs::WidevineLicenseFetcher,
) -> Result<Vec<dashplayrs::PlayerEvent>, dashplayrs::PlayerError> {
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
pub fn static_license_fetcher(response: Vec<u8>) -> dashplayrs::WidevineLicenseFetcher {
    let bytes = Bytes::from(response);
    Arc::new(move |_url: Url, _challenge: Vec<u8>| {
        let payload = bytes.clone();
        Box::pin(async move { Ok(payload) })
            as Pin<Box<dyn Future<Output = Result<Bytes, dashplayrs::PlayerError>> + Send>>
    })
}

/// Spawn a fixture server and rewrite manifest license URL to hit `/license` on the same host.
pub async fn spawn_drm_fixture_with_mock_license(
    fixture: &str,
    license_response: Vec<u8>,
) -> LicenseMockServer {
    LicenseMockServer::spawn(fixture, license_response).await
}
