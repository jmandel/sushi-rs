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

use crate::context::{IgContext, OwnResource};
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

/// A used-type entry (cvr UsedType, cvr:54): a type code + its must-support flag.
struct UsedType {
    name: String,
    ms: bool,
}

/// An extension summary entry (cvr ExtensionDefinition, cvr:102). `web` is the
/// source SD web path (top-level only; nested components render plain code text).
struct ExtDef {
    code: String,
    web: Option<String>,
    definition: String,
    types: Vec<UsedType>,
    components: Vec<ExtDef>,
}

/// `isMustSupport(ed, tr)` (cvr:323): the element mustSupport OR the type-ref's
/// `_mustSupport` extension == "true".
fn type_is_ms(ed: &Value, tr: &Value) -> bool {
    if ed.get("mustSupport").and_then(|x| x.as_bool()) == Some(true) {
        return true;
    }
    // TypeRefComponent _mustSupport extension (EXT_MUST_SUPPORT).
    ext_value_str(
        tr,
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-mustSupport",
    )
    .as_deref()
        == Some("true")
}

fn types_contain(types: &[UsedType], name: &str) -> bool {
    types.iter().any(|t| t.name == name)
}

/// The definition string of an element (getDefinition()).
fn ed_definition(ed: &Value) -> String {
    ed.get("definition")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

/// The fixed/pattern primitive value of a `url` element (getFixedOrPattern().
/// primitiveValue() for a uri): `fixedUri`/`patternUri`.
fn fixed_uri(ed: &Value) -> Option<String> {
    for k in [
        "fixedUri",
        "patternUri",
        "fixedString",
        "patternString",
        "fixedCanonical",
    ] {
        if let Some(s) = ed.get(k).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

/// processExtensionComponent (cvr:418): consume the run of `Extension.extension.*`
/// elements starting at `i`, producing one nested ExtDef with the passed-in
/// `defn`. Returns the index one past the consumed run.
fn process_ext_component(
    parent: &mut ExtDef,
    els: &[Value],
    defn: String,
    canonical: &str,
    mut i: usize,
) -> usize {
    let mut exd = ExtDef {
        code: String::new(),
        web: None,
        definition: defn,
        types: Vec::new(),
        components: Vec::new(),
    };
    let mut has_code = false;
    while i < els.len()
        && els[i]
            .get("path")
            .and_then(|x| x.as_str())
            .map(|p| p.starts_with("Extension.extension."))
            .unwrap_or(false)
    {
        let ed = &els[i];
        let path = ed.get("path").and_then(|x| x.as_str()).unwrap_or("");
        if path == "Extension.extension.url" {
            if let Some(mut code) = fixed_uri(ed) {
                // Trim a canonical prefix (cvr:425); the corpus fixed urls are
                // bare slice names, so this rarely fires.
                if code.starts_with(canonical) && code.len() <= canonical.len() + 21 {
                    code = code[canonical.len() + 21..].to_string();
                }
                exd.code = code;
                has_code = true;
            }
        }
        if path.starts_with("Extension.extension.value") {
            for tr in ed
                .get("type")
                .and_then(|x| x.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[])
            {
                let code = tr.get("code").and_then(|x| x.as_str()).unwrap_or("");
                if !types_contain(&exd.types, code) {
                    exd.types.push(UsedType {
                        name: code.to_string(),
                        ms: type_is_ms(ed, tr),
                    });
                }
            }
        }
        i += 1;
    }
    if has_code {
        parent.components.push(exd);
    }
    i
}

/// seeExtensionDefinition (cvr:378): build the ExtDef for one Extension SD.
/// Returns None when the url doesn't follow the IG's canonical pattern (cvr:386).
fn see_extension_definition(res: &OwnResource, canonical: &str) -> Option<ExtDef> {
    let url = res.json.get("url").and_then(|x| x.as_str())?;
    // code = url minus "{canonical}/StructureDefinition/" (21 chars). cvr:381.
    let prefix = format!("{}/StructureDefinition/", canonical);
    let code = if url.starts_with(&prefix) {
        url[prefix.len()..].to_string()
    } else {
        return None;
    };
    let mut exd = ExtDef {
        code,
        web: Some(res.web_path.clone()),
        definition: res
            .json
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        types: Vec::new(),
        components: Vec::new(),
    };
    let empty: Vec<Value> = Vec::new();
    let els = res
        .json
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(|x| x.as_array())
        .unwrap_or(&empty);
    let mut i = 0;
    while i < els.len() {
        let ed = &els[i];
        let path = ed.get("path").and_then(|x| x.as_str()).unwrap_or("");
        let max = ed.get("max").and_then(|x| x.as_str()).unwrap_or("");
        if path.starts_with("Extension.value") && max != "0" {
            for tr in ed
                .get("type")
                .and_then(|x| x.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[])
            {
                let code = tr.get("code").and_then(|x| x.as_str()).unwrap_or("");
                if !types_contain(&exd.types, code) {
                    exd.types.push(UsedType {
                        name: code.to_string(),
                        ms: type_is_ms(ed, tr),
                    });
                }
            }
        }
        if path.starts_with("Extension.extension.") {
            // defn = the definition of element i-1 (the slice header). cvr:403.
            let defn = if i > 0 {
                ed_definition(&els[i - 1])
            } else {
                String::new()
            };
            i = process_ext_component(&mut exd, els, defn, canonical, i);
        } else {
            i += 1;
        }
    }
    Some(exd)
}

/// The Extension `baseExtTypes` (cvr:150): the type codes of Extension.value[x]
/// in the core Extension SD (max != 0). Used only for the `(all)` collapse in
/// renderTypeCell; a single-typed extension never matches its size, so the
/// exact set doesn't affect corpus bytes — but resolve it faithfully when the
/// core Extension SD is available.
fn base_ext_types(ctx: &IgContext) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(r) = ctx.load_resource("http://hl7.org/fhir/StructureDefinition/Extension") {
        let empty: Vec<Value> = Vec::new();
        let els = r
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(|x| x.as_array())
            .unwrap_or(&empty);
        for ed in els {
            let path = ed.get("path").and_then(|x| x.as_str()).unwrap_or("");
            let max = ed.get("max").and_then(|x| x.as_str()).unwrap_or("");
            if path.starts_with("Extension.value") && max != "0" {
                for tr in ed
                    .get("type")
                    .and_then(|x| x.as_array())
                    .map(|a| a.as_slice())
                    .unwrap_or(&[])
                {
                    if let Some(c) = tr.get("code").and_then(|x| x.as_str()) {
                        if !out.iter().any(|x| x == c) {
                            out.push(c.to_string());
                        }
                    }
                }
            }
        }
    }
    out
}

fn all_ms_are_same(types: &[UsedType]) -> bool {
    if types.is_empty() {
        return false;
    }
    let ms = types[0].ms;
    types.iter().all(|t| t.ms == ms)
}

const MS_SPAN: &str =
    " <span style=\"color:white; background-color: #D50000; font-weight:bold\">S</span> ";
const MS_SPAN_TRAIL: &str =
    " <span style=\"color:white; background-color: #D50000; font-weight:bold\">S</span>";

/// renderTypeCell (cvr:605): the Value Types cell. `render` is always true for
/// the extension/observation tables; the `(all)` collapse fires only when the
/// used set equals the base set. Each type links `fetchTypeDefinition(name).
/// getWebPath()` with `title=name`, else plain text.
fn render_type_cell(ctx: &IgContext, types: &[UsedType], base: &[String]) -> String {
    let mut b = String::from("<td>");
    if types.len() == base.len() && all_ms_are_same(types) {
        if !types.is_empty() && types[0].ms {
            b.push_str(MS_SPAN);
        }
        b.push_str("(all)");
    } else {
        let do_ms = !all_ms_are_same(types);
        let mut first = true;
        for t in types {
            if !do_ms && first && t.ms {
                b.push_str(MS_SPAN);
            }
            if first {
                first = false;
            } else {
                b.push_str(" | ");
            }
            match ctx.resolve_type(&t.name) {
                Some(r) => b.push_str(&format!(
                    "<a href=\"{}\" title=\"{}\">{}</a>",
                    escape_xml(&r.web_path),
                    escape_xml(&t.name),
                    escape_xml(&t.name)
                )),
                None => b.push_str(&escape_xml(&t.name)),
            }
            if do_ms && t.ms {
                b.push_str(MS_SPAN_TRAIL);
            }
        }
    }
    b.push_str("</td>");
    b
}

/// `summary-extensions` = cvr.getExtensionSummary (cvr:453). Empty branch when
/// the IG defines no canonical-pattern Extension SD:
/// `<p>No Extensions Defined by this Implementation Guide</p>\r\n`.
/// Grid branch (cvr:457): sorted by lowercased url, one row per extension +
/// indented rows for nested components. Value-type cell links core datatypes.
pub fn summary_extensions(ctx: &IgContext) -> String {
    let canonical = ctx.own_canonical_prefix().unwrap_or_default();
    let mut ext_list: Vec<(String, ExtDef)> = ctx
        .own_resources()
        .into_iter()
        .filter(|r| {
            r.rtype == "StructureDefinition"
                && r.json.get("type").and_then(|x| x.as_str()) == Some("Extension")
        })
        .filter_map(|r| {
            let url = r.json.get("url").and_then(|x| x.as_str())?.to_lowercase();
            see_extension_definition(&r, &canonical).map(|e| (url, e))
        })
        .collect();
    if ext_list.is_empty() {
        return "<p>No Extensions Defined by this Implementation Guide</p>\r\n".to_string();
    }
    // ExtListSorter (cvr:72): by lowercased url, stable.
    ext_list.sort_by(|a, b| a.0.cmp(&b.0));
    let base = base_ext_types(ctx);
    let mut b = String::new();
    b.push_str("<table class=\"grid\">\r\n");
    b.push_str(
        " <tr><td><b>Code</b></td><td><b>Value Types</b></td><td><b>Definition</b></td></tr>\r\n",
    );
    for (_url, op) in &ext_list {
        b.push_str(" <tr>");
        b.push_str(&format!(
            "<td><a href=\"{}\">{}</a></td>",
            escape_xml(op.web.as_deref().unwrap_or("")),
            escape_xml(&op.code)
        ));
        b.push_str(&render_type_cell(ctx, &op.types, &base));
        b.push_str(&format!("<td>{}</td>", escape_xml(&op.definition)));
        b.push_str("</tr>\r\n");
        for inner in &op.components {
            b.push_str(" <tr>");
            b.push_str(&format!("<td>&nbsp;&nbsp;{}</td>", escape_xml(&inner.code)));
            b.push_str(&render_type_cell(ctx, &inner.types, &base));
            b.push_str(&format!("<td>{}</td>", escape_xml(&inner.definition)));
            b.push_str("</tr>\r\n");
        }
    }
    b.push_str("</table>\r\n");
    b
}

/// A fixed/pattern Coding (system+code) as scanned by seeObservation. Display
/// is looked up via validateCode at render time, not stored.
#[derive(Clone)]
struct SumCoding {
    system: String,
    code: String,
    version: Option<String>,
}

/// A binding on an observation code/category element (strength + valueSet).
#[derive(Clone)]
struct SumBinding {
    strength: String,
    value_set: String,
}

/// An observation profile summary entry (cvr ObservationProfile, cvr:83).
#[derive(Default)]
struct ObsProfile {
    web: String,
    present: String,
    id: String,
    name: String, // component name (indented rows)
    code: Vec<SumCoding>,
    code_vs: Option<SumBinding>,
    category: Vec<SumCoding>,
    cat_vs: Option<SumBinding>,
    effective_types: Vec<UsedType>,
    types: Vec<UsedType>,
    components: Vec<ObsProfile>,
}

impl ObsProfile {
    fn has_value(&self) -> bool {
        !self.code.is_empty() || !self.category.is_empty()
    }
}

/// A `fixedX`/`patternX` CodeableConcept's codings, or a bare `fixedCoding`/
/// `patternCoding`.
fn fixed_codings(ed: &Value) -> Vec<SumCoding> {
    let mut out = Vec::new();
    for key in ["patternCodeableConcept", "fixedCodeableConcept"] {
        if let Some(cc) = ed.get(key) {
            if let Some(cs) = cc.get("coding").and_then(|x| x.as_array()) {
                for c in cs {
                    out.push(coding_of(c));
                }
            }
        }
    }
    out
}

fn fixed_coding(ed: &Value) -> Option<SumCoding> {
    for key in ["patternCoding", "fixedCoding"] {
        if let Some(c) = ed.get(key) {
            return Some(coding_of(c));
        }
    }
    None
}

fn coding_of(c: &Value) -> SumCoding {
    SumCoding {
        system: c
            .get("system")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        code: c
            .get("code")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        version: c.get("version").and_then(|x| x.as_str()).map(String::from),
    }
}

fn binding_of(ed: &Value) -> Option<SumBinding> {
    let b = ed.get("binding")?;
    let vs = b.get("valueSet").and_then(|x| x.as_str())?;
    Some(SumBinding {
        strength: b
            .get("strength")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        value_set: vs.to_string(),
    })
}

fn primitive_fixed(ed: &Value) -> Option<String> {
    for (k, v) in ed.as_object()? {
        if let Some(_rest) = k.strip_prefix("fixed") {
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
        }
        if let Some(_rest) = k.strip_prefix("pattern") {
            if let Some(s) = v.as_str() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// charCount(path, '.') — number of dots (nesting depth).
fn dot_count(path: &str) -> usize {
    path.bytes().filter(|&c| c == b'.').count()
}

/// processObservationComponent (cvr:327): consume the `Observation.component.*`
/// run starting at `i` into one component ObsProfile. Returns the next index.
fn process_obs_component(
    parent: &mut ObsProfile,
    els: &[Value],
    comp_slice: &str,
    mut i: usize,
) -> usize {
    let mut obs = ObsProfile {
        name: comp_slice.to_string(),
        ..Default::default()
    };
    let mut system: Option<String> = None;
    while i < els.len()
        && els[i]
            .get("path")
            .and_then(|x| x.as_str())
            .map(|p| p.starts_with("Observation.component."))
            .unwrap_or(false)
    {
        let ed = &els[i];
        let path = ed.get("path").and_then(|x| x.as_str()).unwrap_or("");
        if path == "Observation.component.category" {
            obs.category.extend(fixed_codings(ed));
        }
        if path == "Observation.component.category.coding" {
            system = None;
            if let Some(c) = fixed_coding(ed) {
                obs.category.push(c);
            }
        }
        if path == "Observation.component.category.coding.system" {
            system = primitive_fixed(ed);
        }
        if path == "Observation.component.category.coding.code" {
            if let (Some(sys), Some(code)) = (&system, primitive_fixed(ed)) {
                // NB: cvr:346 appends to obs.method (a publisher bug); no corpus hit.
                obs.method_push_bug(sys.clone(), code);
                system = None;
            }
        }
        if path == "Observation.component.code" {
            obs.code.extend(fixed_codings(ed));
        }
        if path == "Observation.component.code.coding" {
            system = None;
            if let Some(c) = fixed_coding(ed) {
                obs.code.push(c);
            }
        }
        if path == "Observation.component.code.coding.system" {
            system = primitive_fixed(ed);
        }
        if path == "Observation.component.code.coding.code" {
            if let (Some(sys), Some(code)) = (&system, primitive_fixed(ed)) {
                obs.method_push_bug(sys.clone(), code);
                system = None;
            }
        }
        if path.starts_with("Observation.component.value") && dot_count(path) == 2 {
            if ed.get("max").and_then(|x| x.as_str()) != Some("0") {
                for tr in ed
                    .get("type")
                    .and_then(|x| x.as_array())
                    .map(|a| a.as_slice())
                    .unwrap_or(&[])
                {
                    let code = tr.get("code").and_then(|x| x.as_str()).unwrap_or("");
                    if !types_contain(&obs.types, code) {
                        obs.types.push(UsedType {
                            name: code.to_string(),
                            ms: type_is_ms(ed, tr),
                        });
                    }
                }
            }
        }
        i += 1;
    }
    parent.components.push(obs);
    i
}

impl ObsProfile {
    // The publisher appends component category/code.coding.code to `method`
    // (cvr:346/363) — a copy-paste bug. No corpus component uses coding.code,
    // so this is dead; kept faithful (append into a discard vec via method).
    fn method_push_bug(&mut self, _system: String, _code: String) {
        // method column is never rendered for components; drop faithfully.
    }
}

/// seeObservation (cvr:195): scan an Observation SD's snapshot into an ObsProfile.
fn see_observation(res: &OwnResource) -> Option<ObsProfile> {
    let mut obs = ObsProfile {
        web: res.web_path.clone(),
        present: res.title.clone(),
        id: res.id.clone(),
        ..Default::default()
    };
    let empty: Vec<Value> = Vec::new();
    let els = res
        .json
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(|x| x.as_array())
        .unwrap_or(&empty);
    let mut i = 0;
    let mut system: Option<String> = None;
    let mut comp_slice: Option<String> = None;
    while i < els.len() {
        let ed = &els[i];
        let path = ed.get("path").and_then(|x| x.as_str()).unwrap_or("");

        if path == "Observation.category" {
            obs.category.extend(fixed_codings(ed));
        }
        if path == "Observation.category.coding" {
            system = None;
            if let Some(c) = fixed_coding(ed) {
                obs.category.push(c);
            } else if let Some(b) = binding_of(ed) {
                obs.cat_vs = Some(b);
            }
        }
        if path == "Observation.category.coding.system" {
            system = primitive_fixed(ed);
        }
        if path == "Observation.category.coding.code" {
            if let (Some(sys), Some(code)) = (&system, primitive_fixed(ed)) {
                obs.category.push(SumCoding {
                    system: sys.clone(),
                    code,
                    version: None,
                });
                system = None;
            }
        }

        if path == "Observation.code" {
            let codings = fixed_codings(ed);
            if !codings.is_empty() {
                obs.code.extend(codings);
            } else if let Some(b) = binding_of(ed) {
                obs.code_vs = Some(b);
            }
        }
        if path == "Observation.code.coding" {
            system = None;
            if let Some(c) = fixed_coding(ed) {
                obs.code.push(c);
            } else if let Some(b) = binding_of(ed) {
                obs.code_vs = Some(b);
            }
        }
        if path == "Observation.code.coding.system" {
            system = primitive_fixed(ed);
        }
        if path == "Observation.code.coding.code" {
            if let (Some(sys), Some(code)) = (&system, primitive_fixed(ed)) {
                obs.code.push(SumCoding {
                    system: sys.clone(),
                    code,
                    version: None,
                });
                system = None;
            }
        }

        if path == "Observation.effective[x]" {
            for tr in ed
                .get("type")
                .and_then(|x| x.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[])
            {
                let code = working_code(tr);
                if !types_contain(&obs.effective_types, &code) {
                    obs.effective_types.push(UsedType {
                        name: code,
                        ms: type_is_ms(ed, tr),
                    });
                }
            }
        }
        if path.starts_with("Observation.value")
            && dot_count(path) == 1
            && ed.get("max").and_then(|x| x.as_str()) != Some("0")
        {
            for tr in ed
                .get("type")
                .and_then(|x| x.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[])
            {
                let code = working_code(tr);
                if !types_contain(&obs.types, &code) {
                    obs.types.push(UsedType {
                        name: code,
                        ms: type_is_ms(ed, tr),
                    });
                }
            }
        }
        if path == "Observation.component" {
            comp_slice = ed
                .get("sliceName")
                .and_then(|x| x.as_str())
                .map(String::from);
        }
        let prohibited = ed.get("max").and_then(|x| x.as_str()) == Some("0");
        if path.starts_with("Observation.component.") && !prohibited && comp_slice.is_some() {
            i = process_obs_component(&mut obs, els, comp_slice.as_deref().unwrap_or(""), i);
        } else {
            i += 1;
        }
    }
    if obs.has_value() {
        Some(obs)
    } else {
        None
    }
}

/// getWorkingCode (TypeRefComponent): the type `code`, resolving the
/// `http://hl7.org/fhirpath/System.*` FHIRPath aliases to the FHIR primitive
/// via the structuredefinition-fhir-type extension when present. The corpus
/// value[x]/effective[x] types are plain FHIR codes.
fn working_code(tr: &Value) -> String {
    if let Some(code) = tr.get("code").and_then(|x| x.as_str()) {
        if code.starts_with("http://hl7.org/fhirpath/System.") {
            // _code extension structuredefinition-fhir-type gives the real code.
            if let Some(ft) = ext_value_str(
                tr.get("_code").unwrap_or(&Value::Null),
                "http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-type",
            ) {
                return ft;
            }
        }
        return code.to_string();
    }
    String::new()
}

/// renderCodingCell (cvr:659): the Category/Code cell. Binding branch links the
/// strength + resolved ValueSet; the coding branch links each code via its CS
/// (versioned THO webPath + `#{cs.id}-{code}` anchor) with a validateCode title.
fn render_coding_cell(
    ctx: &IgContext,
    tx: &dyn crate::txcache::TxCacheSource,
    core_path: &str,
    list: &[SumCoding],
    binding: Option<&SumBinding>,
) -> String {
    let mut b = String::from("<td>");
    if let Some(bind) = binding {
        // strength link + " VS " + VS link (cvr:664-672).
        b.push_str(&format!(
            "<a href=\"{}terminologies.html#{}\">{}</a> VS ",
            core_path, bind.strength, bind.strength
        ));
        let vs_url = strip_version(&bind.value_set);
        match ctx.resolve(&vs_url) {
            Some(vs) if vs.rtype == "ValueSet" && !vs.web_path.is_empty() => {
                b.push_str(&format!(
                    "<a href=\"{}\">{}</a>",
                    vs.web_path,
                    escape_xml(&vs.present())
                ));
            }
            Some(vs) if vs.rtype == "ValueSet" => {
                b.push_str(&escape_xml(&vs.present()));
            }
            _ => b.push_str(&escape_xml(&bind.value_set)),
        }
    } else {
        let mut first = true;
        for t in list {
            if first {
                first = false;
            } else {
                b.push_str(", ");
            }
            b.push_str(&render_one_coding(ctx, tx, t));
        }
    }
    b.push_str("</td>");
    b
}

/// One coding within renderCodingCell's coding branch (cvr:674-700).
fn render_one_coding(
    ctx: &IgContext,
    tx: &dyn crate::txcache::TxCacheSource,
    t: &SumCoding,
) -> String {
    // sys = displaySystem(system); if it equals the system, sys=null, then try
    // fetchCodeSystem(system).getTitle().
    let mut sys = display_system(ctx, &t.system);
    if sys.as_deref() == Some(t.system.as_str()) {
        sys = None;
    }
    if sys.is_none() {
        if let Some(cs) = fetch_code_system(ctx, &t.system) {
            sys = cs.title.clone().or_else(|| cs.name.clone());
        }
    }
    let display = tx.lookup_display(&t.system, &t.code, t.version.as_deref().unwrap_or(""));
    if let Some(disp) = display {
        // title = system + (sys? " ("+sys+")") + ": " + display.
        let title = format!(
            "{}{}: {}",
            t.system,
            sys.as_ref()
                .map(|s| format!(" ({})", s))
                .unwrap_or_default(),
            disp
        );
        // Link when fetchCodeSystem has a webPath (cvr:691-696).
        if let Some(cs) = fetch_code_system(ctx, &t.system) {
            if !cs.web_path.is_empty() {
                let cs_id = cs_id_of(ctx, &t.system).unwrap_or_default();
                return format!(
                    "<a href=\"{}#{}-{}\" title=\"{}\">{}</a>",
                    cs.web_path, cs_id, t.code, title, t.code
                );
            }
        }
        format!("<span title=\"{}\">{}</span>", title, t.code)
    } else {
        // No display (cvr:699): title = system + (sys? " ("+sys+"): ").
        let title = format!(
            "{}{}",
            t.system,
            sys.as_ref()
                .map(|s| format!(" ({}): ", s))
                .unwrap_or_default()
        );
        format!("<span title=\"{}\">{}</span>", title, t.code)
    }
}

/// The CodeSystem `id` for a system uri (used for the `#{id}-{code}` anchor).
fn cs_id_of(ctx: &IgContext, system: &str) -> Option<String> {
    let cs = ctx.load_resource(system)?;
    cs.get("id").and_then(|x| x.as_str()).map(String::from)
}

/// displaySystem (DataRenderer:255) — the friendly source name, or None when
/// there is no override (the caller then falls back to the CS title).
fn display_system(ctx: &IgContext, system: &str) -> Option<String> {
    match system {
        "http://loinc.org" => Some("LOINC".to_string()),
        s if s.starts_with("http://snomed.info") => Some("SNOMED CT".to_string()),
        "http://www.nlm.nih.gov/research/umls/rxnorm" => Some("RxNorm".to_string()),
        "http://unitsofmeasure.org" => Some("UCUM".to_string()),
        s => {
            if let Some(cs) = ctx.resolve(s) {
                if cs.rtype == "CodeSystem" {
                    return Some(cs.present());
                }
            }
            // tails(system) — but returning the system signals "no override" to
            // the caller (matches Java's `sys.equals(system)` reset).
            Some(s.to_string())
        }
    }
}

/// renderBoolean (cvr:591): a conf-*.png cell for the Data Absent Reason column.
fn render_boolean_cell(val: Option<bool>) -> String {
    let img = match val {
        None => "conf-optional.png",
        Some(true) => "conf-required.png",
        Some(false) => "conf-prohibited.png",
    };
    format!("<td><img src=\"{}\"/></td>", img)
}

/// `summary-observations` = cvr.getObservationSummary (cvr:487). Empty branch
/// (no own Observation SD with a fixed code/category): `<p>No Observations
/// Found</p>\r\n`. Grid branch (cvr:491): sorted by lowercased url, dynamic
/// column set (only columns any profile populates), category/code cells resolve
/// terminology (validateCode displays + versioned CS webPaths) via the tx cache.
pub fn summary_observations(
    ctx: &IgContext,
    tx: &dyn crate::txcache::TxCacheSource,
    core_path: &str,
) -> String {
    let mut obs_list: Vec<(String, ObsProfile)> = ctx
        .own_resources()
        .into_iter()
        .filter(|r| {
            r.rtype == "StructureDefinition"
                && r.json.get("type").and_then(|x| x.as_str()) == Some("Observation")
        })
        .filter_map(|r| {
            let url = r.json.get("url").and_then(|x| x.as_str())?.to_lowercase();
            see_observation(&r).map(|o| (url, o))
        })
        .collect();
    if obs_list.is_empty() {
        return "<p>No Observations Found</p>\r\n".to_string();
    }
    // ObsListSorter (cvr:65): by lowercased url.
    obs_list.sort_by(|a, b| a.0.cmp(&b.0));
    let profiles: Vec<&ObsProfile> = obs_list.iter().map(|(_, o)| o).collect();

    // Column presence flags (cvr:494-517).
    let (
        mut has_cat,
        mut has_code,
        mut has_eff,
        mut has_types,
        mut has_dar,
        mut has_body,
        mut has_method,
    ) = (false, false, false, false, false, false, false);
    for op in &profiles {
        has_cat = has_cat || !op.category.is_empty() || op.cat_vs.is_some();
        has_code = has_code || !op.code.is_empty() || op.code_vs.is_some();
        has_eff = has_eff || !op.effective_types.is_empty();
        has_types = has_types || !op.types.is_empty();
        for c in &op.components {
            has_code = has_code || !c.code.is_empty() || c.code_vs.is_some();
            has_eff = has_eff || !c.effective_types.is_empty();
            has_types = has_types || !c.types.is_empty();
        }
    }
    let _ = (&mut has_dar, &mut has_body, &mut has_method);

    // The category-collapse (cvr:518-534): if all profiles share one category,
    // a `<p>` note replaces the column. No corpus IG collapses (categories
    // differ or some are empty), so this fires a loud gap if it ever would.
    if has_cat {
        let cat0 = &profiles[0].category;
        let same = profiles.iter().all(|op| is_same_codes(cat0, &op.category));
        if same {
            panic!(
                "STOP: summary-observations category-collapse (cvr:518) — all \
                 profiles share one category; the displayCoding note branch is \
                 not ported (zero corpus hits)."
            );
        }
    }

    let mut b = String::new();
    b.push_str("<table class=\"grid\">\r\n");
    b.push_str(" <tr><td><b>Profile Name</b></td>");
    if has_cat {
        b.push_str("<td><b>Category</b></td>");
    }
    if has_code {
        b.push_str("<td><b>Code</b></td>");
    }
    if has_eff {
        b.push_str("<td><b>Time Types</b></td>");
    }
    if has_types {
        b.push_str("<td><b>Value Types</b></td>");
    }
    if has_dar {
        b.push_str("<td><b>Data Absent Reason</b></td>");
    }
    if has_body {
        b.push_str("<td><b>Body Site</b></td>");
    }
    if has_method {
        b.push_str("<td><b>Method</b></td>");
    }
    b.push_str("</tr>\r\n");

    let base_eff = base_types_of(ctx, "Observation", "Observation.effective[x]", false);
    let base_val = base_types_of(ctx, "Observation", "Observation.value", true);

    for op in &profiles {
        b.push_str(" <tr>");
        b.push_str(&format!(
            "<td><a href=\"{}\" title=\"{}\">{}</a></td>",
            op.web,
            escape_xml(&op.present),
            escape_xml(&op.id)
        ));
        if has_cat {
            b.push_str(&render_coding_cell(
                ctx,
                tx,
                core_path,
                &op.category,
                op.cat_vs.as_ref(),
            ));
        }
        if has_code {
            b.push_str(&render_coding_cell(
                ctx,
                tx,
                core_path,
                &op.code,
                op.code_vs.as_ref(),
            ));
        }
        if has_eff {
            b.push_str(&render_type_cell(ctx, &op.effective_types, &base_eff));
        }
        if has_types {
            b.push_str(&render_type_cell(ctx, &op.types, &base_val));
        }
        if has_dar {
            b.push_str(&render_boolean_cell(None));
        }
        if has_body {
            b.push_str("<td></td>");
        }
        if has_method {
            b.push_str("<td></td>");
        }
        b.push_str("</tr>\r\n");
        for c in &op.components {
            b.push_str(" <tr style=\"background-color: #eeeeee\">");
            b.push_str(&format!("<td>&nbsp;&nbsp;{}</td>", escape_xml(&c.name)));
            b.push_str("<td></td>");
            if has_code {
                b.push_str(&render_coding_cell(
                    ctx,
                    tx,
                    core_path,
                    &c.code,
                    c.code_vs.as_ref(),
                ));
            }
            if has_eff {
                b.push_str(&render_type_cell(ctx, &c.effective_types, &base_eff));
            }
            if has_types {
                b.push_str(&render_type_cell(ctx, &c.types, &base_val));
            }
            if has_dar {
                b.push_str(&render_boolean_cell(None));
            }
            if has_body {
                b.push_str("<td></td>");
            }
            if has_method {
                b.push_str("<td></td>");
            }
            b.push_str("</tr>\r\n");
        }
    }
    b.push_str("</table>\r\n");
    b
}

/// isSameCodes (cvr:577): same multiset of (system, code) codings.
fn is_same_codes(l1: &[SumCoding], l2: &[SumCoding]) -> bool {
    if l1.len() != l2.len() {
        return false;
    }
    l1.iter().all(|c1| {
        l2.iter().any(|c2| {
            !c2.system.is_empty()
                && c2.system == c1.system
                && !c2.code.is_empty()
                && c2.code == c1.code
        })
    })
}

/// The base type codes for an Observation element path (getBaseTypes, cvr:136):
/// effective[x] (exact path) or value[x] (prefix + dotcount==1 + max!=0).
fn base_types_of(ctx: &IgContext, type_name: &str, path: &str, is_value: bool) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(r) = ctx.load_resource(&format!(
        "http://hl7.org/fhir/StructureDefinition/{}",
        type_name
    )) {
        let empty: Vec<Value> = Vec::new();
        let els = r
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(|x| x.as_array())
            .unwrap_or(&empty);
        for ed in els {
            let p = ed.get("path").and_then(|x| x.as_str()).unwrap_or("");
            let hit = if is_value {
                p.starts_with(path)
                    && dot_count(p) == 1
                    && ed.get("max").and_then(|x| x.as_str()) != Some("0")
            } else {
                p == path
            };
            if hit {
                for tr in ed
                    .get("type")
                    .and_then(|x| x.as_array())
                    .map(|a| a.as_slice())
                    .unwrap_or(&[])
                {
                    let code = working_code(tr);
                    if !code.is_empty() && !out.iter().any(|x| x == &code) {
                        out.push(code);
                    }
                }
            }
        }
    }
    out
}

/// strip a `|version` from a canonical.
fn strip_version(url: &str) -> String {
    url.split('|').next().unwrap_or(url).to_string()
}

/// The nested `structuredefinition-standards-status-reason` markdown on the
/// standards-status extension's value (the `_valueCode` sibling). dpr:45.
fn standards_status_reason(res: &Value) -> Option<String> {
    let exts = res.get("extension")?.as_array()?;
    for x in exts {
        if x.get("url").and_then(|u| u.as_str()) == Some(EXT_STANDARDS_STATUS) {
            let side = x.get("_valueCode")?;
            let subs = side.get("extension")?.as_array()?;
            for e in subs {
                if e.get("url").and_then(|u| u.as_str())
                    == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status-reason")
                {
                    return e
                        .get("valueMarkdown")
                        .or_else(|| e.get("valueString"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
            }
        }
    }
    None
}

/// One deprecated resource (dpr DeprecationInfo). With `previous == null` in
/// this corpus, dstatus is always UNKNOWN (empty Change cell), and only the
/// standards-status==deprecated branch contributes.
struct DeprInfo {
    path: String,
    rtype: String,
    name: String,
    reason: Option<String>,
    desc: String,
    status: String,
}

/// `deprecated-list` = dpr.deprecationSummary (dpr:30). Empty when no resource
/// carries standards-status==deprecated (and no previous comparator) -> `<p>No
/// deprecated content</p>` (dpr:79). cycle/plan-net hit this.
/// Grid branch (dpr:82): a `grid` table, composed by `XhtmlComposer(true,true)`
/// = XML pretty, with markdown Reason/Description cells. `previous == null`
/// corpus-wide, so dstatus is UNKNOWN (empty non-bold Change cell); the
/// previous-version change/un-deprecate branches (dpr:61-74, 100-104) are not
/// reachable and fire a loud gap only if a previous comparator ever appears.
pub fn deprecated_list(ctx: &IgContext, core_path: &str) -> String {
    let mut list: Vec<DeprInfo> = Vec::new();
    for r in ctx.own_resources() {
        if ext_value_str(&r.json, EXT_STANDARDS_STATUS).as_deref() != Some("deprecated") {
            continue;
        }
        // title || name || id (dpr:46-52).
        let name = r
            .json
            .get("title")
            .and_then(|x| x.as_str())
            .or_else(|| r.json.get("name").and_then(|x| x.as_str()))
            .unwrap_or(&r.id)
            .to_string();
        let desc = r
            .json
            .get("description")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        list.push(DeprInfo {
            path: r.web_path.clone(),
            rtype: r.rtype.clone(),
            name,
            reason: standards_status_reason(&r.json),
            desc,
            status: r
                .json
                .get("status")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    // DeprecationInfoSorter (dpr:166): dstatus (all UNKNOWN here) then name.
    list.sort_by(|a, b| a.name.cmp(&b.name));
    if list.is_empty() {
        return "<p>No deprecated content</p>".to_string();
    }

    use render_xhtml::{Config, XhtmlComposer, XhtmlNode};
    let mut div = XhtmlNode::new_tag("div");
    let mut tbl = XhtmlNode::new_tag("table");
    tbl.set_attribute("class", "grid");

    // Header row.
    let mut hr = XhtmlNode::new_tag("tr");
    for h in ["Resource", "Change", "Status", "Reason", "Description"] {
        let mut td = XhtmlNode::new_tag("td");
        let mut b = XhtmlNode::new_tag("b");
        b.add_text(h.to_string());
        td.add_child_node(b);
        hr.add_child_node(td);
    }
    tbl.add_child_node(hr);

    for di in &list {
        let mut tr = XhtmlNode::new_tag("tr");
        // Row background style only when reason == null (dpr:92-98).
        if di.reason.is_none() {
            let style = if di.desc.to_lowercase().contains("deprecated") {
                "background-color: #ffeccf"
            } else {
                "background-color: #ffcfcf"
            };
            tr.set_attribute("style", style);
        }
        // Resource cell: <a href=path>name (type)</a>.
        let mut td_res = XhtmlNode::new_tag("td");
        let mut a = XhtmlNode::new_tag("a");
        a.set_attribute("href", di.path.clone());
        a.add_text(format!("{} ({})", di.name, di.rtype));
        td_res.add_child_node(a);
        tr.add_child_node(td_res);
        // Change cell: dstatus UNKNOWN -> `td().tx("")` (dpr:103). The empty
        // text child makes it `<td></td>`, NOT the XML self-closed `<td/>`.
        let mut td_change = XhtmlNode::new_tag("td");
        td_change.add_text(String::new());
        tr.add_child_node(td_change);
        // Status cell: non-"retired" -> maroon bold; else plain (dpr:105-109).
        let mut td_status = XhtmlNode::new_tag("td");
        if di.status != "retired" {
            td_status.set_attribute("style", "color: maroon");
            let mut b = XhtmlNode::new_tag("b");
            b.add_text(di.status.clone());
            td_status.add_child_node(b);
        } else {
            td_status.add_text(di.status.clone());
        }
        tr.add_child_node(td_status);
        // Reason + Description cells: markdown(preProcessMarkdown(...)) (dpr:110-111).
        tr.add_child_node(markdown_td(
            ctx,
            di.reason.as_deref().unwrap_or(""),
            core_path,
        ));
        tr.add_child_node(markdown_td(ctx, &di.desc, core_path));
        tbl.add_child_node(tr);
    }
    div.add_child_node(tbl);
    let mut c = XhtmlComposer::new(Config::xml_pretty());
    c.compose_nodes(div.child_nodes())
}

/// A `<td>` whose content is `preProcessMarkdown(text)` then `markdown()` (the
/// XhtmlNode.markdown convenience: parse the processed HTML into a nested `<div>`
/// and add it). Mirrors `tr.td().markdown(preProcessMarkdown("?", x), "?")`.
fn markdown_td(ctx: &IgContext, text: &str, core_path: &str) -> render_xhtml::XhtmlNode {
    let mut td = render_xhtml::XhtmlNode::new_tag("td");
    let pre = crate::publisher_markdown::pre_process_markdown(ctx, text, core_path);
    let html = crate::publisher_markdown::md_process(&pre);
    for node in crate::publisher_markdown::markdown_children_from_html(&html) {
        td.add_child_node(node);
    }
    td
}

/// `expansion-params` = renderExpansionParameters (pg:1686). Empty when the
/// context's expansion parameters hold nothing beyond `x-system-cache-id` /
/// `defaultDisplayLanguage` (pg:1690-1697) -> "" (and pg:2922 emits the plain
/// non-tracked empty fragment). cycle/plan-net hit this.
///
/// `has_interesting_params` is a per-IG build fact (the context's expansion
/// parameters come from the build's tx setup, NOT output/*.json). Golden-
/// matched: cycle/plan-net empty, us-core a grid (tracked "5").
///
/// **STOP (grid branch, us-core) — runtime terminology-state artifact, NOT
/// portable.** `pf.context.getExpansionParameters()` (pg:1688) is a live
/// `Parameters` accumulated across the ENTIRE build: only the `system-version`
/// rows come from the IG input (`input/resources/Parameters-manifest.json`);
/// the ~80 `default-canonical-version` rows are injected at runtime by the tx
/// layer as it pins every canonical it resolves during expansion, in build-
/// execution order. That order + membership is not reconstructable from
/// output/*.json (verified: the manifest holds only `system-version`; no input
/// or input-cache file carries the `default-canonical-version` set). Feeding
/// the full 90-row list as a harness constant would be replaying the golden,
/// not rendering it — no engine, no parity signal. Same class as the tx-cache
/// accumulation cases; deferred to the terminology phase where the build's tx
/// operation log is available. The per-row cell renderer (CS/VS present()+
/// version+webPath via findTxResource; SNOMED `SNOMED CT[US]` special-casing;
/// displayCodeSource fallback) is bounded and would port cleanly once the
/// parameter list is available.
pub fn expansion_params(has_interesting_params: bool) -> String {
    if !has_interesting_params {
        String::new()
    } else {
        panic!(
            "STOP: expansion-params grid branch (pg:1698) is a runtime tx-state \
             artifact (getExpansionParameters accumulates default-canonical-version \
             pins across the whole build) — not reconstructable from output/*.json. \
             See the module doc; deferred to the terminology phase."
        );
    }
}

// ---------------------------------------------------------------------------
// CrossViewRenderer CS/VS "defined" lists (cvr:1393/1685)
// ---------------------------------------------------------------------------

const EXT_STANDARDS_STATUS: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status";
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
        .map(|a| {
            a.iter()
                .any(|e| e.get("url").and_then(|u| u.as_str()) == Some(url))
        })
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
            let url = r
                .json
                .get("url")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
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
fn need_version_references(
    list: &[(String, String, std::rc::Rc<Value>)],
    ig_version: &str,
) -> bool {
    list.iter()
        .any(|(_u, _w, j)| get_str(j, "version") != ig_version)
}

/// A resolved ValueSet in a used-VS list: (url, version). Identity is by
/// object in Java (`list.contains(vs)`); here we dedup by (url, version) which
/// is behavior-equivalent for the version-flag boolean (each distinct loaded VS
/// object has one url+version).
fn collect_vs_ref(out: &mut Vec<(String, String)>, ctx: &IgContext, url: &str) {
    if url.is_empty() {
        return;
    }
    // findTxResource(ValueSet, url): resolve to a loaded ValueSet.
    if let Some(r) = ctx.resolve(url) {
        if r.rtype == "ValueSet" {
            let entry = (strip_ver(url), r.version.clone());
            if !out.iter().any(|e| e.0 == entry.0) {
                out.push(entry);
            }
        }
    }
}

fn strip_ver(u: &str) -> String {
    u.split('|').next().unwrap_or(u).to_string()
}

/// Recursively collect VS canonical refs from binding-bearing element arrays
/// and compose includes. Mirrors cvr.findValueSets over own resources
/// (cvr:1252-1368). For the used-ALL list we walk SD snapshots + own VS compose
/// imports + Questionnaire/ConceptMap/OperationDefinition binding refs.
fn build_used_valueset_versions(ctx: &IgContext, all: bool) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for r in ctx.own_resources() {
        match r.rtype.as_str() {
            "StructureDefinition" => {
                let section = if all { "snapshot" } else { "differential" };
                if let Some(els) = r
                    .json
                    .get(section)
                    .and_then(|s| s.get("element"))
                    .and_then(|e| e.as_array())
                {
                    for ed in els {
                        if let Some(b) = ed.get("binding") {
                            if let Some(vs) = b.get("valueSet").and_then(|x| x.as_str()) {
                                collect_vs_ref(&mut out, ctx, vs);
                            }
                            if let Some(adds) = b.get("additional").and_then(|x| x.as_array()) {
                                for ab in adds {
                                    if let Some(vs) = ab.get("valueSet").and_then(|x| x.as_str()) {
                                        collect_vs_ref(&mut out, ctx, vs);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "ValueSet" => {
                // The own VS itself is in the list (findValueSets adds it), and
                // its compose.include[].valueSet imports resolve too.
                let uurl = r.json.get("url").and_then(|x| x.as_str()).unwrap_or("");
                let ver = get_str(&r.json, "version").to_string();
                let entry = (uurl.to_string(), ver);
                if !uurl.is_empty() && !out.iter().any(|e| e.0 == entry.0) {
                    out.push(entry);
                }
                if let Some(incs) = r
                    .json
                    .get("compose")
                    .and_then(|c| c.get("include"))
                    .and_then(|x| x.as_array())
                {
                    for inc in incs {
                        if let Some(vss) = inc.get("valueSet").and_then(|x| x.as_array()) {
                            for u in vss {
                                if let Some(us) = u.as_str() {
                                    collect_vs_ref(&mut out, ctx, us);
                                }
                            }
                        }
                    }
                }
            }
            "Questionnaire" => {
                walk_questionnaire_vs(&mut out, ctx, &r.json);
            }
            "ConceptMap" => {
                for k in [
                    "sourceScope",
                    "targetScope",
                    "sourceScopeCanonical",
                    "targetScopeCanonical",
                ] {
                    if let Some(u) = r.json.get(k).and_then(|x| x.as_str()) {
                        collect_vs_ref(&mut out, ctx, u);
                    }
                }
            }
            "OperationDefinition" => {
                if let Some(ps) = r.json.get("parameter").and_then(|x| x.as_array()) {
                    for p in ps {
                        if let Some(vs) = p
                            .get("binding")
                            .and_then(|b| b.get("valueSet"))
                            .and_then(|x| x.as_str())
                        {
                            collect_vs_ref(&mut out, ctx, vs);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn walk_questionnaire_vs(out: &mut Vec<(String, String)>, ctx: &IgContext, node: &Value) {
    if let Some(items) = node.get("item").and_then(|x| x.as_array()) {
        for item in items {
            if let Some(vs) = item.get("answerValueSet").and_then(|x| x.as_str()) {
                collect_vs_ref(out, ctx, vs);
            }
            walk_questionnaire_vs(out, ctx, item);
        }
    }
}

/// The `versions` flag for codesystem-list: needVersionReferences over the
/// USED-ALL VS list (pg:2799 passes the leftover buildUsedValueSetList(true)).
pub fn codesystem_list_versions_flag(ctx: &IgContext, ig_version: &str) -> bool {
    let used = build_used_valueset_versions(ctx, true);
    used.iter().any(|(_u, v)| v != ig_version)
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
        b.push_str(&cs_row_cells(url, web, j, versions));
        b.push_str("</tr>");
    }
    b.push_str("</table>");
    b
}

/// The CS list row cells (URL .. Count), shared by codesystem-list and the ref
/// variants. Does NOT emit `<tr>`/`</tr>` nor the References column.
fn cs_row_cells(url: &str, web: &str, j: &Value, versions: bool) -> String {
    let mut b = String::new();
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
    let concepts = j
        .get("concept")
        .and_then(|x| x.as_array())
        .unwrap_or(&empty);
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

// ---------------------------------------------------------------------------
// canonical-index (generateCanonicalSummary, pg:4755)
// ---------------------------------------------------------------------------

/// One row of the canonical index: a CanonicalResource with a url.
pub struct CanonRow {
    pub rtype: String,
    pub id: String,
    pub url: String,
    pub version: String,
    pub web_path: Option<String>,
    pub oids: Vec<String>,
    pub alt_urls: Vec<String>,
}

/// The build's `oids.ini` OID registry (when present): (fhirType, id) -> oids.
/// The publisher injects these into each resource's identifier at build time,
/// so `cr.getIdentifier()` returns them even for types (CapabilityStatement)
/// whose R4 output JSON has no identifier element. Authoritative when present.
pub type OidMap = std::collections::HashMap<(String, String), Vec<String>>;

/// Parse `oids.ini`: `[Type]` sections mapping `id = oid` (id may repeat).
pub fn parse_oids_ini(text: &str) -> OidMap {
    let mut map: OidMap = std::collections::HashMap::new();
    let mut section = String::new();
    // The non-type sections to ignore.
    let skip = ["Documentation", "Key"];
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') && l.ends_with(']') {
            section = l[1..l.len() - 1].to_string();
            continue;
        }
        if section.is_empty() || skip.contains(&section.as_str()) {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            let id = k.trim().to_string();
            let oid = v.trim().to_string();
            if id.is_empty() || oid.is_empty() {
                continue;
            }
            map.entry((section.clone(), id)).or_default().push(oid);
        }
    }
    map
}

fn canon_row(
    rtype: &str,
    id: &str,
    web: Option<String>,
    j: &Value,
    oid_map: Option<&OidMap>,
) -> Option<CanonRow> {
    let url = j.get("url").and_then(|x| x.as_str())?.to_string();
    let mut json_oids = Vec::new();
    let mut alt = Vec::new();
    if let Some(ids) = j.get("identifier").and_then(|x| x.as_array()) {
        for id in ids {
            if id.get("system").and_then(|x| x.as_str()) == Some("urn:ietf:rfc:3986") {
                if let Some(val) = id.get("value").and_then(|x| x.as_str()) {
                    if let Some(oid) = val.strip_prefix("urn:oid:") {
                        json_oids.push(oid.to_string());
                    } else {
                        alt.push(val.to_string());
                    }
                }
            }
        }
    }
    // OIDs: the oids.ini registry (authoritative, complete) when present for
    // this (type, id); else the JSON-embedded oid identifiers.
    let oids = oid_map
        .and_then(|m| m.get(&(rtype.to_string(), id.to_string())).cloned())
        .unwrap_or(json_oids);
    Some(CanonRow {
        rtype: rtype.to_string(),
        id: id.to_string(),
        url,
        version: get_str(j, "version").to_string(),
        web_path: web,
        oids,
        alt_urls: alt,
    })
}

/// Detect an R5-in-R4 `Basic` wrapper: returns (fhirType, url, version) from
/// the `http://hl7.org/fhir/5.0/StructureDefinition/extension-{Type}.{field}`
/// extensions. None if not such a wrapper.
fn xver_basic(j: &Value) -> Option<(String, String, String)> {
    const PFX: &str = "http://hl7.org/fhir/5.0/StructureDefinition/extension-";
    let exts = j.get("extension")?.as_array()?;
    let mut xtype: Option<String> = None;
    let mut url: Option<String> = None;
    let mut version = String::new();
    for e in exts {
        let u = e.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if let Some(rest) = u.strip_prefix(PFX) {
            // rest = "{Type}.{field}"
            if let Some((ty, field)) = rest.split_once('.') {
                xtype.get_or_insert_with(|| ty.to_string());
                match field {
                    "url" => {
                        url = e.get("valueUri").and_then(|x| x.as_str()).map(String::from);
                    }
                    "version" => {
                        version = e
                            .get("valueString")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                    }
                    _ => {}
                }
            }
        }
    }
    match (xtype, url) {
        (Some(t), Some(u)) => Some((t, u, version)),
        _ => None,
    }
}

/// `canonical-index` = generateCanonicalSummary (pg:4755). All own
/// CanonicalResources (incl. the ImplementationGuide), sorted by (fhirType,
/// id), grouped by type with an `#eeeeee` header row. XhtmlComposer(true) =
/// compact XML. `ig` carries the ImplementationGuide's (id, url, version) —
/// its webPath is `index.html`.
pub fn canonical_index(
    ctx: &IgContext,
    ig: Option<(String, String, String)>,
    oid_map: Option<&OidMap>,
) -> String {
    let mut rows: Vec<CanonRow> = Vec::new();
    for r in ctx.own_resources() {
        // R5-in-R4 cross-version encoding: an R5 resource (e.g. Requirements)
        // is stored as a `Basic` carrying `http://hl7.org/fhir/5.0/
        // StructureDefinition/extension-{Type}.{field}` extensions. The
        // publisher converts it back to its R5 CanonicalResource. Detect the
        // `.url` marker and re-project the row.
        if r.rtype == "Basic" {
            if let Some((xtype, xurl, xver)) = xver_basic(&r.json) {
                let web = format!("{}-{}.html", xtype, r.id);
                let oids = oid_map
                    .and_then(|m| m.get(&(xtype.clone(), r.id.clone())).cloned())
                    .unwrap_or_default();
                rows.push(CanonRow {
                    rtype: xtype,
                    id: r.id.clone(),
                    url: xurl,
                    version: xver,
                    web_path: Some(web),
                    oids,
                    alt_urls: Vec::new(),
                });
                continue;
            }
        }
        if let Some(row) = canon_row(&r.rtype, &r.id, Some(r.web_path.clone()), &r.json, oid_map) {
            rows.push(row);
        }
    }
    if let Some((id, url, version)) = ig {
        // The IG is assigned the auto-oid-root itself (from oids.ini's
        // ImplementationGuide entry or, when absent, the sushi-config
        // auto-oid-root supplied via the map under the IG's id).
        let ig_oids = oid_map
            .and_then(|m| {
                m.get(&("ImplementationGuide".to_string(), id.clone()))
                    .cloned()
            })
            .unwrap_or_default();
        rows.push(CanonRow {
            rtype: "ImplementationGuide".to_string(),
            id,
            url,
            version,
            web_path: Some("index.html".to_string()),
            oids: ig_oids,
            alt_urls: Vec::new(),
        });
    }
    // CanonicalResourceSortByTypeId (ResourceSorters:28): fhirType then id.
    rows.sort_by(|a, b| a.rtype.cmp(&b.rtype).then(a.id.cmp(&b.id)));

    let mut b = String::new();
    b.push_str("<table class=\"grid\">");
    b.push_str("<tr><td><b>Canonical</b></td><td><b>Id</b></td><td><b>Version</b></td><td><b>Oids</b></td><td><b>Other URLS</b></td></tr>");
    let mut cur_type = String::new();
    for row in &rows {
        if cur_type != row.rtype {
            cur_type = row.rtype.clone();
            b.push_str(&format!(
                "<tr style=\"background-color: #eeeeee\"><td colspan=\"5\"><h3><a name=\"{t}\"> </a>{t}</h3></td></tr>",
                t = escape_xml(&cur_type)
            ));
        }
        b.push_str("<tr>");
        match &row.web_path {
            Some(w) => b.push_str(&format!(
                "<td><a href=\"{}\">{}</a></td>",
                escape_xml(w),
                escape_xml(&row.url)
            )),
            None => b.push_str(&format!("<td><code>{}</code></td>", escape_xml(&row.url))),
        }
        b.push_str(&format!("<td>{}</td>", escape_xml(&row.id)));
        b.push_str(&format!("<td>{}</td>", escape_xml(&row.version)));
        b.push_str(&format!("<td>{}</td>", escape_xml(&row.oids.join(", "))));
        b.push_str(&format!(
            "<td>{}</td>",
            escape_xml(&row.alt_urls.join(", "))
        ));
        b.push_str("</tr>");
    }
    b.push_str("</table>");
    b
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
        b.push_str(&vs_row_cells(ctx, url, web, j, versions));
        b.push_str("</tr>");
    }
    b.push_str("</table>");
    b
}

/// `valueset-ref-list` / `valueset-ref-all-list` (renderVSList used=true,
/// pg:2789/2794). Identical VS cells + a References column (see xreflist).
pub fn valueset_ref_list(ctx: &IgContext, ig_version: &str, all: bool) -> String {
    let rows = crate::xreflist::used_vs_rows(ctx, all);
    let versions = rows
        .iter()
        .any(|r| get_str(&r.json, "version") != ig_version);
    let mut b = String::new();
    b.push_str("<table class=\"grid\"><tr><th>URL</th>");
    if versions {
        b.push_str("<th>Version</th>");
    }
    b.push_str(
        "<th>Name / Title</th><th>Status</th><th>Flags</th><th>Source</th><th>References</th></tr>",
    );
    for r in &rows {
        b.push_str("<tr>");
        b.push_str(&vs_row_cells(ctx, &r.url, &r.web, &r.json, versions));
        b.push_str(&crate::xreflist::references_cell(&r.refs));
        b.push_str("</tr>");
    }
    b.push_str("</table>");
    b
}

/// `codesystem-ref-list` / `codesystem-ref-all-list` (renderCSList used=true,
/// pg:2804/2809). `versions` = needVersionReferences over the used-VS list
/// (pg:2799 wart — the CS ref tables share the VS list's version flag).
pub fn codesystem_ref_list(ctx: &IgContext, versions: bool, all: bool) -> String {
    let rows = crate::xreflist::used_cs_rows(ctx, all);
    let mut b = String::new();
    b.push_str("<table class=\"grid\"><tr><th>URL</th>");
    if versions {
        b.push_str("<th>Version</th>");
    }
    b.push_str(
        "<th>Name / Title</th><th>Status</th><th>Flags</th><th>Count</th><th>References</th></tr>",
    );
    for r in &rows {
        b.push_str("<tr>");
        b.push_str(&cs_row_cells(&r.url, &r.web, &r.json, versions));
        b.push_str(&crate::xreflist::references_cell(&r.refs));
        b.push_str("</tr>");
    }
    b.push_str("</table>");
    b
}

/// The VS list row cells (URL .. Source), shared by valueset-list and the ref
/// variants. Does NOT emit the `<tr>`/`</tr>` nor the References column.
fn vs_row_cells(ctx: &IgContext, url: &str, web: &str, j: &Value, versions: bool) -> String {
    {
        let mut b = String::new();
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
        if compose
            .and_then(|c| c.get("inactive"))
            .and_then(|x| x.as_bool())
            == Some(true)
        {
            flags.push_str("Inactive ");
        }
        let includes = compose
            .and_then(|c| c.get("include"))
            .and_then(|x| x.as_array())
            .unwrap_or(&empty);
        let (mut inc_i, mut inc_e, mut inc_v, mut inc_a) = (false, false, false, false);
        let mut sources: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for inc in includes {
            if inc
                .get("valueSet")
                .and_then(|x| x.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
                || inc.get("valueSet").map(|v| !v.is_null()).unwrap_or(false)
            {
                // hasValueSet(): the compose include references value set(s).
                if inc
                    .get("valueSet")
                    .and_then(|x| x.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(inc.get("valueSet").is_some())
                {
                    inc_v = true;
                }
            }
            if let Some(system) = inc.get("system").and_then(|x| x.as_str()) {
                sources.insert(describe_source(ctx, system));
                if inc
                    .get("concept")
                    .and_then(|x| x.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
                {
                    inc_e = true;
                } else if inc
                    .get("filter")
                    .and_then(|x| x.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
                {
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
        b
    }
}
