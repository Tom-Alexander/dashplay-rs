use dash_mpd::{AdaptationSet, Period, Representation, SegmentBase, SegmentList, SegmentTemplate};

use crate::manifest::ManifestError;

use super::end_numbers::SegmentTemplateEndNumbers;
use super::end_numbers::merge_end_number_chain;
use super::inheritance::{
    merge_segment_base_chain, merge_segment_list, merge_segment_list_chain, merge_segment_template,
    merge_segment_template_chain, segment_list_has_timeline_source,
};
use super::template::{TemplateVars, interpolate_template, template_contains_number_or_time_ident};

fn segment_template_has_timeline_source(st: &SegmentTemplate) -> bool {
    st.SegmentTimeline.is_some() || st.duration.is_some()
}

/// Timeline comes from a separate index document (`@index` + `@indexRange` or `RepresentationIndex`).
pub(crate) fn segment_template_uses_sidecar_index(st: &SegmentTemplate) -> bool {
    st.representation_index.is_some() || (st.index.is_some() && st.indexRange.is_some())
}

/// `SegmentBase@indexRange` or `RepresentationIndex`: timeline comes from an sidx index.
pub(crate) fn segment_base_uses_sidx_index(sb: &SegmentBase) -> bool {
    sb.indexRange.is_some() || sb.representation_index.is_some()
}

/// Single sidecar index document listing all segments (no `$Number$` / `$Time$` in `@index`).
pub(crate) fn segment_template_uses_global_sidecar_index(st: &SegmentTemplate) -> bool {
    segment_template_uses_sidecar_index(st) && !segment_template_index_uses_segment_identifiers(st)
}

/// One sidecar index document per media segment (`@index` contains `$Number$` or `$Time$`).
pub(crate) fn segment_template_uses_per_segment_index(st: &SegmentTemplate) -> bool {
    segment_template_uses_sidecar_index(st) && segment_template_index_uses_segment_identifiers(st)
}

pub(super) fn segment_template_index_uses_segment_identifiers(st: &SegmentTemplate) -> bool {
    if st
        .index
        .as_deref()
        .is_some_and(template_contains_number_or_time_ident)
    {
        return true;
    }
    st.representation_index
        .as_ref()
        .and_then(|ri| ri.sourceURL.as_deref())
        .is_some_and(template_contains_number_or_time_ident)
}

fn segment_template_has_addressing_source(st: &SegmentTemplate) -> bool {
    segment_template_has_timeline_source(st) || segment_template_uses_global_sidecar_index(st)
}

/// Inherited `SegmentTemplate@endNumber` for adaptation-set timeline expansion.
pub(crate) fn end_number_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
    supplements: &SegmentTemplateEndNumbers,
    period_idx: usize,
    adapt_idx: usize,
) -> Option<u64> {
    let period_sup = supplements.period(period_idx)?;
    let adapt_sup = period_sup.adaptation_set(adapt_idx)?;

    let mut chain = vec![period_sup.template, adapt_sup.template];

    let merged = merge_segment_template_chain(&[
        period.SegmentTemplate.as_ref(),
        adaptation_set.SegmentTemplate.as_ref(),
    ]);
    if merged
        .as_ref()
        .is_none_or(|st| !segment_template_has_timeline_source(st))
    {
        for (rep_idx, rep) in adaptation_set.representations.iter().enumerate() {
            if rep
                .SegmentTemplate
                .as_ref()
                .is_some_and(segment_template_has_timeline_source)
            {
                chain.push(adapt_sup.representation(rep_idx));
                break;
            }
        }
    }

    merge_end_number_chain(&chain)
}

