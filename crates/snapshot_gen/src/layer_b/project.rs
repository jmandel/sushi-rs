//! Layer B / Phase B0 — R4-artifact projection (stage-6 PROJECT).
//!
//! Converts a native-R5-internal StructureDefinition (walk output) to the
//! R4-artifact shape the IG Publisher writes into `package.db.Resources.Json`
//! for an R4 IG. This is the inverse, for the artifact-relevant fields, of the
//! forward R4->R5 conversion in `convert.rs`.
//!
//! ## What the Publisher actually does (audit §2)
//!
//! `PublisherBase.convertToElement(r, res)` (PublisherBase.java:396-429), called
//! from the snapshot path at :756, for an R4 IG:
//!   1. `VersionConvertorFactory_40_50.convertResource(res)` — R5 SD -> **R4** SD
//!      (PublisherBase.java:411);
//!   2. composes with the **R4** JsonParser;
//!   3. re-parses that R4 JSON back into the R5 element model (:427).
//! The bytes stored in package.db are that R4-shaped tree.
//!
//! For the artifact fields that reach package.db, the R5->R4 downconvert +
//! R4-JsonParser re-serialization is observationally:
//!   * **R4 key order** — every ElementDefinition's keys re-sorted to the R4
//!     `@Child` order (0-33), and SD top-level to R4 order (quirk
//!     `project.r4-key-order`); the native-R5 walk emits keys in walk/merge order.
//!   * **`constraint.xpath` restored** from the carried `EXT_XPATH_CONSTRAINT`
//!     extension (ElementDefinition40_50.java:567-568), the extension dropped
//!     (quirk `project.xpath-restore`). R4-only; present ONLY because the IG is R4.
//!   * **R5-only ED fields demoted** back to extensions: `mustHaveValue` ->
//!     ext[EXT_MUST_VALUE] (ElementDefinition40_50.java:148-150),
//!     `valueAlternatives[]` -> ext[EXT_VALUE_ALT] (:151-153); and
//!     `binding.additional[]` -> ext (:693-695). These are the inverse of
//!     convert.rs's forward promotion.
//!
//! ## Version-conditional
//!
//! Projection applies ONLY when the IG's fhirVersion is R4. For an R5 IG
//! `convertToElement` takes the else-branch (PublisherBase.java:421-424, plain R5
//! compose): no xpath, no downconversion. `project_r4` is only called on the R4
//! path; R5 IGs get no projection (the site_db flag gates on `--core` R4).
//!
//! Default OFF: this is opt-in Layer B only.

use std::cell::RefCell;
use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::PackageContext;

// Extension URLs — must match convert.rs (the forward direction) exactly.
const EXT_XPATH_CONSTRAINT: &str =
    "http://hl7.org/fhir/4.0/StructureDefinition/extension-ElementDefinition.constraint.xpath";
const EXT_MUST_VALUE: &str =
    "http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.mustHaveValue";
const EXT_VALUE_ALT: &str =
    "http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.valueAlternatives";
/// tools additional-binding — the URL the R5->R4 downconvert emits
/// (ElementDefinition40_50.java:693 -> convertAdditional -> EXT_BINDING_ADDITIONAL).
const EXT_BINDING_ADDITIONAL_TOOLS: &str =
    "http://hl7.org/fhir/tools/StructureDefinition/additional-binding";

/// The `constraint.xpath` restorer. In Java the R4->R5 load converts
/// `constraint.xpath` into the `EXT_XPATH_CONSTRAINT` extension, which the
/// snapshot inherits and the R5->R4 downconvert re-emits. Our Layer-A load reads
/// package R4 resources leniently (dropping R4-only `xpath`; REWORK-PLAN §8
/// Increment 2), so the walk snapshot has neither the field nor the extension.
///
/// B0 reconstructs it exactly the way Java's stored bytes do: `constraint.xpath`
/// is a deterministic function of the constraint's defining SD + key. Every
/// walk-emitted inherited constraint carries `source` (the defining SD canonical)
/// and `key`; we fetch that SD from the package context and read the matching
/// constraint's `xpath`. Keyed by `(source, key)` this is unique in R4 core
/// (verified: the only near-collision, `Extension` `ext-1`, resolves per-source).
struct XpathResolver<'a> {
    pkg: &'a PackageContext,
    // memoized (source_url) -> key -> [(defining_leaf_path, xpath)]. A key can be
    // defined on multiple paths of a source SD with DIFFERENT xpaths (e.g.
    // Extension `ext-1` on `Extension` vs `Extension.extension` — single- vs
    // double-quoted `'value'`/`"value"`). We keep all candidates and disambiguate
    // by the target element's leaf segment at lookup time.
    sd_maps: RefCell<HashMap<String, HashMap<String, Vec<(String, String)>>>>,
}

