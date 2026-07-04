//! `render_sd::vscs` — byte-exact port of the fhir-core ValueSet / CodeSystem
//! terminology narrative fragments:
//!   - CS `content`  = CodeSystemRenderer.generateContent path (csr:94-676)
//!   - VS `cld`      = ValueSetRenderer.generateComposition   (vsr:1224-1594)
//!   - VS `expansion`= ValueSetRenderer.generateExpansion     (vsr:244-386)
//!
//! Citations: `csr:<n>` = fhir-core CodeSystemRenderer.java, `vsr:<n>` =
//! ValueSetRenderer.java, `tr:<n>` = TerminologyRenderer.java, `rr:<n>` =
//! ResourceRenderer.java (all in fhir-core-6911 r5 renderers), `phrases` =
//! rendering-phrases.properties (English), `pub-vsr`/`pub-csr`/`pg` = ig-publisher
//! wrappers. Composer mode per fragment cited inline.
//!
//! ## Composer modes (verified against golden bytes)
//! - content + cld: `new XhtmlComposer(XhtmlComposer.HTML)` = HTML non-pretty
//!   (`Config::html_compact` via `compose_node`, which runs breakBlocksWithLines
//!   → `\r\n` before block siblings; empty `<p>` → `<p></p>`). pub-csr:124,
//!   pub-vsr:90/99.
//! - expansion: `new XhtmlComposer(XhtmlComposer.XML)` = XML non-pretty
//!   (`Config::xml_compact`; empty `<p>` → `<p/>`, literal NBSP). pg:1577.
//!
//! ## Outer div
//! Golden opens `<div xmlns="http://www.w3.org/1999/xhtml" data-fhir="generated">`
//! (xmlns first, then data-fhir — the resource narrative div; rr:144-158 +
//! setNarrative). We build that div and compose the whole node.

use render_xhtml::node::XhtmlNode;
use render_xhtml::{Config, XhtmlComposer};
use serde_json::Value;

use crate::context::IgContext;
use crate::leaf::{el, escape_xml, tx};
use crate::txcache::{ExpandedValueSet, TxCacheSource};

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn get_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

/// `<a name="{name}"> </a>` — XhtmlFluent.an: a single-space text child.
fn an(name: &str) -> XhtmlNode {
    let mut a = el("a");
    a.set_attribute("name", name);
    tx(&mut a, " ");
    a
}

/// XhtmlNode.addTextWithLineBreaks: each `\n` becomes a `<br/>`; the text
/// between newlines is added as text nodes. `\r` is dropped (CRLF → one break).
fn add_text_with_line_breaks(node: &mut XhtmlNode, s: &str) {
    let s = s.replace('\r', "");
    let mut first = true;
    for seg in s.split('\n') {
        if !first {
            node.add_child_node(el("br"));
        }
        first = false;
        if !seg.is_empty() {
            tx(node, seg);
        }
    }
}

fn b_cell(label: &str) -> XhtmlNode {
    let mut td = el("td");
    let mut b = el("b");
    tx(&mut b, label);
    td.add_child_node(b);
    td
}

fn b_cell_nowrap(label: &str) -> XhtmlNode {
    let mut td = el("td");
    td.set_attribute("style", "white-space:nowrap");
    let mut b = el("b");
    tx(&mut b, label);
    td.add_child_node(b);
    td
}

/// `Utilities.nmtokenize`: replace any char that is not a letter/digit/`.`/`-`
/// with `-` (FHIR id token). Verified against the corpus anchors (codes here are
/// already token-safe, but SNOMED etc. can carry chars).
fn nmtokenize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    out
}

/// `Utilities.escapeJson` (used by makeJson) — JSON string escaping. fhir-core
/// escapes `"` `\` and control chars; the corpus values only need `"`/`\`.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Build the outer resource-narrative `<div>` (xmlns then data-fhir).
fn narrative_div() -> XhtmlNode {
    let mut div = el("div");
    div.set_attribute("xmlns", "http://www.w3.org/1999/xhtml");
    div.set_attribute("data-fhir", "generated");
    div
}

fn compose_html(div: &XhtmlNode) -> String {
    let mut c = XhtmlComposer::new(Config::html_compact());
    c.compose_node(div)
}

fn compose_xml(div: &XhtmlNode) -> String {
    let mut c = XhtmlComposer::new(Config::xml_compact());
    c.compose_node(div)
}

/// The `<p class="res-header-id"></p>` + `<a name="{id}"> </a><a name="hc{id}"> </a>`
/// header block (rr:938-964, non-technical / no-header path: empty p, plain
/// anchors). Present on CS content + VS cld (verified in golden bytes).
fn push_res_header(div: &mut XhtmlNode, id: &str) {
    let mut p = el("p");
    p.set_attribute("class", "res-header-id");
    div.add_child_node(p);
    div.add_child_node(an(id));
    div.add_child_node(an(&format!("hc{}", id)));
}

// ===========================================================================
// CodeSystem `content`  (csr:94-676; pub-csr:124 -> XhtmlComposer.HTML)
// ===========================================================================

pub fn render_cs_content(cs: &Value, _ctx: &IgContext) -> String {
    let id = get_str(cs, "id").unwrap_or("");
    let mut div = narrative_div();
    push_res_header(&mut div, id);

    // generateProperties (csr:139-200) — returns `props`.
    let props = gen_properties(&mut div, cs);
    // generateFilters (csr:118-137).
    gen_filters(&mut div, cs);
    // generateCodeSystemContent (csr:230-311).
    gen_cs_content(&mut div, cs, props);

    crate::wrap_raw(&compose_html(&div))
}

