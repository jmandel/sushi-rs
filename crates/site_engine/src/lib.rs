//! Canonical target-neutral site execution.
//!
//! This crate owns the execution path shared by native and WASM transports.
//! Hosts supply captured project/package bytes and call the four operations;
//! they do not implement Publisher assembly, rendering, or handle retention.
//!
//! [`SiteEngine`] owns semantic compilation, renderer-neutral guide preparation,
//! target projection, Publisher assembly, runtime retention, and finalization.
//! WASM and native hosts provide only explicit project/package environments and
//! transport the typed results; no host-side preparation facade exists.
//!
//! `ClosedSiteBuild` addresses every external renderer input.
//! [`SiteEngine::restore`] authenticates its complete `ContentStore` closure and
//! admits an ordinary bounded Publisher or Cycle handle in a fresh process.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use package_store::{DirEntry, PackageSource};

mod compilation;
mod events;
mod preparation;
mod render_surface;
mod runtime;

pub use compilation::{
    CompilationDefinition, CompilationDefinitionKind, CompilationDiagnostic,
    CompilationDiagnosticSeverity, CompilationOutcome, CompilationResource, ProjectRevision,
    ResolvedPackageClosure,
};
pub use events::{
    BuildError, BuildErrorCode, BuildErrorPhase, BuildEvent, BuildEventSource, BuildOperation,
    BuildStage,
};
pub use preparation::{
    GeneratorKind, GeneratorSpec, PackageEnvironment, PackageProvider, PrepareResult,
    PreparedProjectResult, ProjectSource, TemplateResolution,
};
pub(crate) use render_surface::{
    build_render_semantics, build_render_state_from_semantics, RenderState, SiteOptions,
};
pub use runtime::{
    OutputCatalog, OutputDescriptor, OutputKind, OutputPageKind, OutputResourceSubject,
    OutputSubjectPage, SiteEngine,
};

/// The engine's only retention primitive. The three owners are semantic
/// compilation, prepared target derivations, and installed runtimes. Reads do
/// not affect recency; only a successful transaction may promote or touch an
/// entry.
pub(crate) struct History2<T> {
    pub(crate) current: Option<T>,
    pub(crate) previous: Option<T>,
}

impl<T> Default for History2<T> {
    fn default() -> Self {
        Self {
            current: None,
            previous: None,
        }
    }
}

impl<T> History2<T> {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.current.iter().chain(self.previous.iter())
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.current.iter_mut().chain(self.previous.iter_mut())
    }

    pub(crate) fn promote(&mut self, value: T) {
        self.previous = self.current.replace(value);
    }

    pub(crate) fn len(&self) -> usize {
        usize::from(self.current.is_some()) + usize::from(self.previous.is_some())
    }
}

/// Cloneable, resolver-scoped view of one package source.
///
/// Production compilation and rendering use captured immutable package
/// carriers, which can also supply member identities. Native callers may use a
/// disk source, but that source cannot attest immutability and therefore cannot
/// participate in retained read-cache proofs. The allowed-label set binds every
/// compiler/renderer read to the exact resolver fixpoint captured for the
/// project revision.
#[derive(Clone)]
pub struct PackageView {
    source: Rc<dyn PackageSource>,
    root: PathBuf,
    /// Exact authenticated carriers mounted in the source which produced this
    /// view. This is identity evidence only: `allowed_labels` remains the sole
    /// authority for package visibility, so resolution-support packages cannot
    /// become compiler inputs merely because their carriers are recorded here.
    carrier_identities: Option<Rc<BTreeMap<String, site_build::ContentRef>>>,
    allowed_labels: Option<BTreeSet<String>>,
    direct_file_directories: Option<BTreeSet<PathBuf>>,
    allowed_files: Option<BTreeSet<PathBuf>>,
    allowed_directories: Option<BTreeSet<PathBuf>>,
}

