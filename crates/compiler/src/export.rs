//! ValueSet / CodeSystem export (Phase 4).
//!
//! Ports `sushi-ts/src/export/ValueSetExporter.ts` and `CodeSystemExporter.ts`
//! to produce byte-identical `ValueSet-*.json` / `CodeSystem-*.json`. Resource
//! bodies are built as ordered `serde_json` objects in the exact assignment
//! order SUSHI uses (constructor-initialized fields, then setMetadata, then
//! caret rules / compose / concepts), then emitted via `json_emit`.
//!
//! The caret-value application engine here is a focused port of
//! `setInstancePropertyByPath` / `setPropertyOnInstance` (`common.ts`). Instead
//! of fishing the real ValueSet/CodeSystem StructureDefinition (Phase 5), it
//! uses a small embedded element-type table covering the elements + datatypes
//! that VS/CS metadata caret rules actually touch. This keeps VS/CS export
//! self-contained (no FHIR packages needed).

use crate::config::Config;
use crate::type_resolver::TypeResolver;
use fsh_model::{
    FilterValue, FshCode, FshCodeSystem, FshDocument, FshValueSet, Rule, Value as FshValue,
    ValueSetComponentFrom,
};
use package_store::{FishType, PackageStore};
use serde_json::{Map, Value as J};

/// `fisher.fishForMetadata(name, ty)?.url` against the FHIR packages — the
/// fallback when a compose system/valueset isn't a LOCAL CS/VS (e.g. a bare
/// external name like `ConditionCategoryCodes` resolved from THO).
fn pkg_url(store: Option<&PackageStore>, name: &str, ty: FishType) -> Option<String> {
    let base = name.split('|').next().unwrap_or(name);
    store?
        .fish_for_metadata(base, &[ty])
        .and_then(|m| m.get("url").and_then(|u| u.as_str()).map(String::from))
}

// ---------------------------------------------------------------------------
// Tank fisher: resolve local ValueSet/CodeSystem names/ids/urls to their url.
// ---------------------------------------------------------------------------

/// Index of local VS/CS metadata for `fishForMetadata`-style url resolution.
pub struct TankIndex {
    /// name|id|url -> canonical url, for CodeSystems.
    code_systems: Vec<(Vec<String>, String)>,
    /// name|id|url -> canonical url, for ValueSets.
    value_sets: Vec<(Vec<String>, String)>,
}

impl TankIndex {
    pub(crate) fn build(docs: &[FshDocument], cfg: &Config) -> TankIndex {
        let mut code_systems = Vec::new();
        let mut value_sets = Vec::new();
        for doc in docs {
            for (_k, cs) in &doc.code_systems {
                let id = effective_id(&cs.rules, &cs.id);
                // G14/G11: a `* ^url = ...` caret overrides the default canonical
                // (stock fishes the CodeSystem's declared url, not the derived one).
                let url = effective_url(&cs.rules)
                    .unwrap_or_else(|| format!("{}/CodeSystem/{}", cfg.canonical, id));
                code_systems.push((vec![cs.name.clone(), id, url.clone()], url));
            }
            for (_k, vs) in &doc.value_sets {
                let id = effective_id(&vs.rules, &vs.id);
                let url = effective_url(&vs.rules)
                    .unwrap_or_else(|| format!("{}/ValueSet/{}", cfg.canonical, id));
                value_sets.push((vec![vs.name.clone(), id, url.clone()], url));
            }
        }
        // Instance-defined conformance ValueSets/CodeSystems (`InstanceOf: ValueSet`
        // / `CodeSystem` with `Usage` other than Inline) are fishable as their
        // resource type. Stock's MasterFisher.fixMetadata synthesizes a url
        // `{canonical}/{resourceType}/{id}` when the instance has no explicit
        // `* url`. Added after keyword-defined VS/CS so those win on name clash,
        // matching FSHTank.fish (entities before instances).
        for doc in docs {
            for (_k, inst) in &doc.instances {
                let Some(instance_of) = inst.instance_of.as_deref() else { continue };
                let (target, fhir_type) = match instance_of {
                    "ValueSet" => (&mut value_sets, "ValueSet"),
                    "CodeSystem" => (&mut code_systems, "CodeSystem"),
                    _ => continue,
                };
                if inst.usage == "Inline" {
                    continue;
                }
                let id = instance_effective_id(inst);
                let url = instance_assigned_url(inst)
                    .unwrap_or_else(|| format!("{}/{}/{}", cfg.canonical, fhir_type, id));
                target.push((vec![inst.name.clone(), id, url.clone()], url));
            }
        }
        TankIndex {
            code_systems,
            value_sets,
        }
    }

