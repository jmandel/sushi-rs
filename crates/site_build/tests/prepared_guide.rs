use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use site_build::{cycle_semantic as cycle, *};

fn fixture() -> (PreparedGuide, cycle::CycleProjectionInput) {
    let config_path = SourcePath::parse("sushi-config.yaml").unwrap();
    let project = ProjectRevision {
        project_id: "example.ig".into(),
        revision: "source-1".into(),
        sources: SourceManifest::from_entries([(
            config_path,
            SourceEntry {
                kind: SourceKind::Config,
                content: ContentRef::of_bytes(b"id: example.ig\n", Some("text/yaml")),
            },
        )])
        .unwrap(),
    };
    let key = SemanticResourceKey {
        resource_type: "ImplementationGuide".into(),
        id: "example".into(),
    };
    let prepared = PreparedGuide {
        guide: GuideIdentity {
            implementation_guide: key.clone(),
            package_id: "example.ig".into(),
            canonical: Some("https://example.org/ig".into()),
            name: Some("ExampleIG".into()),
            version: Some("1.0.0".into()),
            fhir_version: "4.0.1".into(),
            release_label: None,
            fhir_publication_base: "http://hl7.org/fhir/R4/".into(),
            generated: GeneratedIdentity {
                epoch_seconds: 1_700_000_000,
                date: "2023-11-14T22:13:20Z".into(),
                day: "20231114".into(),
            },
            source_control: None,
        },
        resources: vec![SemanticResource {
            key,
            resource: json!({"resourceType":"ImplementationGuide","id":"example"}),
            publication: None,
        }],
        publisher_compatibility: None,
        expansions: Vec::new(),
        pages: Vec::new(),
        menu: Vec::new(),
        sushi_config: json!({"id":"example.ig"}),
        assets: Vec::new(),
    };
    let input = cycle::CycleProjectionInput {
        project,
        package_lock: PackageLock::default(),
        render_target: RenderTarget {
            renderer: ProducerRef::new("cycle-site", "2"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::from([
                ("contract".into(), cycle::TARGET.into()),
                ("buildEpochSecs".into(), "1700000000".into()),
            ]),
        },
        diagnostics: BTreeSet::new(),
    };
    (prepared, input)
}

#[test]
fn cycle_v2_projects_prepared_guide_without_site_db() {
    let (prepared, input) = fixture();
    let projection = cycle::close_prepared(&prepared, input).unwrap();
    let build = projection.site_build.site_build();
    assert!(build
        .render_plan()
        .required_artifacts()
        .contains(&cycle::resources_key()));
    assert_eq!(build.render_target().parameters["contract"], cycle::TARGET);
}

#[test]
fn direct_projection_fails_closed_on_semantic_identity_mismatch() {
    let (mut prepared, input) = fixture();
    prepared.resources[0].resource["id"] = json!("different");
    assert!(matches!(
        cycle::close_prepared(&prepared, input),
        Err(cycle::CycleProjectionError::Invalid(message))
            if message.contains("JSON identity")
    ));
}
