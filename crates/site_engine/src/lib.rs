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
#[cfg(feature = "dependency-observation")]
mod dependency_observation;
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

/// Cloneable, resolver-scoped view of one immutable package source.
///
/// The source may be an in-memory browser bundle or a native disk source. The
/// allowed-label set binds every compiler/renderer read to the exact resolver
/// fixpoint captured for the project revision.
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
                "allowed_file_count",
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
            allowed_files: self.allowed_files.clone(),
            allowed_directories: self.allowed_directories.clone(),
        }
    }

    pub(crate) fn restricted_to_files(&self, files: BTreeSet<PathBuf>) -> Result<Self, String> {
        let labels = self.allowed_labels.as_ref().ok_or_else(|| {
            "restricted package view must already have an exact label scope".to_string()
        })?;
        for file in &files {
            let relative = file.strip_prefix(&self.root).map_err(|_| {
                format!(
                    "restricted package file {} is outside the package root",
                    file.display()
                )
            })?;
            let label = relative
                .components()
                .next()
                .and_then(|component| component.as_os_str().to_str())
                .ok_or_else(|| {
                    format!("restricted package file {} has no label", file.display())
                })?;
            if !labels.contains(label) {
                return Err(format!(
                    "restricted package file {} is outside the exact label scope",
                    file.display()
                ));
            }
        }
        let mut directories = BTreeSet::from([self.root.clone()]);
        for file in &files {
            let mut parent = file.parent();
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
            allowed_files: Some(files),
            allowed_directories: Some(directories),
        })
    }

    pub fn is_scoped_to(&self, labels: &[String]) -> bool {
        let expected = labels.iter().cloned().collect::<BTreeSet<_>>();
        expected.len() == labels.len() && self.allowed_labels.as_ref() == Some(&expected)
    }

    fn permits(&self, path: &Path) -> bool {
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
        let Some(files) = &self.allowed_files else {
            return true;
        };
        files.contains(path)
            || self
                .allowed_directories
                .as_ref()
                .is_some_and(|directories| directories.contains(path))
    }
}

impl PackageSource for PackageView {
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        if !self.permits(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "package is outside the compile resolver fixpoint",
            ));
        }
        self.source.read(path)
    }

    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<DirEntry>> {
        if !self.permits(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "package is outside the compile resolver fixpoint",
            ));
        }
        let mut entries = self.source.read_dir(path)?;
        if path == self.root {
            if let Some(allowed) = &self.allowed_labels {
                entries.retain(|entry| allowed.contains(&entry.file_name));
            }
        } else if self.allowed_files.is_some() {
            entries.retain(|entry| self.permits(&path.join(&entry.file_name)));
        }
        Ok(entries)
    }

    fn exists(&self, path: &Path) -> bool {
        self.permits(path) && self.source.exists(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.permits(path) && self.source.is_dir(path)
    }

    fn fork_read_cache(&self) -> std::io::Result<Box<dyn PackageSource>> {
        Ok(Box::new(self.fork_for_compile()?))
    }
}
