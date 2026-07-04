//! `render_sd::ipstmt` — the IG-level `ip-statements` fragment.
//!
//! Port of `org.hl7.fhir.igtools.renderers.IPStatementsRenderer.genIpStatements(
//! List<FetchedFile>, lang)` (the whole-IG variant, PublisherGenerator:2895 →
//! trackedFragment "1"). Citations `ipr:` = IPStatementsRenderer.java (publisher
//! 2.2.11 clone).
//!
//! The renderer walks EVERY own resource twice (ipr:117-119):
//!   - `listAllCodeSystems(element)` — the element-model tree walk that calls
//!     `seeSystem` for every `Coding.system` / `Quantity.system` (ipr:329-339);
//!   - `listAllCodeSystems(resource)` — the typed walk: StructureDefinition
//!     differential bindings → ValueSet → compose/expansion systems; ValueSet
//!     compose+expansion; CodeSystem.supplements; OperationDefinition param
//!     bindings; Questionnaire answerValueSets (ipr:249-328).
//! Each seen system is grouped by its copyright statement; systems whose
//! `getCopyRightStatement` is null (no copyright, and not LOINC/SNOMED) drop out
//! (ipr:129-135). The output lists each statement with a collapsible per-system
//! usage list (MAX_LIST_DISPLAY=3 → "Show N more", ipr:183-187).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::context::IgContext;
use crate::leaf::escape_xml;
use crate::publisher_markdown;

const MAX_LIST_DISPLAY: usize = 3; // ipr:52

// HTA copyright catalog (Messages.properties, fhir-core-6911).
const HTA_SCT_DESC: &str = "SNOMED Clinical Terms&reg; (SNOMED CT&reg;)";
const HTA_SCT_MESSAGE: &str = "This material contains content that is copyright of SNOMED International. Implementers of these specifications must have the appropriate SNOMED CT Affiliate license - for more information contact <a href=\"https://www.snomed.org/get-snomed\">https://www.snomed.org/get-snomed</a> or <a href=\"mailto:info@snomed.org\">info@snomed.org</a>.";
const HTA_LOINC_DESC: &str = "LOINC";
const HTA_LOINC_MESSAGE: &str = "This material contains content from <a href=\"http://loinc.org\">LOINC</a>. LOINC is copyright &copy; 1995-2020, Regenstrief Institute, Inc. and the Logical Observation Identifiers Names and Codes (LOINC) Committee and is available at no cost under the <a href=\"http://loinc.org/license\">license</a>. LOINC&reg; is a registered United States trademark of Regenstrief Institute, Inc.";

/// A code system's usage record (ipr:39-44). `desc` and the copyright statement
/// are resolved lazily in `get_copyright_statement`.
struct SystemUsage {
    system: String,
    desc: String,
    /// The resolved CodeSystem web path (Some) or None (cs == null, e.g. SNOMED,
    /// LOINC, or an unresolvable system).
    cs_web_path: Option<String>,
    /// Insertion order of first-seen `(title, path)` uses; deduped by source
    /// FetchedResource (ipr:80-82). Keyed here by (title,path) since a resource's
    /// identity == its (rtype,id) in our own-resource model.
    uses: Vec<(String, Option<String>)>,
    /// dedup set for `uses` (source identity = web page path).
    seen_sources: std::collections::HashSet<String>,
}

/// `seeSystem(url, source)` (ipr:71-83). `source` = (title, page-path).
fn see_system(
    systems: &mut BTreeMap<String, SystemUsage>,
    order: &mut Vec<String>,
    ctx: &IgContext,
    url: Option<&str>,
    src_key: &str,
    src_title: &str,
    src_path: &Option<String>,
) {
    let Some(url) = url else { return };
    if url.is_empty() {
        return;
    }
    if !systems.contains_key(url) {
        // cs = ctxt.fetchCodeSystem(url) — resolve to a CodeSystem web path (None
        // when the system is not a fetchable CS: SNOMED/LOINC/unitsofmeasure are
        // excluded from the terminology index or absent).
        let cs_web_path = fetch_code_system_webpath(ctx, url);
        systems.insert(
            url.to_string(),
            SystemUsage {
                system: url.to_string(),
                desc: String::new(),
                cs_web_path,
                uses: Vec::new(),
                seen_sources: std::collections::HashSet::new(),
            },
        );
        order.push(url.to_string());
    }
    let su = systems.get_mut(url).unwrap();
    if su.seen_sources.insert(src_key.to_string()) {
        su.uses.push((src_title.to_string(), src_path.clone()));
    }
}

