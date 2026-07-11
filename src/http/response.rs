use bytes::Bytes;

use super::HttpError;

/// HTTP response returned by an [`HttpClient`](super::HttpClient).
#[derive(Debug, Clone)]
pub struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Bytes,
}

impl HttpResponse {
    /// Build a response for custom [`HttpClient`](super::HttpClient) implementations.
    pub fn new(status: u16, headers: Vec<(String, String)>, body: Bytes) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// HTTP status code.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Whether the status is in the 2xx range.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&name))
            .map(|(_, v)| v.as_str())
    }

    /// Response body bytes.
    pub fn bytes(&self) -> &Bytes {
        &self.body
    }

    /// Consume the response and return the body.
    pub fn into_bytes(self) -> Bytes {
        self.body
    }

    /// Decode the body as UTF-8 text.
    pub fn text(self) -> Result<String, HttpError> {
        String::from_utf8(self.body.to_vec()).map_err(|err| HttpError::Body(err.to_string()))
    }
}
