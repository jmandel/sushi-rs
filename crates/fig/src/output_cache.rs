//! Native host integration for exact complete-site output reuse.
//!
//! This module deliberately does not wrap the legacy staged-tree `fig render`
//! path: that path has no canonical [`ClosedSiteBuild`] input and therefore
//! cannot compute a truthful pre-render cache key. Native renderers use these
//! two transitions around their canonical flow instead:
//!
//! 1. [`load`] verifies an exact cache hit before rendering; and
//! 2. [`publish_tree`] imports a renderer's canonical `SiteOutput` and addressed
//!    bytes, verifies them against the same closed build, then atomically
//!    publishes the manifest through [`FileSiteOutputCache`].
//!
//! The cache root contains only the existing native implementations:
//! `manifests/` is a `FileSiteOutputCache`, and `objects/sha256/` is a
//! `FileContentStore`. No additional manifest or cached domain value exists.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use content_store::{ContentStore, FileContentStore};
use site_build::{
    ArtifactState, ClosedSiteBuild, FileSiteOutputCache, OutputCacheKey, RendererImplementation,
    SiteOutput, SiteOutputCache, SITE_OUTPUT_MANIFEST_PATH,
};

/// Result of one verified native output-cache lookup.
#[derive(Clone, Debug)]
pub struct LoadOutcome {
    pub cache_key: OutputCacheKey,
    pub output: Option<SiteOutput>,
}

/// Open a native closed-build bundle and re-verify its canonical manifest plus
/// every content object addressed by the build.
pub fn open_closed_bundle(bundle: &Path) -> Result<ClosedSiteBuild> {
    require_real_dir(bundle, "closed-build bundle")?;
    let manifest_path = bundle.join("site-build.json");
    require_real_file(&manifest_path, "closed-build manifest")?;
    let bytes = fs::read(&manifest_path)
        .with_context(|| format!("read closed-build manifest {}", manifest_path.display()))?;
    let closed: ClosedSiteBuild = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse closed-build manifest {}", manifest_path.display()))?;
    if closed.site_build().canonical_bytes()? != bytes {
        bail!(
            "closed-build manifest is not in canonical SiteBuild form: {}",
            manifest_path.display()
        );
    }

    let object_root = bundle.join("objects/sha256");
    require_real_dir(&object_root, "closed-build object store")?;
    let store = FileContentStore::create(&object_root).context("open closed-build object store")?;
    for content in closed_build_refs(&closed) {
        store
            .read(&content)
            .with_context(|| format!("verify closed-build object {}", content.sha256))?;
    }
    Ok(closed)
}

/// Compute the exact pre-render key for a closed build and renderer recipe.
pub fn cache_key(
    input: &ClosedSiteBuild,
    renderer: &RendererImplementation,
    output_schema: &str,
    options: &BTreeMap<String, String>,
) -> Result<OutputCacheKey> {
    OutputCacheKey::for_closed(input, renderer, output_schema, options).map_err(Into::into)
}

/// Load and completely verify one cached `SiteOutput`. `None` is an ordinary
/// miss. A corrupt manifest or missing/changed object is an error, never a miss.
pub fn load(
    input: &ClosedSiteBuild,
    cache_root: &Path,
    renderer: &RendererImplementation,
    output_schema: &str,
    options: &BTreeMap<String, String>,
) -> Result<LoadOutcome> {
    let key = cache_key(input, renderer, output_schema, options)?;
    let (manifests, objects) = open_output_cache(cache_root)?;
    let output = manifests
        .load(&key, input, &objects)
        .context("load verified SiteOutput cache entry")?;
    Ok(LoadOutcome {
        cache_key: key,
        output,
    })
}

