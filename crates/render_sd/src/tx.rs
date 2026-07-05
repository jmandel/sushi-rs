//! `tx` / `tx-must-support` / `tx-key` / `tx-diff` / `tx-diff-must-support` —
//! the publisher SDR's terminology-binding tables (psdr `tx`:851, `txDiff`:798,
//! `txItem`:911, `txItemHeadings`:840, `showVersion`:1088; fhir-core
//! `ResourceRenderer.renderVersionReference`:1597). Composer =
//! `new XhtmlComposer(false, true)` over the div children (HTML pretty).
//!
//! Producing calls (PublisherGenerator:1989-2005):
//!   tx                  = tx(true, false, false)   — snapshot walk
//!   tx-must-support     = tx(true, true,  false)
//!   tx-key              = tx(true, false, true)    — getKeyElements walk
//!   tx-diff             = txDiff(true, false)      — DIFFERENTIAL walk
//!   tx-diff-must-support= txDiff(true, true)
//!
//! Load-bearing determinations (golden-verified):
//! - Status column is HARDCODED "Base" (psdr:925).
//! - Usage anchor = `"terminologies.html#" + tx.getStrengthElement()` — the Java
//!   Enumeration toString → `Enumeration[extensible]` (993 golden rows).
//! - `inherited` = `ed.hasUserData(SNAPSHOT_DERIVATION_POINTER)`: set only on
//!   DIFFERENTIAL elements (PU:2591), so the snapshot walks NEVER dim
//!   (0 dimmed links in tx/tx-key/tx-ms goldens) and the diff walks dim the VS
//!   link on every pointer-bearing element (reconstructed via
//!   `diff::reconstruct_diff_pointers`). The strength link dims only when the
//!   diff binding does NOT restate strength (pointer's snapshot strength used).
//! - txItemHeadings is always called with hasFixed=false (psdr:887/838) — the
//!   "ValueSet / Code" heading and the render_tx_value userdata are dead.
//! - The publisher's own-IG resources carry a sourcePackage, so
//!   `vs.hasSourcePackage()` is true for everything except tx-cache externals;
//!   modeled as `from_packages = !resolved.external`.

use std::collections::HashMap;

use render_xhtml::node::XhtmlNode;
use serde_json::Value;

use crate::context::IgContext;
use crate::leaf::{compose_children_html_pretty, el, tx as txt};
use crate::sdmodel::{Ed, Sd};

/// Which wrapper produced the fragment.
#[derive(Clone, Copy)]
pub struct TxOpts {
    /// txDiff (differential walk) vs tx (snapshot/key walk).
    pub diff: bool,
    pub must_support_only: bool,
    pub key_only: bool,
}

impl TxOpts {
    pub fn tx() -> TxOpts {
        TxOpts { diff: false, must_support_only: false, key_only: false }
    }
    pub fn tx_must_support() -> TxOpts {
        TxOpts { diff: false, must_support_only: true, key_only: false }
    }
    pub fn tx_key() -> TxOpts {
        TxOpts { diff: false, must_support_only: false, key_only: true }
    }
    pub fn tx_diff() -> TxOpts {
        TxOpts { diff: true, must_support_only: false, key_only: false }
    }
    pub fn tx_diff_must_support() -> TxOpts {
        TxOpts { diff: true, must_support_only: true, key_only: false }
    }
}

