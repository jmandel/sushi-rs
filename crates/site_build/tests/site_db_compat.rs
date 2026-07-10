#![cfg(feature = "site-db-compat")]

use std::collections::{BTreeMap, BTreeSet};

use site_build::{
    site_db_compat, ArtifactState, ContentRef, PackageLock, ProducerRef, ProjectRevision,
    ReadDependency, RenderMode, RenderTarget, SourceEntry, SourceKind, SourceManifest, SourcePath,
};

#[test]
fn site_db_is_quarantined_as_one_content_addressed_legacy_artifact() {
    let mut db = site_db::SiteDb::default();
    db.metadata.push(site_db::model::MetadataRow {
        key: 1,
        name: "packageId".into(),
        value: "demo.ig".into(),
    });
    let projection = site_db_compat::project(&db).unwrap();
    assert_eq!(projection.record.provenance.recipe, site_db_compat::FORMAT);
    let ArtifactState::Ready { content } = &projection.record.state else {
        panic!("compatibility projection must be ready")
    };
    assert_eq!(
        content,
        &ContentRef::of_bytes(&projection.bytes, Some("application/json"))
    );
    assert!(String::from_utf8(projection.bytes)
        .unwrap()
        .contains("demo.ig"));
}

fn close_input(mode: RenderMode) -> site_db_compat::CloseProjectionInput {
    let config_path = SourcePath::parse("sushi-config.yaml").unwrap();
    site_db_compat::CloseProjectionInput {
        project: ProjectRevision {
            project_id: "demo.ig".into(),
            revision: "sources-sha256:test".into(),
            sources: SourceManifest::from_entries([(
                config_path,
                SourceEntry {
                    kind: SourceKind::Config,
                    content: ContentRef::of_bytes(b"id: demo.ig", Some("application/yaml")),
                },
            )])
            .unwrap(),
        },
        package_lock: PackageLock::default(),
        render_target: RenderTarget {
            renderer: ProducerRef::new("cycle-site", "1"),
            mode,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::from([("contract".into(), "cycle-site/v1".into())]),
        },
        diagnostics: BTreeSet::new(),
    }
}

#[test]
fn shared_close_projection_builds_the_closed_one_object_handoff() {
    let db = site_db::SiteDb::default();
    let projection =
        site_db_compat::close_projection(&db, close_input(RenderMode::ExternalBuilder)).unwrap();
    let build = projection.site_build.site_build();
    assert_eq!(build.render_plan().required_artifacts().len(), 1);
    let record = build.artifacts().iter().next().unwrap().1;
    assert!(record.reads.contains(&ReadDependency::Source {
        path: SourcePath::parse("sushi-config.yaml").unwrap(),
    }));
    let ArtifactState::Ready { content } = &record.state else {
        panic!("closed compatibility artifact must be ready")
    };
    assert_eq!(
        content,
        &ContentRef::of_bytes(&projection.bytes, Some("application/json"))
    );
    build.verify().unwrap();
}

#[test]
fn shared_close_projection_refuses_a_native_template_target() {
    let error = site_db_compat::close_projection(
        &site_db::SiteDb::default(),
        close_input(RenderMode::NativeTemplate),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        site_db_compat::CloseProjectionError::NotExternalBuilder
    ));
}
