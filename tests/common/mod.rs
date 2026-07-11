//! Local HTTP server and playback helpers for integration tests.

#![allow(dead_code)]

pub mod drm;

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
use std::time::Duration as StdDuration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use url::Url;

#[derive(Clone)]
struct AppState {
    files: Arc<HashMap<String, Vec<u8>>>,
    not_found_prefixes: Arc<HashSet<String>>,
    delay_prefixes: Arc<HashSet<String>>,
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
        Self::spawn_configured(fixture, not_found_prefixes, &[]).await
    }

    /// Like [`Self::spawn_with_options`], but adds an artificial delay before responding for
    /// paths under the given prefixes (used to simulate slow high-bitrate downloads in ABR tests).
    pub async fn spawn_with_delays(fixture: &str, delay_prefixes: &[&str]) -> Self {
        Self::spawn_configured(fixture, &[], delay_prefixes).await
    }

    async fn spawn_configured(
        fixture: &str,
        not_found_prefixes: &[&str],
        delay_prefixes: &[&str],
    ) -> Self {
        let root = fixture_dir(fixture);
        let files = Arc::new(load_fixture_files(&root));
        let not_found_prefixes = Arc::new(
            not_found_prefixes
                .iter()
                .map(|p| p.trim_end_matches('/').to_string())
                .collect::<HashSet<_>>(),
        );
        let delay_prefixes = Arc::new(
            delay_prefixes
                .iter()
                .map(|p| p.trim_end_matches('/').to_string())
                .collect::<HashSet<_>>(),
        );

        let state = AppState {
            files,
            not_found_prefixes,
            delay_prefixes,
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

#[derive(Clone)]
struct PartialLiveState {
    files: Arc<HashMap<String, Vec<u8>>>,
}

/// Dynamic live server with `@availabilityTimeComplete=false` and chunked CMAF segment bodies.
pub struct PartialLiveServer {
    pub manifest_url: Url,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl PartialLiveServer {
    pub async fn spawn() -> Self {
        let root = fixture_dir("live_duration");
        let files = Arc::new(load_fixture_files(&root));
        let state = PartialLiveState { files };

        let app = Router::new()
            .route("/manifest.mpd", get(serve_partial_live_manifest))
            .route("/{*path}", get(serve_partial_live_path))
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

impl Drop for PartialLiveServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_partial_live_manifest() -> Response {
    let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     type="dynamic"
     minimumUpdatePeriod="PT0.5S"
     availabilityStartTime="2020-05-01T12:00:00Z"
     timeShiftBufferDepth="PT20S"
     suggestedPresentationDelay="PT4S"
     minBufferTime="PT2S">
  <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014" value="2020-05-01T12:00:20Z"/>
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <SegmentTemplate timescale="1000" duration="4000" initialization="init.mp4" media="seg-$Number$.m4s" startNumber="1" availabilityTimeOffset="7" availabilityTimeComplete="false"/>
      <Representation id="1" bandwidth="100000" codecs="avc1.42E01E" width="640" height="360"/>
      <ProducerReferenceTime id="0" type="encoder" wallClockTime="2020-05-01T12:00:00Z" presentationTime="0"/>
    </AdaptationSet>
  </Period>
</MPD>
"#;

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/dash+xml")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_partial_live_path(
    State(state): State<PartialLiveState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    let url_path = uri.path().trim_end_matches('/').to_string();
    if url_path.ends_with(".m4s") {
        let segment_id = url_path.rsplit('/').next().unwrap_or("");
        let (chunk_a, chunk_b) = dual_cmaf_chunks_for_segment(segment_id);
        let stream = futures_util::stream::iter(vec![
            Ok::<_, std::convert::Infallible>(chunk_a),
            Ok(chunk_b),
        ]);
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "video/iso.segment")
            .body(Body::from_stream(stream))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    serve_static_path(&state.files, &path, &uri, &headers)
}

fn dual_cmaf_chunks_for_segment(segment_id: &str) -> (Vec<u8>, Vec<u8>) {
    (
        build_cmaf_chunk(format!("partial-{segment_id}-a").as_bytes()),
        build_cmaf_chunk(format!("partial-{segment_id}-b").as_bytes()),
    )
}

fn build_cmaf_chunk(payload: &[u8]) -> Vec<u8> {
    let mut moof = Vec::with_capacity(8);
    moof.extend_from_slice(&8u32.to_be_bytes());
    moof.extend_from_slice(b"moof");
    let mut mdat = Vec::with_capacity(8 + payload.len());
    mdat.extend_from_slice(&(8u32 + payload.len() as u32).to_be_bytes());
    mdat.extend_from_slice(b"mdat");
    mdat.extend_from_slice(payload);
    moof.extend_from_slice(&mdat);
    moof
}

#[derive(Clone)]
struct ProducerReferenceLiveState {
    files: Arc<HashMap<String, Vec<u8>>>,
}

/// Dynamic live server where `ProducerReferenceTime` intentionally diverges from `UTCTiming`.
///
/// `UTCTiming` reports 20s since `availabilityStartTime`, but the encoder anchor says only 4s
/// of media have elapsed at that same wall instant — live-window selection must follow PRT.
pub struct ProducerReferenceLiveServer {
    pub manifest_url: Url,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl ProducerReferenceLiveServer {
    pub async fn spawn() -> Self {
        let root = fixture_dir("live_duration");
        let files = Arc::new(load_fixture_files(&root));
        let state = ProducerReferenceLiveState { files };

        let app = Router::new()
            .route("/manifest.mpd", get(serve_producer_reference_live_manifest))
            .route("/{*path}", get(serve_producer_reference_live_path))
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

impl Drop for ProducerReferenceLiveServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_producer_reference_live_manifest() -> Response {
    let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     type="dynamic"
     minimumUpdatePeriod="PT0.5S"
     availabilityStartTime="2020-05-01T12:00:00Z"
     timeShiftBufferDepth="PT20S"
     suggestedPresentationDelay="PT4S"
     minBufferTime="PT2S">
  <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014" value="2020-05-01T12:00:20Z"/>
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <SegmentTemplate timescale="1000" duration="4000" initialization="init.mp4" media="seg-$Number$.m4s" startNumber="1"/>
      <Representation id="1" bandwidth="100000" codecs="avc1.42E01E" width="640" height="360"/>
      <ProducerReferenceTime id="0" type="encoder" wallClockTime="2020-05-01T12:00:20Z" presentationTime="4000"/>
    </AdaptationSet>
  </Period>
</MPD>
"#;

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/dash+xml")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_producer_reference_live_path(
    State(state): State<ProducerReferenceLiveState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    serve_static_path(&state.files, &path, &uri, &headers)
}

#[derive(Clone)]
struct InbandProducerReferenceLiveState {
    files: Arc<HashMap<String, Vec<u8>>>,
}

/// Dynamic live server with `ProducerReferenceTime@inband=true` and matching `prft` boxes in segments.
pub struct InbandProducerReferenceLiveServer {
    pub manifest_url: Url,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl InbandProducerReferenceLiveServer {
    pub async fn spawn() -> Self {
        let root = fixture_dir("live_duration");
        let files = Arc::new(load_fixture_files(&root));
        let state = InbandProducerReferenceLiveState { files };

        let app = Router::new()
            .route(
                "/manifest.mpd",
                get(serve_inband_producer_reference_live_manifest),
            )
            .route("/{*path}", get(serve_inband_producer_reference_live_path))
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

impl Drop for InbandProducerReferenceLiveServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_inband_producer_reference_live_manifest() -> Response {
    let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     type="dynamic"
     minimumUpdatePeriod="PT0.5S"
     availabilityStartTime="2020-05-01T12:00:00Z"
     timeShiftBufferDepth="PT20S"
     suggestedPresentationDelay="PT4S"
     minBufferTime="PT2S">
  <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014" value="2020-05-01T12:00:20Z"/>
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <SegmentTemplate timescale="1000" duration="4000" initialization="init.mp4" media="seg-$Number$.m4s" startNumber="1"/>
      <Representation id="1" bandwidth="100000" codecs="avc1.42E01E" width="640" height="360"/>
      <ProducerReferenceTime id="0" type="encoder" inband="true" wallClockTime="2020-05-01T12:00:20Z" presentationTime="4000"/>
    </AdaptationSet>
  </Period>
</MPD>
"#;

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/dash+xml")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_inband_producer_reference_live_path(
    State(state): State<InbandProducerReferenceLiveState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    let url_path = uri.path().trim_end_matches('/').to_string();
    if url_path.ends_with(".m4s") {
        let segment_id = url_path.rsplit('/').next().unwrap_or("");
        let key = format!("/{segment_id}");
        let Some(raw) = state
            .files
            .get(&key)
            .or_else(|| state.files.get(segment_id))
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        // prft anchor: 4s media at 2020-05-01T12:00:20Z (matches MPD PRT; diverges from UTCTiming).
        let wrapped = wrap_segment_with_prft(raw, 4000, 1_588_334_420);
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "video/iso.segment")
            .body(Body::from(wrapped))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    serve_static_path(&state.files, &path, &uri, &headers)
}

const NTP_UNIX_OFFSET: i64 = 2_208_988_800;

fn ntp_timestamp_from_unix(unix_secs: i64) -> u64 {
    let ntp_sec = (unix_secs + NTP_UNIX_OFFSET) as u64;
    ntp_sec << 32
}

fn build_prft_box(reference_track_id: u32, ntp_timestamp: u64, media_time: u64) -> Vec<u8> {
    let mut payload = vec![0u8, 0, 0, 0]; // version 0, flags 0
    payload.extend_from_slice(&reference_track_id.to_be_bytes());
    payload.extend_from_slice(&ntp_timestamp.to_be_bytes());
    payload.extend_from_slice(&(media_time as u32).to_be_bytes());
    let size = (8 + payload.len()) as u32;
    let mut out = Vec::with_capacity(size as usize);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(b"prft");
    out.extend_from_slice(&payload);
    out
}

fn wrap_segment_with_prft(segment: &[u8], media_time: u64, wall_unix_secs: i64) -> Vec<u8> {
    let mut out = build_prft_box(1, ntp_timestamp_from_unix(wall_unix_secs), media_time);
    out.extend_from_slice(segment);
    out
}

#[derive(Clone)]
struct MultiPeriodLiveState {
    files: Arc<HashMap<String, Vec<u8>>>,
    fetch_count: Arc<AtomicUsize>,
}

/// Dynamic live server that transitions from period 1 to period 2 as simulated wall clock advances.
pub struct MultiPeriodLiveServer {
    pub manifest_url: Url,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl MultiPeriodLiveServer {
    pub async fn spawn() -> Self {
        let root = fixture_dir("live_multi_period");
        let files = Arc::new(load_fixture_files(&root));
        let state = MultiPeriodLiveState {
            files,
            fetch_count: Arc::new(AtomicUsize::new(0)),
        };

        let app = Router::new()
            .route("/manifest.mpd", get(serve_multi_period_manifest))
            .route("/{*path}", get(serve_multi_period_path))
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

impl Drop for MultiPeriodLiveServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_multi_period_manifest(State(state): State<MultiPeriodLiveState>) -> Response {
    let fetch = state.fetch_count.fetch_add(1, Ordering::SeqCst);
    // First manifest loads (MediaPlayer + initial loop pass) stay in period 1; later refresh enters period 2.
    let elapsed_secs = if fetch < 2 { 5 } else { 12 };
    let wall_now = format!("2020-05-01T12:00:{elapsed_secs:02}Z");
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     type="dynamic"
     minimumUpdatePeriod="PT0.5S"
     availabilityStartTime="2020-05-01T12:00:00Z"
     timeShiftBufferDepth="PT30S"
     suggestedPresentationDelay="PT1S"
     minBufferTime="PT2S">
  <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014" value="{wall_now}"/>
  <Period id="p1" duration="PT10S">
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <SegmentTemplate timescale="1000" duration="4000" initialization="p1-init.mp4" media="p1-seg-$Number$.m4s" startNumber="1"/>
      <Representation id="1" bandwidth="100000" codecs="avc1.42E01E" width="640" height="360"/>
    </AdaptationSet>
  </Period>
  <Period id="p2" start="PT10S">
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <SegmentTemplate timescale="1000" duration="4000" initialization="p2-init.mp4" media="p2-seg-$Number$.m4s" startNumber="1"/>
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

async fn serve_multi_period_path(
    State(state): State<MultiPeriodLiveState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    serve_static_path(&state.files, &path, &uri, &headers)
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
    headers: axum::http::HeaderMap,
) -> Response {
    serve_static_path(&state.files, &path, &uri, &headers)
}

fn serve_static_path(
    files: &HashMap<String, Vec<u8>>,
    path: &str,
    uri: &Uri,
    headers: &axum::http::HeaderMap,
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

    let Some(bytes) = files.get(&key) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Some(range_hdr) = headers.get(axum::http::header::RANGE) {
        if let Ok(range_str) = range_hdr.to_str() {
            if let Some((start, end)) = parse_http_range(range_str, bytes.len()) {
                let slice = &bytes[start..=end];
                return Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(
                        axum::http::header::CONTENT_RANGE,
                        format!("bytes {start}-{end}/{}", bytes.len()),
                    )
                    .body(Body::from(slice.to_vec()))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(bytes.clone()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn parse_http_range(header: &str, len: usize) -> Option<(usize, usize)> {
    let bytes_part = header.strip_prefix("bytes=")?;
    let (start_s, end_s) = bytes_part.split_once('-')?;
    let end = if end_s.is_empty() {
        len.saturating_sub(1)
    } else {
        end_s.parse().ok()?
    };
    let start = if start_s.is_empty() {
        len.saturating_sub(end + 1)
    } else {
        start_s.parse().ok()?
    };
    if start > end || end >= len {
        return None;
    }
    Some((start, end))
}

fn path_matches_prefix(url_path: &str, prefix: &str) -> bool {
    url_path == prefix || url_path.starts_with(&format!("{prefix}/"))
}

fn path_has_prefix(url_path: &str, prefix: &str) -> bool {
    url_path == prefix
        || url_path.starts_with(&format!("{prefix}/"))
        || url_path.starts_with(prefix)
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

pub fn read_fixture_bytes(name: &str, relative: &str) -> Vec<u8> {
    std::fs::read(fixture_dir(name).join(relative))
        .unwrap_or_else(|e| panic!("read fixture {name}/{relative}: {e}"))
}

async fn serve_path(
    State(state): State<AppState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    let url_path = uri.path().trim_end_matches('/').to_string();
    if url_path.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }

    for prefix in state.not_found_prefixes.iter() {
        if path_matches_prefix(&url_path, prefix) {
            return StatusCode::NOT_FOUND.into_response();
        }
    }

    for prefix in state.delay_prefixes.iter() {
        if path_has_prefix(&url_path, prefix) {
            tokio::time::sleep(StdDuration::from_secs(11)).await;
            break;
        }
    }

    serve_static_path(&state.files, &path, &uri, &headers)
}

fn load_fixture_files(root: &FsPath) -> HashMap<String, Vec<u8>> {
    let mut files = HashMap::new();
    collect_files(root, root, &mut files);
    files
}

pub fn load_fixture_files_public(root: &FsPath) -> HashMap<String, Vec<u8>> {
    load_fixture_files(root)
}

pub fn serve_static_path_public(
    files: &HashMap<String, Vec<u8>>,
    path: &str,
    uri: &Uri,
    headers: &axum::http::HeaderMap,
) -> Response {
    serve_static_path(files, path, uri, headers)
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

pub async fn collect_events(
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
            Ok(Ok(ev)) if ev.is_terminal() => {
                events.push(ev);
                break;
            }
            Ok(Ok(ev)) => events.push(ev),
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    events
}

/// Receive the next event matching `pred`, skipping others until `timeout` elapses.
pub async fn recv_matching(
    rx: &mut tokio::sync::broadcast::Receiver<dashplayrs::PlayerEvent>,
    timeout: std::time::Duration,
    mut pred: impl FnMut(&dashplayrs::PlayerEvent) -> bool,
) -> Option<dashplayrs::PlayerEvent> {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(ev)) if pred(&ev) => return Some(ev),
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => return None,
            Err(_) => return None,
        }
    }
    None
}

/// Simulates 1× playback consumption by draining buffer occupancy over wall-clock time.
pub fn spawn_playback_buffer_simulation(
    buffer_feedback: dashplayrs::BufferFeedback,
    initial_buffer_s: f64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer_s = initial_buffer_s;
        let _ = buffer_feedback.report(buffer_s);
        let mut interval = tokio::time::interval(StdDuration::from_millis(100));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            buffer_s = (buffer_s - 0.1).max(0.0);
            if buffer_feedback.report(buffer_s).is_err() {
                break;
            }
        }
    })
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
    play_single_track_with_buffer(manifest_url, timeout, drop_receiver_before_join, 25.0).await
}

pub async fn play_single_track_with_buffer(
    manifest_url: &Url,
    timeout: std::time::Duration,
    drop_receiver_before_join: bool,
    initial_buffer_s: f64,
) -> Result<Vec<dashplayrs::PlayerEvent>, dashplayrs::PlayerError> {
    let player = dashplayrs::Player::new(manifest_url.as_str(), None)?;
    let outputs = player.start_tracks().await?;
    let buffer_feedback = outputs.buffer_feedback(0).expect("one track");
    let _ = buffer_feedback.report(initial_buffer_s);
    let drain = spawn_playback_buffer_simulation(buffer_feedback, initial_buffer_s);
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
    drain.abort();
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
    let mut drains = Vec::with_capacity(track_count);
    for i in 0..track_count {
        if let Some(feedback) = outputs.buffer_feedback(i) {
            drains.push(spawn_playback_buffer_simulation(feedback, 25.0));
        }
    }
    let mut receivers: Vec<_> = outputs
        .tracks
        .into_iter()
        .map(|t| t.into_receiver())
        .collect();

    let mut all_events = Vec::with_capacity(track_count);
    for rx in receivers.iter_mut() {
        all_events.push(collect_events(rx, timeout).await);
    }

    for drain in drains {
        drain.abort();
    }
    outputs.join.await.unwrap()?;
    Ok(all_events)
}

pub fn init_payload(events: &[dashplayrs::PlayerEvent]) -> Option<Vec<u8>> {
    init_payloads(events).into_iter().next()
}

pub fn init_payloads(events: &[dashplayrs::PlayerEvent]) -> Vec<Vec<u8>> {
    events
        .iter()
        .filter_map(|ev| match ev {
            dashplayrs::PlayerEvent::Init(data) => Some(trim_payload(data.as_ref())),
            _ => None,
        })
        .collect()
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

pub fn trim_payload(bytes: &[u8]) -> Vec<u8> {
    let end = bytes
        .iter()
        .rposition(|b| *b != b'\n' && *b != b'\r')
        .map(|i| i + 1)
        .unwrap_or(0);
    bytes[..end].to_vec()
}

pub fn has_end(events: &[dashplayrs::PlayerEvent]) -> bool {
    events.iter().any(|ev| ev.is_terminal())
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

/// Unique `(number, time, sub_number)` keys for delivered segments.
pub fn segment_keys(events: &[dashplayrs::PlayerEvent]) -> Vec<(u64, u64, Option<u64>)> {
    events
        .iter()
        .filter_map(|ev| match ev {
            dashplayrs::PlayerEvent::Segment {
                number,
                time,
                sub_number,
                ..
            } => Some((*number, *time, *sub_number)),
            _ => None,
        })
        .collect()
}

pub fn assert_no_duplicate_segments(events: &[dashplayrs::PlayerEvent]) {
    let keys = segment_keys(events);
    let mut seen = std::collections::HashSet::new();
    for key in keys {
        assert!(
            seen.insert(key),
            "duplicate segment {key:?}; all keys: {:?}",
            segment_keys(events)
        );
    }
}

pub fn partial_segment_payloads(
    events: &[dashplayrs::PlayerEvent],
) -> Vec<(Option<dashplayrs::PartialSegmentChunk>, Vec<u8>)> {
    events
        .iter()
        .filter_map(|ev| match ev {
            dashplayrs::PlayerEvent::Segment { partial, data, .. } => {
                Some((*partial, trim_payload(data.as_ref())))
            }
            _ => None,
        })
        .collect()
}
