//! `render_sd::xreflist` — the `*-ref-list` / `*-ref-all-list` IG aggregates
//! (CrossViewRenderer `renderVSList`/`renderCSList` with `used=true`,
//! PublisherGenerator pg:2787-2810).
//!
//! These are the `valueset-list` / `codesystem-list` tables with an extra
//! **References** column (`used=true`): for every VS/CS *referenced* by an IG
//! resource, the set of resources that reference it. This module owns the
//! whole-IG reference SCAN (`buildUsed{VS,CS}List`, CVR:1239/1568); the table
//! itself is rendered by `aggregates::{render_vs_list, render_cs_list}` with the
//! `used` extension (identical URL/Version/Name/Status/Flags/Source|Count cells).
//!
//!  - `valueset-ref-list`       = renderVSList(buildUsedValueSetList(all=false), used=true)
//!  - `valueset-ref-all-list`   = renderVSList(buildUsedValueSetList(all=true),  used=true)
//!  - `codesystem-ref-list`     = renderCSList(buildUsedCodeSystemList(all=false),used=true)
//!  - `codesystem-ref-all-list` = renderCSList(buildUsedCodeSystemList(all=true), used=true)
//!
//! `all=false` scans the source resource's **differential** element bindings;
//! `all=true` scans the **snapshot** (CVR:1327/1637). Rows = every resolved
//! referenced VS/CS (own AND dependency), sorted by url
//! (`CanonicalResourceSortByUrl`, CVR:1397/1690).
//!
//! ## UNSTABLE-ORACLE quirk (References column ordering)
//! The References cell iterates a Java `HashSet<Resource>` in identity-hash order
//! (CVR:1497 VS / 1762 CS, the `rl.size() < 10` branch) — nondeterministic across
//! JVM runs. `rl.size() >= 10` collapses to the deterministic string
//! `"{N} references"`. We emit a DETERMINISTIC order: first-seen over the sorted
//! own-resource scan. Per cell the (title,link) MULTISET matches the golden
//! (verified: no duplicate pairs corpus-wide → multiset == set), only intra-cell
//! ORDER can differ — same unstable-oracle class as quirk #3 / the HTG uuid (#1).
//! `bin/corpus classify-reflist` proves per-cell set-equality.
//!
//! Citations: `cvr:` = CrossViewRenderer.java (publisher 2.2.11 clone).

use serde_json::Value;
use std::collections::HashMap;
use std::rc::Rc;

use crate::context::{IgContext, OwnResource};

/// One resolved used-row: the referenced VS/CS + its ordered referencing sources.
pub struct UsedRow {
    /// resolved canonical url (row URL cell).
    pub url: String,
    /// resolved webPath (row URL cell href).
    pub web: String,
    /// the referenced resource's full JSON (for Name/Status/Flags/Source cells).
    pub json: Rc<Value>,
    /// (title, web_path) per referencing source, DETERMINISTIC first-seen order.
    pub refs: Vec<(String, String)>,
    ref_keys: Vec<String>,
}

struct Scan {
    order: Vec<String>,
    rows: HashMap<String, UsedRow>,
}

impl Scan {
    fn new() -> Self {
        Scan { order: Vec::new(), rows: HashMap::new() }
    }
    fn into_sorted(self) -> Vec<UsedRow> {
        let mut rows: Vec<UsedRow> = self.order.into_iter().filter_map(|k| self.rows.get(&k).map(|r| UsedRow {
            url: r.url.clone(), web: r.web.clone(), json: r.json.clone(),
            refs: r.refs.clone(), ref_keys: r.ref_keys.clone(),
        })).collect();
        rows.sort_by(|a, b| a.url.cmp(&b.url)); // CanonicalResourceSortByUrl
        rows
    }
    /// resolveVS/resolveCS (CVR:1354/1668): resolve `url` as `want`; if found,
    /// add the row (once) and record `src` in its reference set. Mirrors the
    /// Java: the row is added iff the resource resolves, and `rl.add(source)`.
    fn touch(&mut self, ctx: &IgContext, want: &str, url: &str, src: &SrcRef) {
        if let Some((key, row_url, web, json)) = self.resolve_row(ctx, want, url) {
            let entry = self.ensure_row(key, row_url, web, json);
            if !entry.ref_keys.contains(&src.key) {
                entry.ref_keys.push(src.key.clone());
                entry.refs.push((src.title.clone(), src.link.clone()));
            }
        }
    }

