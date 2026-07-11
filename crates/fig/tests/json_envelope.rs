//! Shared-schema gate: the `fig --json` envelope and the
//! wasm `Session` envelope are the SAME schema — because they are the SAME
//! implementation (`api_envelope`). This test pins the shape from BOTH sides so
//! a drift in either fails here.

use serde_json::Value;

/// Run the built `fig` binary with args and parse its (last-line) JSON envelope.
fn fig_json(args: &[&str]) -> Value {
    let exe = env!("CARGO_BIN_EXE_fig");
    let out = std::process::Command::new(exe)
        .args(args)
        .output()
        .expect("run fig");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().last().unwrap_or("");
    serde_json::from_str(line).unwrap_or_else(|e| panic!("fig --json not JSON: {e}\n{stdout}"))
}

#[test]
fn fig_success_envelope_shape() {
    let v = fig_json(&["version", "--json"]);
    assert_eq!(v["apiVersion"], api_envelope::API_VERSION);
    assert_eq!(v["ok"], true);
    assert_eq!(v["op"], "version");
    assert!(v.get("result").is_some());
    assert!(v.get("error").is_none());
}

#[test]
fn fig_error_envelope_shape() {
    // A missing required arg → ok:false envelope (never a panic, never a throw).
    let v = fig_json(&["snapshot", "--json"]);
    assert_eq!(v["apiVersion"], api_envelope::API_VERSION);
    assert_eq!(v["ok"], false);
    assert_eq!(v["op"], "snapshot");
    assert!(v["error"]["message"].is_string());
    assert!(v.get("result").is_none());
}

#[test]
fn retired_template_only_package_bundle_is_not_an_alias_for_a_complete_bundle() {
    let v = fig_json(&[
        "packages",
        "bundle",
        "--template",
        "example.template#1.0.0",
        "--cache",
        "/unused",
        "--out",
        "/unused",
        "--json",
    ]);
    assert_eq!(v["ok"], false);
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown option --template"));
}

#[test]
fn fig_and_session_envelopes_are_schema_identical() {
    // The Session (wasm_api) and fig both emit via api_envelope::envelope. Prove
    // the produced JSON has the identical key set for both a success and an
    // error, from the actual functions each skin calls.
    let fig_ok = fig_json(&["version", "--json"]);
    let sess_ok: Value = {
        // Session's version() is the un-enveloped build-info; wrap it the way
        // every OTHER Session op wraps, via the shared envelope, to compare the
        // envelope schema itself.
        let s = api_envelope::envelope("version", Ok(serde_json::json!({ "x": 1 })));
        serde_json::from_str(&s).unwrap()
    };
    let keys = |v: &Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&fig_ok),
        keys(&sess_ok),
        "success envelope key sets differ"
    );

    let fig_err = fig_json(&["snapshot", "--json"]);
    let sess_err: Value =
        serde_json::from_str(&api_envelope::envelope("snapshot", Err("boom".into()))).unwrap();
    assert_eq!(
        keys(&fig_err),
        keys(&sess_err),
        "error envelope key sets differ"
    );
    assert_eq!(
        keys(&fig_err["error"]),
        keys(&sess_err["error"]),
        "error.message shape differs"
    );
}
