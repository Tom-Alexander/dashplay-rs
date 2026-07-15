//! MPD descriptive metadata (ISO/IEC 23009-1 `ProgramInformation`, `Metrics`, labels, …).
//!
//! These types are observational: they surface document metadata for UI and analytics and do not
//! drive ABR, scheduling, or Metrics reporting clients.

use std::time::Duration;

use dash_mpd::MPD;
use roxmltree::{Document, Node};

use crate::manifest::ManifestError;

/// Textual description usable for content discovery or UI selection (`Label` / `GroupLabel`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentLabel {
    /// Optional `@id`.
    pub id: Option<String>,
    /// Optional RFC 5646 language.
    pub lang: Option<String>,
    /// Element text content.
    pub content: String,
}

/// SCTE-214 content identifier (`scte214:ContentIdentifier`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scte214ContentId {
    /// `@type`.
    pub id_type: Option<String>,
    /// `@value`.
    pub id_value: Option<String>,
}

/// MPD `ProgramInformation` entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProgramInformation {
    /// `@lang` in RFC 5646 format.
    pub lang: Option<String>,
    /// `@moreInformationURL`.
    pub more_information_url: Option<String>,
    /// `Title` text.
    pub title: Option<String>,
    /// `Source` text.
    pub source: Option<String>,
    /// `Copyright` text.
    pub copyright: Option<String>,
    /// Optional SCTE-214 content identifier.
    pub scte214_content_identifier: Option<Scte214ContentId>,
}

/// One `Reporting` descriptor under MPD `Metrics`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReportingDescriptor {
    /// `@id`.
    pub id: Option<String>,
    /// `@schemeIdUri`.
    pub scheme_id_uri: String,
    /// `@value`.
    pub value: Option<String>,
    /// `@reportingUrl` / `@dvb:reportingUrl`.
    pub reporting_url: Option<String>,
    /// `@probability` / `@dvb:probability`.
    pub probability: Option<u64>,
}

/// Time window under MPD `Metrics/Range`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetricsRange {
    /// `@starttime`.
    pub start_time: Option<Duration>,
    /// `@duration`.
    pub duration: Option<Duration>,
}

/// MPD `Metrics` element (DASH reporting descriptors).
///
/// Named to distinguish from playback [`crate::TrackMetrics`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MpdReportingMetrics {
    /// Whitespace-separated `@metrics` key list.
    pub metrics: String,
    /// Reporting descriptors.
    pub reporting: Vec<ReportingDescriptor>,
    /// Optional time ranges for which reporting applies.
    pub ranges: Vec<MetricsRange>,
}

/// Period `AssetIdentifier` descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AssetIdentifier {
    /// `@schemeIdUri`.
    pub scheme_id_uri: String,
    /// `@value`.
    pub value: Option<String>,
    /// Nested SCTE-214 content identifiers.
    pub scte214_content_identifiers: Vec<Scte214ContentId>,
}

/// Metadata for one MPD `Period`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PeriodMetadata {
    /// `Period@id`.
    pub id: Option<String>,
    /// `AssetIdentifier`, when present.
    pub asset_identifier: Option<AssetIdentifier>,
    /// Direct `Period/Label` children.
    pub labels: Vec<ContentLabel>,
    /// `Period/GroupLabel` children.
    pub group_labels: Vec<ContentLabel>,
}

/// Document-level descriptive metadata extracted from a parsed MPD.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManifestMetadata {
    /// `MPD/ProgramInformation` entries in document order.
    pub program_information: Vec<ProgramInformation>,
    /// `MPD/Metrics` reporting entries in document order.
    pub metrics: Vec<MpdReportingMetrics>,
    /// Per-period metadata in the same order as `MPD.periods`.
    pub periods: Vec<PeriodMetadata>,
}

