//! Tier-1 terminology evaluator — `expand_enumerable()` (editor spec §6 tier 1).
//!
//! A self-contained ValueSet-expansion evaluator for the composes that are
//! *pure functions of IG content*: enumerated codes (`include.concept`), whole
//! IG-local complete CodeSystems (`include.system`), local-CS filters that are
//! decidable from the local concept tree (`is-a`/`descendent-of`/`=`), nested
//! enumerable `include.valueSet` references, and `exclude` mirroring the same.
//!
//! HARD BOUNDARY (spec §6 tier 1, cycle-plan §3): this is NOT a terminology
//! service. There is NO external-system subsumption, ever. A filter over a code
//! system we do not have as *complete local content* → [`NotEnumerable`], with a
//! precise reason naming the offending include/exclude. That refusal string is
//! what the editor surfaces verbatim as its "needs terminology server" state.
//!
//! Input/output are FHIR JSON: this consumes a compiled `ValueSet` resource
//! (`compiler::export`'s output body) and a [`Resolver`] over the IG-local +
//! package-cache `ValueSet`/`CodeSystem` resources, and produces a FHIR
//! `ValueSet.expansion` object (`contains[]`, `total`, `parameter[]`).
//!
//! ## FHIR spec citations (R4 ValueSet / expansion op)
//! - Compose model + `include.concept` / `include.system` / `include.valueSet` /
//!   `exclude`: <https://hl7.org/fhir/R4/valueset.html#compose> and
//!   <https://hl7.org/fhir/R4/valueset-definitions.html#ValueSet.compose>.
//! - Filter operations (`is-a`, `descendent-of`, `=`, etc.):
//!   <https://hl7.org/fhir/R4/valueset-filter-operator.html> and
//!   <https://hl7.org/fhir/R4/codesystem-definitions.html#CodeSystem.filter>.
//! - CodeSystem `content` (`complete` required to enumerate):
//!   <https://hl7.org/fhir/R4/codesystem-definitions.html#CodeSystem.content>.
//! - `$expand` output shape (`expansion.contains`, `.total`, `.parameter`):
//!   <https://hl7.org/fhir/R4/valueset-operation-expand.html>.
//!
//! ## Deterministic ordering (documented rule)
//! `expansion.contains[]` is sorted by `(system, code)` with a byte-wise
//! (Unicode-scalar) string comparison — stable and reproducible. tx.fhir.org's
//! observable order for the enumerable composes we target is authored/insertion
//! order per include, which is NOT stable across compose edits; the oracle gate
//! (`tests/oracle_tx.rs`) therefore normalizes BOTH sides to this same
//! `(system, code)` sort before comparing, and documents that normalization.

use serde_json::{json, Map, Value as J};
use std::collections::{BTreeMap, BTreeSet};

/// A resolvable FHIR terminology resource (`ValueSet` or `CodeSystem`), keyed by
/// canonical URL. The evaluator only ever *reads* — the abstraction lets tests
/// feed an in-memory map and production wrap `PackageStore::fish_for_fhir` over
/// the compiled IG-local resources + the package cache.
pub trait Resolver {
    /// Return the `CodeSystem` resource whose `.url` matches `url` (version
    /// stripped by the caller), if it is available as local/cached content.
    fn code_system(&self, url: &str) -> Option<J>;
    /// Return the `ValueSet` resource whose `.url` matches `url`, if available.
    fn value_set(&self, url: &str) -> Option<J>;
}

/// A resolver that owns two URL→resource maps. Convenient for tests and for the
/// wasm/integration pass (build it from the compiled tank output + cache reads).
#[derive(Default, Clone)]
pub struct MapResolver {
    code_systems: BTreeMap<String, J>,
    value_sets: BTreeMap<String, J>,
}

impl MapResolver {
    pub fn new() -> Self {
        Self::default()
    }
    /// Insert a resource, keying by its `.url` (version suffix stripped). Ignores
    /// resources without a `url`.
    pub fn insert(&mut self, resource: J) -> &mut Self {
        let rt = resource.get("resourceType").and_then(J::as_str).unwrap_or("");
        if let Some(url) = resource.get("url").and_then(J::as_str) {
            let key = strip_version(url).to_string();
            match rt {
                "CodeSystem" => {
                    self.code_systems.insert(key, resource);
                }
                "ValueSet" => {
                    self.value_sets.insert(key, resource);
                }
                _ => {}
            }
        }
        self
    }
}

impl Resolver for MapResolver {
    fn code_system(&self, url: &str) -> Option<J> {
        self.code_systems.get(strip_version(url)).cloned()
    }
    fn value_set(&self, url: &str) -> Option<J> {
        self.value_sets.get(strip_version(url)).cloned()
    }
}

