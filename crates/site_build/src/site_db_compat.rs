//! Narrow compatibility projection for the existing Cycle-oriented `SiteDb` row
//! model. This module is feature-gated so neither SQLite nor the row schema enters
//! the core SiteBuild contract.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{
    canonical_json_bytes, ArtifactCatalog, ArtifactKey, ArtifactProvenance, ArtifactRecord,
    ArtifactState, BuildDiagnostic, CanonicalError, ClosedSiteBuild, ContentRef, ContractError,
    PackageLock, ProducerRef, ProjectRevision, RenderMode, RenderPlan, RenderTarget, SealError,
    SiteBuild, SiteBuildError,
};

pub const FORMAT: &str = "legacy-cycle-site-db-rows/v1";

/// A ready artifact and the bytes its content reference addresses. Callers put
/// `bytes` in their CAS and add `record` to an [`crate::ArtifactCatalog`].
#[derive(Clone, Debug)]
pub struct SiteDbProjection {
    pub record: ArtifactRecord,
    pub bytes: Vec<u8>,
}

/// Exact identity and target data needed to turn the compatibility rows into a
/// callback-free external-builder handoff. Hosts are responsible for deriving
/// the project and package values from the same compilation that produced
/// `db`; this helper deliberately cannot invent either identity from a database.
#[derive(Clone, Debug)]
pub struct CloseProjectionInput {
    pub project: ProjectRevision,
    pub package_lock: PackageLock,
    pub render_target: RenderTarget,
    pub diagnostics: BTreeSet<BuildDiagnostic>,
}

/// A closed manifest plus the one CAS object its render plan requires.
#[derive(Clone, Debug)]
pub struct ClosedSiteDbProjection {
    pub site_build: ClosedSiteBuild,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum CloseProjectionError {
    #[error("site.db compatibility projections are only valid for an external builder target")]
    NotExternalBuilder,
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Catalog(#[from] ContractError),
    #[error(transparent)]
    Build(#[from] SiteBuildError),
    #[error(transparent)]
    Seal(#[from] SealError),
}

/// Canonically serialize all existing rows as one explicitly legacy data
/// artifact. Keeping this aggregate prevents accidental claims that individual
/// site.db rows are the universal semantic model.
pub fn project(db: &site_db::SiteDb) -> Result<SiteDbProjection, CanonicalError> {
    let bytes = canonical_json_bytes(db)?;
    let content = ContentRef::of_bytes(&bytes, Some("application/json"));
    let record = ArtifactRecord {
        key: ArtifactKey::Data {
            namespace: "compat.site_db".into(),
            name: "rows.json".into(),
        },
        state: ArtifactState::Ready { content },
        provenance: ArtifactProvenance {
            producer: ProducerRef::new("site_db.compat_projection", env!("CARGO_PKG_VERSION")),
            recipe: FORMAT.into(),
            attributes: BTreeMap::from([("format".into(), FORMAT.into())]),
        },
        reads: BTreeSet::new(),
    };
    Ok(SiteDbProjection { record, bytes })
}

/// Project rows, attach their exact source/package reads, and seal the one-root
/// render plan. This is the shared Rust construction used by WASM and native
/// `fig prepare`. Both hosts supply honest exact project and package manifests
/// instead of asking this helper to invent identity from row data.
pub fn close_projection(
    db: &site_db::SiteDb,
    input: CloseProjectionInput,
) -> Result<ClosedSiteDbProjection, CloseProjectionError> {
    if input.render_target.mode != RenderMode::ExternalBuilder {
        return Err(CloseProjectionError::NotExternalBuilder);
    }

    let mut projection = project(db)?;
    projection.record.reads =
        input
            .project
            .sources
            .iter()
            .map(|(path, _)| crate::ReadDependency::Source { path: path.clone() })
            .chain(input.package_lock.iter().map(|(coordinate, _)| {
                crate::ReadDependency::Package {
                    coordinate: coordinate.clone(),
                }
            }))
            .collect();
    let required = projection.record.key.clone();
    let artifacts = ArtifactCatalog::from_records([projection.record])?;
    let site_build = SiteBuild::new(
        input.project,
        input.package_lock,
        input.render_target,
        RenderPlan::new([required]),
        artifacts,
        input.diagnostics,
    )?
    .close()?;

    Ok(ClosedSiteDbProjection {
        site_build,
        bytes: projection.bytes,
    })
}
