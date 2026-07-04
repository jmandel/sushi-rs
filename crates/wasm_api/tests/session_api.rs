//! Native coverage for the `Session` surface (the preferred, enveloped API that
//! collapses the accreted flat exports into one handle — simplification-ledger
//! #1). Two things are proven here:
//!   1. Session methods return the uniform envelope
//!      (`{ apiVersion, ok, op, result | error }`).
//!   2. The envelope's `result` payload is IDENTICAL to what the (still-present,
//!      deprecated) legacy free function returns — so migrating callers from the
//!      legacy surface to `Session` is a pure re-wrapping, no behavior change.
//!
//! `Session` methods return a JSON string (never a `JsError` for domain errors —
//! failures land as `ok:false`), so unlike expand_api.rs we never need the
//! JsError-panics-off-wasm dance.

#![allow(deprecated)] // the parity checks intentionally call the legacy exports too

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
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(ALPHA[((n >> 18) & 63) as usize] as char);
        out.push(ALPHA[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHA[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHA[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn synthetic_pkg(label: &str) -> Value {
    json!({ "label": label, "files": { ".index.json": base64(br#"{"files":[]}"#) } })
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
    assert!(err["error"]["message"].as_str().unwrap().contains("bad ValueSet JSON"));
    assert!(err.get("result").is_none());
}

#[test]
fn session_mount_is_additive_and_idempotent() {
    let s = Session::new();
    let mounted = |v: Value| v["result"]["mounted"].as_u64().unwrap();
    assert_eq!(mounted(parse(s.init(&json!([synthetic_pkg("m.a#1.0.0")]).to_string()))), 1);
    assert_eq!(mounted(parse(s.mount(&json!([synthetic_pkg("m.b#1.0.0")]).to_string()))), 2);
    // dup is skipped
    assert_eq!(mounted(parse(s.mount(&json!([synthetic_pkg("m.a#1.0.0")]).to_string()))), 2);
}

#[test]
fn session_expand_result_equals_legacy_payload() {
    // The Session envelope's `result` must equal the RAW payload the deprecated
    // `expand_enumerable` returns — migrating is pure re-wrapping.
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

    let legacy: Value = serde_json::from_str(
        &wasm_api::expand_enumerable(&vs.to_string(), &json!([cs]).to_string()).unwrap(),
    )
    .unwrap();

    assert_eq!(session_result, &legacy, "Session result must equal legacy payload");
    assert_eq!(session_result["expansion"]["total"], 1);
}

#[test]
fn session_resolve_result_equals_legacy_and_native() {
    // Session.resolveProject's envelope result must equal the legacy raw JSON,
    // which in turn equals the native package_store resolver (the #32 invariant).
    let pkgjson = |id: &str, deps: &[(&str, &str)]| {
        let d: serde_json::Map<String, Value> = deps
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
            .collect();
        json!({ "name": id, "version": "1.0.0", "fhirVersions": ["4.0.1"], "dependencies": d })
            .to_string()
    };
    let bundle = |label: &str, pkg: &str| {
        json!({ "label": label, "files": { "package.json": base64(pkg.as_bytes()) } })
    };
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

    // Legacy raw JSON == native resolver (already asserted in expand_api.rs; here
    // we bind the Session result to the legacy string too).
    let legacy: Value =
        serde_json::from_str(&wasm_api::resolve_project(config, &index).unwrap()).unwrap();
    assert_eq!(session_result, &legacy);

    let ctx = session_result["context_closure"].as_array().unwrap();
    let ids: Vec<&str> = ctx.iter().map(|r| r["package_id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"dep") && ids.contains(&"t") && ids.contains(&"hl7.fhir.r4.core"));
}

#[test]
fn session_version_is_stamped() {
    let v: Value = serde_json::from_str(&Session::version()).unwrap();
    assert_eq!(v["apiVersion"], 1);
    assert!(v["version"].is_string());
    assert!(v["engine"].as_str().unwrap().contains("rust_sushi"));
}

/// ContentApi: mountSite + renderLiquid (include from tree + data global +
/// markdownify filter) + renderMarkdown + renderPage/listPages over a tiny
/// mounted site. All through the Session envelopes — the same wire the editor
/// worker drives.
#[test]
fn session_content_api() {
    let s = Session::new();
    let site = serde_json::json!({
        "en/index.html": "---\n---\n<h1>{{ site.data.info.title }}</h1>{% include hello.xhtml %}",
        "_includes/hello.xhtml": "<p>hi {{ include.who }}{{ who }}</p>",
        "_data/info.json": "{\"title\":\"Smoke IG\"}",
    })
    .to_string();
    let env = parse(s.mount_site(&site, ""));
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(env["result"]["mounted"], 3);

    // listPages + renderPage (rel-path keys; front-matter gate inside render_page).
    let env = parse(s.list_pages());
    assert_eq!(env["result"]["pages"], serde_json::json!(["en/index.html"]));
    let env = parse(s.render_page("en/index.html"));
    assert_eq!(env["result"]["html"], "<h1>Smoke IG</h1><p>hi </p>");

    // renderLiquid: caller globals + tree include + markdownify filter.
    let env = parse(s.render_liquid(
        "{% include hello.xhtml %} — {{ note | markdownify }}",
        r#"{"who":"you","note":"*em*"}"#,
    ));
    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(
        env["result"]["html"],
        "<p>hi you</p> — <p><em>em</em></p>\n"
    );

    // renderMarkdown: kramdown semantics; rouge wrappers default ON.
    let env = parse(s.render_markdown("a `b` c", ""));
    assert_eq!(
        env["result"]["html"],
        "<p>a <code class=\"language-plaintext highlighter-rouge\">b</code> c</p>\n"
    );
    let env = parse(s.render_markdown("a `b` c", r#"{"rougeWrappers":false}"#));
    assert_eq!(env["result"]["html"], "<p>a <code>b</code> c</p>\n");

    // renderFragment on an unknown kind: typed domain error, not a throw.
    let env = parse(s.render_fragment("X-y", "nope"));
    assert_eq!(env["ok"], false);
}
