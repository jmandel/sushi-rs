//! The S1..S7 orchestrator. Consumes ONLY public APIs of the sibling crates:
//! `compiler::build_project_with_cache` (S1/S2) and
//! `snapshot_gen::{generate_snapshot, PackageContext}` (S3). Everything is
//! in-process — no subprocess — which keeps the build deterministic and the
//! ledger honest.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::augment::{augment, AugmentInputs};
use crate::ledger::{BuildLedger, LedgerReport};
use crate::model::{ResourceIdentity, SiteDb};
use crate::rows::{
    apply_global_resource_metadata, derive_concept_rows, derive_metadata_rows,
    derive_resource_rows, populate_core_rows, resource_ref, MetadataInputs,
};

/// Everything needed to produce a site.db.
pub struct BuildConfig {
    /// The cycle IG repo dir (contains sushi-config.yaml + input/).
    pub ig_dir: PathBuf,
    /// A rust_sushi output dir (fsh-generated). If `run_sushi` is true this is
    /// (re)generated; otherwise it is read as-is (must already contain
    /// fsh-generated/resources).
    pub sushi_out: PathBuf,
    /// FHIR package cache (`<cache>/<name>#<ver>/package`). Never ~/.fhir unless
    /// the caller passes it.
    pub cache_dir: PathBuf,
    /// Output site.db path.
    pub out_db: PathBuf,
    /// Injected build timestamp (SOURCE_DATE_EPOCH-style), seconds since epoch.
    /// Drives genDate/genDay and resource `date` — never wall clock.
    pub build_epoch_secs: i64,
    /// git branch string for Metadata.gitstatus (optional).
    pub branch: Option<String>,
    /// git revision string for Metadata.revision (optional).
    pub revision: Option<String>,
    /// Run rust_sushi (compiler) before reading resources. False = consume an
    /// existing fsh-generated dir.
    pub run_sushi: bool,
    /// Distinguished FHIR core coordinate (e.g. `hl7.fhir.r4.core#4.0.1`).
    /// Snapshot base resolution uses the complete exact config/cache closure;
    /// this member controls FHIR-version validation and Layer-B behavior.
    pub core_package: String,
    /// OPT-IN Layer B (task #17): canonical version pinning (B1) + R4-artifact
    /// projection (B0) over the walk snapshots, matching the IG Publisher's
    /// package.db Resources.Json shape. Default OFF (empty = no overlay, so the
    /// pipeline is byte-identical to the pre-Layer-B path). B0 (project_r4) is
    /// applied only when `core_package` is an R4 core (version-conditional).
    pub layer_b: snapshot_gen::LayerBOptions,
}

pub struct BuildOutcome {
    pub db: SiteDb,
    pub ledger: LedgerReport,
    /// The snapshot-complete resources dir written for downstream consumers
    /// (the TS oracle reads the same files → row parity).
    pub resources_dir: PathBuf,
}

fn resource_type(r: &Value) -> &str {
    r.get("resourceType").and_then(Value::as_str).unwrap_or("")
}

/// build.ts:176 — typeRank ordering.
fn type_rank(t: &str) -> u32 {
    match t {
        "ImplementationGuide" => 0,
        "CodeSystem" => 1,
        "StructureDefinition" => 2,
        "ValueSet" => 3,
        "Bundle" => 4,
        "Observation" => 5,
        _ => 100,
    }
}

fn read_json_files(dir: &Path) -> Result<Vec<(PathBuf, Value)>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let v: Value =
            serde_json::from_str(text).with_context(|| format!("parse {}", path.display()))?;
        out.push((path, v));
    }
    Ok(out)
}

