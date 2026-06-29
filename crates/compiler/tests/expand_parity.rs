//! Insert-expansion parity gate (Phase 3): importing a fixture and running
//! `applyInsertRules` over all entities must yield the SAME post-expansion import
//! AST as stock SUSHI (`harness/expand-oracle.cjs`). Semantic JSON equality,
//! `file`/`appliedFile` normalized to basename.
//!
//! Goldens: `tests/goldens/expand/<name>.expand.json` (regen
//! `harness/gen-expand-goldens.sh`).
//! Contract: `compiler::expand_to_json(&[(path, content)]) -> serde_json::Value`.

use std::fs;
use std::path::{Path, PathBuf};

use compiler::expand_to_json;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/expand")
}
fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/expand")
}

fn normalize(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            for key in ["file", "appliedFile"] {
                if let Some(serde_json::Value::String(f)) = map.get_mut(key) {
                    if let Some(b) = Path::new(f.as_str()).file_name().and_then(|s| s.to_str()) {
                        *f = b.to_string();
                    }
                }
            }
            for (_k, val) in map.iter_mut() {
                normalize(val);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(normalize),
        _ => {}
    }
}

fn check_fixture(name: &str) -> Result<(), String> {
    let path = fixtures_dir().join(format!("{name}.fsh"));
    let content = fs::read_to_string(&path).map_err(|e| format!("read fixture: {e}"))?;
    let golden_raw = fs::read_to_string(goldens_dir().join(format!("{name}.expand.json")))
        .map_err(|e| format!("read golden: {e}"))?;
    let mut expected: serde_json::Value = serde_json::from_str(&golden_raw).unwrap();
    normalize(&mut expected);

    let mut actual = expand_to_json(&[(path.to_string_lossy().as_ref(), &content)]);
    normalize(&mut actual);

    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{name}: post-expansion AST mismatch vs oracle.\n--- expected ---\n{}\n--- actual ---\n{}",
            serde_json::to_string_pretty(&expected).unwrap(),
            serde_json::to_string_pretty(&actual).unwrap(),
        ))
    }
}

macro_rules! parity_tests {
    ($($fn_name:ident => $fixture:literal),+ $(,)?) => {
        $(#[test] fn $fn_name() { if let Err(e) = check_fixture($fixture) { panic!("{e}"); } })+
    };
}

parity_tests! {
    plain_insert     => "e01_plain_insert",
    param_insert     => "e02_param_insert",
    softindex_handoff => "e03_softindex_handoff",
    circular         => "e04_circular",
    code_hierarchy   => "e05_code_hierarchy",
    concept_duality  => "e06_concept_duality",
    nested           => "e07_nested",
}
