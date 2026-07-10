//! P2 equivalence gate: `build_project_in_memory` (the wasm/editor entry point)
//! produces the SAME conformance resources, byte-for-byte, as the disk build
//! (`build_project_with_cache`) — proving the in-memory entry point is the same
//! code path with different input plumbing, not a divergent reimplementation.
//!
//! We drive both paths over real committed IG projects (the `tests/sushi-harvest`
//! cases + the P0 cycle IG when present) and diff the `fsh-generated/resources`
//! JSON files. The IG resource (`ImplementationGuide-*.json`) is disk-only
//! (`build_project_in_memory` deliberately stops before it — it needs
//! IG-project/cache-dir FS scans beyond the `PackageSource` boundary), so it is
//! excluded from the comparison; every OTHER resource must match exactly.
//!
//! The `PackageSource` here is `DiskSource` on both sides: what this test isolates
//! is the config-text / FSH-map / predefined-map plumbing that the wasm build
//! introduced. Byte parity through a `BundleSource` is already proven by the P1
//! `snapshot_gen/tests/bundle_ladder.rs` gate and the P2 wasm parity harness.

use package_store::DiskSource;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Gather `input/fsh/**/*.fsh` as `(path, text)` in the same sorted order the disk
/// build walks (`build_project_inner`'s `walk`): recurse dirs, sort by path.
fn gather_fsh(ig_dir: &Path) -> Vec<(String, String)> {
    let fsh_root = ig_dir.join("input").join("fsh");
    let mut files: Vec<PathBuf> = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if !dir.exists() {
            return;
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.path());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("fsh") {
                out.push(p);
            }
        }
    }
    walk(&fsh_root, &mut files);
    files
        .iter()
        .map(|p| {
            (
                p.to_string_lossy().into_owned(),
                std::fs::read_to_string(p).unwrap(),
            )
        })
        .collect()
}

/// Gather predefined `input/{resources,profiles,...}` JSON bodies in the order the
/// disk `collect_predefined_paths` would visit them: the fixed sub-dir list, each
/// dir's files sorted. (The harvest/cycle IGs use only JSON predefined resources;
/// XML predefined is disk-only and not exercised here.)
fn gather_predefined(ig_dir: &Path) -> Vec<(PathBuf, serde_json::Value)> {
    let input = ig_dir.join("input");
    let mut out = Vec::new();
    for sub in [
        "capabilities",
        "extensions",
        "models",
        "operations",
        "profiles",
        "resources",
        "vocabulary",
        "examples",
    ] {
        let dir = input.join(sub);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        files.sort();
        for f in files {
            if let Ok(bytes) = std::fs::read(&f) {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    out.push((f, v));
                }
            }
        }
    }
    out
}

/// Run the disk build into a temp dir, returning `filename -> text` for every
/// non-IG resource.
fn disk_build(ig_dir: &Path, cache: &str) -> BTreeMap<String, String> {
    let tmp = std::env::temp_dir().join(format!(
        "compile_equiv_{}_{}",
        ig_dir.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    compiler::build_project_with_cache(&ig_dir.to_string_lossy(), &tmp.to_string_lossy(), cache)
        .unwrap();
    let resources = tmp.join("fsh-generated").join("resources");
    let mut out = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(&resources) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("ImplementationGuide-") {
                continue; // disk-only; see module docs
            }
            out.insert(name, std::fs::read_to_string(e.path()).unwrap());
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    out
}

/// Run the in-memory build, returning `filename -> text`.
fn mem_build(ig_dir: &Path, cache: &str) -> BTreeMap<String, String> {
    let cfg_text = std::fs::read_to_string(ig_dir.join("sushi-config.yaml")).unwrap();
    let fsh = gather_fsh(ig_dir);
    let predefined = gather_predefined(ig_dir);
    let compiled =
        compiler::build_project_in_memory(&cfg_text, &fsh, predefined, DiskSource, cache).unwrap();
    compiled.into_iter().map(|r| (r.filename, r.text)).collect()
}

fn assert_equiv(ig_dir: &Path, cache: &str) {
    let disk = disk_build(ig_dir, cache);
    let mem = mem_build(ig_dir, cache);
    let name = ig_dir.file_name().unwrap().to_string_lossy();
    assert_eq!(
        disk.keys().collect::<Vec<_>>(),
        mem.keys().collect::<Vec<_>>(),
        "[{name}] filename set differs (disk vs in-memory)"
    );
    for (fname, disk_text) in &disk {
        let mem_text = mem.get(fname).unwrap();
        assert_eq!(
            disk_text, mem_text,
            "[{name}] resource {fname} differs byte-for-byte between disk and in-memory build"
        );
    }
    assert!(!disk.is_empty(), "[{name}] produced zero resources");
}

#[test]
fn in_memory_matches_disk_on_cycle_and_harvest_igs() {
    let repo = repo();
    let cache = repo.join("temp/fhir-home/.fhir/packages");
    if !cache.is_dir() {
        eprintln!(
            "skipping compile_equiv: no isolated FHIR cache at {}",
            cache.display()
        );
        return;
    }
    let cache = cache.to_string_lossy().into_owned();

    let mut ran = 0usize;

    // The P0 cycle IG (has FSH profiles/VS/CS + predefined input/resources examples
    // + instances) — the richest single case, exercising every resource family.
    let cycle = repo.join("demo/wasm-p0/data/vfs/cycle");
    if cycle.join("sushi-config.yaml").is_file() {
        assert_equiv(&cycle, &cache);
        ran += 1;
    }

    // A spread of committed harvest mini-IGs across resource families.
    for case in [
        "valueset-064",
        "codesystem-021",
        "extension-010",
        "instance-021",
        "alias-002",
        "context-032",
    ] {
        let dir = repo.join("tests/sushi-harvest").join(case);
        if dir.join("sushi-config.yaml").is_file() {
            assert_equiv(&dir, &cache);
            ran += 1;
        }
    }

    assert!(
        ran > 0,
        "compile_equiv ran zero IGs (no cycle, no harvest cases?)"
    );
    eprintln!("compile_equiv: {ran} IGs byte-identical (disk == in-memory)");
}