/// The tx-fetched external CS's webPath = `{server}/ValueSet/{id}`
/// (BaseWorkerContext; same rule as vscs::resolve_cs). Used when the system is a
/// tx-cache external (nucc etc.) — those systems are excluded from the normal
/// terminology index so `resolve()` returns None, but `fetchCodeSystem` prefers
/// the tx-fetched copy (findTxResource).
fn external_cs(ctx: &IgContext, url: &str) -> Option<(String, std::rc::Rc<Value>)> {
    let (server, json) = ctx.resolve_cs_external(url)?;
    let id = json.get("id").and_then(|x| x.as_str()).unwrap_or("");
    let web = format!("{}/ValueSet/{}", server.trim_end_matches('/'), id);
    Some((web, json))
}

/// `ctxt.fetchCodeSystem(url)`: the CS web path when the url resolves to a
/// CodeSystem in the loaded closure OR a tx-cache external, else None. (SNOMED
/// etc. never resolve, matching the Java null.)
fn fetch_code_system_webpath(ctx: &IgContext, url: &str) -> Option<String> {
    if let Some(r) = ctx.resolve(url) {
        if r.rtype == "CodeSystem" {
            return Some(r.web_path);
        }
    }
    external_cs(ctx, url).map(|(web, _)| web)
}

/// The full CodeSystem JSON (own, dependency, or tx-cache external) for
/// `present()`/copyright.
fn fetch_code_system_json(ctx: &IgContext, url: &str) -> Option<std::rc::Rc<Value>> {
    if let Some(r) = ctx.resolve(url) {
        if r.rtype == "CodeSystem" {
            // Dependency CS: load_resource reads the package file. Own-IG CS:
            // scan own_resources for the matching url.
            if let Some(j) = ctx.load_resource(url) {
                return Some(j);
            }
            for own in ctx.own_resources() {
                if own.rtype == "CodeSystem"
                    && own.json.get("url").and_then(|x| x.as_str()) == Some(url)
                {
                    return Some(own.json);
                }
            }
        }
    }
    external_cs(ctx, url).map(|(_, j)| j)
}

/// present(): title || name (ipr `cs.present()`).
fn present(v: &Value) -> String {
    v.get("title")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("name").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string()
}

/// `fixCopyright` (ipr:241): THO copyright is replaced by a canonical CC0 blurb.
fn fix_copyright(copyright: &str) -> String {
    if copyright.contains("HL7 Terminology") {
        "This material derives from the HL7 Terminology (THO). THO is copyright ©1989+ Health Level Seven International and is made available under the CC0 designation. For more licensing information see: https://terminology.hl7.org/license.html".to_string()
    } else {
        copyright.to_string()
    }
}

/// `getCopyRightStatement(system)` (ipr:218-239). Sets `su.desc` as a side
/// effect and returns the copyright statement string, or None.
fn get_copyright_statement(ctx: &IgContext, su: &mut SystemUsage) -> Option<String> {
    if su.system == "http://snomed.info/sct" {
        su.desc = HTA_SCT_DESC.to_string();
        return Some(HTA_SCT_MESSAGE.to_string());
    }
    if su.system == "http://loinc.org" {
        su.desc = HTA_LOINC_DESC.to_string();
        return Some(HTA_LOINC_MESSAGE.to_string());
    }
    let cs = fetch_code_system_json(ctx, &su.system)?;
    su.desc = present(&cs);
    let copyright = cs.get("copyright").and_then(|x| x.as_str())?;
    if copyright.is_empty() {
        return None;
    }
    // markdownEngine.process(fixCopyright(copyright), "Copyright") — the bare
    // MarkDownProcessor COMMON_MARK path (no preProcessMarkdown), then
    // parseMDFragmentStripParas + XhtmlComposer(false,true).setAutoLinks(true).
    let html = publisher_markdown::md_process(&fix_copyright(copyright));
    let composed = publisher_markdown::md_fragment_strip_paras_autolinks(&html);
    if composed.is_empty() {
        Some("?".to_string())
    } else {
        Some(composed)
    }
}

