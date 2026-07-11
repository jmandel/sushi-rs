//! Layer B / Phase B1 — CoreVersionPinner port (mechanism A).
//!
//! Java source: `fhir-core@6.9.10 org.hl7.fhir.r5/.../context/CoreVersionPinner.java`.
//! In Java this runs UNCONDITIONALLY at package-load time
//! (`BaseWorkerContext.finishLoading` -> `CoreVersionPinner.pinCoreVersions`,
//! `BaseWorkerContext.java:3285`) BEFORE base-snapshot generation
//! (`:3290`), so the pinned core snapshots are what profile `generateSnapshot`
//! later copies from; profile snapshots INHERIT already-pinned canonicals.
//!
//! ## Composition choice (audit §6, task-#17 sequencing question): (b), gated.
//!
//! We run the pin as a POST-PASS over the finished walk snapshot rather than
//! pinning loaded base SDs pre-walk (option (a)). The two are observationally
//! equivalent for the artifact fields, and (b) keeps Layer A a pure, policy-free
//! function (the hard rule) and makes Layer B cleanly opt-in / default-off:
//!
//!   * The pinned value is a pure function of (canonical URL string, resolution
//!     context): the same `context.fetchResource(...).getVersion()` in both
//!     compositions (CoreVersionPinner.java:100-105).
//!   * Layer A is decision-isomorphic: the walk copies base-snapshot elements
//!     verbatim (canonical strings included) and never synthesizes NEW
//!     unversioned core canonicals that Java's pre-walk pinner would not also
//!     see. So the *set of canonical strings* in the final snapshot is identical
//!     whether Java pinned the base pre-walk (and inheritance copied the pinned
//!     string) or we pin the final snapshot post-walk.
//!   * Therefore post-walk pinning over the SAME `PackageContext`, gating on the
//!     SAME resolution, yields the identical pinned strings. The proof is the
//!     gate: full resource parity vs the fresh Java Cycle oracle, every residual
//!     classified.
//!
//! Default OFF: `generate_snapshot` never calls this; only the opt-in
//! `generate_snapshot_layer_b` does.

use serde_json::Value;

use crate::PackageContext;

/// Pin unversioned, in-context, resolvable canonicals on a finished SD (the walk
/// output). Mirrors `CoreVersionPinner.pinCoreVersions(...)` restricted to the
/// StructureDefinition traversal (CoreVersionPinner.java:31-38): `baseDefinition`
/// + every `differential` element + every `snapshot` element.
///
/// The differential is walked too, faithfully to Java — authored differential
/// canonicals come out clean because they don't resolve to a versioned in-context
/// resource, NOT because differential is skipped (quirk `pin.walk-everything-gate-on-resolution`).
pub fn pin_core_versions(sd: &mut Value, pkg: &PackageContext) {
    // sd.baseDefinition -> pinCoreVersionSD (CoreVersionPinner.java:32).
    pin_sd_field(sd, "baseDefinition", pkg);

    for section in ["differential", "snapshot"] {
        if let Some(elements) = sd
            .get_mut(section)
            .and_then(|s| s.get_mut("element"))
            .and_then(Value::as_array_mut)
        {
            for ed in elements.iter_mut() {
                pin_element(ed, pkg);
            }
        }
    }
}

/// `pinCoreVersions(ElementDefinition)` — CoreVersionPinner.java:42-61.
fn pin_element(ed: &mut Value, pkg: &PackageContext) {
    // type[].profile[] and type[].targetProfile[] -> pinCoreVersionSD (:45,:48).
    if let Some(types) = ed.get_mut("type").and_then(Value::as_array_mut) {
        for tr in types.iter_mut() {
            pin_sd_array(tr, "profile", pkg);
            pin_sd_array(tr, "targetProfile", pkg);
        }
    }
    // valueAlternatives[] (R5-only, "for thoroughness") -> pinCoreVersionSD (:52-54).
    // quirk `pin.value-alternatives-thoroughness`.
    pin_sd_array(ed, "valueAlternatives", pkg);

    // binding.valueSet + binding.additional[].valueSet -> pinCoreVersionVS (:56,:58).
    if let Some(binding) = ed.get_mut("binding") {
        pin_vs_field(binding, "valueSet", pkg);
        if let Some(additional) = binding.get_mut("additional").and_then(Value::as_array_mut) {
            for adb in additional.iter_mut() {
                pin_vs_field(adb, "valueSet", pkg);
            }
        }
    }
}

