use std::collections::{BTreeMap, BTreeSet};

use site_build::*;

fn path(value: &str) -> SourcePath {
    SourcePath::parse(value).unwrap()
}

fn package(value: &str) -> PackageCoordinate {
    PackageCoordinate::parse(value).unwrap()
}

fn source(value: &[u8], kind: SourceKind) -> SourceEntry {
    SourceEntry {
        kind,
        content: ContentRef::of_bytes(value, Some("text/plain")),
    }
}

fn provenance() -> ArtifactProvenance {
    ArtifactProvenance {
        producer: ProducerRef::new("test.renderer", "1.0.0"),
        recipe: "fixture".into(),
        attributes: BTreeMap::new(),
    }
}

fn fixture(reverse: bool) -> SiteBuild {
    let mut sources = vec![
        (
            path("sushi-config.yaml"),
            source(b"id: demo", SourceKind::Config),
        ),
        (
            path("input/fsh/demo.fsh"),
            source(b"Profile: Demo", SourceKind::Fsh),
        ),
    ];
    if reverse {
        sources.reverse();
    }
    let sources = SourceManifest::from_entries(sources).unwrap();

    let core = LockedPackage {
        coordinate: package("hl7.fhir.r4.core#4.0.1"),
        content: ContentRef::of_bytes(b"core", Some(PREPARED_PACKAGE_MEDIA_TYPE)),
        dependencies: BTreeSet::new(),
    };
    let template = LockedPackage {
        coordinate: package("hl7.fhir.template#1.0.0"),
        content: ContentRef::of_bytes(b"template", Some(PREPARED_PACKAGE_MEDIA_TYPE)),
        dependencies: BTreeSet::from([core.coordinate.clone()]),
    };
    let package_lock = PackageLock::from_packages(if reverse {
        vec![template.clone(), core.clone()]
    } else {
        vec![core.clone(), template.clone()]
    })
    .unwrap();

    let resource_key = ArtifactKey::Resource {
        resource: ResourceKey {
            resource_type: "StructureDefinition".into(),
            id: "demo".into(),
        },
    };
    let fragment_key = ArtifactKey::Fragment {
        scope: FragmentScope::Resource {
            resource: ResourceKey {
                resource_type: "StructureDefinition".into(),
                id: "demo".into(),
            },
        },
        fragment: FragmentKind::Summary,
        parameters: BTreeMap::new(),
    };
    let resource = ArtifactRecord {
        key: resource_key.clone(),
        state: ArtifactState::Ready {
            content: ContentRef::of_bytes(b"{}", Some("application/fhir+json")),
        },
        provenance: provenance(),
        reads: BTreeSet::from([
            ReadDependency::Package {
                coordinate: core.coordinate.clone(),
            },
            ReadDependency::Source {
                path: path("input/fsh/demo.fsh"),
            },
        ]),
    };
    let fragment = ArtifactRecord {
        key: fragment_key,
        state: ArtifactState::Deferred {
            reason: "materialize on renderer demand".into(),
        },
        provenance: provenance(),
        reads: BTreeSet::from([ReadDependency::Artifact {
            key: resource_key.clone(),
        }]),
    };
    let artifacts = ArtifactCatalog::from_records(if reverse {
        vec![fragment, resource]
    } else {
        vec![resource, fragment]
    })
    .unwrap();

    let mut diagnostics = vec![
        BuildDiagnostic::new(DiagnosticSeverity::Warning, "W2", "second"),
        BuildDiagnostic::new(DiagnosticSeverity::Information, "I1", "first"),
    ];
    if reverse {
        diagnostics.reverse();
    }

    SiteBuild::new(
        ProjectRevision {
            project_id: "demo.ig".into(),
            revision: "0123456789abcdef".into(),
            sources,
        },
        package_lock,
        RenderTarget {
            renderer: ProducerRef::new("native-template", "0.1.0"),
            mode: RenderMode::NativeTemplate,
            fhir_version: "4.0.1".into(),
            template: Some(template.coordinate),
            parameters: BTreeMap::from([
                ("locale".into(), "en".into()),
                ("strict".into(), "true".into()),
            ]),
        },
        RenderPlan::new([resource_key]),
        artifacts,
        diagnostics.into_iter().collect(),
    )
    .unwrap()
}

