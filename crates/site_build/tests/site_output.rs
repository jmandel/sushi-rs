use std::collections::{BTreeMap, BTreeSet};

use content_store::{ContentStore, FileContentStore};
use site_build::*;

fn closed(project: &str) -> ClosedSiteBuild {
    SiteBuild::new(
        ProjectRevision {
            project_id: project.into(),
            revision: "exact-sources".into(),
            sources: SourceManifest::default(),
        },
        PackageLock::default(),
        RenderTarget {
            renderer: ProducerRef::new("cycle-site", "2"),
            mode: RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::from([("contract".into(), "cycle-site/v2".into())]),
        },
        RenderPlan::default(),
        ArtifactCatalog::default(),
        BTreeSet::new(),
    )
    .unwrap()
    .close()
    .unwrap()
}

fn renderer(recipe: &[u8]) -> RendererImplementation {
    RendererImplementation {
        id: "cycle-site".into(),
        version: "1.0.0".into(),
        recipe_sha256: Sha256Digest::of_bytes(recipe),
    }
}

fn file(path: &str, bytes: &[u8]) -> SiteOutputFile {
    SiteOutputFile {
        path: OutputPath::parse(path).unwrap(),
        content: ContentRef::of_bytes(bytes, Some("text/html")),
        producer: OutputProducer {
            id: "cycle-page".into(),
            version: "1".into(),
        },
        source: Some("page recipe".into()),
        owner: None,
    }
}

#[test]
fn cache_key_binds_exact_derivation_and_output_id_adds_bytes() {
    let input = closed("example.ig");
    let options = BTreeMap::from([("minify".into(), "true".into())]);
    let original = SiteOutput::new(
        &input,
        renderer(b"recipe-a"),
        "static-site/v1",
        options.clone(),
        [file("index.html", b"first")],
    )
    .unwrap();
    assert_eq!(
        OutputCacheKey::for_closed(&input, &renderer(b"recipe-a"), "static-site/v1", &options)
            .unwrap(),
        original.cache_key().clone()
    );
    let changed_bytes = SiteOutput::new(
        &input,
        renderer(b"recipe-a"),
        "static-site/v1",
        options.clone(),
        [file("index.html", b"second")],
    )
    .unwrap();
    assert_eq!(original.cache_key(), changed_bytes.cache_key());
    assert_ne!(original.output_id(), changed_bytes.output_id());

    let changed_recipe = SiteOutput::new(
        &input,
        renderer(b"recipe-b"),
        "static-site/v1",
        options.clone(),
        [file("index.html", b"first")],
    )
    .unwrap();
    let changed_options = SiteOutput::new(
        &input,
        renderer(b"recipe-a"),
        "static-site/v1",
        BTreeMap::from([("minify".into(), "false".into())]),
        [file("index.html", b"first")],
    )
    .unwrap();
    let changed_input = SiteOutput::new(
        &closed("other.ig"),
        renderer(b"recipe-a"),
        "static-site/v1",
        options,
        [file("index.html", b"first")],
    )
    .unwrap();
    assert_ne!(original.cache_key(), changed_recipe.cache_key());
    assert_ne!(original.cache_key(), changed_options.cache_key());
    assert_ne!(original.cache_key(), changed_input.cache_key());
}

#[test]
fn output_paths_are_safe_unique_sorted_and_owner_closed() {
    for unsafe_path in [
        "",
        "/index.html",
        "../index.html",
        "a//b",
        "a\\b",
        "C:/site.html",
        "line\nbreak.html",
    ] {
        assert!(OutputPath::parse(unsafe_path).is_err());
    }
    assert!(OutputPath::parse(SITE_OUTPUT_MANIFEST_PATH).is_err());

    let input = closed("example.ig");
    let duplicate = SiteOutput::new(
        &input,
        renderer(b"recipe"),
        "static-site/v1",
        BTreeMap::new(),
        [file("index.html", b"a"), file("index.html", b"b")],
    );
    assert!(matches!(duplicate, Err(SiteOutputError::DuplicatePath(_))));

    let mut orphan = file("assets/app.js", b"js");
    orphan.content.media_type = Some("text/javascript".into());
    orphan.owner = Some(OutputPath::parse("index.html").unwrap());
    assert!(matches!(
        SiteOutput::new(
            &input,
            renderer(b"recipe"),
            "static-site/v1",
            BTreeMap::new(),
            [orphan]
        ),
        Err(SiteOutputError::MissingOwner { .. })
    ));
}

