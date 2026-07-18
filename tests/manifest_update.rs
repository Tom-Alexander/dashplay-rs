mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use common::{init_payload, play_single_track_live, segment_payloads};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use url::Url;

#[derive(Clone)]
struct LocationLiveState {
    files: Arc<HashMap<String, Vec<u8>>>,
    fetch_count: Arc<AtomicUsize>,
}

struct LocationLiveServer {
    pub manifest_url: Url,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl LocationLiveServer {
    async fn spawn() -> Self {
        let root = common::fixture_dir("live_duration");
        let files = Arc::new(common::load_fixture_files_public(&root));
        let state = LocationLiveState {
            files,
            fetch_count: Arc::new(AtomicUsize::new(0)),
        };

        let app = Router::new()
            .route("/manifest.mpd", get(serve_location_manifest))
            .route("/alt/manifest.mpd", get(serve_location_alt_manifest))
            .route("/alt/{*path}", get(serve_location_alt_path))
            .route("/{*path}", get(serve_location_path))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("serve");
        });

        Self {
            manifest_url: Url::parse(&format!("http://{addr}/manifest.mpd")).unwrap(),
            shutdown: Some(shutdown_tx),
            handle,
        }
    }
}

impl Drop for LocationLiveServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_location_manifest(State(state): State<LocationLiveState>) -> Response {
    let fetch = state.fetch_count.fetch_add(1, Ordering::SeqCst);
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
  <Location>/alt/manifest.mpd</Location>
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
    xml_ok(body)
}

async fn serve_location_alt_manifest(State(state): State<LocationLiveState>) -> Response {
    let fetch = state.fetch_count.fetch_add(1, Ordering::SeqCst);
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
  <Location>/alt/manifest.mpd</Location>
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
    xml_ok(body)
}

async fn serve_location_alt_path(
    State(state): State<LocationLiveState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    common::serve_static_path_public(&state.files, &path, &uri, &headers)
}

async fn serve_location_path(
    State(state): State<LocationLiveState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    common::serve_static_path_public(&state.files, &path, &uri, &headers)
}

#[derive(Clone)]
struct PatchLiveState {
    files: Arc<HashMap<String, Vec<u8>>>,
    refresh_count: Arc<AtomicUsize>,
}

struct PatchLiveServer {
    pub manifest_url: Url,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl PatchLiveServer {
    async fn spawn() -> Self {
        let root = common::fixture_dir("live_duration");
        let files = Arc::new(common::load_fixture_files_public(&root));
        let state = PatchLiveState {
            files,
            refresh_count: Arc::new(AtomicUsize::new(0)),
        };

        let app = Router::new()
            .route("/manifest.mpd", get(serve_patch_manifest))
            .route("/patch.mpp", get(serve_patch_document))
            .route("/{*path}", get(serve_patch_path))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("serve");
        });

        Self {
            manifest_url: Url::parse(&format!("http://{addr}/manifest.mpd")).unwrap(),
            shutdown: Some(shutdown_tx),
            handle,
        }
    }
}

impl Drop for PatchLiveServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_patch_manifest(State(state): State<PatchLiveState>) -> Response {
    let refresh = state.refresh_count.fetch_add(1, Ordering::SeqCst);
    let publish = if refresh == 0 {
        "2020-05-01T12:00:12Z"
    } else {
        "2020-05-01T12:00:16Z"
    };
    let wall_now = publish;
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     id="patch-live"
     type="dynamic"
     publishTime="{publish}"
     minimumUpdatePeriod="PT0.5S"
     availabilityStartTime="2020-05-01T12:00:00Z"
     timeShiftBufferDepth="PT20S"
     suggestedPresentationDelay="PT2S"
     minBufferTime="PT2S">
  <PatchLocation ttl="315360000">patch.mpp</PatchLocation>
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
    xml_ok(body)
}

async fn serve_patch_document() -> Response {
    let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<Patch xmlns="urn:mpeg:dash:schema:mpd-patch:2020"
     mpdId="patch-live"
     originalPublishTime="2020-05-01T12:00:12Z"
     publishTime="2020-05-01T12:00:16Z">
  <replace sel="/MPD/@publishTime">2020-05-01T12:00:16Z</replace>
  <replace sel="/MPD/UTCTiming"> <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014" value="2020-05-01T12:00:20Z"/> </replace>
</Patch>"#;
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/dash-patch+xml")
        .body(Body::from(body))
        .unwrap()
        .into_response()
}

async fn serve_patch_path(
    State(state): State<PatchLiveState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    common::serve_static_path_public(&state.files, &path, &uri, &headers)
}

