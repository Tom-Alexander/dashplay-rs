use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use url::Url;

#[cfg(not(feature = "drm"))]
use crate::PlayerError;
#[cfg(feature = "drm")]
use crate::drm::DrmError;

pub use web_time::Instant;

/// Current UTC wall clock (`std::time::SystemTime` panics on this target).
pub fn utc_now() -> DateTime<Utc> {
    let duration = web_time::SystemTime::now()
        .duration_since(web_time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    DateTime::<Utc>::from_timestamp(duration.as_secs() as i64, duration.subsec_nanos())
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

pub type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

pub trait HttpClientBounds: Sync {}

impl<T: Sync> HttpClientBounds for T {}

#[cfg(feature = "drm")]
pub type LicenseFetcher =
    Arc<dyn Fn(Url, Vec<u8>) -> BoxedFuture<'static, Result<Bytes, DrmError>> + Sync>;
#[cfg(not(feature = "drm"))]
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

/// Fill `buf` with random bytes (deterministic mixer seeded from monotonic time).
pub fn fill_random(buf: &mut [u8]) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Prefer `web_time` over `std::time::SystemTime`, which panics on wasm32-unknown-unknown.
    let mut seed = web_time::SystemTime::now()
        .duration_since(web_time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    for (i, byte) in buf.iter_mut().enumerate() {
        let mut hasher = DefaultHasher::new();
        seed.hash(&mut hasher);
        i.hash(&mut hasher);
        seed = hasher.finish().wrapping_mul(0x9e37_79b9_7f4a_7c15);
        *byte = (seed & 0xff) as u8;
    }
}

/// Generate an RFC 4122 version-4 UUID string (lowercase hex).
pub fn random_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}
