//! Extract Widevine PSSH and default KIDs from fragmented MP4 init/media bytes.

use pssh_box::{PsshBox, ToBytes, WIDEVINE_SYSTEM_ID, from_bytes as pssh_from_bytes};
use std::collections::HashSet;
use thiserror::Error;

/// DRM metadata discovered in-band from init or media segments.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InBandDrmInfo {
    pub widevine_pssh: Vec<PsshBox>,
    pub default_kids: Vec<[u8; 16]>,
    /// Widevine PSSH boxes carried in `emsg@message_data` (key-rotation events).
    pub emsg_widevine_pssh: Vec<PsshBox>,
}

impl InBandDrmInfo {
    pub fn all_widevine_pssh(&self) -> impl Iterator<Item = &PsshBox> {
        self.widevine_pssh
            .iter()
            .chain(self.emsg_widevine_pssh.iter())
    }

    pub fn has_widevine_pssh(&self) -> bool {
        !self.widevine_pssh.is_empty() || !self.emsg_widevine_pssh.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum Mp4DrmError {
    #[error("truncated MP4 box at offset {0}")]
    TruncatedBox(usize),
    #[error("invalid box size at offset {0}")]
    InvalidBoxSize(usize),
    #[error("pssh parse error: {0}")]
    Pssh(String),
}

/// Parse init and optional media bytes for in-band Widevine PSSH and `tenc` default KIDs.
pub fn extract_in_band_drm(
    init: &[u8],
    media: Option<&[u8]>,
) -> Result<InBandDrmInfo, Mp4DrmError> {
    let mut info = InBandDrmInfo::default();
    if !init.is_empty() {
        scan_fragment(init, &mut info)?;
    }
    if let Some(seg) = media {
        if !seg.is_empty() {
            scan_fragment(seg, &mut info)?;
        }
    }
    dedupe_in_band(&mut info);
    Ok(info)
}

fn dedupe_in_band(info: &mut InBandDrmInfo) {
    let mut seen_pssh: HashSet<Vec<u8>> = HashSet::new();
    info.widevine_pssh
        .retain(|p| seen_pssh.insert(p.to_bytes()));
    info.emsg_widevine_pssh
        .retain(|p| seen_pssh.insert(p.to_bytes()));

    let mut seen_kid: HashSet<[u8; 16]> = HashSet::new();
    info.default_kids.retain(|kid| seen_kid.insert(*kid));
}

fn scan_fragment(data: &[u8], out: &mut InBandDrmInfo) -> Result<(), Mp4DrmError> {
    scan_typed_boxes(data, b"pssh", |full_box| collect_pssh_box(full_box, out))?;
    scan_typed_boxes(data, b"tenc", |full_box| {
        let header_len = box_header_len(full_box)?;
        if full_box.len() > header_len {
            if let Some(kid) = parse_tenc_kid(&full_box[header_len..]) {
                out.default_kids.push(kid);
            }
        }
        Ok(())
    })?;
    scan_typed_boxes(data, b"emsg", |full_box| {
        let header_len = box_header_len(full_box)?;
        if full_box.len() > header_len {
            collect_emsg_pssh(&full_box[header_len..], out)?;
        }
        Ok(())
    })?;
    Ok(())
}

fn scan_typed_boxes(
    data: &[u8],
    fourcc: &[u8; 4],
    mut handle: impl FnMut(&[u8]) -> Result<(), Mp4DrmError>,
) -> Result<(), Mp4DrmError> {
    for i in 4..data.len().saturating_sub(4) {
        if &data[i..i + 4] != fourcc {
            continue;
        }
        let box_start = i - 4;
        let Ok((box_size, _)) = read_box_size(data, box_start, data.len()) else {
            continue;
        };
        let Some(box_end) = box_start.checked_add(box_size) else {
            continue;
        };
        if box_end > data.len() || &data[box_start + 4..box_start + 8] != fourcc {
            continue;
        }
        handle(&data[box_start..box_end])?;
    }
    Ok(())
}

fn box_header_len(box_bytes: &[u8]) -> Result<usize, Mp4DrmError> {
    if box_bytes.len() < 8 {
        return Err(Mp4DrmError::TruncatedBox(0));
    }
    let size32 = u32::from_be_bytes(box_bytes[0..4].try_into().expect("4 bytes")) as u64;
    if size32 == 1 { Ok(16) } else { Ok(8) }
}

fn read_box_size(data: &[u8], offset: usize, end: usize) -> Result<(usize, usize), Mp4DrmError> {
    if offset + 8 > end {
        return Err(Mp4DrmError::TruncatedBox(offset));
    }
    let size32 = u32::from_be_bytes(data[offset..offset + 4].try_into().expect("4 bytes")) as u64;
    if size32 == 1 {
        if offset + 16 > end {
            return Err(Mp4DrmError::TruncatedBox(offset));
        }
        let size64 = u64::from_be_bytes(data[offset + 8..offset + 16].try_into().expect("8 bytes"));
        let size = usize::try_from(size64).map_err(|_| Mp4DrmError::InvalidBoxSize(offset))?;
        Ok((size, 16))
    } else {
        let size = if size32 == 0 {
            end - offset
        } else {
            usize::try_from(size32).map_err(|_| Mp4DrmError::InvalidBoxSize(offset))?
        };
        Ok((size, 8))
    }
}

fn collect_pssh_box(box_bytes: &[u8], out: &mut InBandDrmInfo) -> Result<(), Mp4DrmError> {
    let boxes = pssh_from_bytes(box_bytes).map_err(|e| Mp4DrmError::Pssh(e.to_string()))?;
    for pssh in boxes {
        if pssh.system_id == WIDEVINE_SYSTEM_ID {
            out.widevine_pssh.push(pssh);
        }
    }
    Ok(())
}

fn parse_tenc_kid(payload: &[u8]) -> Option<[u8; 16]> {
    // FullBox header (version + flags) precedes the `tenc` payload.
    if payload.len() < 4 + 16 {
        return None;
    }
    let mut kid = [0u8; 16];
    kid.copy_from_slice(&payload[payload.len() - 16..]);
    Some(kid)
}

fn collect_emsg_pssh(payload: &[u8], out: &mut InBandDrmInfo) -> Result<(), Mp4DrmError> {
    if payload.len() < 4 {
        return Ok(());
    }
    let version = payload[0];
    let message_data = match version {
        0 => parse_emsg_v0_message_data(payload)?,
        1 => parse_emsg_v1_message_data(payload)?,
        _ => return Ok(()),
    };
    if message_data.is_empty() {
        return Ok(());
    }

    if let Ok(boxes) = pssh_from_bytes(message_data) {
        for pssh in boxes {
            if pssh.system_id == WIDEVINE_SYSTEM_ID {
                out.emsg_widevine_pssh.push(pssh);
            }
        }
    }
    Ok(())
}

fn parse_emsg_v0_message_data(payload: &[u8]) -> Result<&[u8], Mp4DrmError> {
    let mut offset = 4usize;
    offset = skip_null_string(payload, offset)?;
    offset = skip_null_string(payload, offset)?;
    offset = offset.checked_add(16).ok_or(Mp4DrmError::TruncatedBox(0))?;
    if offset > payload.len() {
        return Err(Mp4DrmError::TruncatedBox(0));
    }
    Ok(&payload[offset..])
}

fn parse_emsg_v1_message_data(payload: &[u8]) -> Result<&[u8], Mp4DrmError> {
    let mut offset = 4usize;
    offset = offset.checked_add(16).ok_or(Mp4DrmError::TruncatedBox(0))?;
    offset = skip_null_string(payload, offset)?;
    offset = skip_null_string(payload, offset)?;
    if offset > payload.len() {
        return Err(Mp4DrmError::TruncatedBox(0));
    }
    Ok(&payload[offset..])
}

fn skip_null_string(data: &[u8], mut offset: usize) -> Result<usize, Mp4DrmError> {
    while offset < data.len() {
        if data[offset] == 0 {
            return Ok(offset + 1);
        }
        offset += 1;
    }
    Err(Mp4DrmError::TruncatedBox(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_bytes(fixture: &str, file: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture)
            .join(file);
        std::fs::read(path).expect("read fixture")
    }

    #[test]
    fn parse_dashif_init_extracts_tenc_default_kid() {
        let init = fixture_bytes("dashif_drm_encrypted", "init.mp4");
        let info = extract_in_band_drm(&init, None).expect("parse init");
        assert!(info.widevine_pssh.is_empty());
        assert_eq!(info.default_kids.len(), 1);
        assert_eq!(
            hex::encode(info.default_kids[0]),
            "eb6769950da145d03ae4082255eb141a"
        );
    }

    #[test]
    fn parse_emsg_message_data_for_widevine_pssh() {
        let pssh_b64 = "AAAANHBzc2gAAAAA7e+LqXnWSs6jyCfc1R0h7QAAABsIARIQ62dplQ2hRdA65AgiVesUGg==";
        let pssh_bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, pssh_b64)
                .expect("decode pssh");

        let mut payload = Vec::new();
        payload.extend_from_slice(&[0u8, 0, 0, 0]); // version + flags
        payload.extend_from_slice(b"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\0");
        payload.extend_from_slice(b"1\0");
        payload.extend_from_slice(&1u32.to_be_bytes()); // timescale
        payload.extend_from_slice(&0u32.to_be_bytes()); // presentation_time_delta
        payload.extend_from_slice(&0u32.to_be_bytes()); // event_duration
        payload.extend_from_slice(&1u32.to_be_bytes()); // id
        payload.extend_from_slice(&pssh_bytes);

        let mut emsg = Vec::new();
        let box_size = (8 + payload.len()) as u32;
        emsg.extend_from_slice(&box_size.to_be_bytes());
        emsg.extend_from_slice(b"emsg");
        emsg.extend_from_slice(&payload);

        let info = extract_in_band_drm(&[], Some(&emsg)).expect("parse emsg");
        assert_eq!(info.all_widevine_pssh().count(), 1);
    }
}