/// The full pipeline. Returns the row model + ledger + resources dir.
pub fn build(config: &BuildConfig, prior_ledger: Option<&BuildLedger>) -> Result<BuildOutcome> {
    let resources_dir = config.sushi_out.join("fsh-generated").join("resources");

    // ---- S1/S2: compile FSH -> resource JSONs (differential SDs + IG). ----
    if config.run_sushi {
        compiler::build_project_with_cache(
            &config.ig_dir.to_string_lossy(),
            &config.sushi_out.to_string_lossy(),
            &config.cache_dir.to_string_lossy(),
        )
        .context("rust_sushi build (compiler::build_project_with_cache)")?;
    }
    if !resources_dir.is_dir() {
        bail!(
            "no fsh-generated/resources at {} (run with run_sushi=true or point --sushi-out at a build)",
            resources_dir.display()
        );
    }

    // Resolve once from the same config and explicit cache the compiler used.
    // S3 must see the complete exact closure: a profile may derive from an
    // external dependency, while `core_package` remains only the distinguished
    // Layer-B/FHIR-version member rather than the whole snapshot context.
    let sushi_config_path = config.ig_dir.join("sushi-config.yaml");
    let sushi_config_yaml = std::fs::read_to_string(&sushi_config_path)
        .with_context(|| format!("read {}", sushi_config_path.display()))?;
    let package_context = resolve_snapshot_package_context(
        &sushi_config_yaml,
        &config.cache_dir,
        &config.core_package,
    )?;

    // ---- S3: snapshot-complete the StructureDefinitions in place. ----
    // Feed local SDs so cross-profile bases (fact <- bleeding <- flow) resolve.
    let ctx = snapshot_gen::PackageContext::new(&config.cache_dir, &package_context).with_context(
        || {
            format!(
                "open package cache {} with {}",
                config.cache_dir.display(),
                config.core_package
            )
        },
    )?;
    let mut ctx = ctx;
    ctx.load_local_dir(&resources_dir)
        .context("load local SD dir for snapshot base resolution")?;

    let mut ledger = BuildLedger::new();

    let generated = read_json_files(&resources_dir)?;
    for (path, resource) in &generated {
        if resource_type(resource) != "StructureDefinition" {
            continue;
        }
        // Snapshot node dep = the differential SD bytes (coarse but honest: the
        // walk closure is recorded implicitly by the base package pin).
        let src = std::fs::read(path)?;
        let node = format!("snapshot:{}", resource_ref(resource));
        let input_hash = BuildLedger::hash(&src);
        // Layer A (walk) then the OPT-IN Layer-B overlay. With layer_b all-OFF
        // (default) this is byte-identical to plain generate_snapshot.
        let out = snapshot_gen::generate_snapshot_layer_b(
            resource.clone(),
            &ctx,
            snapshot_gen::SnapshotOptions::default(),
            layer_b_for(config),
        )
        .with_context(|| format!("generate_snapshot for {}", path.display()))?;
        let out_bytes = json_emit::to_fhir_json_string(&out).into_bytes();
        // Only rewrite the file when the snapshot output changed (in-place update,
        // §2c S7 discipline; also lets a no-op rebuild write nothing).
        let existing = std::fs::read(path).ok();
        if existing.as_deref() != Some(out_bytes.as_slice()) {
            std::fs::write(path, &out_bytes)?;
        }
        ledger.record(&node, &input_hash, &BuildLedger::hash(&out_bytes));
    }

    // ---- Load the snapshot-complete resource set (generated + examples). ----
    let examples_dir = config.ig_dir.join("input").join("resources");
    let generated = read_json_files(&resources_dir)?; // re-read: SDs now snapshot-complete
    let primary_ig_filename = primary_implementation_guide_filename(&sushi_config_yaml)?;
    let primary_implementation_guide = generated
        .iter()
        .find(|(path, _)| {
            path.file_name().and_then(|name| name.to_str()) == Some(primary_ig_filename.as_str())
        })
        .map(|(_, resource)| resource.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "generated primary ImplementationGuide file {primary_ig_filename} is absent"
            )
        })?;
    let examples = read_json_files(&examples_dir)?;
    let generated: Vec<Value> = generated.into_iter().map(|(_, v)| v).collect();
    let examples: Vec<Value> = examples.into_iter().map(|(_, v)| v).collect();

    // ---- S5 + S6 assembly (shared with the in-memory path). ----
    let db = assemble_rows(
        &AssembleInputs {
            generated: &generated,
            primary_implementation_guide: &primary_implementation_guide,
            examples: &examples,
            sushi_config_yaml: &sushi_config_yaml,
            build_epoch_secs: config.build_epoch_secs,
            branch: config.branch.clone(),
            revision: config.revision.clone(),
            pagecontent_dir: config.ig_dir.join("input").join("pagecontent"),
            image_dir: config.ig_dir.join("input").join("images"),
            liquid_asset_dirs: liquid_asset_dirs(&config.ig_dir),
            files: &crate::augment::DiskFiles,
        },
        &mut ledger,
    )?;

    let report = ledger.finish(prior_ledger);
    Ok(BuildOutcome {
        db,
        ledger: report,
        resources_dir,
    })
}

