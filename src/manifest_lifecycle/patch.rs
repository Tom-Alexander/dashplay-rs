//! Apply ISO/IEC 23009-1 MPD patch documents (RFC 5261 subset used by DASH).

use roxmltree::{Document, Node};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum MpdPatchError {
    #[error("invalid patch selector: {0}")]
    InvalidSelector(String),
    #[error("patch selector matched no node: {0}")]
    NodeNotFound(String),
    #[error("patch validation failed: {0}")]
    Validation(String),
    #[error("malformed patch document: {0}")]
    Malformed(String),
    #[error("patch application failed: {0}")]
    Application(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchOp {
    kind: PatchOpKind,
    selector: String,
    pos: Option<String>,
    content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchOpKind {
    Add,
    Remove,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchMeta {
    mpd_id: Option<String>,
    publish_time: String,
    original_publish_time: String,
}

/// Apply a patch document to in-memory MPD XML when validation succeeds.
pub(crate) fn apply_mpd_patch(mpd_xml: &str, patch_xml: &str) -> Result<String, MpdPatchError> {
    let meta = parse_patch_meta(patch_xml)?;
    validate_patch_against_mpd(mpd_xml, &meta)?;

    let mut ops = parse_patch_ops(patch_xml)?;
    if ops.is_empty() {
        return Ok(mpd_xml.to_string());
    }

    let mut xml = mpd_xml.to_string();
    for op in ops.drain(..) {
        let doc = Document::parse(&xml)
            .map_err(|e| MpdPatchError::Application(format!("MPD parse after patch step: {e}")))?;
        xml = apply_one_op(&doc, &xml, &op)?;
    }

    dash_mpd::parse(&xml)
        .map_err(|e| MpdPatchError::Application(format!("resulting MPD is invalid: {e}")))?;

    Ok(xml)
}

fn validate_patch_against_mpd(mpd_xml: &str, meta: &PatchMeta) -> Result<(), MpdPatchError> {
    let doc =
        Document::parse(mpd_xml).map_err(|e| MpdPatchError::Malformed(format!("MPD XML: {e}")))?;
    let mpd = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "MPD")
        .ok_or_else(|| MpdPatchError::Malformed("MPD root missing".into()))?;

    if let Some(ref patch_id) = meta.mpd_id {
        let mpd_id = mpd.attribute("id").unwrap_or("");
        if patch_id != mpd_id {
            return Err(MpdPatchError::Validation(format!(
                "Patch@mpdId {patch_id:?} does not match MPD@id {mpd_id:?}"
            )));
        }
    }

    let mpd_publish = mpd
        .attribute("publishTime")
        .ok_or_else(|| MpdPatchError::Validation("MPD@publishTime missing".into()))?;
    if meta.original_publish_time != mpd_publish {
        return Err(MpdPatchError::Validation(format!(
            "Patch@originalPublishTime {0:?} does not match MPD@publishTime {mpd_publish:?}",
            meta.original_publish_time
        )));
    }
    if meta.publish_time.as_str() <= mpd_publish {
        return Err(MpdPatchError::Validation(format!(
            "Patch@publishTime must be greater than MPD@publishTime ({mpd_publish:?})"
        )));
    }

    Ok(())
}

fn parse_patch_meta(patch_xml: &str) -> Result<PatchMeta, MpdPatchError> {
    let doc = Document::parse(patch_xml)
        .map_err(|e| MpdPatchError::Malformed(format!("patch XML: {e}")))?;
    let patch = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Patch")
        .ok_or_else(|| MpdPatchError::Malformed("Patch root missing".into()))?;

    let publish_time = patch
        .attribute("publishTime")
        .ok_or_else(|| MpdPatchError::Malformed("Patch@publishTime missing".into()))?
        .to_string();
    let original_publish_time = patch
        .attribute("originalPublishTime")
        .ok_or_else(|| MpdPatchError::Malformed("Patch@originalPublishTime missing".into()))?
        .to_string();

    Ok(PatchMeta {
        mpd_id: patch.attribute("mpdId").map(str::to_string),
        publish_time,
        original_publish_time,
    })
}

fn parse_patch_ops(patch_xml: &str) -> Result<Vec<PatchOp>, MpdPatchError> {
    let doc = Document::parse(patch_xml)
        .map_err(|e| MpdPatchError::Malformed(format!("patch XML: {e}")))?;
    let patch = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Patch")
        .ok_or_else(|| MpdPatchError::Malformed("Patch root missing".into()))?;

    let mut ops = Vec::new();
    for child in patch.children().filter(|n| n.is_element()) {
        let name = child.tag_name().name();
        let kind = match name {
            "add" => PatchOpKind::Add,
            "remove" => PatchOpKind::Remove,
            "replace" => PatchOpKind::Replace,
            _ => continue,
        };
        let selector = child
            .attribute("sel")
            .ok_or_else(|| MpdPatchError::Malformed(format!("{name}@sel missing")))?
            .to_string();
        let pos = child.attribute("pos").map(str::to_string);
        let content = operation_payload(child, patch_xml);
        ops.push(PatchOp {
            kind,
            selector,
            pos,
            content,
        });
    }
    Ok(ops)
}

fn operation_payload(node: Node<'_, '_>, source: &str) -> String {
    let element_children: Vec<_> = node.children().filter(|n| n.is_element()).collect();
    if element_children.is_empty() {
        return node.text().unwrap_or("").trim().to_string();
    }
    element_children
        .iter()
        .map(|child| source[child.range()].trim())
        .collect::<Vec<_>>()
        .join("")
}

fn apply_one_op(doc: &Document, xml: &str, op: &PatchOp) -> Result<String, MpdPatchError> {
    match op.kind {
        PatchOpKind::Replace => replace_node(doc, xml, &op.selector, &op.content),
        PatchOpKind::Remove => remove_node(doc, xml, &op.selector),
        PatchOpKind::Add => add_node(doc, xml, &op.selector, op.pos.as_deref(), &op.content),
    }
}

fn replace_node(
    doc: &Document,
    xml: &str,
    selector: &str,
    content: &str,
) -> Result<String, MpdPatchError> {
    if let Some(attr) = selector.strip_prefix('/').and_then(attribute_selector_name) {
        let (element_sel, attr_name) = attr;
        let node = resolve_element(doc, &format!("/{element_sel}"))?;
        let Some(attr_node) = node.attributes().find(|a| a.name() == attr_name) else {
            return Err(MpdPatchError::NodeNotFound(selector.to_string()));
        };
        let range = attr_node.range_value();
        let mut out = String::with_capacity(xml.len() - range.len() + content.len());
        out.push_str(&xml[..range.start]);
        out.push_str(content);
        out.push_str(&xml[range.end..]);
        return Ok(out);
    }

    let node = resolve_element(doc, selector)?;
    let range = node.range();
    let mut out = String::with_capacity(xml.len() - range.len() + content.len());
    out.push_str(&xml[..range.start]);
    out.push_str(content);
    out.push_str(&xml[range.end..]);
    Ok(out)
}

fn remove_node(doc: &Document, xml: &str, selector: &str) -> Result<String, MpdPatchError> {
    let node = resolve_element(doc, selector)?;
    let range = node.range();
    let mut out = String::with_capacity(xml.len() - range.len());
    out.push_str(&xml[..range.start]);
    out.push_str(&xml[range.end..]);
    Ok(out)
}

fn add_node(
    doc: &Document,
    xml: &str,
    selector: &str,
    pos: Option<&str>,
    content: &str,
) -> Result<String, MpdPatchError> {
    let node = resolve_element(doc, selector)?;
    let pos = pos.unwrap_or("append");
    let insert_at = match pos {
        "prepend" => find_element_open_end(xml, node.range().start)
            .ok_or_else(|| MpdPatchError::Application("prepend: open tag end".into()))?,
        "before" => node.range().start,
        "after" => node.range().end,
        "append" => node.range().end,
        other => {
            return Err(MpdPatchError::Application(format!(
                "unsupported add@pos {other:?}"
            )));
        }
    };

    let mut out = String::with_capacity(xml.len() + content.len());
    out.push_str(&xml[..insert_at]);
    out.push_str(content);
    out.push_str(&xml[insert_at..]);
    Ok(out)
}

fn find_element_open_end(xml: &str, start: usize) -> Option<usize> {
    let slice = xml.get(start..)?;
    let gt = slice.find('>')?;
    Some(start + gt + 1)
}

fn attribute_selector_name(selector: &str) -> Option<(&str, &str)> {
    let (element_sel, last_segment) = selector.rsplit_once('/')?;
    let attr = last_segment.strip_prefix('@')?;
    if attr.is_empty() {
        return None;
    }
    if element_sel.is_empty() {
        return None;
    }
    Some((element_sel, attr))
}

fn resolve_element<'a, 'input: 'a>(
    doc: &'a Document<'input>,
    selector: &str,
) -> Result<Node<'a, 'input>, MpdPatchError> {
    let path = selector.trim();
    if path.is_empty() || !path.starts_with('/') {
        return Err(MpdPatchError::InvalidSelector(selector.to_string()));
    }

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err(MpdPatchError::InvalidSelector(selector.to_string()));
    }

    let mut current = doc.root_element();
    let start_idx = if segments.first() == Some(&current.tag_name().name()) {
        1
    } else {
        0
    };

    for segment in segments.iter().skip(start_idx) {
        if segment.starts_with('@') {
            return Err(MpdPatchError::InvalidSelector(
                "attribute selector must use /Element/@attr form".into(),
            ));
        }

        let (name, predicates) = split_segment(segment)?;
        let matches: Vec<Node<'a, 'input>> = current
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == name)
            .filter(|n| element_matches_predicates(*n, &predicates))
            .collect();

        if let Some(idx) = predicates.index {
            let chosen = matches
                .get(idx - 1)
                .copied()
                .ok_or_else(|| MpdPatchError::NodeNotFound(selector.to_string()))?;
            current = chosen;
            continue;
        }

        if matches.is_empty() {
            return Err(MpdPatchError::NodeNotFound(selector.to_string()));
        }
        if matches.len() > 1 {
            return Err(MpdPatchError::InvalidSelector(format!(
                "selector {selector} matched multiple nodes"
            )));
        }
        current = matches[0];
    }

    Ok(current)
}

