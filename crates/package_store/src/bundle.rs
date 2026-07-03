//! `BundleSource` — a read-only, in-memory [`PackageSource`] that mounts a set of
//! prebuilt package bundles.
//!
//! This is the storage shape the browser mounts: cold-start is one `fetch` per
//! package bundle + one inflate, after which every read is an in-memory map
//! lookup — no `std::fs`, so it compiles and runs on `wasm32-unknown-unknown`.
//! Native tests exercise it directly (the P1 BundleSource fixture-ladder gate).
//!
//! # Bundle format (v1)
//!
//! A *package bundle* is the byte content of one materialized `package/`
//! directory: every resource JSON plus the stock `.index.json` and the derived
//! `.derived-index.json` sidecar. The on-the-wire container is a **gzipped tar**
//! whose entries are the package-relative file names (`StructureDefinition-*.json`,
//! `.index.json`, `.derived-index.json`, `package.json`, …) — i.e. exactly what
//! the read path lists and reads. The builder (`package_acquisition`) emits one
//! such blob per package and a [`BundleManifest`] lockfile pinning the set.
//!
//! [`BundleSource`] itself is container-agnostic: it holds a `path -> bytes` map
//! keyed on the same cache paths the code threads around
//! (`<cache>/<id>#<ver>/package/<file>`). [`BundleSource::mount_package`] takes a
//! package's already-inflated `file -> bytes` entries and places them under that
//! package's dir; the tar/gzip inflation lives in the builder crate so this crate
//! keeps zero compression deps and stays trivially wasm-clean.

use crate::source::{DirEntry, PackageSource};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// The bundle format version. Bump on any incompatible change to the container or
/// manifest shape so a reader can reject a stale bundle.
pub const BUNDLE_FORMAT_VERSION: u32 = 1;

/// One entry in a [`BundleManifest`]: a pinned package and the bundle blob that
/// carries it. `bundle` is the name/URL of the blob the browser fetches (the
/// builder writes `<id>#<ver>.tgz`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleManifestEntry {
    /// Package id (`hl7.fhir.r4.core`).
    pub id: String,
    /// Exact pinned version (`4.0.1`).
    pub version: String,
    /// The bundle blob's name (relative to the manifest), e.g.
    /// `hl7.fhir.r4.core#4.0.1.tgz`.
    pub bundle: String,
    /// SHA-256 (hex) of the bundle blob, so a reader can verify integrity.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sha256: Option<String>,
}

/// The editor's "lockfile": the pinned set of package bundles a project needs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleManifest {
    #[serde(rename = "bundle-format-version")]
    pub format_version: u32,
    pub packages: Vec<BundleManifestEntry>,
}

impl BundleManifest {
    /// A fresh manifest at the current [`BUNDLE_FORMAT_VERSION`].
    pub fn new() -> Self {
        Self {
            format_version: BUNDLE_FORMAT_VERSION,
            packages: Vec::new(),
        }
    }

    /// Parse a manifest, rejecting a version mismatch (treated as absent so the
    /// caller re-fetches / rebuilds).
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let m: BundleManifest = serde_json::from_slice(bytes).ok()?;
        if m.format_version != BUNDLE_FORMAT_VERSION {
            return None;
        }
        Some(m)
    }

    /// Serialize to pretty JSON bytes (the manifest is small and human-inspected).
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("BundleManifest serializes")
    }
}

impl Default for BundleManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// A read-only in-memory [`PackageSource`]. Holds every mounted file's bytes keyed
/// by its full path under a synthetic cache root, plus the set of directories that
/// exist (so `is_dir`/`read_dir` answer faithfully without a real FS).
#[derive(Debug, Default)]
pub struct BundleSource {
    /// Synthetic cache root the mounted packages hang under. All the
    /// `<cache>/<id>#<ver>/package/...` paths the caller builds must join this
    /// root, which is exactly what happens when the caller passes
    /// [`BundleSource::cache_root`] as the `cache_dir` to `PackageContext::new_with`
    /// / `PackageStore::for_project_with`.
    root: PathBuf,
    /// path -> file bytes.
    files: BTreeMap<PathBuf, Vec<u8>>,
    /// The set of directory paths that exist (every ancestor of every file).
    dirs: std::collections::BTreeSet<PathBuf>,
}

impl BundleSource {
    /// Create an empty bundle source rooted at a synthetic in-memory cache dir.
    /// The root is a stable virtual path (never touched on disk) that callers pass
    /// as the `cache_dir`. Get it back via [`BundleSource::cache_root`].
    pub fn new() -> Self {
        let root = PathBuf::from("/__bundle_cache__");
        let mut dirs = std::collections::BTreeSet::new();
        dirs.insert(root.clone());
        Self {
            root,
            files: BTreeMap::new(),
            dirs,
        }
    }

    /// The synthetic cache root to pass as `cache_dir` to the store/context
    /// constructors. Package dirs live at `<root>/<id>#<ver>/package`.
    pub fn cache_root(&self) -> &Path {
        &self.root
    }

