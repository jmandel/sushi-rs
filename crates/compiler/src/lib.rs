//! FSH->FHIR compiler: tank indexes, insert-rule expansion, fishing, export.
//! Phase 3: global insert-rule expansion (`applyInsertRules`) + FSHTank.
//! See `docs/specs/08-insert-rules-tank.md`. Source of truth:
//! `sushi-ts/src/fhirtypes/common.ts` `applyInsertRules` and
//! `sushi-ts/src/import/FSHTank.ts`.

use fsh_lexer_parser::{dump, parser};
use fsh_model::{FshCode, FshDocument, Rule, SourceInfo, ValueSetComponentFrom};

pub mod config;
pub mod export;
pub mod ig_export;
pub mod instance_export;
pub mod paths;
pub mod predefined;
pub mod sd_export;
pub mod terminology;
pub mod type_resolver;

/// Entity-type discriminant (mirrors TS `constructorName`) used for the
/// `isAllowedRule` table and diagnostic messages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DefKind {
    Profile,
    Extension,
    Logical,
    Resource,
    Instance,
    ValueSet,
    CodeSystem,
    Invariant,
    Mapping,
    RuleSet,
}

impl DefKind {
    fn name(self) -> &'static str {
        match self {
            DefKind::Profile => "Profile",
            DefKind::Extension => "Extension",
            DefKind::Logical => "Logical",
            DefKind::Resource => "Resource",
            DefKind::Instance => "Instance",
            DefKind::ValueSet => "FshValueSet",
            DefKind::CodeSystem => "FshCodeSystem",
            DefKind::Invariant => "Invariant",
            DefKind::Mapping => "Mapping",
            DefKind::RuleSet => "RuleSet",
        }
    }
}

/// Port of `isAllowedRule` (`sushi-ts/src/fshtypes/AllowedRules.ts:31-133`).
/// InsertRule is allowed nowhere (it must be expanded first).
fn is_allowed_rule(kind: DefKind, rule: &Rule) -> bool {
    use Rule::*;
    match kind {
        DefKind::Profile | DefKind::Extension => matches!(
            rule,
            Card { .. }
                | CaretValue { .. }
                | Contains { .. }
                | Assignment { .. }
                | Flag { .. }
                | Obeys { .. }
                | Only { .. }
                | Binding { .. }
        ),
        DefKind::Logical | DefKind::Resource => matches!(
            rule,
            // NB: NO ContainsRule for Logical/Resource.
            AddElement { .. }
                | Card { .. }
                | CaretValue { .. }
                | Assignment { .. }
                | Flag { .. }
                | Obeys { .. }
                | Only { .. }
                | Binding { .. }
        ),
        DefKind::Instance => matches!(rule, Assignment { .. } | Path { .. }),
        DefKind::ValueSet => {
            matches!(rule, VsConcept { .. } | VsFilter { .. } | CaretValue { .. })
        }
        DefKind::CodeSystem => matches!(rule, Concept { .. } | CaretValue { .. }),
        DefKind::Invariant => matches!(rule, Assignment { .. }),
        DefKind::Mapping => matches!(rule, Mapping { .. }),
        DefKind::RuleSet => matches!(
            rule,
            AddElement { .. }
                | Card { .. }
                | CaretValue { .. }
                | Concept { .. }
                | Contains { .. }
                | Assignment { .. }
                | Flag { .. }
                | Mapping { .. }
                | Obeys { .. }
                | Only { .. }
                | VsConcept { .. }
                | VsFilter { .. }
                | Path { .. }
                | Binding { .. }
        ),
    }
}

/// Locator for a RuleSet living inside the tank's documents.
#[derive(Clone, Copy)]
enum RsLoc {
    Plain(usize, usize),
    Applied(usize, usize),
}

fn rs_rules_mut(docs: &mut [FshDocument], loc: RsLoc) -> &mut Vec<Rule> {
    match loc {
        RsLoc::Plain(d, i) => &mut docs[d].rule_sets[i].1.rules,
        RsLoc::Applied(d, i) => &mut docs[d].applied_rule_sets[i].1.rules,
    }
}

fn rs_name(docs: &[FshDocument], loc: RsLoc) -> String {
    match loc {
        RsLoc::Plain(d, i) => docs[d].rule_sets[i].1.name.clone(),
        RsLoc::Applied(d, i) => docs[d].applied_rule_sets[i].1.name.clone(),
    }
}

/// `resolveAlias` (`FSHTank.ts:137-143`): first hit across docs in order.
fn resolve_alias(docs: &[FshDocument], name: &str) -> Option<String> {
    for d in docs {
        for (k, v) in &d.aliases {
            if k == name {
                return Some(v.clone());
            }
        }
    }
    None
}

/// `tank.fish(name, Type.RuleSet)` — RuleSets match by name only, after alias
/// resolution and version-suffix stripping (`FSHTank.ts:225-516`).
fn fish_ruleset(docs: &[FshDocument], name: &str) -> Option<RsLoc> {
    let resolved = resolve_alias(docs, name).unwrap_or_else(|| name.to_string());
    // version split: base is the substring before the first '|'.
    let base = resolved.split('|').next().unwrap_or(&resolved);
    for (d, doc) in docs.iter().enumerate() {
        for (i, (_k, rs)) in doc.rule_sets.iter().enumerate() {
            if rs.name == base {
                return Some(RsLoc::Plain(d, i));
            }
        }
    }
    None
}

