use url::Url;

use super::PlayerError;
use super::http::{HttpRequest, SharedHttpClient};
use super::manifest::ByteRange;
use super::segment_blacklist::SegmentBlacklist;

/// Try each resolved absolute base with the same relative segment path (multi-CDN / redundant hosts).
pub(crate) async fn fetch_bytes_with_base_failover(
    client: &SharedHttpClient,
    bases: &[Url],
    relative_path: &str,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<u8>, PlayerError> {
    fetch_bytes_with_base_failover_and_range(client, bases, relative_path, None, blacklist).await
}

/// Like [`fetch_bytes_with_base_failover`], but sends an HTTP `Range` header when `range` is set.
pub(crate) async fn fetch_bytes_with_base_failover_and_range(
    client: &SharedHttpClient,
    bases: &[Url],
    relative_path: &str,
    range: Option<ByteRange>,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<u8>, PlayerError> {
    let mut last_err: Option<PlayerError> = None;
    for base in bases {
        let url = if relative_path.is_empty() {
            base.clone()
        } else {
            base.join(relative_path)?
        };
        match fetch_bytes_range(client, url, range, blacklist).await {
            Ok(b) => return Ok(b),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or(PlayerError::SegmentExhaustedRepresentations))
}

async fn fetch_bytes_range(
    client: &SharedHttpClient,
    url: Url,
    range: Option<ByteRange>,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<u8>, PlayerError> {
    if blacklist.contains_url(&url) {
        return Err(PlayerError::SegmentBlacklisted(url.to_string()));
    }

    let mut req = HttpRequest::get(url.clone());
    if let Some(r) = range {
        req = req.byte_range(r.start, r.end);
    }

    let resp = client.send(req).await?;
    if !resp.is_success() {
        blacklist.insert_url(&url);
        return Err(PlayerError::SegmentRequestFailed {
            status: resp.status(),
            url: url.to_string(),
        });
    }
    Ok(resp.into_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ReqwestClient;
    use axum::{Router, body::Body, http::StatusCode, response::IntoResponse, routing::get};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot;

    async fn spawn_not_found_server() -> (Url, oneshot::Sender<()>) {
        let app = Router::new().route("/{*path}", get(|| async { StatusCode::NOT_FOUND }));
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
        (Url::parse(&format!("http://{addr}/")).unwrap(), shutdown_tx)
    }

    #[tokio::test]
    async fn fetch_bytes_blacklists_failed_url() {
        let (base, shutdown) = spawn_not_found_server().await;
        let client = crate::http::shared(ReqwestClient::default());
        let blacklist = SegmentBlacklist::new();
        let url = base.join("seg.m4s").unwrap();

        let err = fetch_bytes_with_base_failover_and_range(
            &client,
            std::slice::from_ref(&url),
            "",
            None,
            &blacklist,
        )
        .await
        .expect_err("404");
        assert!(matches!(
            err,
            PlayerError::SegmentRequestFailed { status: 404, .. }
        ));

        let err = fetch_bytes_with_base_failover_and_range(
            &client,
            std::slice::from_ref(&url),
            "",
            None,
            &blacklist,
        )
        .await
        .expect_err("blacklisted");
        assert!(matches!(err, PlayerError::SegmentBlacklisted(_)));

        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn fetch_bytes_with_base_failover_tries_next_base() {
        static HITS: AtomicUsize = AtomicUsize::new(0);

        async fn counting_handler() -> impl IntoResponse {
            let n = HITS.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                return StatusCode::NOT_FOUND.into_response();
            }
            (StatusCode::OK, Body::from("good")).into_response()
        }

        let app = Router::new().route("/{*path}", get(counting_handler));
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

        let bad = Url::parse(&format!("http://{addr}/bad/")).unwrap();
        let good = Url::parse(&format!("http://{addr}/good/")).unwrap();
        let client = crate::http::shared(ReqwestClient::default());
        let blacklist = SegmentBlacklist::new();

        let bytes = fetch_bytes_with_base_failover(&client, &[bad, good], "seg.m4s", &blacklist)
            .await
            .expect("failover");
        assert_eq!(bytes, b"good");
        assert_eq!(HITS.load(Ordering::SeqCst), 2);

        let _ = shutdown_tx.send(());
    }

    #[tokio::test]
    async fn fetch_bytes_with_base_failover_returns_last_error() {
        let (base, shutdown) = spawn_not_found_server().await;
        let client = crate::http::shared(ReqwestClient::default());
        let blacklist = SegmentBlacklist::new();
        let bases = [base.join("a/").unwrap(), base.join("b/").unwrap()];

        let err = fetch_bytes_with_base_failover(&client, &bases, "seg.m4s", &blacklist)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            PlayerError::SegmentRequestFailed { status: 404, .. }
        ));

        let _ = shutdown.send(());
    }
}
