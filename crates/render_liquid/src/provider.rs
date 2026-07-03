//! The pluggable data + include resolution surface.
//!
//! `DataProvider` is the seam the plan calls for: T1's `site.data.*` reads are
//! served through it, so the same engine can be backed by (a) an in-memory
//! JSON context (tests / oracle parity), or (b) `site.db` queries later (F5).
//! Include resolution is likewise pluggable so the host controls where
//! `{% include %}` bodies and template-artifact `.xhtml` fragments come from.

use crate::value::Value;

/// Resolves the dynamic data surfaces of a Liquid render.
pub trait DataProvider {
    /// Resolve `site.data.<key>` at the FIRST key after `site.data`, returning
    /// the whole subtree (Hash/Array/scalar). The engine walks any deeper path
    /// itself with correct typing (int array indexes, `.[expr]`, etc.), so a
    /// provider only implements one-level lookup and can return a large subtree
    /// (or a lazy Drop later). `None` -> Liquid nil.
    ///
    /// Example: `{{ site.data.fhir.path }}` calls `site_data(&["fhir"])` and the
    /// engine then indexes `.path` on the returned value.
    fn site_data(&self, path: &[&str]) -> Option<Value> {
        let _ = path;
        None
    }

    /// Resolve any other `site.<key>` (e.g. `site.title`, `site.pages`).
    fn site(&self, path: &[&str]) -> Option<Value> {
        let _ = path;
        None
    }

    /// Resolve an `{% include NAME %}` body to raw template source. `params`
    /// are the already-evaluated include arguments (exposed to the include as
    /// `include.*`). Returning `None` means "not found" — the engine emits the
    /// same include-not-found behavior the host configures.
    fn include_source(&self, name: &str) -> Option<String> {
        let _ = name;
        None
    }
}

/// A trivial provider backed by a JSON-shaped in-memory context. Used by the
/// differential gate so the Rust engine and the Ruby oracle share one context
/// and only the ENGINE differs.
pub struct JsonProvider {
    pub data: Value,       // the value at `site.data`
    pub site_extra: Value, // other `site.*` keys (a Hash)
    pub includes: std::collections::HashMap<String, String>,
}

impl JsonProvider {
    pub fn new() -> Self {
        Self {
            data: Value::Nil,
            site_extra: Value::Nil,
            includes: std::collections::HashMap::new(),
        }
    }
}

impl Default for JsonProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DataProvider for JsonProvider {
    fn site_data(&self, path: &[&str]) -> Option<Value> {
        let mut cur = self.data.clone();
        for seg in path {
            cur = cur.index(&Value::str(*seg));
        }
        if cur.is_nil() {
            None
        } else {
            Some(cur)
        }
    }

    fn site(&self, path: &[&str]) -> Option<Value> {
        let mut cur = self.site_extra.clone();
        for seg in path {
            cur = cur.index(&Value::str(*seg));
        }
        if cur.is_nil() {
            None
        } else {
            Some(cur)
        }
    }

    fn include_source(&self, name: &str) -> Option<String> {
        self.includes.get(name).cloned()
    }
}