fn primary_implementation_guide_filename(config_text: &str) -> Result<String> {
    let config: serde_yaml::Value = serde_yaml::from_str(config_text)?;
    let id = config
        .get("id")
        .and_then(serde_yaml::Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("sushi-config.yaml has no primary IG id"))?;
    if id.contains('/') || id.contains('\\') || id.contains('\0') {
        bail!("sushi-config.yaml has an unsafe primary IG id {id:?}");
    }
    Ok(format!("ImplementationGuide-{id}.json"))
}

fn resolve_snapshot_package_context(
    config_text: &str,
    cache_dir: &Path,
    expected_core: &str,
) -> Result<Vec<String>> {
    let source = package_store::DiskSource;
    let index = package_store::version_index_from_cache(&source, cache_dir);
    let step = package_store::resolve_project(config_text, &source, cache_dir, Some(&index))
        .context("resolve exact package closure for snapshot completion")?;
    if !step.satisfied {
        bail!(
            "package cache does not satisfy snapshot closure: {}",
            serde_json::to_string(&step.missing)?
        );
    }
    let mut labels = Vec::new();
    for request in step.compile_set.iter().chain(&step.context_closure) {
        let label = format!("{}#{}", request.package_id, request.version);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    if !labels.iter().any(|label| label == expected_core) {
        bail!(
            "resolved snapshot closure does not contain distinguished core package {expected_core}"
        );
    }
    Ok(labels)
}

/// Inputs to the shared S5+S6 assembly (used by both the disk `build()` and the
/// in-memory `build_from_inputs()`). Every value is already in memory — no file
/// reads happen here except through `files` (S6's `FileSource`).
pub struct AssembleInputs<'a> {
    /// The generated/conformance resources, snapshot-complete, INCLUDING the IG.
    pub generated: &'a [Value],
    /// The compiler's separately identified generated primary guide. Other
    /// authored/generated ImplementationGuide instances remain ordinary
    /// resources and never compete for this role.
    pub primary_implementation_guide: &'a Value,
    /// Example resources (input/resources/**), if any.
    pub examples: &'a [Value],
    /// Raw sushi-config.yaml text (consumed twice: row derivation + verbatim S6).
    pub sushi_config_yaml: &'a str,
    pub build_epoch_secs: i64,
    pub branch: Option<String>,
    pub revision: Option<String>,
    /// input/pagecontent dir (a base path `files` joins slugs under).
    pub pagecontent_dir: PathBuf,
    /// input/images dir.
    pub image_dir: PathBuf,
    /// project.liquidAssetDirs (include search roots).
    pub liquid_asset_dirs: Vec<PathBuf>,
    /// The S6 read source (disk or in-memory VFS).
    pub files: &'a dyn crate::augment::FileSource,
}

