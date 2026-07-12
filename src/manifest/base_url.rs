use std::collections::HashSet;

use dash_mpd::{AdaptationSet, BaseURL, Representation};
use url::Url;

use crate::PlayerError;

/// Hierarchical inputs for resolving segment URLs (ISO/IEC 23009-1 §5.6).
#[derive(Debug, Clone)]
pub(crate) struct SegmentBaseContext {
    pub manifest_uri: Url,
    pub mpd_base_urls: Vec<BaseURL>,
    pub period_base_urls: Vec<BaseURL>,
    pub service_location_priority: Vec<String>,
    pub default_service_location: Option<String>,
}

fn is_absolute_base(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("file://")
        || t.starts_with("ftp://")
}

/// Merge a document base with a `BaseURL@` value (RFC 3986); preserves manifest query when absent on the child (dash-mpd semantics).
pub(crate) fn merge_base_url(current: &Url, new: &str) -> Result<Url, PlayerError> {
    let new = new.trim();
    if new.is_empty() {
        return Ok(current.clone());
    }
    if is_absolute_base(new) {
        return Ok(Url::parse(new)?);
    }
    let mut merged = current.join(new)?;
    if merged.query().is_none() {
        merged.set_query(current.query());
    }
    Ok(merged)
}

fn sorted_base_url_layer(layer: &[BaseURL]) -> Vec<&BaseURL> {
    let mut v: Vec<_> = layer.iter().collect();
    v.sort_by_key(|bu| bu.priority.unwrap_or(u64::MAX));
    v
}

/// Expand one hierarchical level: each incoming base × each alternative `BaseURL` at this level.
fn expand_base_layer(bases: Vec<Url>, layer: &[BaseURL]) -> Result<Vec<Url>, PlayerError> {
    if layer.is_empty() {
        return Ok(bases);
    }
    let sorted = sorted_base_url_layer(layer);
    let alts: Vec<&str> = sorted
        .iter()
        .map(|bu| bu.base.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if alts.is_empty() {
        return Ok(bases);
    }
    let mut next = Vec::with_capacity(bases.len().saturating_mul(alts.len()));
    for b in bases {
        for s in &alts {
            next.push(merge_base_url(&b, s)?);
        }
    }
    Ok(next)
}

fn dedupe_urls(mut bases: Vec<Url>) -> Vec<Url> {
    let mut seen = HashSet::new();
    bases.retain(|u| seen.insert(u.as_str().to_string()));
    bases
}

/// Absolute segment bases for `(AdaptationSet, Representation)` after MPD → Period → AdaptationSet → Representation `BaseURL` expansion.
pub(crate) fn segment_bases_for_representation(
    ctx: &SegmentBaseContext,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<Vec<Url>, PlayerError> {
    let mut bases = vec![ctx.manifest_uri.clone()];
    let mpd_base_urls = crate::manifest_lifecycle::order_base_urls_for_steering(
        &ctx.mpd_base_urls,
        &ctx.service_location_priority,
        ctx.default_service_location.as_deref(),
    );
    bases = expand_base_layer(bases, &mpd_base_urls)?;
    bases = expand_base_layer(bases, &ctx.period_base_urls)?;
    bases = expand_base_layer(bases, &adaptation_set.BaseURL)?;
    bases = expand_base_layer(bases, &representation.BaseURL)?;
    Ok(dedupe_urls(bases))
}
