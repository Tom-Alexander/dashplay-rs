use dash_mpd::{
    AdaptationSet, Period, Representation, SegmentBase, SegmentList, SegmentTemplate, SegmentURL,
};

use crate::manifest::ManifestError;

use super::{
    SegmentAddressing, segment_addressing_for_representation, segment_addressing_for_timeline,
    segment_list_for_representation, segment_template_for_representation,
    segment_template_for_timeline, segment_template_uses_global_sidecar_index,
    segment_template_uses_per_segment_index, segment_template_uses_sidecar_index,
};

#[test]
fn segment_template_inheritance_merges_period_and_adaptation_set() {
    let period = Period {
        SegmentTemplate: Some(SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            startNumber: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    let adaptation_set = AdaptationSet {
        SegmentTemplate: Some(SegmentTemplate {
            initialization: Some("init.mp4".into()),
            media: Some("seg-$Number$.m4s".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let timeline = segment_template_for_timeline(&period, &adaptation_set).unwrap();
    assert_eq!(timeline.timescale, Some(1000));
    assert_eq!(timeline.duration, Some(4000.0));
    assert_eq!(timeline.startNumber, Some(1));
    assert_eq!(timeline.initialization.as_deref(), Some("init.mp4"));
    assert_eq!(timeline.media.as_deref(), Some("seg-$Number$.m4s"));
}

#[test]
fn segment_template_inheritance_supplements_timeline_from_representation() {
    let period = Period {
        SegmentTemplate: Some(SegmentTemplate {
            timescale: Some(90000),
            startNumber: Some(1),
            ..Default::default()
        }),
        ..Default::default()
    };
    let adaptation_set = AdaptationSet {
        representations: vec![Representation {
            SegmentTemplate: Some(SegmentTemplate {
                initialization: Some("i.mp4".into()),
                media: Some("m$Number$.mp4".into()),
                SegmentTimeline: Some(dash_mpd::SegmentTimeline {
                    segments: vec![dash_mpd::S {
                        t: Some(0),
                        d: 180000,
                        r: Some(1),
                        ..Default::default()
                    }],
                }),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };

    let timeline = segment_template_for_timeline(&period, &adaptation_set).unwrap();
    assert_eq!(timeline.timescale, Some(90000));
    assert!(timeline.SegmentTimeline.is_some());
    assert_eq!(timeline.initialization.as_deref(), Some("i.mp4"));

    let rep = &adaptation_set.representations[0];
    let rep_tpl = segment_template_for_representation(&period, &adaptation_set, rep).unwrap();
    assert_eq!(rep_tpl.media.as_deref(), Some("m$Number$.mp4"));
}

#[test]
fn segment_list_inheritance_merges_period_and_representation() {
    let period = Period {
        SegmentList: Some(SegmentList {
            timescale: Some(1000),
            duration: Some(2000),
            ..Default::default()
        }),
        ..Default::default()
    };
    let adaptation_set = AdaptationSet {
        representations: vec![Representation {
            SegmentList: Some(SegmentList {
                Initialization: Some(dash_mpd::Initialization {
                    sourceURL: Some("rep-init.mp4".into()),
                    ..Default::default()
                }),
                segment_urls: vec![SegmentURL {
                    media: Some("seg.m4s".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };

    let rep = &adaptation_set.representations[0];
    let merged = segment_list_for_representation(&period, &adaptation_set, rep).unwrap();
    assert_eq!(merged.timescale, Some(1000));
    assert_eq!(merged.duration, Some(2000));
    assert_eq!(
        merged.Initialization.as_ref().unwrap().sourceURL.as_deref(),
        Some("rep-init.mp4")
    );
    assert_eq!(merged.segment_urls.len(), 1);

    let addressing = segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
    assert!(matches!(addressing, SegmentAddressing::List(_)));
}

#[test]
fn segment_addressing_rejects_conflicting_modes_at_period() {
    let period = Period {
        SegmentTemplate: Some(SegmentTemplate {
            media: Some("tpl-$Number$.m4s".into()),
            ..Default::default()
        }),
        SegmentList: Some(SegmentList {
            segment_urls: vec![SegmentURL {
                media: Some("list.m4s".into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let adaptation_set = AdaptationSet {
        representations: vec![Representation::default()],
        ..Default::default()
    };
    let err = segment_addressing_for_timeline(&period, &adaptation_set).unwrap_err();
    assert!(matches!(
        err,
        ManifestError::ConflictingSegmentAddressing("Period")
    ));
}

#[test]
fn segment_addressing_rejects_conflicting_modes_at_adaptation_set() {
    let period = Period::default();
    let adaptation_set = AdaptationSet {
        SegmentTemplate: Some(SegmentTemplate {
            media: Some("tpl-$Number$.m4s".into()),
            ..Default::default()
        }),
        SegmentBase: Some(SegmentBase {
            indexRange: Some("0-10".into()),
            ..Default::default()
        }),
        representations: vec![Representation::default()],
        ..Default::default()
    };
    let err = segment_addressing_for_timeline(&period, &adaptation_set).unwrap_err();
    assert!(matches!(
        err,
        ManifestError::ConflictingSegmentAddressing("AdaptationSet")
    ));
}

#[test]
fn segment_addressing_rejects_conflicting_modes_at_representation() {
    let period = Period::default();
    let adaptation_set = AdaptationSet {
        representations: vec![Representation {
            SegmentList: Some(SegmentList {
                segment_urls: vec![SegmentURL {
                    media: Some("list.m4s".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            SegmentBase: Some(SegmentBase {
                indexRange: Some("0-10".into()),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let rep = &adaptation_set.representations[0];
    let err = segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap_err();
    assert!(matches!(
        err,
        ManifestError::ConflictingSegmentAddressing("Representation")
    ));
}

#[test]
fn segment_addressing_prefers_list_over_template() {
    let period = Period::default();
    let adaptation_set = AdaptationSet {
        SegmentTemplate: Some(SegmentTemplate {
            media: Some("tpl-$Number$.m4s".into()),
            ..Default::default()
        }),
        representations: vec![Representation {
            SegmentList: Some(SegmentList {
                duration: Some(1000),
                segment_urls: vec![SegmentURL {
                    media: Some("list.m4s".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let rep = &adaptation_set.representations[0];
    let addressing = segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
    match addressing {
        SegmentAddressing::List(sl) => {
            assert_eq!(sl.segment_urls[0].media.as_deref(), Some("list.m4s"));
        }
        SegmentAddressing::Template(_) => panic!("expected SegmentList addressing"),
        SegmentAddressing::Base(_) => panic!("expected SegmentList addressing"),
    }
}

#[test]
fn segment_addressing_prefers_template_over_base() {
    let period = Period {
        SegmentBase: Some(SegmentBase {
            indexRange: Some("0-10".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let adaptation_set = AdaptationSet {
        representations: vec![Representation {
            SegmentTemplate: Some(SegmentTemplate {
                media: Some("seg-$Number$.m4s".into()),
                initialization: Some("init.mp4".into()),
                duration: Some(4000.0),
                timescale: Some(1000),
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let rep = &adaptation_set.representations[0];
    let addressing = segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
    assert!(matches!(addressing, SegmentAddressing::Template(_)));
}

#[test]
fn segment_template_sidecar_index_requires_index_and_index_range_or_representation_index() {
    assert!(!segment_template_uses_sidecar_index(&SegmentTemplate {
        index: Some("idx.mp4".into()),
        ..Default::default()
    }));
    assert!(!segment_template_uses_sidecar_index(&SegmentTemplate {
        indexRange: Some("0-10".into()),
        ..Default::default()
    }));
    assert!(segment_template_uses_sidecar_index(&SegmentTemplate {
        index: Some("idx.mp4".into()),
        indexRange: Some("0-10".into()),
        ..Default::default()
    }));
    assert!(segment_template_uses_sidecar_index(&SegmentTemplate {
        representation_index: Some(dash_mpd::RepresentationIndex {
            sourceURL: Some("idx.mp4".into()),
            range: Some("0-10".into()),
        }),
        ..Default::default()
    }));
}

#[test]
fn segment_template_per_segment_index_detects_number_and_time_identifiers() {
    assert!(segment_template_uses_per_segment_index(&SegmentTemplate {
        index: Some("idx-$Number$.mp4".into()),
        indexRange: Some("0-10".into()),
        ..Default::default()
    }));
    assert!(segment_template_uses_per_segment_index(&SegmentTemplate {
        index: Some("idx-$Time%05d$.mp4".into()),
        indexRange: Some("0-10".into()),
        ..Default::default()
    }));
    assert!(!segment_template_uses_per_segment_index(&SegmentTemplate {
        index: Some("idx.mp4".into()),
        indexRange: Some("0-10".into()),
        ..Default::default()
    }));
    assert!(segment_template_uses_global_sidecar_index(
        &SegmentTemplate {
            index: Some("idx.mp4".into()),
            indexRange: Some("0-10".into()),
            ..Default::default()
        }
    ));
}

#[test]
fn segment_template_per_segment_representation_index_detects_number_identifiers() {
    assert!(segment_template_uses_per_segment_index(&SegmentTemplate {
        representation_index: Some(dash_mpd::RepresentationIndex {
            sourceURL: Some("idx-$Number$.mp4".into()),
            ..Default::default()
        }),
        ..Default::default()
    }));
}

#[test]
fn progressive_base_url_only_uses_empty_segment_base() {
    let period = Period::default();
    let adaptation_set = AdaptationSet {
        mimeType: Some("text/vtt".into()),
        lang: Some("en".into()),
        representations: vec![Representation {
            BaseURL: vec![dash_mpd::BaseURL {
                base: "captions.vtt".into(),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let rep = &adaptation_set.representations[0];

    let addressing = segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
    assert!(matches!(
        addressing,
        SegmentAddressing::Base(SegmentBase {
            indexRange: None,
            representation_index: None,
            ..
        })
    ));

    let timeline =
        segment_addressing_for_timeline(&period, &adaptation_set).unwrap();
    assert!(matches!(timeline, SegmentAddressing::Base(_)));
}
