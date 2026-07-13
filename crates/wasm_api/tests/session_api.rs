//! Native coverage for the isolated, enveloped `Session` API. Two things are
//! proven here:
//!   1. Session methods return the uniform envelope
//!      (`{ apiVersion, ok, op, result | error }`).
//!   2. Envelope payloads remain identical to their typed native computations.
//!
//! `Session` methods return a JSON string (never a `JsError` for domain errors —
//! failures land as `ok:false`), so unlike expand_api.rs we never need the
//! JsError-panics-off-wasm dance.

use serde_json::{json, Value};
use wasm_api::Session;

fn parse(s: String) -> Value {
    serde_json::from_str(&s).expect("Session returns valid JSON")
}

/// Minimal standard base64 encode (mirrors the crate's decoder), for synthetic
/// package bundles.
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

fn synthetic_pkg(label: &str) -> Value {
    let (name, version) = label.split_once('#').unwrap();
    let package_json = json!({ "name": name, "version": version }).to_string();
    json!({
        "label": label,
        "files": {
            "package.json": base64(package_json.as_bytes()),
            ".index.json": base64(br#"{"files":[]}"#),
        }
    })
}

#[test]
fn session_envelope_shape_on_success_and_error() {
    let s = Session::new();

    // Success: init returns { mounted } inside the envelope.
    let ok = parse(s.init(&json!([synthetic_pkg("pkg.a#1.0.0")]).to_string()));
    assert_eq!(ok["apiVersion"], 1);
    assert_eq!(ok["ok"], true);
    assert_eq!(ok["op"], "init");
    assert_eq!(ok["result"]["mounted"], 1);
    assert!(ok.get("error").is_none());

    // Error: snapshot before any compile that resolves the URL still succeeds with
    // a not-found message (domain result), but a truly malformed op — expand with
    // non-JSON resources — lands as ok:false with an error message.
    let err = parse(s.expand_valueset("not json", "[]"));
    assert_eq!(err["apiVersion"], 1);
    assert_eq!(err["ok"], false);
    assert_eq!(err["op"], "expandValueSet");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("bad ValueSet JSON"));
    assert!(err.get("result").is_none());
}

#[test]
fn session_mount_is_additive_and_idempotent() {
    let s = Session::new();
    let mounted = |v: Value| v["result"]["mounted"].as_u64().unwrap();
    assert_eq!(
        mounted(parse(
            s.init(&json!([synthetic_pkg("m.a#1.0.0")]).to_string())
        )),
        1
    );
    assert_eq!(
        mounted(parse(
            s.mount(&json!([synthetic_pkg("m.b#1.0.0")]).to_string())
        )),
        2
    );
    // dup is skipped
    assert_eq!(
        mounted(parse(
            s.mount(&json!([synthetic_pkg("m.a#1.0.0")]).to_string())
        )),
        2
    );
}

#[test]
fn independently_constructed_sessions_do_not_share_mutable_state() {
    let first = Session::new();
    let second = Session::new();
    let initialized = parse(first.init(&json!([synthetic_pkg("isolated.a#1.0.0")]).to_string()));
    assert_eq!(initialized["ok"], true);

    // The second handle has no mounted package source. Under the old zero-sized
    // global handle this unexpectedly succeeded by observing `first`'s state.
    let second_mount = parse(second.mount(&json!([synthetic_pkg("isolated.b#1.0.0")]).to_string()));
    assert_eq!(second_mount["ok"], false);
    assert!(second_mount["error"]["message"]
        .as_str()
        .unwrap()
        .contains("engine not initialized"));

    let first_mount = parse(first.mount(&json!([synthetic_pkg("isolated.b#1.0.0")]).to_string()));
    assert_eq!(first_mount["result"]["mounted"], 2);
}

#[test]
fn session_expand_result_is_stable() {
    // Repeating the same expansion through one Session yields the same payload.
    let cs = json!({
        "resourceType": "CodeSystem", "url": "https://ex.org/cs", "version": "1",
        "content": "complete", "concept": [{"code": "a", "display": "A"}]
    });
    let vs = json!({
        "resourceType": "ValueSet", "url": "https://ex.org/vs",
        "compose": {"include": [{"system": "https://ex.org/cs"}]}
    });

    let s = Session::new();
    let enveloped = parse(s.expand_valueset(&vs.to_string(), &json!([cs]).to_string()));
    assert_eq!(enveloped["ok"], true);
    let session_result = &enveloped["result"];

    let repeated: Value = serde_json::from_str(&{
        let env: Value =
            serde_json::from_str(&s.expand_valueset(&vs.to_string(), &json!([cs]).to_string()))
                .unwrap();
        assert_eq!(env["ok"], true);
        env["result"].to_string()
    })
    .unwrap();

    assert_eq!(session_result, &repeated, "Session result must be stable");
    assert_eq!(session_result["expansion"]["total"], 1);
}

