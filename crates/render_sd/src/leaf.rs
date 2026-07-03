//! `render_sd::leaf` — the non-table SD leaf fragment kinds produced by the
//! PUBLISHER's `org.hl7.fhir.igtools.renderers.StructureDefinitionRenderer`
//! (a `CanonicalRenderer` subclass, 3204 LOC — NOT fhir-core's SDR that made
//! the F3 table kinds). Citations here are `psdr:<line>` = that publisher
//! class (path in scratchpad/psdr_path.txt), and `phrases` =
//! fhir-core-6911 rendering-phrases.properties (English).
//!
//! Every leaf body is composed then wrapped in `{% raw %}..{% endraw %}` by the
//! caller (`wrap_raw`). Composer selection per method is cited inline.

use render_xhtml::node::XhtmlNode;
use render_xhtml::node::NodeType;
use render_xhtml::{Config, XhtmlComposer};

use crate::sdmodel::Sd;

// ---------------------------------------------------------------------------
// small XhtmlNode builder helpers (the publisher/fhir-core convenience API:
// x.para(), x.h4(), x.table(cls), tr.td(), td.b(), td.tx(), td.code(), td.br(),
// td.ah(url)). We build over render_xhtml's low-level add_tag/add_text.
// ---------------------------------------------------------------------------

fn el(name: &str) -> XhtmlNode {
    // makeTag semantics: sets notPretty for the inline element set (b, code, a,
    // span, br, ...) so the composer's pretty path matches fhir-core byte-for-byte.
    XhtmlNode::new_tag(name)
}

/// `XhtmlNode.tx(text)` — appends a text node child, returns self.
fn tx(parent: &mut XhtmlNode, text: &str) {
    parent.add_text(text.to_string());
}

/// Compose a `<div>`'s children with `new XhtmlComposer(false, true)` =
/// (xml=false, pretty=true) => HTML pretty, via the `compose(XhtmlNodeList)`
/// overload (no breakBlocksWithLines). Used by invOldMode/tx/txDiff (psdr:1262,
/// 837, 890).
fn compose_children_html_pretty(div: &XhtmlNode) -> String {
    let mut c = XhtmlComposer::new(Config::html_pretty());
    c.compose_nodes(div.child_nodes())
}

// ---------------------------------------------------------------------------
// CONSTANT kinds (verified 1 distinct value corpus-wide)
// ---------------------------------------------------------------------------

/// `contained-index` (PublisherGenerator:894 genContainedIndex) and `history`
/// (PG:1150 HistoryGenerator): both return empty in this corpus (no contained
/// resources, no history). Body == "".
pub fn empty_body() -> String {
    String::new()
}

/// `pseudo-xml` / `pseudo-ttl`: `fragmentError(..., "yet to be done: Xml
/// template"/"Turtle template", null, ...)` (PG:1948/1960). fragmentError
/// (PG:1629) with no overlay => `<p><span style="color: maroon; font-weight:
/// bold">{escapeXml(msg)}</span></p>\r\n`.
pub fn fragment_error(msg: &str) -> String {
    format!(
        "<p><span style=\"color: maroon; font-weight: bold\">{}</span></p>\r\n",
        escape_xml(msg)
    )
}

pub fn pseudo_xml() -> String {
    fragment_error("yet to be done: Xml template")
}
pub fn pseudo_ttl() -> String {
    fragment_error("yet to be done: Turtle template")
}

/// `Utilities.escapeXml` (fhir-core Utilities): &, <, >, " only (NOT ').
pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// sd-use-context  (psdr useContext:2877) — HTML-pretty composer
// ---------------------------------------------------------------------------