/// `tank.fishForAppliedRuleSet(identifier)` (`FSHTank.ts:520-527`): exact
/// identifier key lookup in each doc's `appliedRuleSets`, first hit.
fn fish_applied(docs: &[FshDocument], identifier: &str) -> Option<RsLoc> {
    for (d, doc) in docs.iter().enumerate() {
        for (i, (k, _rs)) in doc.applied_rule_sets.iter().enumerate() {
            if k == identifier {
                return Some(RsLoc::Applied(d, i));
            }
        }
    }
    None
}

/// `JSON.stringify([name, ...params])` — matches V8 for plain strings.
fn json_identifier(name: &str, params: &[String]) -> String {
    let mut v: Vec<&str> = Vec::with_capacity(1 + params.len());
    v.push(name);
    for p in params {
        v.push(p.as_str());
    }
    serde_json::to_string(&v).unwrap()
}

/// Expand the RuleSet at `loc` in place (idempotent), mirroring the recursive
/// `applyInsertRules(ruleSet, tank, [...seen, id])` call in `common.ts:1280`.
fn expand_ruleset(
    docs: &mut Vec<FshDocument>,
    loc: RsLoc,
    seen: &[String],
    owner: Option<&DefinitionLocation>,
    diag: &mut Vec<CompileDiagnostic>,
) {
    // Take the rules out so the borrow of `docs` is free for recursion/fishing.
    let mut rules = std::mem::take(rs_rules_mut(docs, loc));
    expand_rules(docs, &mut rules, DefKind::RuleSet, seen, owner, diag);
    *rs_rules_mut(docs, loc) = rules;
}

/// Core of `applyInsertRules` (`common.ts:1241-1364`). `rules` is owned locally
/// (taken out of its entity), so `docs` can be mutated freely for fishing and
/// recursive RuleSet expansion.
fn expand_rules(
    docs: &mut Vec<FshDocument>,
    rules: &mut Vec<Rule>,
    def_kind: DefKind,
    seen: &[String],
    owner: Option<&DefinitionLocation>,
    diag: &mut Vec<CompileDiagnostic>,
) {
    let original = std::mem::take(rules);
    let mut expanded: Vec<Rule> = Vec::new();

    for rule in original {
        // Non-insert rules pass through unchanged.
        let (insert_file, insert_loc, insert_path, insert_path_array, params, rule_set_name) =
            match &rule {
                Rule::Insert {
                    source_info,
                    path,
                    path_array,
                    params,
                    rule_set,
                } => (
                    source_info.file.clone(),
                    source_info.location.clone(),
                    path.clone(),
                    path_array.clone(),
                    params.clone(),
                    rule_set.clone(),
                ),
                _ => {
                    expanded.push(rule);
                    continue;
                }
            };

        let identifier = json_identifier(&rule_set_name, &params);
        let loc = if !params.is_empty() {
            fish_applied(docs, &identifier)
        } else {
            fish_ruleset(docs, &rule_set_name)
        };

        // SUSHI reports insert-rule diagnostics at the insert-rule's own
        // location; capture it once for every push in this iteration.
        let d_file = insert_file.clone();
        let d_line = insert_loc.as_ref().map(|l| l.start_line);
        let mk =
            |msg: String| CompileDiagnostic::error(msg, d_file.clone(), d_line, owner.cloned());

        let Some(loc) = loc else {
            diag.push(mk(format!(
                "Unable to find definition for RuleSet {rule_set_name}."
            )));
            continue;
        };

        if seen.contains(&identifier) {
            let name = rs_name(docs, loc);
            diag.push(mk(format!(
                "Inserting {name} will cause a circular dependency, so the rule will be ignored"
            )));
            continue;
        }

        // Recurse first: expand the RuleSet (in place, shared) before consuming it.
        let mut new_seen = seen.to_vec();
        new_seen.push(identifier.clone());
        expand_ruleset(docs, loc, &new_seen, owner, diag);

        let mut context = insert_path.clone();
        let mut first_rule = true;
        let n = rs_rules_mut(docs, loc).len();
        for k in 0..n {
            // (a) Stamp appliedFile/appliedLocation on the SHARED RuleSet rule.
            {
                let si = rs_rules_mut(docs, loc)[k].source_info_mut();
                si.applied_file = insert_file.clone();
                si.applied_location = insert_loc.clone();
            }

            // (b) ConceptRule-with-system disambiguation.
            let mut effective = rs_rules_mut(docs, loc)[k].clone();
            if let Rule::Concept {
                system: Some(sys),
                code,
                display,
                ..
            } = &effective
            {
                if def_kind == DefKind::ValueSet {
                    let sys = sys.clone();
                    let fc = FshCode {
                        source_info: SourceInfo::default(),
                        code: code.clone(),
                        system: Some(sys.clone()),
                        display: display.clone(),
                    };
                    effective = Rule::VsConcept {
                        source_info: SourceInfo::default(),
                        path: String::new(),
                        inclusion: true,
                        from: ValueSetComponentFrom {
                            system: Some(sys),
                            value_sets: None,
                        },
                        concepts: vec![fc],
                    };
                } else if def_kind == DefKind::CodeSystem {
                    diag.push(mk(
                        "Do not include the system when listing concepts for a code system."
                            .to_string(),
                    ));
                }
            }

            // (c) Allowed-rule check.
            if !is_allowed_rule(def_kind, &effective) {
                diag.push(mk(format!(
                    "Rule of type {} cannot be applied to entity of type {}",
                    effective.constructor_name(),
                    def_kind.name()
                )));
                continue;
            }

            // (d) Clone (effective is already an owned clone).
            let mut clone = effective;

            // (e) Path prefixing with the insert's path context.
            if !context.is_empty() {
                let clone_path = clone.path().to_string();
                let new_path = if clone_path == "." {
                    diag.push(mk(
                        "The special '.' path is only allowed in top-level rules. The rule will be processed as if it is not indented."
                            .to_string(),
                    ));
                    clone_path
                } else if !clone_path.is_empty() {
                    format!("{context}.{clone_path}")
                } else {
                    context.clone()
                };
                clone.set_path(new_path);
            }

            // (f) Code-hierarchy / caret-path prefixing.
            if !insert_path_array.is_empty() {
                match &mut clone {
                    Rule::Concept { hierarchy, .. } => {
                        // strip leading '#' from each path-array code, then prepend.
                        let mut prefixed: Vec<String> = insert_path_array
                            .iter()
                            .map(|c| c.get(1..).unwrap_or("").to_string())
                            .collect();
                        prefixed.append(hierarchy);
                        *hierarchy = prefixed;
                    }
                    Rule::CaretValue { path_array, .. } => {
                        let mut prefixed = insert_path_array.clone();
                        prefixed.append(path_array);
                        *path_array = prefixed;
                    }
                    _ => {}
                }
            }

            // (g) ConceptRule with context on a CodeSystem -> error (concept still added).
            if matches!(clone, Rule::Concept { .. })
                && def_kind == DefKind::CodeSystem
                && !context.is_empty()
            {
                diag.push(mk(
                    "Do not insert a RuleSet at a path when the RuleSet adds a concept."
                        .to_string(),
                ));
            }

            // (h) Push.
            expanded.push(clone);

            // (i) Soft-index context handoff after the first applied rule.
            if first_rule {
                context = context.replace("[+]", "[=]");
                first_rule = false;
            }
        }
    }

    *rules = expanded;
}

