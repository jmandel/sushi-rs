//! §1.3 post-walk finalize (Q-steps). Live under CONSTRAINT/oracle config:
//! Q3 prune prohibited type-slices, Q6 setIds, Q7 PC-1 unconsumed-diff check,
//! Q14 EXT_VERSION_BASE. (Q1 group-constraints, Q10 slice cardinality, Q11 ref
//! checks are message-only / not gated by the ladder — noted where skipped.)

use serde_json::Value;
use std::collections::HashMap;

use super::context::{Severity, WalkContext};
use super::ids::generate_ids_with_type;
use super::paths::path_of;
use super::trace;

/// PU:879-891 — for each element with >1 type, drop the type entries whose
/// downstream type slice (same path, single matching type code) is prohibited
/// (`max == "0"`). Mirrors findTypeSlice/pathMatches/typeMatches/prohibited.
fn prune_prohibited_type_slices(ctx: &mut WalkContext) {
    fn working_code(tr: &Value) -> Option<String> {
        super::types_pred::working_code(tr)
    }
    // pathMatches: exact, or `[x]` base matched by a concrete single-segment tail.
    fn path_matches(anchor_path: &str, ed_path: &str) -> bool {
        if anchor_path == ed_path {
            return true;
        }
        if let Some(stem) = anchor_path.strip_suffix("[x]") {
            if ed_path.starts_with(stem)
                && ed_path.len() > stem.len()
                && !ed_path[stem.len()..].contains('.')
            {
                return true;
            }
        }
        false
    }
    // typeMatches: the ed has exactly one type whose working code == typeCode.
    fn type_matches(ed: &Value, type_code: &str) -> bool {
        let types = ed.get("type").and_then(Value::as_array);
        matches!(types, Some(a) if a.len() == 1 && working_code(&a[0]).as_deref() == Some(type_code))
    }

    let n = ctx.output.len();
    let mut removals: Vec<(usize, Vec<String>)> = Vec::new();
    for i in 0..n {
        let ed = &ctx.output[i];
        let Some(types) = ed.get("type").and_then(Value::as_array) else {
            continue;
        };
        if types.len() <= 1 {
            continue;
        }
        let path = path_of(ed).to_string();
        let mut drop_codes: Vec<String> = Vec::new();
        for tr in types {
            let Some(code) = working_code(tr) else {
                continue;
            };
            // findTypeSlice: scan forward for a matching prohibited type slice.
            for j in (i + 1)..n {
                let cand = &ctx.output[j];
                if path_matches(&path, path_of(cand)) && type_matches(cand, &code) {
                    if cand.get("max").and_then(Value::as_str) == Some("0") {
                        drop_codes.push(code.clone());
                    }
                    break;
                }
            }
        }
        if !drop_codes.is_empty() {
            removals.push((i, drop_codes));
        }
    }
    for (i, drop_codes) in removals {
        if let Some(types) = ctx.output[i].get_mut("type").and_then(Value::as_array_mut) {
            types.retain(|tr| {
                super::types_pred::working_code(tr)
                    .map(|c| !drop_codes.contains(&c))
                    .unwrap_or(true)
            });
        }
    }
}

