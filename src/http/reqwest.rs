use std::future::Future;
use std::pin::Pin;

use futures_util::StreamExt;
use reqwest::Client;

use super::{HttpBodyStream, HttpClient, HttpError, HttpMethod, HttpRequest, HttpResponse};

/// Default [`HttpClient`] backed by [`reqwest`](https://docs.rs/reqwest).
#[derive(Debug, Clone)]
pub struct ReqwestClient {
    inner: Client,
}

impl Default for ReqwestClient {
    fn default() -> Self {
        Self {
            inner: Client::new(),
        }
    }
}

impl ReqwestClient {
    /// Wrap an existing `reqwest::Client`.
    pub fn new(inner: Client) -> Self {
        Self { inner }
    }

    /// Access the underlying `reqwest::Client`.
    pub fn inner(&self) -> &Client {
        &self.inner
    }
}

impl HttpClient for ReqwestClient {
    fn send<'a>(
        &'a self,
        request: HttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>> {
        Box::pin(async move {
            let mut builder = match request.method {
                HttpMethod::Get => self.inner.get(request.url),
                HttpMethod::Head => self.inner.head(request.url),
                HttpMethod::Post => self.inner.post(request.url),
            };

            for (name, value) in request.headers {
                builder = builder.header(name, value);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }

            let resp = builder
                .send()
                .await
                .map_err(|err| HttpError::Transport(err.to_string()))?;
            let status = resp.status().as_u16();
            let headers = resp
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
                })
                .collect();
            let body = resp
                .bytes()
                .await
                .map_err(|err| HttpError::Transport(err.to_string()))?;

            Ok(HttpResponse::new(status, headers, body))
        })
    }

    fn open_body_stream<'a>(&'a self, request: HttpRequest) -> super::OpenBodyFuture<'a> {
        Box::pin(async move {
            let mut builder = match request.method {
                HttpMethod::Get => self.inner.get(request.url),
                HttpMethod::Head => self.inner.head(request.url),
                HttpMethod::Post => self.inner.post(request.url),
            };

            for (name, value) in request.headers {
                builder = builder.header(name, value);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }

            let resp = builder
                .send()
                .await
                .map_err(|err| HttpError::Transport(err.to_string()))?;
            let status = resp.status().as_u16();
            let stream = resp
                .bytes_stream()
                .map(|result| result.map_err(|err| HttpError::Transport(err.to_string())));
            Ok((status, HttpBodyStream::from_stream(Box::pin(stream))))
        })
    }
}
