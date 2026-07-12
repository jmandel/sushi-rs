//! `BundleSource` — a read-only, in-memory [`PackageSource`] that mounts a set of
//! prebuilt package bundles.
//!
//! This is the package-source shape the browser mounts. Cold material arrives as
//! one fetched/inflated bundle per package and uses owned in-memory files; warm
//! PreparedPackage v3 material keeps compact chunks and inflates members lazily.
//! Neither path needs `std::fs`, so both compile and run on
//! `wasm32-unknown-unknown`. Native tests exercise the source directly (the P1
//! BundleSource fixture-ladder gate).
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
//! [`BundleSource`] itself is container-agnostic: cold material uses owned
//! `path -> bytes` maps, while prepared material retains a compact raw-DEFLATE
//! chunk store plus a validated member directory. Both use the same cache paths
//! (`<cache>/<id>#<ver>/package/<file>`). [`BundleSource::mount_package`] takes a
//! package's already-inflated `file -> bytes` entries and places them under that
//! package's dir. Prepared-package chunks use the pure-Rust `miniz_oxide`
//! decoder, which is also used by the wasm32 build.

use crate::prepared::PreparedCompressedFiles;
use crate::source::{DirEntry, PackageSource};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
#[derive(Debug, Clone)]
pub struct BundleSource {
    /// Synthetic cache root the mounted packages hang under. All the
    /// `<cache>/<id>#<ver>/package/...` paths the caller builds must join this
    /// root, which is exactly what happens when the caller passes
    /// [`BundleSource::cache_root`] as the `cache_dir` to `PackageContext::new_with`
    /// / `PackageStore::for_project_with`.
    root: PathBuf,
    /// Immutable per-package mount layers. Cloning a BundleSource clones only
    /// this small label map and its `Rc`s, never previously mounted file bytes.
    layers: BTreeMap<String, Rc<BundleLayer>>,
}

/// Runtime footprint and lazy-inflate counters for all compact prepared layers.
/// Backings shared by several package layers are counted exactly once.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BundleCompressionMetrics {
    pub compressed_retained_bytes: u64,
    pub declared_raw_bytes: u64,
    pub chunks_inflated: u64,
    pub raw_inflated_bytes: u64,
    pub cache_hits: u64,
    pub cached_raw_bytes: u64,
}

#[derive(Debug)]
struct BundleLayer {
    files: LayerFiles,
    /// Directory paths introduced by those files.
    dirs: std::collections::BTreeSet<PathBuf>,
}

#[derive(Debug)]
enum LayerFiles {
    /// Cold/native package material already exists as independently owned files.
    Owned(BTreeMap<PathBuf, Vec<u8>>),
    /// Warm prepared material remains one compact immutable artifact/batch;
    /// its member directory is mounted immediately and chunks inflate lazily.
    Compressed {
        files: PreparedCompressedFiles,
        names: BTreeMap<PathBuf, String>,
    },
}

impl BundleLayer {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        match &self.files {
            LayerFiles::Owned(files) => files.get(path).cloned().ok_or_else(not_found),
            LayerFiles::Compressed { files, names } => names
                .get(path)
                .ok_or_else(not_found)
                .and_then(|name| files.read(name)),
        }
    }

    fn contains(&self, path: &Path) -> bool {
        match &self.files {
            LayerFiles::Owned(files) => files.contains_key(path),
            LayerFiles::Compressed { names, .. } => names.contains_key(path),
        }
    }

    fn paths(&self) -> Box<dyn Iterator<Item = &PathBuf> + '_> {
        match &self.files {
            LayerFiles::Owned(files) => Box::new(files.keys()),
            LayerFiles::Compressed { names, .. } => Box::new(names.keys()),
        }
    }
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no such file in bundle")
}

impl BundleSource {
    /// Create an empty bundle source rooted at a synthetic in-memory cache dir.
    /// The root is a stable virtual path (never touched on disk) that callers pass
    /// as the `cache_dir`. Get it back via [`BundleSource::cache_root`].
    pub fn new() -> Self {
        let root = PathBuf::from("/__bundle_cache__");
        Self {
            root,
            layers: BTreeMap::new(),
        }
    }

    /// The synthetic cache root to pass as `cache_dir` to the store/context
    /// constructors. Package dirs live at `<root>/<id>#<ver>/package`.
    pub fn cache_root(&self) -> &Path {
        &self.root
    }

    /// Snapshot lazy prepared-package storage counters without reading a body.
    pub fn compression_metrics(&self) -> BundleCompressionMetrics {
        let mut result = BundleCompressionMetrics::default();
        let mut backings = std::collections::BTreeSet::new();
        for layer in self.layers.values() {
            let LayerFiles::Compressed { files, .. } = &layer.files else {
                continue;
            };
            result.declared_raw_bytes = result
                .declared_raw_bytes
                .saturating_add(files.declared_raw_bytes());
            if backings.insert(files.backing_identity()) {
                let metrics = files.backing_metrics();
                result.compressed_retained_bytes = result
                    .compressed_retained_bytes
                    .saturating_add(metrics.compressed_retained_bytes);
                result.chunks_inflated = result
                    .chunks_inflated
                    .saturating_add(metrics.chunks_inflated);
                result.raw_inflated_bytes = result
                    .raw_inflated_bytes
                    .saturating_add(metrics.raw_inflated_bytes);
                result.cache_hits = result.cache_hits.saturating_add(metrics.cache_hits);
                result.cached_raw_bytes = result
                    .cached_raw_bytes
                    .saturating_add(metrics.cached_raw_bytes);
            }
        }
        result
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
        let mut files = BTreeMap::new();
        let mut dirs = std::collections::BTreeSet::new();
        Self::add_dir(&self.root, &mut dirs, &package_dir);
        for (name, bytes) in entries {
            let path = package_dir.join(name.as_ref());
            if let Some(parent) = path.parent() {
                Self::add_dir(&self.root, &mut dirs, parent);
            }
            files.insert(path, bytes);
        }
        self.layers.insert(
            label.to_string(),
            Rc::new(BundleLayer {
                files: LayerFiles::Owned(files),
                dirs,
            }),
        );
    }