impl<'a> XpathResolver<'a> {
    fn new(pkg: &'a PackageContext) -> Self {
        Self {
            pkg,
            sd_maps: RefCell::new(HashMap::new()),
        }
    }

    /// Resolve the xpath for a constraint `key` defined in `source`, as it applies
    /// to an element at `target_path`. When the source defines the key on multiple
    /// paths, prefer the candidate whose defining path's LEAF segment matches the
    /// target element's leaf segment (so `Observation.extension` picks
    /// `Extension.extension`, not `Extension`). Falls back to the first candidate.
    fn xpath_for(&self, source: &str, key: &str, target_path: &str) -> Option<String> {
        if !self.sd_maps.borrow().contains_key(source) {
            let map = self.build_map(source);
            self.sd_maps.borrow_mut().insert(source.to_string(), map);
        }
        let maps = self.sd_maps.borrow();
        let candidates = maps.get(source)?.get(key)?;
        if candidates.is_empty() {
            return None;
        }
        // Disambiguation, in priority order:
        // 1. exact leaf match (`Observation.identifier`.ele-1 <- `Element` etc.);
        // 2. for extension slots (`.extension`/`.modifierExtension`), the source's
        //    NESTED `Extension.extension` variant (a nested defining path) — Java
        //    propagates `ext-1` from `Extension.extension` (double-quoted) to every
        //    extension AND modifierExtension slot, not from the `Extension` root;
        // 3. first candidate.
        if candidates.len() == 1 {
            return Some(candidates[0].1.clone());
        }
        let target_leaf = leaf(target_path);
        if let Some((_, x)) = candidates.iter().find(|(p, _)| leaf(p) == target_leaf) {
            return Some(x.clone());
        }
        if matches!(target_leaf, "extension" | "modifierExtension") {
            if let Some((_, x)) = candidates.iter().find(|(p, _)| p.contains('.')) {
                return Some(x.clone());
            }
        }
        candidates.first().map(|(_, x)| x.clone())
    }

    /// Build key -> [(defining_leaf_path, xpath)] from a source SD's
    /// snapshot+differential constraints, preserving all distinct-xpath variants.
    fn build_map(&self, source: &str) -> HashMap<String, Vec<(String, String)>> {
        let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let Some(sd) = self.pkg.fetch(source) else {
            return map;
        };
        for section in ["snapshot", "differential"] {
            if let Some(elements) = sd
                .get(section)
                .and_then(|s| s.get("element"))
                .and_then(Value::as_array)
            {
                for ed in elements {
                    let path = ed.get("path").and_then(Value::as_str).unwrap_or("");
                    if let Some(constraints) = ed.get("constraint").and_then(Value::as_array) {
                        for c in constraints {
                            if let (Some(k), Some(x)) = (
                                c.get("key").and_then(Value::as_str),
                                c.get("xpath").and_then(Value::as_str),
                            ) {
                                let entry = map.entry(k.to_string()).or_default();
                                // Record (path, xpath) once per distinct xpath.
                                if !entry.iter().any(|(_, xp)| xp == x) {
                                    entry.push((path.to_string(), x.to_string()));
                                }
                            }
                        }
                    }
                }
            }
        }
        map
    }
}

