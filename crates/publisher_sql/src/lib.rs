//! HL7 Publisher-style SQL directives over a closed query snapshot.
//!
//! SQL is deliberately not a Liquid tag implementation. The Publisher scans
//! page/include source before Liquid, evaluates directives against `package.db`,
//! and then gives the resulting source/data to Jekyll. This crate owns the same
//! capability boundary for both native and `wasm32-unknown-unknown` hosts:
//!
//! 1. an internal query snapshot deterministically projects the guide's compiled
//!    resources into `Resources` plus the CodeSystem and basic ConceptMap
//!    relational tables used by the first supported query surface.
//! 2. [`SqlRuntime`] materializes those rows into an in-memory SQLite database.
//! 3. queries are read-only, bounded, and have no filesystem/network/global DB.
//! 4. [`expand_publisher_sql`] consumes that capability once and returns only
//!    ordinary rewritten sources, generated includes, and generated data.
//!    `render_page` and `render_liquid` remain unaware of SQL.
//!
//! This is deliberately not yet a claim of complete `package.db` parity.
//! Publisher metadata, terminology expansions, cross-view list tables,
//! dependency `codeSystems`, configured resource web paths, and context-aware
//! Canonical/Resource/Coding cell rendering require additional closed inputs
//! and their own native/WASM oracles.

mod database;
mod expand;
mod output;
mod snapshot;

pub use database::SqlRuntime;
pub use expand::{expand_publisher_sql, PublisherSqlExpansion};
