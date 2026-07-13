//! Private native filesystem validation shared by the Fig lifecycle adapters.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use content_store::ContentStore;
use serde::Serialize;
use site_build::ClosedSiteBuild;

/// Parse and authenticate the canonical manifest and bundle layout. The caller
/// must then authenticate and admit the object closure exactly once through
/// `SiteEngine::restore`.
pub(crate) fn open_closed_bundle_manifest(bundle: &Path) -> Result<ClosedSiteBuild> {
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
    Ok(closed)
}

pub(crate) fn require_disjoint_paths(paths: &[&Path]) -> Result<()> {
    let normalized = paths
        .iter()
        .map(|path| canonical_existing_or_future(path))
        .collect::<Result<Vec<_>>>()?;
    for left in 0..normalized.len() {
        for right in (left + 1)..normalized.len() {
            if normalized[left].starts_with(&normalized[right])
                || normalized[right].starts_with(&normalized[left])
            {
                bail!(
                    "protected paths overlap: {} and {}",
                    paths[left].display(),
                    paths[right].display()
                );
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivateOutputCacheKey<'a> {
    schema_version: &'a str,
    input_build_id: &'a site_build::OutputInputId,
    renderer: &'a site_build::RendererImplementation,
    output_schema: &'a str,
    options: &'a std::collections::BTreeMap<String, String>,
}

fn output_cache_key(output: &site_build::SiteOutput) -> Result<String> {
    let bytes = site_build::canonical_json_bytes(&PrivateOutputCacheKey {
        schema_version: output.schema_version(),
        input_build_id: output.input_build_id(),
        renderer: output.renderer(),
        output_schema: output.output_schema(),
        options: output.options(),
    })?;
    Ok(format!(
        "sok1-sha256:{}",
        site_build::Sha256Digest::of_bytes(&bytes)
    ))
}

fn cached_manifest_path(root: &Path, key: &str) -> Result<PathBuf> {
    let digest = key
        .strip_prefix("sok1-sha256:")
        .ok_or_else(|| anyhow::anyhow!("invalid private SiteOutput cache key"))?;
    site_build::Sha256Digest::parse(digest.to_string())?;
    Ok(root.join(format!("{digest}.json")))
}

fn read_cached_output(
    path: &Path,
    expected_key: &str,
    input: &ClosedSiteBuild,
    objects: &dyn ContentStore,
) -> Result<Option<site_build::SiteOutput>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect cache manifest {}", path.display()))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "SiteOutput cache manifest is not a regular file: {}",
            path.display()
        );
    }
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open cache manifest {}", path.display()))?;
    let opened = file.metadata()?;
    let after = fs::symlink_metadata(path)?;
    if after.file_type().is_symlink() || !opened.is_file() || !after.is_file() {
        bail!(
            "SiteOutput cache manifest changed while opening: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != opened.dev()
            || metadata.ino() != opened.ino()
            || after.dev() != opened.dev()
            || after.ino() != opened.ino()
        {
            bail!(
                "SiteOutput cache manifest changed while opening: {}",
                path.display()
            );
        }
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    let output: site_build::SiteOutput = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse cache manifest {}", path.display()))?;
    if output.canonical_bytes()? != bytes {
        bail!(
            "SiteOutput cache manifest is not canonical: {}",
            path.display()
        );
    }
    let actual_key = output_cache_key(&output)?;
    if actual_key != expected_key {
        bail!("SiteOutput cache manifest is stored under the wrong private key");
    }
    output.verify_for(input)?;
    output.verify_store(objects)?;
    Ok(Some(output))
}

/// Private native optimization: publish a verified canonical receipt under a
/// derivation-only pointer. Neither the key nor this store is part of Fig's
/// functional Build API.
pub(crate) fn publish_site_output_cache(
    cache_root: &Path,
    output: &site_build::SiteOutput,
    input: &ClosedSiteBuild,
    objects: &dyn ContentStore,
) -> Result<()> {
    output.verify_for(input)?;
    output.verify_store(objects)?;
    let root = cache_root.join("manifests");
    fs::create_dir_all(&root)
        .with_context(|| format!("create SiteOutput cache {}", root.display()))?;
    let metadata = fs::symlink_metadata(&root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "SiteOutput cache root is not a real directory: {}",
            root.display()
        );
    }
    let root = root.canonicalize()?;
    let key = output_cache_key(output)?;
    let destination = cached_manifest_path(&root, &key)?;
    if let Some(existing) = read_cached_output(&destination, &key, input, objects)? {
        if existing.output_id() == output.output_id() {
            return Ok(());
        }
        bail!(
            "private SiteOutput cache key names different outputs: {} and {}",
            existing.output_id(),
            output.output_id()
        );
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&root)?;
    temporary.write_all(&output.canonical_bytes()?)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&destination) {
        Ok(_) => {
            #[cfg(unix)]
            std::fs::File::open(&root)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = read_cached_output(&destination, &key, input, objects)?
                .ok_or_else(|| anyhow::anyhow!("cache manifest vanished after collision"))?;
            if existing.output_id() == output.output_id() {
                Ok(())
            } else {
                bail!(
                    "private SiteOutput cache key names different outputs: {} and {}",
                    existing.output_id(),
                    output.output_id()
                )
            }
        }
        Err(error) => Err(error.error)
            .with_context(|| format!("publish cache manifest {}", destination.display())),
    }
}