/// Identifies an entity to expand: which per-doc vector to reach into.
#[derive(Clone, Copy)]
enum Field {
    Invariant,
    Profile,
    Extension,
    Logical,
    Resource,
    CodeSystem,
    ValueSet,
    Instance,
    Mapping,
}

impl Field {
    fn def_kind(self) -> DefKind {
        match self {
            Field::Invariant => DefKind::Invariant,
            Field::Profile => DefKind::Profile,
            Field::Extension => DefKind::Extension,
            Field::Logical => DefKind::Logical,
            Field::Resource => DefKind::Resource,
            Field::CodeSystem => DefKind::CodeSystem,
            Field::ValueSet => DefKind::ValueSet,
            Field::Instance => DefKind::Instance,
            Field::Mapping => DefKind::Mapping,
        }
    }

    fn len(self, doc: &FshDocument) -> usize {
        match self {
            Field::Invariant => doc.invariants.len(),
            Field::Profile => doc.profiles.len(),
            Field::Extension => doc.extensions.len(),
            Field::Logical => doc.logicals.len(),
            Field::Resource => doc.resources.len(),
            Field::CodeSystem => doc.code_systems.len(),
            Field::ValueSet => doc.value_sets.len(),
            Field::Instance => doc.instances.len(),
            Field::Mapping => doc.mappings.len(),
        }
    }

    fn rules_mut(self, doc: &mut FshDocument, i: usize) -> &mut Vec<Rule> {
        match self {
            Field::Invariant => &mut doc.invariants[i].1.rules,
            Field::Profile => &mut doc.profiles[i].1.rules,
            Field::Extension => &mut doc.extensions[i].1.rules,
            Field::Logical => &mut doc.logicals[i].1.rules,
            Field::Resource => &mut doc.resources[i].1.rules,
            Field::CodeSystem => &mut doc.code_systems[i].1.rules,
            Field::ValueSet => &mut doc.value_sets[i].1.rules,
            Field::Instance => &mut doc.instances[i].1.rules,
            Field::Mapping => &mut doc.mappings[i].1.rules,
        }
    }

    fn source_info(self, doc: &FshDocument, i: usize) -> &SourceInfo {
        match self {
            Field::Invariant => &doc.invariants[i].1.source_info,
            Field::Profile => &doc.profiles[i].1.source_info,
            Field::Extension => &doc.extensions[i].1.source_info,
            Field::Logical => &doc.logicals[i].1.source_info,
            Field::Resource => &doc.resources[i].1.source_info,
            Field::CodeSystem => &doc.code_systems[i].1.source_info,
            Field::ValueSet => &doc.value_sets[i].1.source_info,
            Field::Instance => &doc.instances[i].1.source_info,
            Field::Mapping => &doc.mappings[i].1.source_info,
        }
    }
}

