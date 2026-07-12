//! Low-Latency DASH resynchronization (ISO/IEC 23009-1 §5.12, DASH-IF IOP §9.X.4.3).
//!
//! [`ProducerReferenceTime`] anchors map wall-clock time to MPD media time. [`Resync`]
//! describes in-segment chunk or random-access spacing for mid-segment join and recovery.

use std::time::Duration;

use chrono::{DateTime, Utc};
use dash_mpd::{AdaptationSet, MPD, Period, ProducerReferenceTime, Representation, Resync};

use crate::manifest::{self, SegmentAddressing};

/// Parsed [`Resync`] hints for an adaptation set / representation pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ResyncHints {
    /// Nominal CMAF chunk duration (`@type=0`, `@dT / timescale`) in seconds.
    pub chunk_duration_s: Option<f64>,
    /// Nominal random-access spacing (`@type` 1–3, `@dT / timescale`) in seconds.
    pub random_access_interval_s: Option<f64>,
    /// `@marker=true` on a random-access [`Resync`] entry.
    pub random_access_markers: bool,
    /// `Resync@type` 2 or 3: random-access points may occur within a segment, not only at
    /// segment boundaries (`@type` 1).
    pub random_access_within_segment: bool,
}

/// Wall-clock / presentation-time anchor from [`ProducerReferenceTime`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ProducerReferenceAnchor {
    pub wall_clock_time: DateTime<Utc>,
    /// `(presentationTime - presentationTimeOffset)` in MPD timescale ticks (DASH-IF PTA).
    pub pta_ticks: u64,
    pub timescale: u64,
}

impl ProducerReferenceAnchor {
    /// MPD-timeline seconds since `availabilityStartTime` at `wall_now` (real-time production).
    pub(crate) fn since_availability_start_at(
        self,
        wall_now: DateTime<Utc>,
        period_start: Duration,
    ) -> Duration {
        let delta = wall_now
            .signed_duration_since(self.wall_clock_time)
            .to_std()
            .unwrap_or(Duration::ZERO);
        let delta_s = delta.as_secs_f64();
        let ts = self.timescale.max(1) as f64;
        let pt_ticks = self.pta_ticks as f64 + delta_s * ts;
        let media_s = period_start.as_secs_f64() + pt_ticks / ts;
        Duration::from_secs_f64(media_s.max(0.0))
    }
}

/// `@referenceId` from the first in-scope [`ServiceDescription::Latency`] entry, if any.
pub(crate) fn latency_reference_id(mpd: &MPD) -> Option<String> {
    for sd in &mpd.ServiceDescription {
        for lat in &sd.Latency {
            if let Some(id) = lat.referenceId.as_ref().filter(|s| !s.is_empty()) {
                return Some(id.clone());
            }
        }
    }
    None
}

/// Select a [`ProducerReferenceTime`] for clock resync on `adaptation_set` / `representation`.
pub(crate) fn producer_reference_anchor(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
    reference_id: Option<&str>,
    inband_anchor: Option<ProducerReferenceAnchor>,
) -> Option<ProducerReferenceAnchor> {
    let prt = select_producer_reference_time(adaptation_set, representation, reference_id)?;

    if prt.inband == Some(true) {
        return inband_anchor.or_else(|| {
            mpd_producer_reference_anchor(period, adaptation_set, representation, prt)
        });
    }

    mpd_producer_reference_anchor(period, adaptation_set, representation, prt)
}

fn mpd_producer_reference_anchor(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
    prt: &ProducerReferenceTime,
) -> Option<ProducerReferenceAnchor> {
    let addressing =
        manifest::segment_addressing_for_representation(period, adaptation_set, representation)
            .ok()?;
    let (timescale, pto) = timescale_and_pto(&addressing)?;

    let wall = prt.wallClockTime?;
    let presentation_time = prt.presentationTime?;
    let pta_ticks = presentation_time.saturating_sub(pto);

    Some(ProducerReferenceAnchor {
        wall_clock_time: wall,
        pta_ticks,
        timescale,
    })
}

/// Whether `adaptation_set` / `representation` declares in-band producer reference time.
pub(crate) fn producer_reference_inband_enabled(
    adaptation_set: &AdaptationSet,
    representation: &Representation,
    reference_id: Option<&str>,
) -> bool {
    select_producer_reference_time(adaptation_set, representation, reference_id)
        .is_some_and(|prt| prt.inband == Some(true))
}