/// Last `.`-separated segment of an element path (`Observation.extension` ->
/// `extension`).
fn leaf(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// Project a native-R5 StructureDefinition (walk output) to the R4-artifact
/// shape. Pure; returns a new Value. `sd` must be a StructureDefinition object.
/// `pkg` is used ONLY to reconstruct `constraint.xpath` from the constraint's
/// defining SD (read-only).
pub fn project_r4(sd: &Value, pkg: &PackageContext) -> Value {
    let src = match sd.as_object() {
        Some(o) => o,
        None => return sd.clone(),
    };
    if src.get("resourceType").and_then(Value::as_str) != Some("StructureDefinition") {
        return sd.clone();
    }
    let resolver = XpathResolver::new(pkg);
    let mut out = Map::new();
    // Emit SD top-level keys in R4 order; unknown keys preserved after, in source
    // order (defensive — the walk emits only known SD fields).
    reorder_into(src, &mut out, SD_ORDER, |key, v| {
        if key == "snapshot" || key == "differential" {
            project_element_list(v, &resolver)
        } else {
            v.clone()
        }
    });
    Value::Object(out)
}

/// snapshot / differential backbone: keep [id, extension], project element[].
fn project_element_list(list: &Value, xr: &XpathResolver) -> Value {
    let src = match list.as_object() {
        Some(o) => o,
        None => return list.clone(),
    };
    let mut out = Map::new();
    if let Some(id) = src.get("id") {
        out.insert("id".into(), id.clone());
    }
    if let Some(ext) = src.get("extension") {
        out.insert("extension".into(), ext.clone());
    }
    if let Some(elems) = src.get("element").and_then(Value::as_array) {
        let projected: Vec<Value> = elems.iter().map(|e| project_element(e, xr)).collect();
        out.insert("element".into(), Value::Array(projected));
    }
    Value::Object(out)
}

/// Project one ElementDefinition into R4 shape: demote R5-only fields to
/// extensions, restore constraint.xpath, then re-order keys to R4 @Child order.
fn project_element(ed: &Value, xr: &XpathResolver) -> Value {
    let src = match ed.as_object() {
        Some(o) => o,
        None => return ed.clone(),
    };
    // Work on a mutable copy so we can demote fields into `extension` before
    // reordering.
    let mut work: Map<String, Value> = src.clone();

    // R5-only ED fields -> extensions (inverse of convert.rs promotion).
    // ElementDefinition40_50.java:148-153.
    let mut demoted: Vec<Value> = Vec::new();
    if let Some(mhv) = work.remove("mustHaveValue") {
        demoted.push(ext_value(EXT_MUST_VALUE, "valueBoolean", mhv));
    }
    if let Some(Value::Array(alts)) = work.remove("valueAlternatives") {
        for a in alts {
            demoted.push(ext_value(EXT_VALUE_ALT, "valueCanonical", a));
        }
    }
    if !demoted.is_empty() {
        append_extensions(&mut work, demoted);
    }

    // binding: additional[] -> ext[EXT_BINDING_ADDITIONAL_TOOLS], key-order to R4.
    if let Some(binding) = work.get("binding").cloned() {
        work.insert("binding".into(), project_binding(&binding));
    }
    // type[]: key-order to R4.
    if let Some(Value::Array(types)) = work.get("type").cloned() {
        let projected: Vec<Value> = types.iter().map(project_type_ref).collect();
        work.insert("type".into(), Value::Array(projected));
    }
    // constraint[]: restore xpath (from the defining SD via `source`+`key`, and
    // from a carried EXT_XPATH_CONSTRAINT extension if present), key-order to R4.
    let elem_path = work
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if let Some(Value::Array(constraints)) = work.get("constraint").cloned() {
        let projected: Vec<Value> = constraints
            .iter()
            .map(|c| project_constraint(c, xr, &elem_path))
            .collect();
        work.insert("constraint".into(), Value::Array(projected));
    }
    // slicing.discriminator, mapping etc. carry through verbatim key-order-wise
    // (their R4/R5 orders coincide; convert.rs uses them bidirectionally).

    // Finally re-order the ED's own keys to the R4 @Child order. ED carries
    // polymorphic value[x] choices (fixed*/pattern*/defaultValue*/minValue*/
    // maxValue*) whose JSON key is the base + a type suffix (e.g. `patternCode`);
    // they must land at their base field's slot, so use the choice-aware reorder.
    let mut out = Map::new();
    reorder_ed_into(&work, &mut out);
    Value::Object(out)
}

/// The polymorphic value[x] base names in an ElementDefinition (each appears in
/// JSON as `<base><TypeSuffix>`, e.g. `patternCode`, plus its `_<base><Suffix>`
/// primitive sidecar). Cite: r4/model/ElementDefinition.java @Child
/// defaultValue(17)/fixed(20)/pattern(21)/minValue(23)/maxValue(24).
const ED_CHOICE_BASES: &[&str] = &["defaultValue", "fixed", "pattern", "minValue", "maxValue"];

/// Choice-aware ED key reorder: for each name in `ED_ORDER`, emit the matching
/// key. A choice base (e.g. `pattern`) matches any `pattern<Suffix>` key AND its
/// `_pattern<Suffix>` sidecar. Non-choice names match exactly. Unknown keys are
/// preserved after, in source order (defensive, never silent).
fn reorder_ed_into(src: &Map<String, Value>, out: &mut Map<String, Value>) {
    let mut placed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in ED_ORDER {
        if ED_CHOICE_BASES.contains(name) {
            // Emit `<base><Suffix>` then its `_<base><Suffix>` sidecar, in source
            // order (there is at most one choice value per base).
            for (k, v) in src.iter() {
                if is_choice_key(k, name) && !placed.contains(k) {
                    out.insert(k.clone(), v.clone());
                    placed.insert(k.clone());
                    let sidecar = format!("_{k}");
                    if let Some(sc) = src.get(&sidecar) {
                        out.insert(sidecar.clone(), sc.clone());
                        placed.insert(sidecar);
                    }
                }
            }
        } else if let Some(v) = src.get(*name) {
            out.insert((*name).to_string(), v.clone());
            placed.insert((*name).to_string());
            // primitive sidecar (e.g. `_min`, `_max`) directly after its value.
            let sidecar = format!("_{name}");
            if let Some(sc) = src.get(&sidecar) {
                out.insert(sidecar.clone(), sc.clone());
                placed.insert(sidecar);
            }
        }
    }
    for (k, v) in src.iter() {
        if !placed.contains(k) {
            out.insert(k.clone(), v.clone());
        }
    }
}

/// True if `key` is `<base><Suffix>` with an uppercase-led type suffix (the
/// polymorphic value[x] pattern), matching convert.rs `choice_suffix`.
fn is_choice_key(key: &str, base: &str) -> bool {
    key.strip_prefix(base)
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_uppercase())
}