pub fn render_tx(sd: &Sd, ctx: &IgContext, core_path: &str, opts: TxOpts) -> String {
    // Element list (psdr:856/803): snapshot / getKeyElements / differential.
    let elements: Vec<Value> = if opts.diff {
        sd.differential_elements().iter().map(|e| e.v.clone()).collect()
    } else if opts.key_only {
        crate::table::key_elements_pub(sd, ctx)
    } else {
        sd.snapshot_elements().iter().map(|e| e.v.clone()).collect()
    };
    // Reconstructed SNAPSHOT_DERIVATION_POINTER (diff walk only): diff id ->
    // own-snapshot index (PU:2591; same machinery as the diff table view).
    let pointers: HashMap<String, usize> = if opts.diff {
        crate::diff::reconstruct_diff_pointers(sd)
    } else {
        HashMap::new()
    };
    let snap = sd.snapshot_elements();

    // Filter (psdr:857/805): hasBinding && max != "0" && (!msOnly || mustSupport).
    let mut rows: Vec<&Value> = Vec::new();
    for edv in &elements {
        let ed = Ed::new(edv);
        if ed.binding().is_none() {
            continue;
        }
        if ed.max() == Some("0") {
            continue;
        }
        if opts.must_support_only && !ed.must_support() {
            continue;
        }
        // Extension-typed id append (psdr:872: id += "<br/>" + List.toString) —
        // zero corpus hits; fire loud rather than silently mis-shape the path.
        let types = ed.types();
        if types.len() == 1 && types[0].working_code() == "Extension" {
            panic!(
                "LOUD GAP: tx Extension-typed binding element id append (psdr:872) at {}",
                ed.id()
            );
        }
        rows.push(edv);
    }
    if rows.is_empty() {
        return String::new();
    }

    let mut div = el("div");
    // withHeadings is always true in the producing calls.
    let mut h4 = el("h4");
    txt(
        &mut h4,
        if opts.diff {
            // STRUC_DEF_TERM_BIND
            "Terminology Bindings (Differential)"
        } else {
            // STRUC_DEF_TERM_BINDS
            "Terminology Bindings"
        },
    );
    div.add_child_node(h4);

    let mut tbl = el("table");
    tbl.set_attribute("class", "list");
    tbl.set_attribute("data-fhir", "generated-heirarchy");

    // txItemHeadings(tbl, false) — hasFixed param dead (always false).
    {
        let mut tr = el("tr");
        for label in ["Path", "Status", "Usage", "ValueSet", "Version", "Source"] {
            let mut td = el("td");
            let mut b = el("b");
            txt(&mut b, label);
            td.add_child_node(b);
            tr.add_child_node(td);
        }
        tbl.add_child_node(tr);
    }

    for edv in rows {
        tx_item(&mut tbl, Ed::new(edv), sd, ctx, core_path, opts.diff, &pointers, &snap);
    }
    div.add_child_node(tbl);
    compose_children_html_pretty(&div)
}

