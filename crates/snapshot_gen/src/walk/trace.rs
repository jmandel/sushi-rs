//! Decision-trace emission mirroring the Java `SnapshotTracer` JSONL schema
//! (snapshot/specs/trace-schema.md). Zero overhead when disabled: the sink is a
//! thread-local `Option`. Enabled via `--trace <file>` / `SNAPSHOT_TRACE=<file>`.
//!
//! Records: `{"seq":N,"fn":..,"branch":..,"base":..,"diff":..,"x":{..}}`.
//! `seq` is a monotonic counter within one process run (one JSON object per line).

use serde_json::{Map, Value};
use std::cell::RefCell;
use std::io::Write;

thread_local! {
    static SINK: RefCell<Option<TraceSink>> = const { RefCell::new(None) };
}

struct TraceSink {
    file: std::fs::File,
    seq: u64,
}

/// True when a trace sink is installed. Callers gate `x`-payload construction on
/// this so the disabled path allocates nothing.
pub(crate) fn active() -> bool {
    SINK.with(|s| s.borrow().is_some())
}

/// Install a trace sink writing to `path` (truncating it). Returns an error if
/// the file cannot be created.
pub(crate) fn enable(path: &str) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    SINK.with(|s| *s.borrow_mut() = Some(TraceSink { file, seq: 0 }));
    Ok(())
}

pub(crate) fn disable() {
    SINK.with(|s| *s.borrow_mut() = None);
}

/// Emit one trace record. `base`/`diff` are element ids (falling back to path).
/// `x` is an optional extras object. No-op when disabled.
pub(crate) fn rec(
    func: &str,
    branch: &str,
    base: Option<&str>,
    diff: Option<&str>,
    x: Option<Value>,
) {
    SINK.with(|s| {
        let mut guard = s.borrow_mut();
        let Some(sink) = guard.as_mut() else {
            return;
        };
        let mut obj = Map::new();
        obj.insert("seq".to_string(), Value::from(sink.seq));
        obj.insert("fn".to_string(), Value::from(func));
        obj.insert("branch".to_string(), Value::from(branch));
        obj.insert(
            "base".to_string(),
            base.map(Value::from).unwrap_or(Value::Null),
        );
        obj.insert(
            "diff".to_string(),
            diff.map(Value::from).unwrap_or(Value::Null),
        );
        if let Some(x) = x {
            if !matches!(&x, Value::Object(m) if m.is_empty()) {
                obj.insert("x".to_string(), x);
            }
        }
        sink.seq += 1;
        let line = serde_json::to_string(&Value::Object(obj)).unwrap_or_default();
        let _ = writeln!(sink.file, "{line}");
    });
}

/// `SnapshotTracer.id(ed)`: element id, falling back to path.
pub(crate) fn id(ed: &Value) -> Option<String> {
    ed.get("id")
        .and_then(Value::as_str)
        .or_else(|| ed.get("path").and_then(Value::as_str))
        .map(str::to_string)
}
