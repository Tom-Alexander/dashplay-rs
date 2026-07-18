use super::{HttpClient, HttpError, HttpFuture, HttpRequest, HttpResponse};

/// Placeholder HTTP client when no default backend is linked.
#[derive(Debug, Clone, Default)]
pub struct UnconfiguredHttpClient;

impl HttpClient for UnconfiguredHttpClient {
    fn send<'a>(
        &'a self,
        _request: HttpRequest,
    ) -> HttpFuture<'a, Result<HttpResponse, HttpError>> {
        Box::pin(async move {
            Err(HttpError::Transport(
                "no HTTP client configured; call MediaPlayer::with_http_client, enable the reqwest-http feature, or use the wasm32 FetchClient default".into(),
            ))
        })
    }
}
