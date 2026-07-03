//! SD resolution: fetch base/type/profile SDs from the PackageContext, convert
//! R4→R5 at load, and recursively walk-generate a snapshot when one is missing
//! (memoized per url). Everything downstream of here is R5-internal.

use serde_json::Value;
use std::rc::Rc;

use super::context::WalkContext;
use crate::{convert_r4_sd_to_r5, PackageContext};

/// Detect R4 by fhirVersion 4.x; if so convert to R5-internal model.
pub(crate) fn to_r5_internal(sd: &Value) -> anyhow::Result<Value> {
    let is_r4 = sd
        .get("fhirVersion")
        .and_then(Value::as_str)
        .map(|v| v.starts_with('4'))
        .unwrap_or(false);
    if is_r4 {
        convert_r4_sd_to_r5(sd)
    } else {
        Ok(sd.clone())
    }
}

/// Fetch a StructureDefinition by url/id/name, converting to R5-internal.
pub(crate) fn fetch_sd(pkg: &PackageContext, query: &str) -> Option<Value> {
    let raw = pkg.fetch(query)?;
    to_r5_internal(&raw).ok()
}

/// Resolve `query` to an R5-internal SD that HAS a snapshot, generating it
/// recursively (and memoizing) when the stored resource only ships a
/// differential. Returns `Rc<Value>` shared from the cache.
pub(crate) fn resolve_with_snapshot(
    ctx: &mut WalkContext,
    query: &str,
) -> anyhow::Result<Option<Rc<Value>>> {
    // Resolve to the canonical url first so cache keys are stable.
    let Some(raw) = ctx.pkg.fetch(query) else {
        return Ok(None);
    };
    let url = raw
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(query)
        .to_string();
    if let Some(hit) = ctx.gen_cache.get(&url) {
        return Ok(Some(hit.clone()));
    }
    let sd = to_r5_internal(&raw)?;
    if sd
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .is_some()
    {
        let rc = Rc::new(sd);
        ctx.gen_cache.insert(url, rc.clone());
        return Ok(Some(rc));
    }
    // No snapshot: recursively walk-generate one.
    let generated = super::generate_snapshot_inner(ctx, sd)?;
    let rc = Rc::new(generated);
    ctx.gen_cache.insert(url, rc.clone());
    Ok(Some(rc))
}