    /// Mount one package's already-inflated `file_name -> bytes` entries under
    /// `<root>/<id>#<ver>/package/`. `label` is the `<id>#<ver>` directory name.
    /// The entries are the package-relative file names from the bundle (resource
    /// JSONs, `.index.json`, `.derived-index.json`, `package.json`).
    pub fn mount_package<I, S>(&mut self, label: &str, entries: I)
    where
        I: IntoIterator<Item = (S, Vec<u8>)>,
        S: AsRef<str>,
    {
        let package_dir = self.root.join(label).join("package");
        self.add_dir(&package_dir);
        for (name, bytes) in entries {
            let path = package_dir.join(name.as_ref());
            if let Some(parent) = path.parent() {
                self.add_dir(parent);
            }
            self.files.insert(path, bytes);
        }
    }

    /// Record `dir` and all its ancestors up to (and including) the root as
    /// existing directories.
    fn add_dir(&mut self, dir: &Path) {
        let mut cur = Some(dir);
        while let Some(d) = cur {
            self.dirs.insert(d.to_path_buf());
            if d == self.root {
                break;
            }
            cur = d.parent();
        }
    }
}

impl PackageSource for BundleSource {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file in bundle"))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        if !self.dirs.contains(path) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no such directory in bundle",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        // Direct file children.
        for fpath in self.files.keys() {
            if fpath.parent() != Some(path) {
                continue;
            }
            if let Some(name) = fpath.file_name().and_then(|n| n.to_str()) {
                if seen.insert(name.to_string()) {
                    out.push(DirEntry {
                        file_name: name.to_string(),
                        is_file: true,
                    });
                }
            }
        }
        // Direct subdirectory children.
        for dpath in &self.dirs {
            if dpath.parent() == Some(path) {
                if let Some(name) = dpath.file_name().and_then(|n| n.to_str()) {
                    if seen.insert(name.to_string()) {
                        out.push(DirEntry {
                            file_name: name.to_string(),
                            is_file: false,
                        });
                    }
                }
            }
        }
        Ok(out)
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path) || self.dirs.contains(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.dirs.contains(path)
    }

    // write_new: default (read-only) — the sidecar write-once fails soft, and the
    // bundle already ships `.derived-index.json`, so `derived_index::load` reads it
    // straight from the mounted files and never needs to write.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrips_and_rejects_wrong_version() {
        let mut m = BundleManifest::new();
        m.packages.push(BundleManifestEntry {
            id: "hl7.fhir.r4.core".into(),
            version: "4.0.1".into(),
            bundle: "hl7.fhir.r4.core#4.0.1.tgz".into(),
            sha256: Some("deadbeef".into()),
        });
        let bytes = m.to_bytes();
        assert_eq!(BundleManifest::parse(&bytes), Some(m));
        let bad = br#"{"bundle-format-version":999,"packages":[]}"#;
        assert!(BundleManifest::parse(bad).is_none());
    }

    #[test]
    fn mounts_and_serves_a_package_dir() {
        let mut src = BundleSource::new();
        src.mount_package(
            "hl7.fhir.r4.core#4.0.1",
            vec![
                (".index.json", br#"{"files":[]}"#.to_vec()),
                (
                    "StructureDefinition-Patient.json",
                    br#"{"resourceType":"StructureDefinition","id":"Patient"}"#.to_vec(),
                ),
            ],
        );
        let pkg = src
            .cache_root()
            .join("hl7.fhir.r4.core#4.0.1")
            .join("package");
        assert!(src.is_dir(&pkg));
        assert!(src.is_dir(src.cache_root()));
        assert!(src.exists(&pkg.join(".index.json")));
        assert_eq!(
            src.read(&pkg.join("StructureDefinition-Patient.json"))
                .unwrap(),
            br#"{"resourceType":"StructureDefinition","id":"Patient"}"#
        );
        let mut names: Vec<String> = src
            .read_dir(&pkg)
            .unwrap()
            .into_iter()
            .map(|e| e.file_name)
            .collect();
        names.sort();
        assert_eq!(names, vec![".index.json", "StructureDefinition-Patient.json"]);
        // The package dir shows up as a subdir of its `<id>#<ver>` parent.
        let ver_dir = src.cache_root().join("hl7.fhir.r4.core#4.0.1");
        let sub: Vec<String> = src
            .read_dir(&ver_dir)
            .unwrap()
            .into_iter()
            .map(|e| e.file_name)
            .collect();
        assert_eq!(sub, vec!["package"]);
        // read_dir of the root lists the version dir.
        let roots: Vec<String> = src
            .read_dir(src.cache_root())
            .unwrap()
            .into_iter()
            .map(|e| e.file_name)
            .collect();
        assert_eq!(roots, vec!["hl7.fhir.r4.core#4.0.1"]);
        // A read-only source: write_new is unsupported (fail-soft for sidecars).
        assert!(src.write_new(&pkg.join(".derived-index.json"), b"{}").is_err());
    }
}
