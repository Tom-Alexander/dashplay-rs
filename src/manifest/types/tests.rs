use crate::manifest::parse_byte_range;

#[test]
fn parse_byte_range_accepts_inclusive_specifier() {
    let br = parse_byte_range("7-62").unwrap();
    assert_eq!(br.start, 7);
    assert_eq!(br.end, 62);
    assert!(parse_byte_range("bad").is_err());
    assert!(parse_byte_range("10-5").is_err());
}
