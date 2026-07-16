use super::cenc::CommonEncryptionScheme;
use pssh_box::{PsshBox, ToBytes, WIDEVINE_SYSTEM_ID, from_base64 as pssh_from_base64};
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use std::collections::HashSet;
use thiserror::Error;
use url::Url;

/// Widevine UUID (edef8ba9-79d6-4ace-a3c8-27dcd51d21ed), normalized.
const WIDEVINE_SCHEME_ID_HEX: &str = "edef8ba979d64acea3c827dcd51d21ed";

/// `ContentProtection@schemeIdUri` for ISO Common Encryption signalling.
const MP4_PROTECTION_SCHEME_ID_URI: &str = "urn:mpeg:dash:mp4protection:2011";

#[derive(Debug, Clone, Default)]
pub struct MpdDrmInfo {
    pub mpd: LevelDrmInfo,
    /// Periods in document order.
    pub periods: Vec<PeriodDrmInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct PeriodDrmInfo {
    pub period: LevelDrmInfo,
    pub adaptation_sets: Vec<AdaptationSetDrmInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct AdaptationSetDrmInfo {
    pub adaptation_set: LevelDrmInfo,
    /// Representations in document order.
    pub representations: Vec<RepresentationDrmInfo>,
    /// Effective DRM for the adaptation set = AdaptationSet + Period + MPD (prefer child).
    pub effective: LevelDrmInfo,
}

#[derive(Debug, Clone, Default)]
pub struct RepresentationDrmInfo {
    pub id: Option<String>,
    pub representation: LevelDrmInfo,
    /// Effective DRM for the representation = Representation + AdaptationSet + Period + MPD (prefer child).
    pub effective: LevelDrmInfo,
}

#[derive(Debug, Clone, Default)]
pub struct LevelDrmInfo {
    pub widevine_pssh: Vec<PsshBox>,
    /// `cenc:default_KID` values found (mp4protection element). Stored as raw string (UUID forms vary).
    pub default_kids: Vec<String>,
    /// Best-effort license URL signaled in MPD (`ms:laurl`, `Laurl`, etc.).
    pub license_urls: Vec<String>,
    /// ISO Common Encryption schemes from `urn:mpeg:dash:mp4protection:2011` `@value`.
    pub protection_schemes: Vec<CommonEncryptionScheme>,
}

#[derive(Debug, Error)]
pub enum MpdDrmError {
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("XML encoding error: {0}")]
    Encoding(#[from] quick_xml::encoding::EncodingError),
    #[error("XML escape error: {0}")]
    Escape(#[from] quick_xml::escape::EscapeError),
    #[error("attribute parse error: {0}")]
    Attr(#[from] quick_xml::events::attributes::AttrError),
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid UTF-8 in MPD")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("invalid license URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("pssh parse error: {0}")]
    Pssh(String),
}

fn scheme_id_uri_is_widevine(value: &str) -> bool {
    let mut s = value.trim().to_ascii_lowercase();
    if let Some(rest) = s.strip_prefix("urn:uuid:") {
        s = rest.into();
    }
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    hex == WIDEVINE_SCHEME_ID_HEX
}

fn scheme_id_uri_is_mp4_protection(value: &str) -> bool {
    value
        .trim()
        .eq_ignore_ascii_case(MP4_PROTECTION_SCHEME_ID_URI)
}

fn collect_protection_scheme(e: &BytesStart<'_>, out: &mut Vec<CommonEncryptionScheme>) {
    let Some(scheme_uri) = attr_value(e, b"schemeIdUri") else {
        return;
    };
    if !scheme_id_uri_is_mp4_protection(&scheme_uri) {
        return;
    }
    let Some(value) = attr_value(e, b"value") else {
        return;
    };
    if let Some(scheme) = CommonEncryptionScheme::parse(&value) {
        if !out.contains(&scheme) {
            out.push(scheme);
        }
    }
}

fn start_is(e: &BytesStart<'_>, local: &[u8]) -> bool {
    e.name().local_name().as_ref() == local
}

fn collapse_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn text_content(t: &BytesText<'_>) -> Result<String, MpdDrmError> {
    let raw = t.decode()?;
    Ok(quick_xml::escape::unescape(raw.as_ref())?.into_owned())
}

fn attr_value(e: &BytesStart<'_>, local: &[u8]) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        if a.key.local_name().as_ref() == local {
            if let Ok(v) = a.unescape_value() {
                return Some(v.into_owned());
            }
        }
    }
    None
}

fn collect_default_kid(e: &BytesStart<'_>, out: &mut Vec<String>) {
    // cenc:default_KID will have local_name "default_KID"
    if let Some(v) = attr_value(e, b"default_KID") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            out.push(v);
        }
    }
}

