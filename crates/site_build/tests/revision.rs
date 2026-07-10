use std::collections::{BTreeMap, BTreeSet};

use site_build::*;

fn path(value: &str) -> SourcePath {
    SourcePath::parse(value).unwrap()
}

fn provenance(recipe: &str) -> ArtifactProvenance {
    ArtifactProvenance {
        producer: ProducerRef::new("test.stock-renderer", "1"),
        recipe: recipe.into(),
        attributes: BTreeMap::new(),
    }
}

fn open_build() -> SiteBuild {
    let fragment = ArtifactKey::Fragment {
        scope: FragmentScope::WholeIg,
        fragment: FragmentKind::Summary,
        parameters: BTreeMap::new(),
    };
    SiteBuild::new(
        ProjectRevision {
            project_id: "example.ig".into(),
            revision: "sources".into(),
            sources: SourceManifest::from_entries([(
                path("input/fsh/example.fsh"),
                SourceEntry {
                    kind: SourceKind::Fsh,
                    content: ContentRef::of_bytes(b"Profile: Example", Some("text/fhir-shorthand")),
                },
            )])
            .unwrap(),
        },
        PackageLock::default(),
        RenderTarget {
            renderer: ProducerRef::new("stock-template", "1"),
            mode: RenderMode::NativeTemplate,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::new(),
        },
        RenderPlan::default(),
        ArtifactCatalog::from_records([ArtifactRecord {
            key: fragment,
            state: ArtifactState::Deferred {
                reason: "not requested".into(),
            },
            provenance: provenance("fragment"),
            reads: BTreeSet::new(),
        }])
        .unwrap(),
        BTreeSet::new(),
    )
    .unwrap()
}

fn resolutions(reverse: bool) -> Vec<ArtifactResolution> {
    let fragment = ArtifactKey::Fragment {
        scope: FragmentScope::WholeIg,
        fragment: FragmentKind::Summary,
        parameters: BTreeMap::new(),
    };
    let page = ArtifactKey::Page {
        path: path("en/index.html"),
    };
    let fragment_resolution = ArtifactResolution::ready(
        fragment.clone(),
        b"<div>summary</div>".to_vec(),
        Some("text/html"),
        provenance("fragment"),
        BTreeSet::from([ReadDependency::Source {
            path: path("input/fsh/example.fsh"),
        }]),
    );
    let page_resolution = ArtifactResolution::ready(
        page,
        b"<html>summary</html>".to_vec(),
        Some("text/html"),
        provenance("page"),
        BTreeSet::from([ReadDependency::Artifact { key: fragment }]),
    );
    if reverse {
        vec![page_resolution, fragment_resolution]
    } else {
        vec![fragment_resolution, page_resolution]
    }
}

#[test]
fn successor_batches_are_order_independent_and_leave_predecessor_unchanged() {
    let predecessor = open_build();
    let before = predecessor.clone();
    let plan = RenderPlan::new([ArtifactKey::Page {
        path: path("en/index.html"),
    }]);
    let forward = predecessor
        .successor(Some(plan.clone()), resolutions(false))
        .unwrap();
    let reverse = predecessor
        .successor(Some(plan), resolutions(true))
        .unwrap();

    assert_eq!(predecessor, before);
    assert_eq!(forward.predecessor(), predecessor.build_id());
    assert_eq!(forward.site_build(), reverse.site_build());
    assert_eq!(forward.objects(), reverse.objects());
    assert_eq!(forward.objects().len(), 2);
    forward.clone().into_site_build().close().unwrap();
}

