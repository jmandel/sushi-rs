//! SnapshotGenerationPreProcessor equivalent (§3.0). The live corpus path is
//! `processSlices` slice-group trailing-property push-down. Implemented lazily:
//! for the fixture ladder no push-down fires (verified via trace parity — the
//! preprocess.* records are absent). The additional-base path (§3.0b) is DEAD
//! under the oracle config. This is a documented gap; extend when a corpus
//! profile requires slice-group property push-down.

use serde_json::Value;
use std::collections::HashMap;

/// Runs the preprocessor on the diff clone in place. Currently a no-op pending
/// slice-group push-down (see module doc).
pub(crate) fn process(_diff: &mut [Value]) {}

/// Sort the differential to Java `sortDifferential` order for the walk: each row
/// sorts by the base-snapshot index of the longest ancestor prefix present in the
/// base, stable within a group (preserving differential order — so slices stay
/// adjacent to their anchor and unfolded-type children stay under their parent).
/// This reproduces the oracle's post-sort processing order verified against the
/// Java decision trace (IPS Patient-uv-ips).
pub(crate) fn sort_differential(diff: &mut [Value], base_elements: &[Value]) {
    let mut base_order: HashMap<String, usize> = HashMap::new();
    for (i, e) in base_elements.iter().enumerate() {
        if let Some(p) = e.get("path").and_then(Value::as_str) {
            base_order.entry(p.to_string()).or_insert(i);
        }
    }
    let ancestor_order = |path: &str| -> usize {
        let mut p = path;
        loop {
            if let Some(&o) = base_order.get(p) {
                return o;
            }
            match p.rfind('.') {
                Some(i) => p = &p[..i],
                None => return usize::MAX / 2,
            }
        }
    };
    // Stable sort by ancestor order.
    let mut indexed: Vec<(usize, Value)> = diff.iter().cloned().enumerate().collect();
    indexed.sort_by(|a, b| {
        let oa = ancestor_order(a.1.get("path").and_then(Value::as_str).unwrap_or(""));
        let ob = ancestor_order(b.1.get("path").and_then(Value::as_str).unwrap_or(""));
        oa.cmp(&ob).then(a.0.cmp(&b.0))
    });
    for (slot, (_, v)) in diff.iter_mut().zip(indexed.into_iter()) {
        *slot = v;
    }
}