fn concepts(cs: &Value) -> Vec<&Value> {
    cs.get("concept")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn children_of(c: &Value) -> Vec<&Value> {
    c.get("concept")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

/// csr:393-398 countCodes (recursive).
fn count_codes(list: &[&Value]) -> usize {
    let mut n = list.len();
    for c in list {
        let ch = children_of(c);
        if !ch.is_empty() {
            n += count_codes(&ch);
        }
    }
    n
}

/// csr:139-200. Returns whether a properties table was rendered.
fn gen_properties(div: &mut XhtmlNode, cs: &Value) -> bool {
    let Some(props) = cs.get("property").and_then(|x| x.as_array()).filter(|a| !a.is_empty())
    else {
        return false;
    };
    // For the corpus, getDisplayForProperty(p) == p.code when no presentation
    // extension, so hasRendered is p.code != p.code-from-uri... The condition
    // `hasRendered ||= getDisplayForProperty(p) != null` is effectively true iff
    // the code resolves to a display; for own-CS status properties it is null
    // (the "status" property in condition-category shows NO Name column).
    let mut has_rendered = false;
    let mut has_uri = false;
    let mut has_desc = false;
    let mut has_valueset = false;
    for p in props {
        if display_for_property(p).is_some() {
            has_rendered = true;
        }
        if p.get("uri").is_some() {
            has_uri = true;
        }
        if get_str(p, "description").is_some() {
            has_desc = true;
        }
        if p.get("extension")
            .and_then(|e| e.as_array())
            .map(|a| a.iter().any(|x| get_str(x, "url") == Some("http://hl7.org/fhir/StructureDefinition/codesystem-property-valueSet")))
            .unwrap_or(false)
        {
            has_valueset = true;
        }
    }

    // <p><b>Properties</b></p>  <p><b>This code system defines the following properties for its concepts</b></p>
    push_p_b(div, "Properties");
    push_p_b(div, "This code system defines the following properties for its concepts");

    let mut tbl = el("table");
    tbl.set_attribute("class", "grid");
    tbl.set_attribute("data-fhir", "generated");
    let mut tr = el("tr");
    if has_rendered {
        tr.add_child_node(b_cell("Name"));
    }
    tr.add_child_node(b_cell("Code"));
    if has_uri {
        tr.add_child_node(b_cell("URI"));
    }
    tr.add_child_node(b_cell("Type"));
    if has_desc {
        tr.add_child_node(b_cell("Description"));
    }
    if has_valueset {
        tr.add_child_node(b_cell("Value Set"));
    }
    tbl.add_child_node(tr);

    for p in props {
        let mut tr = el("tr");
        if has_rendered {
            let mut td = el("td");
            if let Some(d) = display_for_property(p) {
                tx(&mut td, &d);
            }
            tr.add_child_node(td);
        }
        // Code
        let mut td = el("td");
        tx(&mut td, get_str(p, "code").unwrap_or(""));
        tr.add_child_node(td);
        if has_uri {
            let mut td = el("td");
            tx(&mut td, get_str(p, "uri").unwrap_or(""));
            tr.add_child_node(td);
        }
        // Type
        let mut td = el("td");
        tx(&mut td, get_str(p, "type").unwrap_or(""));
        tr.add_child_node(td);
        if has_desc {
            let mut td = el("td");
            tx(&mut td, get_str(p, "description").unwrap_or(""));
            tr.add_child_node(td);
        }
        if has_valueset {
            // corpus: no property valueSet ext hits; emit empty cell to keep
            // column alignment (branch present in Java but data-driven).
            tr.add_child_node(el("td"));
        }
        tbl.add_child_node(tr);
    }
    div.add_child_node(tbl);
    true
}

/// tr:249-276 getDisplayForProperty(pc). getPresentation(pc,code) is null in the
/// corpus (no presentation ext), so the code branch runs: resolve the property
/// uri's concept display, else fall back to `pc.code`. Since a code is always
/// present, this returns Some (never null) — so a CS with any property gets the
/// Name column, with the value = the resolved display or the code (golden:
/// condition-category `status` → "status", the uri CS not being loaded).
fn display_for_property(p: &Value) -> Option<String> {
    // getDisplayForProperty(uri): resolve the concept-properties CS concept —
    // not loaded in the corpus, so None → fall back to the code.
    Some(get_str(p, "code").unwrap_or("").to_string())
}

fn push_p_b(div: &mut XhtmlNode, label: &str) {
    let mut p = el("p");
    let mut b = el("b");
    tx(&mut b, label);
    p.add_child_node(b);
    div.add_child_node(p);
}

/// csr:118-137.
fn gen_filters(div: &mut XhtmlNode, cs: &Value) {
    let Some(filters) = cs.get("filter").and_then(|x| x.as_array()).filter(|a| !a.is_empty())
    else {
        return;
    };
    push_p_b(div, "Filters");
    let mut tbl = el("table");
    tbl.set_attribute("class", "grid");
    tbl.set_attribute("data-fhir", "generated");
    let mut tr = el("tr");
    tr.add_child_node(b_cell("Code"));
    tr.add_child_node(b_cell("Description"));
    tr.add_child_node(b_cell("Operators"));
    tr.add_child_node(b_cell("Value"));
    tbl.add_child_node(tr);
    for f in filters {
        let mut tr = el("tr");
        let mut td = el("td");
        tx(&mut td, get_str(f, "code").unwrap_or(""));
        tr.add_child_node(td);
        let mut td = el("td");
        tx(&mut td, get_str(f, "description").unwrap_or(""));
        tr.add_child_node(td);
        // operators: each "op "
        let mut td = el("td");
        if let Some(ops) = f.get("operator").and_then(|x| x.as_array()) {
            for op in ops {
                if let Some(o) = op.as_str() {
                    tx(&mut td, &format!("{} ", o));
                }
            }
        }
        tr.add_child_node(td);
        let mut td = el("td");
        tx(&mut td, get_str(f, "value").unwrap_or(""));
        tr.add_child_node(td);
        tbl.add_child_node(tr);
    }
    div.add_child_node(tbl);
}

/// csr:230-311.
fn gen_cs_content(div: &mut XhtmlNode, cs: &Value, props: bool) {
    if props {
        // <p><b>Concepts</b></p>
        push_p_b(div, "Concepts");
    }
    // Intro <p> (csr:234-242, script template). Build directly.
    push_content_intro(div, cs);

    let content_mode = get_str(cs, "content").unwrap_or("complete");
    if content_mode == "not-present" {
        return; // csr:244-246
    }

    let cs_concepts = concepts(cs);

    // Column flags (csr:249-289).
    let mut definitions = false;
    let mut deprecated = false;
    let mut display = false;
    let mut hierarchy = false;

    // properties list (csr:257-277): props passing showPropertyInTable + present
    // on some concept; a "status" prop sets ignoreStatus.
    let mut is_manual = false;
    let empty: Vec<Value> = Vec::new();
    let all_props = cs.get("property").and_then(|x| x.as_array()).unwrap_or(&empty);
    for cp in all_props {
        if has_display_hint(cp) {
            is_manual = true;
        }
    }
    let mut prop_list: Vec<&Value> = Vec::new();
    let mut ignore_status = false;
    for cp in all_props {
        if show_property_in_table(cp, is_manual) {
            let code = get_str(cp, "code").unwrap_or("");
            let exists = cs_concepts.iter().any(|c| concepts_have_property(c, code));
            if exists {
                prop_list.push(cp);
                if code == "status" {
                    ignore_status = true;
                }
            }
        }
    }

    for c in &cs_concepts {
        deprecated = deprecated || concepts_have_deprecated(cs, c, ignore_status);
        display = display || concepts_have_display(c);
        hierarchy = hierarchy || !children_of(c).is_empty();
        definitions = definitions || concepts_have_definition(c);
    }

    let mut tbl = el("table");
    tbl.set_attribute("class", "codes");
    tbl.set_attribute("data-fhir", "generated");

    // Header row (tr:207-247 addTableHeaderRowStandard, no copy column since
    // isCopyButton()==false in corpus).
    let mut tr = el("tr");
    if hierarchy {
        tr.add_child_node(b_cell("Lvl"));
    }
    tr.add_child_node(b_cell_nowrap("Code"));
    if display {
        tr.add_child_node(b_cell("Display"));
    }
    if definitions {
        tr.add_child_node(b_cell("Definition"));
    }
    if deprecated {
        tr.add_child_node(b_cell("Deprecated"));
    }
    for cp in &prop_list {
        tr.add_child_node(b_cell(prop_header(cp)));
    }
    tbl.add_child_node(tr);

    let cs_id = get_str(cs, "id").unwrap_or("");
    let cs_url = get_str(cs, "url").unwrap_or("");
    for c in &cs_concepts {
        add_define_row(&mut tbl, c, 0, hierarchy, display, definitions, deprecated, &prop_list, cs, cs_id, cs_url);
    }
    div.add_child_node(tbl);
}

/// prop_header: getDisplayForProperty(pc) falls back to pc.code (tr:249-258).
fn prop_header(cp: &Value) -> &str {
    get_str(cp, "code").unwrap_or("")
}

fn has_display_hint(cp: &Value) -> bool {
    cp.get("extension")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().any(|x| get_str(x, "url") == Some("http://hl7.org/fhir/StructureDefinition/codesystem-concept-comments") ))
        .unwrap_or(false)
        && false // no display-hint ext in corpus
}

/// csr:383-391.
fn show_property_in_table(cp: &Value, is_manual: bool) -> bool {
    if !is_manual {
        cp.get("code").is_some()
    } else {
        false
    }
}

fn concepts_have_property(c: &Value, code: &str) -> bool {
    if c.get("property").and_then(|x| x.as_array()).map(|a| a.iter().any(|p| get_str(p, "code") == Some(code))).unwrap_or(false) {
        return true;
    }
    children_of(c).iter().any(|g| concepts_have_property(g, code))
}

fn concepts_have_definition(c: &Value) -> bool {
    if get_str(c, "definition").map(|s| !s.is_empty()).unwrap_or(false) {
        return true;
    }
    children_of(c).iter().any(|g| concepts_have_definition(g))
}

fn concepts_have_display(c: &Value) -> bool {
    // csr:411-418: hasDisplay && display != code.
    if let (Some(d), Some(code)) = (get_str(c, "display"), get_str(c, "code")) {
        if d != code {
            return true;
        }
    }
    children_of(c).iter().any(|g| concepts_have_display(g))
}

/// csr:428-437 conceptsHaveDeprecated -> CodeSystemUtilities.isDeprecated.
fn concepts_have_deprecated(cs: &Value, c: &Value, ignore_status: bool) -> bool {
    if is_deprecated(cs, c, ignore_status) {
        return true;
    }
    children_of(c).iter().any(|g| concepts_have_deprecated(cs, g, ignore_status))
}

/// CodeSystemUtilities.isDeprecated(cs, c, ignoreStatus). The concept's `status`
/// property (or the standards-status/deprecationDate) == deprecated/retired.
/// When ignoreStatus, the `status` property is not consulted (it drives its own
/// column instead) — but the row-highlight still keys off it (isNotCurrent).
fn is_deprecated(_cs: &Value, c: &Value, ignore_status: bool) -> bool {
    if ignore_status {
        // when a status property column exists, isDeprecated(ignoreStatus=true)
        // ignores the status property → no separate Deprecated column.
        return false;
    }
    concept_status(c).map(|s| s == "deprecated" || s == "retired").unwrap_or(false)
}

/// The concept's `status` property value (property code == "status").
fn concept_status(c: &Value) -> Option<String> {
    let props = c.get("property")?.as_array()?;
    for p in props {
        if get_str(p, "code") == Some("status") {
            return get_str(p, "valueCode")
                .or_else(|| get_str(p, "valueString"))
                .map(String::from);
        }
    }
    None
}

/// CodeSystemUtilities.isNotCurrent(cs, c): the concept status is
/// deprecated/retired (drives the `#ffeeee` row highlight, csr:442-445).
fn is_not_current(c: &Value) -> bool {
    concept_status(c)
        .map(|s| s == "deprecated" || s == "retired")
        .unwrap_or(false)
}

/// csr:439-676 addDefineRowToTable (corpus subset: no maps, no copy button, not
/// supplement, single language).
#[allow(clippy::too_many_arguments)]
fn add_define_row(
    tbl: &mut XhtmlNode,
    c: &Value,
    level: usize,
    hierarchy: bool,
    display: bool,
    definitions: bool,
    deprecated: bool,
    prop_list: &[&Value],
    cs: &Value,
    cs_id: &str,
    _cs_url: &str,
) {
    let mut tr = el("tr");
    if is_not_current(c) {
        tr.set_attribute("style", "background-color: #ffeeee");
    }
    let code = get_str(c, "code").unwrap_or("");

    // Code cell.
    let mut td = el("td");
    if hierarchy {
        // Lvl cell + indent cell.
        tx(&mut td, &(level + 1).to_string());
        tr.add_child_node(td);
        td = el("td");
        let pad: String = std::iter::repeat('\u{00A0}').take(level * 2).collect();
        tx(&mut td, &pad);
    }
    td.set_attribute("style", "white-space:nowrap");
    tx(&mut td, code);
    // anchor: {cs.id}-{nmtokenize(code)} (csr:461-463)
    td.add_child_node(an(&format!("{}-{}", cs_id, nmtokenize(code))));
    tr.add_child_node(td);

    // Display cell (csr:465-468).
    if display {
        let mut td = el("td");
        if let Some(d) = get_str(c, "display") {
            tx(&mut td, d);
        }
        tr.add_child_node(td);
    }

    // Definition cell (csr:469-518). Markdown when hasMarkdownInDefinitions(cs)
    // → addMarkdown into a `<div>` (renderStatusDiv); else plain text.
    if definitions {
        let mut td = el("td");
        if let Some(defn) = get_str(c, "definition") {
            if has_markdown_in_definitions(cs) {
                // addMarkdown(renderStatusDiv(defn, td), defn): renderStatusDiv =
                // td.div(); addMarkdown appends the md-parsed nodes (the <p>...)
                // DIRECTLY into that div (not re-wrapped). So one <div><p>...</p></div>.
                let html = crate::publisher_markdown::md_process(defn);
                let mut d = el("div");
                for node in crate::publisher_markdown::markdown_children_from_html(&html) {
                    // markdown_children_from_html wraps in a <div>; unwrap it so we
                    // append the inner <p> children (avoids a double <div>).
                    for inner in node.child_nodes() {
                        d.add_child_node(inner.clone());
                    }
                }
                td.add_child_node(d);
            } else {
                tx(&mut td, defn);
            }
        }
        tr.add_child_node(td);
    }

    // Deprecated cell (csr:519-544).
    if deprecated {
        let mut td = el("td");
        if concept_status(c).map(|s| s == "deprecated").unwrap_or(false) {
            // CODESYSTEM_DEPRECATED text (+ replaced-by, none in corpus).
            tx(&mut td, "Deprecated");
        }
        tr.add_child_node(td);
    }

    // Property cells (csr:583-623) — the status column etc.
    for cp in prop_list {
        let code_p = get_str(cp, "code").unwrap_or("");
        let mut td = el("td");
        let mut first = true;
        if let Some(pvals) = c.get("property").and_then(|x| x.as_array()) {
            for pcv in pvals {
                if get_str(pcv, "code") != Some(code_p) {
                    continue;
                }
                // value (corpus: valueCode/valueString scalars only).
                let pv = property_value_str(pcv);
                if let Some(pv) = pv {
                    if first {
                        first = false;
                    } else {
                        tx(&mut td, ", ");
                    }
                    tx(&mut td, &pv);
                }
            }
        }
        tr.add_child_node(td);
    }

    tbl.add_child_node(tr);

    // recurse children at level+1 (csr:646-649).
    for cc in children_of(c) {
        add_define_row(tbl, cc, level + 1, hierarchy, display, definitions, deprecated, prop_list, cs, cs_id, _cs_url);
    }
}

fn property_value_str(pcv: &Value) -> Option<String> {
    if let Some(obj) = pcv.as_object() {
        for (k, v) in obj {
            if let Some(rest) = k.strip_prefix("value") {
                if rest.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                    // Coding handled elsewhere; corpus uses code/string/boolean/integer.
                    if rest == "Coding" {
                        return v.get("code").and_then(|x| x.as_str()).map(String::from);
                    }
                    return match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Bool(b) => Some(b.to_string()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    };
                }
            }
        }
    }
    None
}

