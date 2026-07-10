//! `api_envelope` — the ONE apiVersion result/error envelope.
//!
//! Both skins over the engine emit this shape:
//!   - the wasm `Session` (browser / Bun / Node), and
//!   - the `fig` CLI with `--json`.
//!
//! Success: `{ "apiVersion": 1, "ok": true,  "op": "<name>", "result": <payload> }`
//! Failure: `{ "apiVersion": 1, "ok": false, "op": "<name>", "error": { "message": "…" } }`
//!
//! Domain failures are `ok:false` envelopes, never thrown/panicked. One
//! implementation here means the CLI and the Session cannot drift; the
//! shared-schema test (`fig/tests/json_envelope.rs`) pins both against it.

use serde::Serialize;
use serde_json::Value;

/// The envelope SHAPE version. Bump only on a breaking change to the envelope
/// structure (not to any op's payload contents).
pub const API_VERSION: u32 = 1;

/// Serialize an op result (`Ok(payload)` / `Err(message)`) into the uniform
/// envelope string. Never panics: a serialize failure degrades to a hand-built
/// error envelope.
pub fn envelope(op: &str, result: Result<Value, String>) -> String {
    let v = match result {
        Ok(payload) => serde_json::json!({
            "apiVersion": API_VERSION,
            "ok": true,
            "op": op,
            "result": payload,
        }),
        Err(message) => serde_json::json!({
            "apiVersion": API_VERSION,
            "ok": false,
            "op": op,
            "error": { "message": message },
        }),
    };
    serde_json::to_string(&v).unwrap_or_else(|_| {
        format!(
            "{{\"apiVersion\":{API_VERSION},\"ok\":false,\"op\":\"{op}\",\
             \"error\":{{\"message\":\"envelope serialize failed\"}}}}"
        )
    })
}

/// Serialize a `T: Serialize` payload into the envelope (typed-payload path).
pub fn envelope_ser<T: Serialize>(op: &str, result: Result<T, String>) -> String {
    let as_value = result.and_then(|payload| {
        serde_json::to_value(&payload).map_err(|e| format!("{op}: serialize: {e}"))
    });
    envelope(op, as_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_shape() {
        let s = envelope("build", Ok(serde_json::json!({ "resources": 3 })));
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["apiVersion"], API_VERSION);
        assert_eq!(v["ok"], true);
        assert_eq!(v["op"], "build");
        assert_eq!(v["result"]["resources"], 3);
        assert!(v.get("error").is_none());
    }

    #[test]
    fn error_shape() {
        let s = envelope("snapshot", Err("no such profile".into()));
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["apiVersion"], API_VERSION);
        assert_eq!(v["ok"], false);
        assert_eq!(v["op"], "snapshot");
        assert_eq!(v["error"]["message"], "no such profile");
        assert!(v.get("result").is_none());
    }
}