/// S5 (row derivation) + S6 (augmentation) over an in-memory resource set. This is
/// the byte-for-byte body the disk `build()` used to run inline; extracting it lets
/// the wasm/editor path produce identical rows without touching the filesystem for
/// the resource load or config read (only S6 reads, through `input.files`). The
/// `ledger` is populated with resource/page/config/asset nodes as before.
pub fn assemble_rows(input: &AssembleInputs, ledger: &mut BuildLedger) -> Result<SiteDb> {
    let ig = input.primary_implementation_guide.clone();
    if resource_type(&ig) != "ImplementationGuide" {
        bail!("primary guide input is not an ImplementationGuide resource");
    }
    let primary_implementation_guide = ResourceIdentity {
        resource_type: "ImplementationGuide".into(),
        id: ig
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("generated ImplementationGuide has no id"))?
            .to_string(),
    };

    // build.ts:167 igResourceMetadata: reference -> IG definition.resource entry.
    let mut resource_meta: HashMap<String, Value> = HashMap::new();
    if let Some(Value::Array(list)) = ig.pointer("/definition/resource") {
        for r in list {
            if let Some(reference) = r.pointer("/reference/reference").and_then(Value::as_str) {
                resource_meta.insert(reference.to_string(), r.clone());
            }
        }
    }

    let cfg_yaml: serde_yaml::Value = serde_yaml::from_str(input.sushi_config_yaml)?;
    let cfg: Value = serde_yaml::from_value(cfg_yaml)?;

    // ---- Ordering + apply-global-metadata (build.ts loadResources). ----
    let now_fhir = crate::timefmt::fhir_datetime(input.build_epoch_secs);
    let mut by_ref: indexmap_pairs::OrderPairs = indexmap_pairs::OrderPairs::new();
    for r in input.generated.iter().chain(input.examples.iter()) {
        if resource_type(r).is_empty() || r.get("id").and_then(Value::as_str).is_none() {
            continue;
        }
        by_ref.insert(resource_ref(r), r.clone());
    }
    // ig first, then IG.definition.resource order, then the rest by typeRank/id.
    let mut ordered: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let push =
        |ordered: &mut Vec<Value>, seen: &mut std::collections::HashSet<String>, r: Value| {
            let reff = resource_ref(&r);
            if seen.insert(reff) {
                ordered.push(r);
            }
        };
    push(&mut ordered, &mut seen, ig.clone());
    if let Some(Value::Array(list)) = ig.pointer("/definition/resource") {
        for r in list {
            if let Some(reference) = r.pointer("/reference/reference").and_then(Value::as_str) {
                if let Some(res) = by_ref.get(reference) {
                    push(&mut ordered, &mut seen, res.clone());
                }
            }
        }
    }
    // Remaining, sorted by typeRank then id (build.ts:346).
    let mut rest: Vec<Value> = by_ref.values();
    rest.sort_by(|a, b| {
        type_rank(resource_type(a))
            .cmp(&type_rank(resource_type(b)))
            .then_with(|| {
                a.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(b.get("id").and_then(Value::as_str).unwrap_or(""))
            })
    });
    for r in rest {
        push(&mut ordered, &mut seen, r);
    }
    // final sort (build.ts:349) — the whole ordered set, then apply metadata.
    ordered.sort_by(|a, b| {
        type_rank(resource_type(a))
            .cmp(&type_rank(resource_type(b)))
            .then_with(|| {
                a.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(b.get("id").and_then(Value::as_str).unwrap_or(""))
            })
    });
    let resources: Vec<Value> = ordered
        .into_iter()
        .map(|r| apply_global_resource_metadata(r, &cfg, &now_fhir))
        .collect();

    // ---- S5: derive rows. Json blob = byte-stable compact serialization. ----
    let json_by_index: Vec<String> = resources
        .iter()
        .map(|r| serde_json::to_string(r).expect("resource serializes"))
        .collect();
    for (i, r) in resources.iter().enumerate() {
        ledger.record(
            &format!("resource:{}", resource_ref(r)),
            "",
            &BuildLedger::hash(json_by_index[i].as_bytes()),
        );
    }

    let gen_date = crate::timefmt::gen_date(input.build_epoch_secs);
    let gen_day = crate::timefmt::gen_day(input.build_epoch_secs);
    let metadata_rows = derive_metadata_rows(&MetadataInputs {
        cfg: &cfg,
        ig: &ig,
        gen_date,
        gen_day,
        branch: input.branch.clone(),
        revision: input.revision.clone(),
    });
    let (resource_rows, key_by_ref) =
        derive_resource_rows(&resources, &resource_meta, &cfg, &json_by_index);
    let concept_rows = derive_concept_rows(&resources, &key_by_ref);

    let mut db = SiteDb::default();
    db.primary_implementation_guide = Some(primary_implementation_guide);
    populate_core_rows(&mut db, metadata_rows, resource_rows, concept_rows);

    // ---- S6: augment (Pages/Menu/SiteConfig/Assets). ----
    augment(
        &mut db,
        &AugmentInputs {
            ig: &ig,
            sushi_config_yaml: input.sushi_config_yaml,
            pagecontent_dir: input.pagecontent_dir.clone(),
            image_dir: input.image_dir.clone(),
            liquid_asset_dirs: input.liquid_asset_dirs.clone(),
            files: input.files,
        },
    )
    .context("S6 augmentation")?;

    // ---- Ledger nodes for S6 inputs (pages/menu/config/assets). ----
    for p in &db.pages {
        let body = p.body.clone().unwrap_or_default();
        ledger.record(
            &format!("page:{}", p.slug),
            "",
            &BuildLedger::hash(body.as_bytes()),
        );
    }
    ledger.record(
        "config:sushi-config",
        "",
        &BuildLedger::hash(input.sushi_config_yaml.as_bytes()),
    );
    for a in &db.assets {
        ledger.record(
            &format!("asset:{}", a.name),
            "",
            &BuildLedger::hash(&a.content),
        );
    }

    Ok(db)
}

