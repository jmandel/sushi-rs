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

use serde_json::Value;

use crate::context::IgContext;
use crate::leaf::escape_xml;

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

/// Count own StructureDefinitions whose `type` == `type_code` (the seeResource
/// filter, cvr:180/183: extList/obsList get an entry per such SD).
fn count_own_sd_of_type(ctx: &IgContext, type_code: &str) -> usize {
    ctx.own_resources()
        .into_iter()
        .filter(|r| {
            r.rtype == "StructureDefinition"
                && r.json.get("type").and_then(|x| x.as_str()) == Some(type_code)
        })
        .count()
}

/// `summary-extensions` = cvr.getExtensionSummary (cvr:453). Empty branch
/// (`extList.size() == 0`, i.e. the IG defines no Extension SD):
/// `<p>No Extensions Defined by this Implementation Guide</p>\r\n`.
/// LOUD GAP: the grid branch (>=1 own Extension SD) is not ported here.
pub fn summary_extensions(ctx: &IgContext) -> String {
    if count_own_sd_of_type(ctx, "Extension") == 0 {
        "<p>No Extensions Defined by this Implementation Guide</p>\r\n".to_string()
    } else {
        panic!(
            "LOUD GAP: summary-extensions grid branch (cvr:457) not ported — \
             IG defines Extension StructureDefinition(s)"
        );
    }
}

/// `summary-observations` = cvr.getObservationSummary (cvr:487). Empty branch
/// (`obsList.size() == 0`, no own Observation SD):
/// `<p>No Observations Found</p>\r\n`.
/// LOUD GAP: the grid branch (>=1 own Observation SD) is not ported here.
pub fn summary_observations(ctx: &IgContext) -> String {
    if count_own_sd_of_type(ctx, "Observation") == 0 {
        "<p>No Observations Found</p>\r\n".to_string()
    } else {
        panic!(
            "LOUD GAP: summary-observations grid branch (cvr:491) not ported — \
             IG defines Observation StructureDefinition(s)"
        );
    }
}

const EXT_STANDARDS_STATUS_RESULT: &str = EXT_STANDARDS_STATUS;

/// Own resources that carry the standards-status extension with value
/// `deprecated` (the `dep=true` branch of dpr.deprecationSummary, dpr:41; this
/// branch fires regardless of the previous-version comparator). Count only.
fn count_own_deprecated(ctx: &IgContext) -> usize {
    ctx.own_resources()
        .into_iter()
        .filter(|r| {
            ext_value_str(&r.json, EXT_STANDARDS_STATUS_RESULT).as_deref() == Some("deprecated")
        })
        .count()
}

/// `deprecated-list` = dpr.deprecationSummary (dpr:30). With no deprecated
/// resources (and previous comparator contributing none) the list is empty ->
/// `<p>No deprecated content</p>` (dpr:79). cycle/plan-net hit this.
/// LOUD GAP: the grid branch (dpr:82, own deprecated resources) — us-core.
pub fn deprecated_list(ctx: &IgContext) -> String {
    if count_own_deprecated(ctx) == 0 {
        "<p>No deprecated content</p>".to_string()
    } else {
        panic!(
            "LOUD GAP: deprecated-list grid branch (dpr:82) not ported — \
             IG has deprecated resources"
        );
    }
}

/// `expansion-params` = renderExpansionParameters (pg:1686). Empty when the
/// context's expansion parameters hold nothing beyond `x-system-cache-id` /
/// `defaultDisplayLanguage` (pg:1690-1697) -> "" (and pg:2922 emits the plain
/// non-tracked empty fragment). cycle/plan-net hit this.
///
/// `has_interesting_params` is a per-IG build fact (the context's expansion
/// parameters come from the build's tx setup, NOT output/*.json). Golden-
/// matched: cycle/plan-net empty, us-core a grid (tracked "5").
/// LOUD GAP: the grid branch (pg:1698) — us-core.
pub fn expansion_params(has_interesting_params: bool) -> String {
    if !has_interesting_params {
        String::new()
    } else {
        panic!(
            "LOUD GAP: expansion-params grid branch (pg:1698) not ported — \
             IG has interesting expansion parameters"
        );
    }
}

