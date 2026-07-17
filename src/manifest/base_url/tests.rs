use dash_mpd::{AdaptationSet, BaseURL, Representation};
use url::Url;

use super::{
    SegmentBaseContext, base_url_availability_for_representation, merge_base_url,
    segment_bases_for_representation,
};

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
        dvb_selection_seed: 0,
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

    let bases = segment_bases_for_representation(&ctx, &adaptation_set, &representation).unwrap();
    assert_eq!(bases.len(), 1);
    assert!(bases[0].as_str().contains("/rep-a"));
    assert!(bases[0].as_str().contains("/as/"));
    assert_eq!(bases[0].query(), Some("sig=1"));
}

#[test]
fn without_dvb_attrs_all_bases_remain_for_failover() {
    let ctx = SegmentBaseContext {
        manifest_uri: Url::parse("https://example.com/manifest.mpd").unwrap(),
        mpd_base_urls: vec![
            BaseURL {
                base: "https://cdn-a.example/".into(),
                ..Default::default()
            },
            BaseURL {
                base: "https://cdn-b.example/".into(),
                ..Default::default()
            },
        ],
        period_base_urls: Vec::new(),
        service_location_priority: Vec::new(),
        default_service_location: None,
        dvb_selection_seed: 0,
    };
    let adaptation_set = AdaptationSet::default();
    let representation = Representation::default();

    let bases = segment_bases_for_representation(&ctx, &adaptation_set, &representation).unwrap();
    assert_eq!(bases.len(), 2);
    assert_eq!(bases[0].as_str(), "https://cdn-a.example/");
    assert_eq!(bases[1].as_str(), "https://cdn-b.example/");
}

#[test]
fn dvb_priority_orders_failover_across_priority_groups() {
    let ctx = SegmentBaseContext {
        manifest_uri: Url::parse("https://example.com/manifest.mpd").unwrap(),
        mpd_base_urls: Vec::new(),
        period_base_urls: vec![
            BaseURL {
                base: "bad/".into(),
                priority: Some(1),
                ..Default::default()
            },
            BaseURL {
                base: "good/".into(),
                priority: Some(2),
                ..Default::default()
            },
        ],
        service_location_priority: Vec::new(),
        default_service_location: None,
        dvb_selection_seed: 0,
    };
    let adaptation_set = AdaptationSet::default();
    let representation = Representation::default();

    let bases = segment_bases_for_representation(&ctx, &adaptation_set, &representation).unwrap();
    assert_eq!(bases.len(), 2);
    assert!(bases[0].as_str().ends_with("/bad/"));
    assert!(bases[1].as_str().ends_with("/good/"));
}

#[test]
fn dvb_same_priority_picks_one_by_weight() {
    let ctx = SegmentBaseContext {
        manifest_uri: Url::parse("https://example.com/manifest.mpd").unwrap(),
        mpd_base_urls: vec![
            BaseURL {
                base: "https://cdn-light.example/".into(),
                priority: Some(1),
                weight: Some(1),
                ..Default::default()
            },
            BaseURL {
                base: "https://cdn-heavy.example/".into(),
                priority: Some(1),
                weight: Some(99),
                ..Default::default()
            },
            BaseURL {
                base: "https://cdn-fallback.example/".into(),
                priority: Some(3),
                weight: Some(1),
                ..Default::default()
            },
        ],
        period_base_urls: Vec::new(),
        service_location_priority: Vec::new(),
        default_service_location: None,
        // mix_seed(1, priority=1, layer=1) lands in the heavy weight band.
        dvb_selection_seed: 1,
    };
    let adaptation_set = AdaptationSet::default();
    let representation = Representation::default();

    let bases = segment_bases_for_representation(&ctx, &adaptation_set, &representation).unwrap();
    assert_eq!(bases.len(), 2, "one pick per priority group");
    assert_eq!(bases[0].as_str(), "https://cdn-heavy.example/");
    assert_eq!(bases[1].as_str(), "https://cdn-fallback.example/");
}

