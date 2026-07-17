use package_store::BundleSource;
use serde_json::json;
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

fn empty_context() -> anyhow::Result<snapshot_gen::PackageContext> {
    let source = BundleSource::new();
    let root = source.cache_root().to_path_buf();
    snapshot_gen::PackageContext::new_with(source, root, &[])
}

fn minimal_logical(title: &str) -> Value {
    json!({
        "resourceType": "StructureDefinition",
        "id": "Document",
        "url": "https://example.org/StructureDefinition/Document",
        "version": "1.0.0",
        "name": "Document",
        "title": title,
        "status": "draft",
        "description": format!("Envelope for {title}"),
        "fhirVersion": "5.0.0",
        "kind": "logical",
        "abstract": false,
        "type": "Document",
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Base|5.0.0",
        "derivation": "specialization",
        "differential": { "element": [{ "id": "Document", "path": "Document" }] }
    })
}

#[test]
fn retained_snapshot_recomposes_the_exact_current_metadata_envelope() -> anyhow::Result<()> {
    let context = empty_context()?;
    let first = minimal_logical("First title");
    let current = minimal_logical("Current title");
    let first_input = snapshot_gen::prepare_snapshot_derivation(first.clone(), Default::default())?;
    let mut current_input =
        snapshot_gen::prepare_snapshot_derivation(current.clone(), Default::default())?;

    let (_, retained_derivation) =
        snapshot_gen::generate_prepared_snapshot_derivation(first_input, &context)?;
    let mut structurally_mismatched = current.clone();
    structurally_mismatched["differential"]["element"][0]["min"] = json!(1);
    let mut structurally_mismatched =
        snapshot_gen::prepare_snapshot_derivation(structurally_mismatched, Default::default())?;
    assert!(retained_derivation
        .try_recompose(&mut structurally_mismatched, &context)?
        .is_none());
    let mut mismatched = snapshot_gen::prepare_snapshot_derivation(
        current.clone(),
        snapshot_gen::SnapshotOptions {
            sort_differential: false,
        },
    )?;
    assert!(retained_derivation
        .try_recompose(&mut mismatched, &context)?
        .is_none());
    let recomposed = retained_derivation
        .try_recompose(&mut current_input, &context)?
        .expect("matching input and dependencies reuse the snapshot");
    let canonical = snapshot_gen::generate_snapshot(current, &context, Default::default())?;

    assert_eq!(
        serde_json::to_vec(&recomposed)?,
        serde_json::to_vec(&canonical)?,
        "reuse must preserve exact current values and object-key ordering"
    );
    assert_eq!(
        recomposed.get("title").and_then(Value::as_str),
        Some("Current title")
    );
    assert_eq!(
        recomposed.get("description").and_then(Value::as_str),
        Some("Envelope for Current title")
    );
    Ok(())
}

