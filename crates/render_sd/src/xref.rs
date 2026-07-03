//! `render_sd::xref` — the two whole-IG cross-resource SD leaf scans produced by
//! the PUBLISHER's `StructureDefinitionRenderer`:
//!   - `uses`    = `uses()` (psdr:1529)     — derived-from + refer-to profile lists
//!   - `sd-xref` = `references()` (psdr:2254) — Usages: base/refs/trefs/examples/
//!                 searches/capStmts, then the XIG statistics link.
//!
//! Both scan the whole IG. The publisher denominators:
//!   - `scanAllResources(StructureDefinition.class)` (CanonicalRenderer:280) =
//!     `context.fetchResourcesByType(SD)` (ALL loaded SDs) sorted by url. Only
//!     SDs whose baseDefinition / differential type-refs `refersToThisSD` match
//!     (== this SD's url modulo `|version`) survive the filters; in this corpus
//!     that is exactly the IG's OWN SDs (dependencies never reference the IG).
//!     So we iterate `ctx.own_resources()` filtered to StructureDefinition.
//!   - `files` (FetchedFiles) for the examples / capStmts scan = the IG's own
//!     resources — `ctx.own_resources()`.
//!
//! Both methods emit raw `\r\n` HTML via StringBuilder (NOT XhtmlComposer).
//! Citations: `psdr:<line>` (publisher StructureDefinitionRenderer);
//! `phrases` (fhir-core-6911 rendering-phrases.properties, English).

use serde_json::Value;

use crate::context::{IgContext, OwnResource};
use crate::leaf::escape_xml;
use crate::sdmodel::Sd;

const MAX_DEF_SHOW: usize = 5; // psdr:2629

/// `refersToThisSD(url)` (psdr:2583): url == sd.url after stripping `|version`.
fn refers_to(url: &str, sd_url: &str) -> bool {
    let u = url.split('|').next().unwrap_or(url);
    u == sd_url
}

/// `present()` for a raw SD/resource Value: title, else name, else "".
fn present(v: &Value) -> String {
    v.get("title")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("name").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string()
}

/// The current SD's own webPath (own-IG page).
fn own_web_path(sd: &Sd, ctx: &IgContext) -> Option<String> {
    ctx.resolve(&sd.url()).map(|r| r.web_path)
}

/// `typeName(lang, rc)` (psdr:2240) — the SDT_* phrase used in the SDR_* list
/// labels. Corpus hits: Extension, Resource-constraint (=Profile). Others by
/// kind/derivation for completeness.
fn type_name(sd: &Sd) -> &'static str {
    // EXT_OBLIGATION_PROFILE_FLAG: no corpus hit (would be "Obligation Profile").
    if sd.type_name() == "Extension" {
        return "Extension"; // SDT_EXTENSION
    }
    let constraint = sd.derivation() == "constraint";
    match sd.kind() {
        "complex-type" => {
            if constraint {
                "DataType Profile"
            } else {
                "DataType"
            }
        }
        "logical" => {
            if constraint {
                "Logical Model Profile"
            } else {
                "Logical Model"
            }
        }
        "primitive-type" => {
            if constraint {
                "Primitive Type Profile"
            } else {
                "Primitive Type"
            }
        }
        "resource" => {
            if constraint {
                "Profile" // SDT_RES_PROF
            } else {
                "Resource"
            }
        }
        _ => "Definition",
    }
}

// ---------------------------------------------------------------------------
// uses (psdr:1529)
// ---------------------------------------------------------------------------

/// `listResources(b, list)` (CanonicalRenderer:249): comma-separated
/// `<a href="{webPath}">{escapeXml(present())}</a>`, webPath null -> bare.
fn list_resources(b: &mut String, list: &[(String, String)]) {
    let mut first = true;
    for (web_path, present) in list {
        if first {
            first = false;
        } else {
            b.push_str(", ");
        }
        if !web_path.is_empty() {
            b.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                escape_xml(web_path),
                escape_xml(present)
            ));
        } else {
            b.push_str(&escape_xml(present));
        }
    }
}

