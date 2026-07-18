use bytes::Bytes;
use url::Url;

use super::HttpMethod;

/// Outbound HTTP request built by callers and executed by an [`HttpClient`](super::HttpClient).
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub(crate) method: HttpMethod,
    pub(crate) url: Url,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Option<Bytes>,
}

impl HttpRequest {
    /// HTTP method for this request.
    pub fn method(&self) -> HttpMethod {
        self.method
    }

    /// Request URL.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Request headers as `(name, value)` pairs.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// `GET` request for `url`.
    pub fn get(url: Url) -> Self {
        Self {
            method: HttpMethod::Get,
            url,
            headers: Vec::new(),
            body: None,
        }
    }

    /// `HEAD` request for `url`.
    pub fn head(url: Url) -> Self {
        Self {
            method: HttpMethod::Head,
            url,
            headers: Vec::new(),
            body: None,
        }
    }

    /// `POST` request for `url` with an optional body.
    pub fn post(url: Url, body: impl Into<Bytes>) -> Self {
        Self {
            method: HttpMethod::Post,
            url,
            headers: Vec::new(),
            body: Some(body.into()),
        }
    }

    /// Add a request header.
    pub fn header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.headers
            .push((name.as_ref().to_string(), value.as_ref().to_string()));
        self
    }

    /// Set an inclusive HTTP `Range` header (`bytes=start-end`).
    pub fn byte_range(self, start: u64, end: u64) -> Self {
        self.header("Range", format!("bytes={start}-{end}"))
    }
}
