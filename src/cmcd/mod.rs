//! Common Media Client Data (CTA-5004) and Common Media Server Data (CTA-5006).
//!
//! When enabled via [`CmcdConfig`] on [`crate::MediaPlayer::with_cmcd`], the player attaches
//! CMCD request headers (`CMCD-Request`, `CMCD-Object`, `CMCD-Status`, `CMCD-Session`) to
//! manifest and segment fetches. Response `CMSD-Static` / `CMSD-Dynamic` headers are parsed
//! and exposed via metrics and [`crate::PlayerEvent::CmsdUpdated`]; they do **not** influence
//! ABR or scheduling.
//!
//! Browser integrators should allow these request headers in CORS responses
//! (`Access-Control-Allow-Headers: CMCD-Request, CMCD-Object, CMCD-Status, CMCD-Session`).

mod cmsd;
mod encode;
mod keys;

pub use cmsd::{CmsdHop, CmsdSnapshot, CmsdValue, parse_cmsd_headers};
pub use encode::{
    CMCD_OBJECT, CMCD_REQUEST, CMCD_SESSION, CMCD_STATUS, CmcdHeaders, encode_headers,
};
pub use keys::{CmcdObjectType, CmcdRequestContext, CmcdStreamType};

use std::sync::{Arc, Mutex};

use crate::http::HttpRequest;
use crate::metrics::TrackMetrics;
use crate::platform;

/// Configuration for CMCD request reporting (CTA-5004 header mode).
///
/// Presence of a config on the player enables CMCD. A session id (`sid`) is generated at
/// construction unless overridden with [`Self::with_session_id`].
#[derive(Debug, Clone)]
pub struct CmcdConfig {
    sid: String,
    cid: Option<String>,
}

impl CmcdConfig {
    /// Enable CMCD with a freshly generated session id.
    pub fn new() -> Self {
        Self {
            sid: platform::random_uuid_v4(),
            cid: None,
        }
    }

    /// Set an optional content id (`cid`) shared across the session.
    pub fn with_content_id(mut self, cid: impl Into<String>) -> Self {
        self.cid = Some(cid.into());
        self
    }

    /// Override the auto-generated session id (`sid`).
    pub fn with_session_id(mut self, sid: impl Into<String>) -> Self {
        self.sid = sid.into();
        self
    }

    /// Session id that will be sent as CMCD `sid`.
    pub fn session_id(&self) -> &str {
        &self.sid
    }

    /// Content id that will be sent as CMCD `cid`, when set.
    pub fn content_id(&self) -> Option<&str> {
        self.cid.as_deref()
    }
}

impl Default for CmcdConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared CMCD/CMSD session state for one playback run.
#[derive(Clone)]
pub struct CmcdSession {
    config: CmcdConfig,
    last_cmsd: Arc<Mutex<Option<CmsdSnapshot>>>,
    stream_type: Arc<Mutex<CmcdStreamType>>,
}

impl CmcdSession {
    pub(crate) fn new(config: CmcdConfig) -> Self {
        Self {
            config,
            last_cmsd: Arc::new(Mutex::new(None)),
            stream_type: Arc::new(Mutex::new(CmcdStreamType::Vod)),
        }
    }

    /// Update whether the presentation is live (`st=l`) or VOD (`st=v`).
    pub(crate) fn set_stream_type(&self, stream_type: CmcdStreamType) {
        let mut guard = self.stream_type.lock().unwrap_or_else(|e| e.into_inner());
        *guard = stream_type;
    }

    pub(crate) fn stream_type(&self) -> CmcdStreamType {
        *self.stream_type.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Latest CMSD snapshot observed on any request in this session.
    pub fn last_cmsd(&self) -> Option<CmsdSnapshot> {
        self.last_cmsd
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn record_cmsd(&self, cmsd: CmsdSnapshot) {
        let mut guard = self.last_cmsd.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(cmsd);
    }

    /// Build a request context for a media/init/manifest object.
    pub(crate) fn context_for(
        &self,
        object_type: CmcdObjectType,
        metrics: Option<&TrackMetrics>,
        encoded_bitrate_bps: Option<f64>,
        object_duration_ms: Option<u64>,
        next_object_request: Option<String>,
        next_range_request: Option<String>,
    ) -> CmcdRequestContext {
        let (buffer_length_ms, measured_throughput_kbps, startup, buffer_starvation) =
            if let Some(metrics) = metrics {
                let snap = metrics.snapshot();
                let bl = if snap.buffer_s > 0.0 {
                    Some((snap.buffer_s * 1000.0).round() as u64)
                } else {
                    None
                };
                let mtp = if snap.throughput_bps > 0.0 {
                    Some((snap.throughput_bps / 1000.0).round() as u64)
                } else {
                    None
                };
                (
                    bl,
                    mtp,
                    metrics.cmcd_startup(),
                    metrics.take_cmcd_buffer_starvation(),
                )
            } else {
                (None, None, false, false)
            };

        let encoded_bitrate_kbps =
            encoded_bitrate_bps.map(|bps| (bps / 1000.0).round().max(0.0) as u64);

        CmcdRequestContext {
            session_id: self.config.sid.clone(),
            content_id: self.config.cid.clone(),
            stream_type: self.stream_type(),
            object_type,
            encoded_bitrate_kbps,
            object_duration_ms,
            buffer_length_ms,
            measured_throughput_kbps,
            startup,
            buffer_starvation,
            next_object_request,
            next_range_request,
        }
    }

    /// Attach encoded CMCD headers to `request`.
    pub(crate) fn apply(&self, request: HttpRequest, ctx: &CmcdRequestContext) -> HttpRequest {
        apply_cmcd(request, ctx)
    }
}

/// Attach CMCD headers produced from `ctx` onto `request`.
pub fn apply_cmcd(mut request: HttpRequest, ctx: &CmcdRequestContext) -> HttpRequest {
    let headers = encode_headers(ctx);
    for (name, value) in headers.iter() {
        request = request.header(name, value);
    }
    request
}