fn singleton_fragment_build(state: ArtifactState) -> SiteBuild {
    let key = ArtifactKey::Fragment {
        scope: FragmentScope::WholeIg,
        fragment: FragmentKind::Summary,
        parameters: BTreeMap::new(),
    };
    let record = ArtifactRecord {
        key: key.clone(),
        state,
        provenance: provenance(),
        reads: BTreeSet::new(),
    };
    SiteBuild::new(
        ProjectRevision {
            project_id: "demo.ig".into(),
            revision: "rev".into(),
            sources: SourceManifest::default(),
        },
        PackageLock::default(),
        RenderTarget {
            renderer: ProducerRef::new("external", "1.0.0"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::new(),
        },
        RenderPlan::new([key]),
        ArtifactCatalog::from_records([record]).unwrap(),
        BTreeSet::new(),
    )
    .unwrap()
}

#[test]
fn build_id_and_canonical_bytes_are_order_independent() {
    let forward = fixture(false);
    let reverse = fixture(true);
    assert_eq!(forward.build_id(), reverse.build_id());
    assert_eq!(forward.schema_version(), SchemaVersion::V2);
    // Golden v2 hash: changing the wire contract or canonicalization is an
    // explicit schema decision, not an unnoticed serde refactor.
    assert_eq!(
        forward.build_id().as_str(),
        "sb1-sha256:0490a3e4add53e3246b0865ddf07cf757fb8181b6d82beee088781fceefb1cd5"
    );
    assert_eq!(
        forward.canonical_bytes().unwrap(),
        reverse.canonical_bytes().unwrap()
    );
}

#[test]
fn non_bmp_object_keys_have_a_cross_host_v2_hash() {
    let key = ArtifactKey::Data {
        namespace: "test".into(),
        name: "root".into(),
    };
    let build = SiteBuild::new(
        ProjectRevision {
            project_id: "unicode.ig".into(),
            revision: "unicode-order".into(),
            sources: SourceManifest::from_entries([
                (path("\u{e000}"), source(b"a", SourceKind::Asset)),
                (path("\u{10000}"), source(b"b", SourceKind::Asset)),
            ])
            .unwrap(),
        },
        PackageLock::default(),
        RenderTarget {
            renderer: ProducerRef::new("external", "1"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::new(),
        },
        RenderPlan::new([key.clone()]),
        ArtifactCatalog::from_records([ArtifactRecord {
            key,
            state: ArtifactState::Ready {
                content: ContentRef::of_bytes(b"root", Some("text/plain")),
            },
            provenance: provenance(),
            reads: BTreeSet::new(),
        }])
        .unwrap(),
        BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(
        build.build_id().as_str(),
        "sb1-sha256:4b560ddd18498b28623af0a4608727cd6831ab8fdb22c549bbd52073877f9333"
    );
}

#[test]
fn build_id_changes_when_identity_bearing_content_changes() {
    let original = fixture(false);
    let changed = SiteBuild::new(
        ProjectRevision {
            project_id: "demo.ig".into(),
            revision: "new-revision".into(),
            sources: original.project().sources.clone(),
        },
        original.package_lock().clone(),
        original.render_target().clone(),
        original.render_plan().clone(),
        original.artifacts().clone(),
        original.diagnostics().clone(),
    )
    .unwrap();
    assert_ne!(original.build_id(), changed.build_id());
}

#[test]
fn serialization_roundtrip_checks_integrity() {
    let build = fixture(false);
    let bytes = build.canonical_bytes().unwrap();
    let decoded: SiteBuild = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded, build);
    decoded.verify().unwrap();

    let mut tampered: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    tampered["project"]["revision"] = serde_json::Value::String("different".into());
    let error = serde_json::from_value::<SiteBuild>(tampered).unwrap_err();
    assert!(error.to_string().contains("build id mismatch"));
}

#[test]
fn v1_wire_values_are_recognized_but_rejected_as_unsupported() {
    let mut wire = serde_json::to_value(fixture(false)).unwrap();
    wire["schemaVersion"] = serde_json::Value::String("site-build/v1".into());

    let error = serde_json::from_value::<SiteBuild>(wire).unwrap_err();
    assert!(error
        .to_string()
        .contains("unsupported site build schema V1"));
}

#[test]
fn artifact_states_are_explicit_and_exhaustive_on_the_wire() {
    let diagnostic = BuildDiagnostic::new(DiagnosticSeverity::Error, "E1", "failed");
    let states = [
        ArtifactState::Ready {
            content: ContentRef::of_bytes(b"ready", Some("text/plain")),
        },
        ArtifactState::Deferred {
            reason: "later".into(),
        },
        ArtifactState::Unsupported {
            capability: "tx.expand".into(),
            reason: "no terminology service".into(),
        },
        ArtifactState::Failed {
            diagnostics: BTreeSet::from([diagnostic]),
        },
    ];
    let expected = ["ready", "deferred", "unsupported", "failed"];
    for (state, expected) in states.into_iter().zip(expected) {
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["status"], expected);
        assert_eq!(
            serde_json::from_value::<ArtifactState>(json).unwrap(),
            state
        );
    }
    assert!(serde_json::from_value::<ArtifactState>(serde_json::json!({
        "status": "unknown"
    }))
    .is_err());
}

#[test]
fn whole_ig_fragments_and_asset_namespaces_have_distinct_typed_identity() {
    let whole_ig = ArtifactKey::Fragment {
        scope: FragmentScope::WholeIg,
        fragment: FragmentKind::Summary,
        parameters: BTreeMap::new(),
    };
    let resource = ArtifactKey::Fragment {
        scope: FragmentScope::Resource {
            resource: ResourceKey {
                resource_type: "StructureDefinition".into(),
                id: "demo".into(),
            },
        },
        fragment: FragmentKind::Summary,
        parameters: BTreeMap::new(),
    };
    assert_ne!(whole_ig, resource);

    let asset_path = path("assets/css/main.css");
    let authored = ArtifactKey::Asset {
        namespace: AssetNamespace::Authored,
        path: asset_path.clone(),
    };
    let template = ArtifactKey::Asset {
        namespace: AssetNamespace::Template,
        path: asset_path,
    };
    assert_ne!(authored, template);

    let records = [authored, template].map(|key| ArtifactRecord {
        key,
        state: ArtifactState::Ready {
            content: ContentRef::of_bytes(b"css", Some("text/css")),
        },
        provenance: provenance(),
        reads: BTreeSet::new(),
    });
    assert_eq!(ArtifactCatalog::from_records(records).unwrap().len(), 2);
}

#[test]
fn closed_site_build_accepts_a_ready_plan_and_roundtrips() {
    let build = singleton_fragment_build(ArtifactState::Ready {
        content: ContentRef::of_bytes(b"summary", Some("text/html")),
    });
    let closed = build.close().unwrap();
    let bytes = serde_json::to_vec(&closed).unwrap();
    let decoded: ClosedSiteBuild = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded, closed);
}

