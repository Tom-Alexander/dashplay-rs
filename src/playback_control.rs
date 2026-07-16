//! Playback lifecycle controls: seek, pause, resume, stop, playhead position,
//! track selection, LL-DASH latency catch-up, and observable state.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::watch;

use super::abr::QualityConstraints;
use super::clock::latency_control::{
    LatencyControlUpdate, LatencyPolicy, LiveClock, evaluate as evaluate_latency,
};
use super::platform::Instant;
use super::track_selection::TrackSelection;

/// Buffer level (seconds) at or above which playback is considered healthy for stall detection.
/// Matches [`crate::metrics`] rebuffer threshold (BOLA low-water mark).
pub(crate) const STALL_HEALTHY_BUFFER_S: f64 = 4.0;

/// Explicit playback lifecycle state (see `ARCHITECTURE.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    /// No active playback session.
    Idle,
    /// Fetching or refreshing the MPD.
    LoadingManifest,
    /// Waiting for enough media to begin or resume playback.
    Buffering,
    /// Segments are being delivered for active consumption.
    Playing,
    /// Delivery is suspended until [`PlaybackController::resume`].
    Paused,
    /// Repositioning to a new presentation time.
    Seeking,
    /// The manifest window is exhausted or playback was stopped.
    Ended,
    /// The pipeline failed; inspect the background task join result for details.
    Error,
}

/// Errors returned by playback control commands.
#[derive(Debug, Error)]
pub enum PlaybackControlError {
    #[error("playback is not active")]
    NotActive,
    #[error("playback has already stopped")]
    Stopped,
}

/// Result of advancing the internal media clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MediaClockAdvance {
    /// `true` when [`PlaybackController::presentation_time`] changed.
    pub playhead_changed: bool,
}

/// Controls an active playback session and exposes lifecycle state.
///
/// Clone handles share the same session. Subscribe with [`Self::subscribe_state`] to observe
/// [`PlaybackState`] transitions.
#[derive(Clone)]
pub struct PlaybackController {
    inner: Arc<Inner>,
}

struct Inner {
    state_tx: watch::Sender<PlaybackState>,
    state_rx: watch::Receiver<PlaybackState>,
    playhead_tx: watch::Sender<Option<Duration>>,
    playhead_rx: watch::Receiver<Option<Duration>>,
    started: AtomicBool,
    paused: AtomicBool,
    stopped: AtomicBool,
    seek_target: Mutex<Option<Duration>>,
    seek_generation: AtomicU64,
    pending_track_selection: Mutex<Option<TrackSelection>>,
    quality_constraints: Mutex<QualityConstraints>,
    /// Per-track end of delivered media on the presentation timeline.
    track_delivered_ends: Mutex<Vec<Option<Duration>>>,
    /// Consumption position (media clock); published via `playhead_tx`.
    media_clock: Mutex<Option<Duration>>,
    /// Wall sample used to advance [`Self::media_clock`] while [`PlaybackState::Playing`].
    media_clock_wall: Mutex<Option<Instant>>,
    /// `MPD@minBufferTime` used for stall recovery thresholds.
    min_buffer_s: Mutex<f64>,
    /// Whether estimated buffer has been healthy since the last stall.
    buffer_was_healthy: AtomicBool,
    /// Whether playback has ever entered [`PlaybackState::Playing`] this session.
    has_started_playing: AtomicBool,
    latency_policy: Mutex<Option<LatencyPolicy>>,
    live_clock: Mutex<Option<LiveClock>>,
    suggested_rate_tx: watch::Sender<f64>,
    suggested_rate_rx: watch::Receiver<f64>,
    live_latency_tx: watch::Sender<Option<Duration>>,
    live_latency_rx: watch::Receiver<Option<Duration>>,
    /// Avoid repeated max-latency seeks until latency recovers below `@max`.
    latency_seek_armed: AtomicBool,
}