#[test]
fn retained_r4_snapshot_recomposes_after_full_current_conversion() -> anyhow::Result<()> {
    let context = empty_context()?;
    let mut first = minimal_logical("First R4 title");
    first["fhirVersion"] = json!("4.0.1");
    first["baseDefinition"] = json!("http://hl7.org/fhir/StructureDefinition/Base|4.0.1");
    let mut current = first.clone();
    current["title"] = json!("Current R4 title");
    current["description"] = json!("Current R4 description");

    let first_input = snapshot_gen::prepare_snapshot_derivation(first, Default::default())?;
    let mut current_input =
        snapshot_gen::prepare_snapshot_derivation(current.clone(), Default::default())?;
    let (_, derivation) =
        snapshot_gen::generate_prepared_snapshot_derivation(first_input, &context)?;
    let recomposed = derivation
        .try_recompose(&mut current_input, &context)?
        .expect("matching R4 input and dependencies reuse the snapshot");
    let canonical = snapshot_gen::generate_snapshot(current, &context, Default::default())?;
    assert_eq!(
        serde_json::to_vec(&recomposed)?,
        serde_json::to_vec(&canonical)?,
        "R4 reuse must run the same conversion and preserve exact key order"
    );
    let keys = recomposed
        .as_object()
        .expect("completed resource is an object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let snapshot = keys.iter().position(|key| *key == "snapshot").unwrap();
    let differential = keys.iter().position(|key| *key == "differential").unwrap();
    assert_eq!(snapshot + 1, differential);
    Ok(())
}

#[test]
fn structural_snapshot_inputs_change_the_derivation_identity() -> anyhow::Result<()> {
    let context = empty_context()?;
    let base = minimal_logical("Envelope only");
    let baseline = snapshot_gen::prepare_snapshot_derivation(base.clone(), Default::default())?;
    let (_, derivation) = snapshot_gen::generate_prepared_snapshot_derivation(baseline, &context)?;
    let mut different_sort = snapshot_gen::prepare_snapshot_derivation(
        base.clone(),
        snapshot_gen::SnapshotOptions {
            sort_differential: false,
        },
    )?;
    assert!(
        derivation
            .try_recompose(&mut different_sort, &context)?
            .is_none(),
        "differential ordering mode is part of the derivation recipe"
    );
    for (field, changed) in [
        ("resourceType", json!("OtherResource")),
        ("id", json!("OtherId")),
        (
            "url",
            json!("https://example.org/StructureDefinition/Other"),
        ),
        ("version", json!("2.0.0")),
        ("name", json!("OtherName")),
        ("fhirVersion", json!("5.0.1")),
        ("kind", json!("complex-type")),
        ("abstract", json!(true)),
        ("type", json!("OtherType")),
        (
            "baseDefinition",
            json!("http://hl7.org/fhir/StructureDefinition/Element"),
        ),
        ("derivation", json!("constraint")),
        (
            "differential",
            json!({"element":[{"id":"Document","path":"Document","min":1}]}),
        ),
    ] {
        let mut candidate = base.clone();
        candidate[field] = changed;
        let mut candidate =
            snapshot_gen::prepare_snapshot_derivation(candidate, Default::default())?;
        assert!(
            derivation
                .try_recompose(&mut candidate, &context)?
                .is_none(),
            "{field} must invalidate the snapshot derivation"
        );
    }
    for (field, changed) in [
        ("title", json!("A different title")),
        ("status", json!("active")),
        ("description", json!("A different description")),
        ("publisher", json!("A different publisher")),
        ("contact", json!([{"name":"Someone else"}])),
    ] {
        let mut candidate = base.clone();
        candidate[field] = changed;
        let mut candidate =
            snapshot_gen::prepare_snapshot_derivation(candidate, Default::default())?;
        assert!(
            derivation
                .try_recompose(&mut candidate, &context)?
                .is_some(),
            "{field} is part of the current envelope, not the snapshot recipe"
        );
    }
    Ok(())
}

#[test]
fn preparing_a_reuse_candidate_never_bypasses_full_r4_conversion_failure() {
    let mut malformed = minimal_logical("Malformed R4 envelope");
    malformed["fhirVersion"] = json!("4.0.1");
    malformed["contained"] = json!([{"resourceType":"Patient","id":"contained"}]);
    let error = snapshot_gen::prepare_snapshot_derivation(malformed, Default::default())
        .err()
        .expect("unsupported contained resource must still fail conversion");
    assert!(error
        .to_string()
        .contains("contained[] resource conversion"));
}

fn oracle_context(backbone: &Value) -> anyhow::Result<snapshot_gen::PackageContext> {
    let label = "hl7.fhir.r4.core#4.0.1";
    let filename = "StructureDefinition-BackboneElement.json";
    let mut source = BundleSource::new();
    source.mount_package(
        label,
        [
            (
                ".index.json",
                serde_json::to_vec(&json!({
                    "index-version": 2,
                    "files": [{
                        "filename": filename,
                        "resourceType": "StructureDefinition",
                        "id": "BackboneElement",
                        "url": "http://hl7.org/fhir/StructureDefinition/BackboneElement",
                        "kind": "complex-type",
                        "type": "BackboneElement"
                    }]
                }))?,
            ),
            (filename, serde_json::to_vec(backbone)?),
        ],
    );
    let root = source.cache_root().to_path_buf();
    snapshot_gen::PackageContext::new_with(source, root, &[label.to_string()])
}

#[test]
fn r4_logical_model_uses_publishers_versioned_synthetic_base() -> anyhow::Result<()> {
    let context = empty_context()?;

    let base = context
        .fetch("http://hl7.org/fhir/StructureDefinition/Base")
        .expect("PackageContext must expose SUSHI's virtual Base definition");
    assert_eq!(
        base.get("id").and_then(|value| value.as_str()),
        Some("Base")
    );

    let logical = json!({
        "resourceType": "StructureDefinition",
        "id": "Document",
        "url": "https://example.org/StructureDefinition/Document",
        "version": "1.0.0",
        "name": "Document",
        "status": "draft",
        "fhirVersion": "4.0.1",
        "kind": "logical",
        "abstract": false,
        "type": "Document",
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Base|4.0.1",
        "derivation": "specialization",
        "differential": {
            "element": [{
                "id": "Document",
                "path": "Document",
                "short": "Document",
                "definition": "Abstract model of a document"
            }]
        }
    });
    let completed = snapshot_gen::generate_snapshot(logical, &context, Default::default())?;
    let snapshot = completed
        .pointer("/snapshot/element")
        .and_then(|value| value.as_array())
        .expect("logical model snapshot elements");
    assert_eq!(
        snapshot,
        &[json!({
            "id": "Document",
            "path": "Document",
            "short": "Document",
            "definition": "Abstract model of a document",
            "min": 0,
            "max": "*",
            "base": { "path": "Base", "min": 0, "max": "*" },
            "isModifier": false
        })]
    );
    assert_eq!(
        completed
            .pointer("/snapshot/extension/0/valueString")
            .and_then(Value::as_str),
        Some("4.0.1")
    );
    assert_eq!(
        completed
            .pointer("/snapshot/extension/0/url")
            .and_then(Value::as_str),
        Some("http://hl7.org/fhir/tools/StructureDefinition/snapshot-base-version")
    );
    Ok(())
}

#[test]
fn publisher_base_rejects_a_cross_version_reference() -> anyhow::Result<()> {
    let context = empty_context()?;
    let logical = json!({
        "resourceType": "StructureDefinition",
        "id": "Document",
        "url": "https://example.org/StructureDefinition/Document",
        "name": "Document",
        "status": "draft",
        "fhirVersion": "4.0.1",
        "kind": "logical",
        "abstract": false,
        "type": "Document",
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Base|5.0.0",
        "derivation": "specialization",
        "differential": { "element": [{ "id": "Document", "path": "Document" }] }
    });
    let error = snapshot_gen::generate_snapshot(logical, &context, Default::default())
        .expect_err("cross-version Base must not resolve");
    assert!(error
        .to_string()
        .contains("Publisher context FHIR version is 4.0.1"));
    Ok(())
}

#[test]
fn r5_logical_model_keeps_the_real_base_definition() -> anyhow::Result<()> {
    let context = empty_context()?;
    let logical = json!({
        "resourceType": "StructureDefinition",
        "id": "R5Model",
        "url": "https://example.org/StructureDefinition/R5Model",
        "version": "1.0.0",
        "name": "R5Model",
        "status": "draft",
        "fhirVersion": "5.0.0",
        "kind": "logical",
        "abstract": false,
        "type": "R5Model",
        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Base|5.0.0",
        "derivation": "specialization",
        "differential": { "element": [{ "id": "R5Model", "path": "R5Model" }] }
    });
    let completed = snapshot_gen::generate_snapshot(logical, &context, Default::default())?;
    let root = completed
        .pointer("/snapshot/element/0")
        .expect("R5 logical root");
    assert_eq!(
        root.pointer("/constraint/0/key").and_then(Value::as_str),
        Some("ele-1"),
        "R5 must inherit its real Base, not the minimal pre-R5 synthetic Base"
    );
    assert_eq!(
        completed
            .pointer("/snapshot/extension/0/valueString")
            .and_then(Value::as_str),
        Some("5.0.0")
    );
    Ok(())
}

#[test]
fn specialization_matches_the_publisher_oracle() -> anyhow::Result<()> {
    let repo = repo();
    let backbone: Value = serde_json::from_slice(&std::fs::read(
        repo.join("sushi-ts/test/testhelpers/testdefs/r4-definitions/package/StructureDefinition-BackboneElement.json"),
    )?)?;
    let context = oracle_context(&backbone)?;
    let input: Value = serde_json::from_slice(&std::fs::read(
        repo.join("tests/sushi-harvest/logical-026/expected/StructureDefinition-LogicalModel.json"),
    )?)?;
    let expected: Value = serde_json::from_slice(&std::fs::read(
        repo.join("snapshot/goldens/r4-logical-specialization.snapshot.json"),
    )?)?;

    let actual = snapshot_gen::generate_snapshot(input, &context, Default::default())?;
    assert_eq!(
        actual
            .pointer("/snapshot/element")
            .expect("actual snapshot"),
        expected
            .pointer("/snapshot/element")
            .expect("Publisher oracle snapshot")
    );
    Ok(())
}

#[test]
fn local_logical_specialization_chain_keeps_authored_children() -> anyhow::Result<()> {
    let mut context = empty_context()?;
    let logical = |id: &str, base: &str, elements: Vec<Value>| {
        json!({
            "resourceType": "StructureDefinition",
            "id": id,
            "url": format!("https://example.org/StructureDefinition/{id}"),
            "version": "1.0.0",
            "name": id,
            "status": "draft",
            "fhirVersion": "4.0.1",
            "kind": "logical",
            "abstract": false,
            "type": id,
            "baseDefinition": base,
            "derivation": "specialization",
            "differential": { "element": elements }
        })
    };
    let document = logical(
        "Document",
        "http://hl7.org/fhir/StructureDefinition/Base",
        vec![json!({ "id": "Document", "path": "Document" })],
    );
    let section = logical(
        "DocumentSection",
        "http://hl7.org/fhir/StructureDefinition/Base",
        vec![json!({ "id": "DocumentSection", "path": "DocumentSection" })],
    );
    let sections = logical(
        "IPSSectionsLM",
        "https://example.org/StructureDefinition/Document",
        vec![
            json!({ "id": "IPSSectionsLM", "path": "IPSSectionsLM" }),
            json!({
                "id": "IPSSectionsLM.sectionProblems",
                "path": "IPSSectionsLM.sectionProblems",
                "min": 1,
                "max": "1",
                "type": [{ "code": "https://example.org/StructureDefinition/DocumentSection" }]
            }),
            json!({
                "id": "IPSSectionsLM.sectionAllergies",
                "path": "IPSSectionsLM.sectionAllergies",
                "min": 1,
                "max": "1",
                "type": [{ "code": "https://example.org/StructureDefinition/DocumentSection" }]
            }),
            json!({
                "id": "IPSSectionsLM.sectionNotes",
                "path": "IPSSectionsLM.sectionNotes",
                "type": [{ "code": "string" }]
            }),
        ],
    );
    context.load_local_resources([
        (
            PathBuf::from("local/StructureDefinition-Document.json"),
            document,
        ),
        (
            PathBuf::from("local/StructureDefinition-DocumentSection.json"),
            section,
        ),
        (
            PathBuf::from("local/StructureDefinition-IPSSectionsLM.json"),
            sections.clone(),
        ),
    ]);

    let completed = snapshot_gen::generate_snapshot(sections, &context, Default::default())?;
    let elements = completed
        .pointer("/snapshot/element")
        .and_then(Value::as_array)
        .expect("IPS logical-model snapshot");
    assert_eq!(
        elements
            .iter()
            .filter_map(|element| element.get("path").and_then(Value::as_str))
            .collect::<Vec<_>>(),
        [
            "IPSSectionsLM",
            "IPSSectionsLM.sectionProblems",
            "IPSSectionsLM.sectionAllergies",
            "IPSSectionsLM.sectionNotes",
        ]
    );
    assert_eq!(
        elements[1].pointer("/base/path").and_then(Value::as_str),
        Some("IPSSectionsLM.sectionProblems")
    );
    assert_eq!(
        elements[3].pointer("/base/path").and_then(Value::as_str),
        Some("IPSSectionsLM.sectionNotes")
    );
    assert!(elements[3].pointer("/base/max").is_none());
    Ok(())
}
