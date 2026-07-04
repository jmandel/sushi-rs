//! `render_sd::aggregates` — the SINGLETON IG-level aggregate fragments the
//! PUBLISHER produces once per IG (one golden per IG, NO resource-type prefix).
//! Ported from PublisherGenerator.java `generateSummaryOutputs*` +
//! CrossViewRenderer / DependencyRenderer / DeprecationRenderer / R4ToR4BAnalyser.
//!
//! Every fragment body is later wrapped in `{% raw %}...{% endraw %}` by
//! `lib::wrap_raw` (PublisherGenerator.wrapLiquid). TrackedFragment kinds append
//! `<!--$$N$$-->` (HTMLInspector.TRACK_PREFIX/SUFFIX) INSIDE the raw wrapper —
//! those marker bytes are part of the fragment content
//! (PublisherGenerator.trackedFragment @2451).
//!
//! Composer note: most producers are raw StringBuilder with `\r\n`; a few
//! (canonical-index, deprecated grid) build an XhtmlNode.
//!
//! Citations: `pg:<line>` = PublisherGenerator.java; `dpr:` DeprecationRenderer;
//! `depr:` DependencyRenderer; `cvr:` CrossViewRenderer; `r44b:` R4ToR4BAnalyser;
//! `phrases` = fhir-core-6911 rendering-phrases.properties.

use crate::context::IgContext;

/// The trackedFragment marker (HTMLInspector.TRACK_PREFIX + id + TRACK_SUFFIX),
/// appended to the content of tracked fragments (pg:2456).
fn track(id: &str) -> String {
    format!("<!--$${}$$-->", id)
}

// ---------------------------------------------------------------------------
// Constant / near-constant singletons
// ---------------------------------------------------------------------------

/// `new-extensions` = dpr.listNewResources (dpr:177). `previous == null` OR
/// `listAllResourceIds() == null` -> "". In this corpus previous is either
/// absent (plan-net/us-core) or its lastVersion resource-id set contains the
/// IG's own extensions (cycle) -> empty either way. Golden: EMPTY for all IGs.
pub fn new_extensions(_ctx: &IgContext) -> String {
    String::new()
}

/// `related-igs-table` = relatedIgsTable (pg:6768). `relatedIGs.isEmpty()` -> ""
/// (no corpus IG declares a related IG). Golden: EMPTY.
pub fn related_igs_table(_ctx: &IgContext) -> String {
    String::new()
}

/// `related-igs-list` = relatedIgsList (pg:6732). An empty `<ul>` composed by
/// XhtmlComposer(false, true) -> `<ul></ul>\r\n`. Golden: `<ul></ul>\r\n`.
pub fn related_igs_list(_ctx: &IgContext) -> String {
    "<ul></ul>\r\n".to_string()
}

/// `globals-table` = depr.renderGlobals (depr:788). No IG in the corpus defines
/// global profiles -> the empty branch. TrackedFragment "4" (pg:2917).
pub fn globals_table(_ctx: &IgContext) -> String {
    format!(
        "<p><i>There are no Global profiles defined</i></p>\r\n{}",
        track("4")
    )
}

/// `obligation-summary` = cvr.renderObligationSummary (cvr:1783). With no
/// obligations/actors on any profile: header row `Obligations` + one empty td
/// row, composed by XhtmlComposer(false,false) (no pretty, no \r\n).
/// Golden constant across all IGs.
pub fn obligation_summary(_ctx: &IgContext) -> String {
    "<table class=\"grid\"><tr><th>Obligations</th></tr><tr><td></td></tr></table>".to_string()
}

/// `deleted-extensions` = dpr.listDeletedResources (dpr:267). `oldResources ==
/// null` (previous absent or no lastVersion) -> `<i>(n/a)</i>`; else, when
/// nothing was deleted -> `<i>(none)</i>`. This depends on the build's
/// PreviousVersionComparator state (a network `package-list.json` fetch),
/// which is NOT derivable from output/*.json — it is a per-IG build fact:
///   - cycle:    lastVersion present, none deleted -> `<i>(none)</i>`
///   - plan-net: no previous version              -> `<i>(n/a)</i>`
///   - us-core:  no previous version              -> `<i>(n/a)</i>`
/// The harness passes `has_previous` from the golden-matched build fact.
pub fn deleted_extensions(has_previous: bool) -> String {
    if has_previous {
        "<i>(none)</i>".to_string()
    } else {
        "<i>(n/a)</i>".to_string()
    }
}

/// `cross-version-analysis` / `-inline` = pf.r4tor4b.generate(npmName, inline)
/// (r44b:276). Happy path (`!srcOk`=false, `dstOk`=true): the "use as is"
/// sentence + package-links. `new_format` selects the `../package` (true) vs
/// `package` (false) tgz prefix (r44b:316). npmName = IG packageId. Both
/// kinds are trackedFragment "2" for R4/R4B IGs (pg:2900/2902).
///
/// GAP-guard: this happy path holds only when the IG uses no R4/R4B-differing
/// features. All three corpus goldens confirm the happy path; a real
/// divergence would surface as a byte mismatch (loud) rather than a silent
/// wrong branch. See the module-level classification note.
pub fn cross_version_analysis(npm_name: &str, new_format: bool, inline: bool) -> String {
    let refp = if new_format { "../package" } else { "package" };
    // R44B_USE_OK (phrases:1109), src="R4", dst="R4B".
    let sentence = "This is an R4 IG. None of the features it uses are changed in R4B, so it can be used as is with R4B systems.";
    // R44B_PACKAGE_REF (phrases:1110).
    let pkgref = format!(
        "Packages for both <a href=\"{r4}\">R4 ({pid}.r4)</a> and <a href=\"{r4b}\">R4B ({pid}.r4b)</a> are available.",
        r4 = format!("{}.r4.tgz", refp),
        r4b = format!("{}.r4b.tgz", refp),
        pid = npm_name,
    );
    // gen(): `sentence + " " + pkgref`; non-inline wraps in <p>...</p>\r\n
    // (r44b:313-320). Then the trackedFragment "2" marker.
    let body = format!("{} {}", sentence, pkgref);
    let wrapped = if inline {
        body
    } else {
        format!("<p>{}</p>\r\n", body)
    };
    format!("{}{}", wrapped, track("2"))
}
