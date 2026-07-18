//! Bitstream switching signalling (`@bitstreamSwitching` / `BitstreamSwitching`).
//!
//! When enabled for an Adaptation Set, concatenating Media Segments from different
//! Representations does not require re-initialization of the media decoder (ISO/IEC
//! 23009-1 §5.3.3.1). The player therefore reuses an already-fetched Initialization
//! Segment across ABR switches instead of fetching and re-emitting a new one.

use dash_mpd::{AdaptationSet, Period};

use super::addressing::SegmentAddressing;

/// Whether this Adaptation Set permits representation switches without a new init segment.
///
/// True when any of:
/// - `AdaptationSet@bitstreamSwitching` is true (or inherited from `Period@bitstreamSwitching`)
/// - Timeline or per-representation `SegmentTemplate` / `SegmentList` declares a Bitstream
///   Switching Segment via `@bitstreamSwitching` or a `BitstreamSwitching` child element
/// - A `Switching` element with `@type="bitstream"` is present (ISO/IEC 23009-1 §5.3.3.4)
pub(crate) fn bitstream_switching_enabled(
    period: &Period,
    adaptation_set: &AdaptationSet,
    addressing: &SegmentAddressing,
) -> bool {
    if adaptation_set_bitstream_switching(period, adaptation_set) {
        return true;
    }
    if super::switch_access::switching_declares_bitstream(
        &super::switch_access::switching_hints_for(adaptation_set, None),
    ) {
        return true;
    }
    if addressing_declares_bitstream_switching(addressing) {
        return true;
    }
    for rep in &adaptation_set.representations {
        if super::switch_access::switching_declares_bitstream(
            &super::switch_access::switching_hints_for(adaptation_set, Some(rep)),
        ) {
            return true;
        }
        if let Ok(rep_addressing) =
            super::addressing::segment_addressing_for_representation(period, adaptation_set, rep)
            && addressing_declares_bitstream_switching(&rep_addressing)
        {
            return true;
        }
    }
    false
}

fn adaptation_set_bitstream_switching(period: &Period, adaptation_set: &AdaptationSet) -> bool {
    match adaptation_set.bitstreamSwitching {
        Some(v) => v,
        None => period.bitstreamSwitching.unwrap_or(false),
    }
}

fn addressing_declares_bitstream_switching(addressing: &SegmentAddressing) -> bool {
    match addressing {
        SegmentAddressing::Template(st) => {
            st.bitstreamSwitching.is_some() || st.BitstreamSwitching.is_some()
        }
        SegmentAddressing::List(sl) => sl.BitstreamSwitching.is_some(),
        SegmentAddressing::Base(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use dash_mpd::{BitstreamSwitching, SegmentList, SegmentTemplate, Switching};

    use super::*;

    fn empty_addressing() -> SegmentAddressing {
        SegmentAddressing::Template(SegmentTemplate::default())
    }

    #[test]
    fn adaptation_set_flag_enables_switching() {
        let period = Period::default();
        let set = AdaptationSet {
            bitstreamSwitching: Some(true),
            ..Default::default()
        };
        assert!(bitstream_switching_enabled(
            &period,
            &set,
            &empty_addressing()
        ));
    }

    #[test]
    fn period_flag_inherits_when_adaptation_set_omits() {
        let period = Period {
            bitstreamSwitching: Some(true),
            ..Default::default()
        };
        let set = AdaptationSet::default();
        assert!(bitstream_switching_enabled(
            &period,
            &set,
            &empty_addressing()
        ));
    }

    #[test]
    fn adaptation_set_false_overrides_period_true() {
        let period = Period {
            bitstreamSwitching: Some(true),
            ..Default::default()
        };
        let set = AdaptationSet {
            bitstreamSwitching: Some(false),
            ..Default::default()
        };
        assert!(!bitstream_switching_enabled(
            &period,
            &set,
            &empty_addressing()
        ));
    }

    #[test]
    fn segment_template_attribute_enables_switching() {
        let addressing = SegmentAddressing::Template(SegmentTemplate {
            bitstreamSwitching: Some("bs-$RepresentationID$.mp4".into()),
            ..Default::default()
        });
        assert!(bitstream_switching_enabled(
            &Period::default(),
            &AdaptationSet::default(),
            &addressing
        ));
    }

    #[test]
    fn segment_template_element_enables_switching() {
        let addressing = SegmentAddressing::Template(SegmentTemplate {
            BitstreamSwitching: Some(BitstreamSwitching {
                source_url: Some("bs.mp4".into()),
                range: None,
            }),
            ..Default::default()
        });
        assert!(bitstream_switching_enabled(
            &Period::default(),
            &AdaptationSet::default(),
            &addressing
        ));
    }

    #[test]
    fn segment_list_element_enables_switching() {
        let addressing = SegmentAddressing::List(SegmentList {
            BitstreamSwitching: Some(BitstreamSwitching {
                source_url: Some("bs.mp4".into()),
                range: None,
            }),
            ..Default::default()
        });
        assert!(bitstream_switching_enabled(
            &Period::default(),
            &AdaptationSet::default(),
            &addressing
        ));
    }

    #[test]
    fn default_is_disabled() {
        assert!(!bitstream_switching_enabled(
            &Period::default(),
            &AdaptationSet::default(),
            &empty_addressing()
        ));
    }

    #[test]
    fn switching_element_bitstream_type_enables() {
        let set = AdaptationSet {
            Switching: vec![Switching {
                interval: Some(4000),
                stype: Some("bitstream".into()),
            }],
            ..Default::default()
        };
        assert!(bitstream_switching_enabled(
            &Period::default(),
            &set,
            &empty_addressing()
        ));
    }

    #[test]
    fn switching_element_media_type_does_not_enable() {
        let set = AdaptationSet {
            Switching: vec![Switching {
                interval: Some(4000),
                stype: Some("media".into()),
            }],
            ..Default::default()
        };
        assert!(!bitstream_switching_enabled(
            &Period::default(),
            &set,
            &empty_addressing()
        ));
    }
}