// ---------------------------------------------------------------------------
// The two scanning passes over every own resource.
// ---------------------------------------------------------------------------

struct Source<'a> {
    key: String,
    title: String,
    path: Option<String>,
    ctx: &'a IgContext,
}

/// `listAllCodeSystems(element)` (ipr:329-339): the element-model tree walk. We
/// approximate the element-model type check (Coding/Quantity) structurally: an
/// object with a `system` STRING and a sibling `code` is a Coding or a coded
/// Quantity; an Identifier/ContactPoint carries `system`+`value` (no `code`) and
/// is NOT seen. Verified against the corpus goldens (all three IGs byte-parity).
fn walk_element(
    systems: &mut BTreeMap<String, SystemUsage>,
    order: &mut Vec<String>,
    src: &Source,
    node: &Value,
) {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(system)) = map.get("system") {
                if map.contains_key("code") {
                    see_system(
                        systems, order, src.ctx, Some(system), &src.key, &src.title, &src.path,
                    );
                }
            }
            for v in map.values() {
                walk_element(systems, order, src, v);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                walk_element(systems, order, src, v);
            }
        }
        _ => {}
    }
}

/// `listAllCodeSystemsVS` (ipr:288-306): compose include/exclude systems +
/// expansion contains systems (recursive).
fn scan_vs(
    systems: &mut BTreeMap<String, SystemUsage>,
    order: &mut Vec<String>,
    src: &Source,
    vs: &Value,
) {
    if let Some(compose) = vs.get("compose") {
        for key in ["include", "exclude"] {
            if let Some(arr) = compose.get(key).and_then(|x| x.as_array()) {
                for inc in arr {
                    let sys = inc.get("system").and_then(|x| x.as_str());
                    see_system(systems, order, src.ctx, sys, &src.key, &src.title, &src.path);
                }
            }
        }
    }
    if let Some(exp) = vs.get("expansion").and_then(|e| e.get("contains")).and_then(|c| c.as_array()) {
        scan_expansion(systems, order, src, exp);
    }
}

fn scan_expansion(
    systems: &mut BTreeMap<String, SystemUsage>,
    order: &mut Vec<String>,
    src: &Source,
    contains: &[Value],
) {
    for c in contains {
        let sys = c.get("system").and_then(|x| x.as_str());
        see_system(systems, order, src.ctx, sys, &src.key, &src.title, &src.path);
        if let Some(sub) = c.get("contains").and_then(|x| x.as_array()) {
            scan_expansion(systems, order, src, sub);
        }
    }
}

