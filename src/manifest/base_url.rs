use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

use dash_mpd::{AdaptationSet, BaseURL, Representation};
use url::Url;

use crate::manifest::ManifestError;

/// Default `@dvb:priority` when absent (ETSI TS 103 285 §10.8.2.1).
pub(crate) const DEFAULT_DVB_PRIORITY: u64 = 1;
/// Default `@dvb:weight` when absent (ETSI TS 103 285 §10.8.2.1).
pub(crate) const DEFAULT_DVB_WEIGHT: u64 = 1;

/// Hierarchical inputs for resolving segment URLs (ISO/IEC 23009-1 §5.6).
#[derive(Debug, Clone)]
pub(crate) struct SegmentBaseContext {
    pub manifest_uri: Url,
    pub mpd_base_urls: Vec<BaseURL>,
    pub period_base_urls: Vec<BaseURL>,
    pub service_location_priority: Vec<String>,
    pub default_service_location: Option<String>,
    /// Sticky entropy for DVB weighted BaseURL picks within a period/session.
    pub dvb_selection_seed: u64,
}

/// Build a selection seed for sticky DVB weighted picks (load-balances across sessions).
pub(crate) fn new_dvb_selection_seed(manifest_uri: &Url) -> u64 {
    let mut hasher = DefaultHasher::new();
    manifest_uri.as_str().hash(&mut hasher);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos.hash(&mut hasher);
    hasher.finish()
}

fn is_absolute_base(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("file://")
        || t.starts_with("ftp://")
}

/// Merge a document base with a `BaseURL@` value (RFC 3986); preserves manifest query when absent on the child (dash-mpd semantics).
pub(crate) fn merge_base_url(current: &Url, new: &str) -> Result<Url, ManifestError> {
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

fn dvb_priority_of(bu: &BaseURL) -> u64 {
    bu.priority.unwrap_or(DEFAULT_DVB_PRIORITY)
}

fn dvb_weight_of(bu: &BaseURL) -> u64 {
    match bu.weight {
        Some(w) if w > 0 => w as u64,
        _ => DEFAULT_DVB_WEIGHT,
    }
}

fn layer_has_dvb_attrs(layer: &[BaseURL]) -> bool {
    layer
        .iter()
        .any(|bu| bu.priority.is_some() || bu.weight.is_some())
}

/// RFC 2782-style weighted pick: `pick` is an opaque integer (e.g. seed).
fn select_weighted_index(weights: &[u64], pick: u64) -> usize {
    let total: u64 = weights.iter().sum();
    if weights.is_empty() {
        return 0;
    }
    if total == 0 {
        return 0;
    }
    let mut rn = pick % total;
    for (idx, &w) in weights.iter().enumerate() {
        if rn < w {
            return idx;
        }
        rn -= w;
    }
    weights.len() - 1
}

fn mix_seed(seed: u64, priority: u64, layer_tag: u64) -> u64 {
    seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(priority)
        .wrapping_add(layer_tag.wrapping_mul(0x100_0000_01B3))
}

/// Order alternatives at one MPD hierarchy level.
///
/// When DVB `@priority` / `@weight` are present: one weighted pick per priority group,
/// groups ascending by priority (TS 103 285 §10.8.2.2–§10.8.2.3 failover chain).
/// Otherwise: document order (all alternatives kept for sequential failover).
fn ordered_base_url_layer(layer: &[BaseURL], seed: u64, layer_tag: u64) -> Vec<&BaseURL> {
    let usable: Vec<&BaseURL> = layer
        .iter()
        .filter(|bu| !bu.base.trim().is_empty())
        .collect();
    if usable.is_empty() || !layer_has_dvb_attrs(layer) {
        return usable;
    }

    let mut by_priority: BTreeMap<u64, Vec<&BaseURL>> = BTreeMap::new();
    for bu in usable {
        by_priority.entry(dvb_priority_of(bu)).or_default().push(bu);
    }

    let mut ordered = Vec::with_capacity(by_priority.len());
    for (priority, group) in by_priority {
        let weights: Vec<u64> = group.iter().map(|bu| dvb_weight_of(bu)).collect();
        let idx = select_weighted_index(&weights, mix_seed(seed, priority, layer_tag));
        ordered.push(group[idx]);
    }
    ordered
}

/// Expand one hierarchical level: each incoming base × each ordered alternative at this level.
fn expand_base_layer(
    bases: Vec<Url>,
    layer: &[BaseURL],
    seed: u64,
    layer_tag: u64,
) -> Result<Vec<Url>, ManifestError> {
    if layer.is_empty() {
        return Ok(bases);
    }
    let ordered = ordered_base_url_layer(layer, seed, layer_tag);
    if ordered.is_empty() {
        return Ok(bases);
    }
    let mut next = Vec::with_capacity(bases.len().saturating_mul(ordered.len()));
    for b in bases {
        for bu in &ordered {
            next.push(merge_base_url(&b, bu.base.trim())?);
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
///
/// When a level carries `@dvb:priority` / `@dvb:weight` (or un-namespaced aliases), that level
/// contributes one weighted pick per priority for the primary choice, with lower-priority picks
/// available for failover (ETSI TS 103 285 §10.8.2). Levels without those attributes keep every
/// alternative in document order.
pub(crate) fn segment_bases_for_representation(
    ctx: &SegmentBaseContext,
    adaptation_set: &AdaptationSet,
    representation: &Representation,
) -> Result<Vec<Url>, ManifestError> {
    let mut bases = vec![ctx.manifest_uri.clone()];
    let mpd_base_urls = crate::manifest_lifecycle::order_base_urls_for_steering(
        &ctx.mpd_base_urls,
        &ctx.service_location_priority,
        ctx.default_service_location.as_deref(),
    );
    let seed = ctx.dvb_selection_seed;
    bases = expand_base_layer(bases, &mpd_base_urls, seed, 1)?;
    bases = expand_base_layer(bases, &ctx.period_base_urls, seed, 2)?;
    bases = expand_base_layer(bases, &adaptation_set.BaseURL, seed, 3)?;
    bases = expand_base_layer(bases, &representation.BaseURL, seed, 4)?;
    Ok(dedupe_urls(bases))
}

#[cfg(test)]
mod tests;