/// Import and verify a renderer's complete published tree, then publish its
/// canonical receipt under the exact pre-render key. The site directory must
/// contain exactly the receipt's files plus `site-output.json`.
pub fn publish_tree(
    input: &ClosedSiteBuild,
    cache_root: &Path,
    site_root: &Path,
) -> Result<(SiteOutput, bool)> {
    require_real_dir(site_root, "site output")?;
    let manifest_path = site_root.join(SITE_OUTPUT_MANIFEST_PATH);
    require_real_file(&manifest_path, "SiteOutput manifest")?;
    let manifest_bytes = fs::read(&manifest_path)
        .with_context(|| format!("read SiteOutput manifest {}", manifest_path.display()))?;
    let output: SiteOutput = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parse SiteOutput manifest {}", manifest_path.display()))?;
    if output.canonical_bytes()? != manifest_bytes {
        bail!(
            "SiteOutput manifest is not in canonical form: {}",
            manifest_path.display()
        );
    }
    output.verify_for(input)?;

    let expected = output
        .files()
        .iter()
        .map(|file| file.path.as_str().to_string())
        .collect::<BTreeSet<_>>();
    let before = list_site_files(site_root)?;
    if before != expected {
        report_inventory_mismatch(&expected, &before)?;
    }

    let (manifests, objects) = open_output_cache(cache_root)?;
    for file in output.files() {
        let path = site_root.join(file.path.as_str());
        require_real_file(&path, "declared site output")?;
        let bytes = fs::read(&path)
            .with_context(|| format!("read declared site output {}", path.display()))?;
        objects
            .put(&file.content, &bytes)
            .with_context(|| format!("import declared site output {}", file.path))?;
    }
    let after = list_site_files(site_root)?;
    if after != before {
        bail!("site output inventory changed while it was being imported");
    }
    let after_manifest = fs::read(&manifest_path)
        .with_context(|| format!("re-read SiteOutput manifest {}", manifest_path.display()))?;
    if after_manifest != manifest_bytes {
        bail!("SiteOutput manifest changed while its files were being imported");
    }

    let already_present = manifests
        .load(output.cache_key(), input, &objects)
        .context("check existing verified SiteOutput cache entry")?
        .is_some();
    manifests
        .publish(&output, input, &objects)
        .context("publish verified SiteOutput cache entry")?;
    Ok((output, already_present))
}

/// Materialize a verified hit into a caller-owned, existing empty staging
/// directory. This is the composition point for native hosts that already own
/// an atomic publication transaction (Cycle's `AtomicOutputPublication`). A
/// failure may leave partial files only in that private staging directory; the
/// caller must abort its transaction.
pub fn materialize(output: &SiteOutput, cache_root: &Path, destination: &Path) -> Result<()> {
    require_real_dir(destination, "output staging directory")?;
    if fs::read_dir(destination)?.next().is_some() {
        bail!(
            "output staging directory is not empty: {}",
            destination.display()
        );
    }
    let (_, objects) = open_output_cache(cache_root)?;
    output.verify_store(&objects)?;
    for file in output.files() {
        let bytes = objects.read(&file.content)?;
        let path = destination.join(file.path.as_str());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, bytes.bytes())
            .with_context(|| format!("materialize cached output {}", file.path))?;
    }
    fs::write(
        destination.join(SITE_OUTPUT_MANIFEST_PATH),
        output.canonical_bytes()?,
    )?;
    let expected = output
        .files()
        .iter()
        .map(|file| file.path.as_str().to_string())
        .collect::<BTreeSet<_>>();
    if list_site_files(destination)? != expected {
        bail!("materialized cache hit does not match its SiteOutput inventory");
    }
    Ok(())
}

fn open_output_cache(cache_root: &Path) -> Result<(FileSiteOutputCache, FileContentStore)> {
    let manifests = FileSiteOutputCache::create(cache_root.join("manifests"))
        .context("open SiteOutput manifest cache")?;
    let objects = FileContentStore::create(cache_root.join("objects/sha256"))
        .context("open SiteOutput object store")?;
    Ok((manifests, objects))
}

