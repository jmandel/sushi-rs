//! `site_build` defines the versioned, immutable handoff between compilation and
//! rendering.
//!
//! The contract is intentionally independent of a renderer and of `site.db`.
//! It describes an exact project revision, exact package inputs, a render target,
//! and a typed artifact catalog. Artifact bytes live in a content-addressed store;
//! this manifest contains only verified references to those bytes.
//!
//! A [`SiteBuild`] is created atomically with [`SiteBuild::new`]. Its build id is
//! the SHA-256 of a canonical JSON projection of every other field. Collection
//! types whose ordering has no semantic meaning use sorted maps/sets, so source,
//! package, diagnostic, and artifact insertion order cannot change the id.
//! A [`RenderPlan`] names required roots; [`ClosedSiteBuild`] proves those roots
//! and their transitive artifact dependencies are fully materialized for a
//! callback-free consumer.

mod canonical;
mod content;
mod model;

#[cfg(feature = "site-db-compat")]
pub mod site_db_compat;

pub use canonical::{canonical_json_bytes, sha256_canonical, CanonicalError};
pub use content::{BuildId, ContentRef, Sha256Digest};
pub use model::*;
