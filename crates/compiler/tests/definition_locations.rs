use compiler::{DefinitionKind, DefinitionLocation};
use package_store::BundleSource;
use std::collections::HashMap;

fn core_source() -> BundleSource {
    let patient = serde_json::json!({
        "resourceType": "StructureDefinition",
        "id": "Patient",
        "url": "http://hl7.org/fhir/StructureDefinition/Patient",
        "version": "4.0.1",
        "name": "Patient",
        "status": "active",
        "kind": "resource",
        "abstract": false,
        "type": "Patient",
        "derivation": "specialization",
        "snapshot": {
            "element": [{ "id": "Patient", "path": "Patient" }]
        },
        "differential": {
            "element": [{ "id": "Patient", "path": "Patient" }]
        }
    });
    let index = serde_json::json!({
        "index-version": 2,
        "files": [{
            "filename": "StructureDefinition-Patient.json",
            "resourceType": "StructureDefinition",
            "id": "Patient",
            "url": "http://hl7.org/fhir/StructureDefinition/Patient",
            "version": "4.0.1",
            "kind": "resource",
            "type": "Patient"
        }]
    });
    let mut source = BundleSource::new();
    source.mount_package(
        "hl7.fhir.r4.core#4.0.1",
        [
            (
                "package.json",
                br#"{"name":"hl7.fhir.r4.core","version":"4.0.1"}"#.to_vec(),
            ),
            (
                ".index.json",
                serde_json::to_vec(&index).expect("serialize core index"),
            ),
            (
                "StructureDefinition-Patient.json",
                serde_json::to_vec(&patient).expect("serialize Patient SD"),
            ),
        ],
    );
    source
}

#[test]
fn compiled_resources_carry_their_exact_fsh_declarations() {
    let config = r#"id: definition-location-test
canonical: https://example.test/fhir
name: DefinitionLocationTest
status: draft
version: 0.1.0
fhirVersion: 4.0.1
"#;
    let path = "input/fsh/nested/exact-source.fsh";
    let source = r#"
CodeSystem: ExactCodeSystem
Id: exact-code-system

ValueSet: ExactValueSet
Id: exact-value-set

Profile: ExactProfile
Parent: Patient
Id: exact-profile

Instance: ExactPatient
InstanceOf: Patient
Id: exact-patient
Usage: #example
"#;

    let source_packages = core_source();
    let cache_root = source_packages.cache_root().to_string_lossy().into_owned();

    let (mut resources, ig, diagnostics) = compiler::build_project_in_memory_with_ig(
        config,
        &[(path.to_string(), source.to_string())],
        Vec::new(),
        source_packages,
        &cache_root,
        HashMap::new(),
    )
    .expect("compile exact declarations");

    let expected = [
        ("CodeSystem", Some("exact-code-system"), 2),
        ("ValueSet", Some("exact-value-set"), 5),
        ("StructureDefinition", Some("exact-profile"), 8),
        // The intentionally minimal synthetic Patient definition has no `id`
        // element, but the instance is still a written compiled resource.
        ("Patient", None, 12),
    ];
    for (resource_type, id, line) in expected {
        let resource = resources
            .iter()
            .find(|resource| {
                resource.body.get("resourceType").and_then(|v| v.as_str()) == Some(resource_type)
                    && id.map_or(true, |id| {
                        resource.body.get("id").and_then(|v| v.as_str()) == Some(id)
                    })
            })
            .unwrap_or_else(|| {
                panic!(
                    "missing {resource_type}/{id:?}; got {:?}; diagnostics: {:?}",
                    resources
                        .iter()
                        .map(|resource| (&resource.filename, &resource.body))
                        .collect::<Vec<_>>(),
                    diagnostics
                )
            });
        assert_eq!(
            resource.definition,
            Some(DefinitionLocation {
                kind: DefinitionKind::FshDeclaration,
                path: path.to_string(),
                line,
                column: 0,
            }),
            "{resource_type}/{id:?} must point to its entity declaration"
        );
        assert_eq!(
            resource.text,
            json_emit::to_fhir_json_string(&resource.body),
            "definition metadata must not alter output bytes"
        );
    }

    let ig = ig.expect("generated ImplementationGuide");
    assert_eq!(ig.definition, None, "generated IG has no FSH declaration");

    // Do not let the sort below obscure an accidental extra generated resource.
    resources.sort_by(|a, b| a.filename.cmp(&b.filename));
    assert_eq!(resources.len(), expected.len());
}

#[test]
fn input_examples_are_local_resources_with_example_publication_metadata() {
    let config = r#"id: browser-example-test
canonical: https://example.test/fhir
name: BrowserExampleTest
status: draft
version: 0.1.0
fhirVersion: 4.0.1
"#;
    let example = serde_json::json!({
        "resourceType": "Patient",
        "id": "browser-example",
        "active": true
    });
    let source_packages = core_source();
    let cache_root = source_packages.cache_root().to_string_lossy().into_owned();

    let (resources, ig, diagnostics) = compiler::build_project_in_memory_with_ig(
        config,
        &[],
        vec![(
            "input/examples/Patient-browser-example.json".into(),
            example,
        )],
        source_packages,
        &cache_root,
        HashMap::new(),
    )
    .expect("compile authored browser example");

    assert!(
        resources.is_empty(),
        "predefined examples are not compiler outputs"
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != "error"),
        "unexpected diagnostics: {diagnostics:?}"
    );
    let ig = ig.expect("generated ImplementationGuide");
    let entry = ig
        .body
        .pointer("/definition/resource")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| {
            entries.iter().find(|entry| {
                entry
                    .pointer("/reference/reference")
                    .and_then(serde_json::Value::as_str)
                    == Some("Patient/browser-example")
            })
        })
        .expect("example declaration in generated IG");
    assert_eq!(
        entry.get("exampleBoolean"),
        Some(&serde_json::Value::Bool(true))
    );
}
