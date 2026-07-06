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

pub(crate) fn el(name: &str) -> XhtmlNode {
    // makeTag semantics: sets notPretty for the inline element set (b, code, a,
    // span, br, ...) so the composer's pretty path matches fhir-core byte-for-byte.
    XhtmlNode::new_tag(name)
}

/// `XhtmlNode.tx(text)` — appends a text node child, returns self.
pub(crate) fn tx(parent: &mut XhtmlNode, text: &str) {
    parent.add_text(text.to_string());
}

/// Compose a `<div>`'s children with `new XhtmlComposer(false, true)` =
/// (xml=false, pretty=true) => HTML pretty, via the `compose(XhtmlNodeList)`
/// overload (no breakBlocksWithLines). Used by invOldMode/tx/txDiff (psdr:1262,
/// 837, 890).
pub(crate) fn compose_children_html_pretty(div: &XhtmlNode) -> String {
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
// summary / summary-all  (psdr summary:154) — raw StringBuilder (NOT composer)
// ---------------------------------------------------------------------------

fn pluralize_element(n: i64) -> &'static str {
    if n > 1 {
        "elements"
    } else {
        "element"
    }
}

/// psdr summary:154. `all` toggles the anchor name (a-summary vs s-summary).
/// Markdown-dependent branches (EXT_SUMMARY extension; extensionSummary for
/// Extension-type SDs) fire a loud gap — those need the publisher markdown
/// engine. Non-extension profiles are fully markdown-free.
pub fn summary(sd: &Sd, ctx: &crate::context::IgContext, all: bool, core_path_v: &str) -> String {
    // EXT_SUMMARY at psdr:157 short-circuits the WHOLE method (before the header).
    if let Some(md) = read_string_extension(
        sd,
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-summary",
    ) {
        return crate::publisher_markdown::process_markdown(ctx, &md, core_path_v);
    }
    // no differential -> STRUC_DEF_NO_SUMMARY (rare; corpus always has diff)
    if sd.root.get("differential").is_none() {
        return "<p>This structure has no summary</p>".to_string();
    }

    let diff = sd.differential_elements();
    // Reconstructed SNAPSHOT_DERIVATION_POINTER for the nested-mandatory split
    // (psdr parentChainHasOptional:318). diff id -> snapshot element index.
    let pointers = crate::diff::reconstruct_diff_pointers(sd);

    let mut refs: Vec<String> = Vec::new();
    let mut ext: Vec<String> = Vec::new();
    let mut slices: Vec<String> = Vec::new();
    let mut supports = 0i64;
    let mut required_outrights = 0i64;
    let mut required_nesteds = 0i64;
    let mut fixeds = 0i64;
    let mut prohibits = 0i64;

    for ed in &diff {
        if !ed.path().contains('.') {
            continue;
        }
        if ed.min() == Some(1) {
            if parent_chain_has_optional(sd, ed, &pointers) {
                required_nesteds += 1;
            } else {
                required_outrights += 1;
            }
        }
        if ed.max() == Some("0") {
            prohibits += 1;
        }
        if ed.must_support() {
            supports += 1;
        }
        if ed.fixed().is_some() {
            fixeds += 1;
        }
        for t in ed.types() {
            for p in t.profiles() {
                if p.len() > 40 && !igp_is_datatype(ctx, &p[40..]) {
                    if ed.path().ends_with(".extension") {
                        try_add(&mut ext, summarise_extension(ctx, p, false));
                    } else if ed.path().ends_with(".modifierExtension") {
                        try_add(&mut ext, summarise_extension(ctx, p, true));
                    } else {
                        try_add(&mut refs, describe_profile(ctx, p));
                    }
                }
            }
            for tp in t.target_profiles() {
                try_add(&mut refs, describe_profile(ctx, tp));
            }
        }
        if ed.has_slicing()
            && !ed.path().ends_with(".extension")
            && !ed.path().ends_with(".modifierExtension")
        {
            if let Some(s) = describe_slice(ed.path(), ed.slicing().unwrap()) {
                if !slices.contains(&s) {
                    slices.push(s);
                }
            }
        }
    }

    let anchor = if all { "a" } else { "s" };
    let mut res = String::new();
    res.push_str(&format!(
        "<a name=\"{}-summary\"> </a>\r\n<p><b>\r\nSummary\r\n</b></p>\r\n",
        anchor
    ));

    if sd.type_name() == "Extension" {
        // psdr:222 — the whole mandatory/refs/ext/slices block is replaced by
        // extensionSummary(); the FMM block below still runs.
        res.push_str(&extension_summary(sd, ctx, core_path_v));
        push_fmm(sd, &mut res);
        return res;
    }

    if supports + required_outrights + required_nesteds + fixeds + prohibits > 0 {
        let mut started = false;
        res.push_str("<p>");
        if required_outrights > 0 || required_nesteds > 0 {
            started = true;
            res.push_str(&format!(
                "Mandatory: {} {}",
                required_outrights,
                pluralize_element(required_outrights)
            ));
            if required_nesteds > 0 {
                res.push_str(&format!(
                    "({} nested mandatory {})",
                    required_nesteds,
                    pluralize_element(required_nesteds)
                ));
            }
        }
        if supports > 0 {
            if started {
                res.push_str("<br/> ");
            }
            started = true;
            res.push_str(&format!(
                "Must-Support: {} {}",
                supports,
                pluralize_element(supports)
            ));
        }
        if fixeds > 0 {
            if started {
                res.push_str("<br/> ");
            }
            started = true;
            res.push_str(&format!("Fixed: {} {}", fixeds, pluralize_element(fixeds)));
        }
        if prohibits > 0 {
            if started {
                res.push_str("<br/> ");
            }
            let _ = started;
            res.push_str(&format!(
                "Prohibited: {} {}",
                prohibits,
                pluralize_element(prohibits)
            ));
        }
        res.push_str("</p>");
    }

    if !refs.is_empty() {
        res.push_str("<p><b>Structures</b></p>\r\n<p>This structure refers to these other structures:</p>\r\n<ul>\r\n");
        for s in &refs {
            res.push_str(s);
        }
        res.push_str("\r\n</ul>\r\n\r\n");
    }
    if !ext.is_empty() {
        res.push_str("<p><b>Extensions</b></p>\r\n<p>This structure refers to these extensions:</p>\r\n<ul>\r\n");
        for s in &ext {
            res.push_str(s);
        }
        res.push_str("\r\n</ul>\r\n\r\n");
    }
    if !slices.is_empty() {
        // SD_SUMMARY_SLICES = This structure defines the following {0}Slices{1}
        res.push_str(&format!(
            "<p><b>Slices</b></p>\r\n<p>This structure defines the following <a href=\"{}profiling.html#slices\">Slices</a>:</p>\r\n<ul>\r\n",
            core_path_v
        ));
        for s in &slices {
            res.push_str(s);
        }
        res.push_str("\r\n</ul>\r\n\r\n");
    }

    // Maturity (EXT_FMM_LEVEL)
    push_fmm(sd, &mut res);

    res
}

