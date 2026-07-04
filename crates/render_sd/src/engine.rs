//! `FragmentEngine` — the library promotion of `bin/corpus.rs`'s fragment
//! dispatcher (F5 handoff). One entry point, `render_fragment(ref_, kind)`,
//! returns the FULL fragment file body (`{% raw %}`-wrapped) that the publisher
//! would have written to `_includes/{ref_}-{kind}.xhtml`, or a typed error.
//!
//! The page pass (`render_page`) calls this on a `{% include %}` MISS: the
//! publisher pre-generates every fragment, but the editor's lazy model
//! materializes a fragment only when a page's include asks for it
//! (first-include-miss, plan §2 decision 2).
//!
//! ## The (ref_, kind) split
//! An include name like `StructureDefinition-us-core-patient-snapshot.xhtml` is
//! parsed into `ref_ = "StructureDefinition-us-core-patient"` + `kind =
//! "snapshot"` by LONGEST-REGISTERED-KIND-SUFFIX (ids contain hyphens; kinds are
//! a closed registered set). IG-level singleton fragments (`canonical-index`,
//! `dependency-table-nontech`, …) have an EMPTY `ref_`.
//!
//! ## Cache-key split (per-resource vs whole-IG)
//! - Per-resource kinds take `ref_` and read only that one resource → cache key
//!   = the resource's content hash.
//! - Whole-IG kinds (`uses`/`sd-xref`/`maps` and the singleton aggregates)
//!   consult `ctx.own_resources()` → cache key = the IG manifest hash.
//! [`FragmentEngine::is_whole_ig_kind`] exposes this split to the host.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::context::IgContext;
use crate::{wrap_raw, Sd};

/// Per-IG build facts the singleton (IG-level) renderers need that are NOT
/// derivable from `output/*.json` alone (documented in corpus.rs). In the editor
/// these are computed from the loaded package set / sushi-config / IG manifest;
/// the parity harness harvests them (same oracle-input pattern as run_uuid).
#[derive(Clone, Default)]
pub struct IgFacts {
    /// `getExpansionParameters()` had interesting params (expansion-params).
    pub has_expansion_params: bool,
    /// PreviousVersionComparator loaded a lastVersion (deleted-extensions).
    pub has_previous: bool,
    /// R4ToR4BAnalyser `newFormat`/isNewML (cross-version-analysis tgz prefix).
    pub new_format: bool,
    /// The IG business version (ImplementationGuide.version).
    pub ig_version: String,
    /// corePath for the IG-level renderers = getSpecUrl(igVersion)+"/".
    pub singleton_core_path: String,
    /// The own ImplementationGuide (id, url, version) for canonical-index.
    pub ig_resource: Option<(String, String, String)>,
    /// The own ImplementationGuide JSON (dependsOn, for ip-statements/deptable).
    pub ig_json: serde_json::Value,
    /// oids.ini registry (canonical-index).
    pub oid_map: Option<crate::aggregates::OidMap>,
    /// Package cache dir (dependency-table transitive graph).
    pub cache_dir: PathBuf,
    /// The build's loaded package version-id set (dependency-table isLoaded).
    pub loaded_set: HashSet<String>,
    /// DependencyRenderer dstFolder (temp/pages) — tree-line PNG srcs.
    pub dep_dst_folder: String,
    /// txcache dir (summary-observations terminology resolution).
    pub txcache_dir: Option<PathBuf>,
}

/// A typed fragment-render error. `Gap` is a DOCUMENTED loud gap surfaced across
/// the `catch_unwind` boundary (a not-yet-ported branch fired a panic).
#[derive(Debug, Clone)]
pub enum FragError {
    /// `kind` is not in the registry at all.
    UnknownKind(String),
    /// The named resource does not exist in the IG.
    NoSuchResource(String),
    /// A documented loud gap (the renderer panicked at a gap marker).
    Gap { kind: String, refname: String, msg: String },
}

impl std::fmt::Display for FragError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FragError::UnknownKind(k) => write!(f, "unknown fragment kind: {}", k),
            FragError::NoSuchResource(r) => write!(f, "no such resource: {}", r),
            FragError::Gap { kind, refname, msg } => {
                write!(f, "fragment gap [{} / {}]: {}", kind, refname, msg)
            }
        }
    }
}

