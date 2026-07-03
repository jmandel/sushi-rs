//! §1.3 post-walk finalize (Q-steps). Live under CONSTRAINT/oracle config:
//! Q3 prune prohibited type-slices, Q6 setIds, Q7 PC-1 unconsumed-diff check,
//! Q14 EXT_VERSION_BASE. (Q1 group-constraints, Q10 slice cardinality, Q11 ref
//! checks are message-only / not gated by the ladder — noted where skipped.)

use serde_json::Value;

use super::context::{Severity, WalkContext};
use super::ids::generate_ids_with_type;
use super::paths::path_of;
use super::trace;

/// Q6 + Q7 + Q14. `base_version` is the base SD's `version` for EXT_VERSION_BASE.
pub(crate) fn finalize(
    ctx: &mut WalkContext,
    derived: &mut Value,
    base_version: Option<&str>,
) -> anyhow::Result<()> {
    // Q6 setIds on the snapshot.
    let type_name = derived.get("type").and_then(Value::as_str).map(str::to_string);
    generate_ids_with_type(&mut ctx.output, type_name.as_deref());

    // Q7 PC-1 unconsumed-differential check.
    let mut unconsumed_messages: Vec<(String, String)> = Vec::new();
    {
        let diff = ctx.diff.clone();
        for (i, consumed) in ctx.diff_consumed.iter().enumerate() {
            if *consumed {
                continue;
            }
            if ctx.diff_injected.get(i).copied().unwrap_or(false) {
                continue;
            }
            let ed = &diff[i];
            let has_id = ed.get("id").and_then(Value::as_str).is_some();
            if trace::active() {
                trace::rec(
                    "generateSnapshot",
                    "generateSnapshot.diffNotConsumed",
                    None,
                    trace::id(ed).as_deref(),
                    Some(serde_json::json!({
                        "diffIndex": i,
                        "hasId": has_id,
                        "path": path_of(ed),
                    })),
                );
            }
            if has_id {
                let id = ed.get("id").and_then(Value::as_str).unwrap();
                unconsumed_messages.push((
                    format!("StructureDefinition.differential.element[{i}]"),
                    format!(
                        "No match found for {id} in the generated snapshot: check that the path and definitions are legal in the differential (including order)"
                    ),
                ));
            }
        }
    }
    for (path, text) in unconsumed_messages {
        ctx.add_message(Severity::Error, &path, text);
    }

    // Move the built snapshot onto the derived SD.
    let obj = derived
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("derived is not an object"))?;
    let mut snapshot = serde_json::Map::new();
    snapshot.insert(
        "element".to_string(),
        Value::Array(std::mem::take(&mut ctx.output)),
    );

    // Q14 EXT_VERSION_BASE stamp on the snapshot.
    if let Some(version) = base_version {
        if !version.is_empty() {
            snapshot.insert(
                "extension".to_string(),
                serde_json::json!([{
                    "url": "http://hl7.org/fhir/StructureDefinition/structuredefinition-base-version",
                    "valueString": version
                }]),
            );
        }
    }

    // Reorder so snapshot follows differential like the goldens.
    let differential = obj.remove("differential");
    obj.insert("snapshot".to_string(), Value::Object(snapshot));
    if let Some(differential) = differential {
        obj.insert("differential".to_string(), differential);
    }
    Ok(())
}
