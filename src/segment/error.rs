use thiserror::Error;

use crate::http::HttpError;

/// Errors from segment HTTP fetch and representation fallback.
#[derive(Debug, Error)]
pub enum SegmentError {
    #[error("request: {0}")]
    Request(#[from] HttpError),
    #[error("url: {0}")]
    Url(#[from] url::ParseError),
    #[error("segment URL blacklisted: {0}")]
    Blacklisted(String),
    #[error("segment request failed: HTTP {status} for {url}")]
    RequestFailed { status: u16, url: String },
    #[error("all representation attempts failed for a segment")]
    ExhaustedRepresentations,
}
