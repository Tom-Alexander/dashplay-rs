//! Rust implementation of DASH-IF IOP schematron rules from
//! `tests/conformance/schematron/schematron.sch`.
//!
//! Rules using XPath 2.0-only functions from the original schematron are omitted;
//! all rules here are evaluated deterministically from the parsed MPD XML tree.

use roxmltree::{Document, Node};

const DASH_NS: &str = "urn:mpeg:dash:schema:mpd:2011";

/// A single IOP / MPEG-DASH schematron violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IopViolation {
    pub rule: &'static str,
    pub message: &'static str,
}

/// Validate `mpd_xml` against DASH-IF IOP schematron rules.
///
/// When the MPD `@profiles` does not contain a DASH-IF IOP identifier
/// (`http://dashif.org/guidelines/`), only generic MPEG-DASH rules (R*) are applied.
/// DASH-IF-specific rules (RD*) additionally require an IOP profile tag.
pub fn validate_iop(mpd_xml: &str) -> Result<(), Vec<IopViolation>> {
    let doc = match Document::parse(mpd_xml) {
        Ok(d) => d,
        Err(_) => {
            return Err(vec![IopViolation {
                rule: "XML",
                message: "MPD XML is not well-formed",
            }]);
        }
    };

    let mut violations = Vec::new();
    let root = doc.root_element();
    let dashif = profiles_contain_dashif(root);

    validate_mpd(root, dashif, &mut violations);
    for period in children(root, "Period") {
        validate_period(period, dashif, &mut violations);
        for aset in children(period, "AdaptationSet") {
            validate_adaptation_set(aset, dashif, &mut violations);
            for rep in children(aset, "Representation") {
                validate_representation(rep, dashif, &mut violations);
                for acc in children(rep, "AudioChannelConfiguration") {
                    validate_audio_channel_configuration(acc, &mut violations);
                }
            }
            for st in children(aset, "SegmentTemplate") {
                validate_segment_template(st, &mut violations);
            }
            for sl in children(aset, "SegmentList") {
                validate_segment_list(sl, &mut violations);
            }
            for sb in children(aset, "SegmentBase") {
                validate_segment_base(sb, &mut violations);
            }
            for cp in children(aset, "ContentProtection") {
                validate_content_protection(cp, &mut violations);
            }
            for acc in children(aset, "AudioChannelConfiguration") {
                validate_audio_channel_configuration(acc, &mut violations);
            }
        }
        for st in children(period, "SegmentTemplate") {
            validate_segment_template(st, &mut violations);
        }
        for sl in children(period, "SegmentList") {
            validate_segment_list(sl, &mut violations);
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

fn validate_mpd(mpd: Node<'_, '_>, dashif: bool, out: &mut Vec<IopViolation>) {
    let mpd_type = attr(mpd, "type").unwrap_or("static");

    // R1.0
    if mpd_type == "dynamic" && attr(mpd, "availabilityStartTime").is_none() {
        out.push(violation(
            "R1.0",
            "If MPD is of type \"dynamic\" availabilityStartTime shall be defined.",
        ));
    }
    // R1.1
    if mpd_type == "dynamic" && attr(mpd, "publishTime").is_none() {
        out.push(violation(
            "R1.1",
            "If MPD is of type \"dynamic\" publishTime shall be defined.",
        ));
    }
    // R1.2
    if mpd_type == "static" && attr(mpd, "timeShiftBufferDepth").is_some() && dashif {
        out.push(violation(
            "R1.2",
            "If MPD is of type \"static\" and the profile contains a DASH-IF IOP profile, then the timeShiftBufferDepth shall not be defined.",
        ));
    }
    // R1.5
    if attr(mpd, "mediaPresentationDuration").is_none()
        && attr(mpd, "minimumUpdatePeriod").is_none()
    {
        out.push(violation(
            "R1.5",
            "If mediaPresentationDuration is not defined for the MPD minimumUpdatePeriod shall be defined or vice versa.",
        ));
    }
    // R1.6
    if mpd_type == "static" && attr(mpd, "minimumUpdatePeriod").is_some() && dashif {
        out.push(violation(
            "R1.6",
            "If MPD is of type \"static\" and the profile contains a DASH-IF IOP profile, then the minimumUpdatePeriod shall not be defined.",
        ));
    }
    // R1.7 — only when @profiles is present
    if let Some(profiles) = attr(mpd, "profiles") {
        if !known_profile(profiles) {
            out.push(violation(
                "R1.7",
                "The MPD @profiles shall identify a known MPEG-DASH or DASH-IF profile URN.",
            ));
        }
    }
    // R1.8
    if profiles_contain(mpd, "urn:mpeg:dash:profile:isoff-on-demand:2011") && mpd_type != "static" {
        out.push(violation(
            "R1.8",
            "For On-Demand profile, the MPD @type shall be \"static\".",
        ));
    }
    // RD1.0
    if dashif
        && mpd_type == "dynamic"
        && !profiles_contain(mpd, "urn:mpeg:dash:profile:isoff-live:2011")
    {
        out.push(violation(
            "RD1.0",
            "DASH-IF IOP: For dynamic MPD, the @profile shall include urn:mpeg:dash:profile:isoff-live:2011.",
        ));
    }
}

fn validate_period(period: Node<'_, '_>, dashif: bool, out: &mut Vec<IopViolation>) {
    let mpd = ancestor_mpd(period);
    let mpd_type = mpd.and_then(|n| attr(n, "type")).unwrap_or("static");

    // R2.1 — unique Period @id values
    if let Some(mpd) = ancestor_mpd(period) {
        if let Some(id) = attr(period, "id") {
            let mut matches = 0;
            for p in children(mpd, "Period") {
                if attr(p, "id") == Some(id) {
                    matches += 1;
                }
            }
            if matches > 1 {
                out.push(violation("R2.1", "The id of each Period shall be unique."));
            }
        }
    }

    // R2.3 — at most one segment addressing at Period level
    if segment_addressing_count(period) > 1 {
        out.push(violation(
            "R2.3",
            "At most one of SegmentBase, SegmentTemplate and SegmentList shall be defined in Period.",
        ));
    }

    // R2.4
    if mpd_type == "dynamic" && attr(period, "id").is_none() {
        out.push(violation(
            "R2.4",
            "If the MPD is dynamic the Period element shall have an id.",
        ));
    }

    // R2.5 — addressing somewhere under Period
    if !has_segment_addressing(period) {
        out.push(violation(
            "R2.5",
            "At least one BaseURL, SegmentTemplate or SegmentList shall be defined in Period, AdaptationSet or Representation.",
        ));
    }

    // RD2.0
    if dashif && has_direct_child(period, "SegmentList") {
        out.push(violation(
            "RD2.0",
            "DASH-IF IOP: the Period.SegmentList element shall not be present.",
        ));
    }
}

fn validate_adaptation_set(aset: Node<'_, '_>, dashif: bool, out: &mut Vec<IopViolation>) {
    // R3.0 — unique AdaptationSet @id within Period
    if let Some(id) = attr(aset, "id") {
        if let Some(period) = ancestor(aset, "Period") {
            let mut count = 0;
            for sibling in children(period, "AdaptationSet") {
                if attr(sibling, "id") == Some(id) {
                    count += 1;
                }
            }
            if count > 1 {
                out.push(violation(
                    "R3.0",
                    "The id of each AdaptationSet within a Period shall be unique.",
                ));
            }
        }
    }

    // R3.7
    if children(aset, "Representation").is_empty() {
        out.push(violation(
            "R3.7",
            "An AdaptationSet shall have at least one Representation element.",
        ));
    }

    // R3.8
    if segment_addressing_count(aset) > 1 {
        out.push(violation(
            "R3.8",
            "At most one of SegmentBase, SegmentTemplate and SegmentList shall be defined in AdaptationSet.",
        ));
    }

    if !dashif {
        return;
    }

    let content_type = attr(aset, "contentType");
    if content_type == Some("video") {
        // RD3.0
        if attr(aset, "par").is_none() {
            out.push(violation(
                "RD3.0",
                "DASH-IF IOP: For video AdaptationSet @par shall be present.",
            ));
        }
        // RD3.3
        if attr(aset, "maxWidth").is_none() && attr(aset, "width").is_none() {
            out.push(violation(
                "RD3.3",
                "DASH-IF IOP: For video AdaptationSet @maxWidth or @width shall be present.",
            ));
        }
        // RD3.4
        if attr(aset, "maxHeight").is_none() && attr(aset, "height").is_none() {
            out.push(violation(
                "RD3.4",
                "DASH-IF IOP: For video AdaptationSet @maxHeight or @height shall be present.",
            ));
        }
        // RD3.5
        if attr(aset, "maxFrameRate").is_none() && attr(aset, "frameRate").is_none() {
            out.push(violation(
                "RD3.5",
                "DASH-IF IOP: For video AdaptationSet @maxFrameRate or @frameRate shall be present.",
            ));
        }
    }

    if content_type == Some("audio") && attr(aset, "lang").is_none() {
        out.push(violation(
            "RD3.2",
            "DASH-IF IOP: For audio AdaptationSet @lang shall be present.",
        ));
    }

    if let Some(mime) = attr(aset, "mimeType") {
        if !matches!(
            mime,
            "video/mp4"
                | "audio/mp4"
                | "application/mp4"
                | "application/ttml+xml"
                | "text/vtt"
                | "image/jpeg"
        ) {
            out.push(violation(
                "RD3.7",
                "DASH-IF IOP: AdaptationSet @mimeType shall be one of the six defined types.",
            ));
        }
    }

    validate_hbbtv_adaptation_set(aset, dashif, out);
}

fn validate_representation(rep: Node<'_, '_>, dashif: bool, out: &mut Vec<IopViolation>) {
    // R5.0 — mimeType on Rep or AS
    if attr(rep, "mimeType").is_none() {
        if let Some(aset) = parent(rep) {
            if attr(aset, "mimeType").is_none() {
                out.push(violation(
                    "R5.0",
                    "Either the Representation or the containing AdaptationSet shall have the mimeType attribute.",
                ));
            }
        }
    }

    // R5.2
    if segment_addressing_count(rep) > 1 {
        out.push(violation(
            "R5.2",
            "At most one of SegmentBase, SegmentTemplate and SegmentList shall be defined in Representation.",
        ));
    }

    validate_hbbtv_representation(rep, out);

    if !dashif {
        return;
    }

    let content_type = parent(rep).and_then(|aset| attr(aset, "contentType"));
    if content_type == Some("video") {
        let aset = parent(rep).expect("parent AdaptationSet");
        if attr(rep, "width").is_none() && attr(aset, "width").is_none() {
            out.push(violation(
                "RD5.1",
                "DASH-IF IOP: Representation @width shall be present when not on AdaptationSet.",
            ));
        }
        if attr(rep, "height").is_none() && attr(aset, "height").is_none() {
            out.push(violation(
                "RD5.1",
                "DASH-IF IOP: Representation @height shall be present when not on AdaptationSet.",
            ));
        }
        if attr(rep, "frameRate").is_none() && attr(aset, "frameRate").is_none() {
            out.push(violation(
                "RD5.1",
                "DASH-IF IOP: Representation @frameRate shall be present when not on AdaptationSet.",
            ));
        }
        if attr(rep, "sar").is_none() {
            out.push(violation(
                "RD5.1",
                "DASH-IF IOP: Representation @sar shall be present for video.",
            ));
        }
    }
}

fn validate_segment_template(st: Node<'_, '_>, out: &mut Vec<IopViolation>) {
    let has_duration = attr(st, "duration").is_some();
    let has_timeline = has_direct_child(st, "SegmentTimeline");
    let has_init = attr(st, "initialization").is_some();

    // R7.0
    if !has_duration && !has_timeline && !has_init {
        out.push(violation(
            "R7.0",
            "If more than one Media Segment is present the duration attribute or SegmentTimeline element shall be present.",
        ));
    }
    // R7.1
    if has_duration && has_timeline {
        out.push(violation(
            "R7.1",
            "Either the duration attribute or SegmentTimeline element shall be present but not both.",
        ));
    }
    // R7.3
    if let Some(init) = attr(st, "initialization") {
        if init.contains("$Number") || init.contains("$Time") {
            out.push(violation(
                "R7.3",
                "Neither $Number$ nor the $Time$ identifier shall be included in the initialization attribute.",
            ));
        }
    }
}

fn validate_segment_list(sl: Node<'_, '_>, out: &mut Vec<IopViolation>) {
    let url_count = children(sl, "SegmentURL").len();
    let has_duration = attr(sl, "duration").is_some();
    let has_timeline = has_direct_child(sl, "SegmentTimeline");

    if !has_duration && !has_timeline && url_count > 1 {
        out.push(violation(
            "R8.0",
            "If more than one Media Segment is present the duration attribute or SegmentTimeline element shall be present.",
        ));
    }
    if has_duration && has_timeline {
        out.push(violation(
            "R8.1",
            "Either the duration attribute or SegmentTimeline element shall be present but not both.",
        ));
    }
}

fn validate_segment_base(sb: Node<'_, '_>, out: &mut Vec<IopViolation>) {
    if attr(sb, "indexRange").is_none() && attr(sb, "indexRangeExact").is_some() {
        out.push(violation(
            "R9.0",
            "If indexRange is not present indexRangeExact shall not be present.",
        ));
    }
}

fn validate_content_protection(cp: Node<'_, '_>, out: &mut Vec<IopViolation>) {
    // R12.2
    if parent_tag(cp) != Some("AdaptationSet") {
        out.push(violation(
            "R12.2",
            "The ContentProtection descriptors shall always be present in the AdaptationSet element.",
        ));
    }
}

fn validate_audio_channel_configuration(acc: Node<'_, '_>, out: &mut Vec<IopViolation>) {
    let Some(mpd) = ancestor_mpd(acc) else {
        return;
    };
    let scheme = attr(acc, "schemeIdUri").unwrap_or("");

    // R15 — profile-specific AudioChannelConfiguration schemes
    if profiles_contain(mpd, "http://dashif.org/guidelines/dashif#ac-4")
        && scheme != "tag:dolby.com,2014:dash:audio_channel_configuration:2011"
    {
        out.push(violation(
            "R15.ac-4",
            "If profile http://dashif.org/guidelines/dashif#ac-4 is used, then schemeIdUri attribute shall be tag:dolby.com,2014:dash:audio_channel_configuration:2011.",
        ));
    }
    if profiles_contain(mpd, "http://dashif.org/guidelines/dashif#mha1")
        && scheme != "urn:mpeg:mpegB:cicp:ChannelConfiguration"
    {
        out.push(violation(
            "R15.mha1",
            "If profile http://dashif.org/guidelines/dashif#mha1 is used, then schemeIdUri attribute shall be urn:mpeg:mpegB:cicp:ChannelConfiguration.",
        ));
    }
}

fn hbbtv_applies(node: Node<'_, '_>) -> bool {
    const HBBTV: &str = "urn:hbbtv:dash:profile:isoff-live:2012";
    if attr(node, "profiles").is_some_and(|p| p.contains(HBBTV)) {
        return true;
    }
    if let Some(aset) = ancestor(node, "AdaptationSet")
        && attr(aset, "profiles").is_some_and(|p| p.contains(HBBTV))
    {
        return true;
    }
    ancestor_mpd(node).is_some_and(|mpd| profiles_contain(mpd, HBBTV))
}

fn validate_hbbtv_adaptation_set(aset: Node<'_, '_>, dashif: bool, out: &mut Vec<IopViolation>) {
    if !hbbtv_applies(aset) {
        return;
    }

    if attr(aset, "subsegmentAlignment") == Some("true") {
        out.push(violation(
            "HbbTV.AS.subsegmentAlignment",
            "HbbTV: AdaptationSet@subsegmentAlignment shall not be true.",
        ));
    }
    if matches!(attr(aset, "subsegmentStartsWithSAP"), Some("1") | Some("2")) {
        out.push(violation(
            "HbbTV.AS.subsegmentStartsWithSAP",
            "HbbTV: AdaptationSet@subsegmentStartsWithSAP shall not be 1 or 2.",
        ));
    }
    if dashif && attr(aset, "segmentAlignment") != Some("true") {
        out.push(violation(
            "HbbTV.AS.segmentAlignment",
            "HbbTV: AdaptationSet@segmentAlignment shall be true.",
        ));
    }
}

fn validate_hbbtv_representation(rep: Node<'_, '_>, out: &mut Vec<IopViolation>) {
    if !hbbtv_applies(rep) {
        return;
    }

    if has_direct_child(rep, "BaseURL") {
        out.push(violation(
            "HbbTV.Rep.BaseURL",
            "HbbTV: Representation shall not contain a BaseURL element.",
        ));
    }

    let has_template = has_direct_child(rep, "SegmentTemplate")
        || parent(rep).is_some_and(|aset| has_direct_child(aset, "SegmentTemplate"))
        || ancestor(rep, "Period").is_some_and(|p| has_direct_child(p, "SegmentTemplate"));
    if !has_template {
        out.push(violation(
            "HbbTV.Rep.SegmentTemplate",
            "HbbTV: SegmentTemplate shall be present on Period, AdaptationSet, or Representation.",
        ));
    }
}

fn violation(rule: &'static str, message: &'static str) -> IopViolation {
    IopViolation { rule, message }
}

fn profiles_contain_dashif(mpd: Node<'_, '_>) -> bool {
    attr(mpd, "profiles").is_some_and(|p| p.contains("http://dashif.org/guidelines/"))
}

fn profiles_contain(mpd: Node<'_, '_>, needle: &str) -> bool {
    attr(mpd, "profiles").is_some_and(|p| p.contains(needle))
}

fn known_profile(profiles: &str) -> bool {
    const KNOWN: &[&str] = &[
        "urn:mpeg:dash:profile:isoff-on-demand:2011",
        "urn:mpeg:dash:profile:isoff-live:2011",
        "urn:mpeg:dash:profile:isoff-main:2011",
        "urn:mpeg:dash:profile:full:2011",
        "urn:mpeg:dash:profile:mp2t-main:2011",
        "urn:mpeg:dash:profile:mp2t-simple:2011",
        "http://dashif.org/guidelines/dashif#ac-4",
        "http://dashif.org/guidelines/dashif#mha1",
        "http://dashif.org/guidelines/dashif#vp9",
        "http://dashif.org/guidelines/dash-if-uhd#vp9",
        "http://dashif.org/guidelines/dashif#vp9-hdr",
        "http://dashif.org/guidelines/dash-if-uhd#vp9-hdr",
        "urn:hbbtv:dash:profile:isoff-live:2012",
        "urn:dvb:dash:profile:dvb-dash:2014",
        "http://dashif.org/guidelines/dashif#ec-3",
        "http://dashif.org/guidelines/dash264",
        "http://dashif.org/guidelines/dash-if-simple",
    ];
    KNOWN.iter().any(|k| profiles.contains(k))
}

fn attr<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.attribute(name)
}

fn is_dash(node: Node<'_, '_>, local: &str) -> bool {
    node.tag_name().name() == local && node.tag_name().namespace() == Some(DASH_NS)
}

fn children<'a>(node: Node<'a, 'a>, local: &str) -> Vec<Node<'a, 'a>> {
    node.children()
        .filter(|c| c.is_element() && is_dash(*c, local))
        .collect()
}

fn has_direct_child(node: Node<'_, '_>, local: &str) -> bool {
    node.children().any(|c| c.is_element() && is_dash(c, local))
}

fn parent<'a>(node: Node<'a, 'a>) -> Option<Node<'a, 'a>> {
    node.parent().filter(|p| p.is_element())
}

