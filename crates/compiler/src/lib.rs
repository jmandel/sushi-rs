//! FSH->FHIR compiler: tank indexes, insert-rule expansion, fishing, export.
//! Phase 3: global insert-rule expansion (`applyInsertRules`) + FSHTank.
//! See `docs/specs/08-insert-rules-tank.md`. Source of truth:
//! `sushi-ts/src/fhirtypes/common.ts` `applyInsertRules` and
//! `sushi-ts/src/import/FSHTank.ts`.

use fsh_lexer_parser::{dump, parser};
use fsh_model::{FshCode, FshDocument, Rule, SourceInfo, ValueSetComponentFrom};

pub mod caret_schema;
pub mod config;
pub mod export;
pub mod instance_export;
pub mod paths;
pub mod sd_export;

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
fn expand_ruleset(docs: &mut Vec<FshDocument>, loc: RsLoc, seen: &[String], diag: &mut Vec<String>) {
    // Take the rules out so the borrow of `docs` is free for recursion/fishing.
    let mut rules = std::mem::take(rs_rules_mut(docs, loc));
    expand_rules(docs, &mut rules, DefKind::RuleSet, seen, diag);
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
    diag: &mut Vec<String>,
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

        let Some(loc) = loc else {
            diag.push(format!(
                "Unable to find definition for RuleSet {rule_set_name}."
            ));
            continue;
        };

        if seen.contains(&identifier) {
            let name = rs_name(docs, loc);
            diag.push(format!(
                "Inserting {name} will cause a circular dependency, so the rule will be ignored"
            ));
            continue;
        }

        // Recurse first: expand the RuleSet (in place, shared) before consuming it.
        let mut new_seen = seen.to_vec();
        new_seen.push(identifier.clone());
        expand_ruleset(docs, loc, &new_seen, diag);

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
                    diag.push(
                        "Do not include the system when listing concepts for a code system."
                            .to_string(),
                    );
                }
            }

            // (c) Allowed-rule check.
            if !is_allowed_rule(def_kind, &effective) {
                diag.push(format!(
                    "Rule of type {} cannot be applied to entity of type {}",
                    effective.constructor_name(),
                    def_kind.name()
                ));
                continue;
            }

            // (d) Clone (effective is already an owned clone).
            let mut clone = effective;

            // (e) Path prefixing with the insert's path context.
            if !context.is_empty() {
                let clone_path = clone.path().to_string();
                let new_path = if clone_path == "." {
                    diag.push(
                        "The special '.' path is only allowed in top-level rules. The rule will be processed as if it is not indented."
                            .to_string(),
                    );
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
                diag.push(
                    "Do not insert a RuleSet at a path when the RuleSet adds a concept.".to_string(),
                );
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
}

/// Run `applyInsertRules` over every entity in `FHIRExporter.export` order
/// (`FHIRExporter.ts:38-53`): invariants, then SDs (profiles ++ extensions ++
/// logicals ++ resources), code systems, value sets, instances, mappings.
fn run_global_expansion(docs: &mut Vec<FshDocument>, diag: &mut Vec<String>) {
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
        // Take the entity's rules out so `docs` is free for fishing/recursion.
        let mut rules = std::mem::take(field.rules_mut(&mut docs[d], i));
        expand_rules(docs, &mut rules, field.def_kind(), &[], diag);
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
    let mut diag: Vec<String> = Vec::new();
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
    use std::path::{Path, PathBuf};

    // 1. Config.
    let cfg_path = Path::new(ig_dir).join("sushi-config.yaml");
    let cfg_text = std::fs::read_to_string(&cfg_path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", cfg_path.display()))?;
    let cfg = config::Config::from_yaml(&cfg_text)?;

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
        .map(|p| Ok((p.to_string_lossy().into_owned(), std::fs::read_to_string(p)?)))
        .collect::<anyhow::Result<_>>()?;
    let refs: Vec<(&str, &str)> = loaded.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();

    // 3. Import + global insert-rule expansion.
    let mut imp = parser::Importer::new();
    imp.import(&refs);
    let mut docs = imp.docs;
    let mut diag: Vec<String> = Vec::new();
    run_global_expansion(&mut docs, &mut diag);

    // 4. Export resources and write byte-identical JSON.
    let resources_dir = Path::new(out_dir).join("fsh-generated").join("resources");
    std::fs::create_dir_all(&resources_dir)?;

    // ValueSets + CodeSystems (Phase 4).
    for exported in export::export_all(&docs, &cfg) {
        let text = json_emit::to_fhir_json_string(&exported.body);
        std::fs::write(resources_dir.join(&exported.filename), text)?;
    }

    // StructureDefinitions (Phase 5/6): needs the FHIR package cache.
    let cache_dir = resolve_cache_dir()?;
    let store = package_store::PackageStore::for_project(ig_dir, &cache_dir)?;
    let tank = export::TankIndex::build(&docs, &cfg);
    let vs_url = |s: &str| tank.vs_url(s);
    let cs_url = |s: &str| tank.cs_url(s);
    let ctx = sd_export::build_sd_context(&docs, &cfg, &store, &vs_url, &cs_url);
    for exported in sd_export::exported_files(&ctx) {
        let text = json_emit::to_fhir_json_string(&exported.body);
        std::fs::write(resources_dir.join(&exported.filename), text)?;
    }

    // Instances (Phase 7).
    for exported in instance_export::export_instances(&docs, &cfg, &ctx) {
        let text = json_emit::to_fhir_json_string(&exported.body);
        std::fs::write(resources_dir.join(&exported.filename), text)?;
    }
    Ok(())
}

/// Locate the explicit FHIR package cache. Honors `FHIR_CACHE`, else the repo's
/// isolated cache under `temp/fhir-home/.fhir/packages` relative to cwd. NEVER
/// falls back to `~/.fhir` (hard rule). Fails loud if missing.
fn resolve_cache_dir() -> anyhow::Result<String> {
    use std::path::Path;
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
