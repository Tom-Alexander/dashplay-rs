//! Playback metrics collection (dash.js: `DashMetrics`).
//!
//! Metrics observe download and buffer behaviour without influencing playback or ABR
//! decisions directly.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;

/// Maximum number of historical samples retained per series.
const MAX_HISTORY: usize = 256;

/// Buffer level (seconds) below which a drop is recorded as a rebuffer event.
/// Matches BOLA's low-water mark (one segment duration).
const REBUFFER_THRESHOLD_S: f64 = 4.0;

/// EWMA smoothing factor for the throughput estimate exposed in [`TrackMetricsSnapshot`].
const THROUGHPUT_EWMA_ALPHA: f64 = 0.3;

/// One measured segment-download throughput sample.
#[derive(Debug, Clone, PartialEq)]
pub struct ThroughputSample {
    /// Elapsed time since metrics collection began for this track.
    pub elapsed: Duration,
    /// Observed throughput in bits per second.
    pub throughput_bps: f64,
    /// Payload size in bytes.
    pub bytes: usize,
    /// Wall-clock time spent downloading the segment.
    pub download_duration: Duration,
}

/// Consumer-reported buffer occupancy at a point in time.
#[derive(Debug, Clone, PartialEq)]
pub struct BufferSample {
    /// Elapsed time since metrics collection began for this track.
    pub elapsed: Duration,
    /// Seconds of media buffered ahead of the playhead.
    pub buffer_s: f64,
}

/// A representation / bitrate switch on the adaptation ladder.
#[derive(Debug, Clone, PartialEq)]
pub struct BitrateSwitch {
    /// Elapsed time since metrics collection began for this track.
    pub elapsed: Duration,
    pub from_quality_index: usize,
    pub to_quality_index: usize,
    pub from_bitrate_bps: f64,
    pub to_bitrate_bps: f64,
}

/// A rebuffering event (buffer fell below the low-water mark after playback began).
#[derive(Debug, Clone, PartialEq)]
pub struct RebufferEvent {
    /// Elapsed time since metrics collection began for this track.
    pub elapsed: Duration,
    /// Buffer level in seconds when the event was recorded.
    pub buffer_s: f64,
}

/// Point-in-time playback metrics for one adaptation-set track.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackMetricsSnapshot {
    /// Time from stream start to the first media segment delivered, if playback began.
    pub startup_delay: Option<Duration>,
    /// Latest consumer-reported buffer level in seconds.
    pub buffer_s: f64,
    /// Smoothed throughput estimate in bits per second (EWMA of download samples).
    pub throughput_bps: f64,
    pub throughput_history: Vec<ThroughputSample>,
    pub buffer_history: Vec<BufferSample>,
    pub bitrate_switch_history: Vec<BitrateSwitch>,
    pub rebuffer_events: Vec<RebufferEvent>,
}

/// Collects and exposes playback metrics for a single adaptation-set track.
///
/// Clone handles share the same session. Use [`Self::snapshot`] for a point-in-time view
/// or [`Self::subscribe`] to watch updates.
#[derive(Clone)]
pub struct TrackMetrics {
    inner: Arc<Mutex<TrackMetricsInner>>,
}

struct TrackMetricsInner {
    started_at: Instant,
    first_segment_at: Option<Instant>,
    buffer_s: f64,
    throughput_ewma_bps: f64,
    was_buffer_healthy: bool,
    has_delivered_segment: bool,
    last_quality_index: Option<usize>,
    throughput_history: Vec<ThroughputSample>,
    buffer_history: Vec<BufferSample>,
    bitrate_switch_history: Vec<BitrateSwitch>,
    rebuffer_events: Vec<RebufferEvent>,
    snapshot_tx: watch::Sender<TrackMetricsSnapshot>,
}

impl TrackMetrics {
    pub(crate) fn new() -> Self {
        let snapshot = TrackMetricsSnapshot::default();
        let (snapshot_tx, _) = watch::channel(snapshot);
        Self {
            inner: Arc::new(Mutex::new(TrackMetricsInner {
                started_at: Instant::now(),
                first_segment_at: None,
                buffer_s: 0.0,
                throughput_ewma_bps: 0.0,
                was_buffer_healthy: false,
                has_delivered_segment: false,
                last_quality_index: None,
                throughput_history: Vec::new(),
                buffer_history: Vec::new(),
                bitrate_switch_history: Vec::new(),
                rebuffer_events: Vec::new(),
                snapshot_tx,
            })),
        }
    }

    /// Current metrics snapshot for this track.
    pub fn snapshot(&self) -> TrackMetricsSnapshot {
        self.with_inner(|inner| inner.build_snapshot())
    }

    /// Watch metrics snapshots; the receiver is initialized with the current snapshot.
    pub fn subscribe(&self) -> watch::Receiver<TrackMetricsSnapshot> {
        self.with_inner(|inner| inner.snapshot_tx.subscribe())
    }

    pub(crate) fn record_throughput(
        &self,
        throughput_bps: f64,
        bytes: usize,
        download_duration: Duration,
    ) {
        self.with_inner_mut(|inner| {
            if inner.throughput_ewma_bps == 0.0 {
                inner.throughput_ewma_bps = throughput_bps;
            } else {
                inner.throughput_ewma_bps = THROUGHPUT_EWMA_ALPHA * throughput_bps
                    + (1.0 - THROUGHPUT_EWMA_ALPHA) * inner.throughput_ewma_bps;
            }

            let elapsed = inner.elapsed();
            push_bounded(
                &mut inner.throughput_history,
                ThroughputSample {
                    elapsed,
                    throughput_bps,
                    bytes,
                    download_duration,
                },
            );
            inner.publish_snapshot();
        });
    }

