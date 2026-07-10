//! M2 gate (i): the IN-MEMORY site.db row producer (`build_from_inputs`, the
//! wasm/editor path) produces the SAME `SiteDb` rows as the DISK producer
//! (`build`, the native pipeline) for the cycle IG — JSON-identical, minus the
//! BuildState timestamps (`genDate`/`genDay`/resource `date`, which are injected
//! and identical here because we inject the same `build_epoch_secs`).
//!
//! This isolates S5/S6 assembly parity: both sides consume the SAME
//! snapshot-complete resource set (the disk build writes it; we load it back for
//! the in-memory side) and the SAME sushi-config + pagecontent/images/examples,
//! so any row divergence is a bug in the in-memory assembly, not in compile or
//! snapshot. (Compile/snapshot parity is proven by `compile_equiv` +
//! `snapshot_gen` gates.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read_json_dir(dir: &Path) -> Vec<Value> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files
        .iter()
        .filter_map(|p| {
            let t = std::fs::read_to_string(p).ok()?;
            let t = t.strip_prefix('\u{feff}').unwrap_or(&t);
            serde_json::from_str(t).ok()
        })
        .collect()
}

/// Build the in-memory VFS the in-memory augment reads (pagecontent/images/
/// resources), keyed under `<ig_root>/<project-relative path>`.
fn build_vfs(ig_dir: &Path, ig_root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut vfs = BTreeMap::new();
    for sub in ["pagecontent", "images", "includes", "resources"] {
        let dir = ig_dir.join("input").join(sub);
        collect_into(
            &dir,
            &ig_dir.join("input"),
            ig_root.join("input").as_path(),
            &mut vfs,
        );
    }
    vfs
}

fn collect_into(
    dir: &Path,
    strip_base: &Path,
    dest_base: &Path,
    out: &mut BTreeMap<PathBuf, Vec<u8>>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() {
            collect_into(&p, strip_base, dest_base, out);
        } else if p.is_file() {
            if let Ok(rel) = p.strip_prefix(strip_base) {
                if let Ok(bytes) = std::fs::read(&p) {
                    out.insert(dest_base.join(rel), bytes);
                }
            }
        }
    }
}

/// Serialize a SiteDb to a canonical JSON `Value` for comparison.
fn db_json(db: &site_db::SiteDb) -> Value {
    serde_json::to_value(db).unwrap()
}

#[test]
fn inmem_rows_match_disk_rows_on_cycle() {
    let repo = repo();
    let cache = repo.join("temp/fhir-home/.fhir/packages");
    let cycle = repo.join("demo/wasm-p0/data/vfs/cycle");
    if !cache.is_dir() || !cycle.join("sushi-config.yaml").is_file() {
        eprintln!("skipping inmem_vs_disk: no cache or cycle IG present");
        return;
    }

    let epoch = 1_700_000_000i64; // fixed injected timestamp (determinism).
    let tmp = std::env::temp_dir().join(format!("inmem_vs_disk_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    // ---- Disk producer: full pipeline, returns rows + writes snapshot-complete SDs. ----
    let config = site_db::BuildConfig {
        ig_dir: cycle.clone(),
        sushi_out: tmp.join("out"),
        cache_dir: cache.clone(),
        out_db: tmp.join("site.db"),
        build_epoch_secs: epoch,
        branch: None,
        revision: None,
        run_sushi: true,
        core_package: "hl7.fhir.r4.core#4.0.1".to_string(),
        layer_b: Default::default(),
    };
    let mut disk_ledger = site_db::BuildLedger::new();
    let _ = &mut disk_ledger;
    let disk_outcome = site_db::build(&config, None).expect("disk build");
    let disk_json = db_json(&disk_outcome.db);

    // ---- In-memory producer: SAME snapshot-complete resources + examples + VFS. ----
    let resources_dir = disk_outcome.resources_dir.clone();
    let generated = read_json_dir(&resources_dir);
    let examples = read_json_dir(&cycle.join("input").join("resources"));
    let cfg_text = std::fs::read_to_string(cycle.join("sushi-config.yaml")).unwrap();
    let ig_root = PathBuf::from("/ig");
    let vfs = build_vfs(&cycle, &ig_root);

    let mem_outcome = site_db::build_from_inputs(&site_db::InMemoryInputs {
        generated: &generated,
        examples: &examples,
        sushi_config_yaml: &cfg_text,
        build_epoch_secs: epoch,
        branch: None,
        revision: None,
        vfs,
        ig_root,
        liquid_asset_rel_dirs: vec!["input/includes".to_string()],
    })
    .expect("in-memory build");
    let mem_json = db_json(&mem_outcome.db);

    let _ = std::fs::remove_dir_all(&tmp);

    // The whole row model must be JSON-identical (timestamps are equal by
    // construction — same injected epoch).
    if disk_json != mem_json {
        // Pinpoint the first differing top-level table for a useful failure.
        for key in [
            "metadata",
            "resources",
            "concepts",
            "valueSetCodes",
            "pages",
            "menu",
            "siteConfig",
            "assets",
        ] {
            let d = disk_json.get(key);
            let m = mem_json.get(key);
            assert_eq!(d, m, "site.db table `{key}` differs (disk vs in-memory)");
        }
        panic!("site.db rows differ but per-table scan found no diff (shape mismatch)");
    }

    let n_res = disk_outcome.db.resources.len();
    let n_pages = disk_outcome.db.pages.len();
    assert!(n_res > 0, "cycle produced zero resources");
    eprintln!("inmem_vs_disk: cycle rows identical — {n_res} resources, {n_pages} pages");
}
