//! Byte-stable JSON emission with SUSHI-compatible property ordering. Phase 1/4.
//!
//! SUSHI serializes each resource with `JSON.stringify(obj, null, 2) + '\n'`
//! after running it through `orderedCloneDeep` (`common.ts:1571`), which moves
//! each `_x` "primitive sibling" key to sit immediately after its base key `x`
//! (orphan `_x` keys go last). Property order is otherwise the JS object
//! insertion order. We build resources as `serde_json::Value::Object` maps
//! (backed by `indexmap` via the `preserve_order` feature) in assignment order,
//! then emit with a 2-space pretty printer and a single trailing newline.

use serde_json::{Map, Value};

/// Port of `orderedCloneDeep` (`sushi-ts/src/fhirtypes/common.ts:1571`).
/// Recursively reorders object keys so each `_key` is glued directly after its
/// base `key`; orphan underscore keys keep their order and land at the end.
/// Arrays are never reordered. Non-objects are returned as-is.
pub fn ordered_clone_deep(input: &Value) -> Value {
    match input {
        Value::Array(items) => Value::Array(items.iter().map(ordered_clone_deep).collect()),
        Value::Object(map) => {
            // Partition keys into non-underscore (base) keys and underscore keys.
            let mut underscore: Vec<&String> =
                map.keys().filter(|k| k.starts_with('_')).collect();
            let base: Vec<&String> = map.keys().filter(|k| !k.starts_with('_')).collect();

            let mut out = Map::new();
            for k in base {
                out.insert(k.clone(), ordered_clone_deep(&map[k]));
                let under = format!("_{k}");
                if let Some(pos) = underscore.iter().position(|u| **u == under) {
                    out.insert(under.clone(), ordered_clone_deep(&map[&under]));
                    underscore.remove(pos);
                }
            }
            // Leftover orphan underscore keys, in original order.
            for u in underscore {
                out.insert(u.clone(), ordered_clone_deep(&map[u]));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Serialize to the exact textual form SUSHI writes to disk: 2-space indented
/// JSON (matching `JSON.stringify(obj, null, 2)`) terminated by one `'\n'`.
/// Runs `ordered_clone_deep` first to apply underscore-sibling gluing.
pub fn to_fhir_json_string(value: &Value) -> String {
    let ordered = ordered_clone_deep(value);
    let mut s = serde_json::to_string_pretty(&ordered).expect("serialize json");
    s.push('\n');
    s
}
