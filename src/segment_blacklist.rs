//! URLs that returned a failing HTTP status for a segment/init fetch are recorded so we
//! avoid repeating the same request and can try another representation instead.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use url::Url;

#[derive(Clone, Default)]
pub(crate) struct SegmentBlacklist {
    urls: Arc<RwLock<HashSet<String>>>,
}

impl SegmentBlacklist {
    pub(crate) fn new() -> Self {
        Self {
            urls: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    pub(crate) fn contains_url(&self, url: &Url) -> bool {
        self.urls
            .read()
            .map(|s| s.contains(url.as_str()))
            .unwrap_or(false)
    }

    pub(crate) fn insert_url(&self, url: &Url) {
        if let Ok(mut guard) = self.urls.write() {
            guard.insert(url.as_str().to_string());
        }
    }
}