    /// Mount a validated compact package directory. No compressed chunk is
    /// inflated until `PackageSource::read` asks for one of its members.
    pub(crate) fn mount_prepared_compressed(
        &mut self,
        label: &str,
        files: PreparedCompressedFiles,
    ) {
        let package_dir = self.root.join(label).join("package");
        let mut names = BTreeMap::new();
        let mut dirs = std::collections::BTreeSet::new();
        Self::add_dir(&self.root, &mut dirs, &package_dir);
        for name in files.names() {
            let path = package_dir.join(name);
            if let Some(parent) = path.parent() {
                Self::add_dir(&self.root, &mut dirs, parent);
            }
            names.insert(path, name.clone());
        }
        self.layers.insert(
            label.to_string(),
            Rc::new(BundleLayer {
                files: LayerFiles::Compressed { files, names },
                dirs,
            }),
        );
    }

    /// Record `dir` and all its ancestors up to (and including) the root as
    /// existing directories.
    fn add_dir(root: &Path, dirs: &mut std::collections::BTreeSet<PathBuf>, dir: &Path) {
        let mut cur = Some(dir);
        while let Some(d) = cur {
            dirs.insert(d.to_path_buf());
            if d == root {
                break;
            }
            cur = d.parent();
        }
    }

    fn layer_for_path(&self, path: &Path) -> Option<&BundleLayer> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let label = relative.components().next()?.as_os_str().to_str()?;
        self.layers.get(label).map(Rc::as_ref)
    }
}

impl PackageSource for BundleSource {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.layer_for_path(path).ok_or_else(not_found)?.read(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        if !self.is_dir(path) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no such directory in bundle",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        // Direct file children.
        for layer in self.layers.values() {
            for fpath in layer.paths() {
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
        }
        // Direct subdirectory children.
        for layer in self.layers.values() {
            for dpath in &layer.dirs {
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
        }
        Ok(out)
    }

    fn exists(&self, path: &Path) -> bool {
        self.is_dir(path)
            || self
                .layer_for_path(path)
                .is_some_and(|layer| layer.contains(path))
    }

    fn is_dir(&self, path: &Path) -> bool {
        path == self.root
            || self
                .layer_for_path(path)
                .is_some_and(|layer| layer.dirs.contains(path))
    }

    // write_new: default (read-only) — the sidecar write-once fails soft, and the
    // bundle already ships `.derived-index.json`, so `derived_index::load` reads it
    // straight from the mounted files and never needs to write.
}

impl Default for BundleSource {
    fn default() -> Self {
        Self::new()
    }
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
        assert_eq!(
            names,
            vec![".index.json", "StructureDefinition-Patient.json"]
        );
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
        assert!(src
            .write_new(&pkg.join(".derived-index.json"), b"{}")
            .is_err());
    }

    #[test]
    fn clones_share_immutable_layers_and_diverge_by_appending() {
        let mut original = BundleSource::new();
        original.mount_package("a#1", [("a.txt", b"a".to_vec())]);
        let mut transaction = original.clone();
        assert!(Rc::ptr_eq(
            &original.layers["a#1"],
            &transaction.layers["a#1"]
        ));

        transaction.mount_package("b#1", [("b.txt", b"b".to_vec())]);
        assert_eq!(original.layers.len(), 1);
        assert_eq!(transaction.layers.len(), 2);
        assert!(original
            .read(&original.cache_root().join("b#1/package/b.txt"))
            .is_err());
        assert_eq!(
            transaction
                .read(&transaction.cache_root().join("a#1/package/a.txt"))
                .unwrap(),
            b"a"
        );
    }

    #[test]
    fn prepared_layer_retains_compressed_directory_and_reads_lazily() {
        let prepared = crate::PreparedPackage::prepare(
            "p#1",
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"p","version":"1"}"#.to_vec(),
                ),
                ("first.txt".into(), b"first".to_vec()),
                ("second.txt".into(), b"second".to_vec()),
            ]),
        )
        .unwrap();
        let bytes = prepared.encode();
        let decoded = crate::PreparedPackage::decode_expected(&bytes, &prepared.key).unwrap();
        let mut source = BundleSource::new();
        decoded.mount_into(&mut source);
        let layer = &source.layers["p#1"];
        match &layer.files {
            LayerFiles::Compressed { names, .. } => assert_eq!(names.len(), 4),
            LayerFiles::Owned(_) => panic!("prepared mount unexpectedly materialized files"),
        }
        assert_eq!(
            source
                .read(&source.cache_root().join("p#1/package/second.txt"))
                .unwrap(),
            b"second"
        );
    }
}