/// `listAllCodeSystems(resource)` typed dispatch (ipr:249-328) for SD/VS/CS/OD/Q.
fn scan_resource(
    systems: &mut BTreeMap<String, SystemUsage>,
    order: &mut Vec<String>,
    src: &Source,
    res: &Value,
) {
    match res.get("resourceType").and_then(|x| x.as_str()) {
        Some("StructureDefinition") => {
            // differential bindings → valueSet → scan_vs (ipr:318-327).
            let diff = res
                .get("differential")
                .and_then(|d| d.get("element"))
                .and_then(|e| e.as_array());
            if let Some(els) = diff {
                for ed in els {
                    if let Some(vsref) = ed
                        .get("binding")
                        .and_then(|b| b.get("valueSet"))
                        .and_then(|x| x.as_str())
                    {
                        if let Some(vs) = src.ctx.load_resource(&strip_ver(vsref)) {
                            scan_vs(systems, order, src, &vs);
                        }
                    }
                }
            }
        }
        Some("ValueSet") => scan_vs(systems, order, src, res),
        Some("CodeSystem") => {
            // seeSystem(cs.getSupplements()) (ipr:284-286).
            let sup = res.get("supplements").and_then(|x| x.as_str());
            see_system(systems, order, src.ctx, sup, &src.key, &src.title, &src.path);
        }
        Some("OperationDefinition") => {
            if let Some(params) = res.get("parameter").and_then(|x| x.as_array()) {
                for p in params {
                    if let Some(vsref) = p
                        .get("binding")
                        .and_then(|b| b.get("valueSet"))
                        .and_then(|x| x.as_str())
                    {
                        if let Some(vs) = src.ctx.load_resource(&strip_ver(vsref)) {
                            scan_vs(systems, order, src, &vs);
                        }
                    }
                }
            }
        }
        Some("Questionnaire") => scan_questionnaire(systems, order, src, res),
        _ => {}
    }
}

fn scan_questionnaire(
    systems: &mut BTreeMap<String, SystemUsage>,
    order: &mut Vec<String>,
    src: &Source,
    q: &Value,
) {
    fn walk_items(
        systems: &mut BTreeMap<String, SystemUsage>,
        order: &mut Vec<String>,
        src: &Source,
        items: &[Value],
    ) {
        for i in items {
            if let Some(vsref) = i.get("answerValueSet").and_then(|x| x.as_str()) {
                if let Some(vs) = src.ctx.load_resource(&strip_ver(vsref)) {
                    scan_vs(systems, order, src, &vs);
                }
            }
            if let Some(sub) = i.get("item").and_then(|x| x.as_array()) {
                walk_items(systems, order, src, sub);
            }
        }
    }
    if let Some(items) = q.get("item").and_then(|x| x.as_array()) {
        walk_items(systems, order, src, items);
    }
}

fn strip_ver(u: &str) -> String {
    crate::context::strip_version(u)
}

/// The logical fhirType of an R5-in-R4 `Basic` wrapper (from the
/// `http://hl7.org/fhir/5.0/StructureDefinition/extension-{Type}.{field}`
/// marker), else None. (Same detection as aggregates::xver_basic.)
fn xver_type(j: &Value) -> Option<String> {
    const PFX: &str = "http://hl7.org/fhir/5.0/StructureDefinition/extension-";
    if j.get("resourceType").and_then(|x| x.as_str()) != Some("Basic") {
        return None;
    }
    for e in j.get("extension")?.as_array()? {
        let u = e.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if let Some(rest) = u.strip_prefix(PFX) {
            if let Some((ty, _)) = rest.split_once('.') {
                return Some(ty.to_string());
            }
        }
    }
    None
}

/// The IG's `definition.resource[].reference.reference` set (`Type/id`), the
/// FetchedResource denominator (PublisherIGLoader). Empty when the IG has no
/// manifest (cycle: fall back to scanning all own resources).
fn manifest_refs(ig: &Value) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    if let Some(arr) = ig
        .get("definition")
        .and_then(|d| d.get("resource"))
        .and_then(|r| r.as_array())
    {
        for r in arr {
            if let Some(reference) = r
                .get("reference")
                .and_then(|rr| rr.get("reference"))
                .and_then(|x| x.as_str())
            {
                set.insert(reference.to_string());
            }
        }
    }
    set
}

/// `FetchedResource.getTitle()` (FetchedResource:137): the resource's `name`
/// element, else `Type/id` (same rule as sd-xref examples). For an R5-in-R4
/// `Basic` projection the `name` lives in the `extension-{Type}.name` valueString.
fn resource_title(rtype: &str, id: &str, json: &Value) -> String {
    let name = json
        .get("name")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| xver_name(json));
    match name {
        Some(n) if !n.is_empty() => n,
        _ => format!("{}/{}", rtype, id),
    }
}

