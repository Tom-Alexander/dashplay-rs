use std::pin::Pin;

use bytes::Bytes;

use super::HttpError;

enum BodyInner {
    Buffered { data: Bytes, offset: usize },
    Streaming(Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, HttpError>> + Send>>),
}

/// Progressive HTTP response body for Low-Latency DASH partial segment transfer.
pub struct HttpBodyStream {
    inner: BodyInner,
}

impl HttpBodyStream {
    pub(crate) fn from_bytes(data: Bytes) -> Self {
        Self {
            inner: BodyInner::Buffered { data, offset: 0 },
        }
    }

    pub(crate) fn from_stream(
        stream: Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, HttpError>> + Send>>,
    ) -> Self {
        Self {
            inner: BodyInner::Streaming(stream),
        }
    }

    pub(crate) async fn next_chunk(&mut self) -> Result<Option<Bytes>, HttpError> {
        match &mut self.inner {
            BodyInner::Buffered { data, offset } => {
                if *offset >= data.len() {
                    return Ok(None);
                }
                let rest = data.slice(*offset..);
                *offset = data.len();
                Ok(Some(rest))
            }
            BodyInner::Streaming(stream) => {
                use futures_util::StreamExt;
                match stream.next().await {
                    Some(Ok(bytes)) => Ok(Some(bytes)),
                    Some(Err(err)) => Err(err),
                    None => Ok(None),
                }
            }
        }
    }
}