/// Effective `SegmentTemplate` for timeline expansion on an adaptation set (Period → AdaptationSet,
/// supplementing from the first representation that carries timeline or duration when needed).
pub(crate) fn segment_template_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentTemplate, ManifestError> {
    let mut merged = merge_segment_template_chain(&[
        period.SegmentTemplate.as_ref(),
        adaptation_set.SegmentTemplate.as_ref(),
    ]);

    if merged
        .as_ref()
        .is_none_or(|st| !segment_template_has_addressing_source(st))
    {
        for rep in &adaptation_set.representations {
            if let Ok(rep_st) = segment_template_for_representation(period, adaptation_set, rep) {
                if segment_template_has_addressing_source(&rep_st) {
                    merged = Some(match merged {
                        None => rep_st,
                        Some(parent) => merge_segment_template(&parent, &rep_st),
                    });
                    break;
                }
            }
        }
    }

    merged.ok_or(ManifestError::MissingSegmentTemplate)
}

/// Resolved segment addressing mode after Period → AdaptationSet → Representation inheritance.
#[derive(Debug, Clone)]
pub(crate) enum SegmentAddressing {
    Template(SegmentTemplate),
    List(SegmentList),
    Base(SegmentBase),
}

/// ISO/IEC 23009-1 §5.3.9: at most one of `SegmentTemplate`, `SegmentList`, or
/// `SegmentBase` may appear on the same Period, AdaptationSet, or Representation.
fn validate_addressing_modes_at_level(
    level: &'static str,
    has_template: bool,
    has_list: bool,
    has_base: bool,
) -> Result<(), ManifestError> {
    let count = u8::from(has_template) + u8::from(has_list) + u8::from(has_base);
    if count > 1 {
        Err(ManifestError::ConflictingSegmentAddressing(level))
    } else {
        Ok(())
    }
}

fn validate_period_addressing(period: &Period) -> Result<(), ManifestError> {
    validate_addressing_modes_at_level(
        "Period",
        period.SegmentTemplate.is_some(),
        period.SegmentList.is_some(),
        period.SegmentBase.is_some(),
    )
}

fn validate_adaptation_set_addressing(adaptation_set: &AdaptationSet) -> Result<(), ManifestError> {
    validate_addressing_modes_at_level(
        "AdaptationSet",
        adaptation_set.SegmentTemplate.is_some(),
        adaptation_set.SegmentList.is_some(),
        adaptation_set.SegmentBase.is_some(),
    )
}

fn validate_representation_addressing(
    representation: &Representation,
) -> Result<(), ManifestError> {
    validate_addressing_modes_at_level(
        "Representation",
        representation.SegmentTemplate.is_some(),
        representation.SegmentList.is_some(),
        representation.SegmentBase.is_some(),
    )
}

/// Reject MPDs that declare more than one addressing mode on any node in the chain.
fn validate_addressing_hierarchy(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: Option<&Representation>,
) -> Result<(), ManifestError> {
    validate_period_addressing(period)?;
    validate_adaptation_set_addressing(adaptation_set)?;
    match representation {
        Some(rep) => validate_representation_addressing(rep)?,
        None => {
            for rep in &adaptation_set.representations {
                validate_representation_addressing(rep)?;
            }
        }
    }
    Ok(())
}

fn has_segment_list_in_chain(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: Option<&Representation>,
) -> bool {
    period.SegmentList.is_some()
        || adaptation_set.SegmentList.is_some()
        || representation.is_some_and(|r| r.SegmentList.is_some())
}

fn adaptation_set_uses_segment_list(period: &Period, adaptation_set: &AdaptationSet) -> bool {
    period.SegmentList.is_some()
        || adaptation_set.SegmentList.is_some()
        || adaptation_set
            .representations
            .iter()
            .any(|r| r.SegmentList.is_some())
}

fn adaptation_set_uses_segment_template(period: &Period, adaptation_set: &AdaptationSet) -> bool {
    period.SegmentTemplate.is_some()
        || adaptation_set.SegmentTemplate.is_some()
        || adaptation_set
            .representations
            .iter()
            .any(|r| r.SegmentTemplate.is_some() || r.representation_index.is_some())
}

fn adaptation_set_uses_segment_base(period: &Period, adaptation_set: &AdaptationSet) -> bool {
    period.SegmentBase.is_some()
        || adaptation_set.SegmentBase.is_some()
        || adaptation_set.representations.iter().any(|r| {
            r.SegmentBase.is_some()
                || (r.representation_index.is_some()
                    && r.SegmentTemplate.is_none()
                    && r.SegmentList.is_none())
        })
}