/// The precise reason `expand_enumerable` refused. Carries WHICH compose element
/// and WHY, so the editor can render "needs terminology server" naming the exact
/// blocker. Every message is stable, human-readable, and single-line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotEnumerable {
    /// `"include"` or `"exclude"` — which side of the compose.
    pub component: &'static str,
    /// 0-based index of the offending element within that side.
    pub index: usize,
    /// The system URL involved (as authored), when known.
    pub system: Option<String>,
    /// A precise, stable reason string (see [`RefusalKind`] for the taxonomy).
    pub reason: String,
    /// The machine-readable refusal class, for the editor to branch on.
    pub kind: RefusalKind,
}

/// Taxonomy of refusal classes (the editor branches on this; the human string in
/// [`NotEnumerable::reason`] is what it shows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// A `filter` over a system that is NOT available as complete local content
    /// (e.g. SNOMED `is-a`). The tier-1/service boundary — never expandable here.
    ExternalSystemFilter,
    /// A bare `include.system` (whole-CS enumeration) over a system we cannot
    /// resolve to a local resource, or whose `content` is not `complete`.
    UnresolvableOrIncompleteSystem,
    /// An `include.valueSet` reference that does not resolve to an available VS.
    UnresolvableValueSet,
    /// A referenced (nested) ValueSet was itself not enumerable — the reason
    /// carries the inner refusal.
    NestedNotEnumerable,
    /// A local-CS filter whose `op`/`property` we do not implement over local
    /// content (e.g. `regex`, `in`, `exists`, a property we can't decide).
    UnsupportedLocalFilter,
    /// Malformed compose (e.g. `include` with neither `system`, `concept`, nor
    /// `valueSet`).
    Malformed,
    /// Cycle detected among `include.valueSet` references.
    CycleGuard,
}

impl std::fmt::Display for NotEnumerable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}[{}]: {}", self.component, self.index, self.reason)
    }
}
impl std::error::Error for NotEnumerable {}

/// A fully-expanded value set: the FHIR `expansion` object plus the set of
/// `(system, version)` pairs the expansion actually drew from (for
/// `expansion.parameter`, per `$expand` norms + cycle-plan §3 pinning).
#[derive(Debug, Clone)]
pub struct Expansion {
    /// `contains[]` rows, already sorted by `(system, code)`.
    contains: Vec<Concept>,
    /// The `used-codesystem` versions, in first-seen order.
    used_systems: Vec<(String, Option<String>)>,
    /// `true` if any drawn-from CS/VS was `inactive`/`retired`/`experimental`
    /// — surfaced as the `inactive`/`experimental` expansion parameters.
    experimental: bool,
    /// Copyright strings gathered from drawn-from code systems (deduped, ordered).
    copyright: Vec<String>,
}

/// One expansion member (`expansion.contains[i]`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Concept {
    system: String,
    code: String,
    display: Option<String>,
    /// `true` if the source concept was flagged inactive (drives `inactive` on
    /// the emitted row; excluded from `total` per no filter-active default is
    /// not applied — we include inactives explicitly enumerated, per authoring).
    inactive: bool,
}

impl Expansion {
    /// Serialize to a FHIR `ValueSet.expansion` JSON object (the `$expand`
    /// output shape). `timestamp`/`identifier` are intentionally omitted — this
    /// evaluator is deterministic and content-addressed; callers that want a
    /// stamp add it. Ordering is `(system, code)` (see module docs).
    pub fn to_expansion_json(&self) -> J {
        let mut params: Vec<J> = Vec::new();
        // `used-codesystem` — the CS versions the expansion drew from (§3 pin).
        for (system, version) in &self.used_systems {
            let value = match version {
                Some(v) => format!("{system}|{v}"),
                None => system.clone(),
            };
            params.push(json!({"name": "used-codesystem", "valueUri": value}));
        }
        if self.experimental {
            // `$expand` reports experimental content via this parameter.
            params.push(json!({"name": "expansion.parameter.experimental", "valueBoolean": true}));
        }

        let contains: Vec<J> = self
            .contains
            .iter()
            .map(|c| {
                let mut o = Map::new();
                o.insert("system".into(), J::String(c.system.clone()));
                if c.inactive {
                    o.insert("inactive".into(), J::Bool(true));
                }
                o.insert("code".into(), J::String(c.code.clone()));
                if let Some(d) = &c.display {
                    o.insert("display".into(), J::String(d.clone()));
                }
                J::Object(o)
            })
            .collect();

        let mut exp = Map::new();
        exp.insert("total".into(), json!(self.contains.len()));
        if !params.is_empty() {
            exp.insert("parameter".into(), J::Array(params));
        }
        exp.insert("contains".into(), J::Array(contains));
        J::Object(exp)
    }

    /// The number of `contains[]` rows.
    pub fn total(&self) -> usize {
        self.contains.len()
    }

    /// Copyright strings gathered from the drawn-from code systems.
    pub fn copyright(&self) -> &[String] {
        &self.copyright
    }
}