#[test]
fn closed_site_build_rejects_deferred_and_unsupported_requirements() {
    let deferred = singleton_fragment_build(ArtifactState::Deferred {
        reason: "requires native callback".into(),
    });
    let error = deferred.close().unwrap_err();
    assert!(matches!(
        error.blockers(),
        [SealBlocker::Deferred { reason, .. }] if reason == "requires native callback"
    ));

    let unsupported = singleton_fragment_build(ArtifactState::Unsupported {
        capability: "tx.expand".into(),
        reason: "terminology unavailable".into(),
    });
    let error = unsupported.close().unwrap_err();
    assert!(matches!(
        error.blockers(),
        [SealBlocker::Unsupported { capability, .. }] if capability == "tx.expand"
    ));
}

#[test]
fn closing_follows_transitive_artifact_reads() {
    let deferred_key = ArtifactKey::Fragment {
        scope: FragmentScope::WholeIg,
        fragment: FragmentKind::Dictionary,
        parameters: BTreeMap::new(),
    };
    let page_key = ArtifactKey::Page {
        path: path("index.html"),
    };
    let deferred = ArtifactRecord {
        key: deferred_key.clone(),
        state: ArtifactState::Deferred {
            reason: "not materialized".into(),
        },
        provenance: provenance(),
        reads: BTreeSet::new(),
    };
    let page = ArtifactRecord {
        key: page_key.clone(),
        state: ArtifactState::Ready {
            content: ContentRef::of_bytes(b"page", Some("text/html")),
        },
        provenance: provenance(),
        reads: BTreeSet::from([ReadDependency::Artifact { key: deferred_key }]),
    };
    let build = SiteBuild::new(
        ProjectRevision {
            project_id: "demo.ig".into(),
            revision: "rev".into(),
            sources: SourceManifest::default(),
        },
        PackageLock::default(),
        RenderTarget {
            renderer: ProducerRef::new("external", "1"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::new(),
        },
        RenderPlan::new([page_key]),
        ArtifactCatalog::from_records([page, deferred]).unwrap(),
        BTreeSet::new(),
    )
    .unwrap();
    assert!(matches!(
        build.close().unwrap_err().blockers(),
        [SealBlocker::Deferred { .. }]
    ));
}

#[test]
fn invalid_failed_state_and_dangling_reads_are_rejected() {
    let project = ProjectRevision {
        project_id: "demo".into(),
        revision: "rev".into(),
        sources: SourceManifest::default(),
    };
    let record = ArtifactRecord {
        key: ArtifactKey::Data {
            namespace: "test".into(),
            name: "failure".into(),
        },
        state: ArtifactState::Failed {
            diagnostics: BTreeSet::new(),
        },
        provenance: provenance(),
        reads: BTreeSet::new(),
    };
    let error = SiteBuild::new(
        project,
        PackageLock::default(),
        RenderTarget {
            renderer: ProducerRef::new("test", "1"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::new(),
        },
        RenderPlan::default(),
        ArtifactCatalog::from_records([record]).unwrap(),
        BTreeSet::new(),
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("failed state has no diagnostics"));
}

#[test]
fn dangling_read_dependencies_are_rejected() {
    let missing = path("input/fsh/missing.fsh");
    let artifact = ArtifactRecord {
        key: ArtifactKey::Data {
            namespace: "test".into(),
            name: "data".into(),
        },
        state: ArtifactState::Deferred {
            reason: "waiting".into(),
        },
        provenance: provenance(),
        reads: BTreeSet::from([ReadDependency::Source {
            path: missing.clone(),
        }]),
    };
    let error = SiteBuild::new(
        ProjectRevision {
            project_id: "demo".into(),
            revision: "rev".into(),
            sources: SourceManifest::default(),
        },
        PackageLock::default(),
        RenderTarget {
            renderer: ProducerRef::new("test", "1"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::new(),
        },
        RenderPlan::default(),
        ArtifactCatalog::from_records([artifact]).unwrap(),
        BTreeSet::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains(missing.as_str()));
}

#[test]
fn digests_and_paths_reject_noncanonical_values() {
    assert!(Sha256Digest::parse("ABC").is_err());
    assert!(SourcePath::parse("../secret").is_err());
    assert!(SourcePath::parse("/absolute").is_err());
    assert!(PackageCoordinate::parse("hl7.fhir.r4.core#latest").is_err());
    assert!(PackageCoordinate::parse("hl7.fhir.r4.core#dev").is_err());
    assert!(PackageCoordinate::parse("hl7.fhir.r4.core#^4.0.1").is_err());
    assert!(PackageCoordinate::parse("hl7.fhir.r4.core#>=4.0.0").is_err());
    assert!(PackageCoordinate::parse("hl7.fhir.r4.core#4.0.1||5.0.0").is_err());
}

#[test]
fn package_lock_rejects_a_wire_key_coordinate_mismatch() {
    let locked = LockedPackage {
        coordinate: package("example.a#1.0.0"),
        content: ContentRef::of_bytes(b"package", Some(PREPARED_PACKAGE_MEDIA_TYPE)),
        dependencies: BTreeSet::new(),
    };
    let mut wire = serde_json::to_value(PackageLock::from_packages([locked]).unwrap()).unwrap();
    wire["example.a#1.0.0"]["coordinate"] = serde_json::Value::String("example.b#1.0.0".into());
    let mismatched: PackageLock = serde_json::from_value(wire).unwrap();
    let error = SiteBuild::new(
        ProjectRevision {
            project_id: "demo".into(),
            revision: "rev".into(),
            sources: SourceManifest::default(),
        },
        mismatched,
        RenderTarget {
            renderer: ProducerRef::new("test", "1"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::new(),
        },
        RenderPlan::default(),
        ArtifactCatalog::default(),
        BTreeSet::new(),
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match embedded coordinate"));
}

#[test]
fn locked_package_keeps_the_content_carrier_wire_field() {
    let locked = LockedPackage {
        coordinate: package("example.a#1.0.0"),
        content: ContentRef::of_bytes(b"prepared package", Some(PREPARED_PACKAGE_MEDIA_TYPE)),
        dependencies: BTreeSet::new(),
    };
    let wire = serde_json::to_value(locked).unwrap();

    assert!(wire.get("content").is_some());
    assert!(wire.get("preparedPackage").is_none());
}

#[test]
fn site_build_v2_rejects_a_non_prepared_package_carrier() {
    let locked = LockedPackage {
        coordinate: package("example.a#1.0.0"),
        content: ContentRef::of_bytes(
            b"legacy normalized payload",
            Some("application/octet-stream"),
        ),
        dependencies: BTreeSet::new(),
    };
    let error = SiteBuild::new(
        ProjectRevision {
            project_id: "demo".into(),
            revision: "rev".into(),
            sources: SourceManifest::default(),
        },
        PackageLock::from_packages([locked]).unwrap(),
        RenderTarget {
            renderer: ProducerRef::new("test", "1"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::new(),
        },
        RenderPlan::default(),
        ArtifactCatalog::default(),
        BTreeSet::new(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("unsupported carrier media type"));
}