impl ManifestMetadata {
    /// Extract metadata from a parsed MPD and optional raw XML.
    ///
    /// `mpd_xml` supplies `Period/Label` (missing from `dash_mpd`). When omitted or unparsable,
    /// period labels are empty while other fields still map from `mpd`.
    pub fn from_mpd(mpd: &MPD, mpd_xml: Option<&str>) -> Self {
        let period_labels = mpd_xml
            .and_then(|xml| parse_period_labels(xml).ok())
            .unwrap_or_default();

        let program_information = mpd
            .ProgramInformation
            .iter()
            .map(map_program_information)
            .collect();
        let metrics = mpd.Metrics.iter().map(map_reporting_metrics).collect();
        let periods = mpd
            .periods
            .iter()
            .enumerate()
            .map(|(idx, period)| PeriodMetadata {
                id: period.id.clone(),
                asset_identifier: period.asset_identifier.as_ref().map(map_asset_identifier),
                labels: period_labels.get(idx).cloned().unwrap_or_default(),
                group_labels: period.group_label.iter().map(map_label).collect(),
            })
            .collect();

        Self {
            program_information,
            metrics,
            periods,
        }
    }
}

fn map_label(label: &dash_mpd::Label) -> ContentLabel {
    ContentLabel {
        id: label.id.clone(),
        lang: label.lang.clone(),
        content: label.content.clone(),
    }
}

fn map_scte214(id: &dash_mpd::Scte214ContentIdentifier) -> Scte214ContentId {
    Scte214ContentId {
        id_type: id.idType.clone(),
        id_value: id.idValue.clone(),
    }
}

fn map_program_information(pi: &dash_mpd::ProgramInformation) -> ProgramInformation {
    ProgramInformation {
        lang: pi.lang.clone(),
        more_information_url: pi.moreInformationURL.clone(),
        title: pi.Title.as_ref().and_then(|t| t.content.clone()),
        source: pi.Source.as_ref().and_then(|s| s.content.clone()),
        copyright: pi.Copyright.as_ref().and_then(|c| c.content.clone()),
        scte214_content_identifier: pi.scte214ContentIdentifier.as_ref().map(map_scte214),
    }
}

fn map_reporting_metrics(m: &dash_mpd::Metrics) -> MpdReportingMetrics {
    MpdReportingMetrics {
        metrics: m.metrics.clone(),
        reporting: m
            .Reporting
            .iter()
            .map(|r| ReportingDescriptor {
                id: r.id.clone(),
                scheme_id_uri: r.schemeIdUri.clone(),
                value: r.value.clone(),
                reporting_url: r.reportingUrl.clone(),
                probability: r.probability,
            })
            .collect(),
        ranges: m
            .Range
            .iter()
            .map(|r| MetricsRange {
                start_time: r.starttime,
                duration: r.duration,
            })
            .collect(),
    }
}

fn map_asset_identifier(asset: &dash_mpd::AssetIdentifier) -> AssetIdentifier {
    AssetIdentifier {
        scheme_id_uri: asset.schemeIdUri.clone(),
        value: asset.value.clone(),
        scte214_content_identifiers: asset
            .scte214ContentIdentifiers
            .iter()
            .map(map_scte214)
            .collect(),
    }
}

