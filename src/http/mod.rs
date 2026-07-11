//! Pluggable HTTP client for manifest, segment, clock-sync, and license requests.
//!
//! The default backend is [`ReqwestClient`]. Supply a custom [`HttpClient`] via
//! [`crate::MediaPlayer::with_http_client`] or [`crate::Player::with_http_client`] to integrate
//! browser fetch, embedded stacks, or custom TLS.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

mod body_stream;
mod error;
mod request;
mod reqwest;
mod response;

pub(crate) use body_stream::HttpBodyStream;

pub use error::HttpError;
pub use request::HttpRequest;
pub use reqwest::ReqwestClient;
pub use response::HttpResponse;

/// HTTP method supported by [`HttpClient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Head,
    Post,
}

/// Shared handle to an [`HttpClient`] implementation.
pub type SharedHttpClient = Arc<dyn HttpClient>;

type OpenBodyFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(u16, HttpBodyStream), HttpError>> + Send + 'a>>;

/// Async HTTP transport used throughout the playback pipeline.
pub trait HttpClient: Send + Sync {
    /// Execute `request` and return the full response.
    fn send<'a>(
        &'a self,
        request: HttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>>;

    /// Open a response body for progressive reads (Low-Latency DASH partial segments).
    ///
    /// The default implementation buffers via [`Self::send`].
    fn open_body_stream<'a>(&'a self, request: HttpRequest) -> OpenBodyFuture<'a> {
        Box::pin(async move {
            let resp = self.send(request).await?;
            Ok((resp.status(), HttpBodyStream::from_bytes(resp.into_bytes())))
        })
    }
}

/// Wrap a concrete client for sharing across playback tasks.
pub fn shared(client: impl HttpClient + 'static) -> SharedHttpClient {
    Arc::new(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use url::Url;

    #[derive(Default)]
    struct MockClient {
        responses: Mutex<HashMap<String, HttpResponse>>,
    }

    impl MockClient {
        fn with_response(self, url: &str, response: HttpResponse) -> Self {
            self.responses
                .lock()
                .expect("lock")
                .insert(url.to_string(), response);
            self
        }
    }

    impl HttpClient for MockClient {
        fn send<'a>(
            &'a self,
            request: HttpRequest,
        ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>> {
            let url = request.url.to_string();
            Box::pin(async move {
                self.responses
                    .lock()
                    .expect("lock")
                    .get(&url)
                    .cloned()
                    .ok_or_else(|| HttpError::Transport(format!("no mock for {url}")))
            })
        }
    }

    #[tokio::test]
    async fn mock_client_returns_configured_response() {
        let url = Url::parse("https://example.com/clock").unwrap();
        let client = MockClient::default().with_response(
            "https://example.com/clock",
            HttpResponse::new(
                200,
                vec![],
                bytes::Bytes::from_static(b"2020-01-01T00:00:00Z"),
            ),
        );

        let resp = client.send(HttpRequest::get(url)).await.expect("response");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.bytes().as_ref(), b"2020-01-01T00:00:00Z");
    }
}