#[test]
fn dvb_attrs_deserialize_from_mpd() {
    let mpd = dash_mpd::parse(
        r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     xmlns:dvb="urn:dvb:dash:dash-extensions:2014-1"
     type="static" mediaPresentationDuration="PT4S" minBufferTime="PT1S"
     profiles="urn:dvb:dash:profile:dvb-dash:2014">
  <Period>
    <BaseURL dvb:priority="1" dvb:weight="70" serviceLocation="A">https://cdn1.example/</BaseURL>
    <BaseURL dvb:priority="1" dvb:weight="30" serviceLocation="B">https://cdn2.example/</BaseURL>
    <BaseURL dvb:priority="5" dvb:weight="1" serviceLocation="C">https://cdn3.example/</BaseURL>
    <AdaptationSet mimeType="video/mp4">
      <Representation id="1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
</MPD>"#,
    )
    .expect("parse");

    let period = &mpd.periods[0];
    assert_eq!(period.BaseURL.len(), 3);
    assert_eq!(period.BaseURL[0].priority, Some(1));
    assert_eq!(period.BaseURL[0].weight, Some(70));
    assert_eq!(period.BaseURL[1].weight, Some(30));
    assert_eq!(period.BaseURL[2].priority, Some(5));

    let ctx = SegmentBaseContext {
        manifest_uri: Url::parse("https://example.com/manifest.mpd").unwrap(),
        mpd_base_urls: mpd.base_url.clone(),
        period_base_urls: period.BaseURL.clone(),
        service_location_priority: Vec::new(),
        default_service_location: None,
        dvb_selection_seed: 0,
    };
    let bases = segment_bases_for_representation(
        &ctx,
        &period.adaptations[0],
        &period.adaptations[0].representations[0],
    )
    .unwrap();
    assert_eq!(bases.len(), 2, "priority 1 pick + priority 5 failover");
    assert!(
        bases[0].as_str() == "https://cdn1.example/"
            || bases[0].as_str() == "https://cdn2.example/"
    );
    assert_eq!(bases[1].as_str(), "https://cdn3.example/");
}

#[test]
fn base_url_availability_sums_offsets_across_hierarchy() {
    let ctx = SegmentBaseContext {
        manifest_uri: Url::parse("https://example.com/manifest.mpd").unwrap(),
        mpd_base_urls: vec![BaseURL {
            base: "mpd/".into(),
            availability_time_offset: Some(2.0),
            ..Default::default()
        }],
        period_base_urls: vec![BaseURL {
            base: "period/".into(),
            availability_time_offset: Some(5.0),
            ..Default::default()
        }],
        service_location_priority: Vec::new(),
        default_service_location: None,
        dvb_selection_seed: 0,
    };
    let adaptation_set = AdaptationSet::default();
    let representation = Representation::default();

    let availability =
        base_url_availability_for_representation(&ctx, &adaptation_set, &representation);
    assert_eq!(availability.availability_time_offset_s, Some(7.0));
    assert!(availability.availability_time_complete);
}

#[test]
fn base_url_availability_merges_complete_false_from_any_level() {
    let ctx = SegmentBaseContext {
        manifest_uri: Url::parse("https://example.com/manifest.mpd").unwrap(),
        mpd_base_urls: vec![BaseURL {
            base: "mpd/".into(),
            availability_time_complete: Some(true),
            ..Default::default()
        }],
        period_base_urls: vec![BaseURL {
            base: "period/".into(),
            availability_time_complete: Some(false),
            ..Default::default()
        }],
        service_location_priority: Vec::new(),
        default_service_location: None,
        dvb_selection_seed: 0,
    };
    let availability = base_url_availability_for_representation(
        &ctx,
        &AdaptationSet::default(),
        &Representation::default(),
    );
    assert!(!availability.availability_time_complete);
}
