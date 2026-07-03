//! `WalkContext` — the long-lived shared/mutable state (output list, diff clone,
//! messages, SD store, config, annotations sidecar). See spec §2, §5.

use serde_json::Value;
use std::collections::HashMap;
use std::rc::Rc;

use crate::PackageContext;

/// A collected message/error (spec §6). We record wording + severity for parity
/// where gated; most are informational to callers.
///
/// This is the walk's message-collection scaffolding (REWORK-PLAN §2:
/// "Unconsumed differential rows are an error ... collected into messages,
/// gate-checked"). Messages are constructed throughout the walk via
/// `add_message`; no consumer surfaces the collected log yet, so the fields read
/// as dead. Kept as the wired-in home for that infrastructure — not migration
/// cruft.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Message {
    pub severity: Severity,
    pub path: String,
    pub text: String,
}

#[allow(dead_code)]
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
    /// Recorded for the provenance/message infrastructure; not read on any hot
    /// branch yet.
    #[allow(dead_code)]
    pub diff_source: Option<usize>,
    /// SNAPSHOT_auto_added_slicing (PU userData): the slicing block on this row
    /// was synthesized by makeExtensionSlicing, so the finalize slice-min pass is
    /// allowed to overwrite its `min` from the sum of slice mins (PU:1012-1014).
    pub auto_added_slicing: bool,
}

pub(crate) struct WalkConfig {
    /// forPublication (oracle: true). Mirrors the Publisher's generateSnapshot
    /// config flag; the walk fixes it to the oracle value and does not branch on
    /// it yet.
    #[allow(dead_code)]
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
    /// Fixed to the oracle config (`WalkConfig::default`); held for parity with
    /// the Publisher's config-threaded generateSnapshot, not branched on yet.
    #[allow(dead_code)]
    pub cfg: WalkConfig,
    /// Memoized generated snapshots by url (recursive base/type generation).
    pub gen_cache: HashMap<String, Rc<Value>>,
    /// Re-entrancy guard for circular snapshot generation.
    pub gen_stack: Vec<String>,
    /// The derived profile url (for messages).
    pub derived_url: String,
    /// context.getSpecUrl() for the current generation (markdown link rewriting).
    pub spec_url: String,
    /// LAYER B / B1 (opt-in, default false). When true, base/dep SD snapshots
    /// resolved for inheritance are version-pinned (CoreVersionPinner mechanism A,
    /// composition (a)) BEFORE the walk copies their elements — reproducing Java's
    /// load-time pin that flows through snapshot inheritance. OFF = zero change to
    /// Layer A (every existing gate proves it). See `layer_b::pin`.
    pub pin_base_versions: bool,
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
        self.output_ann.push(Annotation {
            diff_source,
            auto_added_slicing: false,
        });
    }
}