impl PlaybackController {
    pub(crate) fn new() -> Self {
        let (state_tx, state_rx) = watch::channel(PlaybackState::Idle);
        let (playhead_tx, playhead_rx) = watch::channel(None);
        let (suggested_rate_tx, suggested_rate_rx) = watch::channel(1.0);
        let (live_latency_tx, live_latency_rx) = watch::channel(None);
        Self {
            inner: Arc::new(Inner {
                state_tx,
                state_rx,
                playhead_tx,
                playhead_rx,
                started: AtomicBool::new(false),
                paused: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
                seek_target: Mutex::new(None),
                seek_generation: AtomicU64::new(0),
                pending_track_selection: Mutex::new(None),
                quality_constraints: Mutex::new(QualityConstraints::default()),
                track_delivered_ends: Mutex::new(Vec::new()),
                media_clock: Mutex::new(None),
                media_clock_wall: Mutex::new(None),
                min_buffer_s: Mutex::new(2.0),
                buffer_was_healthy: AtomicBool::new(false),
                has_started_playing: AtomicBool::new(false),
                latency_policy: Mutex::new(None),
                live_clock: Mutex::new(None),
                suggested_rate_tx,
                suggested_rate_rx,
                live_latency_tx,
                live_latency_rx,
                latency_seek_armed: AtomicBool::new(true),
            }),
        }
    }

    /// Current lifecycle state.
    pub fn state(&self) -> PlaybackState {
        *self.inner.state_rx.borrow()
    }

    /// Watch lifecycle state transitions.
    pub fn subscribe_state(&self) -> watch::Receiver<PlaybackState> {
        self.inner.state_tx.subscribe()
    }

    /// Current presentation time (seconds from the start of the presentation).
    ///
    /// Once playback has begun, this is the internal media clock (consumption position).
    /// Before the first segment is delivered, returns `None`. During
    /// [`PlaybackState::Seeking`], reflects the pending seek target until media is
    /// delivered at the new position.
    pub fn presentation_time(&self) -> Option<Duration> {
        *self.inner.playhead_rx.borrow()
    }

    /// Watch presentation time updates.
    pub fn subscribe_presentation_time(&self) -> watch::Receiver<Option<Duration>> {
        self.inner.playhead_tx.subscribe()
    }

    /// Suggested consumption rate for LL-DASH target-latency catch-up (`1.0` when inactive).
    ///
    /// Derived from measured live latency vs `ServiceDescription/Latency@target`, clamped
    /// to `PlaybackRate` bounds. Apply this rate on the consumer media clock.
    pub fn suggested_playback_rate(&self) -> f64 {
        *self.inner.suggested_rate_rx.borrow()
    }

    /// Watch suggested consumption rate updates.
    pub fn subscribe_suggested_playback_rate(&self) -> watch::Receiver<f64> {
        self.inner.suggested_rate_tx.subscribe()
    }

    /// Measured live latency when LL-DASH latency control is active.
    pub fn live_latency(&self) -> Option<Duration> {
        *self.inner.live_latency_rx.borrow()
    }

    /// Watch live latency updates.
    pub fn subscribe_live_latency(&self) -> watch::Receiver<Option<Duration>> {
        self.inner.live_latency_tx.subscribe()
    }

    /// Suspend segment delivery until [`Self::resume`].
    pub fn pause(&self) -> Result<(), PlaybackControlError> {
        self.require_active()?;
        self.inner.paused.store(true, Ordering::Release);
        self.clear_media_clock_wall();
        let _ = self.inner.state_tx.send(PlaybackState::Paused);
        Ok(())
    }

    /// Resume delivery after [`Self::pause`].
    pub fn resume(&self) -> Result<(), PlaybackControlError> {
        self.require_active()?;
        self.inner.paused.store(false, Ordering::Release);
        self.clear_media_clock_wall();
        let _ = self.inner.state_tx.send(PlaybackState::Buffering);
        Ok(())
    }

    /// Seek to a presentation time (seconds from the start of the presentation).
    pub fn seek(&self, presentation_time: Duration) -> Result<(), PlaybackControlError> {
        self.require_active()?;
        *self
            .inner
            .seek_target
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(presentation_time);
        self.inner.seek_generation.fetch_add(1, Ordering::AcqRel);
        self.reset_track_delivered_ends();
        self.inner
            .buffer_was_healthy
            .store(false, Ordering::Release);
        self.inner
            .has_started_playing
            .store(false, Ordering::Release);
        self.set_media_clock(Some(presentation_time));
        let _ = self.inner.state_tx.send(PlaybackState::Seeking);
        Ok(())
    }

