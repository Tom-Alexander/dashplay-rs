use dash_mpd::{SegmentBase, SegmentList, SegmentTemplate, SegmentURL};

use super::super::error::ManifestError;
use super::super::template::TemplateVars;
use super::super::types::{ByteRange, TimelineSegment};
use super::{
    media_range_from_per_segment_index, representation_index_fetch_target,
    segment_base_index_target, segment_base_init_target, segment_list_init_target,
    segment_list_media_target, segment_template_index_target,
};

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
    let target = segment_base_init_target(&sb, &vars).unwrap().unwrap();
    assert_eq!(target.path, "");
    assert_eq!(target.range, Some(ByteRange { start: 0, end: 6 }));
}

#[test]
fn segment_base_init_target_absent_is_none() {
    let sb = SegmentBase {
        presentationDuration: Some(8000),
        timescale: Some(1000),
        ..Default::default()
    };
    let vars = TemplateVars {
        representation_id: "1",
        ..Default::default()
    };
    assert!(segment_base_init_target(&sb, &vars).unwrap().is_none());
}

#[test]
fn segment_base_media_target_whole_file_uses_base_url() {
    use super::segment_base_media_target;

    let sb = SegmentBase {
        presentationDuration: Some(8000),
        timescale: Some(1000),
        ..Default::default()
    };
    let seg = TimelineSegment {
        number: 1,
        time: 0,
        duration: 8000,
        duration_s: 8.0,
        presentation_time_s: 0.0,
        sub_number: None,
        resync_start_chunk: None,
        media_url: None,
        media_range: None,
    };
    let vars = TemplateVars {
        representation_id: "1",
        ..Default::default()
    };
    let target = segment_base_media_target(&sb, &seg, &vars).unwrap();
    assert_eq!(target.path, "");
    assert_eq!(target.range, None);
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
        Err(ManifestError::MissingSegmentTemplateIndexVars)
    ));
}

#[test]
fn representation_index_fetch_target_interpolates_source_url() {
    let ri = dash_mpd::RepresentationIndex {
        sourceURL: Some("idx-$Number$.mp4".into()),
        range: Some("0-10".into()),
    };
    let vars = TemplateVars {
        representation_id: "1",
        number: Some(3),
        ..Default::default()
    };
    let target = representation_index_fetch_target(&ri, &vars).unwrap();
    assert_eq!(target.path, "idx-3.mp4");
    assert_eq!(target.range, Some(ByteRange { start: 0, end: 10 }));
}

#[test]
fn segment_base_index_target_uses_representation_index_source_url() {
    let sb = SegmentBase {
        representation_index: Some(dash_mpd::RepresentationIndex {
            sourceURL: Some("sidecar-index.mp4".into()),
            range: Some("4-20".into()),
        }),
        ..Default::default()
    };
    let vars = TemplateVars {
        representation_id: "1",
        ..Default::default()
    };
    let target = segment_base_index_target(&sb, &vars).unwrap();
    assert_eq!(target.path, "sidecar-index.mp4");
    assert_eq!(target.range, Some(ByteRange { start: 4, end: 20 }));
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
fn segment_list_init_target_uses_range_on_base_url() {
    let sl = SegmentList {
        Initialization: Some(dash_mpd::Initialization {
            range: Some("0-16".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let vars = TemplateVars {
        representation_id: "1",
        ..Default::default()
    };
    let target = segment_list_init_target(&sl, &vars).unwrap().unwrap();
    assert_eq!(target.path, "");
    assert_eq!(target.range, Some(ByteRange { start: 0, end: 16 }));
}

#[test]
fn segment_list_media_target_byte_range_only() {
    let sl = SegmentList {
        segment_urls: vec![
            SegmentURL {
                mediaRange: Some("17-31".into()),
                ..Default::default()
            },
            SegmentURL {
                media: Some("bundle.mp4".into()),
                mediaRange: Some("32-46".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let seg0 = TimelineSegment {
        number: 1,
        time: 0,
        duration: 4000,
        duration_s: 4.0,
        presentation_time_s: 0.0,
        sub_number: None,
        resync_start_chunk: None,
        media_url: None,
        media_range: Some(ByteRange { start: 17, end: 31 }),
    };
    let target = segment_list_media_target(&sl, &seg0, 0).unwrap();
    assert_eq!(target.path, "");
    assert_eq!(target.range, Some(ByteRange { start: 17, end: 31 }));

    let seg1 = TimelineSegment {
        number: 2,
        media_url: Some("bundle.mp4".into()),
        media_range: Some(ByteRange { start: 32, end: 46 }),
        ..seg0
    };
    let target = segment_list_media_target(&sl, &seg1, 1).unwrap();
    assert_eq!(target.path, "bundle.mp4");
    assert_eq!(target.range, Some(ByteRange { start: 32, end: 46 }));
}

#[test]
fn segment_list_media_target_rejects_empty_without_range() {
    let sl = SegmentList {
        segment_urls: vec![SegmentURL::default()],
        ..Default::default()
    };
    let seg = TimelineSegment {
        number: 1,
        time: 0,
        duration: 4000,
        duration_s: 4.0,
        presentation_time_s: 0.0,
        sub_number: None,
        resync_start_chunk: None,
        media_url: None,
        media_range: None,
    };
    assert!(matches!(
        segment_list_media_target(&sl, &seg, 0),
        Err(ManifestError::MissingMediaTemplate)
    ));
}
