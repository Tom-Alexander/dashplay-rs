//! `ServiceDescription` selection and operating constraints (ISO/IEC 23009-1 §5.2.6).
//!
//! Clients must ignore a `ServiceDescription` whose `Scope` list is present and does
//! not match any recognized scheme. Unscoped descriptions apply to all clients.

use dash_mpd::{MPD, ServiceDescription};

/// DVB Low-Latency scope recognized by this client.
pub(crate) const DVB_LOW_LATENCY_SCOPE: &str = "urn:dvb:dash:lowlatency:scope:2019";

/// Whether this client considers itself in scope for `sd`.
///
/// Empty `Scope` list → in scope. Otherwise at least one `Scope@schemeIdUri` must be
/// recognized.
pub(crate) fn service_description_in_scope(sd: &ServiceDescription) -> bool {
    if sd.scopes.is_empty() {
        return true;
    }
    sd.scopes
        .iter()
        .any(|s| s.schemeIdUri.eq_ignore_ascii_case(DVB_LOW_LATENCY_SCOPE))
}

/// Iterate in-scope [`ServiceDescription`] entries in document order.
pub(crate) fn in_scope_service_descriptions(
    mpd: &MPD,
) -> impl Iterator<Item = &ServiceDescription> {
    mpd.ServiceDescription
        .iter()
        .filter(|sd| service_description_in_scope(sd))
}

/// Bandwidth / quality envelope from `OperatingBandwidth` / `OperatingQuality`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ResolvedOperatingConstraints {
    pub bandwidth_min: Option<u64>,
    pub bandwidth_max: Option<u64>,
    pub bandwidth_target: Option<u64>,
    pub quality_min: Option<u64>,
    pub quality_max: Option<u64>,
    pub quality_target: Option<u64>,
}

impl ResolvedOperatingConstraints {
    pub(crate) fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// All operating constraints from the first in-scope `ServiceDescription`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct OperatingConstraints {
    entries: Vec<MediaOperatingEntry>,
}

#[derive(Debug, Clone, PartialEq)]
struct MediaOperatingEntry {
    media_type: Option<String>,
    constraints: ResolvedOperatingConstraints,
}

impl OperatingConstraints {
    /// Parse from the first in-scope `ServiceDescription` that declares operating elements.
    pub(crate) fn from_mpd(mpd: &MPD) -> Option<Self> {
        for sd in in_scope_service_descriptions(mpd) {
            let mut entries = Vec::new();

            for bw in &sd.OperatingBandwidth {
                entries.push(MediaOperatingEntry {
                    media_type: normalize_media_type(bw.mediaType.as_deref()),
                    constraints: ResolvedOperatingConstraints {
                        bandwidth_min: bw.min,
                        bandwidth_max: bw.max,
                        bandwidth_target: bw.target,
                        ..Default::default()
                    },
                });
            }
            for q in &sd.OperatingQuality {
                entries.push(MediaOperatingEntry {
                    media_type: normalize_media_type(q.mediaType.as_deref()),
                    constraints: ResolvedOperatingConstraints {
                        quality_min: q.min,
                        quality_max: q.max,
                        quality_target: q.target,
                        ..Default::default()
                    },
                });
            }

            if !entries.is_empty() {
                return Some(Self { entries });
            }
        }
        None
    }

    /// Resolve constraints for an adaptation set media type (`contentType` / mime family).
    pub(crate) fn resolve_for_media(
        &self,
        media_type: Option<&str>,
    ) -> ResolvedOperatingConstraints {
        let want = normalize_media_type(media_type);
        let mut resolved = ResolvedOperatingConstraints::default();

        for entry in &self.entries {
            if !media_type_matches(entry.media_type.as_deref(), want.as_deref()) {
                continue;
            }
            merge_constraints(&mut resolved, entry.constraints);
        }
        resolved
    }
}

fn normalize_media_type(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("any") {
        return None;
    }
    // "video/mp4" → "video"
    let primary = s.split('/').next().unwrap_or(s);
    Some(primary.to_ascii_lowercase())
}