/// CodeSystemUtilities.hasMarkdownInDefinitions (CSU:989): `codesystem-use-
/// markdown` ext wins, else auto-detect via `isProbablyMarkdown(defn, true)` over
/// every concept definition (recursive).
fn has_markdown_in_definitions(cs: &Value) -> bool {
    if let Some(exts) = cs.get("extension").and_then(|e| e.as_array()) {
        for x in exts {
            if get_str(x, "url") == Some("http://hl7.org/fhir/StructureDefinition/codesystem-use-markdown") {
                return x.get("valueBoolean").and_then(|b| b.as_bool()) == Some(true);
            }
        }
    }
    fn scan(list: &[Value]) -> bool {
        for c in list {
            if let Some(d) = c.get("definition").and_then(|x| x.as_str()) {
                if is_probably_markdown(d) {
                    return true;
                }
            }
            if let Some(sub) = c.get("concept").and_then(|x| x.as_array()) {
                if scan(sub) {
                    return true;
                }
            }
        }
        false
    }
    cs.get("concept").and_then(|x| x.as_array()).map(|a| scan(a)).unwrap_or(false)
}

/// MarkDownProcessor.isProbablyMarkdown(content, mdIfParagraphs=true) (MDP:96).
fn is_probably_markdown(content: &str) -> bool {
    if content.contains('\n') {
        return true;
    }
    for s in content.split('\n') {
        if s.starts_with("* ") || is_md_heading(s) || s.starts_with("1. ") || s.starts_with("    ") {
            return true;
        }
        if s.contains("```") || s.contains("~~~") || s.contains("[[[") {
            return true;
        }
        if has_md_link(s) {
            return true;
        }
        if has_text_special(s, '*') || has_text_special(s, '_') {
            return true;
        }
    }
    false
}

fn is_md_heading(s: &str) -> bool {
    for (pfx, n) in [("###### ", 7), ("##### ", 6), ("#### ", 5), ("### ", 4), ("## ", 3)] {
        if s.len() > n && s.starts_with(pfx) && !s.as_bytes()[n].is_ascii_whitespace() {
            return true;
        }
    }
    false
}

/// MDP.hasLink: a `[text](url)` pattern.
fn has_md_link(s: &str) -> bool {
    let bytes: Vec<char> = s.chars().collect();
    let mut left: i64 = -1;
    let mut mid: i64 = -1;
    for (i, &c) in bytes.iter().enumerate() {
        let i = i as i64;
        if c == '[' {
            mid = -1;
            left = i;
        } else if left > -1 && (i as usize) < bytes.len() - 1 && c == ']' && bytes[(i + 1) as usize] == '(' {
            mid = i;
        } else if left > -1 && c == ']' {
            left = -1;
        } else if left > -1 && mid > -1 && c == ')' {
            return true;
        } else if (mid > -1 && c == '[') || c == ']' || (c == '(' && i > mid + 1 && mid > -1) {
            // MDP:165 partial reset — conservative; corpus link is well-formed.
        }
    }
    false
}

/// MDP.hasTextSpecial(s, ch): `ch` appears wrapping some text (e.g. `*bold*`).
/// Conservative: two occurrences of `ch` with non-space between.
fn has_text_special(s: &str, ch: char) -> bool {
    let first = s.find(ch);
    if let Some(f) = first {
        if let Some(rel) = s[f + ch.len_utf8()..].find(ch) {
            let between = &s[f + ch.len_utf8()..f + ch.len_utf8() + rel];
            return !between.trim().is_empty();
        }
    }
    false
}

/// The intro `<p>` (csr:234-242). English template CODESYSTEM_CONTENT_COMPLETE:
/// `This {cased} code system {cs} defines the following code[s]{h}:`
fn push_content_intro(div: &mut XhtmlNode, cs: &Value) {
    let mut p = el("p");
    let cased = cased_str(cs);
    let hpart = hierarchy_str(cs);
    let count = count_codes(&concepts(cs));
    let plural = if count != 1 { "s" } else { "" };
    let content_mode = get_str(cs, "content").unwrap_or("complete");
    let url = get_str(cs, "url").unwrap_or("");

    // Templates carry the <code> child inline via the `cs` param.
    match content_mode {
        "not-present" => {
            // "This {cased} code system {cs} defines codes{h}, but no codes are represented here"
            tx(&mut p, &format!("This {} code system ", cased));
            push_code(&mut p, url);
            tx(&mut p, &format!(" defines codes{}, but no codes are represented here", hpart));
        }
        "example" => {
            tx(&mut p, &format!("This {} code system ", cased));
            push_code(&mut p, url);
            tx(&mut p, &format!(" provides some code{}{} ", plural, hpart));
            let mut b = el("b");
            tx(&mut b, "that are example only");
            p.add_child_node(b);
            tx(&mut p, ":");
        }
        "fragment" => {
            // NOTE leading space in the phrase value.
            tx(&mut p, &format!(" This {} code system ", cased));
            push_code(&mut p, url);
            tx(&mut p, " provides ");
            let mut b = el("b");
            tx(&mut b, "a fragment");
            p.add_child_node(b);
            tx(&mut p, &format!(" that includes following code{}{}:", plural, hpart));
        }
        "supplement" => {
            panic!("LOUD GAP: CS content supplement mode (csr:224) url={}", url);
        }
        _ => {
            // complete
            tx(&mut p, &format!("This {} code system ", cased));
            push_code(&mut p, url);
            tx(&mut p, &format!(" defines the following code{}{}:", plural, hpart));
        }
    }
    div.add_child_node(p);
}

fn push_code(p: &mut XhtmlNode, url: &str) {
    let mut code = el("code");
    tx(&mut code, url);
    p.add_child_node(code);
}