/// True when a Representation can be fetched as a single progressive file via BaseURL
/// hierarchy alone (ISO/IEC 23009-1 §5.3.9.2), without an explicit Segment* element.
fn has_progressive_base_url(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: Option<&Representation>,
) -> bool {
    representation.is_some_and(|r| !r.BaseURL.is_empty())
        || !adaptation_set.BaseURL.is_empty()
        || !period.BaseURL.is_empty()
}

fn has_segment_template_in_chain(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: Option<&Representation>,
) -> bool {
    period.SegmentTemplate.is_some()
        || adaptation_set.SegmentTemplate.is_some()
        || representation.is_some_and(|r| r.SegmentTemplate.is_some())
}

fn has_segment_base_in_chain(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: Option<&Representation>,
) -> bool {
    period.SegmentBase.is_some()
        || adaptation_set.SegmentBase.is_some()
        || representation.is_some_and(|r| r.SegmentBase.is_some())
}

/// Merge two `SegmentBase` nodes: `child` attributes override `parent` when present.
pub(crate) fn segment_base_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentBase, ManifestError> {
    let mut merged = merge_segment_base_chain(&[
        period.SegmentBase.as_ref(),
        adaptation_set.SegmentBase.as_ref(),
    ]);

    if merged
        .as_ref()
        .is_none_or(|sb| !segment_base_uses_sidx_index(sb))
    {
        for rep in &adaptation_set.representations {
            if let Ok(rep_sb) = segment_base_for_representation(period, adaptation_set, rep) {
                if segment_base_uses_sidx_index(&rep_sb) {
                    merged = Some(match merged {
                        None => rep_sb,
                        Some(parent) => super::inheritance::merge_segment_base(&parent, &rep_sb),
                    });
                    break;
                }
            }
        }
    }

    merged
        .or_else(|| {
            adaptation_set
                .representations
                .iter()
                .find_map(|r| r.SegmentBase.as_ref())
                .cloned()
        })
        .ok_or(ManifestError::MissingSegmentBase)
}

/// Effective `SegmentBase` for fetching init/media of one representation.
pub(crate) fn segment_base_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentBase, ManifestError> {
    let mut sb = merge_segment_base_chain(&[
        period.SegmentBase.as_ref(),
        adaptation_set.SegmentBase.as_ref(),
        representation.SegmentBase.as_ref(),
    ])
    .ok_or(ManifestError::MissingSegmentBase)?;
    if let Some(ri) = &representation.representation_index {
        sb.representation_index = Some(match sb.representation_index {
            Some(parent) => super::inheritance::merge_representation_index(&parent, ri),
            None => ri.clone(),
        });
    }
    Ok(sb)
}

pub(crate) fn segment_list_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentList, ManifestError> {
    let mut merged = merge_segment_list_chain(&[
        period.SegmentList.as_ref(),
        adaptation_set.SegmentList.as_ref(),
    ]);

    if merged
        .as_ref()
        .is_none_or(|sl| !segment_list_has_timeline_source(sl))
    {
        for rep in &adaptation_set.representations {
            if let Some(rep_sl) = &rep.SegmentList {
                if segment_list_has_timeline_source(rep_sl) {
                    merged = Some(match merged {
                        None => rep_sl.clone(),
                        Some(parent) => merge_segment_list(&parent, rep_sl),
                    });
                    break;
                }
            }
        }
    }

    merged.ok_or(ManifestError::MissingSegmentList)
}

/// Effective `SegmentList` for fetching init/media of one representation.
pub(crate) fn segment_list_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentList, ManifestError> {
    merge_segment_list_chain(&[
        period.SegmentList.as_ref(),
        adaptation_set.SegmentList.as_ref(),
        representation.SegmentList.as_ref(),
    ])
    .ok_or(ManifestError::MissingSegmentList)
}

