use dash_mpd::{AdaptationSet, Period, Representation, SegmentBase, SegmentList, SegmentTemplate};

use crate::PlayerError;

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

/// `SegmentTemplate@index` + `@indexRange`: timeline comes from a separate index document.
pub(crate) fn segment_template_uses_sidecar_index(st: &SegmentTemplate) -> bool {
    st.index.is_some() && st.indexRange.is_some()
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
    st.index
        .as_deref()
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
) -> Result<SegmentTemplate, PlayerError> {
    let mut merged = merge_segment_template_chain(&[
        period.SegmentTemplate.as_ref(),
        adaptation_set.SegmentTemplate.as_ref(),
    ]);

    if merged
        .as_ref()
        .is_none_or(|st| !segment_template_has_addressing_source(st))
    {
        for rep in &adaptation_set.representations {
            if let Some(rep_st) = &rep.SegmentTemplate {
                if segment_template_has_addressing_source(rep_st) {
                    merged = Some(match merged {
                        None => rep_st.clone(),
                        Some(parent) => merge_segment_template(&parent, rep_st),
                    });
                    break;
                }
            }
        }
    }

    merged.ok_or(PlayerError::MissingSegmentTemplate)
}

/// Resolved segment addressing mode after Period → AdaptationSet → Representation inheritance.
#[derive(Debug, Clone)]
pub(crate) enum SegmentAddressing {
    Template(SegmentTemplate),
    List(SegmentList),
    Base(SegmentBase),
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
            .any(|r| r.SegmentTemplate.is_some())
}

fn adaptation_set_uses_segment_base(period: &Period, adaptation_set: &AdaptationSet) -> bool {
    period.SegmentBase.is_some()
        || adaptation_set.SegmentBase.is_some()
        || adaptation_set
            .representations
            .iter()
            .any(|r| r.SegmentBase.is_some())
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
) -> Result<SegmentBase, PlayerError> {
    merge_segment_base_chain(&[
        period.SegmentBase.as_ref(),
        adaptation_set.SegmentBase.as_ref(),
    ])
    .or_else(|| {
        adaptation_set
            .representations
            .iter()
            .find_map(|r| r.SegmentBase.as_ref())
            .cloned()
    })
    .ok_or(PlayerError::MissingSegmentBase)
}

/// Effective `SegmentBase` for fetching init/media of one representation.
pub(crate) fn segment_base_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentBase, PlayerError> {
    merge_segment_base_chain(&[
        period.SegmentBase.as_ref(),
        adaptation_set.SegmentBase.as_ref(),
        representation.SegmentBase.as_ref(),
    ])
    .ok_or(PlayerError::MissingSegmentBase)
}

pub(crate) fn segment_list_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentList, PlayerError> {
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

    merged.ok_or(PlayerError::MissingSegmentList)
}

/// Effective `SegmentList` for fetching init/media of one representation.
pub(crate) fn segment_list_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentList, PlayerError> {
    merge_segment_list_chain(&[
        period.SegmentList.as_ref(),
        adaptation_set.SegmentList.as_ref(),
        representation.SegmentList.as_ref(),
    ])
    .ok_or(PlayerError::MissingSegmentList)
}

/// Effective segment addressing for timeline expansion on an adaptation set.
pub(crate) fn segment_addressing_for_timeline(
    period: &Period,
    adaptation_set: &AdaptationSet,
) -> Result<SegmentAddressing, PlayerError> {
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
    Err(PlayerError::MissingSegmentTemplate)
}

/// Effective segment addressing for fetching init/media of one representation.
pub(crate) fn segment_addressing_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentAddressing, PlayerError> {
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
    Err(PlayerError::MissingSegmentTemplate)
}

/// `SegmentList@Initialization@sourceURL` for the effective merged list.
pub(crate) fn segment_list_init_source(sl: &SegmentList) -> Result<&str, PlayerError> {
    sl.Initialization
        .as_ref()
        .and_then(|init| init.sourceURL.as_deref())
        .ok_or(PlayerError::MissingInitializationTemplate)
}

/// Media path for a segment index under `SegmentList` addressing (1-based segment number).
pub(crate) fn segment_list_media_for_index(
    sl: &SegmentList,
    segment_index: usize,
) -> Result<&str, PlayerError> {
    let su = sl
        .segment_urls
        .get(segment_index)
        .ok_or(PlayerError::EmptySegmentList)?;
    su.media.as_deref().ok_or(PlayerError::MissingMediaTemplate)
}
pub(crate) fn segment_template_for_representation(
    period: &Period,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<SegmentTemplate, PlayerError> {
    merge_segment_template_chain(&[
        period.SegmentTemplate.as_ref(),
        adaptation_set.SegmentTemplate.as_ref(),
        representation.SegmentTemplate.as_ref(),
    ])
    .ok_or(PlayerError::MissingSegmentTemplate)
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