#[derive(Debug, Clone)]
struct SegmentPredicates {
    attrs: Vec<(String, String)>,
    index: Option<usize>,
}

fn split_segment(segment: &str) -> Result<(&str, SegmentPredicates), MpdPatchError> {
    let bracket = segment.find('[');
    let name = bracket.map_or(segment, |i| &segment[..i]);
    if name.is_empty() {
        return Err(MpdPatchError::InvalidSelector(segment.to_string()));
    }

    let mut attrs = Vec::new();
    let mut index = None;
    if let Some(start) = bracket {
        let end = segment
            .rfind(']')
            .ok_or_else(|| MpdPatchError::InvalidSelector(segment.to_string()))?;
        let inner = &segment[start + 1..end];
        if let Ok(idx) = inner.parse::<usize>() {
            index = Some(idx);
        } else if let Some((key, value)) = parse_attr_predicate(inner) {
            attrs.push((key, value));
        } else {
            return Err(MpdPatchError::InvalidSelector(segment.to_string()));
        }
    }

    Ok((
        name,
        SegmentPredicates {
            attrs,
            index: index.filter(|&i| i > 0),
        },
    ))
}

fn parse_attr_predicate(part: &str) -> Option<(String, String)> {
    let eq = part.find('=')?;
    let key = part[..eq].trim().trim_start_matches('@');
    let raw = part[eq + 1..].trim();
    let value = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))?;
    Some((key.to_string(), value.to_string()))
}

