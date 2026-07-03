//! Reproduce the `build_site_db` snapshot setup NATIVELY (DiskSource, same 5
//! packages) and assert the per-SD snapshot element counts match the disk
//! `site_db::build` pipeline. Isolates the M2 fidelity residual (browser showed
//! Observation profiles at 60 elements vs the native pipeline's 50) in Rust.

use std::path::PathBuf;

use package_store::DiskSource;
use serde_json::Value;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn gather_fsh(ig_dir: &std::path::Path) -> Vec<(String, String)> {
    let root = ig_dir.join("input").join("fsh");
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut es: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        es.sort_by_key(|e| e.path());
        for e in es {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("fsh") {
                out.push(p);
            }
        }
    }
    let mut fs = Vec::new();
    walk(&root, &mut fs);
    fs.into_iter()
        .map(|p| (p.to_string_lossy().into_owned(), std::fs::read_to_string(&p).unwrap()))
        .collect()
}

#[test]
fn site_db_snapshot_counts_match_disk() {
    let repo = repo();
    let cache = repo.join("temp/fhir-home/.fhir/packages");
    let cycle = repo.join("demo/wasm-p0/data/vfs/cycle");
    if !cache.is_dir() || !cycle.join("sushi-config.yaml").is_file() {
        eprintln!("skip: no cache/cycle");
        return;
    }
    let cache_s = cache.to_string_lossy().into_owned();
    let cfg = std::fs::read_to_string(cycle.join("sushi-config.yaml")).unwrap();
    let fsh = gather_fsh(&cycle);

    // ---- build_site_db-style: compile in memory + IG, snapshot each SD. ----
    let (conformance, _ig, _diag) = compiler::build_project_in_memory_with_ig(
        &cfg,
        &fsh,
        Vec::new(),
        DiskSource,
        &cache_s,
        std::collections::HashMap::new(),
    )
    .unwrap();

    let locals: Vec<(PathBuf, Value)> = conformance
        .iter()
        .map(|r| (PathBuf::from(format!("/__compiled__/{}", r.filename)), r.body.clone()))
        .collect();
    let mut ctx = snapshot_gen::PackageContext::new(&cache, &["hl7.fhir.r4.core#4.0.1".to_string()]).unwrap();
    ctx.load_local_resources(locals);

    let mut mem_counts = std::collections::BTreeMap::new();
    for r in &conformance {
        if r.body.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
            let snap = snapshot_gen::generate_snapshot(r.body.clone(), &ctx, Default::default()).unwrap();
            let id = r.body.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let n = snap.pointer("/snapshot/element").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
            mem_counts.insert(id, n);
        }
    }

    // ---- disk pipeline counts (the oracle). ----
    let tmp = std::env::temp_dir().join(format!("sdsnap_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let cfgb = site_db::BuildConfig {
        ig_dir: cycle.clone(),
        sushi_out: tmp.join("out"),
        cache_dir: cache.clone(),
        out_db: tmp.join("s.db"),
        build_epoch_secs: 1_700_000_000,
        branch: None,
        revision: None,
        run_sushi: true,
        core_package: "hl7.fhir.r4.core#4.0.1".to_string(),
        layer_b: Default::default(),
    };
    let outcome = site_db::build(&cfgb, None).unwrap();
    let mut disk_counts = std::collections::BTreeMap::new();
    for rr in &outcome.db.resources {
        if rr.type_ == "StructureDefinition" {
            let j: Value = serde_json::from_str(&rr.json).unwrap();
            let n = j.pointer("/snapshot/element").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
            disk_counts.insert(rr.id.clone(), n);
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);

    eprintln!("mem : {mem_counts:?}");
    eprintln!("disk: {disk_counts:?}");
    assert_eq!(mem_counts, disk_counts, "snapshot element counts differ (mem vs disk)");
}