/// psdr:264 EXT_FMM_LEVEL maturity block.
fn push_fmm(sd: &Sd, res: &mut String) {
    if let Some(fmm) = read_string_extension(
        sd,
        "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm",
    ) {
        res.push_str(&format!(
            "<p><b><a class=\"fmm\" href=\"http://hl7.org/fhir/versions.html#maturity\" title=\"Maturity Level\">Maturity</a></b>: {}</p>\r\n",
            fmm
        ));
    }
}

/// psdr extensionSummary:285. Simple extension: one `<p>` phrase with the value
/// type + `stripPara(processMarkdown(description))`. Complex extension: a `<p>`
/// with `stripAllPara(processMarkdown(description))` + a `<ul>` of value slices.
fn extension_summary(sd: &Sd, ctx: &crate::context::IgContext, core_path_v: &str) -> String {
    let is_mod = ext_is_modifier(sd);
    let desc = sd.root.get("description").and_then(|x| x.as_str()).unwrap_or("");
    let snap = sd.snapshot_elements();
    // ProfileUtilities.isSimpleExtension: Extension.value[x] present and not
    // prohibited (max != 0).
    let value = snap.iter().find(|e| e.path() == "Extension.value" || e.path() == "Extension.value[x]");
    let is_simple = value.map(|v| v.max() != Some("0")).unwrap_or(false);

    if is_simple {
        let value = value.unwrap();
        let type_summary = type_summary(value);
        let md = crate::publisher_markdown::process_markdown(ctx, desc, core_path_v);
        let stripped = crate::publisher_markdown::strip_para(&md);
        // SDR_EXTENSION_SUMMARY = "Simple Extension with the type {0}: {1}"
        // SDR_EXTENSION_SUMMARY_MODIFIER = "Simple <b>Modifier</b> Extension with the type {0}: {1}"
        let phrase = if is_mod {
            format!(
                "Simple <b>Modifier</b> Extension with the type {}: {}",
                type_summary, stripped
            )
        } else {
            format!("Simple Extension with the type {}: {}", type_summary, stripped)
        };
        format!("<p>{}</p>", phrase)
    } else {
        // Complex: subs = the Extension.*.extension.value[x] elements, each paired
        // with its owning slice (the preceding .extension with a sliceName).
        let mut subs: Vec<(Option<String>, String, String)> = Vec::new(); // (sliceName, typeSummary, definition)
        let mut slice_name: Option<String> = None;
        let mut slice_defn: Option<String> = None;
        for e in &snap {
            let p = e.path();
            if p.ends_with(".extension") && e.has_slice_name() {
                slice_name = e.slice_name().map(String::from);
                slice_defn = e.definition().map(String::from);
            } else if p.ends_with(".extension.value[x]") {
                if let Some(sn) = slice_name.take() {
                    subs.push((Some(sn), type_summary(e), slice_defn.take().unwrap_or_default()));
                } else {
                    // no owning slice -> psdr skips (defn==null); we drop it.
                }
            }
        }
        let html = crate::publisher_markdown::strip_all_para(
            &crate::publisher_markdown::process_markdown(ctx, desc, core_path_v),
        );
        let mut b = String::new();
        // TEXT_ICON_EXTENSION_COMPLEX = "Complex Extension"
        b.push_str(&format!(
            "<p>Complex Extension: {}</p><ul data-fhir=\"generated-heirarchy\">",
            html
        ));
        for (sn, ts, defn) in &subs {
            let sn = sn.clone().unwrap_or_default();
            let dmd = crate::publisher_markdown::strip_para(
                &crate::publisher_markdown::process_markdown(ctx, defn, core_path_v),
            );
            b.push_str(&format!("<li>{}: {}: {}</li>\r\n", sn, ts, dmd));
        }
        b.push_str("</ul>");
        b
    }
}