    /// Change adaptation-set preferences without restarting playback.
    ///
    /// In-flight adaptation streams are interrupted and resumed from the current
    /// presentation time (or `0` before the first segment) using `selection`.
    ///
    /// The number of track slots is fixed at [`crate::MediaPlayer::start`]: a new
    /// selection that would require more tracks than were allocated is truncated to
    /// the existing slot count. Prefer starting with the needed `max_tracks` (for
    /// example text `max_tracks(1)`) so language or role switches keep one stream
    /// per media kind.
    ///
    /// Switched tracks emit [`crate::PlayerEvent::TrackChanged`] then a fresh
    /// [`crate::PlayerEvent::Init`] before continuing segments.
    pub fn set_track_selection(
        &self,
        selection: TrackSelection,
    ) -> Result<(), PlaybackControlError> {
        self.require_active()?;
        *self
            .inner
            .pending_track_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(selection);
        self.interrupt_streams_from_playhead(PlaybackState::Buffering);
        Ok(())
    }

    /// Current user ABR quality constraints.
    pub fn quality_constraints(&self) -> QualityConstraints {
        self.inner
            .quality_constraints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Update user ABR quality constraints (dash.js: `abr.minBitrate` / `maxBitrate`,
    /// `autoSwitchBitrate`, `setQualityFor`).
    ///
    /// Fixed-quality and autoswitch changes apply on the next segment decision without
    /// interrupting delivery. Changes to min/max bitrate or data-saver rebuild ABR state
    /// by interrupting in-flight adaptation streams (same path as
    /// [`Self::set_track_selection`]).
    pub fn set_quality_constraints(
        &self,
        constraints: QualityConstraints,
    ) -> Result<(), PlaybackControlError> {
        self.require_active()?;
        let previous = self.quality_constraints();
        *self
            .inner
            .quality_constraints
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = constraints.clone();
        if previous.ladder_filter_changed(&constraints) {
            self.interrupt_streams_from_playhead(PlaybackState::Buffering);
        }
        Ok(())
    }

    /// Pin the active ladder index and disable autoswitch (dash.js: `setQualityFor`).
    pub fn set_quality_for(&self, quality_index: usize) -> Result<(), PlaybackControlError> {
        let mut constraints = self.quality_constraints();
        constraints.auto_switch = false;
        constraints.fixed_quality_index = Some(quality_index);
        self.set_quality_constraints(constraints)
    }

    /// Enable or disable automatic quality switching (dash.js: `setAutoSwitchQualityFor`).
    pub fn set_auto_switch_bitrate(&self, enabled: bool) -> Result<(), PlaybackControlError> {
        let mut constraints = self.quality_constraints();
        constraints.auto_switch = enabled;
        self.set_quality_constraints(constraints)
    }

    fn interrupt_streams_from_playhead(&self, state: PlaybackState) {
        let resume_at = self.presentation_time().unwrap_or(Duration::ZERO);
        *self
            .inner
            .seek_target
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(resume_at);
        self.inner.seek_generation.fetch_add(1, Ordering::AcqRel);
        self.reset_track_delivered_ends();
        self.inner
            .buffer_was_healthy
            .store(false, Ordering::Release);
        self.inner
            .has_started_playing
            .store(false, Ordering::Release);
        self.set_media_clock(Some(resume_at));
        let _ = self.inner.state_tx.send(state);
    }

    /// Install constraints before playback starts (used by [`crate::MediaPlayer::start`]).
    pub(crate) fn set_quality_constraints_unchecked(&self, constraints: QualityConstraints) {
        *self
            .inner
            .quality_constraints
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = constraints;
    }

    /// Stop playback. No further segments are delivered; state becomes [`PlaybackState::Ended`].
    pub fn stop(&self) -> Result<(), PlaybackControlError> {
        if !self.inner.started.load(Ordering::Acquire) {
            return Err(PlaybackControlError::NotActive);
        }
        self.inner.stopped.store(true, Ordering::Release);
        self.inner.paused.store(false, Ordering::Release);
        self.clear_media_clock_wall();
        let _ = self.inner.state_tx.send(PlaybackState::Ended);
        Ok(())
    }

    fn require_active(&self) -> Result<(), PlaybackControlError> {
        if !self.inner.started.load(Ordering::Acquire) {
            return Err(PlaybackControlError::NotActive);
        }
        if self.inner.stopped.load(Ordering::Acquire) {
            return Err(PlaybackControlError::Stopped);
        }
        Ok(())
    }

    pub(crate) fn mark_started(&self) {
        self.inner.started.store(true, Ordering::Release);
        self.inner.stopped.store(false, Ordering::Release);
        self.inner.paused.store(false, Ordering::Release);
        self.reset_track_delivered_ends();
        self.inner
            .buffer_was_healthy
            .store(false, Ordering::Release);
        self.inner
            .has_started_playing
            .store(false, Ordering::Release);
        self.set_media_clock(None);
        self.clear_latency_control();
        let _ = self.inner.state_tx.send(PlaybackState::LoadingManifest);
    }

    /// Install `MPD@minBufferTime` used for stall recovery.
    pub(crate) fn set_min_buffer_s(&self, min_buffer_s: f64) {
        let value = if min_buffer_s.is_finite() && min_buffer_s >= 0.0 {
            min_buffer_s
        } else {
            2.0
        };
        *self
            .inner
            .min_buffer_s
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = value;
    }

    pub(crate) fn min_buffer_s(&self) -> f64 {
        *self
            .inner
            .min_buffer_s
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Install LL-DASH latency policy and a live clock anchored at `since_ast`.
    pub(crate) fn set_latency_control(
        &self,
        policy: Option<LatencyPolicy>,
        since_ast: Option<Duration>,
    ) {
        *self
            .inner
            .latency_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = policy;
        *self
            .inner
            .live_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = since_ast.map(LiveClock::new);
        self.inner.latency_seek_armed.store(true, Ordering::Release);
        if policy.is_none() {
            let _ = self.inner.suggested_rate_tx.send(1.0);
            let _ = self.inner.live_latency_tx.send(None);
        }
    }

    fn clear_latency_control(&self) {
        self.set_latency_control(None, None);
    }

    /// Recompute catch-up rate (and optional max-latency seek) after playhead movement.
    pub(crate) fn refresh_latency_control(&self) -> Option<LatencyControlUpdate> {
        if self.is_paused() || self.is_stopped() {
            return None;
        }
        if matches!(self.state(), PlaybackState::Seeking) {
            return None;
        }
        let policy = (*self
            .inner
            .latency_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner()))?;
        let clock = self
            .inner
            .live_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let presentation_time = self.presentation_time()?;
        let previous_rate = *self.inner.suggested_rate_rx.borrow();
        let mut update = evaluate_latency(&clock, presentation_time, &policy, previous_rate);

        let _ = self.inner.live_latency_tx.send(Some(update.latency));
        if update.rate_changed {
            let _ = self.inner.suggested_rate_tx.send(update.rate);
        }

        if update.seek_target.is_some() {
            if !self.inner.latency_seek_armed.swap(false, Ordering::AcqRel) {
                update.seek_target = None;
            }
        } else {
            self.inner.latency_seek_armed.store(true, Ordering::Release);
        }

        Some(update)
    }

    /// `Latency@target` when latency control is active.
    pub(crate) fn latency_target(&self) -> Option<Duration> {
        self.inner
            .latency_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|p| p.target)
    }

    /// Record a delivered media segment and extend the buffered range for `track_idx`.
    ///
    /// Initializes the media clock to `start` when unset. Returns `true` when the media
    /// clock was initialized by this call.
    pub(crate) fn record_segment_delivery(
        &self,
        track_idx: usize,
        start: Duration,
        end: Duration,
    ) -> bool {
        let end = end.max(start);
        let mut ends = self
            .inner
            .track_delivered_ends
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if track_idx >= ends.len() {
            ends.resize(track_idx + 1, None);
        }
        ends[track_idx] = Some(match ends[track_idx] {
            Some(prev) => prev.max(end),
            None => end,
        });
        drop(ends);

        let mut clock = self
            .inner
            .media_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if clock.is_some() {
            return false;
        }
        *clock = Some(start);
        drop(clock);
        self.clear_media_clock_wall();
        self.set_presentation_time(Some(start))
    }

    /// Estimated buffered media ahead of the media clock for `track_idx`, in seconds.
    pub(crate) fn estimated_buffer_s(&self, track_idx: usize) -> f64 {
        let ends = self
            .inner
            .track_delivered_ends
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(end) = ends.get(track_idx).copied().flatten() else {
            return 0.0;
        };
        drop(ends);
        let Some(clock) = *self
            .inner
            .media_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        else {
            return 0.0;
        };
        end.saturating_sub(clock).as_secs_f64()
    }

    /// Advance the media clock by a fixed duration (test helper / deterministic drain).
    #[cfg(test)]
    pub(crate) fn advance_media_clock_by(&self, dt: Duration) -> MediaClockAdvance {
        if self.is_paused() || self.is_stopped() || self.state() != PlaybackState::Playing {
            return MediaClockAdvance {
                playhead_changed: false,
            };
        }
        if dt.is_zero() {
            return MediaClockAdvance {
                playhead_changed: false,
            };
        }

        let mut clock_guard = self
            .inner
            .media_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(clock) = *clock_guard else {
            return MediaClockAdvance {
                playhead_changed: false,
            };
        };

        let mut new_clock = clock + dt;
        if let Some(cap) = min_delivered_end(
            &self
                .inner
                .track_delivered_ends
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        ) {
            if new_clock > cap {
                new_clock = cap;
            }
        }

        if new_clock == clock {
            return MediaClockAdvance {
                playhead_changed: false,
            };
        }

        *clock_guard = Some(new_clock);
        drop(clock_guard);
        self.clear_media_clock_wall();
        let changed = self.set_presentation_time(Some(new_clock));
        MediaClockAdvance {
            playhead_changed: changed,
        }
    }

    /// Advance the media clock while [`PlaybackState::Playing`] using wall time and the
    /// suggested playback rate. Caps at the earliest delivered-media end across tracks.
    pub(crate) fn advance_media_clock(&self) -> MediaClockAdvance {
        if self.is_paused() || self.is_stopped() {
            self.clear_media_clock_wall();
            return MediaClockAdvance {
                playhead_changed: false,
            };
        }
        if self.state() != PlaybackState::Playing {
            self.clear_media_clock_wall();
            return MediaClockAdvance {
                playhead_changed: false,
            };
        }

        let rate = self.suggested_playback_rate();
        let rate = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };

        let now = Instant::now();
        let mut clock_guard = self
            .inner
            .media_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(clock) = *clock_guard else {
            return MediaClockAdvance {
                playhead_changed: false,
            };
        };

        let mut wall_guard = self
            .inner
            .media_clock_wall
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(prev_wall) = *wall_guard else {
            *wall_guard = Some(now);
            return MediaClockAdvance {
                playhead_changed: false,
            };
        };

        let dt_s = now.saturating_duration_since(prev_wall).as_secs_f64() * rate;
        *wall_guard = Some(now);
        if dt_s <= 0.0 {
            return MediaClockAdvance {
                playhead_changed: false,
            };
        }

        let mut new_clock = clock + Duration::from_secs_f64(dt_s);
        if let Some(cap) = min_delivered_end(
            &self
                .inner
                .track_delivered_ends
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        ) {
            if new_clock > cap {
                new_clock = cap;
            }
        }

        if new_clock == clock {
            return MediaClockAdvance {
                playhead_changed: false,
            };
        }

        *clock_guard = Some(new_clock);
        drop(clock_guard);
        drop(wall_guard);
        let changed = self.set_presentation_time(Some(new_clock));
        MediaClockAdvance {
            playhead_changed: changed,
        }
    }

