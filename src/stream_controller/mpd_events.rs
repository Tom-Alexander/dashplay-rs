//! Deduplicated emission of MPD `EventStream` events across manifest refreshes.

use std::collections::HashSet;

use dash_mpd::Period;

use super::super::media_events;
use super::super::types::{PlayerEvent, PlayerTrack};

/// Tracks MPD timed events already delivered to track subscribers.
#[derive(Debug, Default)]
pub(crate) struct MpdEventDedup {
    emitted: HashSet<(String, u64, u64)>,
}

impl MpdEventDedup {
    pub(crate) fn emit_new_events(&mut self, period: &Period, tracks: &[PlayerTrack]) {
        for event in media_events::mpd_events_for_period(period) {
            let key = (
                event.scheme_id_uri.clone(),
                event.presentation_time,
                event.id.unwrap_or(0),
            );
            if !self.emitted.insert(key) {
                continue;
            }
            for t in tracks {
                let _ = t.tx.send(PlayerEvent::MediaEvent(event.clone()));
            }
        }
    }
}
