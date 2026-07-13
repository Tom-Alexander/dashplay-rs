use dash_mpd::{SegmentBase, SegmentTemplate};

use crate::manifest::{
    ByteRange, parse_sidx_index, parse_sidx_index_from_representation_index_base,
    parse_sidx_index_from_template, parse_sidx_index_from_template_representation_index,
};

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
fn parse_sidx_index_from_template_representation_index_uses_first_offset() {
    let seg1_len = 11u32;
    let seg2_len = 11u32;
    let sidx = minimal_sidx_bytes(&[(seg1_len, 2000), (seg2_len, 2000)], 1000);
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
    let mut sidx = minimal_sidx_bytes(&[(seg1_len, 2000), (seg2_len, 2000)], 1000);
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
