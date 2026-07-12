#[cfg(test)]
mod manifest_logic_tests {
    use std::time::Duration;

    use chrono::{TimeZone, Utc};
    use dash_mpd::{
        AdaptationSet, BaseURL, MPD, Period, Representation, SegmentBase, SegmentList,
        SegmentTemplate, SegmentURL,
    };
    use url::Url;

    use crate::PlayerError;
    use crate::manifest::*;

    #[test]
    fn merge_base_url_relative_and_absolute() {
        let base = Url::parse("https://cdn.example/vod/?token=abc").unwrap();
        let rel = merge_base_url(&base, "segments/").unwrap();
        assert_eq!(rel.as_str(), "https://cdn.example/vod/segments/?token=abc");

        let abs = merge_base_url(&base, "https://alt.example/").unwrap();
        assert_eq!(abs.as_str(), "https://alt.example/");
    }

    #[test]
    fn segment_bases_expand_hierarchy_and_dedupe() {
        let ctx = SegmentBaseContext {
            manifest_uri: Url::parse("https://example.com/manifest.mpd?sig=1").unwrap(),
            mpd_base_urls: vec![BaseURL {
                base: "mpd/".into(),
                ..Default::default()
            }],
            period_base_urls: vec![BaseURL {
                base: "period/".into(),
                ..Default::default()
            }],
            service_location_priority: Vec::new(),
            default_service_location: None,
        };
        let adaptation_set = AdaptationSet {
            BaseURL: vec![BaseURL {
                base: "as/".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let representation = Representation {
            BaseURL: vec![
                BaseURL {
                    base: "rep-a/".into(),
                    ..Default::default()
                },
                BaseURL {
                    base: "rep-a/".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let bases =
            segment_bases_for_representation(&ctx, &adaptation_set, &representation).unwrap();
        assert_eq!(bases.len(), 1);
        assert!(bases[0].as_str().contains("/rep-a"));
        assert!(bases[0].as_str().contains("/as/"));
        assert_eq!(bases[0].query(), Some("sig=1"));
    }

    #[test]
    fn period_windows_chain_period_starts() {
        let mpd = MPD {
            periods: vec![
                Period {
                    duration: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
                Period {
                    start: Some(Duration::from_secs(10)),
                    duration: Some(Duration::from_secs(5)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let windows = period_windows(&mpd).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].start, Duration::ZERO);
        assert_eq!(windows[0].end, Some(Duration::from_secs(10)));
        assert_eq!(windows[1].start, Duration::from_secs(10));
        assert_eq!(windows[1].end, Some(Duration::from_secs(15)));
    }

    #[test]
    fn current_period_window_static_mpd_starts_at_first_period() {
        let mpd = MPD {
            mpdtype: Some("static".into()),
            periods: vec![
                Period {
                    duration: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
                Period {
                    start: Some(Duration::from_secs(10)),
                    duration: Some(Duration::from_secs(5)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        assert_eq!(current_period_window_at(&mpd, now).unwrap().idx, 0);
    }

    #[test]
    fn current_period_window_selects_by_availability_time() {
        let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        let mpd = MPD {
            availabilityStartTime: Some(ast),
            periods: vec![
                Period {
                    duration: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
                Period {
                    start: Some(Duration::from_secs(10)),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let in_first = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 5).unwrap();
        assert_eq!(current_period_window_at(&mpd, in_first).unwrap().idx, 0);

        let in_second = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 12).unwrap();
        assert_eq!(current_period_window_at(&mpd, in_second).unwrap().idx, 1);
    }

    #[test]
    fn target_presentation_time_applies_suggested_delay() {
        let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        let mpd = MPD {
            availabilityStartTime: Some(ast),
            suggestedPresentationDelay: Some(Duration::from_secs(2)),
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 10).unwrap();
        assert_eq!(
            target_presentation_time_at(&mpd, now).unwrap(),
            Some(Duration::from_secs(8))
        );
    }

    #[test]
    fn target_presentation_time_prefers_service_description_latency() {
        use dash_mpd::{Latency, ServiceDescription};

        let ast = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 0).unwrap();
        let mpd = MPD {
            availabilityStartTime: Some(ast),
            suggestedPresentationDelay: Some(Duration::from_secs(2)),
            ServiceDescription: vec![ServiceDescription {
                Latency: vec![Latency {
                    target: Some(3500.0),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let now = Utc.with_ymd_and_hms(2020, 5, 1, 12, 0, 10).unwrap();
        assert_eq!(
            target_presentation_time_at(&mpd, now).unwrap(),
            Some(Duration::from_secs_f64(6.5))
        );
    }

    #[test]
    fn segment_availability_waits_for_availability_time_offset_when_complete() {
        let seg = TimelineSegment {
            number: 3,
            time: 8000,
            duration: 4000,
            duration_s: 4.0,
            presentation_time_s: 8.0,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        };
        let availability = SegmentAvailability {
            availability_time_offset_s: Some(7.0),
            availability_time_complete: true,
        };
        assert!(!segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(14),
            &availability,
        ));
        assert!(segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(15),
            &availability,
        ));
    }

    #[test]
    fn segment_availability_partial_whole_segment_available_at_sap() {
        let seg = TimelineSegment {
            number: 3,
            time: 8000,
            duration: 4000,
            duration_s: 4.0,
            presentation_time_s: 8.0,
            sub_number: None,
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        };
        let availability = SegmentAvailability {
            availability_time_offset_s: Some(7.0),
            availability_time_complete: false,
        };
        assert!(!segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(7),
            &availability,
        ));
        assert!(segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(8),
            &availability,
        ));
    }

    #[test]
    fn segment_availability_uses_sequence_start_for_subsegments() {
        let seg = TimelineSegment {
            number: 1,
            time: 0,
            duration: 1000,
            duration_s: 1.0,
            presentation_time_s: 2.0,
            sub_number: Some(3),
            resync_start_chunk: None,
            media_url: None,
            media_range: None,
        };
        assert!((segment_sequence_start_s(Duration::ZERO, &seg) - 0.0).abs() < 1e-6);
        let availability = SegmentAvailability {
            availability_time_offset_s: Some(5.0),
            availability_time_complete: true,
        };
        assert!(!segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(4),
            &availability,
        ));
        assert!(segment_is_available(
            &seg,
            Duration::ZERO,
            Duration::from_secs(5),
            &availability,
        ));
    }

    #[test]
    fn filter_segments_by_availability_drops_unpublished_complete_live_edge() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            startNumber: Some(1),
            availabilityTimeOffset: Some(7.0),
            availabilityTimeComplete: Some(true),
            ..Default::default()
        };
        let addressing = SegmentAddressing::Template(st.clone());
        let ctx = TimelineBuildContext {
            is_dynamic: true,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: None,
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: Some(Duration::from_secs(20)),
            since_availability_start: Some(Duration::from_secs(12)),
            resync_hints: None,
        };
        let segments = timeline_segments(&st, &ctx, None).unwrap();
        let filtered = filter_segments_by_availability(
            segments,
            true,
            Duration::ZERO,
            ctx.since_availability_start,
            &addressing,
        );
        let numbers: Vec<_> = filtered.iter().map(|s| s.number).collect();
        assert_eq!(numbers, vec![1, 2]);
    }

    #[test]
    fn filter_segments_by_availability_includes_partial_live_edge_at_sap() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            startNumber: Some(1),
            availabilityTimeOffset: Some(7.0),
            availabilityTimeComplete: Some(false),
            ..Default::default()
        };
        let addressing = SegmentAddressing::Template(st.clone());
        let ctx = TimelineBuildContext {
            is_dynamic: true,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: None,
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: Some(Duration::from_secs(20)),
            since_availability_start: Some(Duration::from_secs(11)),
            resync_hints: None,
        };
        let segments = timeline_segments(&st, &ctx, None).unwrap();
        let filtered = filter_segments_by_availability(
            segments,
            true,
            Duration::ZERO,
            ctx.since_availability_start,
            &addressing,
        );
        let numbers: Vec<_> = filtered.iter().map(|s| s.number).collect();
        assert_eq!(numbers, vec![1, 2, 3]);
    }

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
    fn static_duration_template_emits_expected_segment_count() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: false,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: Some(Duration::from_secs(8)),
            },
            period_duration: None,
            media_presentation_duration: Some(Duration::from_secs(8)),
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
        };

        let segs = timeline_segments(&st, &ctx, None).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].number, 1);
        assert_eq!(segs[1].number, 2);
    }

    #[test]
    fn static_duration_template_bounds_by_end_number_without_period_extent() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: false,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: None,
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
        };

        let err = timeline_segments(&st, &ctx, None).unwrap_err();
        assert!(matches!(
            err,
            PlayerError::MissingPeriodExtentForStaticTemplate
        ));

        let segs = timeline_segments(&st, &ctx, Some(2)).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].number, 1);
        assert_eq!(segs[1].number, 2);
    }

    #[test]
    fn static_duration_template_prefers_end_number_over_period_extent() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: false,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: Some(Duration::from_secs(8)),
            },
            period_duration: None,
            media_presentation_duration: Some(Duration::from_secs(8)),
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
        };