    /// findValueSets(vs) (CVR:1291) `list.add(vs)`: add the row for an
    /// ALREADY-IN-HAND resource with NO reference recorded (the VS itself is a
    /// row even when nothing references it; its own refs come only from OTHER
    /// resources binding it).
    fn add_row_no_ref(&mut self, ctx: &IgContext, want: &str, url: &str) {
        if let Some((key, row_url, web, json)) = self.resolve_row(ctx, want, url) {
            self.ensure_row(key, row_url, web, json);
        }
    }

    fn resolve_row(&self, ctx: &IgContext, want: &str, url: &str) -> Option<(String, String, String, Rc<Value>)> {
        if url.is_empty() {
            return None;
        }
        let base = url.split('|').next().unwrap_or(url);
        if let Some(res) = ctx.resolve(base) {
            if res.rtype == want {
                let json = ctx.load_resource(base).or_else(|| own_json(ctx, base))?;
                let row_url = json.get("url").and_then(|x| x.as_str()).unwrap_or(base).to_string();
                return Some((row_url.clone(), row_url, res.web_path, json));
            }
        }
        // fetchResource(CodeSystem,url) also finds tx-cache externals
        // (cs-externals.json) that CanonicalResourceManager.get() misses — e.g.
        // nucc/formatcode, dropped from THO by INVALID_TERMINOLOGY_URLS yet
        // present as tx-fetched externals. webPath = `{server}/ValueSet/{cs.id}`
        // (BaseWorkerContext external-resource path). (fetchCodeSystem, used by
        // describeSource, does NOT do this — hence the valueset-list "Other".)
        if want == "CodeSystem" {
            if let Some((server, json)) = ctx.resolve_cs_external(base) {
                let id = json.get("id").and_then(|x| x.as_str()).unwrap_or("");
                let row_url = json.get("url").and_then(|x| x.as_str()).unwrap_or(base).to_string();
                let web = format!("{}/ValueSet/{}", server, id);
                return Some((row_url.clone(), row_url, web, json));
            }
        }
        None
    }

    fn ensure_row(&mut self, key: String, row_url: String, web: String, json: Rc<Value>) -> &mut UsedRow {
        self.rows.entry(key.clone()).or_insert_with(|| {
            self.order.push(key);
            UsedRow { url: row_url, web, json, refs: Vec::new(), ref_keys: Vec::new() }
        })
    }
}

fn own_json(ctx: &IgContext, url: &str) -> Option<Rc<Value>> {
    ctx.own_resources()
        .into_iter()
        .find(|r| r.json.get("url").and_then(|x| x.as_str()) == Some(url))
        .map(|r| r.json)
}

struct SrcRef {
    title: String,
    link: String,
    key: String,
}
impl SrcRef {
    fn of(r: &OwnResource) -> Self {
        // FetchedResource.getTitle() = own name via present()=title||name
        // (PublisherIGLoader:3031); non-canonical → Type/id.
        let title = if !r.title.is_empty() {
            r.title.clone()
        } else {
            format!("{}/{}", r.rtype, r.id)
        };
        SrcRef { title, link: r.web_path.clone(), key: r.web_path.clone() }
    }

    /// A source that is itself a resolved canonical resource (a ValueSet whose
    /// includes reference a CS, or whose imports reference another VS). The Java
    /// records the VS OBJECT as source; the render cell title = present()
    /// (CanonicalResource) and link = webPath (CVR:1498-1501).
    fn of_resolved(ctx: &IgContext, url: &str) -> Option<Self> {
        let base = url.split('|').next().unwrap_or(url);
        let res = ctx.resolve(base)?;
        let title = res.present();
        Some(SrcRef { title, link: res.web_path.clone(), key: res.web_path })
    }
}

/// The own resources in a stable scan order (seeds the deterministic References
/// order; the multiset is order-independent so this only fixes OUR member).
fn sorted_own(ctx: &IgContext) -> Vec<OwnResource> {
    let mut v = ctx.own_resources();
    v.sort_by(|a, b| (a.rtype.as_str(), a.id.as_str()).cmp(&(b.rtype.as_str(), b.id.as_str())));
    v
}

/// buildUsedValueSetList (CVR:1239) — the referenced-VS rows, sorted.
pub fn used_vs_rows(ctx: &IgContext, all: bool) -> Vec<UsedRow> {
    let mut s = Scan::new();
    for r in sorted_own(ctx) {
        let src = SrcRef::of(&r);
        find_vs_refs(&mut s, ctx, &r.json, &src, all);
    }
    s.into_sorted()
}