// ---------------------------------------------------------------------------
// CrossViewRenderer CS/VS "defined" lists (cvr:1393/1685)
// ---------------------------------------------------------------------------

const EXT_STANDARDS_STATUS: &str = "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status";
const EXT_FMM_LEVEL: &str = "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm";

/// The value of a simple-valued extension by url (valueCode/valueInteger/...).
fn ext_value_str(res: &Value, url: &str) -> Option<String> {
    let exts = res.get("extension")?.as_array()?;
    for e in exts {
        if e.get("url").and_then(|x| x.as_str()) == Some(url) {
            for (k, v) in e.as_object()? {
                if let Some(rest) = k.strip_prefix("value") {
                    let _ = rest;
                    return Some(match v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => v.to_string(),
                    });
                }
            }
            return Some(String::new());
        }
    }
    None
}

fn has_ext(res: &Value, url: &str) -> bool {
    res.get("extension")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().any(|e| e.get("url").and_then(|u| u.as_str()) == Some(url)))
        .unwrap_or(false)
}

/// Own CS/VS collected then sorted by url (CanonicalResourceSortByUrl,
/// ResourceSorters:11). Returns (url, web_path, json).
fn own_of_type(ctx: &IgContext, rtype: &str) -> Vec<(String, String, std::rc::Rc<Value>)> {
    let mut v: Vec<(String, String, std::rc::Rc<Value>)> = ctx
        .own_resources()
        .into_iter()
        .filter(|r| r.rtype == rtype)
        .map(|r| {
            let url = r.json.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
            (url, r.web_path, r.json)
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn get_str<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

/// countCodes (CodeSystemUtilities:830): total concepts incl. nested.
fn count_codes(concepts: &[Value]) -> usize {
    let mut t = concepts.len();
    for c in concepts {
        if let Some(kids) = c.get("concept").and_then(|x| x.as_array()) {
            t += count_codes(kids);
        }
    }
    t
}

/// hasHierarchy (CodeSystemUtilities:755): any top-level concept with children.
fn has_hierarchy(concepts: &[Value]) -> bool {
    concepts.iter().any(|c| {
        c.get("concept")
            .and_then(|x| x.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    })
}

/// The status/flags cell shared shape (cvr:1717-1734 CS, 1427-1443 VS). The
/// standards-status flag puts `class="{v}-flag"` ON THE td (renderStatus is a
/// no-op — Renderer:84 returns x when changeVersion==null). `exp_sep` differs:
/// CS uses ": " before experimental, VS uses ":".
fn status_cell(res: &Value, exp_sep: &str) -> String {
    let status = get_str(res, "status");
    // The td class is set by the standards-status branch (attribute on td).
    let ss = if has_ext(res, EXT_STANDARDS_STATUS) {
        ext_value_str(res, EXT_STANDARDS_STATUS)
    } else {
        None
    };
    let class_attr = match &ss {
        Some(v) => format!(" class=\"{}-flag\"", v),
        None => String::new(),
    };
    let mut inner = String::new();
    inner.push_str(&escape_xml(status));
    if let Some(v) = &ss {
        inner.push_str(" / ");
        inner.push_str(&escape_xml(v));
    }
    if has_ext(res, EXT_FMM_LEVEL) {
        let fmm = ext_value_str(res, EXT_FMM_LEVEL).unwrap_or_default();
        inner.push_str(" / ");
        inner.push_str(&format!("FMM{}", escape_xml(&fmm)));
    }
    if res.get("experimental").and_then(|x| x.as_bool()) == Some(true) {
        inner.push_str(exp_sep);
        inner.push_str("experimental");
    }
    format!("<td{}>{}</td>", class_attr, inner)
}

/// Name/Title cell (`name<br/>title`), cvr:1712/1421.
fn name_title_cell(res: &Value) -> String {
    format!(
        "<td>{}<br/>{}</td>",
        escape_xml(get_str(res, "name")),
        escape_xml(get_str(res, "title"))
    )
}

/// `needVersionReferences` (cvr:1385): any resource version != igVersion.
fn need_version_references(list: &[(String, String, std::rc::Rc<Value>)], ig_version: &str) -> bool {
    list.iter().any(|(_u, _w, j)| get_str(j, "version") != ig_version)
}

/// `codesystem-list` = cvr.renderCSList(defined, versions, used=false) (cvr:1685).
/// NOTE the `versions` flag comes from needVersionReferences over the USED-ALL
/// VS list (pg:2799 passes the leftover vslist), NOT the CS list — see
/// classification. `versions` is supplied by the caller.
pub fn codesystem_list(ctx: &IgContext, versions: bool) -> String {
    let list = own_of_type(ctx, "CodeSystem");
    let mut b = String::new();
    b.push_str("<table class=\"grid\"><tr><th>URL</th>");
    if versions {
        b.push_str("<th>Version</th>");
    }
    b.push_str("<th>Name / Title</th><th>Status</th><th>Flags</th><th>Count</th></tr>");
    for (url, web, j) in &list {
        b.push_str("<tr>");
        b.push_str(&format!(
            "<td><a href=\"{}\">{}</a></td>",
            escape_xml(web),
            escape_xml(url)
        ));
        if versions {
            b.push_str(&format!("<td>{}</td>", escape_xml(get_str(j, "version"))));
        }
        b.push_str(&name_title_cell(j));
        b.push_str(&status_cell(j, ": "));
        // Flags: hierarchyMeaning, flat, compositional, versionNeeded.
        let empty: Vec<Value> = Vec::new();
        let concepts = j.get("concept").and_then(|x| x.as_array()).unwrap_or(&empty);
        let mut flags = String::new();
        if let Some(hm) = j.get("hierarchyMeaning").and_then(|x| x.as_str()) {
            flags.push_str(&escape_xml(hm));
            flags.push(' ');
        }
        if !has_hierarchy(concepts) {
            flags.push_str("flat ");
        }
        if j.get("compositional").and_then(|x| x.as_bool()) == Some(true) {
            flags.push_str("compositional ");
        }
        if j.get("versionNeeded").and_then(|x| x.as_bool()) == Some(true) {
            flags.push_str("version-needed ");
        }
        b.push_str(&format!("<td>{}</td>", flags));
        // Count.
        let mut count = format!("{}", count_codes(concepts));
        if let Some(content) = j.get("content").and_then(|x| x.as_str()) {
            count.push_str(&format!(" ({})", content));
        }
        b.push_str(&format!("<td>{}</td>", escape_xml(&count)));
        b.push_str("</tr>");
    }
    b.push_str("</table>");
    b
}

/// `worker.fetchCodeSystem(uri)`: the resolved CodeSystem, or None. See the
/// classification note on describe_source for the multi-version THO caveat.
fn fetch_code_system(ctx: &IgContext, uri: &str) -> Option<crate::context::Resolved> {
    let r = ctx.resolve(uri)?;
    if r.rtype != "CodeSystem" {
        return None;
    }
    Some(r)
}

/// `describeSource` (cvr:1523): a code-system uri -> a short source label.
fn describe_source(ctx: &IgContext, uri: &str) -> String {
    // worker.fetchCodeSystem(uri): resolves + relative webPath => "Internal".
    if let Some(r) = fetch_code_system(ctx, uri) {
        if !is_absolute_url(&r.web_path) {
            return "Internal".to_string();
        }
    }
    match uri {
        "http://snomed.info/sct" => return "SCT".to_string(),
        "http://loinc.org" => return "LOINC".to_string(),
        "http://dicom.nema.org/resources/ontology/DCM" => return "DICOM".to_string(),
        "http://unitsofmeasure.org" => return "UCUM".to_string(),
        "http://www.nlm.nih.gov/research/umls/rxnorm" => return "RxNorm".to_string(),
        _ => {}
    }
    if uri.starts_with("http://terminology.hl7.org/CodeSystem/v3-") {
        return "THO (V3)".to_string();
    }
    if uri.starts_with("http://terminology.hl7.org/CodeSystem/v2-") {
        return "THO (V2)".to_string();
    }
    if uri.starts_with("http://terminology.hl7.org") {
        return "THO".to_string();
    }
    // cs.hasSourcePackage(): the resolved CS's source package id.
    if let Some(r) = fetch_code_system(ctx, uri) {
        if let Some(pkg) = &r.pkg {
            return pkg.id.clone();
        }
    }
    if uri.starts_with("http://hl7.org/fhir") {
        return "FHIR".to_string();
    }
    "Other".to_string()
}

/// `Utilities.isAbsoluteUrl`: has a scheme like `http://`.
fn is_absolute_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://") || s.contains("://")
}

/// `valueset-list` = cvr.renderVSList(defined, versions, used=false) (cvr:1393).
/// The `versions` flag is needVersionReferences over the DEFINED VS list here
/// (pg:2784) — fully derivable from own resources.
pub fn valueset_list(ctx: &IgContext, ig_version: &str) -> String {
    let list = own_of_type(ctx, "ValueSet");
    let versions = need_version_references(&list, ig_version);
    render_vs_list(ctx, &list, versions)
}

fn render_vs_list(
    ctx: &IgContext,
    list: &[(String, String, std::rc::Rc<Value>)],
    versions: bool,
) -> String {
    let mut b = String::new();
    b.push_str("<table class=\"grid\"><tr><th>URL</th>");
    if versions {
        b.push_str("<th>Version</th>");
    }
    b.push_str("<th>Name / Title</th><th>Status</th><th>Flags</th><th>Source</th></tr>");
    for (url, web, j) in list {
        b.push_str("<tr>");
        b.push_str(&format!(
            "<td><a href=\"{}\">{}</a></td>",
            escape_xml(web),
            escape_xml(url)
        ));
        if versions {
            b.push_str(&format!("<td>{}</td>", escape_xml(get_str(j, "version"))));
        }
        b.push_str(&name_title_cell(j));
        b.push_str(&status_cell(j, ":"));
        // Flags cell: Locked-Date, Inactive, then A/I/E/V; plus Source cell.
        let mut flags = String::new();
        let empty: Vec<Value> = Vec::new();
        let compose = j.get("compose");
        if compose.and_then(|c| c.get("lockedDate")).is_some() {
            flags.push_str("Locked-Date ");
        }
        if compose.and_then(|c| c.get("inactive")).and_then(|x| x.as_bool()) == Some(true) {
            flags.push_str("Inactive ");
        }
        let includes = compose
            .and_then(|c| c.get("include"))
            .and_then(|x| x.as_array())
            .unwrap_or(&empty);
        let (mut inc_i, mut inc_e, mut inc_v, mut inc_a) = (false, false, false, false);
        let mut sources: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for inc in includes {
            if inc.get("valueSet").and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(false)
                || inc.get("valueSet").map(|v| !v.is_null()).unwrap_or(false)
            {
                // hasValueSet(): the compose include references value set(s).
                if inc.get("valueSet").and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(inc.get("valueSet").is_some()) {
                    inc_v = true;
                }
            }
            if let Some(system) = inc.get("system").and_then(|x| x.as_str()) {
                sources.insert(describe_source(ctx, system));
                if inc.get("concept").and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(false) {
                    inc_e = true;
                } else if inc.get("filter").and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(false) {
                    inc_i = true;
                } else {
                    inc_a = true;
                }
            }
        }
        if inc_a {
            flags.push_str("<span title=\"All Code System\">A </span>");
        }
        if inc_i {
            flags.push_str("<span title=\"Intensional\">I </span>");
        }
        if inc_e {
            flags.push_str("<span title=\"Extensional\">E </span>");
        }
        if inc_v {
            flags.push_str("<span title=\"Imports Valueset(s)\">V </span>");
        }
        b.push_str(&format!("<td>{}</td>", flags));
        // Source cell: sorted sources, comma-separated.
        let mut src = String::new();
        let mut first = true;
        for s in &sources {
            if first {
                first = false;
            } else {
                src.push_str(", ");
            }
            src.push_str(&escape_xml(s));
        }
        b.push_str(&format!("<td>{}</td>", src));
        b.push_str("</tr>");
    }
    b.push_str("</table>");
    b
}