impl std::fmt::Debug for PackageView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageView")
            .field("root", &self.root)
            .field(
                "carrier_identity_count",
                &self
                    .carrier_identities
                    .as_ref()
                    .map(|entries| entries.len()),
            )
            .field("allowed_labels", &self.allowed_labels)
            .field(
                "direct_file_directory_count",
                &self.direct_file_directories.as_ref().map(BTreeSet::len),
            )
            .field(
                "additional_allowed_file_count",
                &self.allowed_files.as_ref().map(BTreeSet::len),
            )
            .finish_non_exhaustive()
    }
}

impl PackageView {
    pub fn new(
        source: Rc<dyn PackageSource>,
        root: PathBuf,
        allowed_labels: Option<BTreeSet<String>>,
    ) -> Self {
        Self {
            source,
            root,
            carrier_identities: None,
            allowed_labels,
            direct_file_directories: None,
            allowed_files: None,
            allowed_directories: None,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn with_carrier_identities(
        mut self,
        identities: BTreeMap<String, site_build::ContentRef>,
    ) -> Self {
        self.carrier_identities = Some(Rc::new(identities));
        self
    }

    pub(crate) fn carrier_identity(&self, label: &str) -> Option<&site_build::ContentRef> {
        self.carrier_identities.as_ref()?.get(label)
    }

    pub(crate) fn fork_for_compile(&self) -> std::io::Result<Self> {
        let source: Rc<dyn PackageSource> = Rc::from(self.source.fork_read_cache()?);
        Ok(Self {
            source,
            root: self.root.clone(),
            carrier_identities: self.carrier_identities.clone(),
            allowed_labels: self.allowed_labels.clone(),
            direct_file_directories: self.direct_file_directories.clone(),
            allowed_files: self.allowed_files.clone(),
            allowed_directories: self.allowed_directories.clone(),
        })
    }

    pub(crate) fn scoped(&self, labels: impl IntoIterator<Item = String>) -> Self {
        Self {
            source: self.source.clone(),
            root: self.root.clone(),
            carrier_identities: self.carrier_identities.clone(),
            allowed_labels: Some(labels.into_iter().collect()),
            direct_file_directories: self.direct_file_directories.clone(),
            allowed_files: self.allowed_files.clone(),
            allowed_directories: self.allowed_directories.clone(),
        }
    }

    /// Project an already label-scoped captured view to direct files under the
    /// supplied directories plus a small set of exact additional files.
    ///
    /// Callers construct this once from an unrestricted immutable package
    /// carrier. Reapplying it or applying it to an ambient mutable source would
    /// make the requested shape depend on state outside the captured build.
    pub(crate) fn restricted_to_direct_files(
        &self,
        direct_file_directories: BTreeSet<PathBuf>,
        additional_files: BTreeSet<PathBuf>,
    ) -> Result<Self, String> {
        let labels = self.allowed_labels.as_ref().ok_or_else(|| {
            "restricted package view must already have an exact label scope".to_string()
        })?;
        let validate_path = |path: &Path, kind: &str| -> Result<(), String> {
            let relative = path.strip_prefix(&self.root).map_err(|_| {
                format!(
                    "restricted package {kind} {} is outside the package root",
                    path.display()
                )
            })?;
            let label = relative
                .components()
                .next()
                .and_then(|component| component.as_os_str().to_str())
                .ok_or_else(|| {
                    format!("restricted package {kind} {} has no label", path.display())
                })?;
            if !labels.contains(label) {
                return Err(format!(
                    "restricted package {kind} {} is outside the exact label scope",
                    path.display()
                ));
            }
            Ok(())
        };
        for directory in &direct_file_directories {
            validate_path(directory, "directory")?;
            if !self.is_dir(directory) {
                return Err(format!(
                    "restricted package directory {} does not exist",
                    directory.display()
                ));
            }
        }
        for file in &additional_files {
            validate_path(file, "file")?;
            if !self.source.is_file(file) {
                return Err(format!(
                    "restricted package file {} is not a regular file",
                    file.display()
                ));
            }
        }
        let mut directories = BTreeSet::from([self.root.clone()]);
        for path in direct_file_directories.iter().chain(&additional_files) {
            let mut parent = if direct_file_directories.contains(path) {
                Some(path.as_path())
            } else {
                path.parent()
            };
            while let Some(path) = parent {
                if !path.starts_with(&self.root) {
                    break;
                }
                directories.insert(path.to_path_buf());
                if path == self.root {
                    break;
                }
                parent = path.parent();
            }
        }
        Ok(Self {
            source: self.source.clone(),
            root: self.root.clone(),
            carrier_identities: self.carrier_identities.clone(),
            allowed_labels: self.allowed_labels.clone(),
            direct_file_directories: Some(direct_file_directories),
            allowed_files: Some(additional_files),
            allowed_directories: Some(directories),
        })
    }

    pub fn is_scoped_to(&self, labels: &[String]) -> bool {
        let expected = labels.iter().cloned().collect::<BTreeSet<_>>();
        expected.len() == labels.len() && self.allowed_labels.as_ref() == Some(&expected)
    }

    fn permits_label(&self, path: &Path) -> bool {
        let Some(allowed) = &self.allowed_labels else {
            return true;
        };
        if path == self.root {
            return true;
        }
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        let Some(label) = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
        else {
            return false;
        };
        if !allowed.contains(label) {
            return false;
        }
        true
    }

    fn permits_file(&self, path: &Path) -> bool {
        if !self.permits_label(path) {
            return false;
        }
        let Some(directories) = &self.direct_file_directories else {
            return true;
        };
        path.parent()
            .is_some_and(|parent| directories.contains(parent))
            || self
                .allowed_files
                .as_ref()
                .is_some_and(|files| files.contains(path))
    }

    fn permits_directory(&self, path: &Path) -> bool {
        self.permits_label(path)
            && self
                .allowed_directories
                .as_ref()
                .is_none_or(|directories| directories.contains(path))
    }
}

impl PackageSource for PackageView {
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        if !self.permits_file(path)
            || (self.direct_file_directories.is_some() && !self.source.is_file(path))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "package is outside the compile resolver fixpoint",
            ));
        }
        self.source.read(path)
    }

    fn immutable_content_identity(&self, path: &Path) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};

        if !self.permits_file(path) || !self.source.is_file(path) {
            return None;
        }
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut components = relative.components();
        let label = components.next()?.as_os_str().to_str()?;
        let carrier = self.carrier_identity(label)?;
        let relative = relative.to_str()?;
        let mut hasher = Sha256::new();
        hasher.update(b"site-engine-package-member-v1\0");
        hasher.update(carrier.sha256.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(relative.as_bytes());
        Some(hasher.finalize().into())
    }

    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<DirEntry>> {
        if !self.permits_directory(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "package is outside the compile resolver fixpoint",
            ));
        }
        let mut entries = self.source.read_dir(path)?;
        entries.retain(|entry| {
            let child = path.join(&entry.file_name);
            if entry.is_file {
                self.permits_file(&child)
            } else {
                self.permits_directory(&child)
            }
        });
        Ok(entries)
    }

    fn exists(&self, path: &Path) -> bool {
        if self.direct_file_directories.is_none() {
            self.permits_label(path) && self.source.exists(path)
        } else {
            (self.permits_file(path) && self.source.is_file(path))
                || (self.permits_directory(path) && self.source.is_dir(path))
        }
    }

    fn is_file(&self, path: &Path) -> bool {
        self.permits_file(path) && self.source.is_file(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.permits_directory(path) && self.source.is_dir(path)
    }

    fn fork_read_cache(&self) -> std::io::Result<Box<dyn PackageSource>> {
        Ok(Box::new(self.fork_for_compile()?))
    }
}