#[allow(clippy::too_many_arguments)]
fn tx_item(
    tbl: &mut XhtmlNode,
    ed: Ed<'_>,
    _sd: &Sd,
    ctx: &IgContext,
    core_path: &str,
    diff: bool,
    pointers: &HashMap<String, usize>,
    snap: &[Ed<'_>],
) {
    let binding = ed.binding().expect("filtered to binding-bearing");
    let id = ed.id();
    // The publisher reads the CANONICAL_RESOLUTION_METHOD extension off
    // binding._valueSet (ExtensionUtilities.getVersionResolutionRules) — zero
    // corpus hits; the LATEST version-cell branch is unreachable here.
    if binding
        .get("_valueSet")
        .and_then(|s| s.get("extension"))
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter().any(|x| {
                x.get("url").and_then(|u| u.as_str())
                    == Some("http://hl7.org/fhir/StructureDefinition/version-resolution-method")
            })
        })
        .unwrap_or(false)
    {
        panic!("LOUD GAP: tx version-resolution-method extension (RR:1598 LATEST branch) at {}", id);
    }

    // strength + strengthInh (psdr:914-922).
    let own_strength = binding.get("strength").and_then(|s| s.as_str());
    let pointer = pointers.get(id).map(|&i| snap[i]);
    let (strength, strength_inh) = match own_strength {
        Some(s) => (Some(s.to_string()), false),
        None => match pointer.and_then(|p| {
            p.binding()
                .and_then(|b| b.get("strength"))
                .and_then(|s| s.as_str())
        }) {
            Some(s) => (Some(s.to_string()), true),
            None => (None, false),
        },
    };
    // inherited (psdr:936): the element carries a POINTER at all. Snapshot
    // walks: never (POINTER is stamped on diff elements only, PU:2591).
    let inherited = diff && pointers.contains_key(id);

    let mut tr = el("tr");
    // Path
    {
        let mut td = el("td");
        txt(&mut td, &insert_breaking_spaces(id));
        tr.add_child_node(td);
    }
    // Status — hardcoded (psdr:925).
    {
        let mut td = el("td");
        txt(&mut td, "Base");
        tr.add_child_node(td);
    }
    // Usage (strength)
    {
        let mut td = el("td");
        if let Some(s) = &strength {
            let mut a = el("a");
            // HashMap attr order in the golden: style before href.
            a.set_attribute("style", format!("opacity: {}", opacity_str(strength_inh)));
            a.set_attribute(
                "href",
                format!("{}terminologies.html#Enumeration[{}]", core_path, s),
            );
            txt(&mut a, s);
            td.add_child_node(a);
        }
        tr.add_child_node(td);
    }
    // ValueSet cell
    let mut td = el("td");
    if let Some(desc) = binding.get("description").and_then(|d| d.as_str()) {
        if !desc.is_empty() {
            td.set_attribute("title", desc);
        }
    }
    let uri: Option<String> = binding
        .get("valueSet")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string());
    // canonicalise (BaseRenderer:210): relative uris get the IG canonical.
    let canonical = uri.as_ref().map(|u| {
        if !u.starts_with("http:") && !u.starts_with("https:") {
            format!("{}/{}", ctx.own_canonical_prefix().unwrap_or_default(), u)
        } else {
            u.clone()
        }
    });
    let resolved = canonical.as_ref().and_then(|c| ctx.resolve(c));

    match &resolved {
        None => {
            // vs == null (psdr:942-962).
            if let Some(u) = &uri {
                if u.starts_with("http://loinc.org/vs/") {
                    // getSpecialValueSetName/Url (psdr:1101-1111).
                    let code = &u[20..];
                    let name = format!("LOINC {}", code);
                    let mut a = el("a");
                    a.set_attribute("style", format!("opacity: {}", opacity_str(inherited)));
                    a.set_attribute("href", format!("https://loinc.org/{}", code));
                    txt(&mut a, &name);
                    td.add_child_node(a);
                } else if u.starts_with("http://") || u.starts_with("https://") {
                    // igp.resolveActualUrl (IGKnowledgeProvider): an absolute
                    // http(s) valueSet uri that resolves to no resource in the
                    // closure still renders as a plain external link — url =
                    // display = uri. The publisher never drops the cell (psdr:947).
                    // No plan-net/cycle golden reaches this branch (corpus only
                    // hits the uri==None empty cell), but a predefined-resource IG
                    // (US Core: Patient.address.state -> the terminology.hl7.org
                    // USPS-State value set, not in its mounted closure) does — so
                    // render it faithfully rather than aborting the whole page.
                    let mut a = el("a");
                    a.set_attribute("style", format!("opacity: {}", opacity_str(inherited)));
                    a.set_attribute("href", u.clone());
                    txt(&mut a, u);
                    td.add_child_node(a);
                } else {
                    // urn: / other non-linkable uri -> display only (no href),
                    // same resolveActualUrl rule.
                    txt(&mut td, u);
                }
            }
            // uri None -> td.markdown(null) adds nothing (empty cell).
            tr.add_child_node(td);
            // Version + Source for the unresolved row (psdr:959-960).
            let mut vtd = el("td");
            show_version(&mut vtd, uri.as_deref(), None, false, false);
            tr.add_child_node(vtd);
            let mut std_ = el("td");
            txt(&mut std_, "Unknown");
            tr.add_child_node(std_);
        }
        Some(r) => {
            // VS resource fields (name/title/version) from the resolved JSON.
            let vs_json = canonical.as_ref().and_then(|c| ctx.load_resource(c));
            let (vs_title, vs_name, vs_version) = match &vs_json {
                Some(j) => (
                    j.get("title").and_then(|x| x.as_str()).map(String::from),
                    j.get("name").and_then(|x| x.as_str()).map(String::from),
                    j.get("version").and_then(|x| x.as_str()).map(String::from),
                ),
                None => (r.title.clone(), r.name.clone(), None),
            };
            let display = vs_title.or(vs_name).unwrap_or_default();
            let mut a = el("a");
            a.set_attribute("style", format!("opacity: {}", opacity_str(inherited)));
            a.set_attribute("href", r.web_path.clone());
            txt(&mut a, &display);
            td.add_child_node(a);
            // copy button (data-clipboard-text = the binding.valueSet VERBATIM).
            let mut btn = el("button");
            btn.set_attribute(
                "data-clipboard-text",
                binding.get("valueSet").and_then(|v| v.as_str()).unwrap_or(""),
            );
            btn.set_attribute("title", "Click to copy URL");
            btn.set_attribute("class", "btn-copy");
            td.add_child_node(btn);
            if r.external {
                let mut img = el("img");
                img.set_attribute("src", "external.png");
                img.set_attribute("alt", ".");
                td.add_child_node(img);
            }
            tr.add_child_node(td);

            // Version cell (showVersion, psdr:1088).
            let from_packages = !r.external;
            let from_this_package = !is_absolute_url_linkable(&r.web_path);
            let mut vtd = el("td");
            show_version_resolved(
                &mut vtd,
                uri.as_deref(),
                vs_version.as_deref(),
                from_packages,
                from_this_package,
            );
            tr.add_child_node(vtd);

            // Source cell (psdr:975-1013).
            let mut std_ = el("td");
            if !r.external {
                match &r.pkg {
                    Some(p) if is_core_package(&p.id) => {
                        // SDR_SRC_FHIR + gen.getLink(SPEC, true) — golden:
                        // https://hl7.org/fhir/R4/ (the https spec base).
                        let mut a = el("a");
                        a.set_attribute("href", core_path.replace("http://", "https://"));
                        txt(&mut a, "FHIR Std.");
                        std_.add_child_node(a);
                    }
                    Some(p) => {
                        let pname = source_package_name(p);
                        let src = format!("{} v{}", pname, maj_min(&p.version));
                        let mut a = el("a");
                        a.set_attribute("href", p.web.clone());
                        txt(&mut a, &src);
                        std_.add_child_node(a);
                    }
                    None => {
                        // Own IG resource: relative webPath -> SDR_SRC_IG, no link.
                        txt(&mut std_, "This IG");
                    }
                }
            } else {
                // tx-cache external: src/link = the server; src reduced to host.
                let server = r.tx_server.clone().unwrap_or_default();
                let host = url_host(&server).unwrap_or_else(|| server.clone());
                let mut a = el("a");
                a.set_attribute("href", server);
                txt(&mut a, &host);
                std_.add_child_node(a);
            }
            tr.add_child_node(std_);
        }
    }
    tbl.add_child_node(tr);
}

