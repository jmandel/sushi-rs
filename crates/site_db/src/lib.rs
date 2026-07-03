//! site_db — the cycle site.db producer (Phase 2, task #15).
//!
//! Pipeline S1..S7 (see docs/cycle-package-db-plan.md §2b):
//!   S1/S2  compiler::build_project_with_cache  -> fsh-generated/resources/*.json
//!   S3     snapshot_gen::generate_snapshot     -> snapshot-complete SDs (in place)
//!   S5     rows::*                              -> Resources/Concepts/Metadata rows
//!   S6     augment::augment                     -> Pages/Menu/SiteConfig/Assets rows
//!   S7     writer::write_site_db                -> site.db (rusqlite sink)
//! S4 (ValueSet expansion) is deferred (§4b — cycle needs zero expansions).
//!
//! The row model (`model::SiteDb`) is sqlite-free; only `writer` touches
//! rusqlite (wasm requirement §5). A §2c BuildState ledger runs from day one.

pub mod augment;
pub mod ledger;
pub mod model;
pub mod pipeline;
pub mod rows;
pub mod timefmt;
pub mod writer;

pub use ledger::{BuildLedger, LedgerReport};
pub use model::SiteDb;
pub use pipeline::{build, BuildConfig, BuildOutcome};

use anyhow::Result;
use std::path::Path;

/// One-shot: run the pipeline and write site.db + the ledger sidecar. Returns the
/// ledger report (for the no-op gate). If a prior ledger sidecar exists next to
/// `out_db`, it is used to classify dirtiness and, on a proven no-op, the site.db
/// write is skipped.
pub fn build_and_write(config: &BuildConfig) -> Result<LedgerReport> {
    let ledger_path = ledger_sidecar_path(&config.out_db);
    let prior = BuildLedger::load(&ledger_path);
    let outcome = build(config, prior.as_ref())?;

    if outcome.ledger.no_op && config.out_db.exists() {
        // Proven no-op: nothing changed and the artifact is already present.
        // Write nothing (gate v). Refresh the ledger sidecar bytes (identical).
        outcome.ledger.ledger.save(&ledger_path)?;
        return Ok(outcome.ledger);
    }

    writer::write_site_db(&config.out_db, &outcome.db)?;
    outcome.ledger.ledger.save(&ledger_path)?;
    Ok(outcome.ledger)
}

/// The ledger sidecar path for a given site.db (`<out>.buildstate.json`).
pub fn ledger_sidecar_path(out_db: &Path) -> std::path::PathBuf {
    let mut s = out_db.as_os_str().to_os_string();
    s.push(".buildstate.json");
    std::path::PathBuf::from(s)
}
