//! Stage-2 (R4 SD JSON -> R5 internal-model JSON) parity gate.
//!
//! For every `snapshot/converted-goldens/**/*.converted.json` oracle golden,
//! find its R4 fixture (`snapshot/harvested/r4/<ig>/fixtures/<name>.json`), run
//! `convert_r4_sd_to_r5`, and compare against the golden with an ORDER-SENSITIVE
//! recursive comparator (serde_json object equality ignores key order, so we
//! walk keys positionally and fail on the first divergence with a path).
//!
//! Goldens are Java-pretty (CRLF, `" : "` separators): we parse both sides to
//! `serde_json::Value` (preserve_order) and never byte-compare.

use serde_json::Value;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Order-sensitive recursive comparison. Returns `Err(path)` at the first
/// divergence (type mismatch, key-set/key-order mismatch, array length, or scalar
/// difference), else `Ok(())`.
fn order_sensitive_eq(actual: &Value, expected: &Value, path: &str) -> Result<(), String> {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            let a_keys: Vec<&String> = a.keys().collect();
            let e_keys: Vec<&String> = e.keys().collect();
            if a_keys != e_keys {
                return Err(format!(
                    "{path}: key order/set differs\n  actual:   {a_keys:?}\n  expected: {e_keys:?}"
                ));
            }
            for k in a_keys {
                order_sensitive_eq(&a[k], &e[k], &format!("{path}.{k}"))?;
            }
            Ok(())
        }
        (Value::Array(a), Value::Array(e)) => {
            if a.len() != e.len() {
                return Err(format!(
                    "{path}: array length {} vs expected {}",
                    a.len(),
                    e.len()
                ));
            }
            for (i, (av, ev)) in a.iter().zip(e.iter()).enumerate() {
                order_sensitive_eq(av, ev, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        (a, e) if a == e => Ok(()),
        (a, e) => Err(format!("{path}: {a} != {e}")),
    }
}

/// Map a golden path to its R4 fixture: goldens live at
/// `converted-goldens/<ig>/<name>.converted.json`, fixtures at
/// `harvested/r4/<ig>/fixtures/<name>.json`.
fn fixture_for(golden: &Path, repo: &Path) -> PathBuf {
    let ig = golden
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let name = golden
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_suffix(".converted.json")
        .unwrap();
    repo.join(format!("snapshot/harvested/r4/{ig}/fixtures/{name}.json"))
}

fn collect_goldens(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_goldens(&path, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".converted.json"))
        {
            out.push(path);
        }
    }
}

#[test]
fn converted_goldens_parity() -> anyhow::Result<()> {
    let repo = repo();
    let goldens_dir = repo.join("snapshot/converted-goldens");
    let mut goldens = Vec::new();
    collect_goldens(&goldens_dir, &mut goldens);
    goldens.sort();
    assert!(
        !goldens.is_empty(),
        "no converted goldens found under {}",
        goldens_dir.display()
    );

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for golden in &goldens {
        let fixture = fixture_for(golden, &repo);
        assert!(
            fixture.is_file(),
            "missing R4 fixture {} for golden {}",
            fixture.display(),
            golden.display()
        );
        let r4: Value = serde_json::from_slice(&std::fs::read(&fixture)?)?;
        let expected: Value = serde_json::from_slice(&std::fs::read(golden)?)?;
        let name = golden.file_name().and_then(|n| n.to_str()).unwrap_or("<?>");
        match snapshot_gen::convert_r4_sd_to_r5(&r4) {
            Ok(actual) => {
                if let Err(divergence) = order_sensitive_eq(&actual, &expected, name) {
                    failures.push(divergence);
                }
            }
            Err(err) => failures.push(format!("{name}: conversion errored: {err:#}")),
        }
        checked += 1;
    }

    assert!(
        failures.is_empty(),
        "{}/{} converted goldens diverged:\n{}",
        failures.len(),
        checked,
        failures.join("\n")
    );
    eprintln!("convert_parity: {checked}/{checked} goldens matched");
    Ok(())
}