/// The projected `name` of an R5-in-R4 `Basic` wrapper (the
/// `extension-{Type}.name` valueString).
fn xver_name(j: &Value) -> Option<String> {
    if j.get("resourceType").and_then(|x| x.as_str()) != Some("Basic") {
        return None;
    }
    for e in j.get("extension")?.as_array()? {
        let u = e.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if u.starts_with("http://hl7.org/fhir/5.0/StructureDefinition/extension-")
            && u.ends_with(".name")
        {
            return e.get("valueString").and_then(|x| x.as_str()).map(String::from);
        }
    }
    None
}

/// `genIpStatements(files, lang)` → `render("publication")` (ipr:114-212).
/// `ig_json` = the own ImplementationGuide resource, which the publisher includes
/// as a FetchedResource (its element-walk contributes jurisdiction Codings etc.);
/// `own_resources()` excludes it, so the harness passes it separately. The IG's
/// page path is `index.html` (FetchedResource.getPath() for the IG).
pub fn ip_statements(ctx: &IgContext, ig_json: &Value) -> String {
    let mut systems: BTreeMap<String, SystemUsage> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();

    // The IG resource itself (path = index.html, title = its `name`).
    if ig_json.get("resourceType").and_then(|x| x.as_str()) == Some("ImplementationGuide") {
        let id = ig_json.get("id").and_then(|x| x.as_str()).unwrap_or("");
        let src = Source {
            key: format!("ImplementationGuide-{}", id),
            title: resource_title("ImplementationGuide", id, ig_json),
            path: Some("index.html".to_string()),
            ctx,
        };
        walk_element(&mut systems, &mut order, &src, ig_json);
        scan_resource(&mut systems, &mut order, &src, ig_json);
    }

    // The FetchedResource denominator = the IG's `definition.resource` manifest
    // (`Type/id` reference set), NOT every own output/*.json. Build artifacts like
    // `expansions.json` (a Bundle) live in output/ but are not IG resources — they
    // must be excluded (else their expansion Codings pollute the usage lists).
    let manifest = manifest_refs(ig_json);
    for r in ctx.own_resources() {
        if r.rtype == "ImplementationGuide" {
            continue;
        }
        // R5-in-R4 `Basic` projection (e.g. Requirements): the FetchedResource's
        // logical fhirType comes from the `extension-{Type}.{field}` marker, so
        // its manifest ref / page path / `Type/id` use the logical type, not the
        // wire `Basic` (canonical-index applies the same re-projection).
        let logical_type = xver_type(&r.json).unwrap_or_else(|| r.rtype.clone());
        if !manifest.is_empty() && !manifest.contains(&format!("{}/{}", logical_type, r.id)) {
            continue;
        }
        let web_path = format!("{}-{}.html", logical_type, r.id);
        let src = Source {
            key: format!("{}-{}", logical_type, r.id),
            title: resource_title(&logical_type, &r.id, &r.json),
            path: Some(web_path),
            ctx,
        };
        // listAllCodeSystems(element) then listAllCodeSystems(resource).
        walk_element(&mut systems, &mut order, &src, &r.json);
        scan_resource(&mut systems, &mut order, &src, &r.json);
    }

    // Group by copyright statement (ipr:128-136). usages keyed by statement, in
    // first-appearance order within each statement per system-iteration order.
    // Java iterates `systems.values()` (HashMap order) but the OUTPUT statement
    // order is Utilities.sorted(usages.keySet()) (ipr:145) and the per-statement
    // system list is Collections.sort(v, SystemUsageSorter by system) (ipr:154),
    // so the HashMap iteration order does not affect output.
    let mut usages: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for sys in &order {
        let mut su = systems.remove(sys).unwrap();
        if let Some(stmt) = get_copyright_statement(ctx, &mut su) {
            usages.entry(stmt).or_default().push(sys.clone());
        }
        // put the (now desc-populated) su back for the render pass.
        systems.insert(sys.clone(), su);
    }

    let is_hl7 = ctx.own_package_id().map(|p| p.starts_with("hl7.")).unwrap_or(false);
    if usages.is_empty() {
        // IP_NONE / IP_NONE_EXT (ipr:139).
        return if is_hl7 {
            "No use of external IP".to_string()
        } else {
            "No use of external IP (other than from the FHIR specification)".to_string()
        };
    }

    let mut b = String::new();
    // IP_INTRO (ipr:142): "This {0} includes IP covered under the following statements."
    b.push_str(
        "<p>This publication includes IP covered under the following statements.</p>\r\n<ul>\r\n",
    );
    let mut key1 = 0usize;
    let mut key2 = 0usize;
    // Utilities.sorted(usages.keySet()) — BTreeMap already sorts by statement.
    for (stmt, syslist) in &usages {
        key1 += 1;
        b.push_str("<li>");
        b.push_str(stmt);
        b.push_str(&format!(
            "<div data-fhir=\"generated\" id=\"ipp_{k}\" onClick=\"if (document.getElementById('ipp2_{k}').innerHTML != '') {{document.getElementById('ipp_{k}').innerHTML = document.getElementById('ipp2_{k}').innerHTML; document.getElementById('ipp2_{k}').innerHTML = ''}}\"> <span style=\"cursor: pointer; border: 1px grey solid; background-color: #fcdcb3; padding-left: 3px; padding-right: 3px; color: black\">Show Usage</span></div><div id=\"ipp2_{k}\" style=\"display: none\">",
            k = key1
        ));
        b.push_str("\r\n<ul>\r\n");
        // Collections.sort(v, SystemUsageSorter) — by system string.
        let mut v: Vec<&String> = syslist.iter().collect();
        v.sort();
        for sysurl in v {
            let su = systems.get(sysurl).unwrap();
            b.push_str("<li>");
            match &su.cs_web_path {
                Some(wp) => {
                    b.push_str(&format!(
                        "<a href=\"{}\">{}</a>",
                        escape_xml(wp),
                        escape_xml(&su.desc)
                    ));
                }
                None => b.push_str(&escape_xml(&su.desc)),
            }
            b.push_str(": ");
            // links = title -> path (ipr:170-177); then Utilities.sorted(keys).
            let mut links: BTreeMap<String, Option<String>> = BTreeMap::new();
            for (title, path) in &su.uses {
                links.insert(title.clone(), path.clone());
            }
            key2 += 1;
            let n = links.len();
            let mut c = 0usize;
            let mut close_span = false;
            for (title, path) in &links {
                c += 1;
                if c == MAX_LIST_DISPLAY && n > MAX_LIST_DISPLAY + 2 {
                    close_span = true;
                    b.push_str(&format!(
                        "<span id=\"ips_{k}\" onClick=\"document.getElementById('ips_{k}').innerHTML = document.getElementById('ips2_{k}').innerHTML\">... <span style=\"cursor: pointer; border: 1px grey solid; background-color: #fcdcb3; padding-left: 3px; padding-right: 3px; color: black\">Show {more} more</span></span><span id=\"ips2_{k}\" style=\"display: none\">",
                        k = key2,
                        more = n - MAX_LIST_DISPLAY + 1
                    ));
                }
                if c == n && c != 1 {
                    b.push_str(" and ");
                } else if c > 1 {
                    b.push_str(", ");
                }
                match path {
                    Some(p) => b.push_str(&format!(
                        "<a href=\"{}\">{}</a>",
                        escape_xml(p),
                        escape_xml(title)
                    )),
                    None => b.push_str(&escape_xml(title)),
                }
            }
            if close_span {
                b.push_str("</span>");
            }
            b.push_str("</li>\r\n");
        }
        b.push_str("</ul>\r\n");
        b.push_str("</div>");
        b.push_str("</li>\r\n");
    }
    b.push_str("</ul>\r\n");
    b
}
