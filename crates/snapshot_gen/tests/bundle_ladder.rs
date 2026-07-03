//! P1 bundle-path gate: prove a `package_store::BundleSource` — the shape the
//! browser mounts — drives the snapshot engine to the SAME goldens as the native
//! disk cache, end-to-end.
//!
//! We build the browser bundles for the r4/r5 core packages from the isolated
//! cache (via `package_acquisition::build_bundle` / `read_bundle`), mount them
//! into a `BundleSource`, construct `PackageContext::new_with(bundle, ...)`, and
//! run the fixture ladder. This exercises the whole trait-based read path (index,
//! derived-index sidecar shipped IN the bundle, lazy resource fetch) with zero
//! `std::fs` touching the package cache.

use package_store::BundleSource;
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

/// The same ladder `walk_parity` runs, but every read goes through the bundle.
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

/// Build a `BundleSource` mounting `labels` from the disk cache by going through
/// the real bundle round-trip: `build_bundle` (tar+gzip) -> `read_bundle`
/// (inflate) -> `mount_package`. This is exactly the browser cold-start path.
fn bundle_source_from_cache(cache: &std::path::Path, labels: &[&str]) -> anyhow::Result<BundleSource> {
    let mut src = BundleSource::new();
    for label in labels {
        let package_dir = cache.join(label).join("package");
        let blob = package_acquisition::build_bundle(&package_dir)?;
        let entries = package_acquisition::read_bundle(&blob)?;
        src.mount_package(label, entries);
    }
    Ok(src)
}

#[test]
fn bundle_source_drives_ladder_to_goldens() -> anyhow::Result<()> {
    let repo = repo();
    let cache = repo.join("temp/fhir-home/.fhir/packages");
    if !cache.is_dir() {
        eprintln!(
            "skipping bundle ladder: no isolated FHIR cache at {}",
            cache.display()
        );
        return Ok(());
    }

    // One BundleSource per core; each mounts exactly the package its rungs need.
    let r4_bundle = bundle_source_from_cache(&cache, &["hl7.fhir.r4.core#4.0.1"])?;
    let r5_bundle = bundle_source_from_cache(&cache, &["hl7.fhir.r5.core#5.0.0"])?;

    // Construct the contexts through the trait, using the bundle's synthetic cache
    // root as the cache dir. No disk read of the package cache happens past here.
    let r4_root = r4_bundle.cache_root().to_path_buf();
    let r5_root = r5_bundle.cache_root().to_path_buf();
    let r4_ctx = snapshot_gen::PackageContext::new_with(
        r4_bundle,
        &r4_root,
        &["hl7.fhir.r4.core#4.0.1".to_string()],
    )?;
    let r5_ctx = snapshot_gen::PackageContext::new_with(
        r5_bundle,
        &r5_root,
        &["hl7.fhir.r5.core#5.0.0".to_string()],
    )?;

    let mut failures = Vec::new();
    let mut ran = 0usize;
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
        let actual = match snapshot_gen::generate_snapshot(input, ctx, Default::default()) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{name}: engine error: {e:#}"));
                continue;
            }
        };
        if snapshot_elements(&expected) != snapshot_elements(&actual) {
            failures.push(format!("{name}: snapshot.element mismatch via BundleSource"));
        }
        ran += 1;
    }
    assert!(
        failures.is_empty(),
        "bundle ladder failures:\n{}",
        failures.join("\n")
    );
    assert!(ran > 0, "bundle ladder ran zero rungs (missing goldens?)");
    Ok(())
}