/// Effective segment addressing for timeline expansion on an adaptation set.
pub(crate) fn segment_addressing_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentAddressing, ManifestError> {
    validate_addressing_hierarchy(period, adaptation_set, None)?;
    if adaptation_set_uses_segment_list(period, adaptation_set) {
        return Ok(SegmentAddressing::List(segment_list_for_timeline(
            period,
            adaptation_set,
        )?));
    }
    if adaptation_set_uses_segment_template(period, adaptation_set) {
        return Ok(SegmentAddressing::Template(segment_template_for_timeline(
            period,
            adaptation_set,
        )?));
    }
    if adaptation_set_uses_segment_base(period, adaptation_set) {
        return Ok(SegmentAddressing::Base(segment_base_for_timeline(
            period,
            adaptation_set,
        )?));
    }
    // Sidecar WebVTT / progressive On-Demand often ships a Representation BaseURL with no
    // SegmentBase element; treat that as a whole-file progressive SegmentBase.
    if has_progressive_base_url(period, adaptation_set, None)
        || adaptation_set
            .representations
            .iter()
            .any(|r| has_progressive_base_url(period, adaptation_set, Some(r)))
    {
        return Ok(SegmentAddressing::Base(SegmentBase::default()));
    }
    Err(ManifestError::MissingSegmentTemplate)
}

/// Effective segment addressing for fetching init/media of one representation.
pub(crate) fn segment_addressing_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentAddressing, ManifestError> {
    validate_addressing_hierarchy(period, adaptation_set, Some(representation))?;
    if has_segment_list_in_chain(period, adaptation_set, Some(representation)) {
        return Ok(SegmentAddressing::List(segment_list_for_representation(
            period,
            adaptation_set,
            representation,
        )?));
    }
    if has_segment_template_in_chain(period, adaptation_set, Some(representation)) {
        return Ok(SegmentAddressing::Template(
            segment_template_for_representation(period, adaptation_set, representation)?,
        ));
    }
    if has_segment_base_in_chain(period, adaptation_set, Some(representation)) {
        return Ok(SegmentAddressing::Base(segment_base_for_representation(
            period,
            adaptation_set,
            representation,
        )?));
    }
    if has_progressive_base_url(period, adaptation_set, Some(representation)) {
        return Ok(SegmentAddressing::Base(SegmentBase::default()));
    }
    Err(ManifestError::MissingSegmentTemplate)
}

/// `SegmentList@Initialization@sourceURL` for the effective merged list.
pub(crate) fn segment_list_init_source(sl: &SegmentList) -> Result<&str, ManifestError> {
    sl.Initialization
        .as_ref()
        .and_then(|init| init.sourceURL.as_deref())
        .ok_or(ManifestError::MissingInitializationTemplate)
}

pub(crate) fn segment_template_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentTemplate, ManifestError> {
    let mut st = merge_segment_template_chain(&[
        period.SegmentTemplate.as_ref(),
        adaptation_set.SegmentTemplate.as_ref(),
        representation.SegmentTemplate.as_ref(),
    ])
    .ok_or(ManifestError::MissingSegmentTemplate)?;
    if let Some(ri) = &representation.representation_index {
        st.representation_index = Some(match st.representation_index {
            Some(parent) => super::inheritance::merge_representation_index(&parent, ri),
            None => ri.clone(),
        });
    }
    Ok(st)
}

pub(crate) fn resolved_initialization_path(
    addressing: &SegmentAddressing,
    vars: &TemplateVars<'_>,
) -> Option<String> {
    match addressing {
        SegmentAddressing::Template(st) => st
            .initialization
            .as_deref()
            .map(|init_tpl| interpolate_template(init_tpl, vars)),
        SegmentAddressing::List(sl) => segment_list_init_source(sl)
            .ok()
            .map(|init_src| interpolate_template(init_src, vars)),
        SegmentAddressing::Base(sb) => sb
            .Initialization
            .as_ref()
            .and_then(|init| init.sourceURL.as_deref())
            .map(|init_src| interpolate_template(init_src, vars)),
    }
}

#[cfg(test)]
mod tests;
