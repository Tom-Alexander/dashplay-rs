use thiserror::Error;

/// Errors from the pluggable HTTP client layer.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HttpError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("invalid response body: {0}")]
    Body(String),
    /// In-flight request or pending retry aborted (pause cancel / caller drop).
    #[error("request cancelled")]
    Cancelled,
}
