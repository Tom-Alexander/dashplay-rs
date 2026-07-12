use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use url::Url;

use crate::PlayerError;

pub use web_time::Instant;

pub type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub trait HttpClientBounds: Sync {}

impl<T: Sync> HttpClientBounds for T {}

pub type LicenseFetcher =
    Arc<dyn Fn(Url, Vec<u8>) -> BoxedFuture<'static, Result<Bytes, PlayerError>> + Sync>;

pub fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    tokio::task::spawn_local(future)
}

pub async fn sleep(duration: Duration) {
    let millis = duration.as_millis().min(u128::from(u32::MAX)) as u32;
    gloo_timers::future::TimeoutFuture::new(millis).await;
}