fn collect_license_url(e: &BytesStart<'_>, out: &mut Vec<String>) {
    // Common patterns:
    // - <ms:laurl licenseUrl="..."/>
    // - <Laurl>https://...</Laurl>
    // We'll handle attribute "licenseUrl" and element text capture elsewhere.
    if let Some(v) = attr_value(e, b"licenseUrl") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            out.push(v);
        }
    }
}

fn normalize_license_url(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    // Ensure it's at least parseable as a URL; keep original string.
    let _ = Url::parse(s).ok()?;
    Some(s.to_string())
}

fn dedupe_push<T, K: Eq + std::hash::Hash>(
    vec: &mut Vec<T>,
    seen: &mut HashSet<K>,
    key: K,
    value: T,
) {
    if seen.insert(key) {
        vec.push(value);
    }
}

fn merge_prefer_child(child: &LevelDrmInfo, parent: &LevelDrmInfo) -> LevelDrmInfo {
    let mut out = LevelDrmInfo::default();

    let mut seen_pssh: HashSet<Vec<u8>> = HashSet::new();
    for p in child
        .widevine_pssh
        .iter()
        .chain(parent.widevine_pssh.iter())
    {
        dedupe_push(
            &mut out.widevine_pssh,
            &mut seen_pssh,
            p.to_bytes(),
            p.clone(),
        );
    }

    let mut seen_kid: HashSet<String> = HashSet::new();
    for k in child.default_kids.iter().chain(parent.default_kids.iter()) {
        dedupe_push(
            &mut out.default_kids,
            &mut seen_kid,
            k.to_string(),
            k.to_string(),
        );
    }

    let mut seen_url: HashSet<String> = HashSet::new();
    for u in child.license_urls.iter().chain(parent.license_urls.iter()) {
        dedupe_push(
            &mut out.license_urls,
            &mut seen_url,
            u.to_string(),
            u.to_string(),
        );
    }

    let mut seen_scheme: HashSet<CommonEncryptionScheme> = HashSet::new();
    for scheme in child
        .protection_schemes
        .iter()
        .chain(parent.protection_schemes.iter())
    {
        dedupe_push(
            &mut out.protection_schemes,
            &mut seen_scheme,
            *scheme,
            *scheme,
        );
    }

    out
}