/// needVersionReferences over the USED-VS list (the pg:2799 argument the CS ref
/// tables share for their Version column flag).
pub fn used_vs_needs_version(ctx: &IgContext, ig_version: &str, all: bool) -> bool {
    used_vs_rows(ctx, all).iter().any(|r| {
        r.json.get("version").and_then(|x| x.as_str()).unwrap_or("") != ig_version
    })
}

/// buildUsedCodeSystemList (CVR:1553) — the referenced-CS rows, sorted.
pub fn used_cs_rows(ctx: &IgContext, all: bool) -> Vec<UsedRow> {
    let mut s = Scan::new();
    for r in sorted_own(ctx) {
        let src = SrcRef::of(&r);
        find_cs_refs(&mut s, ctx, &r.json, &src, all);
    }
    s.into_sorted()
}

// --- findValueSetReferences (CVR:1252) ---

fn find_vs_refs(s: &mut Scan, ctx: &IgContext, res: &Value, src: &SrcRef, all: bool) {
    match res.get("resourceType").and_then(|x| x.as_str()) {
        Some("StructureDefinition") => {
            for ed in sd_elements(res, all) {
                for vs in ed_binding_valuesets(&ed) {
                    s.touch(ctx, "ValueSet", &vs, src);
                }
            }
        }
        Some("ValueSet") => {
            // findValueSets(vs) (CVR:1291): list.add(vs) — the VS is a row with
            // NO reference recorded; only its IMPORTED valueSets get a reference,
            // whose SOURCE is the VS itself (resolveVS(u, vs), CVR:1297), NOT the
            // outer scan source.
            if let Some(url) = res.get("url").and_then(|x| x.as_str()) {
                s.add_row_no_ref(ctx, "ValueSet", url);
            }
            let vs_src = res
                .get("url")
                .and_then(|x| x.as_str())
                .and_then(|u| SrcRef::of_resolved(ctx, u));
            if let Some(vs_src) = vs_src {
                for inc in compose_includes(res) {
                    for u in inc.get("valueSet").and_then(|x| x.as_array()).into_iter().flatten() {
                        if let Some(u) = u.as_str() {
                            s.touch(ctx, "ValueSet", u, &vs_src);
                        }
                    }
                }
            }
        }
        Some("ConceptMap") => {
            for k in ["sourceScope", "sourceScopeUri", "targetScope", "targetScopeUri", "source", "target"] {
                if let Some(u) = res.get(k).and_then(|x| x.as_str()) {
                    s.touch(ctx, "ValueSet", u, src);
                }
            }
        }
        Some("Questionnaire") => {
            for u in questionnaire_answer_valuesets(res) {
                s.touch(ctx, "ValueSet", &u, src);
            }
        }
        Some("OperationDefinition") => {
            for p in res.get("parameter").and_then(|x| x.as_array()).into_iter().flatten() {
                if let Some(u) = p.get("binding").and_then(|b| b.get("valueSet")).and_then(|x| x.as_str()) {
                    s.touch(ctx, "ValueSet", u, src);
                }
            }
        }
        _ => {}
    }
    for c in res.get("contained").and_then(|x| x.as_array()).into_iter().flatten() {
        find_vs_refs(s, ctx, c, src, all);
    }
}

// --- findCodeSystemReferences (CVR:1568) ---

fn find_cs_refs(s: &mut Scan, ctx: &IgContext, res: &Value, src: &SrcRef, all: bool) {
    match res.get("resourceType").and_then(|x| x.as_str()) {
        Some("StructureDefinition") => {
            for ed in sd_elements(res, all) {
                for vs in ed_binding_valuesets(&ed) {
                    cs_from_vs(s, ctx, &vs, src);
                }
            }
        }
        Some("ValueSet") => {
            for inc in compose_includes(res) {
                if let Some(sys) = inc.get("system").and_then(|x| x.as_str()) {
                    s.touch(ctx, "CodeSystem", sys, src);
                }
            }
        }
        Some("ConceptMap") => {
            for k in ["sourceScope", "sourceScopeUri", "targetScope", "targetScopeUri"] {
                if let Some(u) = res.get(k).and_then(|x| x.as_str()) {
                    cs_from_vs(s, ctx, u, src);
                }
            }
            for g in res.get("group").and_then(|x| x.as_array()).into_iter().flatten() {
                for k in ["source", "target"] {
                    if let Some(u) = g.get(k).and_then(|x| x.as_str()) {
                        s.touch(ctx, "CodeSystem", u, src);
                    }
                }
            }
        }
        Some("Questionnaire") => {
            for u in questionnaire_answer_valuesets(res) {
                cs_from_vs(s, ctx, &u, src);
            }
        }
        Some("OperationDefinition") => {
            for p in res.get("parameter").and_then(|x| x.as_array()).into_iter().flatten() {
                if let Some(u) = p.get("binding").and_then(|b| b.get("valueSet")).and_then(|x| x.as_str()) {
                    cs_from_vs(s, ctx, u, src);
                }
            }
        }
        _ => {}
    }
    for c in res.get("contained").and_then(|x| x.as_array()).into_iter().flatten() {
        find_cs_refs(s, ctx, c, src, all);
    }
}

