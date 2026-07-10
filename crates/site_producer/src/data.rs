//! The `_data/*.json` site-data model builder.
//!
//! The stock layouts read these via `site.data.*` (grep of `template/`):
//! `resources` (106), `info` (56), `pages` (48), `structuredefinitions` (25),
//! `fhir` (23), `artifacts` (6). This module emits all of them from the IG
//! source + resources + template config. Values are derived faithfully; the
//! JSON is serialized with `serde_json` (the Java `JsonParser.compose` pretty
//! layout is byte-parity only — the `_data` files are PARSED by the Liquid
//! engine, never rendered as text, so their formatting does not affect page
//! output. `artifacts.json` stays byte-exact for `producer_gate.rs`).
//!
//! Cited gaps (do not affect the load-bearing rendered content, or are
//! classified run-context):
//!   * `resources.json.identifiers` — the "Other Identifiers: OID:…" row is a
//!     publisher AUTO-ASSIGNED OID from a persistent registry not in source;
//!     emitted only when the source resource actually carries an identifier.
//!   * `resources.json.{history,testplan,testscript}` — git-audit/run-context,
//!     always `false`.
//!   * `structuredefinitions.json.date` — Java `Date.toString()` in the build
//!     TZ (run-context); not read by any layout.
//!   * `fhir.json.{genDate,errorCount,revision,tooling*,repoSource,…}` —
//!     build-run context; stubbed/omitted.
//!   * `pages.json` artifact-page `label` + cross-section `previous`/`next` —
//!     the publisher interleaves narrative + artifact pages in a single global
//!     order (with hierarchical artifact numbering) we do not reproduce; these
//!     drive the small footer nav + a CSS heading-prefix var only.

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::{ProducerInputs, Resource};

// Extension canonicals the status/maturity model reads (ExtensionDefinitions).
const EXT_FMM: &str = "http://hl7.org/fhir/StructureDefinition/structuredefinition-fmm";
const EXT_STANDARDS_STATUS: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status";
const EXT_NORMATIVE_VERSION: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-normative-version";

