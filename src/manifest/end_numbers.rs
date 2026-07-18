use roxmltree::{Document, Node};

use crate::manifest::ManifestError;

/// `SegmentTemplate@endNumber` per hierarchy node (`dash-mpd` does not deserialize this attribute).
#[derive(Debug, Clone, Default)]
pub(crate) struct SegmentTemplateEndNumbers {
    periods: Vec<PeriodEndNumbers>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct PeriodEndNumbers {
    pub(super) template: Option<u64>,
    adaptation_sets: Vec<AdaptationSetEndNumbers>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct AdaptationSetEndNumbers {
    pub(super) template: Option<u64>,
    representations: Vec<Option<u64>>,
}

fn xml_element_name(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name() == name
}

fn segment_template_end_number_from_node(parent: Node<'_, '_>) -> Option<u64> {
    parent
        .children()
        .find(|n| xml_element_name(*n, "SegmentTemplate"))
        .and_then(|st| st.attribute("endNumber")?.parse().ok())
}

fn parse_adaptation_set_end_numbers(as_node: Node<'_, '_>) -> AdaptationSetEndNumbers {
    let template = segment_template_end_number_from_node(as_node);
    let representations = as_node
        .children()
        .filter(|n| xml_element_name(*n, "Representation"))
        .map(segment_template_end_number_from_node)
        .collect();
    AdaptationSetEndNumbers {
        template,
        representations,
    }
}

fn parse_period_end_numbers(period_node: Node<'_, '_>) -> PeriodEndNumbers {
    let template = segment_template_end_number_from_node(period_node);
    let adaptation_sets = period_node
        .children()
        .filter(|n| xml_element_name(*n, "AdaptationSet"))
        .map(parse_adaptation_set_end_numbers)
        .collect();
    PeriodEndNumbers {
        template,
        adaptation_sets,
    }
}

/// Parse `SegmentTemplate@endNumber` from raw MPD XML (indexed like `Period.adaptations`).
pub(crate) fn parse_segment_template_end_numbers(
    mpd_xml: &str,
) -> Result<SegmentTemplateEndNumbers, ManifestError> {
    let doc = Document::parse(mpd_xml)
        .map_err(|e| ManifestError::Parse(dash_mpd::DashMpdError::Parsing(e.to_string())))?;
    let periods = doc
        .root_element()
        .children()
        .filter(|n| xml_element_name(*n, "Period"))
        .map(parse_period_end_numbers)
        .collect();
    Ok(SegmentTemplateEndNumbers { periods })
}

pub(super) fn merge_end_number_chain(end_numbers: &[Option<u64>]) -> Option<u64> {
    end_numbers
        .iter()
        .copied()
        .fold(None, |parent, child| child.or(parent))
}

impl SegmentTemplateEndNumbers {
    pub(super) fn period(&self, period_idx: usize) -> Option<&PeriodEndNumbers> {
        self.periods.get(period_idx)
    }
}

impl PeriodEndNumbers {
    pub(super) fn adaptation_set(&self, adapt_idx: usize) -> Option<&AdaptationSetEndNumbers> {
        self.adaptation_sets.get(adapt_idx)
    }
}

impl AdaptationSetEndNumbers {
    pub(super) fn representation(&self, rep_idx: usize) -> Option<u64> {
        self.representations.get(rep_idx).copied().flatten()
    }
}

#[cfg(test)]
mod tests;