#[test]
fn wire_roundtrip_rechecks_ids() {
    let output = SiteOutput::new(
        &closed("example.ig"),
        renderer(b"recipe"),
        "static-site/v1",
        BTreeMap::from([("locale".into(), "en".into())]),
        [file("index.html", b"hello")],
    )
    .unwrap();
    assert!(output.cache_key().as_str().starts_with("sok1-sha256:"));
    assert!(output.output_id().as_str().starts_with("so1-sha256:"));
    let bytes = output.canonical_bytes().unwrap();
    // Fixed independently in Cycle's browser implementation. This catches
    // UTF-8 ordering or canonical-field drift across Rust and JavaScript.
    assert_eq!(
        output.cache_key().as_str(),
        "sok1-sha256:52a6568c5df7d5db15d43a1c5c1ce4eb0a64cffad5f4c2dc53ba09335180af2b"
    );
    assert_eq!(
        output.output_id().as_str(),
        "so1-sha256:5c395c8bde04a11939c040de1bb920dc720db9e859453dea647560b46b18f0c1"
    );
    assert_eq!(
        serde_json::from_slice::<SiteOutput>(&bytes).unwrap(),
        output
    );

    let mut tampered = serde_json::to_value(&output).unwrap();
    tampered["files"][0]["content"]["byteLength"] = 999.into();
    assert!(serde_json::from_value::<SiteOutput>(tampered).is_err());
}

#[test]
fn store_verification_reads_every_addressed_byte() {
    let input = closed("example.ig");
    let index = b"index";
    let output = SiteOutput::new(
        &input,
        renderer(b"recipe"),
        "static-site/v1",
        BTreeMap::new(),
        [file("index.html", index)],
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let store = FileContentStore::create(temp.path()).unwrap();
    assert!(output.verify_store(&store).is_err());
    store.put(&output.files()[0].content, index).unwrap();
    output.verify_store(&store).unwrap();
    output.verify_cached(&input, &store).unwrap();
    assert!(output.verify_for(&closed("other.ig")).is_err());
}

#[test]
fn filesystem_output_cache_is_exact_verified_and_no_clobber() {
    let input = closed("example.ig");
    let first_bytes = b"first";
    let first = SiteOutput::new(
        &input,
        renderer(b"recipe"),
        "static-site/v1",
        BTreeMap::new(),
        [file("index.html", first_bytes)],
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let objects = FileContentStore::create(temp.path().join("objects")).unwrap();
    let cache = FileSiteOutputCache::create(temp.path().join("outputs")).unwrap();
    assert!(cache
        .load(first.cache_key(), &input, &objects)
        .unwrap()
        .is_none());

    objects.put(&first.files()[0].content, first_bytes).unwrap();
    cache.publish(&first, &input, &objects).unwrap();
    cache.publish(&first, &input, &objects).unwrap();
    assert_eq!(
        cache
            .load(first.cache_key(), &input, &objects)
            .unwrap()
            .unwrap()
            .output_id(),
        first.output_id()
    );

    // The lookup key deliberately excludes output bytes. A renderer producing
    // different bytes under the same exact recipe is nondeterministic and must
    // not replace the already-published cache entry.
    let second_bytes = b"second";
    let second = SiteOutput::new(
        &input,
        renderer(b"recipe"),
        "static-site/v1",
        BTreeMap::new(),
        [file("index.html", second_bytes)],
    )
    .unwrap();
    assert_eq!(first.cache_key(), second.cache_key());
    objects
        .put(&second.files()[0].content, second_bytes)
        .unwrap();
    assert!(matches!(
        cache.publish(&second, &input, &objects),
        Err(SiteOutputCacheError::Collision { .. })
    ));
    assert_eq!(
        cache
            .load(first.cache_key(), &input, &objects)
            .unwrap()
            .unwrap()
            .output_id(),
        first.output_id()
    );
}

#[test]
fn filesystem_output_cache_rejects_missing_or_corrupt_objects() {
    let input = closed("example.ig");
    let bytes = b"body";
    let output = SiteOutput::new(
        &input,
        renderer(b"recipe"),
        "static-site/v1",
        BTreeMap::new(),
        [file("index.html", bytes)],
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let objects = FileContentStore::create(temp.path().join("objects")).unwrap();
    let cache = FileSiteOutputCache::create(temp.path().join("outputs")).unwrap();
    assert!(cache.publish(&output, &input, &objects).is_err());

    objects.put(&output.files()[0].content, bytes).unwrap();
    cache.publish(&output, &input, &objects).unwrap();
    std::fs::write(
        objects.object_path(&output.files()[0].content.sha256),
        b"bad!",
    )
    .unwrap();
    assert!(cache.load(output.cache_key(), &input, &objects).is_err());
}
