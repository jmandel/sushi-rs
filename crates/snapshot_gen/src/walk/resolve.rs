//! SD resolution: fetch base/type/profile SDs from the PackageContext, convert
//! R4→R5 at load, and recursively walk-generate a snapshot when one is missing
//! (memoized per url). Everything downstream of here is R5-internal.
//!
//! CONVERSION PATHS (mirrors SnapOracleR4 exactly):
//! 1. The INPUT profile (runOne, SnapOracleR4:165-167): R4 parse + full
//!    VersionConvertorFactory_40_50 conversion — `to_r5_internal` (convert.rs).
//! 2. local-dir resources (loadLocalR4CanonicalResources): R4 parse + full
//!    conversion — same as 1.
//! 3. PACKAGE resources (SimpleWorkerContextBuilder.fromPackage →
//!    loadFromPackage(pi, null): loader == null): the R4 JSON is read by the
//!    R5 JsonParser DIRECTLY. R4-only properties the R5 model does not know are
//!    silently dropped (verified empirically: the oracle's loaded r4.core base
//!    has NO constraint.xpath in any form, while the input conversion stores it
//!    as the /4.0/ cross-version extension) — `lenient_r5_read_r4`.

use serde_json::Value;
use std::rc::Rc;

use super::context::WalkContext;
use crate::{convert_r4_sd_to_r5, PackageContext};

/// Detect R4 by fhirVersion 4.x; if so convert to R5-internal model (full
/// VersionConvertor path — for the input profile and local-dir resources).
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

/// Emulate the R5 JsonParser reading an R4 StructureDefinition (package-load
/// path, loader == null): keep the JSON as-is, dropping R4-only properties the
/// R5 model has no slot for. For snapshot generation the load-bearing drop is
/// `ElementDefinition.constraint.xpath`; `StructureDefinition.contextType` and
/// R4 string-form `context` are also R4-only (not element-relevant, dropped for
/// completeness).
pub(crate) fn lenient_r5_read_r4(sd: &Value) -> Value {
    let is_r4 = sd
        .get("fhirVersion")
        .and_then(Value::as_str)
        .map(|v| v.starts_with('4'))
        .unwrap_or(false);
    if !is_r4 {
        return sd.clone();
    }
    let mut out = sd.clone();
    if let Some(obj) = out.as_object_mut() {
        obj.remove("contextType");
        if let Some(ctx_arr) = obj.get("context").and_then(Value::as_array) {
            if ctx_arr.iter().any(Value::is_string) {
                obj.remove("context");
            }
        }
    }
    for section in ["snapshot", "differential"] {
        if let Some(elements) = out
            .get_mut(section)
            .and_then(|s| s.get_mut("element"))
            .and_then(Value::as_array_mut)
        {
            for ed in elements {
                if let Some(constraints) = ed.get_mut("constraint").and_then(Value::as_array_mut) {
                    for c in constraints {
                        if let Some(cobj) = c.as_object_mut() {
                            cobj.remove("xpath");
                        }
                    }
                }
            }
        }
    }
    out
}

/// Fetch a StructureDefinition by url/id/name in its R5-internal load form:
/// local-dir resources get the full conversion; package resources get the
/// lenient R5 read (see module doc).
pub(crate) fn fetch_sd(pkg: &PackageContext, query: &str) -> Option<Value> {
    let raw = pkg.fetch(query)?;
    let url = raw.get("url").and_then(Value::as_str).unwrap_or(query);
    if pkg.is_local(url) || pkg.is_local(query) {
        to_r5_internal(&raw).ok()
    } else {
        Some(lenient_r5_read_r4(&raw))
    }
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
    let sd = if ctx.pkg.is_local(&url) || ctx.pkg.is_local(query) {
        to_r5_internal(&raw)?
    } else {
        lenient_r5_read_r4(&raw)
    };
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
