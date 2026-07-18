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

pub use std::time::Instant;

/// Current UTC wall clock.
pub fn utc_now() -> DateTime<Utc> {
    Utc::now()
}

pub type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait HttpClientBounds: Send + Sync {}

impl<T: Send + Sync> HttpClientBounds for T {}

#[cfg(feature = "drm")]
pub type LicenseFetcher =
    Arc<dyn Fn(Url, Vec<u8>) -> BoxedFuture<'static, Result<Bytes, DrmError>> + Send + Sync>;
#[cfg(not(feature = "drm"))]
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

/// Fill `buf` with cryptographically strong random bytes when available.
pub fn fill_random(buf: &mut [u8]) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Prefer OS entropy via getrandom when linked transitively; otherwise mix timers.
    #[cfg(any(unix, windows))]
    {
        if fill_random_os(buf) {
            return;
        }
    }

    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    for (i, byte) in buf.iter_mut().enumerate() {
        let mut hasher = DefaultHasher::new();
        seed.hash(&mut hasher);
        i.hash(&mut hasher);
        std::thread::current().id().hash(&mut hasher);
        seed = hasher.finish();
        *byte = (seed & 0xff) as u8;
    }
}

#[cfg(unix)]
fn fill_random_os(buf: &mut [u8]) -> bool {
    use std::fs::File;
    use std::io::Read;
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buf))
        .is_ok()
}

#[cfg(windows)]
fn fill_random_os(buf: &mut [u8]) -> bool {
    // Fallback path uses the mixer above when OS read is unavailable.
    let _ = buf;
    false
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