    /// Align delivered end / media clock so `estimated_buffer_s(track_idx)` matches `buffer_s`.
    ///
    /// Used when the consumer reports the real decoder buffer via
    /// [`crate::BufferFeedback::report`]. Extends or shrinks the track's delivered end so the
    /// estimate matches even when the reported occupancy exceeds library-delivered media
    /// (common while the decoder still holds previously buffered content).
    pub(crate) fn resync_media_clock_from_buffer(&self, track_idx: usize, buffer_s: f64) {
        let buffer_s = if buffer_s.is_finite() && buffer_s > 0.0 {
            buffer_s
        } else {
            0.0
        };

        let mut clock_guard = self
            .inner
            .media_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let clock = clock_guard.unwrap_or(Duration::ZERO);
        let initialized_clock = clock_guard.is_none();
        if initialized_clock {
            *clock_guard = Some(clock);
        }
        drop(clock_guard);
        if initialized_clock {
            self.clear_media_clock_wall();
            let _ = self.set_presentation_time(Some(clock));
        } else {
            self.clear_media_clock_wall();
        }

        let needed_end = clock + Duration::from_secs_f64(buffer_s);
        let mut ends = self
            .inner
            .track_delivered_ends
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if track_idx >= ends.len() {
            ends.resize(track_idx + 1, None);
        }
        ends[track_idx] = Some(needed_end);
    }

