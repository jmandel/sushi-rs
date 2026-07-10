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

/// LAYER B / B1 (composition (a)): pin a base/dep SD's canonicals in place iff
/// `ctx.pin_base_versions` is set AND `url` is a PACKAGE-loaded (non-local)
/// resource. Java's `CoreVersionPinner` runs at load ONLY over the core/dependency
/// packages' structures — never the IG's own authored profiles
/// (CoreVersionPinner is invoked from `SimpleWorkerContext.fromPackage`,
/// SimpleWorkerContext.java:329; the IG's local profiles are loaded outside that
/// path). Locally-authored profiles therefore never get pinned, and their
/// differential-supplied canonicals (e.g. a re-stated `subject` targetProfile)
/// stay UNPINNED even though they resolve — the profile snapshot only carries
/// pins it INHERITED from the already-pinned core base. Pinning a local profile's
/// snapshot here would re-pin those differential-supplied canonicals and diverge
/// from Java (measured on period-tracking-fact `subject`/`device`). No-op when
/// the flag is off, so Layer A is untouched.
fn pin_base_if_enabled(ctx: &WalkContext, url: &str, sd: &mut Value) {
    if ctx.pin_base_versions && !ctx.pkg.is_local(url) {
        crate::layer_b::pin::pin_core_versions(sd, ctx.pkg);
    }
}

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

/// Port of `PackageHackerR5.fixLoadedResource` (BaseWorkerContext:417, called
/// from `registerResourceFromPackage` for every package-loaded resource). These
/// are load-time content fixups on package-loaded StructureDefinitions. The
/// url-keyed hacks are gated by exact url + `fhirVersion == 4.0.1`, so they only
/// ever fire on the named core resources (never on IG profiles). The
/// extensions.r4 R5-only-datatype removeIf is gated by the OWNING package id
/// (`packageInfo.getId()`), Java-exact — `package_id` is threaded from
/// PackageContext. Idempotent. The CodeSystem/ValueSet `.hack()` version fixes
/// and the r2b removeIf are not exercised by this corpus. See
/// PackageHackerR5.java:14-135.
pub(crate) fn fix_loaded_resource(sd: &mut Value, package_id: Option<&str>) {
    let url = sd
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let ver = sd.get("fhirVersion").and_then(Value::as_str).unwrap_or("");
    let rtype = sd
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let base_def = sd
        .get("baseDefinition")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let is_r4 = ver == "4.0.1";
    if !is_r4 {
        return;
    }

    // For each element in snapshot + differential, apply the per-element fixup.
    let mut apply = |f: &dyn Fn(&mut Value)| {
        for section in ["snapshot", "differential"] {
            if let Some(elements) = sd
                .get_mut(section)
                .and_then(|s| s.get_mut("element"))
                .and_then(Value::as_array_mut)
            {
                for ed in elements {
                    f(ed);
                }
            }
        }
    };

    // iso21090-nullFlavor (PackageHackerR5:45): strip the versioned v3-NullFlavor
    // valueSet suffix from bindings.
    if url == "http://hl7.org/fhir/StructureDefinition/iso21090-nullFlavor" {
        apply(&|ed| {
            if let Some(b) = ed.get_mut("binding") {
                if b.get("valueSet").and_then(Value::as_str)
                    == Some("http://terminology.hl7.org/ValueSet/v3-NullFlavor|4.0.1")
                {
                    if let Some(o) = b.as_object_mut() {
                        o.insert(
                            "valueSet".to_string(),
                            Value::String(
                                "http://terminology.hl7.org/ValueSet/v3-NullFlavor".to_string(),
                            ),
                        );
                    }
                }
            }
        });
    }

    // DeviceUseStatement (PackageHackerR5:58): rewrite the null.html bodySite link
    // in requirements.
    if url == "http://hl7.org/fhir/StructureDefinition/DeviceUseStatement" {
        apply(&|ed| {
            if let Some(r) = ed.get("requirements").and_then(Value::as_str) {
                let fixed = r.replace(
                    "[http://hl7.org/fhir/StructureDefinition/bodySite](null.html)",
                    "[http://hl7.org/fhir/StructureDefinition/bodySite](http://hl7.org/fhir/extension-bodysite.html)",
                );
                if fixed != r {
                    if let Some(o) = ed.as_object_mut() {
                        o.insert("requirements".to_string(), Value::String(fixed));
                    }
                }
            }
        });
    }

    // ServiceRequest (PackageHackerR5:74): trim the trailing LOINC-Order-codes
    // sentence from the code binding description.
    if url == "http://hl7.org/fhir/StructureDefinition/ServiceRequest" {
        apply(&|ed| {
            if let Some(b) = ed.get_mut("binding") {
                if b.get("description").and_then(Value::as_str) == Some(
                    "Codes for tests or services that can be carried out by a designated individual, organization or healthcare service.  For laboratory, LOINC is  (preferred)[http://build.fhir.org/terminologies.html#preferred] and a valueset using LOINC Order codes is available [here](valueset-diagnostic-requests.html)."
                ) {
                    if let Some(o) = b.as_object_mut() {
                        o.insert("description".to_string(), Value::String(
                            "Codes for tests or services that can be carried out by a designated individual, organization or healthcare service.  For laboratory, LOINC is  (preferred)[http://build.fhir.org/terminologies.html#preferred].".to_string()
                        ));
                    }
                }
            }
        });
    }

    // extensions.r4 R5-only datatype removal (PackageHackerR5:115): the R4 build
    // of the extensions pack carries R5-only datatypes in its element `type[]`
    // lists; the loader strips them. Java scopes this EXACTLY to
    // `packageInfo.getId().equals("hl7.fhir.uv.extensions.r4")` (the OWNING
    // package id of the loaded resource), not to fhirVersion — so an R4 SD from
    // any other package keeps these type codes even if it (invalidly) declares
    // them. We mirror that scoping via the owning package id plumbed from
    // PackageContext. Fixes AU Core `au-core-rsg-sexassignedab`
    // Extension.value[x] (54→49 types).
    if package_id == Some("hl7.fhir.uv.extensions.r4") {
        const R5_ONLY: [&str; 5] = [
            "integer64",
            "CodeableReference",
            "RatioRange",
            "Availability",
            "ExtendedContactDetail",
        ];
        apply(&|ed| {
            if let Some(types) = ed.get_mut("type").and_then(Value::as_array_mut) {
                types.retain(|t| {
                    t.get("code")
                        .and_then(Value::as_str)
                        .map(|c| !R5_ONLY.contains(&c))
                        .unwrap_or(true)
                });
            }
        });
    }

    // vitalsigns binding relaxation (PackageHackerR5:90): any Observation SD that
    // is `vitalsigns` or derives from it backs the ucum-vitals-common binding on
    // `Observation.component.value[x]` off to EXTENSIBLE.
    if url.starts_with("http://hl7.org/fhir/StructureDefinition/")
        && rtype == "Observation"
        && (url == "http://hl7.org/fhir/StructureDefinition/vitalsigns"
            || base_def == "http://hl7.org/fhir/StructureDefinition/vitalsigns")
    {
        apply(&|ed| {
            if ed.get("path").and_then(Value::as_str) == Some("Observation.component.value[x]") {
                if let Some(b) = ed.get_mut("binding") {
                    if b.get("valueSet").and_then(Value::as_str)
                        == Some("http://hl7.org/fhir/ValueSet/ucum-vitals-common|4.0.1")
                    {
                        if let Some(o) = b.as_object_mut() {
                            o.insert(
                                "strength".to_string(),
                                Value::String("extensible".to_string()),
                            );
                        }
                    }
                }
            }
        });
    }
}