fn timescale_and_pto(addressing: &SegmentAddressing) -> Option<(u64, u64)> {
    match addressing {
        SegmentAddressing::Template(st) => Some((
            st.timescale.unwrap_or(1).max(1),
            st.presentationTimeOffset.unwrap_or(0),
        )),
        SegmentAddressing::List(sl) => Some((sl.timescale.unwrap_or(1).max(1), 0)),
        SegmentAddressing::Base(sb) => Some((
            sb.timescale.unwrap_or(1).max(1),
            sb.presentationTimeOffset.unwrap_or(0),
        )),
    }
}

fn select_producer_reference_time<'a>(
    adaptation_set: &'a AdaptationSet,
    representation: &'a Representation,
    reference_id: Option<&str>,
) -> Option<&'a ProducerReferenceTime> {
    let candidates: Vec<&ProducerReferenceTime> = representation
        .ProducerReferenceTime
        .iter()
        .chain(adaptation_set.ProducerReferenceTime.iter())
        .collect();

    if let Some(id) = reference_id {
        if let Some(prt) = candidates
            .iter()
            .find(|p| p.id.as_deref() == Some(id))
            .copied()
        {
            return Some(prt);
        }
    }

    candidates.into_iter().find(|p| {
        p.prtType
            .as_deref()
            .is_none_or(|t| t.eq_ignore_ascii_case("encoder"))
    })
}

/// Merge [`Resync`] from adaptation set and representation (both apply; rep entries follow).
pub(crate) fn resync_hints(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Option<ResyncHints> {
    let timescale =
        manifest::segment_addressing_for_representation(period, adaptation_set, representation)
            .ok()
            .and_then(|a| timescale_and_pto(&a).map(|(ts, _)| ts))
            .unwrap_or(1)
            .max(1);

    let mut hints = ResyncHints {
        chunk_duration_s: None,
        random_access_interval_s: None,
        random_access_markers: false,
        random_access_within_segment: false,
    };
    let mut any = false;

    for resync in merged_resync_chain(adaptation_set, representation) {
        any = true;
        apply_resync_entry(&mut hints, resync, timescale);
    }

    any.then_some(hints)
}

fn merged_resync_chain<'a>(
    adaptation_set: &'a AdaptationSet,
    representation: &'a Representation,
) -> Vec<&'a Resync> {
    let mut out: Vec<&Resync> = adaptation_set.Resync.iter().collect();
    out.extend(representation.Resync.iter());
    out
}

fn apply_resync_entry(hints: &mut ResyncHints, resync: &Resync, timescale: u64) {
    let Some(d_t) = resync.dT.filter(|d| *d > 0) else {
        return;
    };
    let duration_s = d_t as f64 / timescale as f64;
    let rtype = resync.rtype.as_deref().unwrap_or("0");
    match rtype {
        "0" => hints.chunk_duration_s = Some(duration_s),
        "1" => {
            hints.random_access_interval_s = Some(duration_s);
            hints.random_access_within_segment = false;
            if resync.marker.unwrap_or(false) {
                hints.random_access_markers = true;
            }
        }
        "2" | "3" => {
            hints.random_access_interval_s = Some(duration_s);
            hints.random_access_within_segment = true;
            if resync.marker.unwrap_or(false) {
                hints.random_access_markers = true;
            }
        }
        _ => {}
    }
}