    /// Update [`PlaybackState`] from estimated buffer (stall / recovery).
    ///
    /// Returns `true` when the lifecycle state changed.
    pub(crate) fn update_stall_state(&self, buffer_s: f64) -> bool {
        if self.is_paused() || self.is_stopped() {
            return false;
        }
        if matches!(
            self.state(),
            PlaybackState::Seeking
                | PlaybackState::Ended
                | PlaybackState::Error
                | PlaybackState::LoadingManifest
                | PlaybackState::Idle
                | PlaybackState::Paused
        ) {
            return false;
        }

        let min_buffer_s = self.min_buffer_s();
        let before = self.state();

        if buffer_s >= STALL_HEALTHY_BUFFER_S {
            self.inner.buffer_was_healthy.store(true, Ordering::Release);
        }

        if buffer_s <= 0.0 && self.inner.buffer_was_healthy.swap(false, Ordering::AcqRel) {
            self.clear_media_clock_wall();
            let _ = self.inner.state_tx.send(PlaybackState::Buffering);
        } else if before == PlaybackState::Buffering && buffer_s >= min_buffer_s {
            self.inner
                .has_started_playing
                .store(true, Ordering::Release);
            let _ = self.inner.state_tx.send(PlaybackState::Playing);
        }

        self.state() != before
    }