/// Run `applyInsertRules` over every entity in `FHIRExporter.export` order
/// (`FHIRExporter.ts:38-53`): invariants, then SDs (profiles ++ extensions ++
/// logicals ++ resources), code systems, value sets, instances, mappings.
fn run_global_expansion(docs: &mut Vec<FshDocument>, diag: &mut Vec<CompileDiagnostic>) {
    // Build the processing order as (field, doc_idx, vec_idx) up front so we can
    // then mutate one entity at a time. flatMap-over-docs per field mirrors the
    // FSHTank getAll* iteration order.
    let order: [Field; 9] = [
        Field::Invariant,
        Field::Profile,
        Field::Extension,
        Field::Logical,
        Field::Resource,
        Field::CodeSystem,
        Field::ValueSet,
        Field::Instance,
        Field::Mapping,
    ];
    let mut work: Vec<(Field, usize, usize)> = Vec::new();
    for field in order {
        for (d, doc) in docs.iter().enumerate() {
            for i in 0..field.len(doc) {
                work.push((field, d, i));
            }
        }
    }

    for (field, d, i) in work {
        let owner = DefinitionLocation::from_source_info(field.source_info(&docs[d], i));
        // Take the entity's rules out so `docs` is free for fishing/recursion.
        let mut rules = std::mem::take(field.rules_mut(&mut docs[d], i));
        expand_rules(
            docs,
            &mut rules,
            field.def_kind(),
            &[],
            owner.as_ref(),
            diag,
        );
        *field.rules_mut(&mut docs[d], i) = rules;
    }
}

/// Import the FSH, build the FSHTank, run `applyInsertRules` over every entity in
/// FHIRExporter order, and serialize the POST-EXPANSION import AST to the oracle
/// JSON shape (matching `harness/expand-oracle.cjs`, incl.
/// `appliedFile`/`appliedLocation` on inserted rules). Gated by
/// `tests/expand_parity.rs`.
pub fn expand_to_json(files: &[(&str, &str)]) -> serde_json::Value {
    let mut imp = parser::Importer::new();
    imp.import(files);
    let mut docs = imp.docs;
    let mut diag: Vec<CompileDiagnostic> = Vec::new();
    run_global_expansion(&mut docs, &mut diag);
    dump::dump_docs(&docs)
}

/// Build a SUSHI project: read `sushi-config.yaml` + `input/fsh/**/*.fsh` from
/// `ig_dir`, run the compile pipeline, and write generated resources to
/// `<out_dir>/fsh-generated/resources/<ResourceType>-<id>.json` byte-identically
/// to stock SUSHI. Grows resource-family by family (Phase 4: ValueSet/CodeSystem
/// first). Gate: `harness/diff-resources-glob.sh`.
///
pub fn build_project(ig_dir: &str, out_dir: &str) -> anyhow::Result<()> {
    build_project_inner(ig_dir, out_dir, None)
}

/// Build a SUSHI project against an explicit materialized FHIR package cache
/// (e.g. one produced by `package_acquisition` materialize). The explicit cache
/// is validated to exist; we still NEVER fall back to `~/.fhir`.
pub fn build_project_with_cache(
    ig_dir: &str,
    out_dir: &str,
    cache_dir: &str,
) -> anyhow::Result<()> {
    build_project_inner(ig_dir, out_dir, Some(cache_dir))
}

fn build_project_inner(
    ig_dir: &str,
    out_dir: &str,
    explicit_cache_dir: Option<&str>,
) -> anyhow::Result<()> {
    use std::path::{Path, PathBuf};

    // 1. Config.
    let cfg_path = Path::new(ig_dir).join("sushi-config.yaml");
    let cfg_text = std::fs::read_to_string(&cfg_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", cfg_path.display()))?;

    // 2. Gather all input/fsh/**/*.fsh files (sorted for determinism).
    let fsh_root = Path::new(ig_dir).join("input").join("fsh");
    let mut files: Vec<PathBuf> = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.path());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out)?;
            } else if p.extension().and_then(|s| s.to_str()) == Some("fsh") {
                out.push(p);
            }
        }
        Ok(())
    }
    walk(&fsh_root, &mut files)?;

    let loaded: Vec<(String, String)> = files
        .iter()
        .map(|p| {
            Ok((
                p.to_string_lossy().into_owned(),
                std::fs::read_to_string(p)?,
            ))
        })
        .collect::<anyhow::Result<_>>()?;
    let refs: Vec<(&str, &str)> = loaded
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();

    // The FHIR package cache (needed by VS external-name resolution + SD export).
    let cache_dir = resolve_cache_dir(explicit_cache_dir)?;
    let store = package_store::PackageStore::for_project(ig_dir, &cache_dir)?;
    let cfg_yaml: serde_yaml::Value = serde_yaml::from_str(&cfg_text)?;
    let predefined = predefined::PredefinedPackage::load(ig_dir, &cfg_yaml, &store);

    // 3-4. Import + expansion + export the conformance resources (SD/VS/CS/
    // Instances). This whole compute core is the same code the in-memory
    // `compile_conformance` entry point runs — the only difference is where the
    // config text / FSH sources / predefined resources come from (disk here, a JS
    // Map in the browser) and whether the outputs are written or returned. The
    // native `compile_equiv` test proves the two paths agree byte-for-byte on the
    // cycle IG.
    let compiled = compile_conformance(&cfg_text, &refs, &store, &predefined)?;

    // Write the conformance resources byte-identically.
    let resources_dir = Path::new(out_dir).join("fsh-generated").join("resources");
    std::fs::create_dir_all(&resources_dir)?;
    for res in &compiled.resources {
        std::fs::write(resources_dir.join(&res.filename), &res.text)?;
    }

    // ImplementationGuide resource (last — references all of the above).
    // If FSHOnly is true in the config, do not generate IG content (matches
    // stock SUSHI app.ts: the IGExporter block is skipped entirely under FSHOnly).
    //
    // The IG resource is DISK-ONLY: `ig_export` scans the IG project tree
    // (`input/pagecontent`, predefined re-collection) and the package cache dir
    // (depends-on version resolution) via `std::fs` beyond the package-cache
    // `PackageSource` boundary. The editor's M1 views (JSON / differential /
    // snapshot) do not need it; wiring the IG-project + cache-dir scans through
    // an abstraction is deferred on this disk-only compatibility path. The
    // in-memory `compile_conformance` therefore stops before this step.
    let cfg = config::Config::from_yaml(&cfg_text)?;
    if !cfg.fsh_only {
        use ig_export::IgInputs;
        let inputs = IgInputs {
            conformance: compiled.conformance,
            instances: compiled.instance_ig.iter().collect(),
            local_profile_logical: compiled.local_profile_logical,
            has_custom_resources: compiled.has_custom_resources,
            cache_dir: cache_dir.clone(),
            ig_dir: ig_dir.to_string(),
            predefined: &predefined,
            page_dir_listing: None, // disk path: scan input/** via std::fs (byte-identical).
        };
        if let Some(ig) = ig_export::export_ig(&cfg_yaml, &cfg, &inputs) {
            let text = json_emit::to_fhir_json_string(&ig.body);
            std::fs::write(resources_dir.join(&ig.filename), text)?;
        }
    }
    Ok(())
}

