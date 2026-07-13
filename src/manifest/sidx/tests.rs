use dash_mpd::{SegmentBase, SegmentTemplate};

use super::super::types::ByteRange;
use super::{
    parse_sidx_index, parse_sidx_index_from_representation_index_base,
    parse_sidx_index_from_template, parse_sidx_index_from_template_representation_index,
};
use crate::manifest::ManifestError;

#[test]
fn parse_sidx_index_builds_timeline_with_byte_ranges() {
    let seg1_len = 11u32;
    let seg2_len = 11u32;
    let init_len = 7usize;
    let sidx = minimal_sidx_bytes(
        &[(false, seg1_len, 2000), (false, seg2_len, 2000)],
        1000,
        0,
        0,
    );
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
    let sidx = minimal_sidx_bytes(
        &[(false, seg1_len, 2000), (false, seg2_len, 2000)],
        1000,
        0,
        0,
    );
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
fn parse_sidx_index_from_template_representation_index_uses_first_offset() {
    let seg1_len = 11u32;
    let seg2_len = 11u32;
    let sidx = minimal_sidx_bytes(
        &[(false, seg1_len, 2000), (false, seg2_len, 2000)],
        1000,
        0,
        0,
    );
    let st = SegmentTemplate {
        timescale: Some(1000),
        representation_index: Some(dash_mpd::RepresentationIndex {
            sourceURL: Some("index.mp4".into()),
            range: Some(format!("0-{}", sidx.len() - 1)),
        }),
        startNumber: Some(1),
        ..Default::default()
    };
    let segs = parse_sidx_index_from_template_representation_index(
        &st,
        st.representation_index.as_ref().unwrap(),
        &sidx,
    )
    .unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(
        segs[0].media_range,
        Some(ByteRange {
            start: 0,
            end: seg1_len as u64 - 1,
        })
    );
}

#[test]
fn parse_sidx_index_from_representation_index_base_uses_first_offset() {
    let seg1_len = 11u32;
    let seg2_len = 11u32;
    let mut sidx = minimal_sidx_bytes(
        &[(false, seg1_len, 2000), (false, seg2_len, 2000)],
        1000,
        0,
        0,
    );
    // first_offset field starts at byte 24 in the full sidx box.
    sidx[24..28].copy_from_slice(&7u32.to_be_bytes());
    let sb = SegmentBase {
        timescale: Some(1000),
        representation_index: Some(dash_mpd::RepresentationIndex {
            sourceURL: Some("index.mp4".into()),
            range: Some(format!("0-{}", sidx.len() - 1)),
        }),
        ..Default::default()
    };
    let segs = parse_sidx_index_from_representation_index_base(
        &sb,
        sb.representation_index.as_ref().unwrap(),
        &sidx,
    )
    .unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(
        segs[0].media_range,
        Some(ByteRange {
            start: 7,
            end: 7 + seg1_len as u64 - 1,
        })
    );
}

#[test]
fn hierarchical_sidx_flattens_nested_index_references() {
    let seg1_len = 100u32;
    let seg2_len = 200u32;
    let child1 = minimal_sidx_bytes(&[(false, seg1_len, 2000)], 1000, 0, 0);
    let child2 = minimal_sidx_bytes(&[(false, seg2_len, 2000)], 1000, 2000, seg1_len as u64);
    let root = minimal_sidx_bytes(
        &[
            (true, child1.len() as u32, 2000),
            (true, child2.len() as u32, 2000),
        ],
        1000,
        0,
        0,
    );
    let mut index = root;
    index.extend_from_slice(&child1);
    index.extend_from_slice(&child2);

    let st = SegmentTemplate {
        timescale: Some(1000),
        index: Some("index.mp4".into()),
        indexRange: Some(format!("0-{}", index.len() - 1)),
        startNumber: Some(1),
        ..Default::default()
    };
    let segs = parse_sidx_index_from_template(&st, &index).unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].number, 1);
    assert_eq!(
        segs[0].media_range,
        Some(ByteRange {
            start: 0,
            end: seg1_len as u64 - 1,
        })
    );
    assert_eq!(segs[1].number, 2);
    assert_eq!(
        segs[1].media_range,
        Some(ByteRange {
            start: seg1_len as u64,
            end: seg1_len as u64 + seg2_len as u64 - 1,
        })
    );
    assert!((segs[1].presentation_time_s - 2.0).abs() < 1e-9);
}

#[test]
fn daisy_chain_sidx_flattens_trailing_index_reference() {
    let seg1_len = 100u32;
    let seg2_len = 50u32;
    let next = minimal_sidx_bytes(&[(false, seg2_len, 1000)], 1000, 0, seg1_len as u64);
    let first = minimal_sidx_bytes(
        &[(false, seg1_len, 1000), (true, next.len() as u32, 1000)],
        1000,
        0,
        0,
    );
    let mut index = first;
    index.extend_from_slice(&next);

    let st = SegmentTemplate {
        timescale: Some(1000),
        index: Some("index.mp4".into()),
        indexRange: Some(format!("0-{}", index.len() - 1)),
        startNumber: Some(5),
        ..Default::default()
    };
    let segs = parse_sidx_index_from_template(&st, &index).unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].number, 5);
    assert_eq!(
        segs[0].media_range,
        Some(ByteRange {
            start: 0,
            end: seg1_len as u64 - 1,
        })
    );
    assert_eq!(segs[1].number, 6);
    assert_eq!(
        segs[1].media_range,
        Some(ByteRange {
            start: seg1_len as u64,
            end: seg1_len as u64 + seg2_len as u64 - 1,
        })
    );
}

#[test]
fn hierarchical_sidx_outside_fetched_index_errors() {
    // Type-1 reference claims a nested sidx that is not present in the fetched bytes.
    let sidx = minimal_sidx_bytes(&[(true, 64, 2000)], 1000, 0, 0);
    let st = SegmentTemplate {
        timescale: Some(1000),
        index: Some("index.mp4".into()),
        indexRange: Some(format!("0-{}", sidx.len() - 1)),
        startNumber: Some(1),
        ..Default::default()
    };
    let err = parse_sidx_index_from_template(&st, &sidx).unwrap_err();
    assert!(matches!(err, ManifestError::HierarchicalSidxNotSupported));
}

/// Build a version-0 `sidx` box.
///
/// `refs` entries are `(reference_type_is_index, referenced_size, subsegment_duration)`.
fn minimal_sidx_bytes(
    refs: &[(bool, u32, u32)],
    timescale: u32,
    earliest_presentation_time: u64,
    first_offset: u64,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(0); // version
    body.extend_from_slice(&[0, 0, 0]); // flags
    body.extend_from_slice(&1u32.to_be_bytes()); // reference_id
    body.extend_from_slice(&timescale.to_be_bytes());
    body.extend_from_slice(&(earliest_presentation_time as u32).to_be_bytes());
    body.extend_from_slice(&(first_offset as u32).to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes()); // reserved
    body.extend_from_slice(&(refs.len() as u16).to_be_bytes());
    for &(is_index, size, dur) in refs {
        let mut chunk = size & 0x7FFF_FFFF;
        if is_index {
            chunk |= 0x8000_0000;
        }
        body.extend_from_slice(&chunk.to_be_bytes());
        body.extend_from_slice(&dur.to_be_bytes());
        body.extend_from_slice(&0x9000_0000u32.to_be_bytes());
    }
    let mut out = (8 + body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(b"sidx");
    out.extend_from_slice(&body);
    out
}