fn media_type_matches(entry: Option<&str>, want: Option<&str>) -> bool {
    match (entry, want) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(e), Some(w)) => e.eq_ignore_ascii_case(w),
    }
}

fn merge_constraints(into: &mut ResolvedOperatingConstraints, from: ResolvedOperatingConstraints) {
    if into.bandwidth_min.is_none() {
        into.bandwidth_min = from.bandwidth_min;
    }
    if into.bandwidth_max.is_none() {
        into.bandwidth_max = from.bandwidth_max;
    }
    if into.bandwidth_target.is_none() {
        into.bandwidth_target = from.bandwidth_target;
    }
    if into.quality_min.is_none() {
        into.quality_min = from.quality_min;
    }
    if into.quality_max.is_none() {
        into.quality_max = from.quality_max;
    }
    if into.quality_target.is_none() {
        into.quality_target = from.quality_target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_mpd::{Latency, OperatingBandwidth, OperatingQuality, Scope, ServiceDescription};

    #[test]
    fn unscoped_service_description_is_in_scope() {
        let sd = ServiceDescription::default();
        assert!(service_description_in_scope(&sd));
    }

    #[test]
    fn unknown_scope_is_out_of_scope() {
        let sd = ServiceDescription {
            scopes: vec![Scope {
                schemeIdUri: "urn:example:unknown:scope".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!service_description_in_scope(&sd));
    }

    #[test]
    fn dvb_low_latency_scope_is_recognized() {
        let sd = ServiceDescription {
            scopes: vec![Scope {
                schemeIdUri: DVB_LOW_LATENCY_SCOPE.into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(service_description_in_scope(&sd));
    }

    #[test]
    fn first_in_scope_skips_out_of_scope_entries() {
        let mpd = MPD {
            ServiceDescription: vec![
                ServiceDescription {
                    scopes: vec![Scope {
                        schemeIdUri: "urn:example:other".into(),
                        ..Default::default()
                    }],
                    Latency: vec![Latency {
                        target: Some(1000.0),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ServiceDescription {
                    Latency: vec![Latency {
                        target: Some(3500.0),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let sd = in_scope_service_descriptions(&mpd)
            .next()
            .expect("in-scope");
        assert_eq!(sd.Latency[0].target, Some(3500.0));
    }

    #[test]
    fn operating_constraints_resolve_by_media_type() {
        let mpd = MPD {
            ServiceDescription: vec![ServiceDescription {
                OperatingBandwidth: vec![
                    OperatingBandwidth {
                        mediaType: Some("video".into()),
                        min: Some(500_000),
                        max: Some(2_000_000),
                        target: Some(1_000_000),
                    },
                    OperatingBandwidth {
                        mediaType: Some("audio".into()),
                        min: Some(64_000),
                        max: Some(128_000),
                        target: Some(96_000),
                    },
                ],
                OperatingQuality: vec![OperatingQuality {
                    mediaType: Some("video".into()),
                    min: Some(1),
                    max: Some(3),
                    target: Some(2),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let ops = OperatingConstraints::from_mpd(&mpd).expect("ops");
        let video = ops.resolve_for_media(Some("video"));
        assert_eq!(video.bandwidth_min, Some(500_000));
        assert_eq!(video.bandwidth_max, Some(2_000_000));
        assert_eq!(video.bandwidth_target, Some(1_000_000));
        assert_eq!(video.quality_min, Some(1));
        assert_eq!(video.quality_max, Some(3));
        assert_eq!(video.quality_target, Some(2));

        let audio = ops.resolve_for_media(Some("audio"));
        assert_eq!(audio.bandwidth_max, Some(128_000));
        assert!(audio.quality_target.is_none());
    }

    #[test]
    fn out_of_scope_operating_constraints_ignored() {
        let mpd = MPD {
            ServiceDescription: vec![ServiceDescription {
                scopes: vec![Scope {
                    schemeIdUri: "urn:example:other".into(),
                    ..Default::default()
                }],
                OperatingBandwidth: vec![OperatingBandwidth {
                    mediaType: Some("video".into()),
                    max: Some(1_000_000),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(OperatingConstraints::from_mpd(&mpd).is_none());
    }
}