#[derive(Clone)]
struct SteeringLiveState {
    beta_files: Arc<HashMap<String, Vec<u8>>>,
    dcsm_hits: Arc<AtomicUsize>,
    ttl_secs: u64,
}

struct SteeringLiveServer {
    pub manifest_url: Url,
    pub dcsm_hits: Arc<AtomicUsize>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl SteeringLiveServer {
    async fn spawn() -> Self {
        Self::spawn_with_ttl(300).await
    }

    async fn spawn_with_ttl(ttl_secs: u64) -> Self {
        let beta = common::fixture_dir("live_duration");
        let dcsm_hits = Arc::new(AtomicUsize::new(0));
        let state = SteeringLiveState {
            beta_files: Arc::new(common::load_fixture_files_public(&beta)),
            dcsm_hits: dcsm_hits.clone(),
            ttl_secs,
        };

        let app = Router::new()
            .route("/manifest.mpd", get(serve_steering_manifest))
            .route("/steering.json", get(serve_steering_dcsm))
            .route("/alpha/{*path}", get(serve_steering_alpha))
            .route("/beta/{*path}", get(serve_steering_beta))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("serve");
        });

        Self {
            manifest_url: Url::parse(&format!("http://{addr}/manifest.mpd")).unwrap(),
            dcsm_hits,
            shutdown: Some(shutdown_tx),
            handle,
        }
    }
}

impl Drop for SteeringLiveServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.handle.abort();
    }
}

async fn serve_steering_manifest() -> Response {
    let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     type="dynamic"
     minimumUpdatePeriod="PT0.5S"
     availabilityStartTime="2020-05-01T12:00:00Z"
     timeShiftBufferDepth="PT20S"
     suggestedPresentationDelay="PT2S"
     minBufferTime="PT2S">
  <BaseURL serviceLocation="alpha">alpha/</BaseURL>
  <BaseURL serviceLocation="beta">beta/</BaseURL>
  <ContentSteering defaultServiceLocation="alpha">steering.json</ContentSteering>
  <UTCTiming schemeIdUri="urn:mpeg:dash:utc:direct:2014" value="2020-05-01T12:00:16Z"/>
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <SegmentTemplate timescale="1000" duration="4000" initialization="init.mp4" media="seg-$Number$.m4s" startNumber="1"/>
      <Representation id="1" bandwidth="100000" codecs="avc1.42E01E" width="640" height="360"/>
    </AdaptationSet>
  </Period>
</MPD>
"#;
    xml_ok(body.to_string())
}

async fn serve_steering_dcsm(State(state): State<SteeringLiveState>) -> Response {
    state.dcsm_hits.fetch_add(1, Ordering::SeqCst);
    let body = format!(
        r#"{{"VERSION":1,"TTL":{},"PATHWAY-PRIORITY":["beta"],"RELOAD-URI":"steering.json"}}"#,
        state.ttl_secs
    );
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(body))
        .unwrap()
        .into_response()
}

async fn serve_steering_alpha() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

async fn serve_steering_beta(
    State(state): State<SteeringLiveState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: axum::http::HeaderMap,
) -> Response {
    common::serve_static_path_public(&state.beta_files, &path, &uri, &headers)
}

fn xml_ok(body: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/dash+xml")
        .body(Body::from(body))
        .unwrap()
        .into_response()
}

#[tokio::test]
async fn live_location_redirects_manifest_refresh() {
    let server = LocationLiveServer::spawn().await;
    let events =
        play_single_track_live(&server.manifest_url, std::time::Duration::from_millis(800))
            .await
            .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    assert!(
        segment_payloads(&events).len() >= 2,
        "expected segments after location-based refresh"
    );
}

#[tokio::test]
async fn live_mpd_patch_updates_manifest() {
    let server = PatchLiveServer::spawn().await;
    let events =
        play_single_track_live(&server.manifest_url, std::time::Duration::from_millis(800))
            .await
            .expect("playback");

    assert!(
        segment_payloads(&events).len() >= 2,
        "expected segments after MPD patch refresh"
    );
}

#[tokio::test]
async fn content_steering_selects_preferred_base_url() {
    let server = SteeringLiveServer::spawn().await;
    let events =
        play_single_track_live(&server.manifest_url, std::time::Duration::from_millis(800))
            .await
            .expect("playback");

    assert_eq!(
        init_payload(&events).as_deref(),
        Some(b"dashplay-init-v1".as_ref())
    );
    let segments = segment_payloads(&events);
    assert!(
        !segments.is_empty(),
        "expected segments from steered beta base URL"
    );
    assert!(
        server.dcsm_hits.load(Ordering::SeqCst) >= 1,
        "expected at least one DCSM fetch"
    );
}