fn closed_build_refs(closed: &ClosedSiteBuild) -> Vec<site_build::ContentRef> {
    let build = closed.site_build();
    let mut refs = build
        .project()
        .sources
        .iter()
        .map(|(_, source)| source.content.clone())
        .collect::<Vec<_>>();
    refs.extend(
        build
            .package_lock()
            .iter()
            .map(|(_, package)| package.content.clone()),
    );
    refs.extend(build.artifacts().iter().filter_map(|(_, artifact)| {
        if let ArtifactState::Ready { content } = &artifact.state {
            Some(content.clone())
        } else {
            None
        }
    }));
    refs
}

fn require_real_dir(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{label} is not a real directory: {}", path.display());
    }
    Ok(())
}

fn require_real_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} is not a regular file: {}", path.display());
    }
    Ok(())
}

fn list_site_files(root: &Path) -> Result<BTreeSet<String>> {
    let mut files = BTreeSet::new();
    let mut pending = vec![(root.to_path_buf(), PathBuf::new())];
    while let Some((directory, relative)) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .with_context(|| format!("enumerate site output {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let metadata = entry
                .file_type()
                .with_context(|| format!("inspect site output {}", entry.path().display()))?;
            if metadata.is_symlink() {
                bail!(
                    "site output may not contain symlinks: {}",
                    entry.path().display()
                );
            }
            let child_relative = relative.join(entry.file_name());
            if metadata.is_dir() {
                pending.push((entry.path(), child_relative));
            } else if metadata.is_file() {
                let normalized = child_relative
                    .components()
                    .map(|component| {
                        let std::path::Component::Normal(value) = component else {
                            unreachable!("read_dir child path is normalized");
                        };
                        value.to_str().ok_or_else(|| {
                            anyhow::anyhow!(
                                "site output path is not UTF-8: {}",
                                entry.path().display()
                            )
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
                    .join("/");
                if normalized != SITE_OUTPUT_MANIFEST_PATH {
                    files.insert(normalized);
                }
            } else {
                bail!(
                    "site output member is not a regular file: {}",
                    entry.path().display()
                );
            }
        }
    }
    Ok(files)
}

fn report_inventory_mismatch(expected: &BTreeSet<String>, actual: &BTreeSet<String>) -> Result<()> {
    let missing = expected.difference(actual).cloned().collect::<Vec<_>>();
    let extra = actual.difference(expected).cloned().collect::<Vec<_>>();
    bail!(
        "site output inventory does not match SiteOutput; missing=[{}], extra=[{}]",
        missing.join(", "),
        extra.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use site_build::{
        ArtifactCatalog, ContentRef, OutputPath, OutputProducer, PackageLock, ProducerRef,
        ProjectRevision, RenderMode, RenderPlan, RenderTarget, Sha256Digest, SiteBuild,
        SiteOutputFile, SourceManifest,
    };
    fn fixture() -> (
        tempfile::TempDir,
        ClosedSiteBuild,
        RendererImplementation,
        SiteOutput,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let closed = SiteBuild::new(
            ProjectRevision {
                project_id: "demo.ig".into(),
                revision: "exact".into(),
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
        .unwrap();
        let renderer = RendererImplementation {
            id: "cycle-site".into(),
            version: "1".into(),
            recipe_sha256: Sha256Digest::of_bytes(b"renderer recipe"),
        };
        let output = SiteOutput::new(
            &closed,
            renderer.clone(),
            "cycle-static-site/v1",
            BTreeMap::from([("minify".into(), "true".into())]),
            [SiteOutputFile {
                path: OutputPath::parse("en/index.html").unwrap(),
                content: ContentRef::of_bytes(b"<h1>Demo</h1>", Some("text/html")),
                producer: OutputProducer {
                    id: "cycle-site".into(),
                    version: "1".into(),
                },
                source: Some("render-page".into()),
                owner: None,
            }],
        )
        .unwrap();
        (temp, closed, renderer, output)
    }

    fn write_bundle(root: &Path, closed: &ClosedSiteBuild) {
        fs::create_dir_all(root.join("objects/sha256")).unwrap();
        fs::write(
            root.join("site-build.json"),
            closed.site_build().canonical_bytes().unwrap(),
        )
        .unwrap();
    }

    fn write_site(root: &Path, output: &SiteOutput) {
        fs::create_dir_all(root.join("en")).unwrap();
        fs::write(root.join("en/index.html"), b"<h1>Demo</h1>").unwrap();
        fs::write(
            root.join(SITE_OUTPUT_MANIFEST_PATH),
            output.canonical_bytes().unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn miss_publish_verified_hit_and_atomic_materialization_share_one_site_output() {
        let (temp, closed, renderer, output) = fixture();
        let bundle = temp.path().join("bundle");
        let cache = temp.path().join("cache");
        let site = temp.path().join("site");
        write_bundle(&bundle, &closed);
        write_site(&site, &output);
        let opened = open_closed_bundle(&bundle).unwrap();
        let options = BTreeMap::from([("minify".into(), "true".into())]);

        let miss = load(&opened, &cache, &renderer, "cycle-static-site/v1", &options).unwrap();
        assert!(miss.output.is_none());
        assert_eq!(miss.cache_key, output.cache_key().clone());

        let (published, already_present) = publish_tree(&opened, &cache, &site).unwrap();
        assert!(!already_present);
        assert_eq!(published, output);

        let hit = load(&opened, &cache, &renderer, "cycle-static-site/v1", &options).unwrap();
        assert_eq!(hit.output.as_ref(), Some(&output));

        let restored = temp.path().join("restored");
        fs::create_dir(&restored).unwrap();
        materialize(hit.output.as_ref().unwrap(), &cache, &restored).unwrap();
        assert_eq!(
            fs::read(restored.join("en/index.html")).unwrap(),
            b"<h1>Demo</h1>"
        );
        assert_eq!(
            fs::read(restored.join(SITE_OUTPUT_MANIFEST_PATH)).unwrap(),
            output.canonical_bytes().unwrap()
        );

        let (_, repeated) = publish_tree(&opened, &cache, &site).unwrap();
        assert!(repeated);
        assert!(materialize(&output, &cache, &restored)
            .unwrap_err()
            .to_string()
            .contains("not empty"));
    }

    #[test]
    fn corrupt_or_incomplete_cached_output_never_degrades_to_a_miss() {
        let (temp, closed, renderer, output) = fixture();
        let bundle = temp.path().join("bundle");
        let cache = temp.path().join("cache");
        let site = temp.path().join("site");
        write_bundle(&bundle, &closed);
        write_site(&site, &output);
        let opened = open_closed_bundle(&bundle).unwrap();
        publish_tree(&opened, &cache, &site).unwrap();
        let object = cache
            .join("objects/sha256")
            .join(output.files()[0].content.sha256.as_str());
        fs::remove_file(object).unwrap();
        let error = load(
            &opened,
            &cache,
            &renderer,
            "cycle-static-site/v1",
            &BTreeMap::from([("minify".into(), "true".into())]),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("load verified SiteOutput cache entry"));
    }

    #[test]
    fn publication_rejects_extra_files_and_noncanonical_receipts() {
        let (temp, closed, _renderer, output) = fixture();
        let bundle = temp.path().join("bundle");
        let cache = temp.path().join("cache");
        let site = temp.path().join("site");
        write_bundle(&bundle, &closed);
        write_site(&site, &output);
        let opened = open_closed_bundle(&bundle).unwrap();
        fs::write(site.join("extra.txt"), b"extra").unwrap();
        assert!(publish_tree(&opened, &cache, &site)
            .unwrap_err()
            .to_string()
            .contains("extra=[extra.txt]"));
        fs::remove_file(site.join("extra.txt")).unwrap();
        let mut noncanonical = output.canonical_bytes().unwrap();
        noncanonical.push(b'\n');
        fs::write(site.join(SITE_OUTPUT_MANIFEST_PATH), noncanonical).unwrap();
        assert!(publish_tree(&opened, &cache, &site)
            .unwrap_err()
            .to_string()
            .contains("not in canonical form"));
    }
}