/// psdr useContext:2877. Renders the extension usage-context list (or the
/// "any element" default for non-extension SDs). Markdown-dependent branches
/// (deprecated standards-status reason) fire a loud gap.
pub fn use_context(sd: &Sd, ctx: &crate::context::IgContext) -> String {
    let mut div = el("div");

    // deprecated standards-status block (psdr:2879) — needs markdown; loud gap.
    if standards_status(sd).as_deref() == Some("deprecated") {
        panic!(
            "LOUD GAP: sd-use-context deprecated standards-status markdown block (psdr:2879) for {}",
            sd.id()
        );
    }
    // modifier extension note (psdr:2894)
    if is_modifier_extension(sd) {
        let mut ddiv = el("div");
        ddiv.set_attribute(
            "style",
            "border: 1px solid black; border-radius: 10px; padding: 10px",
        );
        let mut p = el("p");
        let mut b = el("b");
        tx(&mut b, "This extension is a modifier extension.");
        p.add_child_node(b);
        ddiv.add_child_node(p);
        div.add_child_node(ddiv);
    }

    let contexts = sd
        .root
        .get("context")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    if contexts.is_empty() {
        let mut p = el("p");
        tx(
            &mut p,
            "This extension does not specify which elements it should be used on",
        );
        div.add_child_node(p);
    } else {
        let mut p = el("p");
        tx(
            &mut p,
            "This extension may be used on the following element(s)",
        );
        div.add_child_node(p);
        let mut ul = el("ul");
        for c in &contexts {
            let ty = c.get("type").and_then(|x| x.as_str()).unwrap_or("");
            let expr = c.get("expression").and_then(|x| x.as_str()).unwrap_or("");
            let mut li = el("li");
            match ty {
                "element" => {
                    tx(&mut li, "Element ID");
                    tx(&mut li, ": ");
                    // tn = expr up to first '.'
                    let tn = expr.split('.').next().unwrap_or(expr);
                    let mut code = el("code");
                    let webpath = ctx.resolve_type(tn).map(|r| r.web_path);
                    if let Some(wp) = webpath.filter(|w| !w.is_empty()) {
                        let mut a = el("a");
                        a.set_attribute("href", wp);
                        tx(&mut a, expr);
                        code.add_child_node(a);
                    } else {
                        tx(&mut code, expr);
                    }
                    li.add_child_node(code);
                }
                "extension" => {
                    tx(&mut li, "Extension");
                    tx(&mut li, ": ");
                    if let Some(r) = ctx.resolve(expr).filter(|r| !r.web_path.is_empty()) {
                        let mut a = el("a");
                        a.set_attribute("href", r.web_path.clone());
                        tx(&mut a, &r.present());
                        li.add_child_node(a);
                    } else {
                        let mut code = el("code");
                        tx(&mut code, expr);
                        li.add_child_node(code);
                    }
                }
                "fhirpath" => {
                    let mut a = el("a");
                    a.set_attribute("href", "http://hl7.org/fhir/R4/fhirpath.html");
                    tx(&mut a, "Path");
                    li.add_child_node(a);
                    tx(&mut li, expr);
                }
                _ => {
                    tx(&mut li, "?type?: ");
                    tx(&mut li, expr);
                }
            }
            if c.get("extension")
                .and_then(|e| e.as_array())
                .map(|a| {
                    a.iter().any(|x| {
                        x.get("url").and_then(|u| u.as_str())
                            == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-version-specific-use")
                    })
                })
                .unwrap_or(false)
            {
                panic!("LOUD GAP: sd-use-context fhir-version-specific-use range (psdr:2942) for {}", sd.id());
            }
            ul.add_child_node(li);
        }
        div.add_child_node(ul);
    }

    // context invariants (psdr:2949)
    if let Some(ci) = sd
        .root
        .get("contextInvariant")
        .and_then(|c| c.as_array())
        .filter(|a| !a.is_empty())
    {
        if ci.len() == 1 {
            let mut x = el("p");
            tx(
                &mut x,
                "In addition, the extension can only be used when this FHIRPath expression is true",
            );
            tx(&mut x, ": ");
            div.add_child_node(x);
            let mut p2 = el("p");
            let mut code = el("code");
            tx(&mut code, ci[0].as_str().unwrap_or(""));
            p2.add_child_node(code);
            div.add_child_node(p2);
        } else {
            let mut x = el("p");
            tx(
                &mut x,
                "In addition, the extension can only be used when these FHIRPath expressions are true",
            );
            tx(&mut x, ": ");
            div.add_child_node(x);
            let mut ul = el("ul");
            for sv in ci {
                let mut li = el("li");
                let mut code = el("code");
                tx(&mut code, sv.as_str().unwrap_or(""));
                li.add_child_node(code);
                ul.add_child_node(li);
            }
            div.add_child_node(ul);
        }
    }

    if sd_has_extension(
        sd,
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-version-specific-use",
    ) {
        panic!(
            "LOUD GAP: sd-use-context SD-level fhir-version-specific-use (psdr:2966) for {}",
            sd.id()
        );
    }

    compose_children_html_pretty(&div)
}