/// ElementDefinition.typeSummary(): comma-space-joined workingCode() of types.
fn type_summary(ed: &crate::sdmodel::Ed) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for t in ed.types() {
        // hasCode(): code present
        if !t.code().is_empty() {
            parts.push(t.working_code());
        }
    }
    parts.join(", ")
}

/// ProfileUtilities.isModifierExtension(sd): the snapshot (else diff) "Extension"
/// element isModifier.
fn ext_is_modifier(sd: &Sd) -> bool {
    let snap = sd.snapshot_elements();
    if let Some(e) = snap.iter().find(|e| e.path() == "Extension") {
        return e.is_modifier();
    }
    sd.differential_elements()
        .iter()
        .find(|e| e.path() == "Extension")
        .map(|e| e.is_modifier())
        .unwrap_or(false)
}

fn try_add(list: &mut Vec<String>, s: Option<String>) {
    if let Some(s) = s {
        if !s.is_empty() && !list.contains(&s) {
            list.push(s);
        }
    }
}

/// psdr describeProfile:385. Datatype/resource core types return None.
fn describe_profile(ctx: &crate::context::IgContext, url: &str) -> Option<String> {
    if url.starts_with("http://hl7.org/fhir/StructureDefinition/") {
        let tail = &url[40..];
        if igp_is_datatype(ctx, tail) || is_core_resource(ctx, tail) || tail == "Resource" {
            return None;
        }
    }
    match ctx.resolve(url) {
        None => Some(format!(
            "<li>Unable to summarise profile {} (no profile found)</li>",
            url
        )),
        Some(r) => Some(format!(
            "<li><a href=\"{}\">{} <span style=\"font-size: 8px\">({})</span></a></li>\r\n",
            escape_xml(&r.web_path),
            r.present(),
            url
        )),
    }
}