/// PU:996-1050 slice-cardinality pass. Overwrites an auto-added slicing anchor's
/// `min` with the sum of its slice mins when that sum exceeds the anchor min and
/// the slice repeats. Mirrors ElementDefinitionCounter + the close-on-dedent walk.
fn apply_slice_cardinality(ctx: &mut WalkContext) {
    /// One open slice group (ElementDefinitionCounter).
    struct SliceCounter {
        anchor_idx: usize,
        anchor_min: i64,
        base_max_is_one: bool,
        auto_added: bool,
        count_min: i64,
    }
    fn char_count(s: &str, c: char) -> usize {
        s.chars().filter(|&x| x == c).count()
    }
    fn min_of(ed: &Value) -> i64 {
        ed.get("min").and_then(Value::as_i64).unwrap_or(0)
    }
    // path -> open counter
    let mut slices: HashMap<String, SliceCounter> = HashMap::new();
    // Deferred min overwrites: (output_idx, new_min).
    let mut overwrites: Vec<(usize, i64)> = Vec::new();

    for i in 0..ctx.output.len() {
        let ed = &ctx.output[i];
        let path = path_of(ed).to_string();
        let has_slicing = ed.get("slicing").is_some();
        let has_slice_name = ed.get("sliceName").and_then(Value::as_str).is_some();
        let ed_min = min_of(ed);
        if has_slicing {
            let base_max = ed
                .get("base")
                .and_then(|b| b.get("max"))
                .and_then(Value::as_str)
                .unwrap_or("");
            slices.insert(
                path.clone(),
                SliceCounter {
                    anchor_idx: i,
                    anchor_min: ed_min,
                    base_max_is_one: base_max == "1",
                    auto_added: ctx.output_ann[i].auto_added_slicing,
                    count_min: 0,
                },
            );
        } else {
            // Close any open group whose path is at the same or deeper dot-depth
            // and is not exactly this path.
            let ed_dots = char_count(&path, '.');
            let to_remove: Vec<String> = slices
                .keys()
                .filter(|s| char_count(s, '.') >= ed_dots && *s != &path)
                .cloned()
                .collect();
            for s in to_remove {
                let slice = slices.remove(&s).unwrap();
                // checkMin(): countMin if countMin > anchor min, else -1.
                if slice.count_min > slice.anchor_min {
                    let repeats = !slice.base_max_is_one;
                    if repeats && slice.auto_added {
                        overwrites.push((slice.anchor_idx, slice.count_min));
                    }
                }
            }
        }
        // Count this row into its slice group (PU:1044 — runs for every row that
        // has a sliceName and an open group at its path, including a slicing
        // anchor that also carries a sliceName).
        if has_slice_name {
            if let Some(sc) = slices.get_mut(&path) {
                sc.count_min += ed_min;
            }
        }
    }

    for (idx, new_min) in overwrites {
        if let Some(obj) = ctx.output[idx].as_object_mut() {
            obj.insert("min".to_string(), Value::from(new_min));
        }
    }
}

/// Q6 + Q7 + Q14. `base_version` is the base SD's `version` for EXT_VERSION_BASE.
pub(crate) fn finalize(
    ctx: &mut WalkContext,
    derived: &mut Value,
    base_version: Option<&str>,
) -> anyhow::Result<()> {
    // Q8 (PU:964-983): trim mapping.map whitespace; absolutize relative
    // constraint.source references.
    for ed in ctx.output.iter_mut() {
        if let Some(mappings) = ed.get_mut("mapping").and_then(Value::as_array_mut) {
            for mm in mappings {
                if let Some(m) = mm.get("map").and_then(Value::as_str) {
                    let trimmed = m.trim();
                    if trimmed.len() != m.len() {
                        let trimmed = trimmed.to_string();
                        if let Some(obj) = mm.as_object_mut() {
                            obj.insert("map".to_string(), Value::String(trimmed));
                        }
                    }
                }
            }
        }
        if let Some(constraints) = ed.get_mut("constraint").and_then(Value::as_array_mut) {
            for c in constraints {
                let Some(src) = c.get("source").and_then(Value::as_str) else {
                    continue;
                };
                let is_absolute = src.contains("://") || src.starts_with("urn:");
                if !is_absolute {
                    let new_src = if let Some(dot) = src.find('.') {
                        format!(
                            "http://hl7.org/fhir/StructureDefinition/{}#{src}",
                            &src[..dot]
                        )
                    } else {
                        format!("http://hl7.org/fhir/StructureDefinition/{src}")
                    };
                    if let Some(obj) = c.as_object_mut() {
                        obj.insert("source".to_string(), Value::String(new_src));
                    }
                }
            }
        }
    }

    // Q3 (PU:879-891): prune a polymorphic anchor's `type[]` entries whose
    // matching type slice is prohibited (max=0). Runs before setIds.
    prune_prohibited_type_slices(ctx);

    // Q6 setIds on the snapshot.
    let type_name = derived
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string);
    generate_ids_with_type(&mut ctx.output, type_name.as_deref());

    // Q10 slice cardinality (PU:996-1050): walk the snapshot tracking open slice
    // groups; when a group closes, overwrite the anchor `min` with the sum of its
    // slice mins — but ONLY when the anchor's slicing was auto-added
    // (SNAPSHOT_auto_added_slicing) and the slice repeats (base.max != "1", i.e.
    // not type-slicing). Otherwise Java only emits a (message-only) warning.
    apply_slice_cardinality(ctx);

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
                    "url": "http://hl7.org/fhir/tools/StructureDefinition/snapshot-base-version",
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