    /// `fisher.fishForMetadata(system, Type.CodeSystem)?.url` (first hit, version
    /// stripped). Returns `None` if not a local CodeSystem.
    pub(crate) fn cs_url(&self, system: &str) -> Option<String> {
        let base = system.split('|').next().unwrap_or(system);
        self.code_systems
            .iter()
            .find(|(keys, _)| keys.iter().any(|k| k == base))
            .map(|(_, url)| url.clone())
    }

    /// `fishForMetadataBestVersion(vs, Type.ValueSet)?.url`.
    pub(crate) fn vs_url(&self, vs: &str) -> Option<String> {
        let base = vs.split('|').next().unwrap_or(vs);
        self.value_sets
            .iter()
            .find(|(keys, _)| keys.iter().any(|k| k == base))
            .map(|(_, url)| url.clone())
    }
}

/// Effective instance id: the last `* id = "..."` AssignmentRule's value, else
/// the declared id (which defaults to the instance name).
fn instance_effective_id(inst: &fsh_model::Instance) -> String {
    for r in inst.rules.iter().rev() {
        if let Rule::Assignment { path, value: Some(FshValue::Str(s)), .. } = r {
            if path == "url" {
                continue;
            }
            if path == "id" {
                return s.clone();
            }
        }
    }
    inst.id.clone()
}

/// An explicit `* url = "..."` AssignmentRule on an instance, if present
/// (mirrors `getNonInstanceValueFromRules(entity, 'url')`).
fn instance_assigned_url(inst: &fsh_model::Instance) -> Option<String> {
    for r in inst.rules.iter().rev() {
        if let Rule::Assignment { path, value: Some(FshValue::Str(s)), .. } = r {
            if path == "url" {
                return Some(s.clone());
            }
        }
    }
    None
}

/// Recompute the effective `id` (`FshValueSet.get id()` / `FshCodeSystem`):
/// `findLast` non-instance `^id` CaretValueRule, else the declared id.
pub(crate) fn effective_id(rules: &[Rule], declared: &str) -> String {
    for r in rules.iter().rev() {
        if let Rule::CaretValue {
            path,
            caret_path,
            value,
            is_instance,
            ..
        } = r
        {
            if path.is_empty()
                && caret_path.as_deref() == Some("id")
                && !is_instance
            {
                if let Some(FshValue::Str(s)) = value {
                    return s.clone();
                }
            }
        }
    }
    declared.to_string()
}

/// Effective canonical url from a `* ^url = "..."` caret override (findLast,
/// non-instance, root path), or `None` to fall back to the derived default.
pub(crate) fn effective_url(rules: &[Rule]) -> Option<String> {
    for r in rules.iter().rev() {
        if let Rule::CaretValue {
            path,
            caret_path,
            value,
            is_instance,
            ..
        } = r
        {
            if path.is_empty()
                && caret_path.as_deref() == Some("url")
                && !is_instance
            {
                if let Some(FshValue::Str(s)) = value {
                    return Some(s.clone());
                }
            }
        }
    }
    None
}


// ---------------------------------------------------------------------------
// Caret path parsing + application.
// ---------------------------------------------------------------------------

pub(crate) struct Seg {
    pub key: String,
    pub array: bool,
    pub slice_url: Option<String>,
    pub index: Option<usize>,
}