#[cfg(test)]
mod package_view_identity_tests {
    use super::*;

    fn view(carrier_bytes: &[u8], allowed: BTreeSet<String>) -> PackageView {
        let mut source = package_store::BundleSource::new();
        source.mount_package(
            "p#1",
            [
                ("member.json", br#"{"value":1}"#.to_vec()),
                ("nested/child.json", br#"{"value":2}"#.to_vec()),
            ],
        );
        let root = source.cache_root().to_path_buf();
        PackageView::new(Rc::new(source), root, Some(allowed)).with_carrier_identities(
            BTreeMap::from([(
                "p#1".into(),
                site_build::ContentRef::of_bytes(
                    carrier_bytes,
                    Some("application/vnd.fhir.package.prepared.v3"),
                ),
            )]),
        )
    }

    #[test]
    fn member_identity_binds_exact_carrier_path_and_scope() {
        let first = view(b"carrier-a", BTreeSet::from(["p#1".into()]));
        let same = view(b"carrier-a", BTreeSet::from(["p#1".into()]));
        let changed = view(b"carrier-b", BTreeSet::from(["p#1".into()]));
        let member = first.root().join("p#1/package/member.json");
        let sibling = first.root().join("p#1/package/other.json");
        assert_eq!(
            first.immutable_content_identity(&member),
            same.immutable_content_identity(&member)
        );
        assert_ne!(
            first.immutable_content_identity(&member),
            changed.immutable_content_identity(&member)
        );
        assert_ne!(
            first.immutable_content_identity(&member),
            first.immutable_content_identity(&sibling)
        );

        let denied = view(b"carrier-a", BTreeSet::new());
        assert_eq!(denied.immutable_content_identity(&member), None);
    }

    #[test]
    fn direct_file_surface_attests_only_existing_visible_files() {
        let unrestricted = view(b"carrier-a", BTreeSet::from(["p#1".into()]));
        let package = unrestricted.root().join("p#1/package");
        let restricted = unrestricted
            .restricted_to_direct_files(BTreeSet::from([package.clone()]), BTreeSet::new())
            .unwrap();
        assert!(restricted
            .immutable_content_identity(&package.join("member.json"))
            .is_some());
        assert_eq!(
            restricted.immutable_content_identity(&package.join("missing.json")),
            None
        );
        assert_eq!(
            restricted.immutable_content_identity(&package.join("nested")),
            None
        );
        assert_eq!(
            restricted.immutable_content_identity(&package.join("nested/child.json")),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_file_surface_excludes_disk_symlinks_like_read_dir() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("cache");
        let package = root.join("p#1/package");
        std::fs::create_dir_all(package.join("nested")).unwrap();
        std::fs::write(package.join("member.json"), br#"{"value":1}"#).unwrap();
        std::fs::write(package.join("nested/child.json"), br#"{"value":2}"#).unwrap();
        symlink(package.join("member.json"), package.join("file-link.json")).unwrap();
        symlink(package.join("nested"), package.join("directory-link")).unwrap();

        let unrestricted = PackageView::new(
            Rc::new(package_store::DiskSource::new()),
            root,
            Some(BTreeSet::from(["p#1".into()])),
        )
        .with_carrier_identities(BTreeMap::from([(
            "p#1".into(),
            site_build::ContentRef::of_bytes(
                b"carrier-a",
                Some("application/vnd.fhir.package.prepared.v3"),
            ),
        )]));
        let restricted = unrestricted
            .restricted_to_direct_files(BTreeSet::from([package.clone()]), BTreeSet::new())
            .unwrap();

        assert!(restricted.read(&package.join("member.json")).is_ok());
        assert!(restricted.exists(&package.join("member.json")));
        assert!(restricted.is_file(&package.join("member.json")));
        for link in ["file-link.json", "directory-link"] {
            let path = package.join(link);
            assert!(restricted.read(&path).is_err());
            assert!(!restricted.exists(&path));
            assert!(!restricted.is_file(&path));
            assert!(!restricted.is_dir(&path));
            assert_eq!(restricted.immutable_content_identity(&path), None);
        }
        assert_eq!(
            restricted
                .read_dir(&package)
                .unwrap()
                .into_iter()
                .map(|entry| (entry.file_name, entry.is_file))
                .collect::<Vec<_>>(),
            vec![("member.json".into(), true)]
        );
    }
}