/// `showVersion` for the vs==null row: statedVersion from the uri, everything
/// else null -> renderVersionReference falls to STATED or NOTHING.
fn show_version(td: &mut XhtmlNode, uri: Option<&str>, _vs: Option<()>, _fp: bool, _ftp: bool) {
    let stated = uri.and_then(|u| u.split_once('|').map(|(_, v)| v.to_string()));
    match stated {
        Some(v) => {
            td.set_attribute("title", format!("Version is explicitly stated to be {}", v));
            txt(td, &format!("\u{1F4CD}{}", v));
        }
        None => {
            // VS_VERSION_NOTHING(type="Value Set") + VS_VERSION_NOTHING_TEXT.
            td.set_attribute(
                "title",
                "Version is not explicitly stated. No matching Value Set found",
            );
            txt(td, "Not State");
        }
    }
}

/// `renderVersionReference` (fhir-core ResourceRenderer:1597) for a RESOLVED
/// ValueSet. Branch order is the Java order; the LATEST / WILDCARD / NONE
/// branches have zero corpus hits and fire loud.
fn show_version_resolved(
    td: &mut XhtmlNode,
    uri: Option<&str>,
    actual: Option<&str>,
    from_packages: bool,
    from_this_package: bool,
) {
    let stated: Option<String> = uri.and_then(|u| u.split_once('|').map(|(_, v)| v.to_string()));
    match (&stated, actual) {
        (Some(s), Some(a)) if s != a && from_packages => {
            panic!(
                "LOUD GAP: tx version WILDCARD_BY_PACKAGE branch (RR:1605) stated={} actual={}",
                s, a
            );
        }
        (Some(s), _) => {
            // VS_VERSION_STATED
            td.set_attribute("title", format!("Version is explicitly stated to be {}", s));
            txt(td, &format!("\u{1F4CD}{}", s));
        }
        (None, _) if from_this_package => {
            // VS_VERSION_THIS_PACKAGE
            td.set_attribute(
                "title",
                "Version is not explicitly stated, which means it is fixed to the version provided in this specification",
            );
            txt(td, &format!("\u{1F4E6}{}", actual.unwrap_or("")));
        }
        (None, _) if from_packages => {
            // VS_VERSION_BY_PACKAGE
            td.set_attribute(
                "title",
                format!(
                    "Version is not explicitly stated, which means it is fixed to {}, the version found through the package references",
                    actual.unwrap_or("")
                ),
            );
            txt(td, &format!("\u{1F4E6}{}", actual.unwrap_or("")));
        }
        (None, Some(a)) => {
            // VS_VERSION_FOUND — style + title (HashMap order: style first).
            let title = format!(
                "Version is not explicitly stated. When building this specification, the most recent version {} has been used",
                a
            );
            td.set_attribute("style", "opacity: 0.5");
            td.set_attribute("title", title);
            txt(td, &format!("\u{23FF}{}", a));
        }
        (None, None) => {
            // tgt != null -> VS_VERSION_NONE (∅ + null text) — zero corpus hits.
            panic!("LOUD GAP: tx version NONE branch (RR:1637) — resolved VS without version");
        }
    }
}