/// constraint: restore `xpath` from the carried EXT_XPATH_CONSTRAINT extension,
/// drop that extension, and order keys to R4 (key, requirements, severity, human,
/// expression, xpath, source). ElementDefinition40_50.java:567-568.
fn project_constraint(c: &Value, xr: &XpathResolver, elem_path: &str) -> Value {
    let src = match c.as_object() {
        Some(o) => o,
        None => return c.clone(),
    };
    let mut work: Map<String, Value> = src.clone();
    // Extract + drop a carried EXT_XPATH_CONSTRAINT extension (present when the
    // constraint came through a full R4->R5 convert, e.g. local-dir SDs).
    let mut xpath: Option<Value> = None;
    if let Some(Value::Array(exts)) = work.get("extension").cloned() {
        let mut kept = Vec::new();
        for e in exts {
            if e.get("url").and_then(Value::as_str) == Some(EXT_XPATH_CONSTRAINT) {
                if let Some(v) = e.get("valueString") {
                    xpath = Some(v.clone());
                }
            } else {
                kept.push(e);
            }
        }
        if kept.is_empty() {
            work.remove("extension");
        } else {
            work.insert("extension".into(), Value::Array(kept));
        }
    }
    // If no carried extension (the common case for package-loaded R4 core
    // constraints, whose xpath our lenient load dropped), reconstruct it from the
    // defining SD via (source, key). quirk `project.xpath-restore`.
    if xpath.is_none() {
        if let (Some(source), Some(key)) = (
            work.get("source").and_then(Value::as_str),
            work.get("key").and_then(Value::as_str),
        ) {
            if let Some(x) = xr.xpath_for(source, key, elem_path) {
                xpath = Some(Value::String(x));
            }
        }
    }
    if let Some(x) = xpath {
        work.insert("xpath".into(), x);
    }
    let mut out = Map::new();
    reorder_into(&work, &mut out, CONSTRAINT_ORDER, |_k, v| v.clone());
    Value::Object(out)
}