/// Parse MPD XML and return DRM info with DASH inheritance:
/// Effective = Representation + AdaptationSet + Period + MPD (prefer child, keep parent as fallback).
pub fn parse_mpd_drm_info(mpd_xml: &str) -> Result<MpdDrmInfo, MpdDrmError> {
    let mut reader = Reader::from_str(mpd_xml);
    reader.config_mut().trim_text(false);

    let mut buf = Vec::new();
    let mut info = MpdDrmInfo {
        mpd: LevelDrmInfo::default(),
        periods: Vec::new(),
    };

    let mut in_period = false;
    let mut current_period = PeriodDrmInfo::default();

    let mut in_adaptation_set = false;
    let mut current_aset = AdaptationSetDrmInfo::default();

    let mut in_representation = false;
    let mut current_rep = RepresentationDrmInfo::default();

    // Track content protection state
    let mut cp_depth: u32 = 0;
    let mut cp_is_widevine = false;
    let mut in_pssh = false;
    let mut pssh_acc = String::new();

    // License URL element text capture (e.g. <Laurl>...</Laurl>)
    let mut in_laurl = false;
    let mut laurl_acc = String::new();

    #[derive(Clone, Copy, Debug)]
    enum CpTarget {
        Mpd,
        Period,
        AdaptationSet,
        Representation,
    }
    let mut cp_target: CpTarget = CpTarget::Mpd;

    let mut mpd_level = LevelDrmInfo::default();
    let mut period_level = LevelDrmInfo::default();
    let mut aset_level = LevelDrmInfo::default();
    let mut rep_level = LevelDrmInfo::default();

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,

            Event::Start(e) if start_is(&e, b"MPD") => {
                // root already initialized
                let _ = e;
            }

            Event::Start(e) if start_is(&e, b"Period") => {
                in_period = true;
                current_period = PeriodDrmInfo::default();
                period_level = LevelDrmInfo::default();
                let _ = e;
            }
            Event::End(e) if e.name().local_name().as_ref() == b"Period" => {
                if in_period {
                    // finalize period effective inheritance on children
                    current_period.period = period_level.clone();
                    info.periods.push(current_period.clone());
                }
                in_period = false;
                current_period = PeriodDrmInfo::default();
                period_level = LevelDrmInfo::default();
            }

            Event::Start(e) if start_is(&e, b"AdaptationSet") => {
                in_adaptation_set = true;
                current_aset = AdaptationSetDrmInfo::default();
                aset_level = LevelDrmInfo::default();
            }
            Event::End(e) if e.name().local_name().as_ref() == b"AdaptationSet" => {
                if in_adaptation_set {
                    current_aset.adaptation_set = aset_level.clone();
                    // compute effective for the adaptation set (without representation overrides)
                    let p_m = merge_prefer_child(&period_level, &mpd_level);
                    current_aset.effective = merge_prefer_child(&aset_level, &p_m);

                    // finalize representations (inheritance)
                    let mut reps = Vec::with_capacity(current_aset.representations.len());
                    for mut r in current_aset.representations.clone() {
                        let rep_parent = current_aset.effective.clone();
                        r.effective = merge_prefer_child(&r.representation, &rep_parent);
                        reps.push(r);
                    }
                    current_aset.representations = reps;

                    if in_period {
                        current_period.adaptation_sets.push(current_aset.clone());
                    } else {
                        // Period-less MPD is invalid, but keep it by creating an implicit period.
                        let p = PeriodDrmInfo {
                            period: period_level.clone(),
                            adaptation_sets: vec![current_aset.clone()],
                        };
                        info.periods.push(p);
                    }
                }
                in_adaptation_set = false;
                current_aset = AdaptationSetDrmInfo::default();
                aset_level = LevelDrmInfo::default();
            }

            Event::Start(e) if start_is(&e, b"Representation") => {
                in_representation = true;
                current_rep = RepresentationDrmInfo::default();
                rep_level = LevelDrmInfo::default();
                current_rep.id = attr_value(&e, b"id");
            }
            Event::End(e) if e.name().local_name().as_ref() == b"Representation" => {
                if in_representation {
                    current_rep.representation = rep_level.clone();
                    if in_adaptation_set {
                        current_aset.representations.push(current_rep.clone());
                    }
                }
                in_representation = false;
                current_rep = RepresentationDrmInfo::default();
                rep_level = LevelDrmInfo::default();
            }

            Event::Start(e) if start_is(&e, b"ContentProtection") => {
                cp_depth += 1;
                cp_target = if in_representation {
                    CpTarget::Representation
                } else if in_adaptation_set {
                    CpTarget::AdaptationSet
                } else if in_period {
                    CpTarget::Period
                } else {
                    CpTarget::Mpd
                };

                // Capture default_KID / licenseUrl / mp4protection at the correct level.
                let target_level: &mut LevelDrmInfo = match cp_target {
                    CpTarget::Mpd => &mut mpd_level,
                    CpTarget::Period => &mut period_level,
                    CpTarget::AdaptationSet => &mut aset_level,
                    CpTarget::Representation => &mut rep_level,
                };
                collect_default_kid(&e, &mut target_level.default_kids);
                collect_license_url(&e, &mut target_level.license_urls);
                collect_protection_scheme(&e, &mut target_level.protection_schemes);

                // Determine widevine scheme
                if let Some(scheme) = attr_value(&e, b"schemeIdUri") {
                    cp_is_widevine = scheme_id_uri_is_widevine(&scheme);
                } else {
                    cp_is_widevine = false;
                }
            }
            Event::Empty(e) if start_is(&e, b"ContentProtection") => {
                // Self-closing descriptors (typical for mp4protection @value).
                let target = if in_representation {
                    CpTarget::Representation
                } else if in_adaptation_set {
                    CpTarget::AdaptationSet
                } else if in_period {
                    CpTarget::Period
                } else {
                    CpTarget::Mpd
                };
                let target_level: &mut LevelDrmInfo = match target {
                    CpTarget::Mpd => &mut mpd_level,
                    CpTarget::Period => &mut period_level,
                    CpTarget::AdaptationSet => &mut aset_level,
                    CpTarget::Representation => &mut rep_level,
                };
                collect_default_kid(&e, &mut target_level.default_kids);
                collect_license_url(&e, &mut target_level.license_urls);
                collect_protection_scheme(&e, &mut target_level.protection_schemes);
            }
            Event::End(e) if e.name().local_name().as_ref() == b"ContentProtection" => {
                cp_depth = cp_depth.saturating_sub(1);
                cp_is_widevine = false;
                in_pssh = false;
                pssh_acc.clear();
                in_laurl = false;
                laurl_acc.clear();
            }

            // <cenc:pssh> ... </cenc:pssh> (local name "pssh")
            Event::Start(e) if start_is(&e, b"pssh") => {
                if cp_depth > 0 && cp_is_widevine {
                    in_pssh = true;
                    pssh_acc.clear();
                }
            }
            Event::End(e) if e.name().local_name().as_ref() == b"pssh" => {
                if in_pssh {
                    in_pssh = false;
                    let collapsed = collapse_ws(&pssh_acc);
                    if !collapsed.is_empty() {
                        let boxes = pssh_from_base64(&collapsed)
                            .map_err(|e| MpdDrmError::Pssh(e.to_string()))?;
                        for pssh in boxes {
                            if pssh.system_id == WIDEVINE_SYSTEM_ID {
                                let target_level: &mut LevelDrmInfo = match cp_target {
                                    CpTarget::Mpd => &mut mpd_level,
                                    CpTarget::Period => &mut period_level,
                                    CpTarget::AdaptationSet => &mut aset_level,
                                    CpTarget::Representation => &mut rep_level,
                                };
                                target_level.widevine_pssh.push(pssh);
                            }
                        }
                    }
                }
            }

            // <ms:laurl licenseUrl="..."/> (local name "laurl") OR <Laurl>text</Laurl>
            Event::Empty(e)
                if e.name().local_name().as_ref() == b"laurl"
                    || e.name().local_name().as_ref() == b"Laurl" =>
            {
                if cp_depth > 0 {
                    let target_level: &mut LevelDrmInfo = match cp_target {
                        CpTarget::Mpd => &mut mpd_level,
                        CpTarget::Period => &mut period_level,
                        CpTarget::AdaptationSet => &mut aset_level,
                        CpTarget::Representation => &mut rep_level,
                    };
                    collect_license_url(&e, &mut target_level.license_urls);
                }
            }
            Event::Start(e)
                if e.name().local_name().as_ref() == b"laurl"
                    || e.name().local_name().as_ref() == b"Laurl" =>
            {
                if cp_depth > 0 {
                    let target_level: &mut LevelDrmInfo = match cp_target {
                        CpTarget::Mpd => &mut mpd_level,
                        CpTarget::Period => &mut period_level,
                        CpTarget::AdaptationSet => &mut aset_level,
                        CpTarget::Representation => &mut rep_level,
                    };
                    collect_license_url(&e, &mut target_level.license_urls);
                    in_laurl = true;
                    laurl_acc.clear();
                }
            }
            Event::End(e)
                if e.name().local_name().as_ref() == b"laurl"
                    || e.name().local_name().as_ref() == b"Laurl" =>
            {
                if in_laurl {
                    in_laurl = false;
                    if let Some(v) = normalize_license_url(&laurl_acc) {
                        let target_level: &mut LevelDrmInfo = match cp_target {
                            CpTarget::Mpd => &mut mpd_level,
                            CpTarget::Period => &mut period_level,
                            CpTarget::AdaptationSet => &mut aset_level,
                            CpTarget::Representation => &mut rep_level,
                        };
                        target_level.license_urls.push(v);
                    }
                    laurl_acc.clear();
                }
            }

            Event::Text(t) if in_pssh => {
                pssh_acc.push_str(&text_content(&t)?);
            }
            Event::CData(c) if in_pssh => {
                let s = c.decode()?;
                pssh_acc.push_str(s.as_ref());
            }

            Event::Text(t) if in_laurl => {
                laurl_acc.push_str(&text_content(&t)?);
            }
            Event::CData(c) if in_laurl => {
                let s = c.decode()?;
                laurl_acc.push_str(s.as_ref());
            }

            _ => {}
        }
    }

    info.mpd = mpd_level.clone();
    Ok(info)
}