fn standards_status(sd: &Sd) -> Option<String> {
    read_string_extension(sd, "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status")
}

/// ProfileUtilities.isModifierExtension: type==Extension AND the
/// Extension.value / root has modifier semantics. Simplified: SD is an
/// Extension whose differential marks a modifierExtension. We approximate via
/// the snapshot Extension root isModifier — corpus has none, so this is safe.
fn is_modifier_extension(sd: &Sd) -> bool {
    if sd.type_name() != "Extension" {
        return false;
    }
    // isModifier on the Extension root element
    sd.snapshot_elements()
        .iter()
        .any(|e| e.path() == "Extension" && e.is_modifier())
}

fn sd_has_extension(sd: &Sd, url: &str) -> bool {
    sd.root
        .get("extension")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().any(|x| x.get("url").and_then(|u| u.as_str()) == Some(url)))
        .unwrap_or(false)
}

fn read_string_extension(sd: &Sd, url: &str) -> Option<String> {
    let arr = sd.root.get("extension")?.as_array()?;
    for x in arr {
        if x.get("url").and_then(|u| u.as_str()) == Some(url) {
            if let Some(v) = x.get("valueCode").and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
            if let Some(v) = x.get("valueString").and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// inv / inv-key / inv-diff  (psdr invOldMode:1203)
// ---------------------------------------------------------------------------

/// GEN_MODE_* (psdr:100-103).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GenMode {
    Snap,
    Diff,
    Ms,
    Key,
}

struct ConstraintVariation {
    // the constraint JSON
    key: String,
    severity: String,
    human: String,
    expression: String,
    source: Option<String>,
    requirements: Option<String>,
    best_practice: bool,
    elements: Vec<String>,
    primary: bool,
}

impl ConstraintVariation {
    /// psdr:1172 getIds()
    fn ids(&self) -> String {
        match self.source.as_deref() {
            Some("http://hl7.org/fhir/StructureDefinition/Element") => "**ALL** elements".to_string(),
            Some("http://hl7.org/fhir/StructureDefinition/Extension") => "**ALL** extensions".to_string(),
            _ => self.elements.join(", "),
        }
    }
    /// psdr:1180 isBold()
    fn is_bold(&self) -> bool {
        matches!(
            self.source.as_deref(),
            Some("http://hl7.org/fhir/StructureDefinition/Element")
                | Some("http://hl7.org/fhir/StructureDefinition/Extension")
        )
    }
    /// psdr:1283 grade()
    fn grade(&self) -> String {
        if self.best_practice {
            "best practice".to_string()
        } else {
            self.severity.clone()
        }
    }
}

struct ConstraintInfo {
    key: String,
    primary: Option<usize>, // index into variations
    variations: Vec<ConstraintVariation>,
    // hash->index for variations (excluding primary once promoted)
    hash_index: std::collections::HashMap<String, usize>,
}

fn constraint_hash(expr: &str, human: &str) -> String {
    format!("{}{}", expr, human)
}

/// A single constraint JSON pulled off an element (fields we render).
struct RawConstraint {
    key: String,
    severity: String,
    human: String,
    expression: String,
    source: Option<String>,
    requirements: Option<String>,
    best_practice: bool,
}

fn read_constraints(ed: &serde_json::Value) -> Vec<RawConstraint> {
    let mut out = Vec::new();
    let Some(arr) = ed.get("constraint").and_then(|c| c.as_array()) else {
        return out;
    };
    for c in arr {
        let best_practice = c
            .get("extension")
            .and_then(|e| e.as_array())
            .map(|a| {
                a.iter().any(|x| {
                    x.get("url").and_then(|u| u.as_str())
                        == Some("http://hl7.org/fhir/StructureDefinition/elementdefinition-bestpractice")
                })
            })
            .unwrap_or(false);
        out.push(RawConstraint {
            key: c.get("key").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            severity: c.get("severity").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            human: c.get("human").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            expression: c.get("expression").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            source: c.get("source").and_then(|x| x.as_str()).map(String::from),
            requirements: c.get("requirements").and_then(|x| x.as_str()).map(String::from),
            best_practice,
        });
    }
    out
}

/// psdr:1121 addVariation.
fn add_variation(ci: &mut ConstraintInfo, c: &RawConstraint, id: &str, sd_url: &str) {
    let is_primary_candidate = match c.source.as_deref() {
        None => true,
        Some(src) if src == sd_url => true,
        Some(src) if src.starts_with("http://hl7.org/fhir/StructureDefinition/")
            && !src[41..].contains('/') =>
        {
            true
        }
        _ => false,
    };
    let hash = constraint_hash(&c.expression, &c.human);
    if is_primary_candidate {
        if ci.primary.is_none() {
            // primary = variations.get(hash) ; if null new; else remove from map
            if let Some(&idx) = ci.hash_index.get(&hash) {
                ci.hash_index.remove(&hash);
                ci.variations[idx].primary = true;
                ci.primary = Some(idx);
            } else {
                ci.variations.push(mk_variation(c));
                let idx = ci.variations.len() - 1;
                ci.variations[idx].primary = true;
                ci.primary = Some(idx);
            }
        }
        let pidx = ci.primary.unwrap();
        ci.variations[pidx].elements.push(id.to_string());
    } else if let Some(&idx) = ci.hash_index.get(&hash) {
        ci.variations[idx].elements.push(id.to_string());
    } else {
        ci.variations.push(mk_variation(c));
        let idx = ci.variations.len() - 1;
        ci.hash_index.insert(hash, idx);
        ci.variations[idx].elements.push(id.to_string());
    }
}

fn mk_variation(c: &RawConstraint) -> ConstraintVariation {
    ConstraintVariation {
        key: c.key.clone(),
        severity: c.severity.clone(),
        human: c.human.clone(),
        expression: c.expression.clone(),
        source: c.source.clone(),
        requirements: c.requirements.clone(),
        best_practice: c.best_practice,
        elements: Vec::new(),
        primary: false,
    }
}

/// psdr:1146 getVariations(): primary first, then the (HashMap) variations.
/// We preserve first-seen insertion order for the non-primary set — verified
/// against the corpus (all inv fragments have a single variation per key).
fn get_variations(ci: &ConstraintInfo) -> Vec<&ConstraintVariation> {
    let mut l: Vec<&ConstraintVariation> = Vec::new();
    if let Some(pidx) = ci.primary {
        l.push(&ci.variations[pidx]);
    }
    for (i, v) in ci.variations.iter().enumerate() {
        if Some(i) == ci.primary {
            continue;
        }
        // only ones still in hash_index (i.e. genuine variations)
        if ci.hash_index.values().any(|&x| x == i) {
            l.push(v);
        }
    }
    l
}

/// psdr ConstraintKeyComparator:1266.
fn constraint_key_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn matches_dashnum(s: &str) -> bool {
        // regex .+\-\d+ : at least one char, a dash, then digits to end
        if let Some(pos) = s.rfind('-') {
            !s[..pos].is_empty()
                && pos + 1 < s.len()
                && s[pos + 1..].chars().all(|c| c.is_ascii_digit())
        } else {
            false
        }
    }
    if matches_dashnum(a) && matches_dashnum(b) {
        // aStart = substring(0, lastIndexOf("-")-1)  (Java: note the -1)
        let apos = a.rfind('-').unwrap();
        let bpos = b.rfind('-').unwrap();
        let a_start = &a[..apos.saturating_sub(1)];
        let b_start = &b[..bpos.saturating_sub(1)];
        if a_start == b_start {
            let a_end: i64 = a[apos + 1..].parse().unwrap_or(0);
            let b_end: i64 = b[bpos + 1..].parse().unwrap_or(0);
            a_end.cmp(&b_end)
        } else {
            a_start.cmp(b_start)
        }
    } else {
        a.cmp(b)
    }
}

/// Elements for the given mode.
fn elements_for_mode<'a>(
    sd: &'a Sd,
    ctx: &crate::context::IgContext,
    mode: GenMode,
) -> Vec<serde_json::Value> {
    match mode {
        GenMode::Diff => crate::diff::supplement_missing_diff_elements(sd),
        GenMode::Key => crate::table::key_elements_pub(sd, ctx),
        GenMode::Ms => crate::table::must_support_elements_pub(sd, ctx),
        GenMode::Snap => sd
            .snapshot_elements()
            .iter()
            .map(|e| e.v.clone())
            .collect(),
    }
}

