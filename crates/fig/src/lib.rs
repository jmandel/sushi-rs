//! Native host composition for the `fig` FHIR IG CLI.
//!
//! The canonical domain flow remains `PreparedGuide -> SiteBuild -> SiteOutput`
//! over `ContentStore`; this crate does not define a native-only model. CLI
//! subcommands are thin argument/result adapters over typed library calls.
//!
//! Layout:
//!   - [`prepare`] captures native inputs and emits the canonical closed
//!     `cycle-site/v2` SiteBuild plus addressed objects.
//!   - [`output_cache`] performs verified pre-render SiteOutput lookup and
//!     post-render publication through the shared cache/store contracts.
//!   - [`template`] owns native package acquisition/materialization helpers.
//!   - [`engine`] and [`watch`] are legacy staged-Publisher-tree tools. They do
//!     not compile or implement `prepare -> outputs -> render -> finalize`; they
//!     remain only while useful rendering/read-set machinery is migrated, then
//!     are deletion targets rather than a parallel host architecture.
//!
//! `--json` on every subcommand emits the shared [`api_envelope`] envelope —
//! schema-identical to the WASM Session's transport envelope.

pub mod engine;
pub mod output_cache;
pub mod prepare;
pub mod template;
pub mod watch;

/// Engine + pins, as the `version` op payload (shared by the human and `--json`
/// paths). Mirrors `wasm_api`'s `version()` fields so the two skins report the
/// same identity.
pub fn version_payload() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("FIG_GIT_COMMIT").unwrap_or("unknown"),
        "engine": "rust_sushi + snapshot_gen (walk) + render_sd/render_page",
        "apiVersion": api_envelope::API_VERSION,
    })
}
