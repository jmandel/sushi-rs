//! Native host composition for the `fig` FHIR IG CLI.
//!
//! The canonical domain flow remains `PreparedGuide -> SiteBuild -> SiteOutput`
//! over `ContentStore`; this crate does not define a native-only model. CLI
//! subcommands are thin argument/result adapters over typed library calls.
//!
//! Layout:
//!   - [`prepare`] captures native inputs and emits one canonical closed
//!     SiteBuild plus addressed objects.
//!   - [`site`] restores a Publisher build and delegates directly to
//!     `SiteEngine::outputs`, `render`, and `finalize`.
//! Native cache lookup is a private renderer-adapter optimization and never a
//! distinct Fig operation or result shape.
//!
//! `--json` on every subcommand emits the shared [`api_envelope`] envelope —
//! schema-identical to the WASM Session's transport envelope.

mod output_cache;
pub mod prepare;
mod publication;
pub mod site;

/// Engine + pins, as the `version` op payload (shared by the human and `--json`
/// paths). Mirrors `wasm_api`'s `version()` fields so the two skins report the
/// same identity.
pub fn version_payload() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("FIG_GIT_COMMIT").unwrap_or("unknown"),
        "engine": "site_engine + rust_sushi + snapshot_gen",
        "apiVersion": api_envelope::API_VERSION,
    })
}
