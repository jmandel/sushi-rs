//! `render_sd` — C1: a byte-exact Rust port of fhir-core's
//! `StructureDefinitionRenderer` element-table path (`generateTable`,
//! `generateGrid`, and the row builders), producing the SD table fragments.
//!
//! Source of truth (READ-ONLY): fhir-core 6.9.10-SNAPSHOT
//! `.../r5/renderers/StructureDefinitionRenderer.java` — the renderer version
//! that made the golden corpus. Output is a `render_xhtml::XhtmlNode` tree
//! composed with the HTML-non-pretty composer (the publisher's
//! `new XhtmlComposer(XhtmlComposer.HTML)`), then wrapped in `{% raw %}...`.
//!
//! Depends on `render_tables` (C2) for the table model + render engine and
//! `render_xhtml` (C3) for the byte-exact composer.

/// An unported render feature. On NATIVE it panics LOUDLY (the dev/parity
/// discipline — an unported path must never silently render wrong output). On
/// WASM it DEGRADES to `$fallback` instead, because wasm32-unknown-unknown forces
/// `panic=abort`: a panic there aborts the WHOLE engine and 404s every later page
/// of the preview. `$fallback` is the graceful value for the call site (`()` to
/// skip an optional row, `None`, or a visible placeholder string/node).
#[macro_export]
macro_rules! loud_gap {
    ($fallback:expr, $($arg:tt)*) => {{
        #[cfg(not(target_arch = "wasm32"))]
        { panic!($($arg)*) }
        #[cfg(target_arch = "wasm32")]
        { $fallback }
    }};
}

pub mod aggregates;
pub mod commonmark;
pub mod context;
pub mod tree;
pub mod dict;
pub mod deptable;
pub mod diff;
pub mod engine;
pub mod fstxcache;
pub mod txcache;
pub mod vscs;
pub mod gentypes;
pub mod grid;
pub mod ipstmt;
pub mod leaf;
pub mod markdown;
pub mod pseudojson;
pub mod publisher_markdown;
pub mod sdmodel;
pub mod span;
pub mod table;
pub mod tx;
pub mod xref;
pub mod xreflist;

pub use sdmodel::Sd;

/// Wrap a composed fragment body in the publisher's `{% raw %}...{% endraw %}`
/// (PublisherGenerator.java wrapLiquid). The golden files carry this wrapper.
pub fn wrap_raw(body: &str) -> String {
    format!("{{% raw %}}{}{{% endraw %}}", body)
}

/// The set of SD table fragment kinds that all route through one
/// `sdr.generateTable(...)` / `generateGrid(...)` call (the publisher SDR
/// wrappers). Each maps to a flag tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Grid,
    Snapshot,
    SnapshotAll,
    Diff,
    DiffAll,
    // ... remaining kinds added as they are brought to parity.
}