/// binding: demote `additional[]` back to `EXT_BINDING_ADDITIONAL_TOOLS`
/// extensions (inverse of convert.rs conv_binding / ElementDefinition40_50
/// convertAdditional), then order keys to R4 (id, extension, strength,
/// description, valueSet).
fn project_binding(b: &Value) -> Value {
    let src = match b.as_object() {
        Some(o) => o,
        None => return b.clone(),
    };
    let mut work: Map<String, Value> = src.clone();
    if let Some(Value::Array(additional)) = work.remove("additional") {
        let exts: Vec<Value> = additional
            .iter()
            .map(|a| additional_to_extension(a))
            .collect();
        append_extensions(&mut work, exts);
    }
    let mut out = Map::new();
    reorder_into(&work, &mut out, BINDING_ORDER, |_k, v| v.clone());
    Value::Object(out)
}

/// Rebuild an `additional-binding` extension from an R5 `binding.additional[]`
/// entry (inverse of convert.rs conv_additional). The child extensions carry the
/// sub-fields; we keep it minimal and faithful to the forward mapping.
fn additional_to_extension(add: &Value) -> Value {
    let src = as_obj(add);
    let mut children: Vec<Value> = Vec::new();
    // Order mirrors convert.rs conv_additional's child probing.
    if let Some(p) = src.get("purpose") {
        children.push(ext_value("purpose", "valueCode", p.clone()));
    }
    if let Some(vs) = src.get("valueSet") {
        children.push(ext_value("valueSet", "valueCanonical", vs.clone()));
    }
    if let Some(d) = src.get("documentation") {
        children.push(ext_value("documentation", "valueMarkdown", d.clone()));
    }
    if let Some(s) = src.get("shortDoco") {
        children.push(ext_value("shortDoco", "valueString", s.clone()));
    }
    if let Some(Value::Array(usages)) = src.get("usage") {
        for u in usages {
            children.push(ext_value("usage", "valueUsageContext", u.clone()));
        }
    }
    if let Some(a) = src.get("any") {
        children.push(ext_value("any", "valueBoolean", a.clone()));
    }
    // Preserve any extra extensions carried on the R5 additional entry.
    if let Some(Value::Array(extra)) = src.get("extension") {
        children.extend(extra.iter().cloned());
    }
    let mut ext = Map::new();
    ext.insert("url".into(), Value::String(EXT_BINDING_ADDITIONAL_TOOLS.into()));
    if !children.is_empty() {
        ext.insert("extension".into(), Value::Array(children));
    }
    Value::Object(ext)
}

/// type: order keys to R4 (id, extension, code, profile, targetProfile,
/// aggregation, versioning). R4 == R5 here; reorder is a no-op unless the walk
/// emitted a different order, but we normalize for byte-parity safety.
fn project_type_ref(t: &Value) -> Value {
    let src = match t.as_object() {
        Some(o) => o,
        None => return t.clone(),
    };
    let mut out = Map::new();
    reorder_into(src, &mut out, TYPE_ORDER, |_k, v| v.clone());
    Value::Object(out)
}

// --- key-ordering machinery --------------------------------------------------

/// Insert `src`'s keys into `out` in the order given by `order`, then any keys
/// not in `order` in source order (defensive). Each value is passed through `map`
/// (keyed by name) so nested sections can be projected. Keys absent from `src`
/// are skipped; `id`/`extension`/`modifierExtension` are handled as envelope
/// keys via the order arrays.
fn reorder_into(
    src: &Map<String, Value>,
    out: &mut Map<String, Value>,
    order: &[&str],
    map: impl Fn(&str, &Value) -> Value,
) {
    for key in order {
        if let Some(v) = src.get(*key) {
            out.insert((*key).to_string(), map(key, v));
        }
    }
    // Preserve unknown keys (should be none for the walk output) after, in
    // source order, so projection is never lossy/silent.
    for (k, v) in src.iter() {
        if !order.contains(&k.as_str()) {
            out.insert(k.clone(), map(k, v));
        }
    }
}