/// A compile-time diagnostic (SUSHI-exact wording) with the source location it
/// refers to, when known. This is the structured form of the `diag` strings the
/// compiler already produces during global insert-rule expansion (and SD export);
/// the editor's worker maps these 1:1 to Monaco markers (see the wasm_api
/// `compile()` result's `diagnostics` array). Additive: collecting these does
/// not change any emitted resource bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileDiagnostic {
    /// `"error"` | `"warning"` | `"info"` — matches SUSHI's severity words.
    pub severity: &'static str,
    /// The SUSHI-exact message text (byte-identical to the string the compiler
    /// already logged internally).
    pub message: String,
    /// The source file the diagnostic points at (as passed to `compile()`),
    /// when the compiler had a span in scope. `None` when unattributable.
    pub file: Option<String>,
    /// 1-based start line, when known.
    pub line: Option<u32>,
    /// Exact authored entity whose compiled/rendered consequence is affected,
    /// when the compiler still has that owner in scope. This is deliberately
    /// absent rather than guessed for unattributable exporter diagnostics.
    pub owner_definition: Option<DefinitionLocation>,
}

impl CompileDiagnostic {
    fn error(
        message: impl Into<String>,
        file: Option<String>,
        line: Option<u32>,
        owner_definition: Option<DefinitionLocation>,
    ) -> Self {
        Self {
            severity: "error",
            message: message.into(),
            file,
            line,
            owner_definition,
        }
    }
}

/// One compiled FHIR resource: the exact output filename SUSHI writes and the
/// byte-identical serialized JSON body (from `json_emit::to_fhir_json_string`).
/// The in-memory `compile_conformance` returns these instead of writing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionKind {
    /// An entity declaration in an authored FSH source file.
    FshDeclaration,
}

/// Exact authored declaration that produced a compiled resource.
///
/// Lines are 1-based and columns are 0-based at this public seam. The parser's
/// `SourceInfo` columns are 1-based, so construction performs the conversion
/// once rather than making every host reinterpret the parser coordinate space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionLocation {
    pub kind: DefinitionKind,
    pub path: String,
    pub line: u32,
    pub column: u32,
}

impl DefinitionLocation {
    fn from_source_info(source_info: &SourceInfo) -> Option<Self> {
        let path = source_info.file.clone()?;
        let location = source_info.location.as_ref()?;
        Some(Self {
            kind: DefinitionKind::FshDeclaration,
            path,
            line: location.start_line,
            column: location.start_column.saturating_sub(1),
        })
    }
}

pub struct CompiledResource {
    pub filename: String,
    pub text: String,
    /// The parsed body, so callers (the wasm API) can feed it to the snapshot
    /// generator or inspect it without re-parsing `text`.
    pub body: serde_json::Value,
    /// Exact entity declaration for authored FSH outputs. Generated resources
    /// such as the ImplementationGuide have no FSH declaration and use `None`.
    pub definition: Option<DefinitionLocation>,
}

/// The full result of the conformance compile core: the byte-identical resource
/// files plus the IG-export metadata the disk path threads into `ig_export`
/// (kept here so the caller that CAN reach the IG-project/cache filesystem can
/// still produce the ImplementationGuide resource).
pub struct CompiledProject {
    pub resources: Vec<CompiledResource>,
    /// SUSHI-exact diagnostics gathered during the compile (insert-rule expansion
    /// + SD export). Additive: independent of `resources`. See `CompileDiagnostic`.
    pub diagnostics: Vec<CompileDiagnostic>,
    conformance: Vec<ig_export::ConformanceRes>,
    instance_ig: Vec<instance_export::IgInstanceMeta>,
    local_profile_logical: std::collections::HashMap<String, String>,
    has_custom_resources: bool,
}

