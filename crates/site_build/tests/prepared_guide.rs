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
        authored_files: Vec::new(),
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
fn cycle_v2_projects_prepared_guide_directly() {
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

#[test]
fn cycle_projects_images_publicly_and_includes_privately() {
    let (mut prepared, mut input) = fixture();
    let entries = [
        (
            "sushi-config.yaml",
            SourceKind::Config,
            b"id: example.ig\n".as_slice(),
        ),
        (
            "input/images/logo.svg",
            SourceKind::Asset,
            b"<svg/>".as_slice(),
        ),
        (
            "input/includes/shared.md",
            SourceKind::Asset,
            b"shared".as_slice(),
        ),
        ("input/data/site.json", SourceKind::Asset, b"{}".as_slice()),
    ];
    input.project.sources =
        SourceManifest::from_entries(entries.into_iter().map(|(path, kind, bytes)| {
            (
                SourcePath::parse(path).unwrap(),
                SourceEntry {
                    kind,
                    content: ContentRef::of_bytes(bytes, None::<String>),
                },
            )
        }))
        .unwrap();
    prepared.authored_files = vec![
        AuthoredFile {
            role: AuthoredFileRole::Image,
            path: PreparedPath::parse("logo.svg").unwrap(),
            mime: "image/svg+xml".into(),
            content: b"<svg/>".to_vec(),
            source_reads: BTreeSet::from([PreparedPath::parse("input/images/logo.svg").unwrap()]),
        },
        AuthoredFile {
            role: AuthoredFileRole::Include,
            path: PreparedPath::parse("shared.md").unwrap(),
            mime: "text/markdown".into(),
            content: b"shared".to_vec(),
            source_reads: BTreeSet::from(
                [PreparedPath::parse("input/includes/shared.md").unwrap()],
            ),
        },
        AuthoredFile {
            role: AuthoredFileRole::Data,
            path: PreparedPath::parse("site.json").unwrap(),
            mime: "application/json".into(),
            content: b"{}".to_vec(),
            source_reads: BTreeSet::from([PreparedPath::parse("input/data/site.json").unwrap()]),
        },
    ];

    let projection = cycle::close_prepared(&prepared, input).unwrap();
    let required = projection
        .site_build
        .site_build()
        .render_plan()
        .required_artifacts();
    assert!(required.contains(&cycle::asset_key(SourcePath::parse("logo.svg").unwrap())));
    assert!(required.contains(&cycle::include_key(SourcePath::parse("shared.md").unwrap())));
    assert!(!required.iter().any(|key| {
        matches!(key, ArtifactKey::Asset { path, .. } if path.as_str() == "site.json")
    }));
}

#[test]
fn authored_page_body_requires_its_exact_project_source() {
    let (mut prepared, input) = fixture();
    prepared.pages.push(PageNode {
        name_url: "index.html".into(),
        title: "Home".into(),
        generation: "markdown".into(),
        body: Some("# Home".into()),
        source: Some(PreparedPath::parse("input/pagecontent/index.md").unwrap()),
        children: Vec::new(),
    });

    assert!(matches!(
        cycle::close_prepared(&prepared, input),
        Err(cycle::CycleProjectionError::Invalid(message))
            if message.contains("page index.html reads absent project source")
    ));
}
