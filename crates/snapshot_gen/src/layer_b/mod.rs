//! Layer B — an OPT-IN, composable overlay over Layer-A (walk) snapshot output.
//!
//! Layer A (`walk/`) stays the pure, policy-free `generateSnapshot` function.
//! Layer B reproduces the IG Publisher's post-passes over a finished native-R5
//! snapshot, as separately-toggleable stages, each cited to Publisher/fhir-core
//! code and each demanded by a fixture (REWORK-PLAN §7 item 4). Default OFF:
//! nothing here runs unless a caller explicitly opts in.
//!
//! Stages implemented (task #17):
//!   * **B1 PIN** (`pin`) — CoreVersionPinner mechanism A (canonical `|version`
//!     on in-context-resolvable refs). Composition (b): post-pass over walk
//!     output, proven equivalent to Java's load-time pin (see `pin.rs`).
//!   * **B0 PROJECT** (`project`) — R4-artifact projection (R4 key order +
//!     constraint.xpath restore + R5-only field demotion). Version-conditional:
//!     R4 IGs only.
//!
//! Quirk registry (`quirks`) exists from day one.
//!
//! Composition mirrors the Publisher exactly:
//!   * **B1 PIN runs at WALK time** (composition (a)): the walk pins inherited
//!     base/dep SD snapshots (`walk::generate_snapshot_opt_pin` +
//!     `WalkContext::pin_base_versions`) so pins flow through snapshot
//!     inheritance, and canonicals the profile's own differential re-supplies
//!     stay UNPINNED — because Java's `CoreVersionPinner` only ever stamps the
//!     *core* package's structures at load, never the IG's authored profiles,
//!     and the profile snapshot inherits already-pinned base elements. A naive
//!     post-pass that pins the final snapshot over-pins differential-supplied
//!     canonicals (measured: `Observation.subject`->Patient is re-supplied by
//!     period-tracking-fact's differential and Java leaves it unpinned). Hence
//!     (a), not (b), for B1. The `apply_post` overlay does NOT re-pin.
//!   * **B0 PROJECT runs as a post-pass** over the finished (pinned) snapshot:
//!     R4 key order + `constraint.xpath` restore + R5-only field demotion. It
//!     never touches canonical *values*, so it composes cleanly after the pin.

pub mod pin;
pub mod project;
pub mod quirks;

use serde_json::Value;

use crate::PackageContext;

/// Which Layer-B stages to run. All default OFF.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayerBOptions {
    /// B1 — CoreVersionPinner (mechanism A) version pinning.
    pub pin: bool,
    /// B0 — R4-artifact projection. Only meaningful for an R4 IG; the caller is
    /// responsible for the version condition (site_db gates on `--core` R4).
    pub project_r4: bool,
}

impl LayerBOptions {
    /// True if any Layer-B stage is enabled (i.e. this is not a no-op overlay).
    pub fn any(&self) -> bool {
        self.pin || self.project_r4
    }
}

/// Apply the enabled POST-WALK Layer-B stages to a finished (walk-generated,
/// already-pinned if `opts.pin`) SD. Only B0 (`project_r4`) is a post-pass; B1
/// (pin) is done at walk time (see module docs). Pure w.r.t. `pkg` (reads only).
pub fn apply_post(sd: Value, pkg: &PackageContext, opts: LayerBOptions) -> Value {
    if opts.project_r4 {
        project::project_r4(&sd, pkg)
    } else {
        sd
    }
}
