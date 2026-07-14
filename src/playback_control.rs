//! Playback lifecycle controls: seek, pause, resume, stop, playhead position,
//! track selection, and observable state.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::watch;

use super::track_selection::TrackSelection;

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
    /// Per-track start time of the last delivered segment (presentation timeline).
    track_delivery_positions: Mutex<Vec<Option<Duration>>>,
}

impl PlaybackController {
    pub(crate) fn new() -> Self {
        let (state_tx, state_rx) = watch::channel(PlaybackState::Idle);
        let (playhead_tx, playhead_rx) = watch::channel(None);
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
                track_delivery_positions: Mutex::new(Vec::new()),
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
    /// Returns the synchronized delivery frontier across adaptation-set tracks: the minimum
    /// start time of the last delivered segment on each active track. Before the first segment
    /// is delivered, returns `None`. During [`PlaybackState::Seeking`], reflects the pending
    /// seek target until new segments arrive.
    pub fn presentation_time(&self) -> Option<Duration> {
        *self.inner.playhead_rx.borrow()
    }

    /// Watch presentation time updates.
    pub fn subscribe_presentation_time(&self) -> watch::Receiver<Option<Duration>> {
        self.inner.playhead_tx.subscribe()
    }

    /// Suspend segment delivery until [`Self::resume`].
    pub fn pause(&self) -> Result<(), PlaybackControlError> {
        self.require_active()?;
        self.inner.paused.store(true, Ordering::Release);
        let _ = self.inner.state_tx.send(PlaybackState::Paused);
        Ok(())
    }

    /// Resume delivery after [`Self::pause`].
    pub fn resume(&self) -> Result<(), PlaybackControlError> {
        self.require_active()?;
        self.inner.paused.store(false, Ordering::Release);
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
        self.reset_track_delivery_positions();
        self.set_presentation_time(Some(presentation_time));
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
        let resume_at = self.presentation_time().unwrap_or(Duration::ZERO);
        *self
            .inner
            .seek_target
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(resume_at);
        self.inner.seek_generation.fetch_add(1, Ordering::AcqRel);
        self.reset_track_delivery_positions();
        self.set_presentation_time(Some(resume_at));
        let _ = self.inner.state_tx.send(PlaybackState::Buffering);
        Ok(())
    }

    /// Stop playback. No further segments are delivered; state becomes [`PlaybackState::Ended`].
    pub fn stop(&self) -> Result<(), PlaybackControlError> {
        if !self.inner.started.load(Ordering::Acquire) {
            return Err(PlaybackControlError::NotActive);
        }
        self.inner.stopped.store(true, Ordering::Release);
        self.inner.paused.store(false, Ordering::Release);
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
        self.reset_track_delivery_positions();
        self.set_presentation_time(None);
        let _ = self.inner.state_tx.send(PlaybackState::LoadingManifest);
    }

    /// Record a delivered segment and update the session playhead when it advances.
    ///
    /// Returns `true` when the synchronized presentation time changed.
    pub(crate) fn record_segment_delivery(
        &self,
        track_idx: usize,
        presentation_time: Duration,
    ) -> bool {
        let mut positions = self
            .inner
            .track_delivery_positions
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if track_idx >= positions.len() {
            positions.resize(track_idx + 1, None);
        }
        positions[track_idx] = Some(presentation_time);
        let frontier = synchronized_delivery_frontier(&positions);
        drop(positions);
        self.set_presentation_time(frontier)
    }

    pub(crate) fn reset_track_delivery_positions(&self) {
        self.inner
            .track_delivery_positions
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

fn synchronized_delivery_frontier(positions: &[Option<Duration>]) -> Option<Duration> {
    positions.iter().filter_map(|p| *p).min()
}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert!(playback.record_segment_delivery(0, Duration::from_secs(4)));
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(4)));

        playback.seek(Duration::from_secs(10)).unwrap();
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(10)));

        assert!(playback.record_segment_delivery(0, Duration::from_secs(8)));
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(8)));
    }

    #[test]
    fn presentation_time_uses_minimum_across_tracks() {
        let playback = PlaybackController::new();
        playback.mark_started();

        assert!(playback.record_segment_delivery(0, Duration::from_secs(10)));
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(10)));

        assert!(playback.record_segment_delivery(1, Duration::from_secs(6)));
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(6)));

        assert!(!playback.record_segment_delivery(0, Duration::from_secs(12)));
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(6)));

        assert!(playback.record_segment_delivery(1, Duration::from_secs(14)));
        assert_eq!(playback.presentation_time(), Some(Duration::from_secs(12)));
    }

    #[test]
    fn subscribe_presentation_time_receives_updates() {
        let playback = PlaybackController::new();
        let mut rx = playback.subscribe_presentation_time();
        playback.mark_started();
        assert_eq!(*rx.borrow_and_update(), None);

        playback.record_segment_delivery(0, Duration::from_secs(2));
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), Some(Duration::from_secs(2)));
    }

    #[test]
    fn set_track_selection_resumes_from_playhead() {
        let playback = PlaybackController::new();
        playback.mark_started();
        playback.record_segment_delivery(0, Duration::from_secs(4));

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
}