/// psdr summariseExtension:372.
fn summarise_extension(
    ctx: &crate::context::IgContext,
    url: &str,
    modifier: bool,
) -> Option<String> {
    let modif = if modifier {
        " (<b>Modifier</b>) "
    } else {
        ""
    };
    match ctx.resolve(url) {
        None => Some(format!(
            "<li>Unable to summarise extension {} (no extension found)</li>",
            url
        )),
        Some(r) if r.web_path.is_empty() => Some(format!(
            "<li><a href=\"extension-{}.html\">{}</a>{}</li>\r\n",
            // ed.getId().toLowerCase() — id = last URL segment for own extensions
            url.rsplit('/').next().unwrap_or(url).to_lowercase(),
            url,
            modif
        )),
        Some(r) => Some(format!(
            "<li><a href=\"{}\">{}</a>{}</li>\r\n",
            escape_xml(&r.web_path),
            url,
            modif
        )),
    }
}

/// psdr describeSlice:351.
fn describe_slice(path: &str, slicing: &serde_json::Value) -> Option<String> {
    let discriminators = slicing.get("discriminator").and_then(|d| d.as_array());
    if discriminators.map(|d| d.is_empty()).unwrap_or(true) {
        return Some(format!(
            "<li>There is a slice with no discriminator at {}</li>\r\n",
            path
        ));
    }
    let discs = discriminators.unwrap();
    let mut s = String::new();
    if slicing.get("ordered").and_then(|o| o.as_bool()) == Some(true) {
        s = "ordered".to_string();
    }
    let rules = slicing.get("rules").and_then(|r| r.as_str()).unwrap_or("");
    if rules != "open" {
        let disp = rules_display(rules);
        s = if s.is_empty() {
            disp.to_string()
        } else {
            format!("{}, {}", s, disp)
        };
    }
    if !s.is_empty() {
        s = format!(" ({})", s);
    }
    let count = discs.len();
    // SD_SUMMARY_SLICE_{one,other}: {0}=count, {1}=path (discriminators arg dropped)
    let phrase = if count == 1 {
        format!("The element {} is sliced based on the value of {}", count, path)
    } else {
        format!("The element {} is sliced based on the values of {}", count, path)
    };
    Some(format!("<li>{}{}</li>\r\n", phrase, s))
}

fn rules_display(code: &str) -> &'static str {
    match code {
        "closed" => "Closed",
        "open" => "Open",
        "openAtEnd" => "Open At End",
        _ => "",
    }
}

