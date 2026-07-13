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

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ApiSuccess<T> {
    #[cfg_attr(feature = "wire-contract", ts(type = "1"))]
    pub api_version: u32,
    #[cfg_attr(feature = "wire-contract", ts(type = "true"))]
    pub ok: bool,
    pub op: String,
    pub result: T,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ApiFailure<E> {
    #[cfg_attr(feature = "wire-contract", ts(type = "1"))]
    pub api_version: u32,
    #[cfg_attr(feature = "wire-contract", ts(type = "false"))]
    pub ok: bool,
    pub op: String,
    pub error: E,
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[serde(untagged)]
pub enum ApiEnvelope<T, E> {
    Success(ApiSuccess<T>),
    Failure(ApiFailure<E>),
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
pub struct ApiMessageError {
    pub message: String,
}

fn success<T, E>(op: &str, result: T) -> ApiEnvelope<T, E> {
    ApiEnvelope::Success(ApiSuccess {
        api_version: API_VERSION,
        ok: true,
        op: op.to_string(),
        result,
    })
}

fn failure<T, E>(op: &str, error: E) -> ApiEnvelope<T, E> {
    ApiEnvelope::Failure(ApiFailure {
        api_version: API_VERSION,
        ok: false,
        op: op.to_string(),
        error,
    })
}

/// Serialize an op result (`Ok(payload)` / `Err(message)`) into the uniform
/// envelope string. Never panics: a serialize failure degrades to a hand-built
/// error envelope.
pub fn envelope(op: &str, result: Result<Value, String>) -> String {
    let value: ApiEnvelope<Value, ApiMessageError> = match result {
        Ok(payload) => success(op, payload),
        Err(message) => failure(op, ApiMessageError { message }),
    };
    serde_json::to_string(&value).unwrap_or_else(|_| {
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

/// Serialize a typed error object without collapsing its phase/code/context to
/// a message. The outer envelope remains identical; only the operation's error
/// vocabulary is richer.
pub fn envelope_typed<T: Serialize, E: Serialize>(op: &str, result: Result<T, E>) -> String {
    let value: Result<ApiEnvelope<Value, Value>, serde_json::Error> = match result {
        Ok(payload) => serde_json::to_value(payload).map(|result| success(op, result)),
        Err(error) => serde_json::to_value(error).map(|error| failure(op, error)),
    };
    match value {
        Ok(value) => serde_json::to_string(&value),
        Err(_) => serde_json::to_string(&serde_json::json!({
            "apiVersion": API_VERSION,
            "ok": false,
            "op": op,
            "error": { "message": "envelope serialize failed" },
        })),
    }
    .unwrap_or_else(|_| {
        format!(
            "{{\"apiVersion\":{API_VERSION},\"ok\":false,\"op\":\"{op}\",\
             \"error\":{{\"message\":\"envelope serialize failed\"}}}}"
        )
    })
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

    #[test]
    fn typed_error_shape_preserves_fields() {
        #[derive(Serialize)]
        struct TypedError {
            message: &'static str,
            code: &'static str,
        }
        let value: Value = serde_json::from_str(&envelope_typed::<Value, _>(
            "prepare",
            Err(TypedError {
                message: "failed",
                code: "compile-failed",
            }),
        ))
        .unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "compile-failed");
    }
}