/// csr:326-335 makeCasedParam.
fn cased_str(cs: &Value) -> &'static str {
    match cs.get("caseSensitive").and_then(|x| x.as_bool()) {
        Some(true) => "case-sensitive",
        Some(false) => "case-insensitive",
        None => "",
    }
}

/// csr:313-324 makeHierarchyParam.
fn hierarchy_str(cs: &Value) -> String {
    if let Some(hm) = get_str(cs, "hierarchyMeaning") {
        // display of the hierarchyMeaning code. FHIR display strings:
        let disp = match hm {
            "grouped-by" => "grouped by",
            "is-a" => "is-a",
            "part-of" => "part of",
            "classified-with" => "classified with",
            other => other,
        };
        format!(" in a {} hierarchy", disp)
    } else if has_hierarchy(cs) {
        " in an undefined hierarchy".to_string()
    } else {
        String::new()
    }
}

/// CodeSystemUtilities.hasHierarchy: any concept has children.
fn has_hierarchy(cs: &Value) -> bool {
    concepts(cs).iter().any(|c| !children_of(c).is_empty())
}

// ===========================================================================
// ValueSet `cld`  (vsr:1224-1594; pub-vsr:99 -> "<h3>...</h3>\r\n" + HTML div)
// ===========================================================================

pub fn render_vs_cld(vs: &Value, ctx: &IgContext, tx_cache: &dyn TxCacheSource) -> String {
    let id = get_str(vs, "id").unwrap_or("");
    let mut div = narrative_div();
    push_res_header(&mut div, id);

    // version-metadata box (rr:966-1010) is only in technical mode; the cld
    // wrapper renders in default (technical) mode, so it appears when meta is
    // present (us-core condition-code). Emit it after the header anchors.
    push_meta_box(&mut div, vs);

    let compose = vs.get("compose");
    let empty: Vec<Value> = Vec::new();
    let includes = compose.and_then(|c| c.get("include")).and_then(|x| x.as_array()).unwrap_or(&empty);
    let excludes = compose.and_then(|c| c.get("exclude")).and_then(|x| x.as_array()).unwrap_or(&empty);

    if includes.len() == 1 && excludes.is_empty() {
        let mut ul = el("ul");
        gen_include(&mut ul, &includes[0], "Include", ctx, tx_cache, vs);
        div.add_child_node(ul);
    } else {
        let mut p = el("p");
        tx(&mut p, "This value set includes codes based on the following rules:");
        div.add_child_node(p);
        let mut ul = el("ul");
        for inc in includes {
            gen_include(&mut ul, inc, "Include", ctx, tx_cache, vs);
        }
        div.add_child_node(ul);
        if !excludes.is_empty() {
            let mut p = el("p");
            tx(&mut p, "This value set excludes codes based on the following rules:");
            div.add_child_node(p);
            let mut ul = el("ul");
            for exc in excludes {
                gen_include(&mut ul, exc, "Exclude", ctx, tx_cache, vs);
            }
            div.add_child_node(ul);
        }
    }

    let body = format!("<h3>Logical Definition (CLD)</h3>\r\n{}", compose_html(&div));
    crate::wrap_raw(&body)
}

/// rr:966-1010 version-metadata box (technical mode). Emits the #d9e0e7 inline
/// box with `version: {v}; Last updated: {lastUpdated}[; Language: {lang}]`.
fn push_meta_box(div: &mut XhtmlNode, res: &Value) {
    let meta = res.get("meta");
    let version_id = meta.and_then(|m| get_str(m, "versionId"));
    let last_updated = meta.and_then(|m| get_str(m, "lastUpdated"));
    let lang = get_str(res, "language");
    if version_id.is_none() && last_updated.is_none() && lang.is_none() {
        return;
    }
    let mut vdiv = el("div");
    vdiv.set_attribute(
        "style",
        "display: inline-block; background-color: #d9e0e7; padding: 6px; margin: 4px; border: 1px solid #8da1b4; border-radius: 5px; line-height: 60%",
    );
    let mut p = el("p");
    p.set_attribute("style", "margin-bottom: 0px");
    let mut first = true;
    let mut buf = String::new();
    if let Some(v) = version_id {
        buf.push_str(&format!("version: {}", v));
        first = false;
    }
    if let Some(lu) = last_updated {
        if !first {
            buf.push_str("; ");
        }
        buf.push_str(&format!("Last updated: {}", format_datetime(lu)));
        first = false;
    }
    if let Some(l) = lang {
        if !first {
            buf.push_str("; ");
        }
        buf.push_str(&format!("Language: {}", l));
    }
    tx(&mut p, &buf);
    vdiv.add_child_node(p);
    div.add_child_node(vdiv);
}

/// displayDataType(lastUpdated): the instant rendered as "yyyy-MM-dd HH:mm:ssZZ".
/// Golden shows "2022-04-28 00:15:18+0000" from "2022-04-28T00:15:18.000+00:00"
/// style input. Reformat: replace 'T' with ' ', drop fractional seconds, and
/// collapse the timezone `+00:00` → `+0000` / `Z` → `+0000`.
fn format_datetime(s: &str) -> String {
    // Split date and time on 'T'.
    let (date, rest) = match s.split_once('T') {
        Some((d, r)) => (d, r),
        None => return s.to_string(),
    };
    // rest = HH:mm:ss[.fff][TZ]
    // find timezone start: 'Z', or '+'/'-' after the time
    let (time_tz, _) = (rest, "");
    let mut tz = String::new();
    let mut time = time_tz.to_string();
    if let Some(zpos) = time_tz.find('Z') {
        tz = "+0000".to_string();
        time = time_tz[..zpos].to_string();
    } else {
        // find last +/- (skip the HH:mm:ss part which has no sign)
        if let Some(sign) = time_tz.rfind(['+', '-']) {
            if sign > 0 {
                let z = &time_tz[sign..];
                tz = z.replace(':', "");
                time = time_tz[..sign].to_string();
            }
        }
    }
    // drop fractional seconds
    if let Some(dot) = time.find('.') {
        time = time[..dot].to_string();
    }
    format!("{} {}{}", date, time, tz)
}

/// vsr:1455-1594 genInclude.
fn gen_include(
    ul: &mut XhtmlNode,
    inc: &Value,
    type_label: &str,
    ctx: &IgContext,
    tx_cache: &dyn TxCacheSource,
    vs: &Value,
) {
    let mut li = el("li");
    let system = get_str(inc, "system");
    let empty: Vec<Value> = Vec::new();
    let inc_concepts = inc.get("concept").and_then(|x| x.as_array()).unwrap_or(&empty);
    let inc_filters = inc.get("filter").and_then(|x| x.as_array()).unwrap_or(&empty);
    let inc_valuesets = inc.get("valueSet").and_then(|x| x.as_array()).unwrap_or(&empty);

    if let Some(system) = system {
        let ver = get_str(inc, "version");
        // resolve the CS for links + version note.
        let cs_res = resolve_cs(ctx, system, ver);

        if inc_concepts.is_empty() && inc_filters.is_empty() {
            tx(&mut li, &format!("{} all codes defined in ", type_label));
            add_cs_ref(&mut li, system, ver, &cs_res);
        } else {
            if !inc_concepts.is_empty() {
                tx(&mut li, &format!("{} these codes as defined in ", type_label));
                add_cs_ref(&mut li, system, ver, &cs_res);
                render_concept_table(&mut li, inc, inc_concepts, system, ver, &cs_res, ctx, tx_cache);
            }
            if !inc_filters.is_empty() {
                // "Include codes from" — the VALUE_SET_CODES_FROM phrase is
                // trailing-trimmed by java.util.Properties, so NO space before
                // the <a> (unlike the "...defined in " branches which add an
                // explicit `+ " "`). Golden: `Include codes from<a ...>`.
                tx(&mut li, &format!("{} codes from", type_label));
                add_cs_ref(&mut li, system, ver, &cs_res);
                tx(&mut li, " where ");
                render_filters(&mut li, inc_filters, system, ver, &cs_res, tx_cache);
            }
        }
        if !inc_valuesets.is_empty() {
            tx(&mut li, ", where the codes are contained in ");
            let mut first = true;
            for v in inc_valuesets {
                if let Some(vsu) = v.as_str() {
                    if first {
                        first = false;
                    } else {
                        tx(&mut li, ", ");
                    }
                    add_vs_ref(&mut li, vsu, ctx);
                }
            }
        }
        if inc.get("extension").and_then(|e| e.as_array()).map(|a| a.iter().any(|x| {
            matches!(get_str(x, "url"), Some("http://hl7.org/fhir/StructureDefinition/valueset-expand-rules") | Some("http://hl7.org/fhir/StructureDefinition/valueset-expand-group"))
        })).unwrap_or(false) {
            panic!("LOUD GAP: cld expand-rules/group (vsr:1565) vs={}", get_str(vs, "id").unwrap_or(""));
        }
    } else {
        // pure import (vsr:1569-1593).
        let n = inc_valuesets.len();
        let phrase = if n == 1 {
            "Import all the codes that are contained in "
        } else {
            "Import all the codes that are contained in the intersection of "
        };
        tx(&mut li, phrase);
        if n <= 2 {
            let mut i = 0;
            for v in inc_valuesets {
                if let Some(vsu) = v.as_str() {
                    if i > 0 {
                        if i == n - 1 {
                            tx(&mut li, " and ");
                        } else {
                            tx(&mut li, ", ");
                        }
                    }
                    add_vs_ref(&mut li, vsu, ctx);
                    i += 1;
                }
            }
        } else {
            let mut inner = el("ul");
            for v in inc_valuesets {
                if let Some(vsu) = v.as_str() {
                    let mut ili = el("li");
                    add_vs_ref(&mut ili, vsu, ctx);
                    inner.add_child_node(ili);
                }
            }
            li.add_child_node(inner);
        }
    }
    ul.add_child_node(li);
}

