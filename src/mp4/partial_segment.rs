//! Progressive CMAF chunk extraction for Low-Latency DASH partial segment transfer.

use bytes::{Bytes, BytesMut};

use crate::cmcd::{CmsdSnapshot, parse_cmsd_headers};
use crate::http::{HttpRequest, SharedHttpClient};
use crate::manifest::SegmentFetchTarget;
use crate::segment::SegmentError;
use crate::segment_blacklist::SegmentBlacklist;
use crate::segment_fetcher::CmcdFetch;

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
    cmcd: Option<CmcdFetch<'_>>,
) -> Result<(Vec<Bytes>, Option<CmsdSnapshot>), SegmentError> {
    fetch_cmaf_fragments_with_failover(client, bases, &target.path, blacklist, cmcd).await
}

async fn fetch_cmaf_fragments_with_failover(
    client: &SharedHttpClient,
    bases: &[Url],
    relative_path: &str,
    blacklist: &SegmentBlacklist,
    cmcd: Option<CmcdFetch<'_>>,
) -> Result<(Vec<Bytes>, Option<CmsdSnapshot>), SegmentError> {
    let mut last_err: Option<SegmentError> = None;
    for base in bases {
        let url = if relative_path.is_empty() {
            base.clone()
        } else {
            base.join(relative_path)?
        };
        match fetch_cmaf_fragments(client, url.clone(), blacklist, cmcd.as_ref()).await {
            Ok(frags) => return Ok(frags),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or(SegmentError::ExhaustedRepresentations))
}

async fn fetch_cmaf_fragments(
    client: &SharedHttpClient,
    url: Url,
    blacklist: &SegmentBlacklist,
    cmcd: Option<&CmcdFetch<'_>>,
) -> Result<(Vec<Bytes>, Option<CmsdSnapshot>), SegmentError> {
    if blacklist.contains_url(&url) {
        return Err(SegmentError::Blacklisted(url.to_string()));
    }

    let mut req = HttpRequest::get(url.clone());
    if let Some(cmcd) = cmcd {
        req = cmcd.session.apply(req, &cmcd.context);
    }

    let mut stream_resp = client.open_body_stream(req).await?;
    let status = stream_resp.status();
    if !(200..300).contains(&status) {
        blacklist.insert_url(&url);
        return Err(SegmentError::RequestFailed {
            status,
            url: url.to_string(),
        });
    }

    let cmsd = parse_cmsd_headers(
        stream_resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str())),
    );
    if let (Some(cmcd), Some(snapshot)) = (cmcd, cmsd.as_ref()) {
        cmcd.session.record_cmsd(snapshot.clone());
    }

    let body = stream_resp.body_mut();
    let mut acc = CmafChunkAccumulator::default();
    let mut fragments = Vec::new();
    while let Some(chunk) = body.next_chunk().await? {
        acc.push(&chunk);
        fragments.extend(acc.drain_fragments());
    }
    if let Some(tail) = acc.finish() {
        fragments.push(tail);
    }
    Ok((fragments, cmsd))
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
    fn fragments_from_complete_body_returns_whole_when_no_boxes() {
        let body = b"not-boxes";
        let frags = fragments_from_complete_body(body);
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].as_ref(), body.as_slice());
    }
}
