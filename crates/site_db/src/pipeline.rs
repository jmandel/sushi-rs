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
use crate::model::SiteDb;
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
    /// FHIR core package coordinate for snapshot base resolution (e.g.
    /// "hl7.fhir.r4.core#4.0.1").
    pub core_package: String,
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
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let v: Value = serde_json::from_str(text)
            .with_context(|| format!("parse {}", path.display()))?;
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

    // ---- S3: snapshot-complete the StructureDefinitions in place. ----
    // Feed local SDs so cross-profile bases (fact <- bleeding <- flow) resolve.
    let ctx = snapshot_gen::PackageContext::new(
        &config.cache_dir,
        std::slice::from_ref(&config.core_package),
    )
    .with_context(|| {
        format!(
            "open package cache {} with {}",
            config.cache_dir.display(),
            config.core_package
        )
    })?;
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
        let out = snapshot_gen::generate_snapshot(
            resource.clone(),
            &ctx,
            snapshot_gen::SnapshotOptions::default(),
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
    let examples = read_json_files(&examples_dir)?;

    let ig = generated
        .iter()
        .map(|(_, v)| v)
        .find(|r| resource_type(r) == "ImplementationGuide")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no ImplementationGuide in {}", resources_dir.display()))?;

    // build.ts:167 igResourceMetadata: reference -> IG definition.resource entry.
    let mut resource_meta: HashMap<String, Value> = HashMap::new();
    if let Some(Value::Array(list)) = ig.pointer("/definition/resource") {
        for r in list {
            if let Some(reference) = r.pointer("/reference/reference").and_then(Value::as_str) {
                resource_meta.insert(reference.to_string(), r.clone());
            }
        }
    }

    // ---- Config (parsed sushi-config.yaml, for row derivation only). ----
    let sushi_config_path = config.ig_dir.join("sushi-config.yaml");
    let sushi_config_yaml = std::fs::read_to_string(&sushi_config_path)
        .with_context(|| format!("read {}", sushi_config_path.display()))?;
    let cfg_yaml: serde_yaml::Value = serde_yaml::from_str(&sushi_config_yaml)?;
    let cfg: Value = serde_yaml::from_value(cfg_yaml)?;

    // ---- Ordering + apply-global-metadata (build.ts loadResources). ----
    let now_fhir = crate::timefmt::fhir_datetime(config.build_epoch_secs);
    let mut by_ref: indexmap_pairs::OrderPairs = indexmap_pairs::OrderPairs::new();
    for (_, r) in generated.iter().chain(examples.iter()) {
        if resource_type(r).is_empty() || r.get("id").and_then(Value::as_str).is_none() {
            continue;
        }
        by_ref.insert(resource_ref(r), r.clone());
    }
    // ig first, then IG.definition.resource order, then the rest by typeRank/id.
    let mut ordered: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let push = |ordered: &mut Vec<Value>, seen: &mut std::collections::HashSet<String>, r: Value| {
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

    let gen_date = crate::timefmt::gen_date(config.build_epoch_secs);
    let gen_day = crate::timefmt::gen_day(config.build_epoch_secs);
    let metadata_rows = derive_metadata_rows(&MetadataInputs {
        cfg: &cfg,
        ig: &ig,
        gen_date,
        gen_day,
        branch: config.branch.clone(),
        revision: config.revision.clone(),
    });
    let (resource_rows, key_by_ref) =
        derive_resource_rows(&resources, &resource_meta, &cfg, &json_by_index);
    let concept_rows = derive_concept_rows(&resources, &key_by_ref);

    let mut db = SiteDb::default();
    populate_core_rows(&mut db, metadata_rows, resource_rows, concept_rows);

    // ---- S6: augment (Pages/Menu/SiteConfig/Assets). ----
    augment(
        &mut db,
        &AugmentInputs {
            ig: &ig,
            sushi_config_yaml: &sushi_config_yaml,
            pagecontent_dir: config.ig_dir.join("input").join("pagecontent"),
            image_dir: config.ig_dir.join("input").join("images"),
            liquid_asset_dirs: liquid_asset_dirs(&config.ig_dir),
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
        &BuildLedger::hash(sushi_config_yaml.as_bytes()),
    );
    for a in &db.assets {
        ledger.record(
            &format!("asset:{}", a.name),
            "",
            &BuildLedger::hash(&a.content),
        );
    }

    let report = ledger.finish(prior_ledger);
    Ok(BuildOutcome {
        db,
        ledger: report,
        resources_dir,
    })
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
