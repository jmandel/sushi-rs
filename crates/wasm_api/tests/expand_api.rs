//! Native coverage for the additive wasm-api surface the editor's terminology
//! tiers (spec §6) + lazy loading (spec §1) depend on:
//!   - `expand_enumerable(vs, resources)` — the tier-1 in-engine expansion the
//!     ValueSet tab calls per keystroke (thin wrapper over
//!     `compiler::terminology`).
//!   - `mount_bundles(bundles)` — the additive, idempotent lazy-mount seam.
//!
//! These functions return `Result<String, JsError>`; `JsError` is opaque
//! natively, so the tests inspect the Ok JSON string and treat any Err as a
//! failure via a small helper.
//!
//! This suite deliberately exercises the DEPRECATED legacy free-function surface
//! (kept for the live editor until F6) — hence `allow(deprecated)`. The new
//! `Session` surface is covered in tests/session_api.rs, including a check that
//! the legacy raw payloads equal the Session envelope's `result`.
#![allow(deprecated)]

use serde_json::{json, Value};
use wasm_api::Session;

/// Session-envelope helpers: every op returns `{apiVersion, ok, op, result|error}`.
fn call(env_json: String) -> Value {
    let env: Value = serde_json::from_str(&env_json).unwrap();
    assert_eq!(env["ok"], true, "engine error: {env}");
    env["result"].clone()
}
fn expand_enumerable(vs: &str, resources: &str) -> Result<String, String> {
    let s = Session::new();
    let env: Value = serde_json::from_str(&s.expand_valueset(vs, resources)).unwrap();
    if env["ok"] == true {
        Ok(env["result"].to_string())
    } else {
        Err(env["error"]["message"].to_string())
    }
}
fn init(bundles: &str) -> Result<u32, String> {
    Ok(call(Session::global().init(bundles))["mounted"]
        .as_u64()
        .unwrap() as u32)
}
fn mount_bundles(bundles: &str) -> Result<u32, String> {
    Ok(call(Session::global().mount(bundles))["mounted"]
        .as_u64()
        .unwrap() as u32)
}
fn wasm_resolve(config: &str, index: &str) -> Result<String, String> {
    Ok(call(Session::global().resolve_project(config, index)).to_string())
}

fn ok(r: Result<String, String>) -> Value {
    // `JsError` has no Debug/Display we can read natively; on Err we only know it
    // failed. Surface that as a test failure with a generic message.
    match r {
        Ok(s) => serde_json::from_str(&s).expect("valid JSON from wasm-api"),
        Err(_) => panic!("wasm-api call returned Err(JsError)"),
    }
}

#[test]
fn expand_enumerable_local_codesystem() {
    // The cycle IG's menstrual-flow shape: 5 enumerated codes over a local
    // complete CodeSystem — tier-1 enumerable, displays filled from the CS.
    let cs = json!({
        "resourceType": "CodeSystem",
        "url": "https://cycle.fhir.me/CodeSystem/cycle",
        "version": "0.2.0",
        "content": "complete",
        "concept": [
            {"code": "flow-none", "display": "None"},
            {"code": "flow-spotting", "display": "Spotting"},
            {"code": "flow-light", "display": "Light"},
            {"code": "flow-moderate", "display": "Moderate"},
            {"code": "flow-heavy", "display": "Heavy"}
        ]
    });
    let vs = json!({
        "resourceType": "ValueSet",
        "url": "https://cycle.fhir.me/ValueSet/menstrual-flow",
        "compose": {"include": [{"system": "https://cycle.fhir.me/CodeSystem/cycle", "concept": [
            {"code": "flow-none"}, {"code": "flow-spotting"}, {"code": "flow-light"},
            {"code": "flow-moderate"}, {"code": "flow-heavy"}
        ]}]}
    });
    let out = ok(expand_enumerable(&vs.to_string(), &json!([cs]).to_string()));
    assert_eq!(out["ok"], true);
    assert_eq!(out["expansion"]["total"], 5);
    let contains = out["expansion"]["contains"].as_array().unwrap();
    assert_eq!(contains.len(), 5);
    // Display filled from the local CS.
    let none = contains.iter().find(|c| c["code"] == "flow-none").unwrap();
    assert_eq!(none["display"], "None");
    // used-codesystem version surfaced for the "code system versions" table.
    let used = out["usedCodeSystems"].as_array().unwrap();
    assert_eq!(used.len(), 1);
    assert_eq!(used[0]["system"], "https://cycle.fhir.me/CodeSystem/cycle");
    assert_eq!(used[0]["version"], "0.2.0");
}