fn canonical_existing_or_future(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(cursor) {
            Ok(_) => {
                let mut resolved = cursor
                    .canonicalize()
                    .with_context(|| format!("canonicalize protected path {}", cursor.display()))?;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor.file_name().ok_or_else(|| {
                    anyhow::anyhow!(
                        "protected path has no existing ancestor: {}",
                        path.display()
                    )
                })?;
                missing.push(name.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    anyhow::anyhow!("protected path has no parent: {}", path.display())
                })?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect protected path {}", cursor.display()));
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use content_store::{ContentStore, FileContentStore};
    use site_build::{
        ArtifactCatalog, ContentRef, OutputPath, OutputProducer, PackageLock, ProjectIdentity,
        RenderMode, RenderPlan, RenderTarget, RendererImplementation, Sha256Digest, SiteBuild,
        SiteOutput, SiteOutputFile, SourceManifest,
    };

    use super::{cached_manifest_path, output_cache_key, publish_site_output_cache};

    fn closed(project: &str) -> site_build::ClosedSiteBuild {
        SiteBuild::new(
            ProjectIdentity {
                project_id: project.into(),
                revision: "exact-sources".into(),
                sources: SourceManifest::default(),
            },
            PackageLock::default(),
            RenderTarget {
                renderer: site_build::ProducerRef::new("cycle-site", "2"),
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

    fn renderer() -> RendererImplementation {
        RendererImplementation {
            id: "cycle-site".into(),
            version: "1.0.0".into(),
            recipe_sha256: Sha256Digest::of_bytes(b"recipe"),
        }
    }

    fn output(input: &site_build::ClosedSiteBuild, bytes: &[u8]) -> SiteOutput {
        SiteOutput::new(
            input,
            renderer(),
            "static-site/v1",
            BTreeMap::from([("locale".into(), "en".into())]),
            [SiteOutputFile {
                path: OutputPath::parse("index.html").unwrap(),
                content: ContentRef::of_bytes(bytes, Some("text/html")),
                producer: OutputProducer {
                    id: "cycle-page".into(),
                    version: "1".into(),
                },
                source: Some("page recipe".into()),
                owner: None,
            }],
        )
        .unwrap()
    }

    fn store_with(root: &Path, output: &SiteOutput, bytes: &[u8]) -> FileContentStore {
        let store = FileContentStore::create(root).unwrap();
        store.put(&output.files()[0].content, bytes).unwrap();
        store
    }

    use std::path::Path;

    #[test]
    fn private_key_matches_the_independent_cycle_fixture() {
        let output: SiteOutput = serde_json::from_value(serde_json::json!({
            "schemaVersion": "site-output/v1",
            "inputBuildId": "sb1-sha256:5eb1101c55a13f90a6af2ef851eb32705b663caf669dc8b596baad690f15495d",
            "renderer": {
                "id": "cycle-site",
                "version": "1.0.0",
                "recipeSha256": "e1d8e552330911f9f779f85b6f2c00a15e790dcc3fbb3b28f5da1d660a30c5b8"
            },
            "outputSchema": "static-site/v1",
            "options": { "locale": "en" },
            "files": [{
                "path": "index.html",
                "content": {
                    "sha256": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
                    "byteLength": 5,
                    "mediaType": "text/html"
                },
                "producer": { "id": "cycle-page", "version": "1" },
                "source": "page recipe"
            }],
            "outputId": "so1-sha256:35e7078d4768dd1565de1097e61ec4e09d809a127275a934626eeae09e36eee5"
        }))
        .unwrap();

        assert_eq!(
            output_cache_key(&output).unwrap(),
            "sok1-sha256:52a6568c5df7d5db15d43a1c5c1ce4eb0a64cffad5f4c2dc53ba09335180af2b"
        );
    }

    #[test]
    fn publication_is_idempotent_and_does_not_replace_the_manifest() {
        let input = closed("example.ig");
        let output = output(&input, b"hello");
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let store = store_with(&temp.path().join("objects"), &output, b"hello");

        publish_site_output_cache(&cache, &output, &input, &store).unwrap();
        let root = cache.join("manifests").canonicalize().unwrap();
        let manifest = cached_manifest_path(&root, &output_cache_key(&output).unwrap()).unwrap();
        let before = std::fs::metadata(&manifest).unwrap();
        let bytes = std::fs::read(&manifest).unwrap();

        publish_site_output_cache(&cache, &output, &input, &store).unwrap();

        assert_eq!(std::fs::read(&manifest).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let after = std::fs::metadata(&manifest).unwrap();
            assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        }
    }

    #[test]
    fn same_derivation_with_different_output_collides_without_clobbering() {
        let input = closed("example.ig");
        let first = output(&input, b"hello");
        let second = output(&input, b"other");
        assert_eq!(
            output_cache_key(&first).unwrap(),
            output_cache_key(&second).unwrap()
        );
        assert_ne!(first.output_id(), second.output_id());

        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let store = FileContentStore::create(temp.path().join("objects")).unwrap();
        store.put(&first.files()[0].content, b"hello").unwrap();
        store.put(&second.files()[0].content, b"other").unwrap();
        publish_site_output_cache(&cache, &first, &input, &store).unwrap();
        let root = cache.join("manifests").canonicalize().unwrap();
        let manifest = cached_manifest_path(&root, &output_cache_key(&first).unwrap()).unwrap();
        let original = std::fs::read(&manifest).unwrap();

        let error = publish_site_output_cache(&cache, &second, &input, &store).unwrap_err();
        assert!(error
            .to_string()
            .contains("private SiteOutput cache key names different outputs"));
        assert_eq!(std::fs::read(&manifest).unwrap(), original);
    }

    #[test]
    fn publication_rejects_missing_and_corrupt_objects() {
        let input = closed("example.ig");
        let output = output(&input, b"hello");
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let store = FileContentStore::create(temp.path().join("objects")).unwrap();

        let missing = publish_site_output_cache(&cache, &output, &input, &store).unwrap_err();
        assert!(missing.to_string().contains("is absent"));
        assert!(!cache.join("manifests").exists());

        std::fs::write(
            store.object_path(&output.files()[0].content.sha256),
            b"jello",
        )
        .unwrap();
        let corrupt = publish_site_output_cache(&cache, &output, &input, &store).unwrap_err();
        assert!(corrupt.to_string().contains("digest mismatch"));
        assert!(!cache.join("manifests").exists());
    }
}