/// The engine: an IgContext + the run-scoped inputs, exposing `render_fragment`.
pub struct FragmentEngine {
    ctx: IgContext,
    run_uuid: String,
    active_tables: bool,
    facts: IgFacts,
}

/// The closed set of registered PER-RESOURCE fragment kinds (the corpus.rs
/// `render()` arms). Ordered longest-first so the include-name split picks the
/// longest matching suffix.
pub const PER_RESOURCE_KINDS: &[&str] = &[
    // SD table kinds
    "snapshot-by-mustsupport-all",
    "snapshot-by-mustsupport",
    "snapshot-by-key-all",
    "snapshot-by-key",
    "snapshot-obligations-all",
    "snapshot-obligations",
    "snapshot-bindings-all",
    "snapshot-bindings",
    "snapshot-all",
    "snapshot",
    "diff-obligations-all",
    "diff-obligations",
    "diff-bindings-all",
    "diff-bindings",
    "diff-all",
    "diff",
    "grid",
    "spanall",
    "span",
    // leaves
    "contained-index",
    "history",
    "pseudo-ttl",
    "pseudo-xml",
    "pseudo-json",
    "inv-key",
    "inv-diff",
    "inv",
    "sd-use-context",
    "tx-diff-must-support",
    "tx-must-support",
    "tx-diff",
    "tx-key",
    "tx",
    "dict-active",
    "dict-diff",
    "dict-ms",
    "dict-key",
    "dict",
    "summary-all",
    "summary",
    "uses",
    "sd-xref",
    "maps",
    // VS/CS terminology
    "cld",
    "expansion",
    "content",
];

/// The closed set of registered WHOLE-IG (singleton) fragment kinds (corpus.rs
/// `render_singleton()` / `is_singleton_kind`). An empty `ref_`.
pub const SINGLETON_KINDS: &[&str] = &[
    "new-extensions",
    "related-igs-table",
    "related-igs-list",
    "globals-table",
    "obligation-summary",
    "deleted-extensions",
    "cross-version-analysis-inline",
    "cross-version-analysis",
    "valueset-list",
    "summary-extensions",
    "summary-observations",
    "deprecated-list",
    "expansion-params",
    "codesystem-list",
    "canonical-index",
    "ip-statements",
    "dependency-table-nontech",
    "dependency-table-short",
    "dependency-table",
    "valueset-ref-all-list",
    "valueset-ref-list",
    "codesystem-ref-all-list",
    "codesystem-ref-list",
];

/// Whole-IG PER-RESOURCE kinds (take a `ref_` but read ALL resources): their
/// cache key is the IG manifest hash, not the single resource's.
const WHOLE_IG_PER_RESOURCE: &[&str] = &["uses", "sd-xref", "maps"];

impl FragmentEngine {
    pub fn new(ctx: IgContext, run_uuid: String, active_tables: bool, facts: IgFacts) -> Self {
        FragmentEngine { ctx, run_uuid, active_tables, facts }
    }

    pub fn ctx(&self) -> &IgContext {
        &self.ctx
    }

    /// Split an include name (minus the `.xhtml`/`.html` extension) into
    /// `(ref_, kind)` by longest registered kind suffix. Returns `None` if no
    /// registered kind is a suffix. A bare singleton kind (e.g. `canonical-index`)
    /// yields `("", kind)`.
    pub fn split_include(name: &str) -> Option<(String, String)> {
        let stem = name
            .strip_suffix(".xhtml")
            .or_else(|| name.strip_suffix(".html"))
            .unwrap_or(name);
        // Singletons: the whole stem IS the kind.
        if SINGLETON_KINDS.contains(&stem) {
            return Some((String::new(), stem.to_string()));
        }
        // Per-resource: `{ref_}-{kind}` with kind the longest registered suffix.
        for k in PER_RESOURCE_KINDS {
            let tail = format!("-{}", k);
            if let Some(refpart) = stem.strip_suffix(&tail) {
                if !refpart.is_empty() {
                    return Some((refpart.to_string(), k.to_string()));
                }
            }
        }
        None
    }

    /// Is this a whole-IG kind (cache key = IG manifest hash)?
    pub fn is_whole_ig_kind(kind: &str) -> bool {
        SINGLETON_KINDS.contains(&kind) || WHOLE_IG_PER_RESOURCE.contains(&kind)
    }