        let segs = timeline_segments(&st, &ctx, Some(1)).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].number, 1);
    }

    #[test]
    fn parse_segment_template_end_numbers_reads_adaptation_set_attribute() {
        let xml = include_str!("../../tests/fixtures/dashif_simple/manifest.mpd");
        let mpd = dash_mpd::parse(xml).unwrap();
        let supplements = parse_segment_template_end_numbers(xml).unwrap();
        let end = end_number_for_timeline(
            &mpd.periods[0],
            &mpd.periods[0].adaptations[0],
            &supplements,
            0,
            0,
        );
        assert_eq!(end, Some(4));
    }

    #[test]
    fn dynamic_duration_template_limits_window_to_time_shift_buffer() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            presentationTimeOffset: Some(0),
            startNumber: Some(1),
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: true,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: None,
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: Some(Duration::from_secs(8)),
            since_availability_start: Some(Duration::from_secs(20)),
            resync_hints: None,
        };

        let segs = timeline_segments(&st, &ctx, None).unwrap();
        assert_eq!(segs.first().map(|s| s.number), Some(2));
        assert_eq!(segs.last().map(|s| s.number), Some(6));
    }

    #[test]
    fn segment_list_explicit_urls_builds_timeline() {
        let sl = SegmentList {
            timescale: Some(1000),
            duration: Some(4000),
            Initialization: Some(dash_mpd::Initialization {
                sourceURL: Some("init.mp4".into()),
                ..Default::default()
            }),
            segment_urls: vec![
                SegmentURL {
                    media: Some("seg-1.m4s".into()),
                    ..Default::default()
                },
                SegmentURL {
                    media: Some("seg-2.m4s".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: false,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: Some(Duration::from_secs(8)),
            },
            period_duration: None,
            media_presentation_duration: Some(Duration::from_secs(8)),
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
        };

        let segs = timeline_segments_from_list(&sl, &ctx).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].media_url.as_deref(), Some("seg-1.m4s"));
        assert_eq!(segs[1].media_url.as_deref(), Some("seg-2.m4s"));
        assert!((segs[0].duration_s - 4.0).abs() < 1e-9);
        assert!((segs[1].presentation_time_s - 4.0).abs() < 1e-9);
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

        let addressing =
            segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
        assert!(matches!(addressing, SegmentAddressing::List(_)));
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
        let addressing =
            segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
        match addressing {
            SegmentAddressing::List(sl) => {
                assert_eq!(sl.segment_urls[0].media.as_deref(), Some("list.m4s"));
            }
            SegmentAddressing::Template(_) => panic!("expected SegmentList addressing"),
            SegmentAddressing::Base(_) => panic!("expected SegmentList addressing"),
        }
    }

    #[test]
    fn parse_byte_range_accepts_inclusive_specifier() {
        let br = parse_byte_range("7-62").unwrap();
        assert_eq!(br.start, 7);
        assert_eq!(br.end, 62);
        assert!(parse_byte_range("bad").is_err());
        assert!(parse_byte_range("10-5").is_err());
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
        let addressing =
            segment_addressing_for_representation(&period, &adaptation_set, rep).unwrap();
        assert!(matches!(addressing, SegmentAddressing::Template(_)));
    }

    #[test]
    fn segment_base_init_target_uses_range_on_base_url() {
        let sb = SegmentBase {
            Initialization: Some(dash_mpd::Initialization {
                range: Some("0-6".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let vars = TemplateVars {
            representation_id: "1",
            ..Default::default()
        };
        let target = segment_base_init_target(&sb, &vars).unwrap();
        assert_eq!(target.path, "");
        assert_eq!(target.range, Some(ByteRange { start: 0, end: 6 }));
    }

    fn minimal_sidx_bytes(seg_sizes: &[(u32, u32)], timescale: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(0); // version
        body.extend_from_slice(&[0, 0, 0]); // flags
        body.extend_from_slice(&1u32.to_be_bytes()); // reference_id
        body.extend_from_slice(&timescale.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes()); // ept
        body.extend_from_slice(&0u32.to_be_bytes()); // first_offset
        body.extend_from_slice(&0u16.to_be_bytes()); // reserved
        body.extend_from_slice(&(seg_sizes.len() as u16).to_be_bytes());
        for &(size, dur) in seg_sizes {
            body.extend_from_slice(&(size & 0x7FFF_FFFF).to_be_bytes());
            body.extend_from_slice(&dur.to_be_bytes());
            body.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        let mut out = (8 + body.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b"sidx");
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn parse_sidx_index_builds_timeline_with_byte_ranges() {
        let seg1_len = 11u32;
        let seg2_len = 11u32;
        let init_len = 7usize;
        let sidx = minimal_sidx_bytes(&[(seg1_len, 2000), (seg2_len, 2000)], 1000);
        let index_start = init_len;
        let index_end = init_len + sidx.len() - 1;
        let sb = SegmentBase {
            timescale: Some(1000),
            indexRange: Some(format!("{index_start}-{index_end}")),
            ..Default::default()
        };
        let segs = parse_sidx_index(&sb, &sidx).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(
            segs[0].media_range,
            Some(ByteRange {
                start: (index_end + 1) as u64,
                end: (index_end + 1 + seg1_len as usize - 1) as u64,
            })
        );
        assert!((segs[0].duration_s - 2.0).abs() < 1e-9);
        assert!((segs[1].presentation_time_s - 2.0).abs() < 1e-9);
    }

    #[test]
    fn parse_sidx_index_from_template_sidecar_uses_first_offset() {
        let seg1_len = 11u32;
        let seg2_len = 11u32;
        let sidx = minimal_sidx_bytes(&[(seg1_len, 2000), (seg2_len, 2000)], 1000);
        let st = SegmentTemplate {
            timescale: Some(1000),
            index: Some("index.mp4".into()),
            indexRange: Some(format!("0-{}", sidx.len() - 1)),
            startNumber: Some(1),
            ..Default::default()
        };
        let segs = parse_sidx_index_from_template(&st, &sidx).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(
            segs[0].media_range,
            Some(ByteRange {
                start: 0,
                end: seg1_len as u64 - 1,
            })
        );
        assert_eq!(
            segs[1].media_range,
            Some(ByteRange {
                start: seg1_len as u64,
                end: seg1_len as u64 + seg2_len as u64 - 1,
            })
        );
    }

    #[test]
    fn segment_template_sidecar_index_requires_index_and_index_range() {
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
    fn segment_template_index_target_interpolates_number_and_time() {
        let st = SegmentTemplate {
            index: Some("idx-$Number$-$Time$.mp4".into()),
            indexRange: Some("0-10".into()),
            ..Default::default()
        };
        let vars = TemplateVars {
            representation_id: "1",
            number: Some(7),
            time: Some(42),
            ..Default::default()
        };
        let target = segment_template_index_target(&st, &vars).unwrap();
        assert_eq!(target.path, "idx-7-42.mp4");
        assert_eq!(target.range, Some(ByteRange { start: 0, end: 10 }));

        let base_vars = TemplateVars {
            representation_id: "1",
            ..Default::default()
        };
        assert!(matches!(
            segment_template_index_target(&st, &base_vars),
            Err(PlayerError::MissingSegmentTemplateIndexVars)
        ));
    }

    #[test]
    fn media_range_from_per_segment_index_spans_all_sidx_references() {
        let seg1_len = 11u32;
        let seg2_len = 13u32;
        let sidx = minimal_sidx_bytes(&[(seg1_len, 2000), (seg2_len, 2000)], 1000);
        let st = SegmentTemplate {
            timescale: Some(1000),
            index: Some("idx-$Number$.mp4".into()),
            indexRange: Some(format!("0-{}", sidx.len() - 1)),
            startNumber: Some(1),
            ..Default::default()
        };
        let media_range = media_range_from_per_segment_index(&st, &sidx).unwrap();
        assert_eq!(
            media_range,
            ByteRange {
                start: 0,
                end: seg1_len as u64 + seg2_len as u64 - 1,
            }
        );
    }

    #[test]
    fn timeline_segments_for_per_segment_index_uses_explicit_timeline() {
        let st = SegmentTemplate {
            timescale: Some(1000),
            duration: Some(4000.0),
            index: Some("idx-$Number$.mp4".into()),
            indexRange: Some("0-10".into()),
            media: Some("seg-$Number$.m4s".into()),
            startNumber: Some(1),
            ..Default::default()
        };
        let ctx = TimelineBuildContext {
            is_dynamic: false,
            period_window: PeriodWindow {
                idx: 0,
                start: Duration::ZERO,
                end: Some(Duration::from_secs(8)),
            },
            period_duration: None,
            media_presentation_duration: None,
            time_shift_buffer_depth: None,
            since_availability_start: None,
            resync_hints: None,
        };
        let segs =
            timeline_segments_for_addressing(&SegmentAddressing::Template(st), &ctx, None).unwrap();
        assert_eq!(segs.len(), 2);
        assert!(segs.iter().all(|s| s.media_range.is_none()));
    }
}
