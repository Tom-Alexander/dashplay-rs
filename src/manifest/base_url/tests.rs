use dash_mpd::{AdaptationSet, BaseURL, Representation};
use url::Url;

use crate::manifest::{SegmentBaseContext, merge_base_url, segment_bases_for_representation};

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

    let bases = segment_bases_for_representation(&ctx, &adaptation_set, &representation).unwrap();
    assert_eq!(bases.len(), 1);
    assert!(bases[0].as_str().contains("/rep-a"));
    assert!(bases[0].as_str().contains("/as/"));
    assert_eq!(bases[0].query(), Some("sig=1"));
}
