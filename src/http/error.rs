use thiserror::Error;

/// Errors from the pluggable HTTP client layer.
#[derive(Debug, Error)]
pub enum HttpError {
    #[error("transport: {0}")]
    Transport(String),
    #[error("invalid response body: {0}")]
    Body(String),
}
