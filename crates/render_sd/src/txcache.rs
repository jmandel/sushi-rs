//! `render_sd::txcache` — the terminology-cache SEAM for the ValueSet/CodeSystem
//! terminology fragments (`cld`, `expansion`).
//!
//! ## Why a trait
//!
//! The publisher's `BaseWorkerContext` answers two terminology questions while
//! rendering VS/CS narratives:
//!   1. **expand** a ValueSet — the `expansion` fragment renders whatever is in
//!      `vs.expansion.contains[]` + `.parameter[]`. For a VS the publisher had
//!      not pre-expanded, it calls `context.expandVS(...)`; the RESULT (an
//!      expanded ValueSet) is what `generateExpansion` renders. Some expansions
//!      are computed LOCALLY (enumerating an IG-owned CodeSystem — the golden
//!      says "Expansion performed internally based on ..."); others come from a
//!      terminology server and are stored as cached `$expand` responses in the
//!      build's `input-cache/txcache/*.cache` files.
//!   2. **lookup a code's display** — `genInclude`'s filter branch renders
//!      `... where concept is-a 404684003 (Clinical finding (finding))`; the
//!      `(display)` is a `validateCode` display lookup, cached the same way.
//!
//! To keep the renderer storage-agnostic, both questions go through the
//! [`TxCacheSource`] trait. The filesystem implementation [`FsTxCache`] reads
//! the build's `input-cache/txcache` directory. **The editor's OPFS-backed tx
//! cache will implement this SAME trait** (reading request/response pairs out of
//! OPFS instead of the local FS), so the renderer code never touches
//! `std::fs` — only `FsTxCache` does. The trait signatures take/return plain
//! owned data (no `Path`, no `std::fs`), which is the seam the editor plugs into.
//!
//! ## The on-disk cache formats (what `FsTxCache` reads)
//!
//! - `vs-externals.json` / `cs-externals.json`: `{ canonical: { server, filename } }`
//!   maps to a tx-fetched ValueSet/CodeSystem body (`vs-*.json` / `cs-*.json`).
//! - `*.cache` files: a sequence of request/response blocks. Blocks are
//!   separated by a line of `-` characters. Each block is
//!   `<request-json>####<tag>: <response>` where `<tag>` is `e` (expand),
//!   `v` (validate-code), etc. An **expand** request JSON has a top-level
//!   `"valueSet": { compose... }` (optionally `"hierarchical"`); its response
//!   `e:` body has `valueSet.expansion` with `.contains[]` + `.parameter[]`. A
//!   **validate-code** request has a `"code"` (coding) + optional `"url"`/
//!   `"system"`; its `v:` response has a `display`.

use serde_json::Value;

/// An expanded ValueSet, as the `expansion` fragment renders it: the
/// `expansion.contains[]` concept list plus `expansion.parameter[]` (which carry
/// the `used-codesystem` / `system-version` version notices). This mirrors the
/// `ValueSet.expansion` object the publisher renders.
#[derive(Debug, Clone)]
pub struct ExpandedValueSet {
    /// `expansion.contains[]` (each entry a JSON object with system/code/display/
    /// version/contains). Raw JSON so the renderer reads the same fields the Java
    /// `ValueSetExpansionContainsComponent` exposes.
    pub contains: Vec<Value>,
    /// `expansion.parameter[]` (name/value objects). Drives the version-notice
    /// box (`used-*` / `version` params) and the JSON/XML copy version.
    pub parameters: Vec<Value>,
    /// `expansion.total`, if the server reported it.
    pub total: Option<i64>,
    /// The expansion source: `"internal"` (local CodeSystem enumeration → the
    /// "Expansion performed internally based on ..." header) or a server base URL
    /// (e.g. `"tx.fhir.org"` → "Expansion from tx.fhir.org based on ..."). None
    /// means the plain "Expansion based on ..." header (no source userdata).
    pub source: Option<String>,
}

/// The terminology-cache seam. Minimal + storage-agnostic (no `std::fs`, no
/// `Path` in the signatures) so the editor's OPFS cache can implement it too.
pub trait TxCacheSource {
    /// Expand a ValueSet for the `expansion` fragment. `vs_url` is the VS
    /// canonical (versionless), `vs_json` the full VS resource (its `compose`
    /// keys the cached `$expand` request). Returns the expanded VS if the cache
    /// (or a local enumeration) can answer, else None (the caller fires a loud
    /// gap rather than approximate).
    fn expand(&self, vs_url: &str, vs_json: &Value) -> Option<ExpandedValueSet>;

    /// Look up a code's display (the `validateCode` display used by the cld
    /// filter branch's `(display)` suffix). `version` is the include's stated
    /// version (may be empty). Returns None when the cache has no matching entry.
    fn lookup_display(&self, system: &str, code: &str, version: &str) -> Option<String>;
}
