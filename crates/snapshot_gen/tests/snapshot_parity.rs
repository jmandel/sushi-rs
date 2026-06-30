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

fn differential_elements(v: &Value) -> &[Value] {
    v.get("differential")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[test]
fn fixtures_match_oracle_snapshot_elements() -> anyhow::Result<()> {
    let repo = repo();
    let cache = repo.join("temp/fhir-home/.fhir/packages");
    if !cache.is_dir() {
        eprintln!(
            "skipping snapshot oracle parity: no isolated FHIR cache at {}",
            cache.display()
        );
        return Ok(());
    }
    let r4_packages = vec!["hl7.fhir.r4.core#4.0.1".to_string()];
    let r5_packages = vec!["hl7.fhir.r5.core#5.0.0".to_string()];
    let r4_ctx = snapshot_gen::PackageContext::new(&cache, &r4_packages)?;
    let r5_ctx = snapshot_gen::PackageContext::new(&cache, &r5_packages)?;

    for name in [
        "r4-patient-card-ms",
        "r5-extension-simple",
        "r5-observation-reference-profile",
        "r5-patient-binding-overlay",
        "r5-patient-fixed-pattern",
        "r5-patient-choice-type",
        "r5-patient-merge-additive",
        "r5-patient-min",
        "r5-patient-nested-child",
        "r5-patient-reslice",
        "r5-patient-slice-child",
        "r5-patient-simple-slice",
        "r5-patient-type-unfold",
        "r5-questionnaire-content-reference",
        "r5-patient-card-ms",
        "r5-patient-card-ms-unsorted",
        "r5-real-moneyquantity",
    ] {
        let fixture_path = repo.join(format!("snapshot/fixtures/{name}.json"));
        let golden_path = repo.join(format!("snapshot/goldens/{name}.snapshot.json"));
        if !golden_path.is_file() {
            eprintln!("skipping {name}: missing {}", golden_path.display());
            continue;
        }
        let input: Value = serde_json::from_slice(&std::fs::read(fixture_path)?)?;
        let expected: Value = serde_json::from_slice(&std::fs::read(golden_path)?)?;
        let ctx = if name.starts_with("r4-") {
            &r4_ctx
        } else {
            &r5_ctx
        };
        let actual = snapshot_gen::generate_snapshot(input.clone(), &ctx, Default::default())?;

        assert_eq!(
            snapshot_elements(&expected),
            snapshot_elements(&actual),
            "{name}"
        );

        let expected_diff_paths: Vec<_> = differential_elements(&expected)
            .iter()
            .map(|e| e.get("path").and_then(Value::as_str))
            .collect();
        let actual_diff_paths: Vec<_> = differential_elements(&actual)
            .iter()
            .map(|e| e.get("path").and_then(Value::as_str))
            .collect();
        assert_eq!(
            expected_diff_paths, actual_diff_paths,
            "{name} differential order"
        );
    }
    Ok(())
}
