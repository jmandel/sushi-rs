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
pub mod layer_b;
mod merge;
mod package;
mod text;
mod walk;

pub use cli::{main_cli, SnapshotOptions};
pub use layer_b::{apply_post as apply_layer_b_post, LayerBOptions};
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

/// OPT-IN: generate a snapshot (Layer A, unchanged) and then apply the enabled
/// Layer-B overlay stages (`layer_b::apply`) to the result. With
/// `LayerBOptions::default()` (all OFF) this is byte-identical to
/// [`generate_snapshot`]; every gate proves it. See `layer_b` for the stages.
pub fn generate_snapshot_layer_b(
    derived: Value,
    ctx: &PackageContext,
    options: SnapshotOptions,
    layer_b: LayerBOptions,
) -> anyhow::Result<Value> {
    // B1 (pin) is composition (a): the walk pins inherited base/dep snapshots so
    // pins flow through inheritance and differential-supplied canonicals stay
    // unpinned (Java-isomorphic). B0 (project_r4) is a pure post-pass over the
    // finished snapshot. With `layer_b` all-OFF this is byte-identical to
    // `generate_snapshot`.
    let snapshot = walk::generate_snapshot_opt_pin(derived, ctx, options, layer_b.pin)?;
    Ok(layer_b::apply_post(snapshot, ctx, layer_b))
}