/// Emit the derivable `_data` files.
pub fn emit_data(inputs: &ProducerInputs, data: &mut BTreeMap<String, String>) -> Result<()> {
    data.insert("artifacts.json".to_string(), artifacts_json(inputs));
    data.insert(
        "structuredefinitions.json".to_string(),
        structuredefinitions_model(inputs).to_string(),
    );
    data.insert(
        "resources.json".to_string(),
        resources_json(inputs).to_string(),
    );
    data.insert("pages.json".to_string(), pages_json(inputs).to_string());
    data.insert("fhir.json".to_string(), fhir_json(inputs).to_string());
    data.insert("info.json".to_string(), info_json(inputs).to_string());
    data.insert(
        "languages.json".to_string(),
        json!({ "hasTranslations": false, "defLang": Value::Null, "langs": [] }).to_string(),
    );
    data.insert("related.json".to_string(), json!({}).to_string());
    Ok(())
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// The FHIR spec base path for the IG's fhirVersion (`fhir.json.path`,
/// `structuredefinitions.json.basepath` for core bases). `checkAppendSlash`.
fn spec_path(ig: &Value) -> String {
    let ver = ig
        .get("fhirVersion")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .or_else(|| ig.get("fhirVersion").and_then(Value::as_str))
        .unwrap_or("4.0.1");
    let rel = if ver.starts_with("5.") {
        "R5"
    } else if ver.starts_with("4.3") {
        "R4B"
    } else {
        "R4"
    };
    format!("http://hl7.org/fhir/{rel}/")
}

/// The FHIR core version string (`fhir.json.version`) for the IG's fhirVersion.
fn fhir_version(ig: &Value) -> String {
    ig.get("fhirVersion")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .or_else(|| ig.get("fhirVersion").and_then(Value::as_str))
        .unwrap_or("4.0.1")
        .to_string()
}

/// canonical = the IG url up to `/ImplementationGuide/`.
fn canonical(ig: &Value) -> Option<String> {
    str_field(ig, "url").map(|u| {
        u.split("/ImplementationGuide/")
            .next()
            .unwrap_or(&u)
            .to_string()
    })
}

/// The value of `resource.extension[url==url]` (`valueCode`/`valueString`/
/// `valueInteger`) as a string.
fn ext_value_str(res: &Value, url: &str) -> Option<String> {
    let arr = res.get("extension").and_then(Value::as_array)?;
    for e in arr {
        if e.get("url").and_then(Value::as_str) == Some(url) {
            if let Some(c) = e.get("valueCode").and_then(Value::as_str) {
                return Some(c.to_string());
            }
            if let Some(s) = e.get("valueString").and_then(Value::as_str) {
                return Some(s.to_string());
            }
            if let Some(i) = e.get("valueInteger").and_then(Value::as_i64) {
                return Some(i.to_string());
            }
        }
    }
    None
}

/// The first `contact.telecom` with `system == url` — `StatusRenderer.readOwnerLink`.
fn contact_link(res: &Value) -> Option<String> {
    for cd in res.get("contact").and_then(Value::as_array)?.iter() {
        for cp in cd
            .get("telecom")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if cp.get("system").and_then(Value::as_str) == Some("url") {
                if let Some(v) = cp.get("value").and_then(Value::as_str) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// FHIR `PublicationStatus.getDisplay()` — the code capitalized.
fn status_display(code: &str) -> String {
    let mut c = code.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `StatusRenderer.getColor(status, sStatus, fmm)` (StatusRenderer.java:180).
fn get_color(status: Option<&str>, sstatus: Option<&str>, fmm: Option<&str>) -> String {
    let fmm0 = fmm == Some("0");
    if let Some(ss) = sstatus {
        match ss.to_ascii_lowercase().as_str() {
            "draft" => return "colsd".into(),
            "trial-use" => return if fmm0 { "colsd" } else { "colstu" }.into(),
            "normative" => return "colsn".into(),
            "informative" => return "colsi".into(),
            "deprecated" => return "colsdp".into(),
            "external" => return "colse".into(),
            _ => {}
        }
    }
    if fmm.is_some() {
        return if fmm0 { "colsd" } else { "colstu" }.into();
    }
    if let Some(st) = status {
        match st.to_ascii_lowercase().as_str() {
            "draft" => return "colsd".into(),
            "retired" => return "colsdp".into(),
            _ => {}
        }
    }
    "colsi".into()
}

/// Jurisdictions as `[{code,name,flag}]`, from the resource's `jurisdiction`
/// with IG-level fallback (apply-jurisdiction). `populateResourceEntry`:4602.
fn jurisdictions(res: &Value, ig: &Value) -> Option<Value> {
    let arr = res
        .get("jurisdiction")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .or_else(|| ig.get("jurisdiction").and_then(Value::as_array))?;
    let mut out = Vec::new();
    for cc in arr {
        let coding = cc
            .get("coding")
            .and_then(Value::as_array)
            .and_then(|a| a.first());
        let (code, system) = match coding {
            Some(c) => (
                c.get("code").and_then(Value::as_str),
                c.get("system").and_then(Value::as_str),
            ),
            None => (None, None),
        };
        let mut node = Map::new();
        match (system, code) {
            (Some("urn:iso:std:iso:3166"), Some(cd)) => {
                node.insert("code".into(), json!(cd));
                let (name, flag) = country_display(cd);
                node.insert("name".into(), json!(name));
                if let Some(flag) = flag {
                    node.insert("flag".into(), json!(flag));
                }
            }
            (Some("http://unstats.un.org/unsd/methods/m49/m49.htm"), Some("001")) => {
                node.insert("code".into(), json!("001"));
                node.insert("name".into(), json!("International"));
                node.insert("flag".into(), json!("001"));
            }
            (_, Some(cd)) => {
                node.insert("code".into(), json!(cd));
                node.insert("name".into(), json!(cd));
            }
            _ => continue,
        }
        out.push(Value::Object(node));
    }
    if out.is_empty() {
        None
    } else {
        Some(Value::Array(out))
    }
}

/// ISO-3166 alpha-2 → (display name, flag-image code). Covers the codes the
/// corpus IGs use; unknown codes fall back to the code as name, no flag.
fn country_display(code: &str) -> (String, Option<String>) {
    match code {
        "US" => ("United States of America".into(), Some("usa".into())),
        "CA" => ("Canada".into(), Some("ca".into())),
        "GB" => (
            "United Kingdom of Great Britain and Northern Ireland".into(),
            Some("gb".into()),
        ),
        "AU" => ("Australia".into(), Some("au".into())),
        "NZ" => ("New Zealand".into(), Some("nz".into())),
        other => (other.to_string(), None),
    }
}

// ---------------------------------------------------------------------------
// artifacts.json — byte-identical (kept from the original)
// ---------------------------------------------------------------------------

/// `artifacts.json` — `{ "<type>-<id>.html": {"type": T[, "example": true]} }`,
/// compact. Key order follows the IG `definition.resource[]` processing order.
/// Byte-identical to the publisher output for US Core.
pub fn artifacts_json(inputs: &ProducerInputs) -> String {
    let mut s = String::from("{");
    for (i, r) in inputs.resources.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let mut o = serde_json::Map::new();
        o.insert("type".to_string(), Value::String(r.rt.clone()));
        if r.is_example {
            o.insert("example".to_string(), Value::Bool(true));
        }
        s.push_str(&serde_json::to_string(&r.base_path()).unwrap());
        s.push(':');
        s.push_str(&serde_json::to_string(&Value::Object(o)).unwrap());
    }
    s.push('}');
    s
}

// ---------------------------------------------------------------------------
// structuredefinitions.json
// ---------------------------------------------------------------------------

/// `structuredefinitions.json` — keyed by SD id. Layouts read `.abstract`,
/// `.basepath`, `.basename`, `.title`. `basename`/`basepath` resolve local SDs
/// among the IG's own; a core-R4/R5 base maps to the FHIR spec page.
pub fn structuredefinitions_model(inputs: &ProducerInputs) -> Value {
    let spec = spec_path(&inputs.ig_json);
    let sds: Vec<_> = inputs
        .resources
        .iter()
        .filter(|r| r.rt == "StructureDefinition")
        .collect();
    let by_url: BTreeMap<&str, (&Option<String>, &str)> = sds
        .iter()
        .filter_map(|r| r.url.as_deref().map(|u| (u, (&r.name, r.id.as_str()))))
        .collect();

    let mut out = Map::new();
    for (index, r) in sds.iter().enumerate() {
        let j = &r.json;
        let base = str_field(j, "baseDefinition");
        let (basename, basepath) = match base.as_deref() {
            Some(b) => match by_url.get(b) {
                // Local SD base → its name + its generated page.
                Some((n, bid)) => (
                    (*n).clone(),
                    Some(format!("StructureDefinition-{bid}.html")),
                ),
                None => {
                    let tail = b.rsplit('/').next().unwrap_or(b).to_string();
                    if b.starts_with("http://hl7.org/fhir/StructureDefinition/") {
                        // Core FHIR base → the spec page (e.g. R4/patient.html).
                        (
                            Some(tail.clone()),
                            Some(format!("{spec}{}.html", tail.to_ascii_lowercase())),
                        )
                    } else {
                        // Other external base → best-effort: tail name, base url.
                        (Some(tail), Some(b.to_string()))
                    }
                }
            },
            None => (None, None),
        };
        out.insert(
            r.id.clone(),
            json!({
                "index": index,
                "url": r.url,
                "name": r.name,
                "title": str_field(j, "title"),
                "uml": false,
                "titlelang": {},
                "path": format!("StructureDefinition-{}.html", r.id),
                "kind": r.kind,
                "type": r.type_,
                "base": base,
                "basename": basename,
                "basepath": basepath,
                "adl": false,
                "status": str_field(j, "status"),
                "date": str_field(j, "date"),
                "abstract": r.abstract_,
                "derivation": str_field(j, "derivation"),
                "publisher": str_field(j, "publisher").or_else(|| inputs.ig.publisher.clone()),
                "publisherlang": {},
                "copyright": str_field(j, "copyright"),
                "copyrightlang": {},
                "description": str_field(j, "description"),
                "descriptionlang": {},
                "obligations": false,
            }),
        );
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// resources.json
// ---------------------------------------------------------------------------

/// `resources.json` — keyed `"Type/id"` for every resource INCLUDING the IG
/// (`fragment-resourceTable.html` compares `site.data.resources[igId]`). Ports
/// `PublisherGenerator.generateDataFile` (per-resource loop :3081) +
/// `populateResourceEntry` (:4508) + `StatusRenderer.analyse`.
fn resources_json(inputs: &ProducerInputs) -> Value {
    let ig = &inputs.ig_json;
    let mut out = Map::new();

    // The IG's own entry (igId) — required for the resource-table colspan logic.
    if let Some(id) = str_field(ig, "id") {
        let mut item = Map::new();
        item.insert("history".into(), json!(false));
        item.insert("testplan".into(), json!(false));
        item.insert("testscript".into(), json!(false));
        item.insert("index".into(), json!(0));
        item.insert(
            "path".into(),
            json!(format!("ImplementationGuide-{id}.html")),
        );
        populate_entry(&mut item, ig, ig);
        out.insert(format!("ImplementationGuide/{id}"), Value::Object(item));
    }

    for (i, r) in inputs.resources.iter().enumerate() {
        let mut item = Map::new();
        item.insert("history".into(), json!(false));
        item.insert("testplan".into(), json!(false));
        item.insert("testscript".into(), json!(false));
        item.insert("index".into(), json!(i + 1));
        let fname = r
            .file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if !fname.is_empty() {
            item.insert("source".into(), json!(format!("input/resources/{fname}")));
            item.insert("sourceTail".into(), json!(fname));
        }
        item.insert("path".into(), json!(r.base_path()));
        // populateResourceEntry runs for CanonicalResources (those with a url).
        if r.json.get("url").and_then(Value::as_str).is_some() {
            populate_entry(&mut item, &r.json, ig);
        }
        out.insert(format!("{}/{}", r.rt, r.id), Value::Object(item));
    }
    Value::Object(out)
}

/// Fill a CanonicalResource's `resources.json` entry from the resource + IG
/// fallbacks (apply-publisher/version/contact/jurisdiction). `res` is the
/// resource being described (the IG itself for the igId entry).
fn populate_entry(item: &mut Map<String, Value>, res: &Value, ig: &Value) {
    if let Some(u) = str_field(res, "url") {
        item.insert("url".into(), json!(u));
    }
    // identifiers: only when the resource actually carries one (the publisher's
    // AUTO-ASSIGNED OID registry is not derivable from source — see module docs).
    if let Some(ids) = res.get("identifier").and_then(Value::as_array) {
        let vals: Vec<String> = ids
            .iter()
            .filter_map(|id| {
                let v = id.get("value").and_then(Value::as_str)?;
                Some(match v.strip_prefix("urn:oid:") {
                    Some(oid) => format!("OID:{oid}"),
                    None => match v.strip_prefix("urn:uuid:") {
                        Some(uuid) => format!("UUID:{uuid}"),
                        None => v.to_string(),
                    },
                })
            })
            .collect();
        if !vals.is_empty() {
            item.insert("identifiers".into(), json!(vals.join(", ")));
        }
    }
    let version = str_field(res, "version").or_else(|| str_field(ig, "version"));
    if let Some(v) = version {
        item.insert("version".into(), json!(v));
    }
    if let Some(n) = str_field(res, "name") {
        item.insert("name".into(), json!(n));
    }
    if let Some(t) = str_field(res, "title") {
        item.insert("title".into(), json!(t));
        item.insert("titlelang".into(), json!({}));
    }
    if let Some(e) = res.get("experimental").and_then(Value::as_bool) {
        item.insert("experimental".into(), json!(e));
    }
    if let Some(d) = str_field(res, "date") {
        item.insert("date".into(), json!(d));
    }
    if let Some(desc) = str_field(res, "description") {
        item.insert("description".into(), json!(desc));
        item.insert("descriptionlang".into(), json!({}));
    }
    if let Some(p) = str_field(res, "purpose") {
        item.insert("purpose".into(), json!(p));
    }
    if let Some(j) = jurisdictions(res, ig) {
        item.insert("jurisdictions".into(), j);
    }
    if let Some(c) = str_field(res, "copyright") {
        item.insert("copyright".into(), json!(c));
        item.insert("copyrightlang".into(), json!({}));
    }
    // fmm (top-level item field too, alongside status.fmm)
    let fmm = ext_value_str(res, EXT_FMM).or_else(|| ext_value_str(ig, EXT_FMM));
    if let Some(f) = &fmm {
        item.insert("fmm".into(), json!(f));
    }

    // status{} — StatusRenderer.analyse.
    let mut st = Map::new();
    let owner = str_field(res, "publisher").or_else(|| str_field(ig, "publisher"));
    let link = contact_link(res).or_else(|| contact_link(ig));
    let status = str_field(res, "status").map(|s| status_display(&s));
    let sstatus = ext_value_str(res, EXT_STANDARDS_STATUS)
        .or_else(|| ext_value_str(ig, EXT_STANDARDS_STATUS));
    let norm = ext_value_str(res, EXT_NORMATIVE_VERSION);
    let class = get_color(status.as_deref(), sstatus.as_deref(), fmm.as_deref());
    st.insert("class".into(), json!(class));
    if let Some(o) = owner {
        st.insert("owner".into(), json!(o));
    }
    if let Some(l) = link {
        st.insert("link".into(), json!(l));
    }
    if let Some(ss) = sstatus {
        st.insert("standards-status".into(), json!(ss));
    }
    if let Some(f) = fmm {
        st.insert("fmm".into(), json!(f));
    }
    if let Some(n) = norm {
        item.insert("normativeVersion".into(), json!(n));
    }
    if let Some(s) = status {
        st.insert("status".into(), json!(s));
    }
    item.insert("status".into(), Value::Object(st));
}

// ---------------------------------------------------------------------------
// fhir.json
// ---------------------------------------------------------------------------

fn fhir_json(inputs: &ProducerInputs) -> Value {
    let ig = &inputs.ig_json;
    let mut ig_block = Map::new();
    for (k, dst) in [
        ("id", "id"),
        ("name", "name"),
        ("title", "title"),
        ("url", "url"),
        ("status", "status"),
        ("publisher", "publisher"),
        ("description", "description"),
        ("copyright", "copyright"),
    ] {
        if let Some(v) = str_field(ig, k) {
            ig_block.insert(dst.into(), json!(v));
        }
    }
    ig_block.insert("version".into(), json!(str_field(ig, "version")));
    ig_block.insert(
        "experimental".into(),
        json!(ig
            .get("experimental")
            .and_then(Value::as_bool)
            .unwrap_or(false)),
    );
    ig_block.insert("date".into(), json!(str_field(ig, "date")));
    ig_block.insert("fhirVersion".into(), json!(fhir_version(ig)));
    ig_block.insert("titlelang".into(), json!({}));
    ig_block.insert("publisherlang".into(), json!({}));
    ig_block.insert("descriptionlang".into(), json!({}));
    ig_block.insert("copyrightlang".into(), json!({}));

    json!({
        "path": spec_path(ig),
        "canonical": canonical(ig),
        "igId": str_field(ig, "id"),
        "igName": str_field(ig, "name"),
        "packageId": str_field(ig, "id"),
        "igVer": str_field(ig, "version"),
        "version": fhir_version(ig),
        "ig": Value::Object(ig_block),
        // run-context (stubbed; not read by the gated pages):
        "errorCount": 0,
        "genDate": "",
        "genDay": "",
    })
}

// ---------------------------------------------------------------------------
// info.json — IG definition.parameter (+ template excludes default "N")
// ---------------------------------------------------------------------------

fn info_json(inputs: &ProducerInputs) -> Value {
    let ig = &inputs.ig_json;
    let mut params: BTreeMap<String, String> = BTreeMap::new();
    if let Some(arr) = ig
        .get("definition")
        .and_then(|d| d.get("parameter"))
        .and_then(Value::as_array)
    {
        for p in arr {
            if let (Some(code), Some(val)) = (
                p.get("code").and_then(Value::as_str),
                p.get("value").and_then(Value::as_str),
            ) {
                // multi-valued params (path-*, html-exempt) not needed by info.json
                params
                    .entry(code.to_string())
                    .or_insert_with(|| val.to_string());
            }
        }
    }
    let mut o = Map::new();
    if let Some(v) = params.get("releaselabel") {
        o.insert("releaselabel".into(), json!(v));
    }
    if let Some(v) = params.get("copyrightyear") {
        o.insert("copyrightyear".into(), json!(v));
    }
    // Publisher defaults for the tab-exclusion flags (mostly gate HTML-commented
    // sections; "N"/"Y" as in the F0 oracle).
    o.insert("shownav".into(), json!("N"));
    o.insert("excludexml".into(), json!("N"));
    o.insert("excludejson".into(), json!("N"));
    o.insert("excludettl".into(), json!("N"));
    o.insert("excludemap".into(), json!("N"));
    Value::Object(o)
}

// ---------------------------------------------------------------------------
// pages.json
// ---------------------------------------------------------------------------

/// `pages.json` — every page's `{title,label,breadcrumb,previous,next,…}`.
/// Narrative pages come from `ImplementationGuide.definition.page` (port of
/// `addPageData`, PublisherGenerator.java:3583); artifact pages
/// (`Type-id.html`) are synthesized with the fixed "Table of Contents →
/// Artifacts Summary → self" breadcrumb the publisher gives them.
fn pages_json(inputs: &ProducerInputs) -> Value {
    let ig = &inputs.ig_json;
    let mut out = Map::new();

    // ---- narrative pages: DFS the definition.page tree ----
    let mut narrative_order: Vec<String> = Vec::new();
    if let Some(root) = ig
        .get("definition")
        .and_then(|d| d.get("page"))
        .filter(|p| !p.is_null())
    {
        add_page_data(root, "0", "", &mut out, &mut narrative_order);
    }
    // Linear previous/next among narrative pages (genBasePages :3380).
    for w in narrative_order.windows(2) {
        if let Some(obj) = out.get_mut(&w[0]).and_then(Value::as_object_mut) {
            obj.insert("next".into(), json!(w[1]));
        }
        if let Some(obj) = out.get_mut(&w[1]).and_then(Value::as_object_mut) {
            obj.insert("previous".into(), json!(w[0]));
        }
    }

    // ---- artifact pages: one per resource that gets a base page ----
    let root_name = ig
        .get("definition")
        .and_then(|d| d.get("page"))
        .and_then(|p| p.get("nameUrl").or_else(|| p.get("name")))
        .and_then(Value::as_str)
        .unwrap_or("toc.html");
    let root_title = ig
        .get("definition")
        .and_then(|d| d.get("page"))
        .and_then(|p| p.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("Table of Contents");
    let artifact_crumb = format!(
        "<li><a href='{}'><b>{}</b></a></li><li><a href='artifacts.html'><b>Artifacts Summary</b></a></li>",
        root_name,
        escape_xml(root_title),
    );

    // Example grouping: exampleCanonical (profile url) -> [(exampleUrl, title)].
    let examples_by_profile = example_groups(ig);

    // Artifact page title = the IG `definition.resource[].name` (keyed by Type/id).
    // The publisher titles every artifact page (SD/example/VS/CS) from its IG
    // registration name, not the resource's own title/name — an example instance
    // carries no title, so the resource-field fallback would wrongly show `Type/id`.
    let ig_names = ig_resource_names(ig);

    for r in &inputs.resources {
        let url = r.base_path();
        let title = ig_names
            .get(&format!("{}/{}", r.rt, r.id))
            .cloned()
            .unwrap_or_else(|| artifact_title(r));
        let mut entry = Map::new();
        entry.insert("title".into(), json!(title));
        entry.insert("titlelang".into(), json!({}));
        entry.insert(
            "breadcrumb".into(),
            json!(format!(
                "{artifact_crumb}<li><b>{}</b></li>",
                escape_xml(&title)
            )),
        );
        // intro/notes — only when the fragment file is actually present.
        let intro = format!("{}-{}-intro.md", r.rt, r.id);
        if inputs.page_includes.contains(&intro) {
            entry.insert("intro".into(), json!(intro));
            entry.insert("intro-type".into(), json!("md"));
        }
        let notes = format!("{}-{}-notes.md", r.rt, r.id);
        if inputs.page_includes.contains(&notes) {
            entry.insert("notes".into(), json!(notes));
            entry.insert("notes-type".into(), json!("md"));
        }
        // examples list (for the profile-examples layout).
        if let Some(url_key) = &r.url {
            if let Some(exs) = examples_by_profile.get(url_key) {
                let arr: Vec<Value> = exs
                    .iter()
                    .map(|(u, t)| json!({ "url": u, "title": t, "titlelang": {} }))
                    .collect();
                entry.insert("examples".into(), json!(arr));
            }
        }
        // Don't clobber a narrative page that already registered this url.
        out.entry(url).or_insert(Value::Object(entry));
    }

    // The pages.json KEY must equal the render surface's `page.path` (the shell
    // file location). Apply the page-dir prefix to the KEYS only — the inner
    // href values (breadcrumb, previous/next, example urls) stay FLAT (in-site
    // relative links). FLAT (native) leaves keys unchanged.
    if inputs.page_prefix.is_empty() {
        Value::Object(out)
    } else {
        let prefixed: Map<String, Value> = out
            .into_iter()
            .map(|(k, v)| (format!("{}{k}", inputs.page_prefix), v))
            .collect();
        Value::Object(prefixed)
    }
}

/// Port of `addPageData` (PublisherGenerator.java:3583) for narrative pages.
/// `label` is this page's numbering prefix; `crumb` is the accumulated ancestor
/// breadcrumb (links). Records into `out` and appends the page name to `order`.
fn add_page_data(
    page: &Value,
    label: &str,
    crumb: &str,
    out: &mut Map<String, Value>,
    order: &mut Vec<String>,
) {
    let name = page
        .get("nameUrl")
        .or_else(|| page.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = page.get("title").and_then(Value::as_str).unwrap_or("");
    let children = page.get("page").and_then(Value::as_array);
    let has_children = children.map(|c| !c.is_empty()).unwrap_or(false);

    let row_label = if has_children {
        format!("{label}.0")
    } else {
        label.to_string()
    };
    let self_crumb = format!("{crumb}<li><b>{}</b></li>", escape_xml(title));

    if !name.is_empty() {
        let mut entry = Map::new();
        entry.insert("title".into(), json!(title));
        entry.insert("titlelang".into(), json!({}));
        entry.insert("label".into(), json!(row_label));
        entry.insert("breadcrumb".into(), json!(self_crumb));
        entry.insert("breadcrumblang".into(), json!({}));
        out.insert(name.to_string(), Value::Object(entry));
        order.push(name.to_string());
    }

    if let Some(children) = children {
        // child breadcrumb = accumulated + link to THIS page.
        let child_crumb = format!(
            "{crumb}<li><a href='{name}'><b>{}</b></a></li>",
            escape_xml(title)
        );
        for (i, child) in children.iter().enumerate() {
            let child_label = if label == "0" {
                (i + 1).to_string()
            } else {
                format!("{label}.{}", i + 1)
            };
            add_page_data(child, &child_label, &child_crumb, out, order);
        }
    }
}

/// The artifact-page title: canonical `title` ?? `name` ?? `Type/id`.
fn artifact_title(r: &Resource) -> String {
    str_field(&r.json, "title")
        .or_else(|| r.name.clone())
        .unwrap_or_else(|| format!("{}/{}", r.rt, r.id))
}

/// `Type/id` -> the IG registration `name`, from `IG.definition.resource[]`.
/// This is the publisher's artifact-page title source for every artifact kind.
fn ig_resource_names(ig: &Value) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(arr) = ig
        .get("definition")
        .and_then(|d| d.get("resource"))
        .and_then(Value::as_array)
    else {
        return map;
    };
    for r in arr {
        let Some(reference) = r
            .get("reference")
            .and_then(|x| x.get("reference"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if let Some(name) = r.get("name").and_then(Value::as_str) {
            map.insert(reference.to_string(), name.to_string());
        }
    }
    map
}

/// exampleCanonical (profile url) -> [(example page url, example title)], from
/// `IG.definition.resource[]`.
fn example_groups(ig: &Value) -> BTreeMap<String, Vec<(String, String)>> {
    let mut map: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let Some(arr) = ig
        .get("definition")
        .and_then(|d| d.get("resource"))
        .and_then(Value::as_array)
    else {
        return map;
    };
    for r in arr {
        let Some(profile) = r.get("exampleCanonical").and_then(Value::as_str) else {
            continue;
        };
        let reference = r
            .get("reference")
            .and_then(|x| x.get("reference"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let Some((rt, id)) = reference.split_once('/') else {
            continue;
        };
        let url = format!("{rt}-{id}.html");
        let title = r
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();
        map.entry(profile.to_string())
            .or_default()
            .push((url, title));
    }
    // The publisher renders examples sorted by page url (Set<FetchedResource>
    // ordering); match it so the example list is byte-stable.
    for v in map.values_mut() {
        v.sort();
    }
    map
}

/// Minimal XML text escaping (`Utilities.escapeXml`) for breadcrumb titles.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
