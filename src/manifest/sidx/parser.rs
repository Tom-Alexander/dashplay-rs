//! ISO BMFF `sidx` box parser (subset of dash-mpd's implementation).

use byteorder::{BigEndian, ReadBytesExt};
use std::io::{Cursor, Read};

use crate::manifest::ManifestError;

/// Parsed Segment Index (`sidx`) box.
#[derive(Debug, Clone, PartialEq)]
pub struct SidxBox {
    /// Full box size in bytes, including the size and type headers.
    pub box_size: usize,
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
    /// `0` = media, `1` = nested `sidx` (hierarchical / daisy-chain).
    pub reference_type: u8,
    pub referenced_size: u32,
    pub subsegment_duration: u32,
}

impl SidxBox {
    pub fn parse(data: &[u8]) -> Result<Self, ManifestError> {
        let mut reader = Cursor::new(data);
        let raw_size = reader
            .read_u32::<BigEndian>()
            .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
        let mut box_type = [0u8; 4];
        reader
            .read_exact(&mut box_type)
            .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
        if box_type != *b"sidx" {
            return Err(ManifestError::SidxParse("expected sidx box".into()));
        }

        let box_size = if raw_size == 0 {
            data.len()
        } else if raw_size == 1 {
            return Err(ManifestError::SidxParse(
                "sidx largesize (64-bit) is not supported".into(),
            ));
        } else {
            raw_size as usize
        };
        if box_size < 8 || box_size > data.len() {
            return Err(ManifestError::SidxParse(
                "sidx box size exceeds available bytes".into(),
            ));
        }

        let version = reader
            .read_u8()
            .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
        let _flags = reader
            .read_u24::<BigEndian>()
            .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
        let reference_id = reader
            .read_u32::<BigEndian>()
            .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
        let timescale = reader
            .read_u32::<BigEndian>()
            .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
        let earliest_presentation_time = if version == 0 {
            u64::from(
                reader
                    .read_u32::<BigEndian>()
                    .map_err(|e| ManifestError::SidxParse(e.to_string()))?,
            )
        } else {
            reader
                .read_u64::<BigEndian>()
                .map_err(|e| ManifestError::SidxParse(e.to_string()))?
        };
        let first_offset = if version == 0 {
            u64::from(
                reader
                    .read_u32::<BigEndian>()
                    .map_err(|e| ManifestError::SidxParse(e.to_string()))?,
            )
        } else {
            reader
                .read_u64::<BigEndian>()
                .map_err(|e| ManifestError::SidxParse(e.to_string()))?
        };
        let _reserved = reader
            .read_u16::<BigEndian>()
            .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
        let reference_count = reader
            .read_u16::<BigEndian>()
            .map_err(|e| ManifestError::SidxParse(e.to_string()))?;

        let mut references = Vec::with_capacity(reference_count as usize);
        for _ in 0..reference_count {
            let chunk = reader
                .read_u32::<BigEndian>()
                .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
            let reference_type = ((chunk & 0x8000_0000) >> 31) as u8;
            let referenced_size = chunk & 0x7FFF_FFFF;
            let subsegment_duration = reader
                .read_u32::<BigEndian>()
                .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
            let _sap_fields = reader
                .read_u32::<BigEndian>()
                .map_err(|e| ManifestError::SidxParse(e.to_string()))?;
            references.push(SidxReference {
                reference_type,
                referenced_size,
                subsegment_duration,
            });
        }

        if reader.position() as usize > box_size {
            return Err(ManifestError::SidxParse(
                "sidx references exceed declared box size".into(),
            ));
        }

        Ok(Self {
            box_size,
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
        assert!(matches!(err, ManifestError::SidxParse(_)));
    }
}