    /// Render the fragment body the publisher would write to
    /// `_includes/{ref_}-{kind}.xhtml`. `ref_` is the resource prefix
    /// (`StructureDefinition-us-core-patient`) or `""` for IG singletons.
    pub fn render_fragment(&self, ref_: &str, kind: &str) -> Result<String, FragError> {
        if SINGLETON_KINDS.contains(&kind) {
            return self.render_singleton(kind);
        }
        if !PER_RESOURCE_KINDS.contains(&kind) {
            return Err(FragError::UnknownKind(kind.to_string()));
        }
        // Load the resource JSON from the IG's own resources.
        let json = self
            .ctx
            .load_own_file(ref_)
            .ok_or_else(|| FragError::NoSuchResource(ref_.to_string()))?;
        let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.render_per_resource(ref_, kind, &json)
        }))
        .map_err(|e| FragError::Gap {
            kind: kind.to_string(),
            refname: ref_.to_string(),
            msg: panic_msg(e),
        })??;
        Ok(wrap_raw(&body))
    }

    /// The per-resource dispatch (mirrors corpus.rs::render). Returns the raw
    /// body (pre-wrap). `Err(FragError)` for a kind that needs a resource shape
    /// it did not get (e.g. an SD kind on a ValueSet).
    fn render_per_resource(
        &self,
        _ref: &str,
        kind: &str,
        json: &str,
    ) -> Result<String, FragError> {
        use crate::txcache::TxCacheSource;
        let ctx = &self.ctx;
        let run_uuid = &self.run_uuid;
        let at = self.active_tables;

        // VS/CS terminology kinds route through vscs (need the resource Value).
        if matches!(kind, "cld" | "expansion" | "content") {
            let v: serde_json::Value = serde_json::from_str(json)
                .map_err(|_| FragError::NoSuchResource(_ref.to_string()))?;
            let txcache = crate::fstxcache::FsTxCache::new(
                self.facts.txcache_dir.as_deref(),
                ctx,
            );
            let _ = &txcache as &dyn TxCacheSource;
            let body = match kind {
                "content" => crate::vscs::render_cs_content(&v, ctx),
                "cld" => crate::vscs::render_vs_cld(&v, ctx, &txcache),
                "expansion" => crate::vscs::render_vs_expansion(&v, ctx, &txcache),
                _ => unreachable!(),
            };
            return Ok(body);
        }

        // Everything else is an SD kind.
        let sd = Sd::from_json(json)
            .map_err(|_| FragError::NoSuchResource(_ref.to_string()))?;
        let def_file = format!("StructureDefinition-{}-definitions.html", sd.id());
        let cp = core_path_for(&sd);
        use crate::table::TableConfig;
        let body = match kind {
            "grid" => crate::grid::render_grid(&sd, ctx, &def_file, ""),
            "span" => {
                let mut c = crate::span::SpanConfig::span();
                c.active_tables = at;
                crate::span::render_span(&sd, ctx, &c)
            }
            "spanall" => {
                let mut c = crate::span::SpanConfig::spanall();
                c.active_tables = at;
                crate::span::render_span(&sd, ctx, &c)
            }
            "snapshot" => tbl(TableConfig::snapshot(run_uuid), at, &sd, ctx, &def_file),
            "snapshot-all" => tbl(TableConfig::snapshot_all(run_uuid), at, &sd, ctx, &def_file),
            "snapshot-by-mustsupport" => {
                tbl(TableConfig::snapshot_by_mustsupport(run_uuid), at, &sd, ctx, &def_file)
            }
            "snapshot-by-mustsupport-all" => {
                tbl(TableConfig::snapshot_by_mustsupport_all(run_uuid), at, &sd, ctx, &def_file)
            }
            "snapshot-by-key" => {
                tbl(TableConfig::snapshot_by_key(run_uuid), at, &sd, ctx, &def_file)
            }
            "snapshot-by-key-all" => {
                tbl(TableConfig::snapshot_by_key_all(run_uuid), at, &sd, ctx, &def_file)
            }
            "diff" => tbl(TableConfig::diff_view(run_uuid), at, &sd, ctx, &def_file),
            "diff-all" => tbl(TableConfig::diff_all(run_uuid), at, &sd, ctx, &def_file),
            "snapshot-bindings" => {
                tbl(TableConfig::snapshot_bindings(run_uuid), at, &sd, ctx, &def_file)
            }
            "snapshot-bindings-all" => {
                tbl(TableConfig::snapshot_bindings_all(run_uuid), at, &sd, ctx, &def_file)
            }
            "snapshot-obligations" => {
                tbl(TableConfig::snapshot_obligations(run_uuid), at, &sd, ctx, &def_file)
            }
            "snapshot-obligations-all" => {
                tbl(TableConfig::snapshot_obligations_all(run_uuid), at, &sd, ctx, &def_file)
            }
            "diff-bindings" => tbl(TableConfig::diff_bindings(run_uuid), at, &sd, ctx, &def_file),
            "diff-bindings-all" => {
                tbl(TableConfig::diff_bindings_all(run_uuid), at, &sd, ctx, &def_file)
            }
            "diff-obligations" => {
                tbl(TableConfig::diff_obligations(run_uuid), at, &sd, ctx, &def_file)
            }
            "diff-obligations-all" => {
                tbl(TableConfig::diff_obligations_all(run_uuid), at, &sd, ctx, &def_file)
            }
            // leaves
            "contained-index" | "history" => crate::leaf::empty_body(),
            "pseudo-ttl" => crate::leaf::pseudo_ttl(),
            "pseudo-xml" => crate::leaf::pseudo_xml(),
            "pseudo-json" => crate::pseudojson::pseudo_json(&sd, ctx, &cp),
            "inv" => crate::leaf::inv(&sd, ctx, true, crate::leaf::GenMode::Snap, true),
            "inv-key" => crate::leaf::inv(&sd, ctx, true, crate::leaf::GenMode::Key, true),
            "inv-diff" => crate::leaf::inv(&sd, ctx, true, crate::leaf::GenMode::Diff, true),
            "sd-use-context" => crate::leaf::use_context(&sd, ctx, &cp),
            "tx" => crate::tx::render_tx(&sd, ctx, &cp, crate::tx::TxOpts::tx()),
            "tx-must-support" => {
                crate::tx::render_tx(&sd, ctx, &cp, crate::tx::TxOpts::tx_must_support())
            }
            "tx-key" => crate::tx::render_tx(&sd, ctx, &cp, crate::tx::TxOpts::tx_key()),
            "tx-diff" => crate::tx::render_tx(&sd, ctx, &cp, crate::tx::TxOpts::tx_diff()),
            "tx-diff-must-support" => {
                crate::tx::render_tx(&sd, ctx, &cp, crate::tx::TxOpts::tx_diff_must_support())
            }
            "dict" => crate::dict::render_dict(&sd, ctx, &cp, true, crate::dict::GEN_MODE_SNAP, ""),
            "dict-active" => {
                crate::dict::render_dict(&sd, ctx, &cp, false, crate::dict::GEN_MODE_SNAP, "")
            }
            "dict-diff" => {
                crate::dict::render_dict(&sd, ctx, &cp, true, crate::dict::GEN_MODE_DIFF, "diff_")
            }
            "dict-ms" => {
                crate::dict::render_dict(&sd, ctx, &cp, true, crate::dict::GEN_MODE_MS, "ms_")
            }
            "dict-key" => {
                crate::dict::render_dict(&sd, ctx, &cp, true, crate::dict::GEN_MODE_KEY, "key_")
            }
            "summary" => crate::leaf::summary(&sd, ctx, false, &cp),
            "summary-all" => crate::leaf::summary(&sd, ctx, true, &cp),
            "uses" => crate::xref::uses(&sd, ctx),
            "sd-xref" => crate::xref::references(&sd, ctx),
            "maps" => crate::xref::maps(&sd, ctx, &def_file, run_uuid, at),
            _ => return Err(FragError::UnknownKind(kind.to_string())),
        };
        Ok(body)
    }

    /// The IG-level singleton dispatch (mirrors corpus.rs::render_singleton).
    fn render_singleton(&self, kind: &str) -> Result<String, FragError> {
        let ctx = &self.ctx;
        let f = &self.facts;
        let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.singleton_body(kind)
        }))
        .map_err(|e| FragError::Gap {
            kind: kind.to_string(),
            refname: String::new(),
            msg: panic_msg(e),
        })?;
        let _ = (ctx, f);
        Ok(wrap_raw(&body))
    }

    fn singleton_body(&self, kind: &str) -> String {
        use crate::aggregates as agg;
        let ctx = &self.ctx;
        let f = &self.facts;
        let npm = ctx.own_package_id().unwrap_or("").to_string();
        match kind {
            "new-extensions" => agg::new_extensions(ctx),
            "related-igs-table" => agg::related_igs_table(ctx),
            "related-igs-list" => agg::related_igs_list(ctx),
            "globals-table" => agg::globals_table(ctx),
            "obligation-summary" => agg::obligation_summary(ctx),
            "deleted-extensions" => agg::deleted_extensions(f.has_previous),
            "cross-version-analysis" => agg::cross_version_analysis(&npm, f.new_format, false),
            "cross-version-analysis-inline" => {
                agg::cross_version_analysis(&npm, f.new_format, true)
            }
            "valueset-list" => agg::valueset_list(ctx, &f.ig_version),
            "codesystem-list" => {
                let versions = agg::codesystem_list_versions_flag(ctx, &f.ig_version);
                agg::codesystem_list(ctx, versions)
            }
            "summary-extensions" => agg::summary_extensions(ctx),
            "summary-observations" => {
                let txcache =
                    crate::fstxcache::FsTxCache::new(f.txcache_dir.as_deref(), ctx);
                agg::summary_observations(ctx, &txcache, &f.singleton_core_path)
            }
            "deprecated-list" => agg::deprecated_list(ctx, &f.singleton_core_path),
            "expansion-params" => agg::expansion_params(f.has_expansion_params),
            "canonical-index" => {
                agg::canonical_index(ctx, f.ig_resource.clone(), f.oid_map.as_ref())
            }
            "ip-statements" => {
                format!(
                    "{}<!--$$1$$-->",
                    crate::ipstmt::ip_statements(ctx, &f.ig_json)
                )
            }
            "dependency-table" => format!(
                "{}<!--$$3$$-->",
                crate::deptable::dependency_table(
                    &f.cache_dir, &f.ig_json, &f.loaded_set, &f.dep_dst_folder, true, &self.run_uuid
                )
            ),
            "dependency-table-short" => format!(
                "{}<!--$$3$$-->",
                crate::deptable::dependency_table(
                    &f.cache_dir, &f.ig_json, &f.loaded_set, &f.dep_dst_folder, false, &self.run_uuid
                )
            ),
            "dependency-table-nontech" => format!(
                "{}<!--$$3$$-->",
                crate::deptable::dependency_table_nontech(&f.cache_dir, &f.ig_json, &f.loaded_set)
            ),
            "valueset-ref-list" => agg::valueset_ref_list(ctx, &f.ig_version, false),
            "valueset-ref-all-list" => agg::valueset_ref_list(ctx, &f.ig_version, true),
            "codesystem-ref-list" => {
                let versions =
                    crate::xreflist::used_vs_needs_version(ctx, &f.ig_version, true);
                agg::codesystem_ref_list(ctx, versions, false)
            }
            "codesystem-ref-all-list" => {
                let versions =
                    crate::xreflist::used_vs_needs_version(ctx, &f.ig_version, true);
                agg::codesystem_ref_list(ctx, versions, true)
            }
            _ => panic!("unregistered singleton kind {}", kind),
        }
    }
}

fn tbl(
    mut cfg: crate::table::TableConfig,
    active_tables: bool,
    sd: &Sd,
    ctx: &IgContext,
    def_file: &str,
) -> String {
    cfg.active_tables = active_tables;
    let (b, _gaps) = crate::table::render_table(sd, ctx, def_file, &cfg);
    b
}

/// corePath for the CanonicalRenderer leaf methods = getSpecUrl(igVersion)+"/".
fn core_path_for(sd: &Sd) -> String {
    let v = sd.fhir_version();
    let base = if v.starts_with("4.0") {
        "http://hl7.org/fhir/R4"
    } else if v.starts_with("4.3") {
        "http://hl7.org/fhir/R4B"
    } else if v.starts_with("5.0") {
        "http://hl7.org/fhir/R5"
    } else if v.starts_with("3.0") {
        "http://hl7.org/fhir/STU3"
    } else {
        "http://hl7.org/fhir"
    };
    format!("{}/", base)
}

fn panic_msg(e: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}
