use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use url::Url;

use crate::PlayerError;

pub use std::time::Instant;

pub type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait HttpClientBounds: Send + Sync {}

impl<T: Send + Sync> HttpClientBounds for T {}

pub type LicenseFetcher =
    Arc<dyn Fn(Url, Vec<u8>) -> BoxedFuture<'static, Result<Bytes, PlayerError>> + Send + Sync>;

pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(future)
}

pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}