fn element_matches_predicates(node: Node<'_, '_>, pred: &SegmentPredicates) -> bool {
    pred.attrs
        .iter()
        .all(|(key, value)| node.attribute(key.as_str()) == Some(value.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_MPD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"
     id="live"
     type="dynamic"
     publishTime="2024-04-16T07:34:38Z"
     minimumUpdatePeriod="PT1S">
  <PatchLocation ttl="60">patch.mpp</PatchLocation>
  <Period id="P0">
    <AdaptationSet id="1" mimeType="video/mp4">
      <SegmentTemplate timescale="1000" media="seg-$Number$.m4s">
        <SegmentTimeline>
          <S d="4000" t="0"/>
          <S d="4000"/>
        </SegmentTimeline>
      </SegmentTemplate>
      <Representation id="1" bandwidth="100000"/>
    </AdaptationSet>
  </Period>
</MPD>
"#;

    #[test]
    fn apply_replace_publish_time_and_add_segment() {
        let patch = r#"<?xml version="1.0" encoding="UTF-8"?>
<Patch xmlns="urn:mpeg:dash:schema:mpd-patch:2020"
     mpdId="live"
     originalPublishTime="2024-04-16T07:34:38Z"
     publishTime="2024-04-16T07:34:42Z">
  <replace sel="/MPD/@publishTime">2024-04-16T07:34:42Z</replace>
  <add sel="/MPD/Period[@id='P0']/AdaptationSet[@id='1']/SegmentTemplate/SegmentTimeline/S[2]" pos="after">
    <S d="4000"/>
  </add>
</Patch>"#;

        let updated = apply_mpd_patch(BASE_MPD, patch).expect("patch");
        assert!(updated.contains(r#"publishTime="2024-04-16T07:34:42Z""#));
        assert_eq!(updated.matches("<S d=\"4000\"").count(), 3);

        let mpd = dash_mpd::parse(&updated).expect("parse patched MPD");
        assert!(mpd.publishTime.is_some());
        assert!(updated.contains("2024-04-16T07:34:42Z"));
    }

    #[test]
    fn reject_patch_when_original_publish_time_mismatch() {
        let patch = r#"<?xml version="1.0" encoding="UTF-8"?>
<Patch xmlns="urn:mpeg:dash:schema:mpd-patch:2020"
     originalPublishTime="2020-01-01T00:00:00Z"
     publishTime="2024-04-16T07:34:42Z">
  <replace sel="/MPD/@publishTime">2024-04-16T07:34:42Z</replace>
</Patch>"#;

        let err = apply_mpd_patch(BASE_MPD, patch).expect_err("mismatch");
        assert!(matches!(err, MpdPatchError::Validation(_)));
    }
}