#[test]
fn expand_enumerable_external_filter_refuses_verbatim() {
    // A filter over an external system with no local content → NotEnumerable, the
    // precise "needs terminology server" state the editor renders verbatim.
    let vs = json!({
        "resourceType": "ValueSet",
        "url": "https://ex.org/vs/snomed-filter",
        "compose": {"include": [{"system": "http://snomed.info/sct", "filter": [
            {"property": "concept", "op": "is-a", "value": "73211009"}
        ]}]}
    });
    let out = ok(expand_enumerable(&vs.to_string(), "[]"));
    assert_eq!(out["ok"], false);
    let ne = &out["notEnumerable"];
    assert_eq!(ne["component"], "include");
    assert_eq!(ne["index"], 0);
    // A single-line, human refusal reason naming the offending system.
    let reason = ne["reason"].as_str().unwrap();
    assert!(
        reason.contains("snomed"),
        "reason names the system: {reason}"
    );
    // Machine-readable class for the UI to branch on.
    assert_eq!(ne["kind"], "UnresolvableOrIncompleteSystem");
    // Display = "component[index]: reason".
    assert_eq!(ne["display"], format!("include[0]: {reason}"));
}

#[test]
fn expand_enumerable_accepts_object_resource_map() {
    // The editor may pass its predefined `path -> body` object; accept it too.
    let cs = json!({
        "resourceType": "CodeSystem", "url": "https://ex.org/cs", "version": "1",
        "content": "complete", "concept": [{"code": "a", "display": "A"}]
    });
    let vs = json!({
        "resourceType": "ValueSet", "url": "https://ex.org/vs",
        "compose": {"include": [{"system": "https://ex.org/cs"}]}
    });
    let resources = json!({ "input/resources/CodeSystem-cs.json": cs });
    let out = ok(expand_enumerable(&vs.to_string(), &resources.to_string()));
    assert_eq!(out["ok"], true);
    assert_eq!(out["expansion"]["total"], 1);
}

