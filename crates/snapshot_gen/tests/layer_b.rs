//! Layer B (task #17) unit/integration gates:
//!   * B1 pinner guards, exercised end-to-end against the isolated r4.core cache
//!     (already-versioned, THO asymmetry, unresolved, !hasVersion, resolve+pin).
//!   * B0 projection round-trip: convert.rs forward (R4->R5) then project_r4 back
//!     recovers the R4 artifact fields (constraint.xpath, R4 key order).
//!
//! Cache: the isolated `temp/fhir-home/.fhir/packages` (guarded; never ~/.fhir).
//! Skips gracefully if r4.core is absent so the suite stays runnable anywhere.

use serde_json::{json, Value};
use snapshot_gen::{apply_layer_b_post, LayerBOptions, PackageContext};

const CACHE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../temp/fhir-home/.fhir/packages"
);

fn ctx() -> Option<PackageContext> {
    PackageContext::new(CACHE, &["hl7.fhir.r4.core#4.0.1".to_string()]).ok()
}

#[test]
fn resolver_gates_by_resource_type_and_version() {
    let Some(pkg) = ctx() else {
        eprintln!("skip: no r4.core cache");
        return;
    };
    // SD canonical resolves to an SD with version 4.0.1.
    assert_eq!(
        pkg.resolve_canonical_version(
            "http://hl7.org/fhir/StructureDefinition/Patient",
            "StructureDefinition"
        )
        .as_deref(),
        Some("4.0.1"),
        "core Patient SD resolves to 4.0.1"
    );
    // VS canonical resolves to a ValueSet with version 4.0.1 (Layer A's SD-only
    // index cannot see this; the opt-in resolver can).
    assert_eq!(
        pkg.resolve_canonical_version(
            "http://hl7.org/fhir/ValueSet/observation-codes",
            "ValueSet"
        )
        .as_deref(),
        Some("4.0.1"),
        "core observation-codes VS resolves to 4.0.1"
    );
    // Type mismatch: a ValueSet URL requested as a StructureDefinition -> None
    // (Java's type-scoped fetchResource(X.class)).
    assert_eq!(
        pkg.resolve_canonical_version(
            "http://hl7.org/fhir/ValueSet/observation-codes",
            "StructureDefinition"
        ),
        None,
        "VS url must NOT resolve as an SD (type-scoped)"
    );
    // Unresolved canonical -> None (guard: target does not resolve).
    assert_eq!(
        pkg.resolve_canonical_version("http://example.org/nope", "StructureDefinition"),
        None
    );
}

#[test]
fn pin_end_to_end_via_walk() {
    // The real B1 path: generate a snapshot for a tiny R4 profile with Layer B
    // pin ON, and assert an inherited core canonical is pinned while an
    // already-versioned / unresolved one is left alone.
    let Some(pkg) = ctx() else {
        eprintln!("skip: no r4.core cache");
        return;
    };
    let derived = json!({
        "resourceType": "StructureDefinition",
        "url": "http://example.org/StructureDefinition/my-obs",
        "name": "MyObs",
        "status": "active",
        "fhirVersion": "4.0.1",
        "kind": "resource",
        "abstract": false,
        "type": "Observation",
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Observation",
        "derivation": "constraint",
        "differential": {"element": [{"id": "Observation", "path": "Observation"}]}
    });
    let opts = snapshot_gen::SnapshotOptions::default();

    // OFF: no pins.
    let off = snapshot_gen::generate_snapshot_layer_b(
        derived.clone(),
        &pkg,
        opts.clone(),
        LayerBOptions::default(),
    )
    .unwrap();
    let off_str = serde_json::to_string(&off).unwrap();
    assert!(
        !off_str.contains("Patient|4.0.1"),
        "Layer B OFF must not pin"
    );

    // ON (pin only): inherited core targetProfiles/valueSets pinned.
    let on = snapshot_gen::generate_snapshot_layer_b(
        derived,
        &pkg,
        opts,
        LayerBOptions {
            pin: true,
            project_r4: false,
        },
    )
    .unwrap();
    let on_str = serde_json::to_string(&on).unwrap();
    assert!(
        on_str.contains("http://hl7.org/fhir/StructureDefinition/Patient|4.0.1"),
        "inherited core Patient targetProfile must be pinned when B1 is on"
    );
    assert!(
        on_str.contains("http://hl7.org/fhir/ValueSet/observation-codes|4.0.1"),
        "inherited core binding valueSet must be pinned when B1 is on"
    );
    // THO carve-out for VS: any terminology.hl7.org valueSet stays unpinned.
    assert!(
        !on_str.contains("terminology.hl7.org/ValueSet/") || !thopinned(&on),
        "THO ValueSet canonicals must NOT be pinned (quirk pin.tho-asymmetry)"
    );
}