fn parent_tag<'a>(node: Node<'a, 'a>) -> Option<&'a str> {
    parent(node).map(|p| p.tag_name().name())
}

fn ancestor<'a>(node: Node<'a, 'a>, local: &str) -> Option<Node<'a, 'a>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.is_element() && is_dash(n, local) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

fn ancestor_mpd<'a>(node: Node<'a, 'a>) -> Option<Node<'a, 'a>> {
    ancestor(node, "MPD")
}

fn segment_addressing_count(node: Node<'_, '_>) -> usize {
    ["SegmentBase", "SegmentTemplate", "SegmentList"]
        .iter()
        .filter(|name| has_direct_child(node, name))
        .count()
}

fn has_segment_addressing(node: Node<'_, '_>) -> bool {
    if has_direct_child(node, "BaseURL")
        || has_direct_child(node, "SegmentTemplate")
        || has_direct_child(node, "SegmentList")
        || has_direct_child(node, "SegmentBase")
    {
        return true;
    }
    for child in node.children().filter(|c| c.is_element()) {
        if (is_dash(child, "AdaptationSet") || is_dash(child, "Representation"))
            && has_segment_addressing(child)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dynamic_mpd_without_publish_time() {
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="dynamic"
     availabilityStartTime="2020-01-01T00:00:00Z"
     minimumUpdatePeriod="PT1S">
  <Period id="p1"><AdaptationSet mimeType="video/mp4" contentType="video">
    <SegmentTemplate media="a.m4s" initialization="init.mp4" duration="1"/>
    <Representation id="1" bandwidth="1" width="1" height="1"/>
  </AdaptationSet></Period>
</MPD>"#;
        let err = validate_iop(xml).unwrap_err();
        assert!(err.iter().any(|v| v.rule == "R1.1"));
    }

    #[test]
    fn accepts_static_dashif_simple_fixture() {
        let xml = include_str!("../fixtures/dashif_simple/manifest.mpd");
        validate_iop(xml).expect("dashif_simple should pass IOP validation");
    }

    #[test]
    fn accepts_ac4_and_mha1_channel_configuration_schemes() {
        validate_iop(include_str!("../fixtures/vod_ac4/manifest.mpd"))
            .expect("ac-4 fixture should pass IOP validation");
        validate_iop(include_str!("../fixtures/vod_mha1/manifest.mpd"))
            .expect("mha1 fixture should pass IOP validation");
    }

    #[test]
    fn rejects_ac4_profile_with_wrong_channel_configuration_scheme() {
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"
     mediaPresentationDuration="PT4S" minBufferTime="PT1S"
     profiles="http://dashif.org/guidelines/dashif#ac-4">
  <Period>
    <AdaptationSet mimeType="audio/mp4" contentType="audio" lang="en">
      <AudioChannelConfiguration schemeIdUri="urn:mpeg:mpegB:cicp:ChannelConfiguration" value="2"/>
      <SegmentTemplate media="a.m4s" initialization="i.mp4" duration="2"/>
      <Representation id="1" bandwidth="1" audioSamplingRate="48000"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let err = validate_iop(xml).unwrap_err();
        assert!(err.iter().any(|v| v.rule == "R15.ac-4"));
    }

    #[test]
    fn accepts_known_mp2t_and_vp9_hdr_profiles() {
        validate_iop(include_str!("../fixtures/vod_mp2t/manifest.mpd"))
            .expect("mp2t-simple is a known profile");
        validate_iop(include_str!("../fixtures/vod_vp9_hdr/manifest.mpd"))
            .expect("vp9-hdr fixture should pass IOP validation");
        validate_iop(include_str!("../fixtures/vod_dvb_hbbtv/manifest.mpd"))
            .expect("dvb/hbbtv fixture should pass IOP validation");
    }

    #[test]
    fn rejects_hbbtv_adaptation_set_without_segment_alignment_when_dashif() {
        let xml = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static"
     mediaPresentationDuration="PT4S" minBufferTime="PT1S"
     profiles="http://dashif.org/guidelines/dash-if-simple,urn:hbbtv:dash:profile:isoff-live:2012">
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video" maxWidth="640" maxHeight="360"
                   maxFrameRate="25" par="16:9">
      <SegmentTemplate media="a.m4s" initialization="i.mp4" duration="2"/>
      <Representation id="1" bandwidth="1" width="640" height="360" frameRate="25" sar="1:1"/>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let err = validate_iop(xml).unwrap_err();
        assert!(err.iter().any(|v| v.rule == "HbbTV.AS.segmentAlignment"));
    }
}