/// The compute core shared by the disk build (`build_project_inner`) and the
/// in-memory wasm build (`build_project_in_memory`): import the FSH, run global
/// insert-rule expansion, and export every conformance resource
/// (ValueSet/CodeSystem, then StructureDefinition, then Instance) in stock
/// `FHIRExporter` order, serialized byte-identically. NO `std::fs` and NO
/// IG-project/cache-dir path assumptions — every package read flows through
/// `store` (a `PackageSource`), config/FSH/predefined are passed in. This is
/// literally the same code the disk path used to run inline; only the input
/// plumbing and the write-vs-return of outputs differ (proven by the
/// `compile_equiv` native test on the cycle IG).
pub fn compile_conformance(
    cfg_text: &str,
    fsh_refs: &[(&str, &str)],
    store: &package_store::PackageStore,
    predefined: &predefined::PredefinedPackage,
) -> anyhow::Result<CompiledProject> {
    let cfg = config::Config::from_yaml(cfg_text)?;

    // Import + global insert-rule expansion.
    let mut imp = parser::Importer::new();
    imp.import(fsh_refs);
    let mut docs = imp.docs;
    let mut diagnostics: Vec<CompileDiagnostic> = Vec::new();
    run_global_expansion(&mut docs, &mut diagnostics);

    use ig_export::ConformanceRes;
    let mut vs_conformance: Vec<ConformanceRes> = Vec::new();
    let mut cs_conformance: Vec<ConformanceRes> = Vec::new();
    let mut resources: Vec<CompiledResource> = Vec::new();

    // ValueSets + CodeSystems (Phase 4).
    let mut exported_conformance: Vec<std::rc::Rc<serde_json::Value>> = Vec::new();
    for exported in export::export_all(&docs, &cfg, Some(store)) {
        let rt = exported
            .body
            .get("resourceType")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let cr = conformance_from_body(&exported.body);
        if rt == "ValueSet" {
            if let Some(c) = cr {
                vs_conformance.push(c);
            }
        } else if rt == "CodeSystem" {
            if let Some(c) = cr {
                cs_conformance.push(c);
            }
        }
        let text = json_emit::to_fhir_json_string(&exported.body);
        resources.push(CompiledResource {
            filename: exported.filename.clone(),
            text,
            body: exported.body.clone(),
            definition: DefinitionLocation::from_source_info(&exported.source_info),
        });
        exported_conformance.push(std::rc::Rc::new(exported.body));
    }

    // StructureDefinitions (Phase 5/6).
    let tank = export::TankIndex::build(&docs, &cfg);
    let vs_url = |s: &str| tank.vs_url(s);
    let cs_url = |s: &str| tank.cs_url(s);
    let predefined_vs = predefined.value_set_url_map();
    let ctx = sd_export::build_sd_context(
        &docs,
        &cfg,
        store,
        predefined,
        &vs_url,
        &cs_url,
        predefined_vs,
    );

    let mut sd_conformance: Vec<ConformanceRes> = Vec::new();
    let mut local_profile_logical: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut has_custom_resources = false;
    let mut seen_sd: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &ctx.exported {
        use fsh_model::StructureKind;
        if e.kind == StructureKind::Resource {
            has_custom_resources = true;
            continue;
        }
        let id = e.sd.get_str("id").unwrap_or("").to_string();
        if !seen_sd.insert(id.clone()) {
            continue;
        }
        let url = e.sd.get_str("url").map(str::to_string);
        let name = sd_ig_name(&e.sd);
        let description = e.sd.get_str("description").map(str::to_string);
        if matches!(e.kind, StructureKind::Profile | StructureKind::Logical) {
            if let Some(u) = &url {
                let ver = e.sd.get_str("version").unwrap_or("").to_string();
                local_profile_logical.insert(u.clone(), ver);
            }
        }
        sd_conformance.push(ConformanceRes {
            reference_key: format!("StructureDefinition/{id}"),
            name,
            description,
            fhir_name: e.sd.get_str("name").map(str::to_string),
            url: url.clone(),
        });
    }
    for exported in sd_export::exported_files(&ctx) {
        let text = json_emit::to_fhir_json_string(&exported.body);
        resources.push(CompiledResource {
            filename: exported.filename.clone(),
            text,
            body: exported.body.clone(),
            definition: DefinitionLocation::from_source_info(&exported.source_info),
        });
    }
    // SD-export diagnostics (mapping-source / caret / obeys resolution). SUSHI
    // reports these without a precise FSH span through this path, so file/line
    // are unattributed; the wording is byte-identical to what SUSHI logs.
    for msg in &ctx.diag {
        diagnostics.push(CompileDiagnostic::error(msg.clone(), None, None, None));
    }

    // Instances (Phase 7).
    let instances = instance_export::export_instances(&docs, &cfg, &ctx, &exported_conformance);
    let mut instance_ig: Vec<instance_export::IgInstanceMeta> = Vec::new();
    for inst in &instances {
        let text = json_emit::to_fhir_json_string(&inst.exported.body);
        resources.push(CompiledResource {
            filename: inst.exported.filename.clone(),
            text,
            body: inst.exported.body.clone(),
            definition: DefinitionLocation::from_source_info(&inst.exported.source_info),
        });
        instance_ig.push(inst.ig.clone());
    }

    // Assemble the IG-export conformance ordering (SD ++ VS ++ CS) the disk path
    // wants, so the caller can generate the IG resource if it can reach the FS.
    let mut conformance = sd_conformance;
    conformance.append(&mut vs_conformance);
    conformance.append(&mut cs_conformance);

    Ok(CompiledProject {
        resources,
        diagnostics,
        conformance,
        instance_ig,
        local_profile_logical,
        has_custom_resources,
    })
}