#[test]
fn session_resolve_result_matches_native_contract() {
    // Session.resolveProject exposes the native package_store resolver contract.
    let pkgjson = |id: &str, deps: &[(&str, &str)]| {
        let d: serde_json::Map<String, Value> = deps
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
            .collect();
        json!({ "name": id, "version": "1.0.0", "fhirVersions": ["4.0.1"], "dependencies": d })
            .to_string()
    };
    let bundle = |label: &str, pkg: &str| json!({ "label": label, "files": { "package.json": base64(pkg.as_bytes()) } });
    let core_pkg =
        json!({ "name": "hl7.fhir.r4.core", "version": "4.0.1", "fhirVersions": ["4.0.1"] })
            .to_string();
    let bundles = json!([
        bundle("hl7.fhir.r4.core#4.0.1", &core_pkg),
        bundle("dep#1.0.0", &pkgjson("dep", &[("t", "1.0.0")])),
        bundle("t#1.0.0", &pkgjson("t", &[])),
    ]);

    let s = Session::new();
    parse(s.init(&bundles.to_string()));

    let config = "fhirVersion: 4.0.1\ndependencies:\n  dep: 1.0.0\n";
    let index = json!({ "versions": {
        "hl7.fhir.r4.core": ["4.0.1"], "dep": ["1.0.0"], "t": ["1.0.0"]
    }})
    .to_string();

    let enveloped = parse(s.resolve_project(config, &index));
    assert_eq!(enveloped["ok"], true);
    let session_result = &enveloped["result"];

    let ctx = session_result["context_closure"].as_array().unwrap();
    let ids: Vec<&str> = ctx
        .iter()
        .map(|r| r["package_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"dep") && ids.contains(&"t") && ids.contains(&"hl7.fhir.r4.core"));
}

#[test]
fn session_resolve_template_reports_exact_missing_parent_then_complete_chain() {
    let package = |label: &str, manifest: &str| {
        json!({
            "label": label,
            "files": {
                "package.json": base64(manifest.as_bytes()),
                ".index.json": base64(br#"{"files":[]}"#),
            }
        })
    };
    let leaf = package(
        "leaf#2.0.0",
        r#"{"name":"leaf","version":"2.0.0","type":"fhir.template","base":"base","dependencies":{"base":"1.0.0"}}"#,
    );
    let base = package(
        "base#1.0.0",
        r#"{"name":"base","version":"1.0.0","type":"fhir.template"}"#,
    );

    let session = Session::new();
    assert_eq!(parse(session.init(&json!([leaf]).to_string()))["ok"], true);
    let missing = parse(session.resolve_template("leaf#2.0.0"));
    assert_eq!(missing["ok"], true);
    assert_eq!(missing["result"]["satisfied"], false);
    assert_eq!(missing["result"]["missing"], "base#1.0.0");

    assert_eq!(parse(session.mount(&json!([base]).to_string()))["ok"], true);
    let complete = parse(session.resolve_template("leaf#2.0.0"));
    assert_eq!(complete["ok"], true);
    assert_eq!(complete["result"]["satisfied"], true);
    assert_eq!(
        complete["result"]["chain"],
        json!(["base#1.0.0", "leaf#2.0.0"])
    );
}

#[test]
fn session_version_is_stamped() {
    let v: Value = serde_json::from_str(&Session::version()).unwrap();
    assert_eq!(v["apiVersion"], 1);
    assert!(v["version"].is_string());
    assert!(v["engine"].as_str().unwrap().contains("rust_sushi"));
}

#[test]
fn project_compile_and_site_projection_fail_loud_without_hidden_fallbacks() {
    let session = Session::new();

    // Site generation has one atomic project boundary; there is no public
    // compile-then-prepare successor operation or hidden fallback.
    let closed = parse(
        session.prepare_project_site(
            &json!({
                "config": "id: demo\nfhirVersion: 4.0.1\n",
                "fsh": {},
                "predefined": {},
                "siteFiles": {}
            })
            .to_string(),
            &json!({"generator":"cycle", "buildEpochSecs":1, "liquidAssetDirs":[]}).to_string(),
        ),
    );
    assert_eq!(closed["ok"], false);
    assert_eq!(closed["op"], "prepareProject");
    assert!(closed["error"]["message"]
        .as_str()
        .unwrap()
        .contains("resolver"));

    let resent = parse(
        session.prepare_project_site(
            &json!({
                "config": "id: demo\nfhirVersion: 4.0.1\n",
                "fsh": {},
                "predefined": {},
                "siteFiles": {}
            })
            .to_string(),
            &json!({
                "generator":"cycle",
                "buildEpochSecs":1,
                "liquidAssetDirs":[],
                "config":"id: forbidden",
                "siteFiles":{}
            })
            .to_string(),
        ),
    );
    assert_eq!(resent["ok"], false);
    assert!(resent["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown field"));
}
