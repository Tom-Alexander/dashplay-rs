//! Progressive CMAF chunk extraction for Low-Latency DASH partial segment transfer.

use bytes::{Bytes, BytesMut};

use crate::PlayerError;
use crate::http::{HttpRequest, SharedHttpClient};
use crate::manifest::SegmentFetchTarget;
use crate::segment_blacklist::SegmentBlacklist;

use super::{box_type_at, read_box_size};
use url::Url;

/// Accumulates streaming bytes and yields complete top-level `moof`/`mdat` CMAF fragment pairs.
#[derive(Debug, Default)]
pub(crate) struct CmafChunkAccumulator {
    buffer: BytesMut,
}

impl CmafChunkAccumulator {
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// Drain every complete CMAF fragment currently buffered.
    pub(crate) fn drain_fragments(&mut self) -> Vec<Bytes> {
        let mut out = Vec::new();
        while let Some(fragment) = self.try_take_fragment() {
            out.push(fragment);
        }
        out
    }

    /// Remaining bytes when the HTTP body ended (single-box tail or full segment).
    pub(crate) fn finish(mut self) -> Option<Bytes> {
        if self.buffer.is_empty() {
            return None;
        }
        if self.try_take_fragment().is_some() {
            // Should not happen if stream ended cleanly on box boundaries; return rest anyway.
            return Some(self.buffer.freeze());
        }
        Some(self.buffer.freeze())
    }

    fn try_take_fragment(&mut self) -> Option<Bytes> {
        let data = self.buffer.as_ref();
        if data.len() < 8 {
            return None;
        }

        let mut offset = 0usize;
        // Skip auxiliary boxes until the first moof of a fragment.
        while offset + 8 <= data.len() {
            let (box_size, header_len) = read_box_size(data, offset)?;
            if box_size < header_len || offset + box_size > data.len() {
                return None;
            }
            let ty = box_type_at(data, offset, header_len)?;
            if &ty == b"moof" {
                break;
            }
            offset += box_size;
        }

        if offset + 8 > data.len() {
            return None;
        }

        let (moof_size, moof_header) = read_box_size(data, offset)?;
        if moof_size < moof_header || offset + moof_size > data.len() {
            return None;
        }
        let moof_end = offset + moof_size;
        if moof_end + 8 > data.len() {
            return None;
        }

        let (mdat_size, mdat_header) = read_box_size(data, moof_end)?;
        if mdat_size < mdat_header {
            return None;
        }
        let ty = box_type_at(data, moof_end, mdat_header)?;
        if &ty != b"mdat" {
            // moof-only chunk; emit once the moof box is complete.
            let fragment = self.buffer.split_to(moof_end).freeze();
            return Some(fragment);
        }
        if moof_end + mdat_size > data.len() {
            return None;
        }

        let end = moof_end + mdat_size;
        Some(self.buffer.split_to(end).freeze())
    }
}

/// Split a complete segment body into moof/mdat fragments (for non-streaming clients).
#[cfg(test)]
pub(crate) fn fragments_from_complete_body(body: &[u8]) -> Vec<Bytes> {
    let mut acc = CmafChunkAccumulator::default();
    acc.push(body);
    let mut frags = acc.drain_fragments();
    if frags.is_empty() && !body.is_empty() {
        frags.push(Bytes::copy_from_slice(body));
    } else if let Some(tail) = acc.finish() {
        frags.push(tail);
    }
    frags
}

pub(crate) async fn fetch_cmaf_fragments_for_target(
    client: &SharedHttpClient,
    bases: &[Url],
    target: &SegmentFetchTarget,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<Bytes>, PlayerError> {
    fetch_cmaf_fragments_with_failover(client, bases, &target.path, blacklist).await
}

async fn fetch_cmaf_fragments_with_failover(
    client: &SharedHttpClient,
    bases: &[Url],
    relative_path: &str,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<Bytes>, PlayerError> {
    let mut last_err: Option<PlayerError> = None;
    for base in bases {
        let url = if relative_path.is_empty() {
            base.clone()
        } else {
            base.join(relative_path)?
        };
        match fetch_cmaf_fragments(client, url.clone(), blacklist).await {
            Ok(frags) => return Ok(frags),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or(PlayerError::SegmentExhaustedRepresentations))
}

async fn fetch_cmaf_fragments(
    client: &SharedHttpClient,
    url: Url,
    blacklist: &SegmentBlacklist,
) -> Result<Vec<Bytes>, PlayerError> {
    if blacklist.contains_url(&url) {
        return Err(PlayerError::SegmentBlacklisted(url.to_string()));
    }

    let (status, mut body) = client
        .open_body_stream(HttpRequest::get(url.clone()))
        .await?;
    if !(200..300).contains(&status) {
        blacklist.insert_url(&url);
        return Err(PlayerError::SegmentRequestFailed {
            status,
            url: url.to_string(),
        });
    }

    let mut acc = CmafChunkAccumulator::default();
    let mut fragments = Vec::new();
    while let Some(chunk) = body.next_chunk().await? {
        acc.push(&chunk);
        fragments.extend(acc.drain_fragments());
    }
    if let Some(tail) = acc.finish() {
        fragments.push(tail);
    }
    Ok(fragments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_bytes(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut v = Vec::with_capacity(size as usize);
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn accumulator_yields_moof_mdat_pair() {
        let moof = box_bytes(b"moof", b"abc");
        let mdat = box_bytes(b"mdat", b"xyz");
        let mut body = moof.clone();
        body.extend_from_slice(&mdat);

        let mut acc = CmafChunkAccumulator::default();
        acc.push(&body);
        let frags = acc.drain_fragments();
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].as_ref(), body.as_slice());
    }

    #[test]
    fn accumulator_emits_fragments_as_bytes_arrive() {
        let moof = box_bytes(b"moof", b"a");
        let mdat = box_bytes(b"mdat", b"b");
        let mut acc = CmafChunkAccumulator::default();
        acc.push(&moof[..4]);
        assert!(acc.drain_fragments().is_empty());
        acc.push(&moof[4..]);
        acc.push(&mdat);
        let frags = acc.drain_fragments();
        assert_eq!(frags.len(), 1);
    }

    #[test]
    fn complete_body_splitter_matches_accumulator() {
        let moof = box_bytes(b"moof", b"1");
        let mdat = box_bytes(b"mdat", b"2");
        let moof2 = box_bytes(b"moof", b"3");
        let mdat2 = box_bytes(b"mdat", b"4");
        let mut body = moof;
        body.extend_from_slice(&mdat);
        body.extend_from_slice(&moof2);
        body.extend_from_slice(&mdat2);
        let frags = fragments_from_complete_body(&body);
        assert_eq!(frags.len(), 2);
    }
}