/// In-memory SUSHI build for the wasm/editor surface (no `std::fs`). Takes the
/// `sushi-config.yaml` text, the FSH sources as `(path, content)` pairs (the
/// caller MUST pass them in the same sorted order the disk walk yields —
/// `input/fsh/**/*.fsh` sorted by path — so import order matches), the predefined
/// `input/resources/**` bodies as `(path, json)` pairs (same order
/// `predefined::collect_predefined_paths` would visit), a package cache
/// `PackageSource` + its cache root, and the project's dependency-resolving
/// config (re-read from `cfg_text`). Returns the byte-identical conformance
/// resources (SD/VS/CS/Instances). Does NOT emit the ImplementationGuide
/// resource (disk-only; see `build_project_inner`).
pub fn build_project_in_memory(
    cfg_text: &str,
    fsh_files: &[(String, String)],
    predefined_resources: Vec<(std::path::PathBuf, serde_json::Value)>,
    source: impl package_store::PackageSource + 'static,
    cache_dir: &str,
) -> anyhow::Result<Vec<CompiledResource>> {
    // Build the store over the mounted package source, resolving deps from the
    // config TEXT (no `std::fs` on the IG project).
    let store = package_store::PackageStore::for_project_with_config(source, cfg_text, cache_dir)?;
    let predefined = predefined::PredefinedPackage::load_from(predefined_resources);
    let refs: Vec<(&str, &str)> = fsh_files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let compiled = compile_conformance(cfg_text, &refs, &store, &predefined)?;
    Ok(compiled.resources)
}

/// Same as [`build_project_in_memory`] but also returns the SUSHI-exact
/// diagnostics the compile produced (see [`CompileDiagnostic`]). The wasm editor
/// worker uses this so a broken FSH file surfaces markers with file/line; the
/// resources are byte-identical to [`build_project_in_memory`] (diagnostics are
/// additive — collected, not resource-affecting).
pub fn build_project_in_memory_with_diagnostics(
    cfg_text: &str,
    fsh_files: &[(String, String)],
    predefined_resources: Vec<(std::path::PathBuf, serde_json::Value)>,
    source: impl package_store::PackageSource + 'static,
    cache_dir: &str,
) -> anyhow::Result<(Vec<CompiledResource>, Vec<CompileDiagnostic>)> {
    let store = package_store::PackageStore::for_project_with_config(source, cfg_text, cache_dir)?;
    let predefined = predefined::PredefinedPackage::load_from(predefined_resources);
    let refs: Vec<(&str, &str)> = fsh_files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let compiled = compile_conformance(cfg_text, &refs, &store, &predefined)?;
    Ok((compiled.resources, compiled.diagnostics))
}

/// In-memory build that ALSO produces the ImplementationGuide resource — the
/// piece the plain [`build_project_in_memory`] omits because `ig_export` used to
/// require filesystem scans (see `build_project_inner`). This additive entry
/// point feeds `ig_export::export_ig` the same data from in-memory inputs:
///  - conformance/instance metadata + predefined come from the compile itself;
///  - the page-folder file NAMES (`input/{pagecontent,pages,resource-docs}`) come
///    from `page_dir_listing` (folder -> filenames) instead of a `std::fs` scan;
///  - dependency resolution (`dependsOn` version pinning) still reads `cache_dir`
///    via the underlying `PackageStore`'s source when the config declares
///    `dependencies:` — pass a real cache root for IGs that have them.
///
/// Returns `(conformance_resources, Option<ImplementationGuide>, diagnostics)`.
/// The conformance resources are byte-identical to
/// [`build_project_in_memory_with_diagnostics`]; the IG resource is byte-identical
/// to the disk path's `ImplementationGuide-<id>.json` for the same inputs (proven
/// by native/in-memory preparation parity tests). `None` IG when config is
/// FSH-only or lacks an `id`.
pub fn build_project_in_memory_with_ig(
    cfg_text: &str,
    fsh_files: &[(String, String)],
    predefined_resources: Vec<(std::path::PathBuf, serde_json::Value)>,
    source: impl package_store::PackageSource + 'static,
    cache_dir: &str,
    page_dir_listing: std::collections::HashMap<String, Vec<String>>,
) -> anyhow::Result<(
    Vec<CompiledResource>,
    Option<CompiledResource>,
    Vec<CompileDiagnostic>,
)> {
    let store = package_store::PackageStore::for_project_with_config(source, cfg_text, cache_dir)?;
    build_project_in_memory_with_ig_from_store(
        cfg_text,
        fsh_files,
        predefined_resources,
        &store,
        page_dir_listing,
    )
}

