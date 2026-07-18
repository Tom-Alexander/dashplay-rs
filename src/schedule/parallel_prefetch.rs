//! Parallel media-segment prefetch within a single adaptation-set stream.
//!
//! Downloads are cooperative (`join_all` on the caller's task): init/index resolution stays
//! sequential so shared caches remain race-free, then media GETs run concurrently up to
//! [`MAX_PARALLEL_SEGMENT_FETCHES`].

use super::buffer_target::BufferTarget;

/// Maximum concurrent media segment HTTP downloads per track.
pub(crate) const MAX_PARALLEL_SEGMENT_FETCHES: usize = 2;

/// How many next media segments may be scheduled given buffer occupancy and remaining work.
pub(crate) fn prefetch_batch_len(
    buffer_target: &BufferTarget,
    buffer_s: f64,
    media_segments_delivered: usize,
    remaining_segments: usize,
) -> usize {
    if remaining_segments == 0 {
        return 0;
    }
    // Fetch the first media segment alone so ABR can observe throughput before parallelising.
    let max = if media_segments_delivered == 0 {
        1
    } else {
        MAX_PARALLEL_SEGMENT_FETCHES
    };
    let mut n = 0usize;
    while n < max
        && n < remaining_segments
        && buffer_target.should_fetch(buffer_s, media_segments_delivered.saturating_add(n))
    {
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn batch_starts_with_bootstrap_even_when_buffer_full() {
        let target = BufferTarget::from_min_buffer_time(Some(Duration::from_secs(2)));
        assert_eq!(prefetch_batch_len(&target, 30.0, 0, 4), 1);
    }

    #[test]
    fn batch_allows_parallel_when_buffer_has_headroom() {
        let target = BufferTarget::from_min_buffer_time(Some(Duration::from_secs(2)));
        assert_eq!(prefetch_batch_len(&target, 5.0, 0, 4), 1);
        assert_eq!(prefetch_batch_len(&target, 5.0, 1, 3), 2);
    }

    #[test]
    fn batch_respects_remaining_segments() {
        let target = BufferTarget::from_min_buffer_time(Some(Duration::from_secs(2)));
        assert_eq!(prefetch_batch_len(&target, 5.0, 0, 1), 1);
    }

    #[test]
    fn batch_empty_when_throttled_after_first() {
        let target = BufferTarget::from_min_buffer_time(Some(Duration::from_secs(2)));
        assert_eq!(prefetch_batch_len(&target, 30.0, 1, 3), 0);
    }
}