/// psdr parentChainHasOptional:318 — faithful port.
///
/// `match = SNAPSHOT_DERIVATION_POINTER(ed)` (reconstructed: the profile's own
/// snapshot element the diff element derived from — exact-id, sliced-choice, or
/// unsliced-camelCase alias; see `diff::reconstruct_diff_pointers`). If no pointer
/// (`match == null`) return true (psdr:323 "common in existing profiles").
/// Then `while match.path contains ".": if match.min == 0 return true; match =
/// getElementParent(snapshot, match); if match == null return true`. Return false.
///
/// `getElementParent` (psdr:340) = the nearest PRECEDING snapshot element whose
/// path is `match.path` minus its last segment — walked by list index, NOT by a
/// fresh id search, so a sliced/renamed pointer still walks the real snapshot
/// ancestry. This reconstruction drove observation-occupation / -pregnancyintent
/// / -pregnancystatus to byte-parity (was the silent-approx set).
///
/// RESIDUAL — us-core-practitioner only (1 SD, cited). Its 5 diff-mandatory are
/// {identifier, identifier.system, identifier.value, name, name.family}; the
/// golden splits 3 outright / 2 nested. identifier/name are outright (own pointer,
/// snapshot min 1). The sub-elements identifier.system/.value + name.family are
/// NOT present in the immediate base (core Practitioner expands neither Identifier
/// nor HumanName), so the publisher's SNAPSHOT_DERIVATION_POINTER for them is the
/// base-clone captured mid-`updateFromDefinition` (PU:2586) whose `.min` reflects
/// the datatype default (0) at the instant `getMin()` is read for the split — a
/// value that is later overwritten to 1 in the SAME object and so is NOT
/// recoverable from the finished snapshot JSON (min already 1). The exact split
/// (2 of those 3 nested) depends on that transient in-memory state; reconstructing
/// it would require re-running snapshot generation with pointer capture. Our
/// own-snapshot pointers read min 1 for all three -> 3 extra outrights (total
/// mandatory count still correct: 5). Documented; the finished-JSON oracle cannot
/// distinguish the 2/1 sub-split without the live snapshot-gen pointer state.
fn parent_chain_has_optional(
    sd: &Sd,
    ed: &crate::sdmodel::Ed,
    pointers: &std::collections::HashMap<String, usize>,
) -> bool {
    if !ed.path().contains('.') {
        return false;
    }
    let snap = sd.snapshot_elements();
    // match = pointer(ed); null -> true.
    let Some(&start) = pointers.get(ed.id()) else {
        return true;
    };
    let mut idx = start;
    loop {
        let e = &snap[idx];
        if !e.path().contains('.') {
            return false;
        }
        if e.min() == Some(0) {
            return true;
        }
        // getElementParent: preceding snapshot element with path == parent path.
        let ppath = &e.path()[..e.path().rfind('.').unwrap()];
        let mut found = None;
        let mut i = idx as i64 - 1;
        while i >= 0 {
            if snap[i as usize].path() == ppath {
                found = Some(i as usize);
                break;
            }
            i -= 1;
        }
        match found {
            Some(f) => idx = f,
            None => return true,
        }
    }
}

fn is_core_resource(ctx: &crate::context::IgContext, tail: &str) -> bool {
    // igp.isResource(name): fetchTypeDefinition(name) is kind==resource AND
    // derivation==specialization (IGKnowledgeProvider:557).
    ctx.resolve_type(tail)
        .map(|r| {
            r.kind.as_deref() == Some("resource")
                && r.derivation.as_deref() == Some("specialization")
        })
        .unwrap_or(false)
}

