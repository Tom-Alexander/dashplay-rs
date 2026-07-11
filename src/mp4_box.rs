//! ISO BMFF box size helpers shared by in-band event parsing and partial CMAF delivery.

/// Read a box size and header length at `offset` within `data`.
pub(crate) fn read_box_size(data: &[u8], offset: usize) -> Option<(usize, usize)> {
    if offset + 8 > data.len() {
        return None;
    }
    let size32 = u32::from_be_bytes(data[offset..offset + 4].try_into().ok()?);
    if size32 == 1 {
        if offset + 16 > data.len() {
            return None;
        }
        let size64 = u64::from_be_bytes(data[offset + 8..offset + 16].try_into().ok()?);
        let size = usize::try_from(size64).ok()?;
        Some((size, 16))
    } else {
        let size = if size32 == 0 {
            data.len().checked_sub(offset)?
        } else {
            usize::try_from(size32).ok()?
        };
        Some((size, 8))
    }
}

/// Four-character box type at `offset` (after the size field).
pub(crate) fn box_type_at(data: &[u8], offset: usize, header_len: usize) -> Option<[u8; 4]> {
    let start = offset + header_len - 4;
    data.get(start..start + 4).and_then(|s| s.try_into().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_box_size_parses_32_bit_length() {
        let mut data = vec![0u8; 20];
        data[0..4].copy_from_slice(&8u32.to_be_bytes());
        data[4..8].copy_from_slice(b"moof");
        assert_eq!(read_box_size(&data, 0), Some((8, 8)));
    }
}