    /// Enter [`PlaybackState::Playing`] after media delivery when allowed.
    ///
    /// Startup (and post-seek) transitions to Playing as soon as any media is buffered.
    /// Mid-playback stall recovery requires buffer ≥ `minBufferTime`.
    pub(crate) fn on_media_delivered(&self, track_idx: usize) {
        if self.is_paused() || self.is_stopped() {
            return;
        }
        if matches!(
            self.state(),
            PlaybackState::Seeking | PlaybackState::Ended | PlaybackState::Error
        ) {
            return;
        }

        let buffer_s = self.estimated_buffer_s(track_idx);
        if !self.inner.has_started_playing.load(Ordering::Acquire) && buffer_s > 0.0 {
            self.inner
                .has_started_playing
                .store(true, Ordering::Release);
            let _ = self.inner.state_tx.send(PlaybackState::Playing);
            if buffer_s >= STALL_HEALTHY_BUFFER_S {
                self.inner.buffer_was_healthy.store(true, Ordering::Release);
            }
            return;
        }

        let _ = self.update_stall_state(buffer_s);
    }

    fn set_media_clock(&self, presentation_time: Option<Duration>) {
        *self
            .inner
            .media_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = presentation_time;
        self.clear_media_clock_wall();
        self.set_presentation_time(presentation_time);
    }

