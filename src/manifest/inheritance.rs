use dash_mpd::{RepresentationIndex, SegmentBase, SegmentList, SegmentTemplate};

pub(super) fn merge_representation_index(
    parent: &RepresentationIndex,
    child: &RepresentationIndex,
) -> RepresentationIndex {
    RepresentationIndex {
        sourceURL: child.sourceURL.clone().or_else(|| parent.sourceURL.clone()),
        range: child.range.clone().or_else(|| parent.range.clone()),
    }
}

/// Merge two `SegmentTemplate` nodes: `child` attributes override `parent` when present.
pub(super) fn merge_segment_template(
    parent: &SegmentTemplate,
    child: &SegmentTemplate,
) -> SegmentTemplate {
    SegmentTemplate {
        media: child.media.clone().or_else(|| parent.media.clone()),
        index: child.index.clone().or_else(|| parent.index.clone()),
        initialization: child
            .initialization
            .clone()
            .or_else(|| parent.initialization.clone()),
        bitstreamSwitching: child
            .bitstreamSwitching
            .clone()
            .or_else(|| parent.bitstreamSwitching.clone()),
        indexRange: child
            .indexRange
            .clone()
            .or_else(|| parent.indexRange.clone()),
        indexRangeExact: child.indexRangeExact.or(parent.indexRangeExact),
        startNumber: child.startNumber.or(parent.startNumber),
        duration: child.duration.or(parent.duration),
        timescale: child.timescale.or(parent.timescale),
        eptDelta: child.eptDelta.or(parent.eptDelta),
        pbDelta: child.pbDelta.or(parent.pbDelta),
        presentationTimeOffset: child
            .presentationTimeOffset
            .or(parent.presentationTimeOffset),
        availabilityTimeOffset: child
            .availabilityTimeOffset
            .or(parent.availabilityTimeOffset),
        availabilityTimeComplete: child
            .availabilityTimeComplete
            .or(parent.availabilityTimeComplete),
        Initialization: child
            .Initialization
            .clone()
            .or_else(|| parent.Initialization.clone()),
        representation_index: child
            .representation_index
            .clone()
            .or_else(|| parent.representation_index.clone()),
        failover_content: child
            .failover_content
            .clone()
            .or_else(|| parent.failover_content.clone()),
        SegmentTimeline: child
            .SegmentTimeline
            .clone()
            .or_else(|| parent.SegmentTimeline.clone()),
        BitstreamSwitching: child
            .BitstreamSwitching
            .clone()
            .or_else(|| parent.BitstreamSwitching.clone()),
    }
}

pub(super) fn merge_segment_template_chain(
    templates: &[Option<&SegmentTemplate>],
) -> Option<SegmentTemplate> {
    templates.iter().filter_map(|t| *t).fold(None, |acc, st| {
        Some(match acc {
            None => st.clone(),
            Some(parent) => merge_segment_template(&parent, st),
        })
    })
}
pub(super) fn merge_segment_base(parent: &SegmentBase, child: &SegmentBase) -> SegmentBase {
    SegmentBase {
        timescale: child.timescale.or(parent.timescale),
        presentationTimeOffset: child
            .presentationTimeOffset
            .or(parent.presentationTimeOffset),
        indexRange: child
            .indexRange
            .clone()
            .or_else(|| parent.indexRange.clone()),
        indexRangeExact: child.indexRangeExact.or(parent.indexRangeExact),
        availabilityTimeOffset: child
            .availabilityTimeOffset
            .or(parent.availabilityTimeOffset),
        availabilityTimeComplete: child
            .availabilityTimeComplete
            .or(parent.availabilityTimeComplete),
        presentationDuration: child.presentationDuration.or(parent.presentationDuration),
        eptDelta: child.eptDelta.or(parent.eptDelta),
        pbDelta: child.pbDelta.or(parent.pbDelta),
        Initialization: child
            .Initialization
            .clone()
            .or_else(|| parent.Initialization.clone()),
        representation_index: child
            .representation_index
            .clone()
            .or_else(|| parent.representation_index.clone()),
        failover_content: child
            .failover_content
            .clone()
            .or_else(|| parent.failover_content.clone()),
    }
}

pub(super) fn merge_segment_base_chain(bases: &[Option<&SegmentBase>]) -> Option<SegmentBase> {
    bases.iter().filter_map(|sb| *sb).fold(None, |acc, sb| {
        Some(match acc {
            None => sb.clone(),
            Some(parent) => merge_segment_base(&parent, sb),
        })
    })
}
pub(super) fn merge_segment_list(parent: &SegmentList, child: &SegmentList) -> SegmentList {
    SegmentList {
        duration: child.duration.or(parent.duration),
        timescale: child.timescale.or(parent.timescale),
        indexRange: child
            .indexRange
            .clone()
            .or_else(|| parent.indexRange.clone()),
        indexRangeExact: child.indexRangeExact.or(parent.indexRangeExact),
        href: child.href.clone().or_else(|| parent.href.clone()),
        actuate: child.actuate.clone().or_else(|| parent.actuate.clone()),
        sltype: child.sltype.clone().or_else(|| parent.sltype.clone()),
        show: child.show.clone().or_else(|| parent.show.clone()),
        Initialization: child
            .Initialization
            .clone()
            .or_else(|| parent.Initialization.clone()),
        SegmentTimeline: child
            .SegmentTimeline
            .clone()
            .or_else(|| parent.SegmentTimeline.clone()),
        BitstreamSwitching: child
            .BitstreamSwitching
            .clone()
            .or_else(|| parent.BitstreamSwitching.clone()),
        segment_urls: if child.segment_urls.is_empty() {
            parent.segment_urls.clone()
        } else {
            child.segment_urls.clone()
        },
    }
}

pub(super) fn merge_segment_list_chain(lists: &[Option<&SegmentList>]) -> Option<SegmentList> {
    lists.iter().filter_map(|sl| *sl).fold(None, |acc, sl| {
        Some(match acc {
            None => sl.clone(),
            Some(parent) => merge_segment_list(&parent, sl),
        })
    })
}

pub(super) fn segment_list_has_timeline_source(sl: &SegmentList) -> bool {
    sl.SegmentTimeline.is_some() || sl.duration.is_some()
}