/// Split a caret path on `.` that are outside `[...]` brackets.
pub(crate) fn split_caret_path(path: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in path.chars() {
        match c {
            '[' => {
                depth += 1;
                cur.push(c);
            }
            ']' => {
                depth -= 1;
                cur.push(c);
            }
            '.' if depth == 0 => {
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}


/// Port of the `FshCode` branch of `replaceReferences` (`fhirtypes/common.ts`):
/// fish the system name (the part before any `|version`) as a CodeSystem and, if
/// found, substitute its canonical url while preserving the version suffix
/// (`value.system.replace(/^[^|]+/, codeSystemMeta.url)`). Unresolvable systems —
/// including bare names with no matching CodeSystem and systems that are already
/// urls of no known CodeSystem — are left untouched. `resolve_cs` mirrors
/// `fishForMetadata(base, Type.CodeSystem)?.url` (local CodeSystems first, then
/// dependency packages). Returns `Some(new_system)` only when resolution changes
/// (or confirms) the system; `None` when nothing was found.
pub(crate) fn replace_code_system(
    system: &str,
    resolve_cs: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let base = system.split('|').next().unwrap_or(system);
    let url = resolve_cs(base)?;
    Some(system.replacen(base, &url, 1))
}

/// Build a FHIR Coding JSON object from an FshCode (key order: code, system,
/// version, display) — mirrors `FshCode.toFHIRCoding`.
pub(crate) fn coding_from(fc: &FshCode) -> J {
    let mut m = Map::new();
    if !fc.code.is_empty() {
        m.insert("code".into(), J::String(fc.code.clone()));
    }
    if let Some(sys) = &fc.system {
        if let Some(idx) = sys.find('|') {
            m.insert("system".into(), J::String(sys[..idx].to_string()));
            m.insert("version".into(), J::String(sys[idx + 1..].to_string()));
        } else {
            m.insert("system".into(), J::String(sys.clone()));
        }
    }
    if let Some(disp) = &fc.display {
        m.insert("display".into(), J::String(disp.clone()));
    }
    J::Object(m)
}

/// Coerce an FSH caret value to JSON according to the resolved leaf element type
/// (port of the relevant `assignValue` / `assignFshCode` branches).
pub(crate) fn coerce(value: &FshValue, leaf_ty: &str, resolver: &TypeResolver) -> Option<J> {
    if resolver.is_primitive(leaf_ty) {
        Some(match value {
            // For a code/string/uri leaf, a FshCode contributes only its code.
            FshValue::Code(fc) => J::String(fc.code.clone()),
            FshValue::Str(s) => J::String(s.clone()),
            FshValue::Bool(b) => J::Bool(*b),
            FshValue::BigInt(s) => {
                if let Ok(i) = s.parse::<i64>() {
                    J::Number(i.into())
                } else if let Ok(u) = s.parse::<u64>() {
                    J::Number(u.into())
                } else {
                    J::String(s.clone())
                }
            }
            FshValue::Float(f) => serde_json::Number::from_f64(*f)
                .map(J::Number)
                .unwrap_or(J::Null),
            FshValue::Canonical(c) => J::String(c.entity_name.clone()),
            _ => return None,
        })
    } else if resolver.is_complex(leaf_ty) {
        match (leaf_ty, value) {
            ("Coding", FshValue::Code(fc)) => Some(coding_from(fc)),
            ("CodeableConcept", FshValue::Code(fc)) => {
                let mut m = Map::new();
                m.insert("coding".into(), J::Array(vec![coding_from(fc)]));
                Some(J::Object(m))
            }
            _ => None,
        }
    } else {
        None
    }
}

pub(crate) fn set_value(target: &mut J, leaf: J) {
    if target.is_object() && leaf.is_object() {
        let (J::Object(t), J::Object(l)) = (target, leaf) else {
            unreachable!()
        };
        for (k, v) in l {
            t.insert(k, v);
        }
    } else {
        *target = leaf;
    }
}

pub(crate) fn apply(obj: &mut Map<String, J>, segs: &[Seg], leaf: J) {
    let seg = &segs[0];
    let last = segs.len() == 1;
    if seg.array {
        let arr = obj
            .entry(seg.key.clone())
            .or_insert_with(|| J::Array(vec![]));
        if !arr.is_array() {
            *arr = J::Array(vec![]);
        }
        let arr = arr.as_array_mut().unwrap();
        let idx = if let Some(url) = &seg.slice_url {
            // n-th occurrence of this url (default 0); create entries as needed.
            let want = seg.index.unwrap_or(0);
            let positions: Vec<usize> = arr
                .iter()
                .enumerate()
                .filter(|(_, e)| e.get("url") == Some(&J::String(url.clone())))
                .map(|(i, _)| i)
                .collect();
            if let Some(&p) = positions.get(want) {
                p
            } else {
                let mut m = Map::new();
                m.insert("url".into(), J::String(url.clone()));
                arr.push(J::Object(m));
                arr.len() - 1
            }
        } else {
            let n = seg.index.unwrap_or(0);
            while arr.len() <= n {
                arr.push(J::Null);
            }
            n
        };
        if last {
            set_value(&mut arr[idx], leaf);
        } else {
            if !arr[idx].is_object() {
                arr[idx] = J::Object(Map::new());
            }
            apply(arr[idx].as_object_mut().unwrap(), &segs[1..], leaf);
        }
    } else if last {
        match obj.get_mut(&seg.key) {
            Some(existing) if existing.is_object() && leaf.is_object() => {
                set_value(existing, leaf)
            }
            _ => {
                obj.insert(seg.key.clone(), leaf);
            }
        }
    } else {
        let child = obj
            .entry(seg.key.clone())
            .or_insert_with(|| J::Object(Map::new()));
        if !child.is_object() {
            *child = J::Object(Map::new());
        }
        apply(child.as_object_mut().unwrap(), &segs[1..], leaf);
    }
}

/// Apply one top-level caret rule (`path == ''`) onto the resource object.
fn apply_caret(
    obj: &mut Map<String, J>,
    resource_type: &str,
    caret_path: &str,
    value: &FshValue,
    resolver: &TypeResolver,
) {
    // Resolve aliases inside path brackets (e.g. `^extension[FMM]` where FMM is a
    // global Alias) — same export-time resolution the SD exporter does.
    let caret_path = crate::sd_export::resolve_caret_aliases(caret_path);
    let caret_path = caret_path.as_str();
    let Some((segs, leaf_ty)) = resolver.resolve(resource_type, caret_path) else {
        return;
    };
    let Some(leaf) = coerce(value, &leaf_ty, resolver) else {
        return;
    };
    apply(obj, &segs, leaf);
}

/// Pre-pass mirroring stock SUSHI's `setImpliedPropertiesOnInstance`, which runs
/// BEFORE the caret value-assignment loop (`ValueSetExporter.setCaretRules`,
/// `CodeSystemExporter.setCaretPathRules`). It materializes the *implied* (fixed)
/// values that a caret path entails — for VS/CS metadata carets the only such
/// implied value is an `extension`/`modifierExtension` slice's fixed `url`. By
/// creating those entries here, the `extension` top-level key is inserted in
/// element order (early), ahead of later metadata caret keys like `copyright`/
/// `experimental` — even when the extension caret rule appears AFTER them in
/// source. Without this, key insertion order would follow raw rule order and
/// diverge from stock (e.g. mCODE/CRD `^copyright`/`^experimental` set by an
/// inserted RuleSet, followed by `^extension[FMM]`).
fn precreate_implied(obj: &mut Map<String, J>, segs: &[Seg]) {
    // Only materialize paths that carry at least one extension-slice url; other
    // VS/CS metadata carets have no implied (fixed) child values.
    if !segs.iter().any(|s| s.slice_url.is_some()) {
        return;
    }
    let seg = &segs[0];
    let remaining_has_slice = segs.len() > 1 && segs[1..].iter().any(|s| s.slice_url.is_some());
    if seg.array {
        if let Some(url) = &seg.slice_url {
            let arr = obj
                .entry(seg.key.clone())
                .or_insert_with(|| J::Array(vec![]));
            if !arr.is_array() {
                *arr = J::Array(vec![]);
            }
            let arr = arr.as_array_mut().unwrap();
            let want = seg.index.unwrap_or(0);
            let positions: Vec<usize> = arr
                .iter()
                .enumerate()
                .filter(|(_, e)| e.get("url") == Some(&J::String(url.clone())))
                .map(|(i, _)| i)
                .collect();
            let idx = if let Some(&p) = positions.get(want) {
                p
            } else {
                let mut m = Map::new();
                m.insert("url".into(), J::String(url.clone()));
                arr.push(J::Object(m));
                arr.len() - 1
            };
            if remaining_has_slice {
                if !arr[idx].is_object() {
                    arr[idx] = J::Object(Map::new());
                }
                precreate_implied(arr[idx].as_object_mut().unwrap(), &segs[1..]);
            }
        } else if remaining_has_slice {
            // Non-slice array segment with a deeper slice: descend to reach it.
            let n = seg.index.unwrap_or(0);
            let arr = obj
                .entry(seg.key.clone())
                .or_insert_with(|| J::Array(vec![]));
            if !arr.is_array() {
                *arr = J::Array(vec![]);
            }
            let arr = arr.as_array_mut().unwrap();
            while arr.len() <= n {
                arr.push(J::Null);
            }
            if !arr[n].is_object() {
                arr[n] = J::Object(Map::new());
            }
            precreate_implied(arr[n].as_object_mut().unwrap(), &segs[1..]);
        }
    } else if remaining_has_slice {
        let child = obj
            .entry(seg.key.clone())
            .or_insert_with(|| J::Object(Map::new()));
        if !child.is_object() {
            *child = J::Object(Map::new());
        }
        precreate_implied(child.as_object_mut().unwrap(), &segs[1..]);
    }
}

/// Run the implied-properties pre-pass for one caret rule path.
fn precreate_implied_for_path(
    obj: &mut Map<String, J>,
    resource_type: &str,
    caret_path: &str,
    resolver: &TypeResolver,
) {
    let caret_path = crate::sd_export::resolve_caret_aliases(caret_path);
    if let Some((segs, _)) = resolver.resolve(resource_type, caret_path.as_str()) {
        precreate_implied(obj, &segs);
    }
}

// ---------------------------------------------------------------------------
// ValueSet export.
// ---------------------------------------------------------------------------

/// One exported resource: (filename, ordered JSON body).
pub struct Exported {
    pub filename: String,
    pub body: J,
}

pub fn export_value_set(
    vs: &FshValueSet,
    cfg: &Config,
    tank: &TankIndex,
    store: Option<&PackageStore>,
    resolver: &TypeResolver,
) -> Exported {
    let id = effective_id(&vs.rules, &vs.id);
    let mut obj: Map<String, J> = Map::new();

    // Constructor-initialized field order: resourceType, status.
    obj.insert("resourceType".into(), J::String("ValueSet".into()));
    obj.insert("status".into(), J::String(cfg.status().into()));
    // setMetadata: name, id, title, description, [version], status(set), url.
    obj.insert("name".into(), J::String(vs.name.clone()));
    obj.insert("id".into(), J::String(id.clone()));
    if let Some(t) = &vs.title {
        if !t.is_empty() {
            obj.insert("title".into(), J::String(t.clone()));
        }
    }
    if let Some(d) = &vs.description {
        if !d.is_empty() {
            obj.insert("description".into(), J::String(d.clone()));
        }
    }
    if cfg.fsh_only {
        if let Some(v) = &cfg.version {
            obj.insert("version".into(), J::String(v.clone()));
        }
    }
    obj.insert(
        "url".into(),
        J::String(format!("{}/ValueSet/{}", cfg.canonical, id)),
    );

    // Resolve `[+]`/`[=]` soft indices on caret paths (e.g. `^useContext[+]`,
    // `^extension[=]`), exactly as the SD/instance exporters do. Without this a
    // `[=]` would be emitted literally as a slice url.
    let mut resolved_rules = vs.rules.clone();
    crate::paths::resolve_soft_indexing(&mut resolved_rules);

    // partition caret rules: concept-level (pathArray non-empty) vs other.
    let other_carets: Vec<&Rule> = resolved_rules
        .iter()
        .filter(|r| matches!(r, Rule::CaretValue { path_array, .. } if path_array.is_empty()))
        .collect();

    // setImpliedPropertiesOnInstance pre-pass: create extension/modifierExtension
    // slice urls (the only implied/fixed values for VS metadata carets) BEFORE the
    // value loop, so the `extension` key lands in element order ahead of later
    // metadata caret keys regardless of source rule order.
    for r in &other_carets {
        if let Rule::CaretValue {
            caret_path: Some(cp),
            value: Some(_),
            is_instance: false,
            ..
        } = r
        {
            precreate_implied_for_path(&mut obj, "ValueSet", cp, resolver);
        }
    }

    // setCaretRules (otherCaretRules) in source order.
    for r in &other_carets {
        if let Rule::CaretValue {
            caret_path,
            value: Some(value),
            is_instance: false,
            ..
        } = r
        {
            if let Some(cp) = caret_path {
                apply_caret(&mut obj, "ValueSet", cp, value, resolver);
            }
        }
    }

    // setCompose.
    set_compose(&mut obj, vs, tank, store);

    Exported {
        filename: format!("ValueSet-{}.json", id),
        body: J::Object(obj),
    }
}

fn from_to_compose_element(
    from: &ValueSetComponentFrom,
    tank: &TankIndex,
    vs_url: &str,
    store: Option<&PackageStore>,
) -> Map<String, J> {
    let mut ce: Map<String, J> = Map::new();
    if let Some(system) = &from.system {
        let system_parts: Vec<&str> = system.split('|').collect();
        let resolved = tank
            .cs_url(system)
            .or_else(|| pkg_url(store, system, FishType::CodeSystem))
            .unwrap_or_else(|| system_parts[0].to_string());
        ce.insert("system".into(), J::String(resolved));
        let version = system_parts[1..].join("|");
        if !version.is_empty() {
            ce.insert("version".into(), J::String(version));
        }
    }
    if let Some(value_sets) = &from.value_sets {
        let mapped: Vec<J> = value_sets
            .iter()
            .map(|vs| {
                let resolved = tank.vs_url(vs).or_else(|| pkg_url(store, vs, FishType::ValueSet));
                match resolved {
                    Some(u) => {
                        let version = vs.split('|').skip(1).collect::<Vec<_>>().join("|");
                        if version.is_empty() {
                            u
                        } else {
                            format!("{u}|{version}")
                        }
                    }
                    None => vs.clone(),
                }
            })
            .filter(|u| u != vs_url)
            .map(J::String)
            .collect();
        ce.insert("valueSet".into(), J::Array(mapped));
    }
    ce
}

fn compose_concepts(concepts: &[FshCode]) -> Vec<J> {
    concepts
        .iter()
        .map(|c| {
            let mut m = Map::new();
            m.insert("code".into(), J::String(c.code.clone()));
            if let Some(d) = &c.display {
                m.insert("display".into(), J::String(d.clone()));
            }
            J::Object(m)
        })
        .collect()
}

fn ce_get_str<'a>(ce: &'a Map<String, J>, key: &str) -> Option<&'a str> {
    ce.get(key).and_then(|v| v.as_str())
}

fn concept_codes(ce: &Map<String, J>) -> Vec<String> {
    ce.get("concept")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("code").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn value_sets_of(ce: &Map<String, J>) -> Vec<String> {
    ce.get("valueSet")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// `addConceptComposeElement` (`ValueSetExporter.ts:268`).
fn add_concept_compose_element(fresh: Map<String, J>, list: &mut Vec<J>) {
    let fresh_has_concepts = !concept_codes(&fresh).is_empty();
    if fresh_has_concepts {
        let fresh_system = ce_get_str(&fresh, "system").map(String::from);
        let fresh_version = ce_get_str(&fresh, "version").map(String::from);
        let fresh_vs = value_sets_of(&fresh);
        let matching = list.iter().position(|c| {
            let cm = c.as_object().unwrap();
            cm.get("system").and_then(|v| v.as_str()).map(String::from) == fresh_system
                && cm.get("version").and_then(|v| v.as_str()).map(String::from) == fresh_version
                && !concept_codes(cm).is_empty()
                && xor_empty(&value_sets_of(cm), &fresh_vs)
        });
        if let Some(i) = matching {
            let existing = list[i].as_object_mut().unwrap();
            let fresh_concepts = match fresh.get("concept") {
                Some(J::Array(a)) => a.clone(),
                _ => vec![],
            };
            if let Some(J::Array(arr)) = existing.get_mut("concept") {
                arr.extend(fresh_concepts);
            }
        } else {
            list.push(J::Object(fresh));
        }
    } else {
        list.push(J::Object(fresh));
    }
}

fn xor_empty(a: &[String], b: &[String]) -> bool {
    // lodash xor(a,b).length === 0  <=>  same set of elements
    let mut sa: Vec<&String> = a.iter().collect();
    let mut sb: Vec<&String> = b.iter().collect();
    sa.sort();
    sa.dedup();
    sb.sort();
    sb.dedup();
    sa == sb
}

/// `setCompose` (`ValueSetExporter.ts:73`).
fn set_compose(obj: &mut Map<String, J>, vs: &FshValueSet, tank: &TankIndex, store: Option<&PackageStore>) {
    let components: Vec<&Rule> = vs
        .rules
        .iter()
        .filter(|r| matches!(r, Rule::VsConcept { .. } | Rule::VsFilter { .. }))
        .collect();
    if components.is_empty() {
        return;
    }
    let vs_url = obj.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut include: Vec<J> = Vec::new();
    let mut exclude: Vec<J> = Vec::new();

    for comp in &components {
        match comp {
            Rule::VsConcept {
                inclusion,
                from,
                concepts,
                ..
            } => {
                let mut ce = from_to_compose_element(from, tank, &vs_url, store);
                if !concepts.is_empty() {
                    ce.insert("concept".into(), J::Array(compose_concepts(concepts)));
                }
                push_component(*inclusion, ce, concepts.is_empty(), &mut include, &mut exclude);
            }
            Rule::VsFilter {
                inclusion,
                from,
                filters,
                ..
            } => {
                let mut ce = from_to_compose_element(from, tank, &vs_url, store);
                if !filters.is_empty() {
                    let f: Vec<J> = filters
                        .iter()
                        .map(|filter| {
                            let mut m = Map::new();
                            m.insert("property".into(), J::String(filter.property.clone()));
                            m.insert("op".into(), J::String(filter.operator.clone()));
                            m.insert(
                                "value".into(),
                                J::String(filter_value_to_string(&filter.value)),
                            );
                            J::Object(m)
                        })
                        .collect();
                    ce.insert("filter".into(), J::Array(f));
                }
                // Filters never carry concepts; treat as no-concept component.
                push_component(*inclusion, ce, true, &mut include, &mut exclude);
            }
            _ => {}
        }
    }

    let mut compose = Map::new();
    compose.insert("include".into(), J::Array(include));
    if !exclude.is_empty() {
        compose.insert("exclude".into(), J::Array(exclude));
    }
    obj.insert("compose".into(), J::Object(compose));
}

fn push_component(
    inclusion: bool,
    ce: Map<String, J>,
    no_concepts: bool,
    include: &mut Vec<J>,
    exclude: &mut Vec<J>,
) {
    if inclusion {
        if !no_concepts {
            // dedupe-merge against existing includes with same system+version.
            let system = ce_get_str(&ce, "system").map(String::from);
            let version = ce_get_str(&ce, "version").map(String::from);
            let mut potential: Vec<String> = Vec::new();
            for c in include.iter() {
                let cm = c.as_object().unwrap();
                if cm.get("system").and_then(|v| v.as_str()).map(String::from) == system
                    && cm.get("version").and_then(|v| v.as_str()).map(String::from) == version
                    && !concept_codes(cm).is_empty()
                {
                    potential.extend(concept_codes(cm));
                }
            }
            let mut ce = ce;
            // filter ce.concept removing dups (already-present or earlier dup in self)
            if let Some(J::Array(arr)) = ce.get_mut("concept") {
                let mut seen: Vec<String> = Vec::new();
                arr.retain(|c| {
                    let code = c
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if potential.contains(&code) || seen.contains(&code) {
                        false
                    } else {
                        seen.push(code);
                        true
                    }
                });
            }
            if !concept_codes(&ce).is_empty() {
                add_concept_compose_element(ce, include);
            }
        } else {
            // push if it has any valueSet or a defined system.
            let has_vs = ce.contains_key("valueSet");
            let has_system = ce.contains_key("system");
            if has_vs || has_system {
                include.push(J::Object(ce));
            }
        }
    } else {
        add_concept_compose_element(ce, exclude);
    }
}

fn filter_value_to_string(v: &FilterValue) -> String {
    match v {
        FilterValue::Code(fc) => fc.code.clone(),
        FilterValue::Str(s) => s.clone(),
        FilterValue::Bool(b) => b.to_string(),
        FilterValue::Regex(r) => r.clone(),
    }
}

// ---------------------------------------------------------------------------
// CodeSystem export.
// ---------------------------------------------------------------------------

pub fn export_code_system(
    cs: &FshCodeSystem,
    cfg: &Config,
    _tank: &TankIndex,
    resolver: &TypeResolver,
) -> Exported {
    let id = effective_id(&cs.rules, &cs.id);
    let mut obj: Map<String, J> = Map::new();

    // Constructor-initialized field order: resourceType, status, content.
    obj.insert("resourceType".into(), J::String("CodeSystem".into()));
    obj.insert("status".into(), J::String(cfg.status().into()));
    obj.insert("content".into(), J::String("complete".into()));
    // setMetadata.
    obj.insert("name".into(), J::String(cs.name.clone()));
    obj.insert("id".into(), J::String(id.clone()));
    if let Some(t) = &cs.title {
        if !t.is_empty() {
            obj.insert("title".into(), J::String(t.clone()));
        }
    }
    if let Some(d) = &cs.description {
        if !d.is_empty() {
            obj.insert("description".into(), J::String(d.clone()));
        }
    }
    if cfg.fsh_only {
        if let Some(v) = &cfg.version {
            obj.insert("version".into(), J::String(v.clone()));
        }
    }
    obj.insert(
        "url".into(),
        J::String(format!("{}/CodeSystem/{}", cfg.canonical, id)),
    );

    // setConcepts.
    set_concepts(&mut obj, cs);

    // Resolve `[+]`/`[=]` soft indices on caret paths (keyed per concept-path).
    let mut resolved_rules = cs.rules.clone();
    crate::paths::resolve_soft_indexing(&mut resolved_rules);

    // setCaretPathRules (`CodeSystemExporter.ts:108`): both top-level carets
    // (empty pathArray) and concept-level carets (pathArray of concept codes →
    // `concept[i]...` prefix via findConceptPath). The full caret path is the
    // concept prefix joined with the rule's caret path. Concepts must already be
    // built (set_concepts above) so the indices resolve.
    let cs_carets: Vec<(String, &FshValue)> = resolved_rules
        .iter()
        .filter_map(|r| {
            if let Rule::CaretValue {
                path_array,
                caret_path: Some(cp),
                value: Some(value),
                is_instance: false,
                ..
            } = r
            {
                let prefix = find_concept_path(&obj, path_array)?;
                let full = if prefix.is_empty() {
                    cp.clone()
                } else {
                    format!("{prefix}.{cp}")
                };
                Some((full, value))
            } else {
                None
            }
        })
        .collect();

    // setImpliedPropertiesOnInstance pre-pass (see export_value_set): hoist
    // extension/modifierExtension slice urls ahead of later metadata caret keys.
    for (full, _) in &cs_carets {
        precreate_implied_for_path(&mut obj, "CodeSystem", full, resolver);
    }

    // value loop, in source order.
    for (full, value) in &cs_carets {
        apply_caret(&mut obj, "CodeSystem", full, value, resolver);
    }

    // updateCount: only when content == 'complete'.
    if obj.get("content").and_then(|v| v.as_str()) == Some("complete") {
        if let Some(J::Array(concepts)) = obj.get("concept") {
            let count = count_concepts(concepts);
            if count > 0 && !obj.contains_key("count") {
                obj.insert("count".into(), J::Number(count.into()));
            }
        }
    }

    Exported {
        filename: format!("CodeSystem-{}.json", id),
        body: J::Object(obj),
    }
}

/// `CodeSystemExporter.findConceptPath`: resolve a concept-code path array
/// (e.g. `["#_HookType"]`) to a `concept[i].concept[j]` prefix into the built
/// concept tree. Returns `Some("")` for an empty path array (top-level caret),
/// or `None` if a code step can't be resolved (rule is skipped, matching stock's
/// `CannotResolvePathError`).
fn find_concept_path(obj: &Map<String, J>, path_array: &[String]) -> Option<String> {
    if path_array.is_empty() {
        return Some(String::new());
    }
    let mut indices: Vec<usize> = Vec::new();
    let mut list: Option<&Vec<J>> = match obj.get("concept") {
        Some(J::Array(a)) => Some(a),
        _ => None,
    };
    for step in path_array {
        let arr = list?;
        let want = step.strip_prefix('#').unwrap_or(step);
        let idx = arr
            .iter()
            .position(|c| c.get("code").and_then(|v| v.as_str()) == Some(want))?;
        indices.push(idx);
        list = match arr[idx].get("concept") {
            Some(J::Array(a)) => Some(a),
            _ => None,
        };
    }
    Some(
        indices
            .iter()
            .map(|i| format!("concept[{i}]"))
            .collect::<Vec<_>>()
            .join("."),
    )
}

fn count_concepts(concepts: &[J]) -> u64 {
    let mut total = concepts.len() as u64;
    for c in concepts {
        if let Some(J::Array(children)) = c.get("concept") {
            total += count_concepts(children);
        }
    }
    total
}

/// `setConcepts` (`CodeSystemExporter.ts:52`) with hierarchy support.
fn set_concepts(obj: &mut Map<String, J>, cs: &FshCodeSystem) {
    let concept_rules: Vec<&Rule> = cs
        .rules
        .iter()
        .filter(|r| matches!(r, Rule::Concept { .. }))
        .collect();
    if concept_rules.is_empty() {
        return;
    }
    let mut root: Vec<J> = Vec::new();
    // Track codes already added (for duplicate detection like the TS Map).
    let mut existing: Vec<String> = Vec::new();

    for r in &concept_rules {
        let Rule::Concept {
            code,
            display,
            definition,
            hierarchy,
            ..
        } = r
        else {
            continue;
        };
        if existing.contains(code) {
            // duplicate code: TS logs unless it is a pure path-context restatement.
            continue;
        }
        let mut new_concept = Map::new();
        new_concept.insert("code".into(), J::String(code.clone()));
        if let Some(d) = display {
            new_concept.insert("display".into(), J::String(d.clone()));
        }
        if let Some(def) = definition {
            new_concept.insert("definition".into(), J::String(def.clone()));
        }

        // Navigate the hierarchy to find the container array.
        if insert_into_hierarchy(&mut root, hierarchy, J::Object(new_concept)) {
            existing.push(code.clone());
        }
    }

    obj.insert("concept".into(), J::Array(root));
}

/// Returns false if an ancestor in the hierarchy could not be found.
fn insert_into_hierarchy(container: &mut Vec<J>, hierarchy: &[String], concept: J) -> bool {
    if hierarchy.is_empty() {
        container.push(concept);
        return true;
    }
    let ancestor = &hierarchy[0];
    let pos = container.iter().position(|c| {
        c.get("code").and_then(|v| v.as_str()) == Some(ancestor.as_str())
    });
    let Some(pos) = pos else {
        return false;
    };
    let anc = container[pos].as_object_mut().unwrap();
    if !anc.contains_key("concept") {
        anc.insert("concept".into(), J::Array(vec![]));
    }
    let children = anc.get_mut("concept").unwrap().as_array_mut().unwrap();
    insert_into_hierarchy(children, &hierarchy[1..], concept)
}

// ---------------------------------------------------------------------------
// Driver.
// ---------------------------------------------------------------------------

/// Export every ValueSet and CodeSystem from the (already insert-expanded) tank.
pub fn export_all(docs: &[FshDocument], cfg: &Config, store: Option<&PackageStore>) -> Vec<Exported> {
    // Populate the global alias table so caret-path brackets resolve (shared with
    // the SD exporter). Idempotent; safe to call before/after SD export.
    crate::sd_export::set_aliases(docs);
    let tank = TankIndex::build(docs, cfg);
    // SD-driven type resolver over the FHIR packages (fishes ValueSet/CodeSystem +
    // every datatype/extension SD on demand). A local extension referenced by url
    // that isn't yet exported falls back to the generic Extension SD inside the
    // resolver, so `value[x]` still types correctly.
    let fish = |name: &str| store.and_then(|s| s.fish_for_fhir(name, package_store::ALL_FISH_TYPES));
    let resolver = TypeResolver::new(&fish);
    let mut out = Vec::new();
    // CodeSystems export before ValueSets (FHIRExporter order), though it does
    // not affect file output for these self-contained resources.
    let mut seen_cs: Vec<String> = Vec::new();
    for doc in docs {
        for (_k, cs) in &doc.code_systems {
            if seen_cs.contains(&cs.name) {
                continue;
            }
            seen_cs.push(cs.name.clone());
            out.push(export_code_system(cs, cfg, &tank, &resolver));
        }
    }
    let mut seen_vs: Vec<String> = Vec::new();
    for doc in docs {
        for (_k, vs) in &doc.value_sets {
            if seen_vs.contains(&vs.name) {
                continue;
            }
            seen_vs.push(vs.name.clone());
            out.push(export_value_set(vs, cfg, &tank, store, &resolver));
        }
    }
    out
}
