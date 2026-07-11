//! `fig` — the unified FHIR IG CLI library.
//!
//! ONE engine, three skins: the native CLI (`fig`, this crate's bin), the wasm
//! `Session` (`wasm_api`), and the library API (the `render_*`/compiler/
//! snapshot_gen/prepared_guide crates). The CLI subcommands are thin — arg-parse →
//! engine-core call → output — and this library is where any COMPOSITION the
//! engine core lacks lives, so native and WASM hosts can share it.
//!
//! Layout:
//!   - [`engine`]  — native "engine methods": the render composition (build →
//!                   snapshot → PreparedGuide → page pass → asset copy) and the render
//!                   surface assembly the page pass drives. This is the native
//!                   twin of `wasm_api::render_surface`; both compose the SAME
//!                   F5/F6 machinery (`render_sd::engine::FragmentEngine` +
//!                   `render_page::render_page`).
//!   - [`watch`]   — the incremental dev loop (mtime poll → dirty cone via the
//!                   BuildState/PageProvider read-set boundary → re-render →
//!                   live-reload server). The native twin of the browser editor.
//!
//! `--json` on every subcommand emits the shared [`api_envelope`] envelope —
//! schema-identical to the Session's (one implementation, `api_envelope`).

pub mod engine;
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