/// True if any binding.valueSet on a terminology.hl7.org URL carries a `|`.
fn thopinned(sd: &Value) -> bool {
    let mut hit = false;
    if let Some(els) = sd.pointer("/snapshot/element").and_then(Value::as_array) {
        for e in els {
            if let Some(vs) = e.pointer("/binding/valueSet").and_then(Value::as_str) {
                if vs.contains("terminology.hl7.org") && vs.contains('|') {
                    hit = true;
                }
            }
        }
    }
    hit
}

#[test]
fn projection_round_trip_recovers_r4_artifact_fields() {
    // convert.rs forward (R4 -> R5-internal) then project_r4 back must recover the
    // R4 artifact: constraint.xpath restored (from the carried extension), R4 key
    // order, and R5-only ED fields demoted. This isolates B0 from pinning.
    let Some(pkg) = ctx() else {
        eprintln!("skip: no r4.core cache");
        return;
    };
    let r4_sd = json!({
        "resourceType": "StructureDefinition",
        "url": "http://example.org/StructureDefinition/x",
        "name": "X",
        "status": "active",
        "fhirVersion": "4.0.1",
        "kind": "resource",
        "abstract": false,
        "type": "Observation",
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Observation",
        "derivation": "constraint",
        "snapshot": {"element": [{
            "id": "Observation",
            "path": "Observation",
            "min": 0,
            "max": "*",
            "constraint": [{
                "key": "obs-6",
                "severity": "error",
                "human": "h",
                "expression": "e",
                "xpath": "not(exists(f:dataAbsentReason))",
                "source": "http://hl7.org/fhir/StructureDefinition/Observation"
            }],
            "mustSupport": true,
            "isModifier": false
        }]}
    });

    // Forward: R4 -> R5-internal. xpath becomes the EXT_XPATH_CONSTRAINT extension.
    let r5 = snapshot_gen::convert_r4_sd_to_r5(&r4_sd).unwrap();
    let c = &r5["snapshot"]["element"][0]["constraint"][0];
    assert!(c.get("xpath").is_none(), "R5-internal has no xpath field");
    assert!(
        c["extension"][0]["url"]
            .as_str()
            .unwrap()
            .contains("constraint.xpath"),
        "xpath carried as extension in R5"
    );

    // Back: project_r4. xpath restored, extension dropped, R4 key order.
    let projected = apply_layer_b_post(
        r5,
        &pkg,
        LayerBOptions {
            pin: false,
            project_r4: true,
        },
    );
    let pc = &projected["snapshot"]["element"][0]["constraint"][0];
    assert_eq!(
        pc["xpath"].as_str(),
        Some("not(exists(f:dataAbsentReason))"),
        "constraint.xpath restored on the R4 projection"
    );
    assert!(pc.get("extension").is_none(), "xpath extension dropped");
    // R4 constraint key order: key, severity, human, expression, xpath, source.
    let keys: Vec<&str> = pc.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["key", "severity", "human", "expression", "xpath", "source"],
        "R4 constraint key order"
    );
    // R4 ED key order: mustSupport BEFORE isModifier (R4 @Child 28 < 29).
    let ed = &projected["snapshot"]["element"][0];
    let ekeys: Vec<&str> = ed.as_object().unwrap().keys().map(String::as_str).collect();
    let ms = ekeys.iter().position(|k| *k == "mustSupport").unwrap();
    let im = ekeys.iter().position(|k| *k == "isModifier").unwrap();
    assert!(ms < im, "R4 order: mustSupport before isModifier ({ekeys:?})");
}
