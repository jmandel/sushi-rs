use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::json;
use site_build::{
    AuthoredFile, AuthoredFileRole, GeneratedIdentity, GuideIdentity, MenuNode, PreparedGuide,
    PreparedPath, SemanticResource, SemanticResourceKey,
};

fn semantic(resource_type: &str, id: &str, body: serde_json::Value) -> SemanticResource {
    SemanticResource {
        key: SemanticResourceKey {
            resource_type: resource_type.into(),
            id: id.into(),
        },
        resource: body,
        publication: None,
    }
}

fn authored(role: AuthoredFileRole, path: &str, source: &str) -> AuthoredFile {
    AuthoredFile {
        role,
        path: PreparedPath::parse(path).unwrap(),
        mime: "text/markdown".into(),
        content: Vec::new(),
        source_reads: BTreeSet::from([PreparedPath::parse(source).unwrap()]),
    }
}

fn fixture() -> PreparedGuide {
    let primary = SemanticResourceKey {
        resource_type: "ImplementationGuide".into(),
        id: "primary".into(),
    };
    PreparedGuide {
        guide: GuideIdentity {
            implementation_guide: primary.clone(),
            package_id: "example.ig".into(),
            canonical: Some("https://example.org/ig".into()),
            name: Some("ExampleIG".into()),
            version: Some("1.0.0".into()),
            fhir_version: "4.0.1".into(),
            release_label: None,
            fhir_publication_base: "http://hl7.org/fhir/R4/".into(),
            generated: GeneratedIdentity {
                epoch_seconds: 1,
                date: "1970-01-01T00:00:01Z".into(),
                day: "19700101".into(),
            },
            source_control: None,
        },
        resources: vec![
            semantic(
                "ValueSet",
                "values",
                json!({"resourceType":"ValueSet","id":"values","url":"https://example.org/ValueSet/values"}),
            ),
            semantic(
                "ImplementationGuide",
                "secondary",
                json!({"resourceType":"ImplementationGuide","id":"secondary"}),
            ),
            semantic(
                "ImplementationGuide",
                "primary",
                json!({
                    "resourceType":"ImplementationGuide",
                    "id":"primary",
                    "url":"https://example.org/ig/ImplementationGuide/primary",
                    "definition":{"resource":[
                        {"reference":{"reference":"Observation/example"},"exampleCanonical":"https://example.org/StructureDefinition/demo"},
                        {"reference":{"reference":"StructureDefinition/profile"}},
                        {"reference":{"reference":"ValueSet/values"}},
                        {"reference":{"reference":"ImplementationGuide/secondary"}}
                    ]}
                }),
            ),
            semantic(
                "StructureDefinition",
                "profile",
                json!({"resourceType":"StructureDefinition","id":"profile","type":"Observation"}),
            ),
            semantic(
                "Observation",
                "example",
                json!({"resourceType":"Observation","id":"example"}),
            ),
        ],
        publisher_compatibility: None,
        expansions: Vec::new(),
        pages: Vec::new(),
        menu: vec![MenuNode {
            label: "Home".into(),
            href: Some("index.html".into()),
            items: Vec::new(),
        }],
        sushi_config: json!({"id":"example.ig"}),
        authored_files: vec![
            authored(
                AuthoredFileRole::PageContent,
                "index.md",
                "input/pagecontent/index.md",
            ),
            authored(
                AuthoredFileRole::ResourceContent,
                "StructureDefinition-profile-intro.md",
                "input/intro-notes/StructureDefinition-profile-intro.md",
            ),
            authored(
                AuthoredFileRole::Include,
                "nested/shared.md",
                "input/includes/nested/shared.md",
            ),
            authored(AuthoredFileRole::Image, "logo.svg", "input/images/logo.svg"),
        ],
    }
}

#[test]
fn direct_projection_uses_explicit_primary_order_examples_and_authored_fragments() {
    let prepared = fixture();
    let inputs = site_producer::ProducerInputs::from_prepared(
        &prepared,
        &json!({"defaults":{}}),
        HashMap::new(),
        "en/",
    )
    .unwrap();

    assert_eq!(inputs.ig_json["id"], "primary");
    assert_eq!(
        inputs
            .resources
            .iter()
            .map(|resource| format!("{}/{}", resource.rt, resource.id))
            .collect::<Vec<_>>(),
        vec![
            "Observation/example",
            "StructureDefinition/profile",
            "ValueSet/values",
            "ImplementationGuide/secondary",
        ]
    );
    assert!(inputs.resources[0].is_example);
    assert!(inputs.resources[1..]
        .iter()
        .all(|resource| !resource.is_example));
    assert_eq!(
        inputs.page_includes,
        HashSet::from([
            "index.md".into(),
            "StructureDefinition-profile-intro.md".into(),
            "shared.md".into(),
        ])
    );
    assert_eq!(inputs.menu, prepared.menu);
    assert_eq!(inputs.page_prefix, "en/");
}

