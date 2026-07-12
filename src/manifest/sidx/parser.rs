//! ISO BMFF `sidx` box parser (subset of dash-mpd's implementation).

use byteorder::{BigEndian, ReadBytesExt};
use std::io::{Cursor, Read};

use crate::PlayerError;

/// Parsed Segment Index (`sidx`) box.
#[derive(Debug, Clone, PartialEq)]
pub struct SidxBox {
    pub version: u8,
    pub reference_id: u32,
    pub timescale: u32,
    pub earliest_presentation_time: u64,
    pub first_offset: u64,
    pub references: Vec<SidxReference>,
}

/// One reference entry inside a `sidx` box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidxReference {
    pub reference_type: u8,
    pub referenced_size: u32,
    pub subsegment_duration: u32,
}

impl SidxBox {
    pub fn parse(data: &[u8]) -> Result<Self, PlayerError> {
        let mut reader = Cursor::new(data);
        let _box_size = reader
            .read_u32::<BigEndian>()
            .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
        let mut box_type = [0u8; 4];
        reader
            .read_exact(&mut box_type)
            .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
        if box_type != *b"sidx" {
            return Err(PlayerError::SidxParse("expected sidx box".into()));
        }

        let version = reader
            .read_u8()
            .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
        let _flags = reader
            .read_u24::<BigEndian>()
            .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
        let reference_id = reader
            .read_u32::<BigEndian>()
            .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
        let timescale = reader
            .read_u32::<BigEndian>()
            .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
        let earliest_presentation_time = if version == 0 {
            u64::from(
                reader
                    .read_u32::<BigEndian>()
                    .map_err(|e| PlayerError::SidxParse(e.to_string()))?,
            )
        } else {
            reader
                .read_u64::<BigEndian>()
                .map_err(|e| PlayerError::SidxParse(e.to_string()))?
        };
        let first_offset = if version == 0 {
            u64::from(
                reader
                    .read_u32::<BigEndian>()
                    .map_err(|e| PlayerError::SidxParse(e.to_string()))?,
            )
        } else {
            reader
                .read_u64::<BigEndian>()
                .map_err(|e| PlayerError::SidxParse(e.to_string()))?
        };
        let _reserved = reader
            .read_u16::<BigEndian>()
            .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
        let reference_count = reader
            .read_u16::<BigEndian>()
            .map_err(|e| PlayerError::SidxParse(e.to_string()))?;

        let mut references = Vec::with_capacity(reference_count as usize);
        for _ in 0..reference_count {
            let chunk = reader
                .read_u32::<BigEndian>()
                .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
            let reference_type = ((chunk & 0x8000_0000) >> 31) as u8;
            let referenced_size = chunk & 0x7FFF_FFFF;
            let subsegment_duration = reader
                .read_u32::<BigEndian>()
                .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
            let _sap_fields = reader
                .read_u32::<BigEndian>()
                .map_err(|e| PlayerError::SidxParse(e.to_string()))?;
            references.push(SidxReference {
                reference_type,
                referenced_size,
                subsegment_duration,
            });
        }

        Ok(Self {
            version,
            reference_id,
            timescale,
            earliest_presentation_time,
            first_offset,
            references,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_sidx_header() {
        let err = SidxBox::parse(b"not-a-sidx-box").expect_err("invalid");
        assert!(matches!(err, PlayerError::SidxParse(_)));
    }
}
