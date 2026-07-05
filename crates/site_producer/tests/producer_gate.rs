//! Byte-parity gate: produce the US Core stock site's page shells + artifacts.json
//! FROM SOURCE and byte-compare against the publisher's raw `temp/pages` oracle.
//!
//! Oracle = the F0 US Core build's `temp/pages` (the Java IG-Publisher output).
//! The gate is skipped (not failed) when that build tree isn't present, so CI
//! without the F0 corpus stays green; when present, every produced shell must be
//! byte-identical, and artifacts.json must match to the byte.

use std::path::{Path, PathBuf};

fn f0_build() -> Option<PathBuf> {
    for c in [
        "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds/us-core",
        // allow an env override for other machines
    ] {
        let p = PathBuf::from(c);
        if p.join("temp/pages").is_dir() && p.join("template/config.json").is_file() {
            return Some(p);
        }
    }
    std::env::var("USCORE_F0_BUILD").ok().map(PathBuf::from).filter(|p| {
        p.join("temp/pages").is_dir() && p.join("template/config.json").is_file()
    })
}

#[test]
fn page_shells_byte_identical_vs_publisher() {
    let Some(build) = f0_build() else {
        eprintln!("SKIP: US Core F0 build not present");
        return;
    };
    let inputs = site_producer::gather_inputs(&build).expect("gather inputs");
    let out = site_producer::produce(&inputs).expect("produce");

    let pages_root = build.join("temp/pages");
    let mut checked = 0usize;
    let mut mismatch = Vec::new();
    let mut missing = Vec::new();
    for (name, body) in &out.pages {
        let oracle = pages_root.join(name);
        match std::fs::read_to_string(&oracle) {
            Ok(o) => {
                checked += 1;
                if &o != body {
                    mismatch.push(name.clone());
                }
            }
            Err(_) => missing.push(name.clone()),
        }
    }
    eprintln!(
        "page shells: produced {}, checked {}, mismatch {}, not-on-disk {}",
        out.pages.len(),
        checked,
        mismatch.len(),
        missing.len()
    );
    assert!(mismatch.is_empty(), "shell mismatches: {:?}", &mismatch[..mismatch.len().min(10)]);
    assert!(missing.is_empty(), "produced shells absent from oracle: {:?}", &missing[..missing.len().min(10)]);
    assert!(checked > 1000, "expected >1000 shells for US Core, got {checked}");
}

#[test]
fn artifacts_json_byte_identical_vs_publisher() {
    let Some(build) = f0_build() else {
        eprintln!("SKIP: US Core F0 build not present");
        return;
    };
    let inputs = site_producer::gather_inputs(&build).expect("gather inputs");
    let produced = site_producer::data::artifacts_json(&inputs);
    let oracle = std::fs::read_to_string(build.join("temp/pages/_data/artifacts.json"))
        .expect("read oracle artifacts.json");
    assert_eq!(produced, oracle, "artifacts.json byte mismatch");
}

#[test]
fn structuredefinitions_fields_derive() {
    // Field-level (not byte) check: the derivable fields must match the oracle
    // model. Documents the known non-byte gaps (see data.rs module docs).
    let Some(build) = f0_build() else {
        eprintln!("SKIP: US Core F0 build not present");
        return;
    };
    let inputs = site_producer::gather_inputs(&build).expect("gather inputs");
    let model = site_producer::data::structuredefinitions_model(&inputs);
    let oracle: serde_json::Value = serde_json::from_reader(
        std::fs::File::open(build.join("temp/pages/_data/structuredefinitions.json")).unwrap(),
    )
    .unwrap();
    // ignore known gaps: date (Java TZ), and the special `maturities` key.
    let ignore_fields = ["date"];
    let mut field_miss = std::collections::BTreeMap::<String, usize>::new();
    let ob = oracle.as_object().unwrap();
    let mb = model.as_object().unwrap();
    for (id, o) in ob {
        if id == "maturities" {
            continue;
        }
        let Some(m) = mb.get(id) else {
            *field_miss.entry("__no_resource__".into()).or_default() += 1;
            continue;
        };
        for (fld, ov) in o.as_object().unwrap() {
            if ignore_fields.contains(&fld.as_str()) {
                continue;
            }
            if m.get(fld) != Some(ov) {
                *field_miss.entry(fld.clone()).or_default() += 1;
            }
        }
    }
    eprintln!("structuredefinitions field mismatches (non-byte gaps): {field_miss:?}");
    // Load-bearing identity fields MUST be exact:
    for key in ["url", "name", "title", "kind", "type", "status", "derivation", "abstract", "path"] {
        assert_eq!(
            field_miss.get(key).copied().unwrap_or(0),
            0,
            "field `{key}` must derive exactly"
        );
    }
}

#[allow(dead_code)]
fn _assert_path(_p: &Path) {}
