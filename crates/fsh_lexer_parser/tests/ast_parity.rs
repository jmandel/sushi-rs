//! AST parity gate: importing a fixture must yield the SAME import AST as stock
//! SUSHI (`parse-oracle.cjs`). Comparison is SEMANTIC JSON equality (object key
//! order ignored, array order significant) after normalizing `file` fields to
//! their basename (so goldens are path-portable).
//!
//! Goldens: `tests/goldens/ast/<name>.ast.json` (regen `harness/gen-ast-goldens.sh`).
//! Contract: `fsh_lexer_parser::import_to_json(&[(path, content)]) -> serde_json::Value`
//! returns the array-of-FSHDocument JSON in the oracle's shape
//! (`__type` tags, Map->{"__map":..}, bigint->{"__bigint":".."}, id getter->`_id`).

use std::fs;
use std::path::{Path, PathBuf};

use fsh_lexer_parser::import_to_json;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lex")
}
fn goldens_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/ast")
}

/// Normalize for portable comparison: replace every object `"file"` string value
/// with its basename. (Spans/structure are preserved.)
fn normalize(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(f)) = map.get_mut("file") {
                if let Some(base) = Path::new(f.as_str()).file_name().and_then(|s| s.to_str()) {
                    *f = base.to_string();
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
    let golden_raw = fs::read_to_string(goldens_dir().join(format!("{name}.ast.json")))
        .map_err(|e| format!("read golden: {e}"))?;
    let mut expected: serde_json::Value = serde_json::from_str(&golden_raw).unwrap();
    normalize(&mut expected);

    let mut actual = import_to_json(&[(path.to_string_lossy().as_ref(), &content)]);
    normalize(&mut actual);

    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{name}: AST mismatch vs oracle.\n--- expected (oracle) ---\n{}\n--- actual (rust) ---\n{}",
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
    profile_basic    => "01_profile_basic",
    indent_softindex => "02_indent_softindex",
    codes_refs       => "03_codes_refs",
    numbers_strings  => "04_numbers_strings",
    vs_cs            => "05_vs_cs",
    ruleset_insert   => "06_ruleset_insert",
    context_chars    => "07_context_chars",
    caret_regex      => "08_caret_regex",
    nested_param_insert => "09_nested_param_insert",
}