/// Append `|<version>` to a single scalar SD canonical field if pinnable.
fn pin_sd_field(obj: &mut Value, key: &str, pkg: &PackageContext) {
    if let Some(v) = obj.get(key).and_then(Value::as_str) {
        if let Some(pinned) = pin_sd_value(v, pkg) {
            obj[key] = Value::String(pinned);
        }
    }
}

/// Append `|<version>` to each element of a canonical[] SD field if pinnable.
fn pin_sd_array(obj: &mut Value, key: &str, pkg: &PackageContext) {
    if let Some(arr) = obj.get_mut(key).and_then(Value::as_array_mut) {
        for item in arr.iter_mut() {
            if let Some(s) = item.as_str() {
                if let Some(pinned) = pin_sd_value(s, pkg) {
                    *item = Value::String(pinned);
                }
            }
        }
    }
}

/// Append `|<version>` to a single scalar VS canonical field if pinnable.
fn pin_vs_field(obj: &mut Value, key: &str, pkg: &PackageContext) {
    if let Some(v) = obj.get(key).and_then(Value::as_str) {
        if let Some(pinned) = pin_vs_value(v, pkg) {
            obj[key] = Value::String(pinned);
        }
    }
}

/// Already-versioned guard: a canonical containing `|` is skipped by every
/// `pinCoreVersion{SD,VS,CS}` (CoreVersionPinner.java:88,:98).
pub(crate) fn is_already_versioned(url: &str) -> bool {
    url.contains('|')
}

/// THO carve-out predicate: `pinCoreVersionVS`/`pinCoreVersionCS` skip URLs
/// containing `terminology.hl7.org` (CoreVersionPinner.java:88); `pinCoreVersionSD`
/// does NOT (:97) — quirk `pin.tho-asymmetry`.
pub(crate) fn is_tho(url: &str) -> bool {
    url.contains("terminology.hl7.org")
}

/// `pinCoreVersionSD` — CoreVersionPinner.java:97-105. NO `terminology.hl7.org`
/// guard (asymmetry with VS/CS — quirk `pin.tho-asymmetry`). Resolves the target
/// as a StructureDefinition; pins with the resolved SD's version.
fn pin_sd_value(url: &str, pkg: &PackageContext) -> Option<String> {
    if is_already_versioned(url) {
        return None; // already versioned (:98)
    }
    let version = resolved_version(pkg, url, "StructureDefinition")?;
    Some(format!("{url}|{version}"))
}

/// `pinCoreVersionVS` — CoreVersionPinner.java:87-95. Skips `terminology.hl7.org`
/// (quirk `pin.tho-asymmetry`). Resolves the target as a ValueSet.
fn pin_vs_value(url: &str, pkg: &PackageContext) -> Option<String> {
    if is_already_versioned(url) {
        return None; // already versioned (:88)
    }
    if is_tho(url) {
        return None; // THO carve-out (:88) — VS/CS only
    }
    let version = resolved_version(pkg, url, "ValueSet")?;
    Some(format!("{url}|{version}"))
}

/// Resolve `url` in-context and return the target's `version` IFF the target
/// exists AND has the expected `resourceType` AND `hasVersion()`
/// (CoreVersionPinner.java:100-105 `x != null && x.hasVersion()`). The
/// resource-type gate mirrors Java's type-scoped `fetchResource(X.class, ...)`:
/// a VS canonical must resolve to a ValueSet, an SD canonical to a
/// StructureDefinition. Uses the opt-in `resolve_canonical_version` so that
/// ValueSet/CodeSystem canonicals resolve (Layer A's `fetch`/`by_url` see SDs
/// only).
fn resolved_version(pkg: &PackageContext, url: &str, resource_type: &str) -> Option<String> {
    pkg.resolve_canonical_version(url, resource_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_versioned_guard() {
        // Guard `pin.*` : a `|`-bearing canonical is skipped pre-fetch (:88,:98).
        assert!(is_already_versioned(
            "http://hl7.org/fhir/StructureDefinition/Observation|4.0.1"
        ));
        assert!(!is_already_versioned(
            "http://hl7.org/fhir/StructureDefinition/Observation"
        ));
    }

    #[test]
    fn tho_guard_asymmetry() {
        // quirk `pin.tho-asymmetry`: THO is a guard for VS/CS only.
        let tho_vs = "http://terminology.hl7.org/ValueSet/v3-ActReason";
        assert!(is_tho(tho_vs));
        // VS pinning short-circuits on THO (no pkg fetch needed to prove the guard).
        // SD pinning has NO THO guard — is_tho is never consulted on the SD path.
        assert!(!is_tho("http://hl7.org/fhir/ValueSet/languages"));
    }
}