#[test]
fn authored_structural_pages_override_only_their_generated_defaults() {
    let mut prepared = fixture();
    prepared.authored_files.extend([
        authored(
            AuthoredFileRole::PageContent,
            "artifacts.md",
            "input/pages/artifacts.md",
        ),
        authored(
            AuthoredFileRole::Include,
            "en/toc.xml",
            "input/includes/en/toc.xml",
        ),
    ]);
    let inputs = site_producer::ProducerInputs::from_prepared(
        &prepared,
        &json!({"defaults":{}}),
        HashMap::new(),
        "en/",
    )
    .unwrap();

    assert!(inputs.authored_page_content.contains("artifacts.md"));
    assert!(inputs.authored_include_content.contains("en/toc.xml"));
    let output = site_producer::produce(&inputs).unwrap();
    assert!(output.files.contains_key("en/toc.html"));
    assert!(!output.files.contains_key("en/artifacts.html"));
    assert!(!output.files.contains_key("_includes/en/toc.xml"));
    assert!(output.files.contains_key("_includes/en/artifacts.xml"));
}

#[test]
fn missing_explicit_primary_fails_instead_of_selecting_an_ambient_guide() {
    let mut prepared = fixture();
    prepared
        .resources
        .retain(|resource| resource.key != prepared.guide.implementation_guide);
    let error = site_producer::ProducerInputs::from_prepared(
        &prepared,
        &json!({"defaults":{}}),
        HashMap::new(),
        "",
    )
    .err()
    .expect("missing primary must fail");
    assert!(error.to_string().contains("PreparedGuide primary"));
}

#[test]
fn prepared_menu_is_emitted_by_site_producer() {
    let mut prepared = fixture();
    prepared
        .resources
        .retain(|resource| resource.key == prepared.guide.implementation_guide);
    let inputs = site_producer::ProducerInputs::from_prepared(
        &prepared,
        &json!({"defaults":{}}),
        HashMap::new(),
        "",
    )
    .unwrap();
    let output = site_producer::produce(&inputs).unwrap();
    assert!(String::from_utf8_lossy(&output.files["_includes/menu.xml"])
        .contains("<a href=\"index.html\">Home</a>"));
}

#[test]
fn prepared_example_metadata_drives_its_exact_renderer_subject_and_page() {
    let prepared = fixture();
    let inputs = site_producer::ProducerInputs::from_prepared(
        &prepared,
        &json!({
            "defaults": {
                "example": {
                    "template-base": "template/layouts/example.html",
                    "base": "published-example-{{[id]}}.html"
                }
            }
        }),
        HashMap::from([(
            "template/layouts/example.html".into(),
            "example {{[type]}}/{{[id]}}".into(),
        )]),
        "en/",
    )
    .unwrap();
    let output = site_producer::produce(&inputs).unwrap();
    let page = output
        .resource_pages
        .get("en/published-example-example.html")
        .expect("example-specific output path");
    assert_eq!(page.resource_type, "Observation");
    assert_eq!(page.id, "example");
    assert_eq!(page.role, site_producer::ResourcePageRole::Primary);
}

#[test]
fn from_memory_orders_resources_and_reads_captured_template_relative_layouts() {
    let resources = vec![
        site_producer::Resource::from_value(
            json!({"resourceType":"ValueSet","id":"second"}),
            "ValueSet-second.json",
            false,
        )
        .unwrap(),
        site_producer::Resource::from_value(
            json!({"resourceType":"CodeSystem","id":"first"}),
            "CodeSystem-first.json",
            false,
        )
        .unwrap(),
    ];
    let ig = json!({
        "resourceType": "ImplementationGuide",
        "id": "memory",
        "url": "https://example.org/memory/ImplementationGuide/memory",
        "definition": {"resource": [
            {"reference": {"reference": "CodeSystem/first"}},
            {"reference": {"reference": "ValueSet/second"}}
        ]}
    });
    let inputs = site_producer::ProducerInputs::from_memory(
        resources,
        &json!({
            "defaults": {
                "Any": {
                    "template-base": "template/layouts/resource.html",
                    "base": "{{[type]}}-{{[id]}}.html"
                }
            }
        }),
        HashMap::from([("layouts/resource.html".into(), "{{[type]}}/{{[id]}}".into())]),
        &ig,
        HashSet::from(["CodeSystem-first-intro.md".into()]),
        "en/",
    )
    .unwrap();

    assert_eq!(
        inputs
            .resources
            .iter()
            .map(|resource| format!("{}/{}", resource.rt, resource.id))
            .collect::<Vec<_>>(),
        vec!["CodeSystem/first", "ValueSet/second"]
    );
    assert_eq!(inputs.ig.id.as_deref(), Some("memory"));
    assert_eq!(
        inputs.ig.canonical.as_deref(),
        Some("https://example.org/memory")
    );
    assert_eq!(
        inputs.page_includes,
        HashSet::from(["CodeSystem-first-intro.md".into()])
    );

    let output = site_producer::produce(&inputs).unwrap();
    assert_eq!(
        output.files["en/CodeSystem-first.html"],
        b"CodeSystem/first"
    );
    assert_eq!(output.files["en/ValueSet-second.html"], b"ValueSet/second");
}

#[test]
fn duplicate_explicit_primary_fails_instead_of_projecting_ambiguously() {
    let mut prepared = fixture();
    let primary = prepared
        .resources
        .iter()
        .find(|resource| resource.key == prepared.guide.implementation_guide)
        .unwrap()
        .clone();
    prepared.resources.push(primary);

    let error = site_producer::ProducerInputs::from_prepared(
        &prepared,
        &json!({"defaults":{}}),
        HashMap::new(),
        "",
    )
    .err()
    .expect("duplicate primary must fail");
    assert!(error
        .to_string()
        .contains("duplicate primary ImplementationGuides"));
}
