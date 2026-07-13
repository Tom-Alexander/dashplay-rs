use dash_mpd::{SegmentBase, SegmentTemplate};

use crate::manifest::{
    ByteRange, ManifestError, TemplateVars, media_range_from_per_segment_index,
    representation_index_fetch_target, segment_base_index_target, segment_base_init_target,
    segment_template_index_target,
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
    let target = segment_base_init_target(&sb, &vars).unwrap();
    assert_eq!(target.path, "");
    assert_eq!(target.range, Some(ByteRange { start: 0, end: 6 }));
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