/// Prefer a [`ProducerReferenceTime`] anchor over UTC-only `since_availability_start`.
pub(crate) fn resync_corrected_since_ast(
    mpd: &MPD,
    wall_now: DateTime<Utc>,
    period: &Period,
    period_start: Duration,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
    inband_anchor: Option<ProducerReferenceAnchor>,
) -> Option<Duration> {
    let reference_id = latency_reference_id(mpd);
    let anchor = producer_reference_anchor(
        period,
        adaptation_set,
        representation,
        reference_id.as_deref(),
        inband_anchor,
    )?;
    Some(anchor.since_availability_start_at(wall_now, period_start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use dash_mpd::{AdaptationSet, Latency, Representation, SegmentTemplate, ServiceDescription};

    fn sample_adaptation_set() -> AdaptationSet {
        AdaptationSet {
            SegmentTemplate: Some(SegmentTemplate {
                timescale: Some(1000),
                presentationTimeOffset: Some(100),
                ..Default::default()
            }),
            ProducerReferenceTime: vec![ProducerReferenceTime {
                id: Some("0".into()),
                prtType: Some("encoder".into()),
                presentationTime: Some(100),
                wallClockTime: Some(Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap()),
                ..Default::default()
            }],
            Resync: vec![Resync {
                rtype: Some("0".into()),
                dT: Some(500_000),
                ..Default::default()
            }],
            representations: vec![Representation {
                id: Some("1".into()),
                bandwidth: Some(100_000),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn sample_period() -> Period {
        Period::default()
    }

    #[test]
    fn producer_reference_anchor_advances_with_wall_clock() {
        let period = sample_period();
        let ad = sample_adaptation_set();
        let rep = &ad.representations[0];
        let anchor = producer_reference_anchor(&period, &ad, rep, None, None).expect("anchor");
        assert_eq!(anchor.pta_ticks, 0);
        let t0 = anchor.since_availability_start_at(anchor.wall_clock_time, Duration::ZERO);
        assert_eq!(t0, Duration::ZERO);
        let t5 = anchor.since_availability_start_at(
            anchor.wall_clock_time + chrono::Duration::seconds(5),
            Duration::ZERO,
        );
        assert_eq!(t5, Duration::from_secs(5));
    }

    #[test]
    fn resync_hints_type2_enables_within_segment_random_access() {
        let period = sample_period();
        let mut ad = sample_adaptation_set();
        ad.Resync = vec![Resync {
            rtype: Some("2".into()),
            dT: Some(500),
            ..Default::default()
        }];
        let rep = &ad.representations[0];
        let hints = resync_hints(&period, &ad, rep).expect("hints");
        assert!((hints.random_access_interval_s.unwrap() - 0.5).abs() < 1e-6);
        assert!(hints.random_access_within_segment);
    }

    #[test]
    fn resync_hints_type1_is_segment_boundary_only() {
        let period = sample_period();
        let mut ad = sample_adaptation_set();
        ad.Resync = vec![Resync {
            rtype: Some("1".into()),
            dT: Some(2000),
            ..Default::default()
        }];
        let rep = &ad.representations[0];
        let hints = resync_hints(&period, &ad, rep).expect("hints");
        assert!((hints.random_access_interval_s.unwrap() - 2.0).abs() < 1e-6);
        assert!(!hints.random_access_within_segment);
    }

    #[test]
    fn resync_hints_parse_chunk_duration() {
        let period = sample_period();
        let ad = sample_adaptation_set();
        let rep = &ad.representations[0];
        let hints = resync_hints(&period, &ad, rep).expect("hints");
        assert!((hints.chunk_duration_s.unwrap() - 500.0).abs() < 1e-6);
    }

    #[test]
    fn latency_reference_id_selects_matching_prt() {
        let period = sample_period();
        let mut ad = sample_adaptation_set();
        ad.ProducerReferenceTime = vec![
            ProducerReferenceTime {
                id: Some("video".into()),
                presentationTime: Some(100),
                wallClockTime: Some(Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap()),
                ..Default::default()
            },
            ProducerReferenceTime {
                id: Some("0".into()),
                presentationTime: Some(200),
                wallClockTime: Some(Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 10).unwrap()),
                ..Default::default()
            },
        ];
        let rep = &ad.representations[0];
        let anchor = producer_reference_anchor(&period, &ad, rep, Some("0"), None).expect("anchor");
        assert_eq!(anchor.pta_ticks, 100);
        assert_eq!(
            anchor.wall_clock_time,
            Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 10).unwrap()
        );
    }

    #[test]
    fn resync_corrected_since_ast_overrides_utc() {
        let mpd = MPD {
            ServiceDescription: vec![ServiceDescription {
                Latency: vec![Latency {
                    referenceId: Some("0".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let period = sample_period();
        let ad = sample_adaptation_set();
        let rep = &ad.representations[0];
        let wall = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 7).unwrap();
        let corrected =
            resync_corrected_since_ast(&mpd, wall, &period, Duration::ZERO, &ad, rep, None)
                .expect("corrected");
        assert_eq!(corrected, Duration::from_secs(7));
    }

    #[test]
    fn inband_anchor_overrides_mpd_prt_when_inband_true() {
        let period = sample_period();
        let mut ad = sample_adaptation_set();
        ad.ProducerReferenceTime[0].inband = Some(true);
        ad.ProducerReferenceTime[0].presentationTime = Some(20_000);
        ad.ProducerReferenceTime[0].wallClockTime =
            Some(Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 20).unwrap());
        let rep = &ad.representations[0];

        let mpd_only = producer_reference_anchor(&period, &ad, rep, None, None).expect("mpd");
        assert_eq!(mpd_only.pta_ticks, 19_900);

        let inband = ProducerReferenceAnchor {
            wall_clock_time: Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 20).unwrap(),
            pta_ticks: 0,
            timescale: 1000,
        };
        let corrected =
            producer_reference_anchor(&period, &ad, rep, None, Some(inband)).expect("inband");
        assert_eq!(corrected.pta_ticks, 0);
    }
}