#[test]
fn mount_bundles_is_additive_and_idempotent() {
    // init with one synthetic package, then mount_bundles a second, then re-mount
    // the first (skipped). Package count reflects the union.
    let pkg = |label: &str| {
        let (name, version) = label.split_once('#').unwrap();
        let package_json = json!({ "name": name, "version": version }).to_string();
        json!({
            "label": label,
            // Identity metadata is mandatory even for an inert package; the
            // mount boundary verifies that the nominal label names these bytes.
            "files": {
                "package.json": base64(package_json.as_bytes()),
                ".index.json": base64(br#"{"files":[]}"#),
            }
        })
    };
    assert_eq!(
        unwrap_u32(init(&json!([pkg("pkg.a#1.0.0")]).to_string())),
        1
    );
    // Add a new package.
    assert_eq!(
        unwrap_u32(mount_bundles(&json!([pkg("pkg.b#1.0.0")]).to_string())),
        2
    );
    // Re-mount an already-present package → skipped, count unchanged.
    assert_eq!(
        unwrap_u32(mount_bundles(&json!([pkg("pkg.a#1.0.0")]).to_string())),
        2
    );
    // Mount both (one new, one dup) → only the new one lands.
    assert_eq!(
        unwrap_u32(mount_bundles(
            &json!([pkg("pkg.a#1.0.0"), pkg("pkg.c#1.0.0")]).to_string()
        )),
        3
    );
}

// NOTE: the mount_bundles ERROR path (bad base64 → `JsError`) can't be exercised
// natively — constructing a `JsError` panics off-wasm ("cannot call wasm-bindgen
// imported functions on non-wasm targets"). The recovery behaviour (a failed
// mount leaves the engine's existing state intact, because the function mutates a
// clone and only commits on success) is verified by inspection of `mount_bundles`
// + covered end-to-end by the editor's error-path handling.

/// Gate ii (task #32): the wasm `resolve_project` export and the native
/// `package_store::resolve_project` produce IDENTICAL JSON for the same config +
/// mounted state. This proves there is exactly one resolver: the wasm surface is a
/// thin marshalling shell over the same Rust function the CLI + `.cjs` shim drive.
#[test]
fn wasm_resolver_equals_native_resolver() {
    // Build two synthetic R4 packages with a transitive dep, plus the core, as
    // bundle inputs. `dep` depends on `t`; both R4.
    let pkgjson = |id: &str, deps: &[(&str, &str)]| {
        let d: serde_json::Map<String, Value> = deps
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
            .collect();
        json!({ "name": id, "version": "1.0.0", "fhirVersions": ["4.0.1"], "dependencies": d })
            .to_string()
    };
    let bundle = |label: &str, pkg: &str| {
        json!({
            "label": label,
            "files": { "package.json": base64(pkg.as_bytes()) }
        })
    };
    let core_pkg = json!({
        "name": "hl7.fhir.r4.core", "version": "4.0.1", "fhirVersions": ["4.0.1"]
    })
    .to_string();

    let bundles = json!([
        bundle("hl7.fhir.r4.core#4.0.1", &core_pkg),
        bundle("dep#1.0.0", &pkgjson("dep", &[("t", "1.0.0")])),
        bundle("t#1.0.0", &pkgjson("t", &[])),
    ]);
    assert_eq!(unwrap_u32(init(&bundles.to_string())), 3);

    let config = "fhirVersion: 4.0.1\ndependencies:\n  dep: 1.0.0\n";
    // A version index covering the mounted set (so latest/auto-deps resolve the
    // same way both sides).
    let index = json!({
        "versions": {
            "hl7.fhir.r4.core": ["4.0.1"],
            "dep": ["1.0.0"],
            "t": ["1.0.0"]
        }
    })
    .to_string();

    // wasm surface (reads the just-init'd engine).
    let wasm_json = wasm_resolve(config, &index).expect("wasm resolve ok");

    // native: build the identical BundleSource + call package_store directly.
    let mut src = package_store::BundleSource::new();
    for (label, pkg) in [
        ("hl7.fhir.r4.core#4.0.1", core_pkg.clone()),
        ("dep#1.0.0", pkgjson("dep", &[("t", "1.0.0")])),
        ("t#1.0.0", pkgjson("t", &[])),
    ] {
        src.mount_package(label, vec![("package.json".to_string(), pkg.into_bytes())]);
    }
    let vindex: package_store::VersionIndex = serde_json::from_str(&index).unwrap();
    let native_step = package_store::resolve_project(config, &src, src.cache_root(), Some(&vindex))
        .expect("native resolve ok");
    let native_json = native_step.to_json();

    // Compare as parsed JSON (field-for-field identical).
    let a: Value = serde_json::from_str(&wasm_json).unwrap();
    let b: Value = serde_json::from_str(&native_json).unwrap();
    assert_eq!(a, b, "wasm resolver JSON must equal native resolver JSON");
    // Sanity: the context closure walked transitively (core, dep, t).
    let ctx = a["context_closure"].as_array().unwrap();
    let ids: Vec<&str> = ctx
        .iter()
        .map(|r| r["package_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"dep") && ids.contains(&"t") && ids.contains(&"hl7.fhir.r4.core"));
}

fn unwrap_u32(r: Result<u32, String>) -> u32 {
    r.unwrap_or_else(|e| panic!("wasm-api call returned Err: {e}"))
}

/// Minimal standard base64 encode (mirrors the decoder in the crate).
fn base64(bytes: &[u8]) -> String {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(ALPHA[((n >> 18) & 63) as usize] as char);
        out.push(ALPHA[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHA[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHA[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