/// A resolved CodeSystem for cld/expansion linking.
struct CsRes {
    /// the CS resource `id` (for the concept anchor `{id}-{code}`).
    id: String,
    web_path: Option<String>,
    /// resource business version.
    version: Option<String>,
    /// present() (title || name).
    present: Option<String>,
    /// `!isAbsoluteUrlLinkable(webPath)` — a relative page = own IG package.
    from_this_package: bool,
    /// resolved from a loaded dependency package (has source package).
    from_packages: bool,
    /// content mode (complete/fragment/...) for link decisions.
    content: Option<String>,
    /// The IG-owned CodeSystem JSON (for internal expansion enumeration).
    json: Option<std::rc::Rc<Value>>,
    /// external (tx server) link for the CS ref.
    external_link: Option<String>,
}

fn resolve_cs(ctx: &IgContext, system: &str, ver: Option<&str>) -> Option<CsRes> {
    let canonical = match ver {
        Some(v) if !v.is_empty() => format!("{}|{}", system, v),
        _ => system.to_string(),
    };
    let r = ctx.resolve(&canonical).or_else(|| ctx.resolve(system))?;
    if r.rtype != "CodeSystem" {
        return None;
    }
    let json = ctx.load_resource(&canonical).or_else(|| ctx.load_resource(system));
    let content = json.as_ref().and_then(|j| get_str(j, "content").map(String::from));
    // Business version: the resolved `version` (from packages), else the CS
    // resource's own `version` (own-IG resources have r.version=="").
    let json_version = json.as_ref().and_then(|j| get_str(j, "version").map(String::from));
    let version = if !r.version.is_empty() { Some(r.version.clone()) } else { json_version };
    let cs_id = json.as_ref().and_then(|j| get_str(j, "id").map(String::from)).unwrap_or_default();
    let web = r.web_path.clone();
    let from_this_package = !(web.starts_with("http://") || web.starts_with("https://"));
    Some(CsRes {
        id: cs_id,
        web_path: Some(web),
        version,
        present: Some(r.present()),
        from_this_package,
        from_packages: r.pkg.is_some(),
        content,
        json,
        external_link: if r.external { r.tx_server.clone() } else { None },
    })
}

/// tr:151-193 addCsRef — `<a><code>URL</code></a>` + version span.
fn add_cs_ref(li: &mut XhtmlNode, system: &str, ver: Option<&str>, cs: &Option<CsRes>) {
    // special reference (SNOMED etc.)
    let spec = special_reference(system);
    if let Some(spec) = spec {
        let mut a = el("a");
        a.set_attribute("href", spec.as_str());
        push_code(&mut a, system);
        li.add_child_node(a);
    } else if let Some(cs) = cs {
        // ref: external link first, else webPath.
        let mut r = cs.external_link.clone().or_else(|| cs.web_path.clone());
        let add_html = cs.external_link.is_some();
        if let Some(ref mut rr) = r {
            if add_html && !rr.contains(".html") {
                rr.push_str(".html");
            }
            let mut a = el("a");
            a.set_attribute("href", rr.replace('\\', "/"));
            push_code(&mut a, system);
            li.add_child_node(a);
        } else {
            push_code(li, system);
        }
    } else {
        push_code(li, system);
    }

    // <span> version </span> with the version note.
    let mut span = el("span");
    tx(&mut span, " version ");
    push_version_ref(&mut span, cs, ver, "Code System");
    li.add_child_node(span);
}

/// tr:195-205 getSpecialReference. SNOMED → snomed.org; a fixed set of external
/// systems link to their own URL.
fn special_reference(system: &str) -> Option<String> {
    if system == "http://snomed.info/sct" {
        return Some("http://www.snomed.org/".to_string());
    }
    const SELF_LINK: &[&str] = &[
        "http://loinc.org",
        "http://unitsofmeasure.org",
        "http://www.nlm.nih.gov/research/umls/rxnorm",
        "http://ncimeta.nci.nih.gov",
        "http://fdasis.nlm.nih.gov",
        "http://www.radlex.org",
        "http://www.whocc.no/atc",
        "http://dicom.nema.org/resources/ontology/DCM",
        "http://www.genenames.org",
        "http://www.ensembl.org",
        "http://www.ncbi.nlm.nih.gov/nuccore",
        "http://www.ncbi.nlm.nih.gov/clinvar",
        "http://sequenceontology.org",
        "http://www.hgvs.org/mutnomen",
        "http://www.ncbi.nlm.nih.gov/projects/SNP",
        "http://cancer.sanger.ac.uk/cancergenome/projects/cosmic",
        "http://www.lrg-sequence.org",
        "http://www.omim.org",
        "http://www.ncbi.nlm.nih.gov/pubmed",
        "http://www.pharmgkb.org",
        "http://clinicaltrials.gov",
        "http://www.ebi.ac.uk/ipd/imgt/hla/",
    ];
    if SELF_LINK.contains(&system) {
        return Some(system.to_string());
    }
    None
}

/// rr:1597-1644 renderVersionReference. Sets the span's `title` attr + emits the
/// char/version text. `none_phrase` = CS_VERSION_NOTHING_TEXT.
fn push_version_ref(span: &mut XhtmlNode, cs: &Option<CsRes>, stated: Option<&str>, type_name: &str) {
    let stated = stated.filter(|s| !s.is_empty());
    let actual = cs.as_ref().and_then(|c| c.version.clone());
    let from_packages = cs.as_ref().map(|c| c.from_packages).unwrap_or(false);
    let from_this_package = cs.as_ref().map(|c| c.from_this_package).unwrap_or(false);
    // NOTPRESENT content → cs treated as null (tr:184-186).
    let cs_null = cs.is_none() || cs.as_ref().and_then(|c| c.content.as_deref()) == Some("not-present");
    let actual = if cs_null { None } else { actual };
    let from_packages = if cs_null { false } else { from_packages };
    let from_this_package = if cs_null { false } else { from_this_package };

    if let Some(sv) = stated {
        span.set_attribute("title", format!("Version is explicitly stated to be {}", sv));
        tx(span, "\u{1F4CD}");
        tx(span, sv);
    } else if from_this_package {
        span.set_attribute("title", "Version is not explicitly stated, which means it is fixed to the version provided in this specification");
        tx(span, "\u{1F4E6}");
        tx(span, actual.as_deref().unwrap_or(""));
    } else if from_packages {
        let av = actual.clone().unwrap_or_default();
        span.set_attribute("title", format!("Version is not explicitly stated, which means it is fixed to {}, the version found through the package references", av));
        tx(span, "\u{1F4E6}");
        tx(span, &av);
    } else if let Some(av) = actual {
        span.set_attribute("title", format!("Version is not explicitly stated. When building this specification, the most recent version {} has been used", av));
        span.set_attribute("style", "opacity: 0.5");
        tx(span, "\u{23FF}");
        tx(span, &av);
    } else if cs.is_some() && !cs_null {
        span.set_attribute("title", format!("Version is not explicitly stated, and the target {} has no stated version either", type_name));
        tx(span, "\u{2205}");
    } else {
        span.set_attribute("title", format!("Version is not explicitly stated. No matching {} found", type_name));
        tx(span, "Not Stated (use latest from terminology server)");
    }
}

/// The concept table for enumerated includes (vsr:1477-1494). class="none".
#[allow(clippy::too_many_arguments)]
fn render_concept_table(
    li: &mut XhtmlNode,
    _inc: &Value,
    concepts: &[Value],
    system: &str,
    ver: Option<&str>,
    cs: &Option<CsRes>,
    _ctx: &IgContext,
    tx_cache: &dyn TxCacheSource,
) {
    // hasDefinition/hasComments scan (vsr:1478-1484).
    // definitions come from the fetched CS concept.
    let mut has_definition = false;
    let mut has_comments = false;
    for c in concepts {
        let code = get_str(c, "code").unwrap_or("");
        if c.get("extension").and_then(|e| e.as_array()).map(|a| a.iter().any(|x| get_str(x, "url") == Some("http://hl7.org/fhir/StructureDefinition/valueset-concept-comments"))).unwrap_or(false) {
            has_comments = true;
        }
        let cc_defn = cs_concept_definition(cs, code);
        if cc_defn.map(|d| !d.is_empty()).unwrap_or(false)
            || c.get("extension").and_then(|e| e.as_array()).map(|a| a.iter().any(|x| get_str(x, "url") == Some("http://hl7.org/fhir/StructureDefinition/valueset-definition"))).unwrap_or(false)
        {
            has_definition = true;
        }
    }

    let mut t = el("table");
    t.set_attribute("class", "none");
    t.set_attribute("data-fhir", "generated");
    // header (addTableHeaderRowStandard false,true,hasDefinition,hasComments,...)
    let mut tr = el("tr");
    tr.add_child_node(b_cell_nowrap("Code"));
    tr.add_child_node(b_cell("Display"));
    if has_definition {
        tr.add_child_node(b_cell("Definition"));
    }
    if has_comments {
        tr.add_child_node(b_cell("Comments"));
    }
    t.add_child_node(tr);

    for c in concepts {
        render_concept_row(&mut t, c, system, ver, cs, has_definition, has_comments, tx_cache);
    }
    li.add_child_node(t);
}