/// igp.isDatatype(name) (IGKnowledgeProvider:551): fetchTypeDefinition(name) is
/// kind primitive/complex AND derivation==specialization. Crucially resolves
/// the CORE type by name — an extension whose URL matches the core prefix is
/// kind=complex-type but derivation=constraint, so this returns false.
fn igp_is_datatype(ctx: &crate::context::IgContext, tail: &str) -> bool {
    ctx.resolve_type(tail)
        .map(|r| {
            matches!(r.kind.as_deref(), Some("primitive-type") | Some("complex-type"))
                && r.derivation.as_deref() == Some("specialization")
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// sd-use-context  (psdr useContext:2877) — HTML-pretty composer
// ---------------------------------------------------------------------------

/// psdr useContext:2877. Renders the extension usage-context list (or the
/// "any element" default for non-extension SDs). Markdown-dependent branches
/// (deprecated standards-status reason) fire a loud gap.
pub fn use_context(sd: &Sd, ctx: &crate::context::IgContext, core_path_v: &str) -> String {
    let mut div = el("div");

    // deprecated standards-status block (psdr:2879).
    if standards_status(sd).as_deref() == Some("deprecated") {
        let mut ddiv = el("div");
        ddiv.set_attribute(
            "style",
            "background-color: #ffe6e6; border: 1px solid black; border-radius: 10px; padding: 10px",
        );
        let mut p = el("p");
        let mut b = el("b");
        // SDR_EXT_DEPR
        tx(&mut b, "This extension is deprecated and should no longer be used");
        p.add_child_node(b);
        ddiv.add_child_node(p);
        // reason = standards-status ext's value's nested standards-status-reason ext.
        if let Some(reason) = standards_status_reason(sd) {
            // ddiv.markdown(preProcessMarkdown(reason)): preProcessMarkdown then
            // MarkDownProcessor.process then XhtmlParser re-parse -> addChildren.
            let pre = crate::publisher_markdown::pre_process_markdown(ctx, &reason, core_path_v);
            let html = crate::publisher_markdown::md_process(&pre);
            for node in crate::publisher_markdown::markdown_children_from_html(&html) {
                ddiv.add_child_node(node);
            }
        }
        div.add_child_node(ddiv);
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
                crate::loud_gap!((), "LOUD GAP: sd-use-context fhir-version-specific-use range (psdr:2942) for {}", sd.id());
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
        crate::loud_gap!((),
            "LOUD GAP: sd-use-context SD-level fhir-version-specific-use (psdr:2966) for {}",
            sd.id()
        );
    }

    compose_children_html_pretty(&div)
}

fn standards_status(sd: &Sd) -> Option<String> {
    read_string_extension(sd, "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status")
}

/// psdr:2882-2883: the standards-status extension's VALUE carries a nested
/// `standards-status-reason` extension. In JSON the primitive `valueCode` sidecar
/// `_valueCode` holds `.extension[]`; read that reason's markdown value.
fn standards_status_reason(sd: &Sd) -> Option<String> {
    let arr = sd.root.get("extension")?.as_array()?;
    for x in arr {
        if x.get("url").and_then(|u| u.as_str())
            == Some("http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status")
        {
            let side = x.get("_valueCode")?;
            let exts = side.get("extension")?.as_array()?;
            for e in exts {
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
    // ExtensionUtilities.readStringExtension: reads whatever primitive value is
    // present (FMM = valueInteger, standards-status = valueCode, etc.).
    let arr = sd.root.get("extension")?.as_array()?;
    for x in arr {
        if x.get("url").and_then(|u| u.as_str()) == Some(url) {
            if let Some(v) = x.get("valueCode").and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
            if let Some(v) = x.get("valueString").and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
            if let Some(v) = x.get("valueInteger") {
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
    // Derive a TOTAL sort key. The original comparator switched schemes based on
    // whether BOTH sides matched `.+-\d+` (numeric suffix), which is non-transitive
    // — std's sort detects that and PANICS ("comparison not a total order"), which
    // on wasm aborts the whole engine (e.g. rendering an mCODE bundle profile).
    // Map each key to (prefix, number) instead and compare keys: dashnum keys group
    // by their pre-suffix prefix and order numerically; other keys order by their
    // full string (number sentinel). Key comparison is inherently total.
    fn sort_key(s: &str) -> (&str, i64) {
        if matches_dashnum(s) {
            let pos = s.rfind('-').unwrap();
            (&s[..pos.saturating_sub(1)], s[pos + 1..].parse().unwrap_or(0))
        } else {
            (s, i64::MIN)
        }
    }
    sort_key(a).cmp(&sort_key(b))
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
                crate::loud_gap!((), "LOUD GAP: inv requirements markdown (psdr:1256) req={:?}", req);
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
