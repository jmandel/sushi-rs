//! Exact manifest cache for fully materialized [`crate::SiteOutput`] values.
//!
//! Output bytes remain in the shared [`content_store::ContentStore`]. This cache
//! stores only the canonical SiteOutput manifest under its pre-render
//! [`crate::OutputCacheKey`]. A hit is accepted only after the manifest, closed
//! input identity, and every referenced output byte have all been verified.

#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use content_store::ContentStore;
use thiserror::Error;

use crate::{ClosedSiteBuild, OutputCacheKey, SiteOutput, SiteOutputError, SiteOutputId};

/// Storage-independent lookup/publication contract. Browser hosts can implement
/// the same pointer-last behavior over OPFS; native callers use
/// [`FileSiteOutputCache`].
pub trait SiteOutputCache {
    fn load(
        &self,
        key: &OutputCacheKey,
        input: &ClosedSiteBuild,
        objects: &dyn ContentStore,
    ) -> Result<Option<SiteOutput>, SiteOutputCacheError>;

    fn publish(
        &self,
        output: &SiteOutput,
        input: &ClosedSiteBuild,
        objects: &dyn ContentStore,
    ) -> Result<(), SiteOutputCacheError>;
}

#[derive(Debug, Error)]
pub enum SiteOutputCacheError {
    #[error("site output cache root is not a real directory: {0}")]
    InvalidRoot(PathBuf),
    #[error("site output cache manifest is not a regular file: {0}")]
    InvalidManifest(PathBuf),
    #[error("cached manifest key {actual} does not match lookup key {expected}")]
    KeyMismatch {
        expected: OutputCacheKey,
        actual: OutputCacheKey,
    },
    #[error("site output cache key {key} already names different output {existing} (attempted {attempted})")]
    Collision {
        key: OutputCacheKey,
        existing: SiteOutputId,
        attempted: SiteOutputId,
    },
    #[error("site output cache I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Output(#[from] SiteOutputError),
    #[error("invalid cached SiteOutput manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot serialize canonical SiteOutput manifest: {0}")]
    Canonical(#[from] crate::CanonicalError),
}

/// Filesystem pointer store. Each file is a canonical SiteOutput manifest; its
/// name is the safe lowercase digest portion of `sok1-sha256:<digest>`.
#[derive(Clone, Debug)]
pub struct FileSiteOutputCache {
    root: PathBuf,
}

impl FileSiteOutputCache {
    pub fn create(root: impl AsRef<Path>) -> Result<Self, SiteOutputCacheError> {
        let root = root.as_ref();
        fs::create_dir_all(root).map_err(|source| SiteOutputCacheError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let metadata = fs::symlink_metadata(root).map_err(|source| SiteOutputCacheError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SiteOutputCacheError::InvalidRoot(root.to_path_buf()));
        }
        let root = root
            .canonicalize()
            .map_err(|source| SiteOutputCacheError::Io {
                path: root.to_path_buf(),
                source,
            })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest_path(&self, key: &OutputCacheKey) -> PathBuf {
        let digest = key
            .as_str()
            .strip_prefix("sok1-sha256:")
            .expect("validated OutputCacheKey has its fixed prefix");
        self.root.join(format!("{digest}.json"))
    }

    fn read_manifest(
        &self,
        key: &OutputCacheKey,
    ) -> Result<Option<SiteOutput>, SiteOutputCacheError> {
        let path = self.manifest_path(key);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(SiteOutputCacheError::Io { path, source }),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SiteOutputCacheError::InvalidManifest(path));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .open(&path)
            .map_err(|source| SiteOutputCacheError::Io {
                path: path.clone(),
                source,
            })?;
        let opened = file.metadata().map_err(|source| SiteOutputCacheError::Io {
            path: path.clone(),
            source,
        })?;
        let after = fs::symlink_metadata(&path).map_err(|source| SiteOutputCacheError::Io {
            path: path.clone(),
            source,
        })?;
        if after.file_type().is_symlink() || !opened.is_file() || !after.is_file() {
            return Err(SiteOutputCacheError::InvalidManifest(path));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.dev() != opened.dev()
                || metadata.ino() != opened.ino()
                || after.dev() != opened.dev()
                || after.ino() != opened.ino()
            {
                return Err(SiteOutputCacheError::InvalidManifest(path));
            }
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        file.read_to_end(&mut bytes)
            .map_err(|source| SiteOutputCacheError::Io {
                path: path.clone(),
                source,
            })?;
        let output: SiteOutput = serde_json::from_slice(&bytes)?;
        if output.cache_key() != key {
            return Err(SiteOutputCacheError::KeyMismatch {
                expected: key.clone(),
                actual: output.cache_key().clone(),
            });
        }
        Ok(Some(output))
    }

    fn sync_root(&self) -> Result<(), SiteOutputCacheError> {
        #[cfg(unix)]
        return File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| SiteOutputCacheError::Io {
                path: self.root.clone(),
                source,
            });
        #[cfg(not(unix))]
        Ok(())
    }
}

impl SiteOutputCache for FileSiteOutputCache {
    fn load(
        &self,
        key: &OutputCacheKey,
        input: &ClosedSiteBuild,
        objects: &dyn ContentStore,
    ) -> Result<Option<SiteOutput>, SiteOutputCacheError> {
        let Some(output) = self.read_manifest(key)? else {
            return Ok(None);
        };
        output.verify_cached(input, objects)?;
        Ok(Some(output))
    }

    fn publish(
        &self,
        output: &SiteOutput,
        input: &ClosedSiteBuild,
        objects: &dyn ContentStore,
    ) -> Result<(), SiteOutputCacheError> {
        output.verify_cached(input, objects)?;
        if let Some(existing) = self.read_manifest(output.cache_key())? {
            existing.verify_cached(input, objects)?;
            if existing.output_id() == output.output_id() {
                return Ok(());
            }
            return Err(SiteOutputCacheError::Collision {
                key: output.cache_key().clone(),
                existing: existing.output_id().clone(),
                attempted: output.output_id().clone(),
            });
        }

        let bytes = output.canonical_bytes()?;
        let destination = self.manifest_path(output.cache_key());
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root).map_err(|source| {
            SiteOutputCacheError::Io {
                path: self.root.clone(),
                source,
            }
        })?;
        use std::io::Write;
        temporary
            .write_all(&bytes)
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|source| SiteOutputCacheError::Io {
                path: temporary.path().to_path_buf(),
                source,
            })?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => self.sync_root(),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = self.read_manifest(output.cache_key())?.ok_or_else(|| {
                    SiteOutputCacheError::Io {
                        path: destination,
                        source: io::Error::new(
                            io::ErrorKind::NotFound,
                            "manifest disappeared after no-clobber collision",
                        ),
                    }
                })?;
                existing.verify_cached(input, objects)?;
                if existing.output_id() == output.output_id() {
                    Ok(())
                } else {
                    Err(SiteOutputCacheError::Collision {
                        key: output.cache_key().clone(),
                        existing: existing.output_id().clone(),
                        attempted: output.output_id().clone(),
                    })
                }
            }
            Err(error) => Err(SiteOutputCacheError::Io {
                path: destination,
                source: error.error,
            }),
        }
    }
}
