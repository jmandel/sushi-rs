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

use serde_json::{json, Value};
use wasm_api::{expand_enumerable, init, mount_bundles};

fn ok(r: Result<String, wasm_bindgen::JsError>) -> Value {
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
    let out = ok(expand_enumerable(
        &vs.to_string(),
        &json!([cs]).to_string(),
    ));
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
    assert!(reason.contains("snomed"), "reason names the system: {reason}");
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
        json!({
            "label": label,
            // `.index.json` with an empty files array is a valid (if inert) package
            // dir the BundleSource can mount; we only assert the mount bookkeeping.
            "files": { ".index.json": base64(br#"{"files":[]}"#) }
        })
    };
    assert_eq!(unwrap_u32(init(&json!([pkg("pkg.a#1.0.0")]).to_string())), 1);
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

fn unwrap_u32(r: Result<u32, wasm_bindgen::JsError>) -> u32 {
    r.unwrap_or_else(|_| panic!("wasm-api call returned Err(JsError)"))
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
