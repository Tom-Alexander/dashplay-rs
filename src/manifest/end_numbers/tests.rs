use crate::manifest::{end_number_for_timeline, parse_segment_template_end_numbers};

#[test]
fn parse_segment_template_end_numbers_reads_adaptation_set_attribute() {
    let xml = include_str!("../../../tests/fixtures/dashif_simple/manifest.mpd");
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