fn xml_element_name(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn parse_label_node(node: Node<'_, '_>) -> ContentLabel {
    ContentLabel {
        id: node.attribute("id").map(str::to_owned),
        lang: node.attribute("lang").map(str::to_owned),
        content: node.text().unwrap_or("").trim().to_owned(),
    }
}

/// Parse direct `Period/Label` children, indexed like `MPD.periods`.
fn parse_period_labels(mpd_xml: &str) -> Result<Vec<Vec<ContentLabel>>, ManifestError> {
    let doc = Document::parse(mpd_xml)
        .map_err(|e| ManifestError::Parse(dash_mpd::DashMpdError::Parsing(e.to_string())))?;
    let periods = doc
        .root_element()
        .children()
        .filter(|n| xml_element_name(*n, "Period"))
        .map(|period| {
            period
                .children()
                .filter(|n| xml_element_name(*n, "Label"))
                .map(parse_label_node)
                .collect()
        })
        .collect();
    Ok(periods)
}

/// Map a `dash_mpd::Label` into a public [`ContentLabel`].
pub(crate) fn content_label_from_dash(label: &dash_mpd::Label) -> ContentLabel {
    map_label(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT30S"
     minBufferTime="PT2S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <ProgramInformation lang="en" moreInformationURL="https://example.com/info">
    <Title>Demo Title</Title>
    <Source>Demo Source</Source>
    <Copyright>Demo Copyright</Copyright>
  </ProgramInformation>
  <Metrics metrics="TcpConnections ContiguousPresence">
    <Reporting schemeIdUri="urn:dvb:dash:reporting:2014" value="1"
               reportingUrl="https://example.com/report" probability="500"/>
    <Range starttime="PT0S" duration="PT30S"/>
  </Metrics>
  <Period id="p0" duration="PT30S">
    <AssetIdentifier schemeIdUri="urn:org:example:asset-id:2013" value="asset-1"/>
    <Label id="pl1" lang="en">Period Label</Label>
    <GroupLabel lang="en">Period Group</GroupLabel>
    <AdaptationSet id="1" contentType="video" mimeType="video/mp4">
      <Rating schemeIdUri="urn:mpeg:dash:rating:2011" value="PG"/>
      <Label id="al1" lang="en">Main Video</Label>
      <Representation id="r0" bandwidth="1000000" codecs="avc1.4D401F">
        <Label lang="en">720p</Label>
        <SegmentBase timescale="1" presentationTimeOffset="0"/>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn extracts_mpd_and_period_metadata() {
        let mpd = dash_mpd::parse(FIXTURE).expect("parse");
        let meta = ManifestMetadata::from_mpd(&mpd, Some(FIXTURE));

        assert_eq!(meta.program_information.len(), 1);
        let pi = &meta.program_information[0];
        assert_eq!(pi.lang.as_deref(), Some("en"));
        assert_eq!(
            pi.more_information_url.as_deref(),
            Some("https://example.com/info")
        );
        assert_eq!(pi.title.as_deref(), Some("Demo Title"));
        assert_eq!(pi.source.as_deref(), Some("Demo Source"));
        assert_eq!(pi.copyright.as_deref(), Some("Demo Copyright"));

        assert_eq!(meta.metrics.len(), 1);
        let metrics = &meta.metrics[0];
        assert_eq!(metrics.metrics, "TcpConnections ContiguousPresence");
        assert_eq!(metrics.reporting.len(), 1);
        assert_eq!(
            metrics.reporting[0].scheme_id_uri,
            "urn:dvb:dash:reporting:2014"
        );
        assert_eq!(
            metrics.reporting[0].reporting_url.as_deref(),
            Some("https://example.com/report")
        );
        assert_eq!(metrics.reporting[0].probability, Some(500));
        assert_eq!(metrics.ranges.len(), 1);
        assert_eq!(metrics.ranges[0].duration, Some(Duration::from_secs(30)));

        assert_eq!(meta.periods.len(), 1);
        let period = &meta.periods[0];
        assert_eq!(period.id.as_deref(), Some("p0"));
        let asset = period.asset_identifier.as_ref().expect("asset");
        assert_eq!(asset.scheme_id_uri, "urn:org:example:asset-id:2013");
        assert_eq!(asset.value.as_deref(), Some("asset-1"));
        assert_eq!(period.labels.len(), 1);
        assert_eq!(period.labels[0].content, "Period Label");
        assert_eq!(period.labels[0].id.as_deref(), Some("pl1"));
        assert_eq!(period.group_labels.len(), 1);
        assert_eq!(period.group_labels[0].content, "Period Group");
    }

    #[test]
    fn period_labels_empty_without_xml() {
        let mpd = dash_mpd::parse(FIXTURE).expect("parse");
        let meta = ManifestMetadata::from_mpd(&mpd, None);
        assert!(meta.periods[0].labels.is_empty());
        assert!(!meta.periods[0].group_labels.is_empty());
        assert!(meta.periods[0].asset_identifier.is_some());
    }
}
