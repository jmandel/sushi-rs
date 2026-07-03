//! Pure FHIR snapshot generator — decision-isomorphic walk engine.
//!
//! Single engine: `generate_snapshot` runs the `walk/` port of
//! `ProfileUtilities.generateSnapshot` (see snapshot/REWORK-PLAN.md). Everything
//! is R5-internal; R4 inputs and bases are converted at load (`convert.rs`).
//! `merge.rs`/`text.rs`/`walk/` carry the live behaviour; `package.rs` loads
//! resources; `cli.rs` drives the command line.

use serde_json::Value;

mod cli;
pub(crate) mod convert;
mod merge;
mod package;
mod text;
mod walk;

pub use cli::{main_cli, SnapshotOptions};
pub use package::PackageContext;
pub use walk::{disable_trace, enable_trace};

// Re-export the merge helpers at crate root so `walk/` can reach them via
// `use crate::{...}` without per-item import churn. `text` items are referenced
// path-qualified (`crate::text::…`), so no glob re-export is needed for them.
pub(crate) use merge::*;

/// Stage-2 pure R4->R5 StructureDefinition conversion (VersionConvertor_40_50
/// semantics). Context-free; R5 inputs pass through unchanged. Exposed for the
/// `--dump-converted` CLI mode and the `convert_parity` gate.
pub fn convert_r4_sd_to_r5(sd: &Value) -> anyhow::Result<Value> {
    convert::r4_sd_to_r5(sd)
}

/// Generate a StructureDefinition snapshot with the decision-isomorphic walk
/// engine (the only engine). `derived` is the input profile (R4 or R5); the
/// returned value is the input with a generated `snapshot` element, R5-internal.
pub fn generate_snapshot(
    derived: Value,
    ctx: &PackageContext,
    options: SnapshotOptions,
) -> anyhow::Result<Value> {
    walk::generate_snapshot(derived, ctx, options)
}