/// All own SDs (the scanAllResources(SD) denominator for this corpus), sorted
/// by url (CanonicalResourceSortByUrl). Returns (url, webPath, present, json).
fn own_sds(ctx: &IgContext) -> Vec<(String, String, String, std::rc::Rc<Value>)> {
    let mut v: Vec<_> = ctx
        .own_resources()
        .into_iter()
        .filter(|r| r.rtype == "StructureDefinition")
        .map(|r: OwnResource| {
            let url = r.json.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
            (url, r.web_path, r.title, r.json)
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// `findDerived` (psdr:1550): SDs whose baseDefinition refersToThisSD.
fn find_derived(all: &[(String, String, String, std::rc::Rc<Value>)], sd_url: &str) -> Vec<(String, String)> {
    all.iter()
        .filter(|(_url, _wp, _pr, j)| {
            j.get("baseDefinition")
                .and_then(|x| x.as_str())
                .map(|b| refers_to(b, sd_url))
                .unwrap_or(false)
        })
        .map(|(_url, wp, pr, _j)| (wp.clone(), pr.clone()))
        .collect()
}

/// `findUses` (psdr:1561): SDs with a DIFFERENTIAL type whose profile == this url.
fn find_uses(all: &[(String, String, String, std::rc::Rc<Value>)], sd_url: &str) -> Vec<(String, String)> {
    all.iter()
        .filter(|(_url, _wp, _pr, j)| {
            let diff = j
                .get("differential")
                .and_then(|d| d.get("element"))
                .and_then(|e| e.as_array());
            let Some(els) = diff else { return false };
            els.iter().any(|ed| {
                ed.get("type")
                    .and_then(|t| t.as_array())
                    .map(|types| {
                        types.iter().any(|tr| {
                            tr.get("profile")
                                .and_then(|p| p.as_array())
                                .map(|ps| {
                                    ps.iter().any(|c| {
                                        c.as_str().map(|s| s == sd_url).unwrap_or(false)
                                    })
                                })
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .map(|(_url, wp, pr, _j)| (wp.clone(), pr.clone()))
        .collect()
}

/// `uses()` (psdr:1529). Composer: `b.toString()` (raw StringBuilder, \r\n).
pub fn uses(sd: &Sd, ctx: &IgContext) -> String {
    let sd_url = sd.url();
    let all = own_sds(ctx);
    let mut b = String::new();
    let derived = find_derived(&all, &sd_url);
    if !derived.is_empty() {
        b.push_str("<p>\r\n");
        // STRUC_DEF_DERIVED_PROFILE (formatPhrase trims the trailing space) + " ".
        b.push_str("In this IG, the following structures are derived from this profile: ");
        list_resources(&mut b, &derived);
        b.push_str("</p>\r\n");
    }
    let users = find_uses(&all, &sd_url);
    if !users.is_empty() {
        b.push_str("<p>\r\n");
        b.push_str("In this IG, the following structures refer to this profile: ");
        list_resources(&mut b, &users);
        b.push_str("</p>\r\n");
    }
    b
}

// ---------------------------------------------------------------------------
// references (sd-xref, psdr:2254)
// ---------------------------------------------------------------------------

/// A map keyed by webPath -> present, rendered by `refList` (psdr:2630) which
/// sorts keys and comma/and-joins with the "Show N more" collapse past 5.
#[derive(Default)]
struct RefMap(std::collections::BTreeMap<String, String>);
impl RefMap {
    fn put(&mut self, k: String, v: String) {
        self.0.insert(k, v);
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    fn len(&self) -> usize {
        self.0.len()
    }
}

/// `refList(base, key)` (psdr:2630).
fn ref_list(m: &RefMap, key: &str) -> String {
    let mut b = String::new();
    let size = m.len();
    let mut c = 0usize;
    let mut show_link = false;
    for (s, disp) in m.0.iter() {
        c += 1;
        if c == MAX_DEF_SHOW && size > MAX_DEF_SHOW {
            show_link = true;
            b.push_str(&format!(
                "<span id=\"rr_{key}\" onClick=\"document.getElementById('rr_{key}').innerHTML = document.getElementById('rr2_{key}').innerHTML\">... <span style=\"cursor: pointer; border: 1px grey solid; background-color: #fcdcb3; padding-left: 3px; padding-right: 3px; color: black\">Show {n} more</span></span><span id=\"rr2_{key}\" style=\"display: none\">",
                key = key,
                n = size - MAX_DEF_SHOW + 1
            ));
        }
        if c == size && c != 1 {
            b.push_str(" and ");
        } else if c > 1 {
            b.push_str(", ");
        }
        // s is always non-empty (webPath key); Java's `s == null` bare-text
        // branch is unreachable for our maps.
        b.push_str(&format!("<a href=\"{}\">{}</a>", s, disp));
        if c % 80 == 0 {
            b.push_str("\r\n");
        }
    }
    if show_link {
        b.push_str("</span>");
    }
    b
}

/// `usesSD(element)` (psdr:2607): meta.profile refersToThisSD, or any nested
/// `extension.url` refersToThisSD.
fn uses_sd(v: &Value, sd_url: &str) -> bool {
    if let Some(meta) = v.get("meta") {
        if let Some(profiles) = meta.get("profile").and_then(|p| p.as_array()) {
            for p in profiles {
                if let Some(u) = p.as_str() {
                    if refers_to(u, sd_url) {
                        return true;
                    }
                }
            }
        }
    }
    uses_extension(v, sd_url)
}

/// `usesExtension(focus)` (psdr:2618): recursive — any child named `extension`
/// whose `url` refersToThisSD (searches all nested objects/arrays).
fn uses_extension(v: &Value, sd_url: &str) -> bool {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if k == "extension" {
                    if let Some(arr) = child.as_array() {
                        for ext in arr {
                            if let Some(u) = ext.get("url").and_then(|x| x.as_str()) {
                                if refers_to(u, sd_url) {
                                    return true;
                                }
                            }
                        }
                    }
                }
                if uses_extension(child, sd_url) {
                    return true;
                }
            }
            false
        }
        Value::Array(arr) => arr.iter().any(|c| uses_extension(c, sd_url)),
        _ => false,
    }
}

/// `scanCapStmt` (psdr:2565): a CapabilityStatement's rest.resource whose
/// profile or supportedProfile refersToThisSD -> webPath -> present.
fn scan_cap_stmt(m: &mut RefMap, cs: &Value, web_path: &str, sd_url: &str) {
    let mut inc = false;
    if let Some(rests) = cs.get("rest").and_then(|r| r.as_array()) {
        for rest in rests {
            if let Some(ress) = rest.get("resource").and_then(|r| r.as_array()) {
                for res in ress {
                    if let Some(p) = res.get("profile").and_then(|x| x.as_str()) {
                        if refers_to(p, sd_url) {
                            inc = true;
                        }
                    }
                    if let Some(sps) = res.get("supportedProfile").and_then(|x| x.as_array()) {
                        for c in sps {
                            if let Some(u) = c.as_str() {
                                if refers_to(u, sd_url) {
                                    inc = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if inc && !web_path.is_empty() {
        m.put(web_path.to_string(), present(cs));
    }
}

/// `references(lang, lrc)` (psdr:2254) => sd-xref. Composer: raw StringBuilder.
pub fn references(sd: &Sd, ctx: &IgContext) -> String {
    let sd_url = sd.url();
    let all = own_sds(ctx);

    let mut base = RefMap::default();
    let mut refs = RefMap::default();
    let mut trefs = RefMap::default();
    let mut examples = RefMap::default();
    let mut cap_stmts = RefMap::default();
    // invoked/imposed/compliedWith/searches: zero corpus hits (loud-gap-free —
    // the extensions/searchparam-expressions never match). See scanExtensions.

    for (_url, wp, pr, j) in &all {
        if j
            .get("baseDefinition")
            .and_then(|x| x.as_str())
            .map(|b| refers_to(b, &sd_url))
            .unwrap_or(false)
        {
            base.put(wp.clone(), pr.clone());
        }
        // differential type code / profile / targetProfile refs (psdr:2275-2299)
        if let Some(els) = j
            .get("differential")
            .and_then(|d| d.get("element"))
            .and_then(|e| e.as_array())
        {
            for ed in els {
                if let Some(types) = ed.get("type").and_then(|t| t.as_array()) {
                    for tr in types {
                        if let Some(code) = tr.get("code").and_then(|x| x.as_str()) {
                            if refers_to(code, &sd_url) {
                                refs.put(wp.clone(), pr.clone());
                            }
                        }
                        for arr_key in ["profile", "targetProfile"] {
                            if let Some(us) = tr.get(arr_key).and_then(|x| x.as_array()) {
                                for u in us {
                                    if let Some(uv) = u.as_str() {
                                        if refers_to(uv, &sd_url) {
                                            if arr_key == "profile" {
                                                refs.put(wp.clone(), pr.clone());
                                            } else {
                                                trefs.put(wp.clone(), pr.clone());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // examples + capStmts from the IG's own resources (the FetchedFile scan).
    // NOTE: R5-plus sdmap.details examples (psdr:2317) do not apply (R4 corpus).
    const EXAMPLE_UPPER_LIMIT: usize = 50; // psdr:85
    for r in ctx.own_resources() {
        if r.rtype == "ImplementationGuide" {
            continue;
        }
        if r.rtype == "CapabilityStatement" {
            scan_cap_stmt(&mut cap_stmts, &r.json, &r.web_path, &sd_url);
        }
        // SearchParameter expression scan (psdr:2335): no corpus hit
        // (`extension('{url}')` never present); omitted.
        if uses_sd(&r.json, &sd_url) && examples.len() < EXAMPLE_UPPER_LIMIT {
            // FetchedResource.getTitle() (FetchedResource:137): title field (set
            // by PublisherIGLoader:3031 to the resource's OWN `name` element),
            // else `Type/id`. NOT present() (title): an SD example shows its
            // `name` (USCoreAllergyIntolerance), an instance shows `Type/id`.
            let name = r.json.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let title = if name.is_empty() {
                format!("{}/{}", r.rtype, r.id)
            } else {
                name.to_string()
            };
            examples.put(r.web_path.clone(), title);
        }
    }

    let ty = type_name(sd);
    let mut b = String::new();
    // Original Source (psdr:2381-2390): no corpus hit; omitted (checked goldens).
    b.push_str("<p><b>Usages:</b></p>\r\n<ul>\r\n");
    if !base.is_empty() {
        b.push_str(&format!(
            " <li>{}: {}</li>\r\n",
            escape_xml(&format!("Derived from this {}", ty)),
            ref_list(&base, "base")
        ));
    }
    if !refs.is_empty() {
        b.push_str(&format!(
            " <li>{}: {}</li>\r\n",
            escape_xml(&format!("Use this {}", ty)),
            ref_list(&refs, "ref")
        ));
    }
    if !trefs.is_empty() {
        b.push_str(&format!(
            " <li>{}: {}</li>\r\n",
            escape_xml(&format!("Refer to this {}", ty)),
            ref_list(&trefs, "tref")
        ));
    }
    if !examples.is_empty() {
        b.push_str(&format!(
            " <li>{}: {}</li>\r\n",
            escape_xml(&format!("Examples for this {}", ty)),
            ref_list(&examples, "ex")
        ));
    }
    if !cap_stmts.is_empty() {
        b.push_str(&format!(
            " <li>{}: {}</li>\r\n",
            escape_xml(&format!("CapabilityStatements using this {}", ty)),
            ref_list(&cap_stmts, "cst")
        ));
    }
    if base.is_empty() && refs.is_empty() && trefs.is_empty() && examples.is_empty() {
        b.push_str(&format!(
            " <li>{}</li>\r\n",
            escape_xml(&format!(
                "This {} is not used by any profiles in this Specification",
                ty
            ))
        ));
    }
    b.push_str("</ul>\r\n");
    // xigReference (psdr:2557 -> SD_XIG_LINK phrase). packageId from the IG.
    let pkg_id = ctx.own_package_id().unwrap_or("");
    b.push_str(&format!(
        "<p>You can also check for <a href=\"https://packages2.fhir.org/xig/resource/{}|current/StructureDefinition/StructureDefinition-{}.json\">usages in the FHIR IG Statistics</a></p>",
        pkg_id,
        sd.id()
    ));
    // changeSummary() = "" (versionToAnnotate null); compositionSummary = null
    // (no section/entry invariants). Both empty in this corpus.
    let _ = own_web_path;
    b
}