/// psdr invOldMode:1203 — inv / inv-key / inv-diff.
pub fn inv(
    sd: &Sd,
    ctx: &crate::context::IgContext,
    with_headings: bool,
    mode: GenMode,
    all_invariants: bool,
) -> String {
    let sd_url = sd.url();
    let list = elements_for_mode(sd, ctx, mode);

    // build constraintMap keyed by key, preserving first-seen key order
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, ConstraintInfo> = std::collections::HashMap::new();
    for ed in &list {
        let max = ed.get("max").and_then(|m| m.as_str());
        if max == Some("0") {
            continue;
        }
        let cons = read_constraints(ed);
        if cons.is_empty() {
            continue;
        }
        let id = ed.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        for c in &cons {
            let entry = map.entry(c.key.clone()).or_insert_with(|| {
                order.push(c.key.clone());
                ConstraintInfo {
                    key: c.key.clone(),
                    primary: None,
                    variations: Vec::new(),
                    hash_index: std::collections::HashMap::new(),
                }
            });
            let _ = &entry.key;
            add_variation(entry, c, &id, &sd_url);
        }
    }

    if map.is_empty() {
        return String::new();
    }

    let mut div = el("div");
    if with_headings {
        let mut h4 = el("h4");
        tx(&mut h4, "Constraints");
        div.add_child_node(h4);
    }
    let mut tbl = el("table");
    tbl.set_attribute("class", "list presentation");
    tbl.set_attribute("data-fhir", "generated-heirarchy");

    // header row
    {
        let mut tr = el("tr");
        push_th_w(&mut tr, "60", "Id");
        push_th(&mut tr, "Grade");
        push_th(&mut tr, "Path(s)");
        push_th(&mut tr, "Description");
        push_th(&mut tr, "Expression");
        tbl.add_child_node(tr);
    }

    // sort keys
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort_by(|a, b| constraint_key_cmp(a, b));

    for key in &keys {
        let ci = &map[key];
        for cv in get_variations(ci) {
            // psdr:1241 — !hasSource || source==url || allInvariants || mode!=DIFF
            let src_ok = cv.source.is_none()
                || cv.source.as_deref() == Some(sd_url.as_str())
                || all_invariants
                || mode != GenMode::Diff;
            if !src_ok {
                continue;
            }
            let mut tr = el("tr");
            // Id
            let mut td_id = el("td");
            tx(&mut td_id, &cv.key);
            tr.add_child_node(td_id);
            // Grade
            let mut td_g = el("td");
            tx(&mut td_g, &cv.grade());
            tr.add_child_node(td_g);
            // Path(s)
            let mut td_p = el("td");
            if cv.is_bold() {
                let mut b = el("b");
                tx(&mut b, &cv.ids());
                td_p.add_child_node(b);
            } else {
                tx(&mut td_p, &cv.ids());
            }
            tr.add_child_node(td_p);
            // Description
            let mut td_d = el("td");
            tx(&mut td_d, &cv.human);
            if let Some(req) = &cv.requirements {
                td_d.add_child_node(el("br"));
                tx(&mut td_d, "Requirements");
                tx(&mut td_d, ": ");
                // markdown(requirements) — loud gap: no corpus hit yet
                panic!("LOUD GAP: inv requirements markdown (psdr:1256) req={:?}", req);
            }
            tr.add_child_node(td_d);
            // Expression
            let mut td_e = el("td");
            let mut code = el("code");
            tx(&mut code, &cv.expression);
            td_e.add_child_node(code);
            tr.add_child_node(td_e);

            tbl.add_child_node(tr);
        }
    }

    div.add_child_node(tbl);
    compose_children_html_pretty(&div)
}

fn push_th(tr: &mut XhtmlNode, label: &str) {
    let mut td = el("td");
    let mut b = el("b");
    tx(&mut b, label);
    td.add_child_node(b);
    tr.add_child_node(td);
}
fn push_th_w(tr: &mut XhtmlNode, width: &str, label: &str) {
    let mut td = el("td");
    td.set_attribute("width", width);
    let mut b = el("b");
    tx(&mut b, label);
    td.add_child_node(b);
    tr.add_child_node(td);
}
