//! CMCD request headers and CMSD response exposure.

mod common;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use common::{fixture_dir, load_fixture_files_public, spawn_playback_buffer_simulation};
use dashplayrs::{CmcdConfig, HttpClient, HttpRequest, HttpResponse, Player, PlayerEvent, shared};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use url::Url;

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct CaptureState {
    files: Arc<HashMap<String, Vec<u8>>>,
    cmcd_headers: Arc<Mutex<Vec<HashMap<String, String>>>>,
}

async fn serve_with_cmsd(
    State(state): State<CaptureState>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let mut captured = HashMap::new();
    for name in ["cmcd-request", "cmcd-object", "cmcd-status", "cmcd-session"] {
        if let Some(value) = headers.get(name).and_then(|v| v.to_str().ok()) {
            captured.insert(name.to_string(), value.to_string());
        }
    }
    if !captured.is_empty() {
        state.cmcd_headers.lock().expect("lock").push(captured);
    }

    let path = uri.path().trim_end_matches('/');
    let Some(bytes) = state.files.get(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut response = Response::new(Body::from(bytes.clone()));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert("CMSD-Static", "ot=v,sf=d,st=v".parse().expect("header"));
    response.headers_mut().insert(
        "CMSD-Dynamic",
        r#"n="test-cdn";etp=100;rtt=20"#.parse().expect("header"),
    );
    response
}

async fn spawn_capture_server(
    fixture: &str,
) -> (
    Url,
    Arc<Mutex<Vec<HashMap<String, String>>>>,
    oneshot::Sender<()>,
) {
    let files = Arc::new(load_fixture_files_public(&fixture_dir(fixture)));
    let cmcd_headers = Arc::new(Mutex::new(Vec::new()));
    let state = CaptureState {
        files,
        cmcd_headers: cmcd_headers.clone(),
    };
    let app = Router::new()
        .route("/{*path}", get(serve_with_cmsd))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("serve");
    });
    (
        Url::parse(&format!("http://{addr}/manifest.mpd")).unwrap(),
        cmcd_headers,
        shutdown_tx,
    )
}

#[tokio::test]
async fn cmcd_headers_sent_and_cmsd_exposed() {
    let (manifest_url, captured, shutdown) = spawn_capture_server("vod_single").await;

    let player = Player::new(manifest_url.as_str(), None)
        .expect("player")
        .with_cmcd(
            CmcdConfig::new()
                .with_session_id("6e2fb550-c457-11e9-bb97-0800200c9a66")
                .with_content_id("test-content"),
        );
    let outputs = player.start_tracks().await.expect("start");
    let metrics = outputs.tracks[0].metrics();
    let buffer_feedback = outputs.buffer_feedback(0).expect("feedback");
    let _ = buffer_feedback.report(25.0);
    let drain = spawn_playback_buffer_simulation(buffer_feedback, 25.0);

    let mut rx = outputs.subscribe(0).expect("subscribe");
    let mut saw_cmsd_event = false;
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(PlayerEvent::CmsdUpdated { cmsd })) => {
                assert!(!cmsd.static_keys.is_empty());
                assert!(!cmsd.dynamic_hops.is_empty());
                saw_cmsd_event = true;
            }
            Ok(Ok(PlayerEvent::End | PlayerEvent::PlaybackEnded)) => break,
            Ok(Ok(PlayerEvent::Error(err))) => panic!("playback error: {err:?}"),
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    drain.abort();
    outputs.join.await.expect("join").expect("playback");

    let headers = captured.lock().expect("lock");
    assert!(
        !headers.is_empty(),
        "expected CMCD headers on at least one request"
    );
    let any_session = headers.iter().any(|h| {
        h.get("cmcd-session")
            .is_some_and(|v| v.contains("sid=\"6e2fb550-c457-11e9-bb97-0800200c9a66\""))
    });
    assert!(any_session, "expected sid in CMCD-Session: {headers:?}");
    let any_object = headers
        .iter()
        .any(|h| h.get("cmcd-object").is_some_and(|v| v.contains("ot=")));
    assert!(any_object, "expected ot in CMCD-Object: {headers:?}");

    assert!(saw_cmsd_event, "expected PlayerEvent::CmsdUpdated");
    assert!(
        metrics.snapshot().last_cmsd.is_some(),
        "expected CMSD on track metrics"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn cmcd_disabled_sends_no_headers() {
    let (manifest_url, captured, shutdown) = spawn_capture_server("vod_single").await;

    let player = Player::new(manifest_url.as_str(), None).expect("player");
    let outputs = player.start_tracks().await.expect("start");
    let buffer_feedback = outputs.buffer_feedback(0).expect("feedback");
    let _ = buffer_feedback.report(25.0);
    let drain = spawn_playback_buffer_simulation(buffer_feedback, 25.0);
    let mut rx = outputs.subscribe(0).expect("subscribe");
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(PlayerEvent::End | PlayerEvent::PlaybackEnded)) => break,
            Ok(Ok(PlayerEvent::Error(err))) => panic!("playback error: {err:?}"),
            Ok(Ok(_)) | Ok(Err(_)) => {}
            Err(_) => break,
        }
    }
    drain.abort();
    outputs.join.await.expect("join").expect("playback");

    assert!(
        captured.lock().expect("lock").is_empty(),
        "CMCD headers must not be sent when disabled"
    );
    let _ = shutdown.send(());
}

#[tokio::test]
async fn http_stream_response_preserves_headers_via_default() {
    struct EchoClient;

    impl HttpClient for EchoClient {
        fn send<'a>(
            &'a self,
            request: HttpRequest,
        ) -> dashplayrs::HttpFuture<'a, Result<HttpResponse, dashplayrs::HttpError>> {
            Box::pin(async move {
                Ok(HttpResponse::new(
                    200,
                    vec![("CMSD-Static".into(), "ot=m".into())],
                    bytes::Bytes::from(format!("echo:{}", request.url())),
                ))
            })
        }
    }

    let client = shared(EchoClient);
    let url = Url::parse("https://example.com/manifest.mpd").unwrap();
    let stream = client
        .open_body_stream(HttpRequest::get(url))
        .await
        .expect("stream");
    assert_eq!(stream.header("CMSD-Static"), Some("ot=m"));
}
