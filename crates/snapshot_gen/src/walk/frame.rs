//! Per-recursion `WalkFrame`, the mutable `WalkCursor`, `SlicingParams`, and
//! `ElementRedirection`. Mirrors ProfilePathProcessor's immutable builder fields
//! (PPP:61-122) split from the mutable cursor object (PPS:11-21).

use serde_json::Value;
use std::rc::Rc;

/// ElementRedirection (ER:7-28): a contentReference redirection frame.
#[derive(Clone, Debug)]
pub(crate) struct ElementRedirection {
    pub path: String,
    #[allow(dead_code)]
    pub element: Rc<Value>,
}

/// PathSlicingParams (PSP:15-37) — slice context for the loop dispatch.
#[derive(Clone, Debug, Default)]
pub(crate) struct SlicingParams {
    pub done: bool,
    /// The slicing anchor element (may be absent).
    pub element_definition: Option<Rc<Value>>,
    pub path: Option<String>,
    /// Sibling diff slices (`withDiffs` copies diffMatches[1..]).
    pub slices: Vec<Value>,
}

impl SlicingParams {
    pub fn done_with(element_definition: Option<Rc<Value>>, path: Option<String>) -> Self {
        SlicingParams {
            done: true,
            element_definition,
            path,
            slices: Vec::new(),
        }
    }

    pub fn with_diffs(mut self, diff_matches_values: &[Value]) -> Self {
        if diff_matches_values.len() > 1 {
            self.slices = diff_matches_values[1..].to_vec();
        }
        self
    }
}

/// ProfilePathProcessorState (PPS) — the mutable cursor object. `base` is the
/// element list currently being walked (base snapshot, or a data type's
/// snapshot when unfolding).
#[derive(Clone)]
pub(crate) struct WalkCursor {
    pub base_source_url: String,
    pub base: Rc<Vec<Value>>,
    pub base_cursor: usize,
    pub diff_cursor: usize,
    pub context_name: String,
    pub result_path_base: Option<String>,
}

/// The immutable-per-recursion frame (cheap clone; passed by value into each
/// recursion). Shared/mutable state lives in `WalkContext`.
#[derive(Clone)]
pub(crate) struct WalkFrame {
    pub base_limit: usize,
    /// -1 when there is no diff.
    pub diff_limit: isize,
    pub url: String,
    pub web_url: Option<String>,
    pub profile_name: String,
    pub context_path_source: Option<String>,
    pub context_path_target: Option<String>,
    pub trim_differential: bool,
    pub redirector: Vec<ElementRedirection>,
    /// The SD whose url is stamped into base bookkeeping (SNAPSHOT_BASE_MODEL).
    pub source_sd_url: String,
    /// Spec URL for markdown-relative rewriting (context.getSpecUrl()); stable
    /// across the whole generation (the loaded context version).
    pub spec_url: String,
    pub slicing: SlicingParams,
}
