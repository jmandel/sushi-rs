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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use package_store::{DirEntry, PackageSource};

mod compilation;
mod preparation;
mod render_surface;
mod runtime;

pub use compilation::{
    CompilationDefinition, CompilationDefinitionKind, CompilationDiagnostic, CompilationOutcome,
    CompilationResource, CompilationTransition, ProjectRevision, ResolvedPackageClosure,
};
pub use preparation::{
    GeneratorSpec, PackageEnvironment, PackageMaterial, PrepareMetrics, PrepareProjectError,
    PrepareResult, PreparedProjectResult, TemplateResolution,
};
pub(crate) use render_surface::{
    build_render_semantics, build_render_state_from_semantics, RenderState, SiteOptions,
};
pub use runtime::{
    ExternalFinalizeInput, OutputCatalog, OutputDescriptor, OutputResourceSubject,
    OutputSubjectPage, PreparedOutput, RenderedOutput, SiteEngine,
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
    allowed_labels: Option<BTreeSet<String>>,
}

impl std::fmt::Debug for PackageView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageView")
            .field("root", &self.root)
            .field("allowed_labels", &self.allowed_labels)
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
            allowed_labels,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn scoped(&self, labels: impl IntoIterator<Item = String>) -> Self {
        Self {
            source: self.source.clone(),
            root: self.root.clone(),
            allowed_labels: Some(labels.into_iter().collect()),
        }
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
        allowed.contains(label)
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
        }
        Ok(entries)
    }

    fn exists(&self, path: &Path) -> bool {
        self.permits(path) && self.source.exists(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.permits(path) && self.source.is_dir(path)
    }
}