#[test]
fn transitive_non_ready_states_remain_typed_after_a_successor_transition() {
    let predecessor = open_build();
    let fragment = ArtifactKey::Fragment {
        scope: FragmentScope::WholeIg,
        fragment: FragmentKind::Summary,
        parameters: BTreeMap::new(),
    };
    let page = ArtifactKey::Page {
        path: path("en/index.html"),
    };
    let page_resolution = ArtifactResolution::ready(
        page.clone(),
        b"page".to_vec(),
        Some("text/html"),
        provenance("page"),
        BTreeSet::from([ReadDependency::Artifact {
            key: fragment.clone(),
        }]),
    );
    let unsupported = ArtifactResolution::non_ready(
        fragment.clone(),
        ArtifactState::Unsupported {
            capability: "publisher.fragment.summary".into(),
            reason: "renderer gap".into(),
        },
        provenance("fragment"),
        BTreeSet::new(),
    )
    .unwrap();
    let successor = predecessor
        .successor(
            Some(RenderPlan::new([page])),
            [page_resolution, unsupported],
        )
        .unwrap();
    assert!(matches!(
        successor.into_site_build().close().unwrap_err().blockers(),
        [SealBlocker::Unsupported { key, capability, .. }]
            if key == &fragment && capability == "publisher.fragment.summary"
    ));
}

#[test]
fn failed_resolution_is_validated_and_duplicate_keys_are_rejected_atomically() {
    let predecessor = open_build();
    let key = ArtifactKey::Data {
        namespace: "test".into(),
        name: "failure".into(),
    };
    let failed = ArtifactResolution::non_ready(
        key.clone(),
        ArtifactState::Failed {
            diagnostics: BTreeSet::from([BuildDiagnostic::new(
                DiagnosticSeverity::Error,
                "render.failed",
                "could not render",
            )]),
        },
        provenance("failure"),
        BTreeSet::new(),
    )
    .unwrap();
    let error = predecessor
        .successor(None, [failed.clone(), failed])
        .unwrap_err();
    assert!(matches!(error, RevisionError::DuplicateResolution(k) if k == key));
    assert!(predecessor.artifacts().get(&key).is_none());

    assert!(ArtifactResolution::non_ready(
        key.clone(),
        ArtifactState::Ready {
            content: ContentRef::of_bytes(b"orphan", Some("text/plain")),
        },
        provenance("bad"),
        BTreeSet::new(),
    )
    .is_err());
}

#[test]
fn cas_object_set_is_deterministic_when_identical_bytes_have_different_media_types() {
    let predecessor = open_build();
    let a = ArtifactResolution::ready(
        ArtifactKey::Data {
            namespace: "test".into(),
            name: "a".into(),
        },
        b"same".to_vec(),
        Some("text/plain"),
        provenance("a"),
        BTreeSet::new(),
    );
    let b = ArtifactResolution::ready(
        ArtifactKey::Data {
            namespace: "test".into(),
            name: "b".into(),
        },
        b"same".to_vec(),
        Some("application/octet-stream"),
        provenance("b"),
        BTreeSet::new(),
    );
    let forward = predecessor.successor(None, [a.clone(), b.clone()]).unwrap();
    let reverse = predecessor.successor(None, [b, a]).unwrap();
    assert_eq!(forward.site_build(), reverse.site_build());
    assert_eq!(forward.objects(), reverse.objects());
    assert_eq!(forward.objects().len(), 1);
}

#[test]
fn successor_returns_only_objects_absent_from_the_predecessor_cas() {
    let predecessor = open_build();
    let source_bytes = b"Profile: Example";
    let source_reuse = ArtifactResolution::ready(
        ArtifactKey::Data {
            namespace: "test".into(),
            name: "source-reuse".into(),
        },
        source_bytes.to_vec(),
        Some("text/plain"),
        provenance("source-reuse"),
        BTreeSet::new(),
    );
    let reused_source = predecessor.successor(None, [source_reuse]).unwrap();
    assert!(reused_source.objects().is_empty());

    let first = ArtifactResolution::ready(
        ArtifactKey::Data {
            namespace: "test".into(),
            name: "first".into(),
        },
        b"shared artifact".to_vec(),
        Some("text/plain"),
        provenance("first"),
        BTreeSet::new(),
    );
    let first_successor = predecessor.successor(None, [first]).unwrap();
    assert_eq!(first_successor.objects().len(), 1);

    let second = ArtifactResolution::ready(
        ArtifactKey::Data {
            namespace: "test".into(),
            name: "second".into(),
        },
        b"shared artifact".to_vec(),
        Some("application/octet-stream"),
        provenance("second"),
        BTreeSet::new(),
    );
    let second_successor = first_successor
        .site_build()
        .successor(None, [second])
        .unwrap();
    assert!(second_successor.objects().is_empty());
}