/// Fetch a StructureDefinition by url/id/name in its R5-internal load form:
/// local-dir resources get the full conversion; package resources get the
/// lenient R5 read (see module doc).
pub(crate) fn fetch_sd(pkg: &PackageContext, query: &str) -> Option<Value> {
    let raw = pkg.fetch(query)?;
    let raw = raw.as_ref();
    let url = raw.get("url").and_then(Value::as_str).unwrap_or(query);
    let mut out = if pkg.is_local(url) || pkg.is_local(query) {
        to_r5_internal(raw).ok()?
    } else {
        lenient_r5_read_r4(raw)
    };
    let package_id = pkg
        .package_id_for(url)
        .or_else(|| pkg.package_id_for(query));
    fix_loaded_resource(&mut out, package_id.as_deref());
    Some(out)
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
    let raw = raw.as_ref();
    let url = raw
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or(query)
        .to_string();
    if let Some(hit) = ctx.gen_cache.get(&url) {
        return Ok(Some(hit.clone()));
    }
    let mut sd = if ctx.pkg.is_local(&url) || ctx.pkg.is_local(query) {
        to_r5_internal(raw)?
    } else {
        lenient_r5_read_r4(raw)
    };
    let package_id = ctx
        .pkg
        .package_id_for(&url)
        .or_else(|| ctx.pkg.package_id_for(query));
    fix_loaded_resource(&mut sd, package_id.as_deref());
    if sd
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .is_some()
    {
        // LAYER B / B1 composition (a): pin this base/dep snapshot before it is
        // cached + inherited (no-op when the flag is off).
        pin_base_if_enabled(ctx, &url, &mut sd);
        let rc = Rc::new(sd);
        ctx.gen_cache.insert(url, rc.clone());
        return Ok(Some(rc));
    }
    // No snapshot: recursively walk-generate one. The recursive walk carries the
    // same `pin_base_versions` flag, so its inherited canonicals are already
    // pinned; we then pin this SD's own newly-materialized snapshot canonicals.
    let mut generated = super::generate_snapshot_inner(ctx, sd)?;
    pin_base_if_enabled(ctx, &url, &mut generated);
    let rc = Rc::new(generated);
    ctx.gen_cache.insert(url, rc.clone());
    Ok(Some(rc))
}