/// Expand a ValueSet's compose to a concrete member set, IF and ONLY IF the
/// compose is a pure function of IG content (see module docs). Otherwise returns
/// [`NotEnumerable`] naming the exact blocking element.
///
/// `valueset` is a FHIR `ValueSet` resource JSON (the compiler's export body).
/// A ValueSet that already carries a literal `expansion` is re-derived from its
/// `compose` here — this evaluator does not trust a pre-baked expansion.
pub fn expand_enumerable(valueset: &J, resolver: &dyn Resolver) -> Result<Expansion, NotEnumerable> {
    let mut guard = BTreeSet::new();
    if let Some(url) = valueset.get("url").and_then(J::as_str) {
        guard.insert(strip_version(url).to_string());
    }
    expand_inner(valueset, resolver, &mut guard)
}

fn expand_inner(
    valueset: &J,
    resolver: &dyn Resolver,
    guard: &mut BTreeSet<String>,
) -> Result<Expansion, NotEnumerable> {
    let compose = valueset.get("compose");

    let mut acc = Accumulator::default();

    let includes = compose
        .and_then(|c| c.get("include"))
        .and_then(J::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (i, inc) in includes.iter().enumerate() {
        let part = eval_component(inc, resolver, guard, "include", i)?;
        acc.absorb(part);
    }

    // Excludes mirror includes: expand each excluded component (must ALSO be
    // enumerable — an un-enumerable exclude means we cannot know the final set,
    // so we refuse), then subtract by (system, code).
    let excludes = compose
        .and_then(|c| c.get("exclude"))
        .and_then(J::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (i, exc) in excludes.iter().enumerate() {
        let part = eval_component(exc, resolver, guard, "exclude", i)?;
        let drop: BTreeSet<(String, String)> = part
            .concepts
            .iter()
            .map(|c| (c.system.clone(), c.code.clone()))
            .collect();
        acc.concepts.retain(|c| !drop.contains(&(c.system.clone(), c.code.clone())));
        // exclude does not contribute used-systems/copyright it didn't already.
    }

    Ok(acc.finish())
}

/// The running set during one VS expansion: deduped concepts (first display
/// wins) + used-system + copyright bookkeeping.
#[derive(Default)]
struct Accumulator {
    concepts: Vec<Concept>,
    seen: BTreeSet<(String, String)>,
    used_systems: Vec<(String, Option<String>)>,
    used_seen: BTreeSet<String>,
    copyright: Vec<String>,
    copyright_seen: BTreeSet<String>,
    experimental: bool,
}

/// The result of evaluating one include/exclude component.
struct ComponentResult {
    concepts: Vec<Concept>,
    used_systems: Vec<(String, Option<String>)>,
    copyright: Vec<String>,
    experimental: bool,
}

impl Accumulator {
    fn absorb(&mut self, part: ComponentResult) {
        for c in part.concepts {
            let key = (c.system.clone(), c.code.clone());
            if self.seen.insert(key) {
                self.concepts.push(c);
            }
        }
        for (sys, ver) in part.used_systems {
            if self.used_seen.insert(sys.clone()) {
                self.used_systems.push((sys, ver));
            }
        }
        for cr in part.copyright {
            if self.copyright_seen.insert(cr.clone()) {
                self.copyright.push(cr);
            }
        }
        self.experimental |= part.experimental;
    }

    fn finish(mut self) -> Expansion {
        // Deterministic ordering: (system, code) byte-wise. See module docs.
        self.concepts.sort_by(|a, b| {
            a.system.cmp(&b.system).then_with(|| a.code.cmp(&b.code))
        });
        Expansion {
            contains: self.concepts,
            used_systems: self.used_systems,
            experimental: self.experimental,
            copyright: self.copyright,
        }
    }
}

fn refuse(
    component: &'static str,
    index: usize,
    system: Option<&str>,
    kind: RefusalKind,
    reason: impl Into<String>,
) -> NotEnumerable {
    NotEnumerable {
        component,
        index,
        system: system.map(String::from),
        reason: reason.into(),
        kind,
    }
}

/// Evaluate a single `compose.include[i]` / `compose.exclude[i]`.
fn eval_component(
    comp: &J,
    resolver: &dyn Resolver,
    guard: &mut BTreeSet<String>,
    side: &'static str,
    index: usize,
) -> Result<ComponentResult, NotEnumerable> {
    let system = comp.get("system").and_then(J::as_str);
    let concepts = comp.get("concept").and_then(J::as_array);
    let filters = comp.get("filter").and_then(J::as_array);
    let value_sets = comp.get("valueSet").and_then(J::as_array);

    // A component with neither system nor valueSet is malformed for our purposes.
    if system.is_none() && value_sets.is_none() {
        return Err(refuse(
            side,
            index,
            None,
            RefusalKind::Malformed,
            "component has neither `system` nor `valueSet`",
        ));
    }

    let mut out = ComponentResult {
        concepts: Vec::new(),
        used_systems: Vec::new(),
        copyright: Vec::new(),
        experimental: false,
    };

    // --- include.valueSet references (recursive, cycle-guarded) -----------
    // FHIR: the component's codes are the INTERSECTION of the referenced VS(s)
    // with the (optional) system/concept/filter constraints. We support the
    // pure-valueSet form (no system) and the "system + valueSet" intersection.
    if let Some(vs_refs) = value_sets {
        let mut ref_expansions: Vec<Vec<Concept>> = Vec::new();
        for vref in vs_refs {
            let Some(url) = vref.as_str() else {
                return Err(refuse(
                    side, index, system, RefusalKind::Malformed,
                    "`valueSet` entry is not a URL string",
                ));
            };
            let key = strip_version(url).to_string();
            if guard.contains(&key) {
                return Err(refuse(
                    side, index, system, RefusalKind::CycleGuard,
                    format!("value set reference `{url}` forms a cycle"),
                ));
            }
            let Some(inner_vs) = resolver.value_set(url) else {
                return Err(refuse(
                    side, index, system, RefusalKind::UnresolvableValueSet,
                    format!("referenced value set `{url}` is not resolvable from local/cached content"),
                ));
            };
            guard.insert(key.clone());
            let inner = expand_inner(&inner_vs, resolver, guard).map_err(|e| {
                // A cycle detected deeper stays a CycleGuard refusal (a distinct
                // terminal the editor recognizes); anything else nests.
                let kind = if e.kind == RefusalKind::CycleGuard {
                    RefusalKind::CycleGuard
                } else {
                    RefusalKind::NestedNotEnumerable
                };
                refuse(
                    side, index, system, kind,
                    format!("referenced value set `{url}` is not enumerable: {e}"),
                )
            })?;
            guard.remove(&key);
            for (sys, ver) in &inner.used_systems {
                push_used(&mut out.used_systems, sys.clone(), ver.clone());
            }
            for cr in &inner.copyright {
                if !out.copyright.contains(cr) {
                    out.copyright.push(cr.clone());
                }
            }
            out.experimental |= inner.experimental;
            ref_expansions.push(inner.contains);
        }
        // Multiple valueSet entries → AND (intersection); FHIR compose semantics.
        let mut members: Vec<Concept> = ref_expansions.pop().unwrap_or_default();
        for other in &ref_expansions {
            let keep: BTreeSet<(String, String)> = other
                .iter()
                .map(|c| (c.system.clone(), c.code.clone()))
                .collect();
            members.retain(|c| keep.contains(&(c.system.clone(), c.code.clone())));
        }
        // If a system is ALSO given, intersect with that system (and, if
        // present, its enumerated concepts / decidable filters).
        if let Some(sys) = system {
            if concepts.is_some() || filters.is_some() {
                let sys_side = eval_system_component(
                    sys, concepts, filters, resolver, side, index,
                )?;
                for (s, v) in &sys_side.used_systems {
                    push_used(&mut out.used_systems, s.clone(), v.clone());
                }
                let allowed: BTreeSet<(String, String)> = sys_side
                    .concepts
                    .iter()
                    .map(|c| (c.system.clone(), c.code.clone()))
                    .collect();
                members.retain(|c| allowed.contains(&(c.system.clone(), c.code.clone())));
            } else {
                members.retain(|c| c.system == sys);
            }
        }
        out.concepts = members;
        return Ok(out);
    }

    // --- system-based component (no valueSet) -----------------------------
    let sys = system.expect("system present (checked above)");
    let sys_result = eval_system_component(sys, concepts, filters, resolver, side, index)?;
    Ok(sys_result)
}

/// Evaluate a `system`-based component: enumerated `concept`, whole-system, or a
/// local-CS-decidable `filter`. Fills used-systems + copyright.
fn eval_system_component(
    sys: &str,
    concepts: Option<&Vec<J>>,
    filters: Option<&Vec<J>>,
    resolver: &dyn Resolver,
    side: &'static str,
    index: usize,
) -> Result<ComponentResult, NotEnumerable> {
    let cs = resolver.code_system(sys);
    let cs_version = cs
        .as_ref()
        .and_then(|c| c.get("version").and_then(J::as_str))
        .map(String::from);
    let copyright = cs
        .as_ref()
        .and_then(|c| c.get("copyright").and_then(J::as_str))
        .map(String::from);
    let cs_experimental = cs
        .as_ref()
        .and_then(|c| c.get("experimental").and_then(J::as_bool))
        .unwrap_or(false);

    let mut out = ComponentResult {
        concepts: Vec::new(),
        used_systems: vec![(sys.to_string(), cs_version.clone())],
        copyright: copyright.into_iter().collect(),
        experimental: cs_experimental,
    };

    // Case (a): enumerated concepts. Codes + AUTHORED displays pass through;
    // when no authored display AND the system is a local complete CS, we fill
    // the display from the CS concept. FHIR valueset compose §.
    if let Some(concept_list) = concepts {
        if filters.is_some() {
            // FHIR forbids concept + filter in the same component.
            return Err(refuse(
                side, index, Some(sys), RefusalKind::Malformed,
                "component has both `concept` and `filter` (forbidden by FHIR)",
            ));
        }
        let local_index = cs.as_ref().map(build_concept_index);
        for c in concept_list {
            let Some(code) = c.get("code").and_then(J::as_str) else {
                return Err(refuse(
                    side, index, Some(sys), RefusalKind::Malformed,
                    "`concept` entry has no `code`",
                ));
            };
            let authored = c.get("display").and_then(J::as_str).map(String::from);
            let display = authored.or_else(|| {
                local_index
                    .as_ref()
                    .and_then(|idx| idx.get(code))
                    .and_then(|node| node.display.clone())
            });
            let inactive = local_index
                .as_ref()
                .and_then(|idx| idx.get(code))
                .map(|n| n.inactive)
                .unwrap_or(false);
            out.concepts.push(Concept {
                system: sys.to_string(),
                code: code.to_string(),
                display,
                inactive,
            });
        }
        return Ok(out);
    }

    // Below here we NEED the local complete CodeSystem.
    let Some(cs) = cs else {
        return Err(refuse(
            side, index, Some(sys), RefusalKind::UnresolvableOrIncompleteSystem,
            format!("code system `{sys}` is not available as local/cached content, so its full set cannot be enumerated"),
        ));
    };
    let content = cs.get("content").and_then(J::as_str).unwrap_or("");
    if content != "complete" {
        return Err(refuse(
            side, index, Some(sys), RefusalKind::UnresolvableOrIncompleteSystem,
            format!("code system `{sys}` has content:{} (not `complete`); its full set is not enumerable without a terminology server", if content.is_empty() { "<unset>" } else { content }),
        ));
    }
    let index_map = build_concept_index(&cs);

    // Case (c): filters over the local concept tree.
    if let Some(filter_list) = filters {
        let mut current: Option<BTreeSet<String>> = None; // AND across filters
        for filt in filter_list {
            let codes = eval_local_filter(filt, &index_map, sys, side, index)?;
            current = Some(match current {
                None => codes,
                Some(prev) => prev.intersection(&codes).cloned().collect(),
            });
        }
        let selected = current.unwrap_or_default();
        for code in &selected {
            if let Some(node) = index_map.get(code) {
                out.concepts.push(Concept {
                    system: sys.to_string(),
                    code: code.clone(),
                    display: node.display.clone(),
                    inactive: node.inactive,
                });
            }
        }
        return Ok(out);
    }

    // Case (b): bare `include.system` — the WHOLE complete CS, hierarchy
    // flattened (every concept + nested `concept.concept`).
    for (code, node) in &index_map {
        out.concepts.push(Concept {
            system: sys.to_string(),
            code: code.clone(),
            display: node.display.clone(),
            inactive: node.inactive,
        });
    }
    Ok(out)
}

/// A flattened local CodeSystem concept.
struct CsNode {
    display: Option<String>,
    /// direct-parent codes (from nested `concept.concept` and from `parent`
    /// properties) — used for is-a / descendent-of.
    parents: Vec<String>,
    inactive: bool,
    /// non-hierarchy properties (`property[].code` → value), for `=` filters.
    properties: BTreeMap<String, J>,
}

/// Flatten a complete CodeSystem into `code → node`, capturing hierarchy from
/// BOTH nested `concept.concept` (implicit is-a) and any `parent`/`subsumedBy`
/// concept-properties (explicit is-a). FHIR CodeSystem hierarchy §.
fn build_concept_index(cs: &J) -> BTreeMap<String, CsNode> {
    let mut map: BTreeMap<String, CsNode> = BTreeMap::new();
    if let Some(list) = cs.get("concept").and_then(J::as_array) {
        for c in list {
            flatten_concept(c, None, &mut map);
        }
    }
    map
}

fn flatten_concept(c: &J, parent: Option<&str>, map: &mut BTreeMap<String, CsNode>) {
    let Some(code) = c.get("code").and_then(J::as_str) else {
        return;
    };
    let display = c.get("display").and_then(J::as_str).map(String::from);
    let mut parents: Vec<String> = Vec::new();
    if let Some(p) = parent {
        parents.push(p.to_string());
    }
    let mut inactive = false;
    let mut properties: BTreeMap<String, J> = BTreeMap::new();
    if let Some(props) = c.get("property").and_then(J::as_array) {
        for p in props {
            let Some(pcode) = p.get("code").and_then(J::as_str) else {
                continue;
            };
            let val = property_value(p);
            match pcode {
                // explicit hierarchy properties → additional parents
                "parent" | "subsumedBy" => {
                    if let Some(v) = val.as_ref().and_then(J::as_str) {
                        parents.push(v.to_string());
                    }
                }
                "status" => {
                    if val.as_ref().and_then(J::as_str) == Some("inactive") {
                        inactive = true;
                    }
                    if let Some(v) = val {
                        properties.insert(pcode.to_string(), v);
                    }
                }
                "inactive" => {
                    if val.as_ref().and_then(J::as_bool) == Some(true) {
                        inactive = true;
                    }
                    if let Some(v) = val {
                        properties.insert(pcode.to_string(), v);
                    }
                }
                _ => {
                    if let Some(v) = val {
                        properties.insert(pcode.to_string(), v);
                    }
                }
            }
        }
    }
    // A concept can appear once; nested duplicates merge parents.
    map.entry(code.to_string())
        .and_modify(|n| {
            for p in &parents {
                if !n.parents.contains(p) {
                    n.parents.push(p.clone());
                }
            }
            n.inactive |= inactive;
        })
        .or_insert(CsNode {
            display,
            parents: parents.clone(),
            inactive,
            properties,
        });
    if let Some(children) = c.get("concept").and_then(J::as_array) {
        for child in children {
            flatten_concept(child, Some(code), map);
        }
    }
}

/// Extract the typed value from a `CodeSystem.concept.property` element.
fn property_value(p: &J) -> Option<J> {
    for key in [
        "valueCode",
        "valueCoding",
        "valueString",
        "valueInteger",
        "valueBoolean",
        "valueDateTime",
        "valueDecimal",
    ] {
        if let Some(v) = p.get(key) {
            return Some(v.clone());
        }
    }
    None
}

/// Evaluate ONE `include.filter` over the local concept index, returning the set
/// of matching codes. Only the ops FHIR defines that are decidable from complete
/// local content are implemented; anything else → NotEnumerable.
///
/// Implemented (per <https://hl7.org/fhir/R4/valueset-filter-operator.html>):
/// - `is-a`: the code itself + all transitive descendants (reflexive).
/// - `descendent-of`: transitive descendants, EXCLUDING the code itself.
/// - `=`: exact match on a concept property value (incl. the virtual `concept`
///   property, treated as equality on the code) — decidable locally.
fn eval_local_filter(
    filt: &J,
    index: &BTreeMap<String, CsNode>,
    sys: &str,
    side: &'static str,
    comp_index: usize,
) -> Result<BTreeSet<String>, NotEnumerable> {
    let property = filt.get("property").and_then(J::as_str).unwrap_or("");
    let op = filt.get("op").and_then(J::as_str).unwrap_or("");
    let value = filt.get("value").and_then(J::as_str).unwrap_or("");

    match op {
        "is-a" | "descendent-of" => {
            if !index.contains_key(value) {
                // The anchor code isn't in the local CS — empty set (FHIR: an
                // is-a on an unknown code yields nothing).
                return Ok(BTreeSet::new());
            }
            let mut out = BTreeSet::new();
            if op == "is-a" {
                out.insert(value.to_string());
            }
            collect_descendants(value, index, &mut out);
            Ok(out)
        }
        "=" => {
            // Equality on a concept property. `property == "concept"`? FHIR does
            // not define `=` on `concept`; we support `=` on real properties.
            let mut out = BTreeSet::new();
            for (code, node) in index {
                let matches = node
                    .properties
                    .get(property)
                    .map(|v| property_eq(v, value))
                    .unwrap_or(false);
                if matches {
                    out.insert(code.clone());
                }
            }
            Ok(out)
        }
        other => Err(refuse(
            side,
            comp_index,
            Some(sys),
            RefusalKind::UnsupportedLocalFilter,
            format!(
                "filter op `{other}` on property `{property}` is not decidable by the tier-1 evaluator over local content"
            ),
        )),
    }
}

/// Transitively collect every code whose parent chain reaches `ancestor`
/// (children of `ancestor`, and so on). Does NOT insert `ancestor` itself.
fn collect_descendants(ancestor: &str, index: &BTreeMap<String, CsNode>, out: &mut BTreeSet<String>) {
    // Build once per call would be O(n^2) on deep trees; local CS are tiny, so a
    // simple fixpoint over the parent edges is fine and obviously correct.
    let mut changed = true;
    let mut reached: BTreeSet<String> = BTreeSet::new();
    reached.insert(ancestor.to_string());
    while changed {
        changed = false;
        for (code, node) in index {
            if reached.contains(code) {
                continue;
            }
            if node.parents.iter().any(|p| reached.contains(p)) {
                reached.insert(code.clone());
                out.insert(code.clone());
                changed = true;
            }
        }
    }
}

fn property_eq(v: &J, want: &str) -> bool {
    match v {
        J::String(s) => s == want,
        J::Bool(b) => (want == "true") == *b,
        J::Number(n) => n.to_string() == want,
        _ => false,
    }
}

fn push_used(list: &mut Vec<(String, Option<String>)>, sys: String, ver: Option<String>) {
    if !list.iter().any(|(s, _)| *s == sys) {
        list.push((sys, ver));
    }
}

/// Strip a `|version` suffix from a canonical URL for resolution keying.
pub fn strip_version(url: &str) -> &str {
    url.split('|').next().unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn local_cs() -> J {
        json!({
            "resourceType": "CodeSystem",
            "url": "https://ex.org/cs",
            "version": "1.0.0",
            "caseSensitive": true,
            "content": "complete",
            "copyright": "(c) Example",
            "concept": [
                {"code": "animal", "display": "Animal", "concept": [
                    {"code": "bear", "display": "Bear", "concept": [
                        {"code": "grizzly", "display": "Grizzly"},
                        {"code": "polar", "display": "Polar bear"}
                    ]},
                    {"code": "cat", "display": "Cat", "concept": [
                        {"code": "lion", "display": "Lion"}
                    ]}
                ]}
            ]
        })
    }

    fn resolver_with(cs: J) -> MapResolver {
        let mut r = MapResolver::new();
        r.insert(cs);
        r
    }

    #[test]
    fn class_a_enumerated_external_displays_passthrough() {
        // include.concept over an EXTERNAL system: authored displays pass through,
        // no CS needed.
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/a",
            "compose": {"include": [{"system": "http://snomed.info/sct", "concept": [
                {"code": "271681002", "display": "Stomach ache"},
                {"code": "25064002", "display": "Headache"}
            ]}]}
        });
        let r = MapResolver::new();
        let exp = expand_enumerable(&vs, &r).unwrap();
        let j = exp.to_expansion_json();
        assert_eq!(j["total"], 2);
        // sorted by (system, code): 25064002 before 271681002
        assert_eq!(j["contains"][0]["code"], "25064002");
        assert_eq!(j["contains"][0]["display"], "Headache");
        assert_eq!(j["contains"][1]["code"], "271681002");
    }

    #[test]
    fn class_a_local_concept_fills_display_from_cs() {
        // include.concept over a LOCAL CS with no authored displays → fill from CS.
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/local",
            "compose": {"include": [{"system": "https://ex.org/cs", "concept": [
                {"code": "grizzly"}, {"code": "lion"}
            ]}]}
        });
        let exp = expand_enumerable(&vs, &resolver_with(local_cs())).unwrap();
        let j = exp.to_expansion_json();
        assert_eq!(j["total"], 2);
        assert_eq!(j["contains"][0]["code"], "grizzly");
        assert_eq!(j["contains"][0]["display"], "Grizzly");
        // used-codesystem parameter carries the version.
        assert_eq!(j["parameter"][0]["valueUri"], "https://ex.org/cs|1.0.0");
    }

    #[test]
    fn class_b_whole_local_system_flattens_hierarchy() {
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/whole",
            "compose": {"include": [{"system": "https://ex.org/cs"}]}
        });
        let exp = expand_enumerable(&vs, &resolver_with(local_cs())).unwrap();
        // animal, bear, cat, grizzly, lion, polar = 6 concepts, hierarchy flattened.
        assert_eq!(exp.total(), 6);
        assert_eq!(exp.copyright(), &["(c) Example".to_string()]);
    }

    #[test]
    fn class_c_is_a_reflexive() {
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/isa",
            "compose": {"include": [{"system": "https://ex.org/cs", "filter": [
                {"property": "concept", "op": "is-a", "value": "bear"}
            ]}]}
        });
        let exp = expand_enumerable(&vs, &resolver_with(local_cs())).unwrap();
        let codes: Vec<_> = exp.to_expansion_json()["contains"].as_array().unwrap()
            .iter().map(|c| c["code"].as_str().unwrap().to_string()).collect();
        assert_eq!(codes, vec!["bear", "grizzly", "polar"]); // reflexive
    }

    #[test]
    fn class_c_descendent_of_excludes_self() {
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/desc",
            "compose": {"include": [{"system": "https://ex.org/cs", "filter": [
                {"property": "concept", "op": "descendent-of", "value": "bear"}
            ]}]}
        });
        let exp = expand_enumerable(&vs, &resolver_with(local_cs())).unwrap();
        let codes: Vec<_> = exp.to_expansion_json()["contains"].as_array().unwrap()
            .iter().map(|c| c["code"].as_str().unwrap().to_string()).collect();
        assert_eq!(codes, vec!["grizzly", "polar"]); // NOT bear itself
    }

    #[test]
    fn class_c_property_equals() {
        let cs = json!({
            "resourceType": "CodeSystem", "url": "https://ex.org/props",
            "version": "1", "content": "complete",
            "concept": [
                {"code": "a", "property": [{"code": "kind", "valueCode": "x"}]},
                {"code": "b", "property": [{"code": "kind", "valueCode": "y"}]},
                {"code": "c", "property": [{"code": "kind", "valueCode": "x"}]}
            ]
        });
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/eq",
            "compose": {"include": [{"system": "https://ex.org/props", "filter": [
                {"property": "kind", "op": "=", "value": "x"}
            ]}]}
        });
        let exp = expand_enumerable(&vs, &resolver_with(cs)).unwrap();
        let codes: Vec<_> = exp.to_expansion_json()["contains"].as_array().unwrap()
            .iter().map(|c| c["code"].as_str().unwrap().to_string()).collect();
        assert_eq!(codes, vec!["a", "c"]);
    }

    #[test]
    fn class_d_nested_valueset_ref() {
        let inner = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/inner",
            "compose": {"include": [{"system": "https://ex.org/cs", "filter": [
                {"property": "concept", "op": "is-a", "value": "cat"}
            ]}]}
        });
        let outer = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/outer",
            "compose": {"include": [{"valueSet": ["https://ex.org/vs/inner"]}]}
        });
        let mut r = resolver_with(local_cs());
        r.insert(inner);
        let exp = expand_enumerable(&outer, &r).unwrap();
        let codes: Vec<_> = exp.to_expansion_json()["contains"].as_array().unwrap()
            .iter().map(|c| c["code"].as_str().unwrap().to_string()).collect();
        assert_eq!(codes, vec!["cat", "lion"]);
    }

    #[test]
    fn class_e_exclude_subtracts() {
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/excl",
            "compose": {
                "include": [{"system": "https://ex.org/cs", "filter": [
                    {"property": "concept", "op": "is-a", "value": "bear"}]}],
                "exclude": [{"system": "https://ex.org/cs", "concept": [{"code": "polar"}]}]
            }
        });
        let exp = expand_enumerable(&vs, &resolver_with(local_cs())).unwrap();
        let codes: Vec<_> = exp.to_expansion_json()["contains"].as_array().unwrap()
            .iter().map(|c| c["code"].as_str().unwrap().to_string()).collect();
        assert_eq!(codes, vec!["bear", "grizzly"]); // polar excluded
    }

    #[test]
    fn dedup_across_includes() {
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/dup",
            "compose": {"include": [
                {"system": "https://ex.org/cs", "concept": [{"code": "bear"}]},
                {"system": "https://ex.org/cs", "concept": [{"code": "bear"}, {"code": "cat"}]}
            ]}
        });
        let exp = expand_enumerable(&vs, &resolver_with(local_cs())).unwrap();
        assert_eq!(exp.total(), 2); // bear once
    }

    // ---------- refusal classes ----------

    #[test]
    fn refuse_external_system_filter() {
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/r1",
            "compose": {"include": [{"system": "http://snomed.info/sct", "filter": [
                {"property": "concept", "op": "is-a", "value": "73211009"}
            ]}]}
        });
        let err = expand_enumerable(&vs, &MapResolver::new()).unwrap_err();
        assert_eq!(err.kind, RefusalKind::UnresolvableOrIncompleteSystem);
        assert_eq!(err.component, "include");
        assert_eq!(err.index, 0);
        assert!(err.reason.contains("snomed"));
    }

    #[test]
    fn refuse_incomplete_content() {
        let cs = json!({
            "resourceType": "CodeSystem", "url": "https://ex.org/frag",
            "content": "fragment", "concept": [{"code": "a"}]
        });
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/r2",
            "compose": {"include": [{"system": "https://ex.org/frag"}]}
        });
        let err = expand_enumerable(&vs, &resolver_with(cs)).unwrap_err();
        assert_eq!(err.kind, RefusalKind::UnresolvableOrIncompleteSystem);
        assert!(err.reason.contains("fragment"));
    }

    #[test]
    fn refuse_unresolvable_valueset_ref() {
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/r3",
            "compose": {"include": [{"valueSet": ["https://ex.org/missing"]}]}
        });
        let err = expand_enumerable(&vs, &MapResolver::new()).unwrap_err();
        assert_eq!(err.kind, RefusalKind::UnresolvableValueSet);
    }

    #[test]
    fn refuse_cycle() {
        let a = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/cyc-a",
            "compose": {"include": [{"valueSet": ["https://ex.org/vs/cyc-b"]}]}
        });
        let b = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/cyc-b",
            "compose": {"include": [{"valueSet": ["https://ex.org/vs/cyc-a"]}]}
        });
        let mut r = MapResolver::new();
        r.insert(a.clone());
        r.insert(b);
        let err = expand_enumerable(&a, &r).unwrap_err();
        assert_eq!(err.kind, RefusalKind::CycleGuard);
    }

    #[test]
    fn refuse_unsupported_local_filter() {
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/r4",
            "compose": {"include": [{"system": "https://ex.org/cs", "filter": [
                {"property": "display", "op": "regex", "value": "Bear|Ursine"}
            ]}]}
        });
        let err = expand_enumerable(&vs, &resolver_with(local_cs())).unwrap_err();
        assert_eq!(err.kind, RefusalKind::UnsupportedLocalFilter);
        assert!(err.reason.contains("regex"));
    }

    #[test]
    fn refuse_malformed() {
        let vs = json!({
            "resourceType": "ValueSet", "url": "https://ex.org/vs/r5",
            "compose": {"include": [{"concept": [{"code": "x"}]}]}
        });
        let err = expand_enumerable(&vs, &MapResolver::new()).unwrap_err();
        assert_eq!(err.kind, RefusalKind::Malformed);
    }
}