/// resolveCSFromVS (CVR:1660) → findCodeSystems(vs, source=SD) → resolveCS(system,
/// **vs**) (CVR:1650): the CS reference SOURCE is the resolved VALUESET, not the
/// SD that bound it. `_outer_src` (the SD) is intentionally discarded to match.
fn cs_from_vs(s: &mut Scan, ctx: &IgContext, vs_url: &str, _outer_src: &SrcRef) {
    let base = vs_url.split('|').next().unwrap_or(vs_url);
    let Some(vsj) = ctx.load_resource(base).or_else(|| own_json(ctx, base)) else { return };
    if vsj.get("resourceType").and_then(|x| x.as_str()) != Some("ValueSet") {
        return;
    }
    let Some(vs_src) = SrcRef::of_resolved(ctx, base) else { return };
    for inc in compose_includes(&vsj) {
        if let Some(sys) = inc.get("system").and_then(|x| x.as_str()) {
            s.touch(ctx, "CodeSystem", sys, &vs_src);
        }
    }
}

// --- accessors ---

fn sd_elements(sd: &Value, all: bool) -> Vec<Value> {
    let key = if all { "snapshot" } else { "differential" };
    sd.get(key)
        .and_then(|s| s.get("element"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default()
}

const EXT_ADDITIONAL_BINDING: &str =
    "http://hl7.org/fhir/tools/StructureDefinition/additional-binding";

fn ed_binding_valuesets(ed: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(b) = ed.get("binding") {
        if let Some(vs) = b.get("valueSet").and_then(|x| x.as_str()) {
            out.push(vs.to_string());
        }
        // R5-native binding.additional[].valueSet.
        for ab in b.get("additional").and_then(|x| x.as_array()).into_iter().flatten() {
            if let Some(vs) = ab.get("valueSet").and_then(|x| x.as_str()) {
                out.push(vs.to_string());
            }
        }
        // R4 output shape: additional bindings are the tools
        // `additional-binding` extension carrying a nested `valueSet`
        // sub-extension (valueCanonical). ed.getBinding().getAdditional() reads
        // these in fhir-core R5. (us-core DocumentReference.type → clinical-note-type.)
        for ext in b.get("extension").and_then(|x| x.as_array()).into_iter().flatten() {
            if ext.get("url").and_then(|x| x.as_str()) == Some(EXT_ADDITIONAL_BINDING) {
                for sub in ext.get("extension").and_then(|x| x.as_array()).into_iter().flatten() {
                    if sub.get("url").and_then(|x| x.as_str()) == Some("valueSet") {
                        if let Some(vs) = sub
                            .get("valueCanonical")
                            .or_else(|| sub.get("valueUri"))
                            .and_then(|x| x.as_str())
                        {
                            out.push(vs.to_string());
                        }
                    }
                }
            }
        }
    }
    out
}

fn compose_includes(vs: &Value) -> Vec<Value> {
    vs.get("compose")
        .and_then(|c| c.get("include"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default()
}

fn questionnaire_answer_valuesets(q: &Value) -> Vec<String> {
    fn walk(item: &Value, out: &mut Vec<String>) {
        if let Some(vs) = item.get("answerValueSet").and_then(|x| x.as_str()) {
            out.push(vs.to_string());
        }
        for c in item.get("item").and_then(|x| x.as_array()).into_iter().flatten() {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    for item in q.get("item").and_then(|x| x.as_array()).into_iter().flatten() {
        walk(item, &mut out);
    }
    out
}

/// The References cell (CVR:1494-1505 VS / 1759-1770 CS).
pub fn references_cell(refs: &[(String, String)]) -> String {
    use crate::leaf::escape_xml;
    let mut c = String::from("<td>");
    if refs.len() >= 10 {
        c.push_str(&format!("{} references", refs.len()));
    } else {
        let mut first = true;
        for (title, link) in refs {
            if first {
                first = false;
            } else {
                c.push_str(", ");
            }
            c.push_str(&format!("<a href=\"{}\">{}</a>", escape_xml(link), escape_xml(title)));
        }
    }
    c.push_str("</td>");
    c
}