fn opacity_str(inherited: bool) -> &'static str {
    if inherited {
        "0.5"
    } else {
        "1.0"
    }
}

/// `Utilities.insertBreakingSpaces(text, {'.'})`: append a zero-width space
/// after a '.' when >= 20 chars have passed since the last break.
fn insert_breaking_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    let mut since = 0usize;
    for c in text.chars() {
        out.push(c);
        since += 1;
        if since >= 20 && c == '.' {
            out.push('\u{200B}');
            since = 0;
        }
    }
    out
}

/// `VersionUtilities.isCorePackage`: the fhir core package ids.
fn is_core_package(id: &str) -> bool {
    id.starts_with("hl7.fhir.r") && id.ends_with(".core")
}

/// psdr getSourcePackageName: canonical switch, then title (getName) rules.
fn source_package_name(p: &crate::context::PkgMeta) -> String {
    match p.canonical.as_deref() {
        Some("http://terminology.hl7.org") => return "THO".to_string(),
        Some("http://hl7.org/fhir/us/core") => return "US Core".to_string(),
        Some("http://fhir.org/packages/us.nlm.vsac") => return "VSAC".to_string(),
        Some("http://fhir.org/packages/fhir.dicom") => return "DICOM".to_string(),
        _ => {}
    }
    match &p.title {
        None => p.id.clone(),
        Some(n) if n.contains('(') => n[..n.find('(').unwrap()].replace(')', ""),
        Some(n) => n.clone(),
    }
}

/// `VersionUtilities.getMajMin`: first two dotted segments.
fn maj_min(v: &str) -> String {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        v.to_string()
    }
}

/// `Utilities.isAbsoluteUrlLinkable` (http/https absolute).
fn is_absolute_url_linkable(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// `new URL(src).getHost()`.
fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', ':', '?']).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}