/// Canonical in-memory IG compilation over a caller-owned package store.
///
/// Long-lived hosts may retain the immutable store's lookup indexes and lazy
/// successful resource parses across project revisions. The ordinary
/// [`build_project_in_memory_with_ig`] entry constructs a store and delegates
/// here, so reuse does not create a second compilation flow or change package
/// fishing precedence.
pub fn build_project_in_memory_with_ig_from_store(
    cfg_text: &str,
    fsh_files: &[(String, String)],
    predefined_resources: Vec<(std::path::PathBuf, serde_json::Value)>,
    store: &package_store::PackageStore,
    page_dir_listing: std::collections::HashMap<String, Vec<String>>,
) -> anyhow::Result<(
    Vec<CompiledResource>,
    Option<CompiledResource>,
    Vec<CompileDiagnostic>,
)> {
    store.require_compatible_config(cfg_text)?;
    let predefined = predefined::PredefinedPackage::load_from(predefined_resources);
    let refs: Vec<(&str, &str)> = fsh_files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let compiled = compile_conformance(cfg_text, &refs, store, &predefined)?;

    let cfg = config::Config::from_yaml(cfg_text)?;
    let cfg_yaml: serde_yaml::Value = serde_yaml::from_str(cfg_text)?;
    let ig_resource = if cfg.fsh_only {
        None
    } else {
        use ig_export::IgInputs;
        let inputs = IgInputs {
            conformance: compiled.conformance,
            instances: compiled.instance_ig.iter().collect(),
            local_profile_logical: compiled.local_profile_logical,
            has_custom_resources: compiled.has_custom_resources,
            cache_dir: store.cache_root().to_string_lossy().into_owned(),
            ig_dir: String::new(), // unused: page_dir_listing supplies the file names.
            predefined: &predefined,
            page_dir_listing: Some(page_dir_listing),
        };
        ig_export::export_ig(&cfg_yaml, &cfg, &inputs).map(|exported| {
            let text = json_emit::to_fhir_json_string(&exported.body);
            CompiledResource {
                filename: exported.filename,
                text,
                body: exported.body,
                definition: None,
            }
        })
    };
    Ok((compiled.resources, ig_resource, compiled.diagnostics))
}

/// Build a `ConformanceRes` from an exported VS/CS body (`name = title ?? name ?? id`).
fn conformance_from_body(body: &serde_json::Value) -> Option<ig_export::ConformanceRes> {
    let rt = body.get("resourceType")?.as_str()?;
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = body
        .get("title")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("name").and_then(|v| v.as_str()))
        .or(Some(id))
        .map(str::to_string);
    let description = body
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(ig_export::ConformanceRes {
        reference_key: format!("{rt}/{id}"),
        name,
        description,
        fhir_name: body
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        url: body.get("url").and_then(|v| v.as_str()).map(str::to_string),
    })
}

/// `name = title ?? name ?? id` for an SD IG entry.
fn sd_ig_name(sd: &fhir_model::StructureDefinition) -> Option<String> {
    sd.get_str("title")
        .or_else(|| sd.get_str("name"))
        .or_else(|| sd.get_str("id"))
        .map(str::to_string)
}

/// Locate the explicit FHIR package cache. Honors `FHIR_CACHE`, else the repo's
/// isolated cache under `temp/fhir-home/.fhir/packages` relative to cwd. NEVER
/// falls back to `~/.fhir` (hard rule). Fails loud if missing.
fn resolve_cache_dir(explicit: Option<&str>) -> anyhow::Result<String> {
    use std::path::Path;
    if let Some(c) = explicit {
        if Path::new(c).is_dir() {
            return Ok(c.to_string());
        }
        anyhow::bail!("FHIR package cache {c} is not a directory");
    }
    if let Ok(c) = std::env::var("FHIR_CACHE") {
        if Path::new(&c).is_dir() {
            return Ok(c);
        }
        anyhow::bail!("FHIR_CACHE={c} is not a directory");
    }
    let default = "temp/fhir-home/.fhir/packages";
    if Path::new(default).is_dir() {
        return Ok(default.to_string());
    }
    anyhow::bail!(
        "FHIR package cache not found. Set FHIR_CACHE or create {default} (never use ~/.fhir)."
    )
}

#[cfg(test)]
mod diagnostic_owner_tests {
    use super::*;

    #[test]
    fn insert_diagnostic_retains_exact_affected_entity_declaration() {
        let mut importer = parser::Importer::new();
        importer.import(&[(
            "input/fsh/Broken.fsh",
            "Profile: Broken\nParent: Patient\n* insert MissingRuleSet\n",
        )]);
        let mut documents = importer.docs;
        let mut diagnostics = Vec::new();
        run_global_expansion(&mut documents, &mut diagnostics);
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.file.as_deref(), Some("input/fsh/Broken.fsh"));
        assert_eq!(diagnostic.line, Some(3));
        assert_eq!(
            diagnostic.owner_definition,
            Some(DefinitionLocation {
                kind: DefinitionKind::FshDeclaration,
                path: "input/fsh/Broken.fsh".into(),
                line: 1,
                column: 0,
            })
        );
    }
}