fn as_obj(v: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    v.as_object().unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

fn ext_value(url: &str, value_key: &str, value: Value) -> Value {
    let mut m = Map::new();
    m.insert("url".into(), Value::String(url.into()));
    m.insert(value_key.into(), value);
    Value::Object(m)
}

fn append_extensions(obj: &mut Map<String, Value>, mut exts: Vec<Value>) {
    match obj.get_mut("extension").and_then(Value::as_array_mut) {
        Some(arr) => arr.append(&mut exts),
        None => {
            obj.insert("extension".into(), Value::Array(exts));
        }
    }
}

// --- R4 @Child orders (cited to r4/model/*.java) -----------------------------

/// R4 StructureDefinition top-level order. `resourceType` first (JsonParser),
/// then DomainResource envelope (id, meta, implicitRules, language, text,
/// contained, extension, modifierExtension), then the typed fields in R4 order.
/// Cite: r4/model/StructureDefinition.java @Child order + JsonParser.
const SD_ORDER: &[&str] = &[
    "resourceType",
    "id",
    "meta",
    "implicitRules",
    "language",
    "text",
    "contained",
    "extension",
    "modifierExtension",
    "url",
    "identifier",
    "version",
    "name",
    "title",
    "status",
    "experimental",
    "date",
    "publisher",
    "contact",
    "description",
    "useContext",
    "jurisdiction",
    "purpose",
    "copyright",
    "keyword",
    "fhirVersion",
    "mapping",
    "kind",
    "abstract",
    "context",
    "contextInvariant",
    "type",
    "baseDefinition",
    "derivation",
    "snapshot",
    "differential",
];

/// R4 ElementDefinition order: Element envelope (id, extension,
/// modifierExtension) then @Child 0-33. Cite: r4/model/ElementDefinition.java
/// @Child order 0-33.
const ED_ORDER: &[&str] = &[
    "id",
    "extension",
    "modifierExtension",
    "path",             // 0
    "representation",   // 1
    "sliceName",        // 2
    "sliceIsConstraining", // 3
    "label",            // 4
    "code",             // 5
    "slicing",          // 6
    "short",            // 7
    "definition",       // 8
    "comment",          // 9
    "requirements",     // 10
    "alias",            // 11
    "min",              // 12
    "max",              // 13
    "base",             // 14
    "contentReference", // 15
    "type",             // 16
    // defaultValue[x] (17), fixed[x] (20), pattern[x] (21), minValue[x] (23),
    // maxValue[x] (24) are polymorphic — matched by prefix below via
    // `reorder_choice`-free path: we place them via explicit prefixes.
    "defaultValue",
    "meaningWhenMissing", // 18
    "orderMeaning",       // 19
    "fixed",
    "pattern",
    "example",  // 22
    "minValue",
    "maxValue",
    "maxLength",  // 25
    "condition",  // 26
    "constraint", // 27
    "mustSupport", // 28
    "isModifier",  // 29
    "isModifierReason", // 30
    "isSummary",   // 31
    "binding",     // 32
    "mapping",     // 33
];

/// R4 ElementDefinition.constraint order: Element envelope then @Child 1-7 with
/// xpath at 6. Cite: r4/model/ElementDefinition.java constraint @Child.
const CONSTRAINT_ORDER: &[&str] = &[
    "id",
    "extension",
    "modifierExtension",
    "key",          // 1
    "requirements", // 2
    "severity",     // 3
    "human",        // 4
    "expression",   // 5
    "xpath",        // 6
    "source",       // 7
];

/// R4 ElementDefinition.binding order. Cite: r4/model/ElementDefinition.java
/// binding @Child (strength, description, valueSet).
const BINDING_ORDER: &[&str] = &[
    "id",
    "extension",
    "modifierExtension",
    "strength",
    "description",
    "valueSet",
];

/// R4 ElementDefinition.type order. Cite: r4/model/ElementDefinition.java type
/// @Child (code, profile, targetProfile, aggregation, versioning).
const TYPE_ORDER: &[&str] = &[
    "id",
    "extension",
    "modifierExtension",
    "code",
    "profile",
    "targetProfile",
    "aggregation",
    "versioning",
];