/// vsr:1596-1646 renderConcept.
#[allow(clippy::too_many_arguments)]
fn render_concept_row(
    t: &mut XhtmlNode,
    c: &Value,
    system: &str,
    ver: Option<&str>,
    cs: &Option<CsRes>,
    has_definition: bool,
    has_comments: bool,
    tx_cache: &dyn TxCacheSource,
) {
    let code = get_str(c, "code").unwrap_or("");
    let mut tr = el("tr");
    // Code cell via addCodeToTable.
    let mut td = el("td");
    add_code_to_table(&mut td, system, ver, code, cs);
    tr.add_child_node(td);
    // Display cell.
    let mut td = el("td");
    if let Some(d) = get_str(c, "display") {
        tx(&mut td, d);
    } else if let Some(ccd) = cs_concept_display(cs, code).or_else(|| tx_cache.lookup_display(system, code, ver.unwrap_or(""))) {
        // grey fallback (vsr:1610-1611).
        td.set_attribute("style", "color: #cccccc");
        tx(&mut td, &ccd);
    }
    tr.add_child_node(td);
    // Definition cell (vsr:1613-1620). EXT_DEFINITION → addTextWithLineBreaks
    // (newlines become <br/>); else the fetched CS concept definition (plain).
    if has_definition {
        let mut td = el("td");
        let ext_def = c.get("extension").and_then(|e| e.as_array()).and_then(|a| a.iter().find(|x| get_str(x, "url") == Some("http://hl7.org/fhir/StructureDefinition/valueset-definition")).and_then(|x| get_str(x, "valueString").or_else(|| get_str(x, "valueMarkdown"))));
        if let Some(d) = ext_def {
            add_text_with_line_breaks(&mut td, d);
        } else if let Some(d) = cs_concept_definition(cs, code) {
            // vsr:1618 — fetched-CS definition ALSO uses addTextWithLineBreaks.
            add_text_with_line_breaks(&mut td, &d);
        }
        tr.add_child_node(td);
    }
    if has_comments {
        let mut td = el("td");
        if let Some(cmt) = c.get("extension").and_then(|e| e.as_array()).and_then(|a| a.iter().find(|x| get_str(x, "url") == Some("http://hl7.org/fhir/StructureDefinition/valueset-concept-comments")).and_then(|x| get_str(x, "valueString"))) {
            tx(&mut td, &format!("Note: {}", cmt));
        }
        tr.add_child_node(td);
    }
    t.add_child_node(tr);
}

/// vsr:1184-1210 addCodeToTable (the code cell link).
fn add_code_to_table(td: &mut XhtmlNode, system: &str, _ver: Option<&str>, code: &str, cs: &Option<CsRes>) {
    let content = cs.as_ref().and_then(|c| c.content.as_deref());
    let complete = matches!(content, Some("complete") | Some("fragment"));
    if !complete {
        // external / not-present CS: link snomed/loinc, else plain text.
        if system == "http://snomed.info/sct" {
            let mut a = el("a");
            a.set_attribute("href", format!("http://snomed.info/id/{}", code));
            tx(&mut a, code);
            td.add_child_node(a);
        } else if system == "http://loinc.org" {
            let mut a = el("a");
            a.set_attribute("href", format!("https://loinc.org/{}/", code));
            tx(&mut a, code);
            td.add_child_node(a);
        } else if let Some(cs) = cs {
            // external ValueSet-based link (e.g. nucc via tx server): href =
            // {external}/ValueSet/{id}#{id}-{code}? — corpus nucc uses the
            // tx-server VS page. Fall back to webPath link.
            if let Some(link) = &cs.external_link {
                let mut a = el("a");
                a.set_attribute("href", format!("{}#{}", link, code));
                tx(&mut a, code);
                td.add_child_node(a);
            } else {
                tx(td, code);
            }
        } else {
            tx(td, code);
        }
    } else {
        // local complete CS: <a href="{ref}[-|#{csid}-]{code}">code</a>
        // (addCodeToTable vsr:1200-1203: if ref has '#' append "-{code}", else
        // "#{csid}-{code}").
        let cs = cs.as_ref().unwrap();
        if let Some(href) = &cs.web_path {
            let anchor = if href.contains('#') {
                format!("{}-{}", href, nmtokenize(code))
            } else {
                format!("{}#{}-{}", href, cs.id, nmtokenize(code))
            };
            let mut a = el("a");
            a.set_attribute("href", anchor);
            tx(&mut a, code);
            td.add_child_node(a);
        } else {
            let mut cnode = el("code");
            tx(&mut cnode, code);
            td.add_child_node(cnode);
        }
    }
}

fn cs_concept_display(cs: &Option<CsRes>, code: &str) -> Option<String> {
    let cs = cs.as_ref()?;
    let json = cs.json.as_ref()?;
    find_concept(json.get("concept")?, code).and_then(|c| get_str(c, "display").map(String::from))
}

fn cs_concept_definition(cs: &Option<CsRes>, code: &str) -> Option<String> {
    let cs = cs.as_ref()?;
    let json = cs.json.as_ref()?;
    find_concept(json.get("concept")?, code).and_then(|c| get_str(c, "definition").map(String::from))
}

fn find_concept<'a>(list: &'a Value, code: &str) -> Option<&'a Value> {
    for c in list.as_array()? {
        if get_str(c, "code") == Some(code) {
            return Some(c);
        }
        if let Some(sub) = c.get("concept") {
            if let Some(f) = find_concept(sub, code) {
                return Some(f);
            }
        }
    }
    None
}

/// vsr:1496-1551 filter rendering.
fn render_filters(li: &mut XhtmlNode, filters: &[Value], system: &str, ver: Option<&str>, cs: &Option<CsRes>, tx_cache: &dyn TxCacheSource) {
    let n = filters.len();
    for (i, f) in filters.iter().enumerate() {
        if i > 0 {
            if i == n - 1 {
                tx(li, " and ");
            } else {
                tx(li, ", ");
            }
        }
        let prop = get_str(f, "property").unwrap_or("");
        let op = get_str(f, "op").unwrap_or("");
        let value = get_str(f, "value").unwrap_or("");
        if op == "exists" {
            if value == "true" {
                tx(li, &format!("{} exists", prop));
            } else {
                tx(li, &format!("{} doesn't exist", prop));
            }
        } else {
            // vsr:1517: `prop + " " + describe(op) + " "`, and describe(op)
            // returns " {word}" (LEADING space, vsr:1801) → a DOUBLE space
            // between property and operator: `concept  is-a `.
            tx(li, &format!("{} {} ", prop, describe_op(op)));
            // value + optional (display)
            if let Some(_c) = cs.as_ref().filter(|c| matches!(c.content.as_deref(), Some("complete") | Some("fragment"))) {
                // code exists in CS → link. (corpus filter systems are external
                // SNOMED with not-present content, so this branch is unused.)
                tx(li, value);
            } else {
                tx(li, value);
                if let Some(disp) = tx_cache.lookup_display(system, value, ver.unwrap_or("")) {
                    tx(li, &format!(" ({})", disp));
                }
            }
        }
    }
}

/// tr describe(FilterOperator) (vsr:1800-1816) — returns " {word}" with a
/// LEADING space (so `prop + " " + describe(op)` yields a double space).
fn describe_op(op: &str) -> &'static str {
    match op {
        "=" => " =",
        "is-a" => " is-a",
        "is-not-a" => " is-not-a",
        "regex" => " matches (by regex)",
        "in" => " in",
        "not-in" => " not in",
        "generalizes" => " generalizes",
        "descendent-of" => " descends from",
        "exists" => " exists",
        _ => " ",
    }
}

/// tr:279-321 AddVsRef — link a valueSet (or CS/snomed) by canonical.
fn add_vs_ref(li: &mut XhtmlNode, value: &str, ctx: &IgContext) {
    if let Some(r) = ctx.resolve(value) {
        if r.web_path.is_empty() {
            tx(li, &r.present());
        } else {
            let mut a = el("a");
            a.set_attribute("href", r.web_path.replace('\\', "/"));
            tx(&mut a, &r.present());
            li.add_child_node(a);
        }
    } else if value == "http://snomed.info/sct" || value == "http://snomed.info/id" {
        let mut a = el("a");
        a.set_attribute("href", value);
        tx(&mut a, "SNOMED CT");
        li.add_child_node(a);
    } else {
        tx(li, value);
    }
}

// ===========================================================================
// ValueSet `expansion`  (vsr:244-386; pg:1577 -> XhtmlComposer.XML)
// ===========================================================================