    fn clear_media_clock_wall(&self) {
        *self
            .inner
            .media_clock_wall
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(crate) fn reset_track_delivered_ends(&self) {
        self.inner
            .track_delivered_ends
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn set_presentation_time(&self, presentation_time: Option<Duration>) -> bool {
        if *self.inner.playhead_rx.borrow() == presentation_time {
            return false;
        }
        let _ = self.inner.playhead_tx.send(presentation_time);
        true
    }

    pub(crate) fn set_state(&self, state: PlaybackState) {
        if self.is_stopped() {
            if state == PlaybackState::Error {
                let _ = self.inner.state_tx.send(state);
            }
            return;
        }
        if self.is_paused()
            && !matches!(
                state,
                PlaybackState::Paused
                    | PlaybackState::Seeking
                    | PlaybackState::Ended
                    | PlaybackState::Error
            )
        {
            return;
        }
        let _ = self.inner.state_tx.send(state);
    }

    pub(crate) fn mark_error(&self) {
        let _ = self.inner.state_tx.send(PlaybackState::Error);
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.inner.stopped.load(Ordering::Acquire)
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::Acquire)
    }

    pub(crate) fn seek_generation(&self) -> u64 {
        self.inner.seek_generation.load(Ordering::Acquire)
    }

    /// Take a pending seek target, if any.
    pub(crate) fn take_seek_target(&self) -> Option<Duration> {
        self.inner
            .seek_target
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Take a pending track-selection update, if any.
    pub(crate) fn take_track_selection(&self) -> Option<TrackSelection> {
        self.inner
            .pending_track_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    pub(crate) async fn wait_while_paused(&self) {
        while self.is_paused() && !self.is_stopped() {
            crate::platform::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn min_delivered_end(ends: &[Option<Duration>]) -> Option<Duration> {
    ends.iter().filter_map(|p| *p).min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abr::QualityConstraints;
    use crate::clock::latency_control::LatencyPolicy;

    #[test]
    fn state_tracks_commands_on_shared_handle() {
        let a = PlaybackController::new();
        let b = a.clone();
        a.mark_started();
        assert_eq!(b.state(), PlaybackState::LoadingManifest);
        a.pause().unwrap();
        assert_eq!(b.state(), PlaybackState::Paused);
        a.stop().unwrap();
        assert_eq!(b.state(), PlaybackState::Ended);
    }

    #[test]
    fn presentation_time_tracks_delivery_and_seek() {
        let playback = PlaybackController::new();
        playback.mark_started();
        assert_eq!(playback.presentation_time(), None);

        assert!(playback.record_segment_delivery(
            0,
            Duration::from_secs(4),
            Duration::from_secs(8)
        ));
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(4)));
        assert!((playback.estimated_buffer_s(0) - 4.0).abs() < 1e-9);

        playback.seek(Duration::from_secs(10)).unwrap();
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(10)));
        assert_eq!(playback.estimated_buffer_s(0), 0.0);

        assert!(!playback.record_segment_delivery(
            0,
            Duration::from_secs(8),
            Duration::from_secs(12)
        ));
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(10)));
        assert!((playback.estimated_buffer_s(0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn estimated_buffer_uses_earliest_gap_per_track() {
        let playback = PlaybackController::new();
        playback.mark_started();

        assert!(playback.record_segment_delivery(
            0,
            Duration::from_secs(0),
            Duration::from_secs(10)
        ));
        assert!(!playback.record_segment_delivery(
            1,
            Duration::from_secs(0),
            Duration::from_secs(6)
        ));
        assert!((playback.estimated_buffer_s(0) - 10.0).abs() < 1e-9);
        assert!((playback.estimated_buffer_s(1) - 6.0).abs() < 1e-9);
    }

    #[test]
    fn resync_media_clock_from_buffer_aligns_estimate() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.record_segment_delivery(0, Duration::ZERO, Duration::from_secs(10));
        playback.resync_media_clock_from_buffer(0, 3.0);
        assert!((playback.estimated_buffer_s(0) - 3.0).abs() < 1e-9);
        assert_eq!(playback.presentation_time(), Some(Duration::ZERO));
    }

    #[test]
    fn resync_can_extend_delivered_end_beyond_library_media() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.record_segment_delivery(0, Duration::ZERO, Duration::from_secs(4));
        playback.resync_media_clock_from_buffer(0, 30.0);
        assert!((playback.estimated_buffer_s(0) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn update_stall_state_enters_buffering_on_underrun() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.set_min_buffer_s(2.0);
        playback.set_state(PlaybackState::Playing);
        playback
            .inner
            .has_started_playing
            .store(true, Ordering::Release);
        playback.record_segment_delivery(0, Duration::ZERO, Duration::from_secs(10));
        assert!(!playback.update_stall_state(5.0));
        assert_eq!(playback.state(), PlaybackState::Playing);
        assert!(playback.update_stall_state(0.0));
        assert_eq!(playback.state(), PlaybackState::Buffering);
        assert!(playback.update_stall_state(2.5));
        assert_eq!(playback.state(), PlaybackState::Playing);
    }

    #[test]
    fn subscribe_presentation_time_receives_updates() {
        let playback = PlaybackController::new();
        let mut rx = playback.subscribe_presentation_time();
        playback.mark_started();
        assert_eq!(*rx.borrow_and_update(), None);

        playback.record_segment_delivery(0, Duration::from_secs(2), Duration::from_secs(6));
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), Some(Duration::from_secs(2)));
    }

    #[test]
    fn set_track_selection_resumes_from_playhead() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.record_segment_delivery(0, Duration::from_secs(4), Duration::from_secs(8));

        let selection = TrackSelection::default().with_audio(
            crate::TrackPreference::default()
                .language("fr")
                .max_tracks(1),
        );
        playback.set_track_selection(selection.clone()).unwrap();
        assert_eq!(playback.state(), PlaybackState::Buffering);
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(4)));
        assert_eq!(playback.take_seek_target(), Some(Duration::from_secs(4)));
        assert_eq!(playback.take_track_selection(), Some(selection));
    }

    #[test]
    fn set_quality_for_does_not_interrupt_streams() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.record_segment_delivery(0, Duration::from_secs(4), Duration::from_secs(8));
        let before = playback.seek_generation();
        playback.set_quality_for(1).unwrap();
        assert_eq!(playback.seek_generation(), before);
        assert!(!playback.quality_constraints().auto_switch);
        assert_eq!(playback.quality_constraints().fixed_quality_index, Some(1));
    }

    #[test]
    fn set_max_bitrate_interrupts_streams_for_abr_rebuild() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.record_segment_delivery(0, Duration::from_secs(4), Duration::from_secs(8));
        let before = playback.seek_generation();
        playback
            .set_quality_constraints(QualityConstraints::default().max_bitrate_bps(500_000))
            .unwrap();
        assert!(playback.seek_generation() > before);
        assert_eq!(playback.take_seek_target(), Some(Duration::from_secs(4)));
    }

    #[test]
    fn latency_control_suggests_catch_up_rate() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.set_state(PlaybackState::Playing);
        playback.set_latency_control(
            Some(LatencyPolicy {
                target: Duration::from_millis(3500),
                min: Some(Duration::from_secs(2)),
                max: Some(Duration::from_secs(6)),
                rate_min: 0.96,
                rate_max: 1.04,
            }),
            Some(Duration::from_secs(20)),
        );
        playback.record_segment_delivery(0, Duration::from_secs(15), Duration::from_secs(19));

        let update = playback.refresh_latency_control().expect("update");
        assert!(update.rate > 1.0);
        assert!(update.rate_changed);
        assert!((playback.suggested_playback_rate() - update.rate).abs() < 1e-9);
        assert!(playback.live_latency().is_some());
        assert_eq!(playback.latency_target(), Some(Duration::from_millis(3500)));
    }

    #[test]
    fn latency_control_seeks_when_above_max() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.set_state(PlaybackState::Playing);
        playback.set_latency_control(
            Some(LatencyPolicy {
                target: Duration::from_millis(3500),
                min: Some(Duration::from_secs(2)),
                max: Some(Duration::from_secs(6)),
                rate_min: 0.96,
                rate_max: 1.04,
            }),
            Some(Duration::from_secs(20)),
        );
        // latency = 20 - 10 = 10s > max 6s
        playback.record_segment_delivery(0, Duration::from_secs(10), Duration::from_secs(14));
        let update = playback.refresh_latency_control().expect("update");
        let seek = update.seek_target.expect("seek target");
        assert!(
            (seek.as_secs_f64() - 16.5).abs() < 0.05,
            "expected ~16.5s target edge, got {seek:?}"
        );
        // Second evaluation should not re-arm a seek while still over max.
        let update2 = playback.refresh_latency_control().expect("update");
        assert!(update2.seek_target.is_none());
    }

    #[test]
    fn advance_media_clock_by_drains_buffer_while_playing() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.set_state(PlaybackState::Playing);
        playback.record_segment_delivery(0, Duration::ZERO, Duration::from_secs(4));

        assert!(
            playback
                .advance_media_clock_by(Duration::from_secs(1))
                .playhead_changed
        );
        assert!((playback.estimated_buffer_s(0) - 3.0).abs() < 1e-9);
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn pause_freezes_media_clock() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.set_state(PlaybackState::Playing);
        playback.record_segment_delivery(0, Duration::ZERO, Duration::from_secs(8));
        let _ = playback.advance_media_clock_by(Duration::from_secs(1));
        let before = playback.estimated_buffer_s(0);

        playback.pause().unwrap();
        assert!(
            !playback
                .advance_media_clock_by(Duration::from_secs(5))
                .playhead_changed
        );
        assert!((playback.estimated_buffer_s(0) - before).abs() < 1e-9);
    }
}