    pub(crate) fn record_buffer(&self, buffer_s: f64) {
        self.with_inner_mut(|inner| {
            inner.buffer_s = buffer_s;
            let elapsed = inner.elapsed();
            push_bounded(
                &mut inner.buffer_history,
                BufferSample { elapsed, buffer_s },
            );

            if inner.has_delivered_segment {
                if buffer_s >= REBUFFER_THRESHOLD_S {
                    inner.was_buffer_healthy = true;
                } else if inner.was_buffer_healthy {
                    push_bounded(
                        &mut inner.rebuffer_events,
                        RebufferEvent { elapsed, buffer_s },
                    );
                    inner.was_buffer_healthy = false;
                }
            }

            inner.publish_snapshot();
        });
    }

    pub(crate) fn record_segment_delivered(&self) {
        self.with_inner_mut(|inner| {
            if inner.first_segment_at.is_none() {
                inner.first_segment_at = Some(Instant::now());
            }
            inner.has_delivered_segment = true;
            inner.publish_snapshot();
        });
    }

    pub(crate) fn record_bitrate_switch(
        &self,
        from_quality_index: usize,
        to_quality_index: usize,
        from_bitrate_bps: f64,
        to_bitrate_bps: f64,
    ) {
        if from_quality_index == to_quality_index {
            return;
        }

        self.with_inner_mut(|inner| {
            let elapsed = inner.elapsed();
            push_bounded(
                &mut inner.bitrate_switch_history,
                BitrateSwitch {
                    elapsed,
                    from_quality_index,
                    to_quality_index,
                    from_bitrate_bps,
                    to_bitrate_bps,
                },
            );
            inner.last_quality_index = Some(to_quality_index);
            inner.publish_snapshot();
        });
    }

    pub(crate) fn set_quality_index(&self, quality_index: usize) {
        self.with_inner_mut(|inner| {
            inner.last_quality_index = Some(quality_index);
        });
    }

    pub(crate) fn last_quality_index(&self) -> Option<usize> {
        self.with_inner(|inner| inner.last_quality_index)
    }

    fn with_inner<T>(&self, f: impl FnOnce(&TrackMetricsInner) -> T) -> T {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    fn with_inner_mut<T>(&self, f: impl FnOnce(&mut TrackMetricsInner) -> T) -> T {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }
}

impl TrackMetricsInner {
    fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    fn startup_delay(&self) -> Option<Duration> {
        self.first_segment_at
            .map(|t| t.duration_since(self.started_at))
    }

    fn build_snapshot(&self) -> TrackMetricsSnapshot {
        TrackMetricsSnapshot {
            startup_delay: self.startup_delay(),
            buffer_s: self.buffer_s,
            throughput_bps: self.throughput_ewma_bps,
            throughput_history: self.throughput_history.clone(),
            buffer_history: self.buffer_history.clone(),
            bitrate_switch_history: self.bitrate_switch_history.clone(),
            rebuffer_events: self.rebuffer_events.clone(),
        }
    }

    fn publish_snapshot(&mut self) {
        let _ = self.snapshot_tx.send(self.build_snapshot());
    }
}

fn push_bounded<T>(vec: &mut Vec<T>, item: T) {
    if vec.len() >= MAX_HISTORY {
        vec.remove(0);
    }
    vec.push(item);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_startup_delay_on_first_segment() {
        let metrics = TrackMetrics::new();
        metrics.record_segment_delivered();
        let snap = metrics.snapshot();
        assert!(snap.startup_delay.is_some());
    }

    #[test]
    fn records_throughput_history() {
        let metrics = TrackMetrics::new();
        metrics.record_throughput(1_000_000.0, 1000, Duration::from_millis(8));
        metrics.record_throughput(2_000_000.0, 2000, Duration::from_millis(8));

        let snap = metrics.snapshot();
        assert_eq!(snap.throughput_history.len(), 2);
        assert!(snap.throughput_bps > 0.0);
    }

    #[test]
    fn records_buffer_and_rebuffer_after_healthy_playback() {
        let metrics = TrackMetrics::new();
        metrics.record_segment_delivered();
        metrics.record_buffer(10.0);
        metrics.record_buffer(2.0);

        let snap = metrics.snapshot();
        assert_eq!(snap.buffer_history.len(), 2);
        assert_eq!(snap.rebuffer_events.len(), 1);
        assert!(snap.rebuffer_events[0].buffer_s < REBUFFER_THRESHOLD_S);
    }

    #[test]
    fn ignores_rebuffer_before_first_segment() {
        let metrics = TrackMetrics::new();
        metrics.record_buffer(0.0);
        assert!(metrics.snapshot().rebuffer_events.is_empty());
    }

    #[test]
    fn records_bitrate_switch() {
        let metrics = TrackMetrics::new();
        metrics.record_bitrate_switch(2, 0, 2_500_000.0, 300_000.0);

        let snap = metrics.snapshot();
        assert_eq!(snap.bitrate_switch_history.len(), 1);
        assert_eq!(snap.bitrate_switch_history[0].from_quality_index, 2);
        assert_eq!(snap.bitrate_switch_history[0].to_quality_index, 0);
    }

    #[test]
    fn history_is_bounded() {
        let metrics = TrackMetrics::new();
        for _ in 0..(MAX_HISTORY + 10) {
            metrics.record_throughput(1_000_000.0, 100, Duration::from_millis(1));
        }
        assert_eq!(metrics.snapshot().throughput_history.len(), MAX_HISTORY);
    }

    #[test]
    fn subscribe_receives_updates() {
        let metrics = TrackMetrics::new();
        let mut rx = metrics.subscribe();
        assert_eq!(rx.borrow().buffer_s, 0.0);
        metrics.record_buffer(5.0);
        assert_eq!(rx.borrow_and_update().buffer_s, 5.0);
    }
}
