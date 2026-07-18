use super::body_stream::HttpBodyStream;

/// Progressive HTTP response from [`HttpClient::open_body_stream`](super::HttpClient::open_body_stream).
pub struct HttpStreamResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: HttpBodyStream,
}

impl HttpStreamResponse {
    /// Build a streaming response for custom [`HttpClient`](super::HttpClient) implementations.
    pub fn new(status: u16, headers: Vec<(String, String)>, body: HttpBodyStream) -> Self {
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

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All response headers as `(name, value)` pairs.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// Borrow the progressive body stream.
    pub fn body_mut(&mut self) -> &mut HttpBodyStream {
        &mut self.body
    }

    /// Split into status, headers, and body.
    pub fn into_parts(self) -> (u16, Vec<(String, String)>, HttpBodyStream) {
        (self.status, self.headers, self.body)
    }
}