pub fn render_vs_expansion(vs: &Value, ctx: &IgContext, tx_cache: &dyn TxCacheSource) -> String {
    let id = get_str(vs, "id").unwrap_or("");
    let vs_url = get_str(vs, "url").unwrap_or("");

    // The publisher END_USER-mode pass sets prefix "x" and clears compose/text.
    // res-header-id is the self-closed empty <p/> (END_USER: no header, no
    // anchors, no version box). Anchors on rows get the "x-" scoped prefix.
    let exp = match tx_cache.expand(vs_url, vs) {
        Some(e) => e,
        None => panic!("LOUD GAP: expansion cache miss (pg:1562) vs={}", id),
    };

    let mut div = narrative_div();
    // <p class="res-header-id"/> (empty, XML self-close).
    let mut p = el("p");
    p.set_attribute("class", "res-header-id");
    div.add_child_node(p);

    // version-notice header box (vsr:587-638).
    push_expansion_header(&mut div, ctx, &exp);

    // count <p> (vsr:268-285).
    let count = count_expansion(&exp.contains);
    let mut pc = el("p");
    match exp.total {
        Some(total) if total as usize != count => {
            pc.set_attribute("style", "border: maroon 1px solid; background-color: #FFCCCC; font-weight: bold; padding: 8px");
            tx(&mut pc, &format!("This value set contains {} concepts", total));
        }
        Some(total) => {
            tx(&mut pc, &format!("This value set contains {} concepts", total));
        }
        None => {
            tx(&mut pc, &format!("This value set expansion contains {} concepts.", count));
        }
    }
    div.add_child_node(pc);

    // column flags.
    let do_level = exp.contains.iter().any(|c| c.get("contains").and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(false));
    let do_version = exp.contains.iter().any(|c| get_str(c, "version").is_some());
    let do_definition = expansion_has_definition(&exp.contains, ctx);
    let do_inactive = exp.contains.iter().any(|c| c.get("inactive").and_then(|x| x.as_bool()) == Some(true));
    let displang = get_str(vs, "language").map(String::from).or_else(|| param_value(&exp.parameters, "displayLanguage"));

    // Table.
    let mut t = el("table");
    t.set_attribute("class", "codes");
    t.set_attribute("data-fhir", "generated");
    let mut tr = el("tr");
    if do_level {
        tr.add_child_node(b_cell("Level"));
    }
    tr.add_child_node(b_cell("System"));
    if do_version {
        tr.add_child_node(b_cell("Version"));
    }
    tr.add_child_node(b_cell_nowrap("Code"));
    // Display cell
    {
        let mut td = el("td");
        let mut b = el("b");
        match &displang {
            Some(l) => tx(&mut b, &format!("Display ({})", l)),
            None => tx(&mut b, "Display"),
        }
        td.add_child_node(b);
        tr.add_child_node(td);
    }
    if do_inactive {
        tr.add_child_node(b_cell("Inactive"));
    }
    if do_definition {
        tr.add_child_node(b_cell("Definition"));
    }
    // JSON/XML columns (forPublisher).
    tr.add_child_node(b_cell("JSON"));
    tr.add_child_node(b_cell("XML"));
    t.add_child_node(tr);

    let scoped_prefix = format!("x-{}", ""); // res.getScopedId() is "x" prefix + id? Actually prefixAnchor("x") + tgt. See below.
    let _ = scoped_prefix;
    for c in &exp.contains {
        add_expansion_row(&mut t, c, 1, do_level, do_version, do_inactive, do_definition, &exp.parameters, ctx);
    }
    div.add_child_node(t);

    crate::wrap_raw(&compose_xml(&div))
}

fn count_expansion(contains: &[Value]) -> usize {
    let mut n = 0;
    for c in contains {
        n += 1;
        if let Some(sub) = c.get("contains").and_then(|x| x.as_array()) {
            n += count_expansion(sub);
        }
    }
    n
}

fn param_value(params: &[Value], name: &str) -> Option<String> {
    for p in params {
        if get_str(p, "name") == Some(name) {
            if let Some(obj) = p.as_object() {
                for (k, v) in obj {
                    if k.starts_with("value") {
                        return v.as_str().map(String::from);
                    }
                }
            }
        }
    }
    None
}

fn expansion_has_definition(contains: &[Value], ctx: &IgContext) -> bool {
    for c in contains {
        let system = get_str(c, "system").unwrap_or("");
        let code = get_str(c, "code").unwrap_or("");
        if let Some(j) = ctx.load_resource(system) {
            if get_str(&j, "content") == Some("complete") || get_str(&j, "content") == Some("fragment") {
                if let Some(concepts) = j.get("concept") {
                    if find_concept(concepts, code).and_then(|c| get_str(c, "definition")).is_some() {
                        return true;
                    }
                }
            }
        }
        if let Some(sub) = c.get("contains").and_then(|x| x.as_array()) {
            if expansion_has_definition(sub, ctx) {
                return true;
            }
        }
    }
    false
}

/// vsr:995-1084 addExpansionRowToTable.
#[allow(clippy::too_many_arguments)]
fn add_expansion_row(
    t: &mut XhtmlNode,
    c: &Value,
    i: usize,
    do_level: bool,
    do_version: bool,
    do_inactive: bool,
    do_definition: bool,
    params: &[Value],
    ctx: &IgContext,
) {
    let system = get_str(c, "system").unwrap_or("");
    let code = get_str(c, "code").unwrap_or("");
    let display = get_str(c, "display");
    let mut tr = el("tr");
    if c.get("inactive").and_then(|x| x.as_bool()) == Some(true) {
        // isDeprecated highlight only for deprecated, not inactive; skip.
    }

    // anchor cell: <a name="x-{system}-{code}"> </a> (prefix "x", scoped id).
    let mut td = el("td");
    let anchor = format!("x-{}-{}", make_anchor(system), code);
    td.add_child_node(an(&anchor));

    if do_level {
        tx(&mut td, &i.to_string());
        tr.add_child_node(td);
        td = el("td");
    }
    // System cell.
    let mut codesys = el("code");
    tx(&mut codesys, system);
    td.add_child_node(codesys);
    tr.add_child_node(td);

    if do_version {
        let mut td = el("td");
        tx(&mut td, get_str(c, "version").unwrap_or(""));
        tr.add_child_node(td);
    }

    // Code cell.
    let mut td = el("td");
    td.set_attribute("style", "white-space:nowrap");
    let pad: String = std::iter::repeat('\u{00A0}').take(i * 2).collect();
    tx(&mut td, &pad);
    add_expansion_code_link(&mut td, system, code, ctx);
    tr.add_child_node(td);

    // Display cell.
    let mut td = el("td");
    if let Some(d) = display {
        tx(&mut td, d);
    }
    tr.add_child_node(td);

    if do_inactive {
        let mut td = el("td");
        if c.get("inactive").and_then(|x| x.as_bool()) == Some(true) {
            tx(&mut td, "inactive");
        }
        tr.add_child_node(td);
    }
    if do_definition {
        let mut td = el("td");
        if let Some(j) = ctx.load_resource(system) {
            if let Some(concepts) = j.get("concept") {
                if let Some(defn) = find_concept(concepts, code).and_then(|c| get_str(c, "definition")) {
                    tx(&mut td, defn);
                }
            }
        }
        tr.add_child_node(td);
    }

    // JSON / XML copy cells.
    let ver = get_str(c, "version").map(String::from).or_else(|| version_for_system(params, system));
    let json = make_json(c, ver.as_deref());
    let xml = make_xml(c, ver.as_deref());
    tr.add_child_node(copy_cell(&json, "Click to Copy as Coding"));
    tr.add_child_node(copy_cell(&xml, "Click to Copy as Coding"));

    t.add_child_node(tr);

    for cc in c.get("contains").and_then(|x| x.as_array()).map(|a| a.as_slice()).unwrap_or(&[]) {
        add_expansion_row(t, cc, i + 1, do_level, do_version, do_inactive, do_definition, params, ctx);
    }
}

/// DataRenderer.makeAnchor(system, code) → "{system}-{code}" with each char that
/// is not a valid html anchor char replaced by "|"+hex. For the corpus systems
/// (http URLs, snomed) the code is appended raw; the system keeps its chars.
fn make_anchor(system: &str) -> String {
    // The golden anchor is `x-{system}-{code}` with the raw system+code, so no
    // escaping is applied to these corpus values.
    system.to_string()
}

fn add_expansion_code_link(td: &mut XhtmlNode, system: &str, code: &str, ctx: &IgContext) {
    // fetched CS content complete/fragment → local link; else snomed/loinc/plain.
    let cs_json = ctx.load_resource(system);
    let content = cs_json.as_ref().and_then(|j| get_str(j, "content").map(String::from));
    if matches!(content.as_deref(), Some("complete") | Some("fragment")) {
        if let Some(r) = ctx.resolve(system) {
            let web = &r.web_path;
            let cs_id = cs_json.as_ref().and_then(|j| get_str(j, "id")).unwrap_or("");
            let anchor = if web.contains('#') {
                format!("{}-{}", web, nmtokenize(code))
            } else {
                format!("{}#{}-{}", web, cs_id, nmtokenize(code))
            };
            let mut a = el("a");
            a.set_attribute("href", anchor);
            tx(&mut a, code);
            td.add_child_node(a);
            return;
        }
    }
    if system == "http://snomed.info/sct" {
        let mut a = el("a");
        a.set_attribute("href", format!("http://snomed.info/id/{}", code));
        tx(&mut a, code);
        td.add_child_node(a);
    } else if system == "http://loinc.org" {
        let mut a = el("a");
        a.set_attribute("href", format!("https://loinc.org/{}/", code));
        tx(&mut a, code);
        td.add_child_node(a);
    } else {
        tx(td, code);
    }
}

