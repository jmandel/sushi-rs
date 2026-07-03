//! `WalkContext` — the long-lived shared/mutable state (output list, diff clone,
//! messages, SD store, config, annotations sidecar). See spec §2, §5.

use serde_json::Value;
use std::collections::HashMap;
use std::rc::Rc;

use crate::PackageContext;

/// A collected message/error (spec §6). We record wording + severity for parity
/// where gated; most are informational to callers.
#[derive(Clone, Debug)]
pub struct Message {
    pub severity: Severity,
    pub path: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Information,
}

/// Per-output-element sidecar annotations (spec §5). Keyed by output index.
#[derive(Clone, Debug, Default)]
pub(crate) struct Annotation {
    /// SNAPSHOT_diff_source: the diff index this output row consumed (if any).
    pub diff_source: Option<usize>,
}

pub(crate) struct WalkConfig {
    /// forPublication (oracle: true).
    pub for_publication: bool,
}

impl Default for WalkConfig {
    fn default() -> Self {
        WalkConfig {
            for_publication: true,
        }
    }
}

/// The shared walk state. `output` is append-only across all frames; `diff` is
/// the differential clone the walk consumes.
pub(crate) struct WalkContext<'a> {
    pub pkg: &'a PackageContext,
    pub output: Vec<Value>,
    /// Per-output annotation (parallel to `output`).
    pub output_ann: Vec<Annotation>,
    pub diff: Rc<Vec<Value>>,
    /// Which diff indices were consumed (SNAPSHOT_GENERATED_IN_SNAPSHOT set).
    pub diff_consumed: Vec<bool>,
    /// SNAPSHOT_PREPROCESS_INJECTED per diff index (exempt from PC-1 provenance).
    pub diff_injected: Vec<bool>,
    pub messages: Vec<Message>,
    pub cfg: WalkConfig,
    /// Memoized generated snapshots by url (recursive base/type generation).
    pub gen_cache: HashMap<String, Rc<Value>>,
    /// Re-entrancy guard for circular snapshot generation.
    pub gen_stack: Vec<String>,
    /// The derived profile url (for messages).
    pub derived_url: String,
}

impl<'a> WalkContext<'a> {
    pub fn add_message(&mut self, severity: Severity, path: &str, text: String) {
        self.messages.push(Message {
            severity,
            path: path.to_string(),
            text,
        });
    }

    /// Mark a diff row consumed (SNAPSHOT_GENERATED_IN_SNAPSHOT) without emitting
    /// a new output row (e.g. an anchor whose updateFromDefinition consumed it).
    pub fn mark_consumed(&mut self, diff_idx: usize) {
        if diff_idx < self.diff_consumed.len() {
            self.diff_consumed[diff_idx] = true;
        }
    }

    /// Append an emitted element to the output, recording its consumed diff row.
    pub fn add_to_result(&mut self, element: Value, diff_source: Option<usize>) {
        if let Some(d) = diff_source {
            if d < self.diff_consumed.len() {
                self.diff_consumed[d] = true;
            }
        }
        self.output.push(element);
        self.output_ann.push(Annotation { diff_source });
    }
}
