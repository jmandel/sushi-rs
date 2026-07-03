//! Fixture-ladder gate for the walk engine: each fixture's generated snapshot
//! must equal its oracle golden (snapshot.element only, order-sensitive). R5-core
//! fixtures use hl7.fhir.r5.core#5.0.0; the r4-patient fixture uses
//! hl7.fhir.r4.core#4.0.1.

use serde_json::Value;
use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn stable(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().map(stable).collect()),
        Value::Object(m) => {
            let mut keys: Vec<_> = m.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.clone(), stable(&m[key]));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn snapshot_elements(v: &Value) -> Value {
    stable(
        v.get("snapshot")
            .and_then(|s| s.get("element"))
            .unwrap_or(&Value::Array(vec![])),
    )
}

/// The ladder in rung order.
///
/// `r4-patient-card-ms` was re-pinned 2026-07-02: the old golden was in the
/// pre-native-R5 transitional shape (constraint.xpath as a plain field); the
/// coordinator regenerated it with the pinned oracle (`gen-snapshot.sh --r4
/// --sort`) and it now gates the walk engine's R4 load path here.
const LADDER: &[&str] = &[
    "r4-patient-card-ms",
    "r5-patient-min",
    "r5-patient-card-ms",
    "r5-patient-card-ms-unsorted",
    "r5-patient-binding-overlay",
    "r5-patient-fixed-pattern",
    "r5-patient-merge-additive",
    "r5-patient-choice-type",
    "r5-patient-nested-child",
    "r5-patient-simple-slice",
    "r5-patient-slice-child",
    "r5-patient-reslice",
    "r5-patient-type-unfold",
    "r5-extension-simple",
    "r5-observation-reference-profile",
    "r5-real-moneyquantity",
    "r5-questionnaire-content-reference",
];

#[test]
fn walk_ladder_matches_goldens() -> anyhow::Result<()> {
    let repo = repo();
    let cache = repo.join("temp/fhir-home/.fhir/packages");
    if !cache.is_dir() {
        eprintln!("skipping walk ladder: no isolated FHIR cache at {}", cache.display());
        return Ok(());
    }
    let r4_ctx = snapshot_gen::PackageContext::new(&cache, &["hl7.fhir.r4.core#4.0.1".to_string()])?;
    let r5_ctx = snapshot_gen::PackageContext::new(&cache, &["hl7.fhir.r5.core#5.0.0".to_string()])?;

    let mut failures = Vec::new();
    for name in LADDER {
        let fixture_path = repo.join(format!("snapshot/fixtures/{name}.json"));
        let golden_path = repo.join(format!("snapshot/goldens/{name}.snapshot.json"));
        if !golden_path.is_file() {
            eprintln!("skipping {name}: missing golden");
            continue;
        }
        let input: Value = serde_json::from_slice(&std::fs::read(fixture_path)?)?;
        let expected: Value = serde_json::from_slice(&std::fs::read(golden_path)?)?;
        let ctx = if name.starts_with("r4-") { &r4_ctx } else { &r5_ctx };
        let actual = match snapshot_gen::generate_snapshot_walk(input, ctx, Default::default()) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{name}: engine error: {e:#}"));
                continue;
            }
        };
        if snapshot_elements(&expected) != snapshot_elements(&actual) {
            failures.push(format!("{name}: snapshot.element mismatch"));
        }
    }
    assert!(failures.is_empty(), "walk ladder failures:\n{}", failures.join("\n"));
    Ok(())
}
