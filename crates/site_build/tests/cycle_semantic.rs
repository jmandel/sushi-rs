#![cfg(feature = "site-db-projections")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::Value;
use site_build::{cycle_semantic as cycle, *};
use site_db::model::{
    AssetRow, MenuRow, MetadataRow, PageRow, ResourceRow, SiteConfigRow, ValueSetCodeRow,
};

fn source(path: &str, kind: SourceKind, body: &[u8]) -> (SourcePath, SourceEntry) {
    (
        SourcePath::parse(path).unwrap(),
        SourceEntry {
            kind,
            content: ContentRef::of_bytes(body, Some("application/octet-stream")),
        },
    )
}

fn resource_row(key: i64, type_: &str, id: &str, json: &str) -> ResourceRow {
    ResourceRow {
        key,
        type_: type_.into(),
        custom: 0,
        id: id.into(),
        web: if type_ == "ImplementationGuide" {
            "index.html".into()
        } else {
            format!("{type_}-{id}.html")
        },
        url: None,
        version: None,
        status: None,
        date: None,
        name: Some(id.into()),
        title: None,
        experimental: None,
        realm: None,
        description: None,
        purpose: None,
        copyright: None,
        copyright_label: None,
        derivation: None,
        standard_status: None,
        kind: None,
        sd_type: None,
        base: None,
        content: None,
        supplements: None,
        json: json.into(),
    }
}

fn metadata() -> Vec<MetadataRow> {
    [
        ("path", "http://hl7.org/fhir/R4/"),
        ("canonical", "https://example.org/ig"),
        ("igId", "example.ig"),
        ("igName", "ExampleIG"),
        ("packageId", "example.ig"),
        ("igVer", "1.0.0"),
        ("errorCount", "0"),
        ("version", "4.0.1"),
        ("releaseLabel", "ci-build"),
        ("revision", "abc123"),
        ("versionFull", "4.0.1-abc123"),
        ("toolingVersion", "site-gen.publisher"),
        ("toolingRevision", "0"),
        ("toolingVersionFull", "site-gen.publisher experiment"),
        ("genDate", "2023-11-14T22:13:20Z"),
        ("genDay", "20231114"),
        ("gitstatus", "main"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (name, value))| MetadataRow {
        key: index as i64 + 1,
        name: name.into(),
        value: value.into(),
    })
    .collect()
}