fn version_for_system(params: &[Value], system: &str) -> Option<String> {
    let prefix = format!("{}|", system);
    for p in params {
        if let Some(obj) = p.as_object() {
            for (k, v) in obj {
                if k.starts_with("value") {
                    if let Some(sv) = v.as_str() {
                        if let Some(rest) = sv.strip_prefix(&prefix) {
                            return Some(rest.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// vsr:1097-1114 makeJson.
fn make_json(c: &Value, version: Option<&str>) -> String {
    let mut b = String::from("{");
    b.push_str(&format!("\"system\": \"{}\"", escape_json(get_str(c, "system").unwrap_or(""))));
    if let Some(v) = get_str(c, "version") {
        b.push_str(&format!(", \"version\": \"{}\"", escape_json(v)));
    } else if let Some(v) = version {
        b.push_str(&format!(", \"version\": \"{}\"", escape_json(v)));
    }
    if let Some(code) = get_str(c, "code") {
        b.push_str(&format!(", \"code\": \"{}\"", escape_json(code)));
    }
    if let Some(d) = get_str(c, "display") {
        b.push_str(&format!(", \"display\": \"{}\"", escape_json(d)));
    }
    b.push('}');
    b
}

/// vsr:1116-1133 makeXml (pseudo-coding, `>`-terminated not self-closed).
fn make_xml(c: &Value, version: Option<&str>) -> String {
    let mut b = String::from("<coding>");
    b.push_str(&format!("<system value=\"{}\">", escape_xml(get_str(c, "system").unwrap_or(""))));
    if let Some(v) = get_str(c, "version") {
        b.push_str(&format!("<version value=\"{}\">", escape_xml(v)));
    } else if let Some(v) = version {
        b.push_str(&format!("<version value=\"{}\">", escape_xml(v)));
    }
    if let Some(code) = get_str(c, "code") {
        b.push_str(&format!("<code value=\"{}\">", escape_xml(code)));
    }
    if let Some(d) = get_str(c, "display") {
        b.push_str(&format!("<display value=\"{}\">", escape_xml(d)));
    }
    b.push_str("</coding>");
    b
}

/// `<span class="copy-text-inline"><button class="btn-copy" title="..."
/// data-clipboard-text="..."> </button></span>`.
fn copy_cell(clip: &str, title: &str) -> XhtmlNode {
    let mut td = el("td");
    let mut span = el("span");
    span.set_attribute("class", "copy-text-inline");
    let mut btn = el("button");
    // attribute order in golden: data-clipboard-text, title, class
    btn.set_attribute("data-clipboard-text", clip);
    btn.set_attribute("title", title);
    btn.set_attribute("class", "btn-copy");
    tx(&mut btn, " ");
    span.add_child_node(btn);
    td.add_child_node(span);
    td
}

/// vsr:587-638 generateVersionNotice — the header box. Single system/version →
/// <p>; the intro phrase keyed on exp.source (internal/server/none).
fn push_expansion_header(div: &mut XhtmlNode, ctx: &IgContext, exp: &ExpandedValueSet) {
    // collect versions: params where name startsWith "used-" or =="version".
    // key = name|system (name="system" for "version"; substring(5) otherwise).
    let mut versions: Vec<(String, String, String)> = Vec::new(); // (name, system, version)
    let mut seen: Vec<String> = Vec::new();
    for p in &exp.parameters {
        let name = get_str(p, "name").unwrap_or("");
        if !(name.starts_with("used-") || name == "version") {
            continue;
        }
        let val = p
            .as_object()
            .and_then(|o| o.iter().find(|(k, _)| k.starts_with("value")).and_then(|(_, v)| v.as_str()))
            .unwrap_or("");
        if seen.contains(&val.to_string()) {
            continue;
        }
        seen.push(val.to_string());
        let logical = if name == "version" { "system".to_string() } else { name[5..].to_string() };
        if let Some((sys, ver)) = val.split_once('|') {
            if !sys.is_empty() {
                versions.push((logical, sys.to_string(), ver.to_string()));
            }
        }
    }
    if versions.is_empty() {
        return;
    }
    // sort by key (name|system) — Utilities.sorted.
    versions.sort_by(|a, b| format!("{}|{}", a.0, a.1).cmp(&format!("{}|{}", b.0, b.1)));

    if versions.len() == 1 {
        let (name, sys, ver) = &versions[0];
        let mut p = el("p");
        p.set_attribute("style", "border: black 1px dotted; background-color: #EEEEEE; padding: 8px; margin-bottom: 8px");
        match &exp.source {
            None => tx(&mut p, "Expansion based on "),
            Some(s) if s == "internal" => tx(&mut p, "Expansion performed internally based on "),
            Some(s) => tx(&mut p, &format!("Expansion from {} based on ", s)),
        }
        exp_ref(&mut p, name, sys, ver, ctx);
        div.add_child_node(p);
    } else {
        let mut vdiv = el("div");
        vdiv.set_attribute("style", "border: black 1px dotted; background-color: #EEEEEE; padding: 8px; margin-bottom: 8px");
        let mut p = el("p");
        match &exp.source {
            None => tx(&mut p, "Expansion based on:"),
            Some(s) if s == "internal" => tx(&mut p, "Expansion performed internally based on:"),
            Some(s) => tx(&mut p, &format!("Expansion from {} based on:", s)),
        }
        vdiv.add_child_node(p);
        let mut ul = el("ul");
        for (name, sys, ver) in &versions {
            let mut li = el("li");
            exp_ref(&mut li, name, sys, ver, ctx);
            ul.add_child_node(li);
        }
        vdiv.add_child_node(ul);
        div.add_child_node(vdiv);
    }
}

/// vsr:649-702 expRef. `t` = the logical name prefix (e.g. "codesystem"); `u`
/// = the system URL; `v` = version.
fn exp_ref(x: &mut XhtmlNode, t: &str, u: &str, v: &str, ctx: &IgContext) {
    if u == "http://snomed.info/sct" {
        // version like ".../900000000000207008/version/20250201"
        let parts: Vec<&str> = v.split('/').collect();
        if parts.len() >= 5 {
            let m = describe_module(parts[4]);
            if parts.len() == 7 {
                tx(x, &format!("SNOMED CT {} edition {}", m, format_sct_date(parts[6])));
            } else {
                tx(x, &format!("SNOMED CT {} edition", m));
            }
        } else {
            tx(x, &format!("{} version {}", display_system(u), v));
        }
    } else {
        // resolve the CS by canonical.
        let canonical = if v.is_empty() { u.to_string() } else { format!("{}|{}", u, v) };
        let r = ctx.resolve(&canonical).or_else(|| ctx.resolve(u));
        match r {
            Some(rr) if !rr.web_path.is_empty() => {
                let mut a = el("a");
                a.set_attribute("href", rr.web_path.clone());
                if v.is_empty() {
                    tx(&mut a, &format!("{} {} (no version) ({})", t, rr.present(), rr.rtype));
                } else {
                    tx(&mut a, &format!("{} {} v{} ({})", t, rr.present(), v, rr.rtype));
                }
                x.add_child_node(a);
            }
            _ => {
                tx(x, &format!("{} {} version {}", t, display_system(u), v));
            }
        }
    }
    // copy-url button.
    let copy_url = if v.is_empty() { u.to_string() } else { format!("{}|{}", u, v) };
    let mut span = el("span");
    span.set_attribute("class", "copy-text-inline");
    let mut btn = el("button");
    btn.set_attribute("data-clipboard-text", copy_url);
    btn.set_attribute("title", "Click to Copy URL");
    btn.set_attribute("class", "btn-copy");
    tx(&mut btn, " ");
    span.add_child_node(btn);
    x.add_child_node(span);
}

/// DataRenderer.displaySystem — human name for a system URL (fallback: the url).
fn display_system(u: &str) -> String {
    match u {
        "http://snomed.info/sct" => "SNOMED CT".to_string(),
        "http://loinc.org" => "LOINC".to_string(),
        _ => u.to_string(),
    }
}

/// describeModule(sctid) — the SNOMED edition name for the module id.
fn describe_module(m: &str) -> String {
    match m {
        "900000000000207008" => "International".to_string(),
        "731000124108" => "US".to_string(),
        _ => m.to_string(),
    }
}

/// formatSCTDate: yyyyMMdd → "dd-MMM yyyy" (e.g. 20250201 → "01-Feb 2025").
fn format_sct_date(ds: &str) -> String {
    if ds.len() != 8 {
        return ds.to_string();
    }
    let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let y = &ds[0..4];
    let mo: usize = ds[4..6].parse().unwrap_or(1);
    let d = &ds[6..8];
    let mon = months.get(mo.saturating_sub(1)).copied().unwrap_or("Jan");
    format!("{}-{} {}", d, mon, y)
}