/// Inputs to the in-memory site.db producer (the wasm/editor path). The caller
/// (wasm_api) has already run S1/S2 (compile) + S3 (snapshot) in memory and holds
/// the snapshot-complete resources + the IG resource. This runs S5+S6 and returns
/// the row model + ledger — no filesystem access except through `vfs`.
pub struct InMemoryInputs<'a> {
    /// Snapshot-complete generated/conformance resources INCLUDING the IG.
    pub generated: &'a [Value],
    /// The compiler's separately identified primary generated guide.
    pub primary_implementation_guide: &'a Value,
    /// Example resources (input/resources/**), if any.
    pub examples: &'a [Value],
    /// Raw sushi-config.yaml text.
    pub sushi_config_yaml: &'a str,
    pub build_epoch_secs: i64,
    pub branch: Option<String>,
    pub revision: Option<String>,
    /// The editor VFS as an absolute `path -> bytes` map, keyed under `ig_root`
    /// (so `<ig_root>/input/pagecontent/index.md` resolves). Holds pagecontent,
    /// images, and any liquid includes.
    pub vfs: std::collections::BTreeMap<PathBuf, Vec<u8>>,
    /// The synthetic IG root the VFS paths are joined under (e.g. `/ig`).
    pub ig_root: PathBuf,
    /// project.liquidAssetDirs, relative to `ig_root` (e.g. `input/includes`).
    pub liquid_asset_rel_dirs: Vec<String>,
}

/// Produce the site.db row model from fully in-memory inputs (S5+S6). Returns the
/// rows + a fresh ledger report (no prior ledger — the editor rebuilds per edit;
/// incremental replay is future work). Byte/JSON-identical to the disk `build()`
/// row set for the same IG (minus BuildState timestamps), which the native
/// `inmem_vs_disk` parity test asserts.
pub fn build_from_inputs(input: &InMemoryInputs) -> Result<BuildOutcome> {
    let mut ledger = BuildLedger::new();
    let files = crate::augment::MemFiles::new(input.vfs.clone());
    let db = assemble_rows(
        &AssembleInputs {
            generated: input.generated,
            primary_implementation_guide: input.primary_implementation_guide,
            examples: input.examples,
            sushi_config_yaml: input.sushi_config_yaml,
            build_epoch_secs: input.build_epoch_secs,
            branch: input.branch.clone(),
            revision: input.revision.clone(),
            pagecontent_dir: input.ig_root.join("input").join("pagecontent"),
            image_dir: input.ig_root.join("input").join("images"),
            liquid_asset_dirs: input
                .liquid_asset_rel_dirs
                .iter()
                .map(|d| input.ig_root.join(d))
                .collect(),
            files: &files,
        },
        &mut ledger,
    )?;
    let report = ledger.finish(None);
    Ok(BuildOutcome {
        db,
        ledger: report,
        resources_dir: PathBuf::new(),
    })
}

/// Resolve the effective Layer-B options for this build. B1 (pin) is applied as
/// requested; B0 (project_r4) is gated on an R4 core package (version-conditional
/// — an R5 IG gets no projection, audit §2). `core_package` like
/// `hl7.fhir.r4.core#4.0.1` -> R4.
fn layer_b_for(config: &BuildConfig) -> snapshot_gen::LayerBOptions {
    let is_r4 = config.core_package.starts_with("hl7.fhir.r4.core#4");
    snapshot_gen::LayerBOptions {
        pin: config.layer_b.pin,
        project_r4: config.layer_b.project_r4 && is_r4,
    }
}

/// project.liquidAssetDirs default for cycle: input/includes (§ project/cycle.ts).
fn liquid_asset_dirs(ig_dir: &Path) -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("SITE_LIQUID_ASSET_DIRS") {
        return v
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| ig_dir.join(s))
            .collect();
    }
    vec![ig_dir.join("input").join("includes")]
}

/// Order-preserving key->Value store (asset/resource dedup keeping first key
/// order and last value, mirroring JS `Map.set`).
mod indexmap_pairs {
    use serde_json::Value;
    pub struct OrderPairs {
        order: Vec<String>,
        map: std::collections::HashMap<String, Value>,
    }
    impl OrderPairs {
        pub fn new() -> Self {
            Self {
                order: Vec::new(),
                map: std::collections::HashMap::new(),
            }
        }
        pub fn insert(&mut self, key: String, value: Value) {
            if !self.map.contains_key(&key) {
                self.order.push(key.clone());
            }
            self.map.insert(key, value);
        }
        pub fn get(&self, key: &str) -> Option<&Value> {
            self.map.get(key)
        }
        pub fn values(&self) -> Vec<Value> {
            self.order
                .iter()
                .filter_map(|k| self.map.get(k).cloned())
                .collect()
        }
    }
}