fn database() -> site_db::SiteDb {
    let mut profile = resource_row(
        2,
        "StructureDefinition",
        "profile",
        r#"{"resourceType":"StructureDefinition","id":"profile","z":1,"a":2,"url":"https://example.org/StructureDefinition/profile","name":"Profile","description":"Profile description","type":"Observation","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Observation","derivation":"constraint"}"#,
    );
    profile.name = Some("Published Profile".into());
    profile.description = Some("Effective description".into());
    profile.standard_status = Some("trial-use".into());
    profile.base = Some("http://hl7.org/fhir/StructureDefinition/Observation|4.0.1".into());

    site_db::SiteDb {
        primary_implementation_guide: Some(site_db::model::ResourceIdentity {
            resource_type: "ImplementationGuide".into(),
            id: "example-source".into(),
        }),
        metadata: metadata(),
        resources: vec![
            resource_row(
                1,
                "ImplementationGuide",
                "example.ig",
                r#"{"resourceType":"ImplementationGuide","id":"example-source","url":"https://example.org/ig/ImplementationGuide/example-source"}"#,
            ),
            profile,
            resource_row(
                3,
                "CodeSystem",
                "codes",
                r#"{"resourceType":"CodeSystem","id":"codes","url":"https://example.org/CodeSystem/codes","concept":[{"code":"root","concept":[{"code":"child"}]}]}"#,
            ),
            resource_row(
                4,
                "ValueSet",
                "values",
                r#"{"resourceType":"ValueSet","id":"values","url":"https://example.org/ValueSet/values"}"#,
            ),
        ],
        value_set_codes: vec![ValueSetCodeRow {
            key: 1,
            resource_key: 4,
            value_set_uri: "https://example.org/ValueSet/values".into(),
            value_set_version: "1.0.0".into(),
            system: "https://example.org/CodeSystem/codes".into(),
            version: None,
            code: "root".into(),
            display: Some("Root".into()),
        }],
        pages: vec![
            PageRow {
                slug: "index".into(),
                name_url: "index.html".into(),
                title: "Home".into(),
                generation: "markdown".into(),
                ord: 0,
                depth: 0,
                body: Some("# Home".into()),
            },
            PageRow {
                slug: "child".into(),
                name_url: "child.html".into(),
                title: "Child".into(),
                generation: "markdown".into(),
                ord: 1,
                depth: 1,
                body: Some("# Child".into()),
            },
            PageRow {
                slug: "other".into(),
                name_url: "other.html".into(),
                title: "Other".into(),
                generation: "markdown".into(),
                ord: 2,
                depth: 0,
                body: Some("# Other".into()),
            },
        ],
        menu: vec![
            MenuRow {
                id: 1,
                parent_id: None,
                ord: 0,
                depth: 0,
                path: "Guide".into(),
                label: "Guide".into(),
                href: None,
                kind: "group".into(),
            },
            MenuRow {
                id: 2,
                parent_id: Some(1),
                ord: 1,
                depth: 1,
                path: "Guide/Home".into(),
                label: "Home".into(),
                href: Some("index.html".into()),
                kind: "link".into(),
            },
            MenuRow {
                id: 3,
                parent_id: None,
                ord: 2,
                depth: 0,
                path: "Artifacts".into(),
                label: "Artifacts".into(),
                href: Some("artifacts.html".into()),
                kind: "link".into(),
            },
        ],
        site_config: vec![SiteConfigRow {
            name: "sushi-config".into(),
            json: r#"{"id":"example.ig","z":1,"a":2}"#.into(),
        }],
        assets: vec![AssetRow {
            name: "figures/example.svg".into(),
            mime: "image/svg+xml".into(),
            content: b"<svg/>".to_vec(),
        }],
        ..Default::default()
    }
}

fn projection_input() -> cycle::CycleProjectionInput {
    cycle::CycleProjectionInput {
        project: ProjectRevision {
            project_id: "example.ig".into(),
            revision: "sources".into(),
            sources: SourceManifest::from_entries([
                source("sushi-config.yaml", SourceKind::Config, b"id: example.ig"),
                source(
                    "input/fsh/profile.fsh",
                    SourceKind::Fsh,
                    b"Profile: Profile",
                ),
                source("input/pagecontent/index.md", SourceKind::Page, b"# Home"),
                source(
                    "input/images/figures/example.svg",
                    SourceKind::Asset,
                    b"<svg/>",
                ),
            ])
            .unwrap(),
        },
        package_lock: PackageLock::default(),
        render_target: RenderTarget {
            renderer: ProducerRef::new("cycle-site", "2"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::from([
                ("buildEpochSecs".into(), "1700000000".into()),
                ("contract".into(), cycle::TARGET.into()),
            ]),
        },
        diagnostics: BTreeSet::new(),
    }
}

fn artifact_bytes<'a>(projection: &'a cycle::ClosedCycleProjection, key: &ArtifactKey) -> &'a [u8] {
    let record = projection
        .site_build
        .site_build()
        .artifacts()
        .get(key)
        .unwrap();
    let ArtifactState::Ready { content } = &record.state else {
        panic!("artifact is not ready")
    };
    projection.objects.get(&content.sha256).unwrap()
}

#[test]
fn v2_projection_is_closed_typed_and_asset_complete() {
    let projection = cycle::close_projection(&database(), projection_input()).unwrap();
    let build = projection.site_build.site_build();
    assert_eq!(build.render_target().parameters["contract"], cycle::TARGET);
    assert_eq!(build.render_plan().required_artifacts().len(), 5);
    for key in [
        cycle::resources_key(),
        cycle::terminology_key(),
        cycle::navigation_key(),
        cycle::config_key(),
        cycle::asset_key(SourcePath::parse("figures/example.svg").unwrap()),
    ] {
        assert!(build.render_plan().required_artifacts().contains(&key));
    }

    let asset = artifact_bytes(
        &projection,
        &cycle::asset_key(SourcePath::parse("figures/example.svg").unwrap()),
    );
    assert_eq!(asset, b"<svg/>");
    assert!(!String::from_utf8_lossy(asset).contains("PHN2Zy8+"));

    let resources =
        String::from_utf8(artifact_bytes(&projection, &cycle::resources_key()).to_vec()).unwrap();
    assert!(resources.contains(r#""schema":"cycle.semantic.resources/v1""#));
    assert!(resources.contains(r#""z":1,"a":2"#));
    for forbidden in ["\"Key\"", "\"Json\"", "\"ResourceKey\"", "\"Content\""] {
        assert!(
            !resources.contains(forbidden),
            "found legacy field {forbidden}"
        );
    }
    let value: Value = serde_json::from_str(&resources).unwrap();
    assert_eq!(
        value.pointer("/guide/implementationGuide/id"),
        Some(&Value::String("example-source".into()))
    );
    assert_eq!(
        value.pointer("/guide/packageId"),
        Some(&Value::String("example.ig".into()))
    );

    let terminology: Value =
        serde_json::from_slice(artifact_bytes(&projection, &cycle::terminology_key())).unwrap();
    assert_eq!(
        terminology.pointer("/expansions/0/valueSet/id"),
        Some(&Value::String("values".into()))
    );
    let navigation: Value =
        serde_json::from_slice(artifact_bytes(&projection, &cycle::navigation_key())).unwrap();
    assert_eq!(
        navigation.pointer("/pages/0/children/0/nameUrl"),
        Some(&Value::String("child.html".into()))
    );
    assert_eq!(
        navigation.pointer("/menu/0/items/0/href"),
        Some(&Value::String("index.html".into()))
    );
    let config =
        String::from_utf8(artifact_bytes(&projection, &cycle::config_key()).to_vec()).unwrap();
    assert!(config.contains(r#""sushiConfig":{"id":"example.ig","z":1,"a":2}"#));
}

#[test]
fn site_db_adapter_and_direct_prepared_projection_are_identical() {
    let input = projection_input();
    let prepared = cycle::prepare_from_site_db(&database(), &input).unwrap();
    let direct = cycle::close_prepared(&prepared, input.clone()).unwrap();
    let adapted = cycle::close_projection(&database(), input).unwrap();
    assert_eq!(direct.site_build, adapted.site_build);
    assert_eq!(direct.objects, adapted.objects);
}

#[test]
fn shared_preparation_and_site_db_adapter_produce_identical_cycle_objects() {
    let primary = serde_json::json!({
        "resourceType":"ImplementationGuide",
        "id":"example-source",
        "url":"https://example.org/ig/ImplementationGuide/example-source",
        "packageId":"example.ig",
        "name":"ExampleIG",
        "version":"1.0.0",
        "status":"draft",
        "fhirVersion":["4.0.1"],
        "definition":{"resource":[]}
    });
    let outcome = site_db::build_from_inputs(&site_db::InMemoryInputs {
        generated: std::slice::from_ref(&primary),
        primary_implementation_guide: &primary,
        examples: &[],
        sushi_config_yaml: concat!(
            "id: example-source\n",
            "packageId: example.ig\n",
            "canonical: https://example.org/ig\n",
            "name: ExampleIG\n",
            "version: 1.0.0\n",
            "fhirVersion: 4.0.1\n",
        ),
        build_epoch_secs: 1_700_000_000,
        branch: Some("main".into()),
        revision: Some("abc123".into()),
        vfs: BTreeMap::new(),
        ig_root: PathBuf::from("/ig"),
        liquid_asset_rel_dirs: Vec::new(),
    })
    .unwrap();
    let input = projection_input();
    let direct = cycle::close_prepared(&outcome.prepared_guide, input.clone()).unwrap();
    let adapted = cycle::close_projection(&outcome.db, input).unwrap();
    assert_eq!(direct.site_build, adapted.site_build);
    assert_eq!(direct.objects, adapted.objects);
}

#[test]
fn v2_projection_is_deterministic_and_partitions_read_dependencies() {
    let first = cycle::close_projection(&database(), projection_input()).unwrap();
    let second = cycle::close_projection(&database(), projection_input()).unwrap();
    assert_eq!(first.site_build, second.site_build);
    assert_eq!(first.objects, second.objects);

    let build = first.site_build.site_build();
    let terminology = build.artifacts().get(&cycle::terminology_key()).unwrap();
    assert!(terminology.reads.contains(&ReadDependency::Artifact {
        key: cycle::resources_key()
    }));
    let config = build.artifacts().get(&cycle::config_key()).unwrap();
    assert_eq!(
        config.reads,
        BTreeSet::from([ReadDependency::Source {
            path: SourcePath::parse("sushi-config.yaml").unwrap()
        }])
    );
    let navigation = build.artifacts().get(&cycle::navigation_key()).unwrap();
    assert!(navigation.reads.contains(&ReadDependency::Source {
        path: SourcePath::parse("input/pagecontent/index.md").unwrap()
    }));
    assert!(!navigation.reads.contains(&ReadDependency::Source {
        path: SourcePath::parse("input/images/figures/example.svg").unwrap()
    }));

    let mut forward_db = database();
    let mut tied_code = forward_db.value_set_codes[0].clone();
    tied_code.key = 2;
    tied_code.version = Some("2".into());
    tied_code.display = Some("Alternate".into());
    forward_db.value_set_codes.push(tied_code);
    let mut reverse_db = forward_db.clone();
    reverse_db.value_set_codes.reverse();
    let forward = cycle::close_projection(&forward_db, projection_input()).unwrap();
    let reverse = cycle::close_projection(&reverse_db, projection_input()).unwrap();
    assert_eq!(
        artifact_bytes(&forward, &cycle::terminology_key()),
        artifact_bytes(&reverse, &cycle::terminology_key())
    );
}

#[test]
fn v2_projection_rejects_target_confusion_and_inconsistent_rows() {
    let mut wrong = projection_input();
    wrong
        .render_target
        .parameters
        .insert("contract".into(), "cycle-site/v1".into());
    assert!(matches!(
        cycle::close_projection(&database(), wrong),
        Err(cycle::CycleProjectionError::WrongTarget)
    ));

    let mut db = database();
    db.resources[1].type_ = "Patient".into();
    assert!(matches!(
        cycle::close_projection(&db, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("disagrees")
    ));

    let mut db = database();
    db.pages[1].depth = 3;
    assert!(matches!(
        cycle::close_projection(&db, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("jumps")
    ));

    let mut db = database();
    db.value_set_codes[0].value_set_uri = "https://wrong.example/ValueSet/values".into();
    assert!(matches!(
        cycle::close_projection(&db, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("disagrees")
    ));

    let mut db = database();
    db.menu[0].href = Some("guide.html".into());
    db.menu[0].kind = "link".into();
    assert!(matches!(
        cycle::close_projection(&db, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("child items")
    ));

    let mut db = database();
    db.pages[1].name_url = db.pages[0].name_url.clone();
    assert!(matches!(
        cycle::close_projection(&db, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("duplicate page nameUrl")
    ));

    let mut db = database();
    db.menu[1].ord = db.menu[0].ord;
    assert!(matches!(
        cycle::close_projection(&db, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("duplicate menu ordinal")
    ));

    let mut db = database();
    db.metadata.push(db.metadata[0].clone());
    assert!(matches!(
        cycle::close_projection(&db, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("duplicate metadata name")
    ));

    let mut input = projection_input();
    input
        .render_target
        .parameters
        .insert("buildEpochSecs".into(), "9007199254740992".into());
    assert!(matches!(
        cycle::close_projection(&database(), input),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("safe-integer")
    ));
}

#[test]
fn v2_allows_implementation_guide_examples_and_omits_empty_optional_facets() {
    let mut db = database();
    db.resources[1].name = Some(String::new());
    db.resources.push(resource_row(
        5,
        "ImplementationGuide",
        "aaa-example-guide",
        r#"{"resourceType":"ImplementationGuide","id":"aaa-example-guide","status":"draft"}"#,
    ));
    db.resources.rotate_right(1);
    let projection = cycle::close_projection(&db, projection_input()).unwrap();
    let body =
        String::from_utf8(artifact_bytes(&projection, &cycle::resources_key()).to_vec()).unwrap();
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        value["resources"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["key"]["resourceType"] == "ImplementationGuide")
            .count(),
        2
    );
    assert_eq!(
        value["guide"]["implementationGuide"]["id"],
        "example-source"
    );
    assert!(!body.contains(r#""displayName":"""#));

    db.primary_implementation_guide = Some(site_db::model::ResourceIdentity {
        resource_type: "ImplementationGuide".into(),
        id: "missing".into(),
    });
    assert!(matches!(
        cycle::close_projection(&db, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("primary guide")
    ));
}

#[test]
fn v2_navigation_depth_matches_the_consumer_boundary() {
    let mut accepted = database();
    accepted.pages = (0..=256)
        .map(|depth| PageRow {
            slug: format!("p{depth}"),
            name_url: format!("p{depth}.html"),
            title: format!("Page {depth}"),
            generation: "markdown".into(),
            ord: depth,
            depth,
            body: Some(format!("# Page {depth}")),
        })
        .collect();
    cycle::close_projection(&accepted, projection_input()).unwrap();

    let mut rejected = accepted;
    rejected.pages.push(PageRow {
        slug: "p257".into(),
        name_url: "p257.html".into(),
        title: "Page 257".into(),
        generation: "markdown".into(),
        ord: 257,
        depth: 257,
        body: Some("# Page 257".into()),
    });
    assert!(matches!(
        cycle::close_projection(&rejected, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("maximum depth")
    ));
}

#[test]
fn v2_navigation_normalizes_a_skipped_structural_root() {
    let original = cycle::close_projection(&database(), projection_input()).unwrap();
    let original_navigation = artifact_bytes(&original, &cycle::navigation_key());

    for offset in [1, 7] {
        let mut db = database();
        for page in &mut db.pages {
            page.depth += offset;
        }

        let projection = cycle::close_projection(&db, projection_input()).unwrap();
        let navigation = artifact_bytes(&projection, &cycle::navigation_key());
        assert_eq!(navigation, original_navigation);
    }

    let mut db = database();
    for page in &mut db.pages {
        page.depth += 1;
    }
    let projection = cycle::close_projection(&db, projection_input()).unwrap();
    let navigation: Value =
        serde_json::from_slice(artifact_bytes(&projection, &cycle::navigation_key())).unwrap();
    assert_eq!(
        navigation.pointer("/pages/0/nameUrl"),
        Some(&Value::String("index.html".into()))
    );
    assert_eq!(
        navigation.pointer("/pages/0/children/0/nameUrl"),
        Some(&Value::String("child.html".into()))
    );
    assert_eq!(
        navigation.pointer("/pages/1/nameUrl"),
        Some(&Value::String("other.html".into()))
    );

    db.pages[2].depth = 0;
    assert!(matches!(
        cycle::close_projection(&db, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("invalid depth transition")
    ));

    let mut negative = database();
    negative.pages[0].depth = -1;
    assert!(matches!(
        cycle::close_projection(&negative, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("negative depth")
    ));

    let mut overflow = database();
    overflow.pages.truncate(1);
    overflow.pages[0].depth = i64::MAX;
    assert!(matches!(
        cycle::close_projection(&overflow, projection_input()),
        Err(cycle::CycleProjectionError::Invalid(message)) if message.contains("overflows")
    ));
}
