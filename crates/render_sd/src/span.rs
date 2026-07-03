//! C1 `generateSpanningTable` path (SDR:3713): the `-span` / `-spanall` SD
//! fragments — a spanning table of a constraint profile and the constrained
//! reference-target profiles it points at (one hop, recursion via `processed`).
//!
//! Publisher wrapper (PublisherGenerator:2080/2084):
//!   span:    sdr.span(onlyConstraints=true, canonical, "sp")
//!   spanall: sdr.span(onlyConstraints=true, canonical, "spall")
//! Both pass `onlyConstraints=true` and the IG canonical as `constraintPrefix`;
//! the ONLY difference is the HTG uniqueLocalPrefix ("sp" vs "spall"), which the
//! HTG applies to row anchors in renderCell (golden: `sp-…` / `spall-…`).
//!
//! `initSpanningTable` (SDR:3674): active=true (isActive), 4 titles
//! (Property/Card./Content/Description), docoRef=`formats.html#table`,
//! docoImg=`pathURL(prefix="", "help16.png")` = "help16.png". Composed with the
//! HTML-non-pretty composer, wrapped `{% raw %}`.

use std::collections::HashSet;

use render_tables::model::{Cell, Row};
use render_tables::{generate, Gen};
use render_xhtml::{Config, XhtmlComposer};

use crate::context::IgContext;
use crate::sdmodel::{Ed, Sd};

/// span config: the only knobs are the anchor prefix and the table-id suffix.
pub struct SpanConfig {
    /// HTG uniqueLocalPrefix — "sp" (span) or "spall" (spanall).
    pub prefix: String,
    /// IG's `active-tables` param (ACTIVE_TABLES) — the JS-script gate.
    pub active_tables: bool,
}

impl SpanConfig {
    pub fn span() -> SpanConfig {
        SpanConfig { prefix: "sp".into(), active_tables: false }
    }
    pub fn spanall() -> SpanConfig {
        SpanConfig { prefix: "spall".into(), active_tables: false }
    }
}

/// A `SpanEntry` (SDR:422): one row's data + its child span entries.
struct SpanEntry {
    name: String,
    cardinality: String,
    profile_link: Option<String>,
    /// `type` = resType, with the Observation.code `[code=…]` suffix (SDR:3635).
    type_text: String,
    description: String,
    id: String,
    is_profile: bool,
    children: Vec<SpanEntry>,
}

/// Render one `-span`/`-spanall` fragment body (unwrapped).
pub fn render_span(sd: &Sd, ctx: &IgContext, cfg: &SpanConfig) -> String {
    // initSpanningTable uses `new HierarchicalTableGenerator(context, imageFolder,
    // false, true, "", anchorPrefix)` — mode stays null (like grid/custom), so no
    // `no-external` attrs. Gen::new gives mode None already. `anchors.clear()`
    // (SDR:3714) is a no-op for us: genSpanEntry never dedups (see below).
    let gen = Gen::new(if cfg.prefix.is_empty() { None } else { Some(cfg.prefix.clone()) });

    let mut model = init_spanning_table(sd.id());
    model.active_tables = cfg.active_tables;

    let mut processed: HashSet<String> = HashSet::new();
    let span = build_spanning_table("(focus)", "", sd, ctx, &mut processed);

    let mut rows: Vec<Row> = Vec::new();
    gen_span_entry(&mut rows, &span);
    model.rows = rows;

    let node = generate::generate(&gen, &mut model, "", 0);
    let mut c = XhtmlComposer::new(Config::html_compact());
    c.compose_node(&node)
}

/// `initSpanningTable` (SDR:3674). prefix="" (imageFolder), so docoImg =
/// `pathURL("", "help16.png")` = "help16.png"; docoRef = "formats.html#table".
fn init_spanning_table(id: &str) -> render_tables::TableModel {
    use render_tables::model::{TableModel, Title};
    let mut model = TableModel::new(Some(id.to_string()), true);
    // NOT VALID_RESOURCE / inlineGraphics -> pathURL("", "help16.png").
    model.doco_img = Some(generate::path_url("", "help16.png"));
    let doco_ref = generate::path_url("", "formats.html#table");
    model.doco_ref = Some(doco_ref.clone());
    // 4 titles (SDR:3683-3686). Text/hint = the rendering phrases.
    let t = |text: &str, hint: &str| Title::new(None, Some(doco_ref.clone()), Some(text.into()), Some(hint.into()), None, 0);
    model.titles.push(t("Property", "A profiled resource"));
    model.titles.push(t("Card.", "Minimum and Maximum # of times the element can appear in the instance"));
    model.titles.push(t("Content", "What goes here"));
    model.titles.push(t("Description", "Description of the profile"));
    model
}

/// `buildSpanningTable` (SDR:3724): build the focus entry, then (for a CONSTRAINT
/// profile not yet processed) recurse into each non-max-0 typed element's first
/// reference-target profile that is itself an in-IG constraint. The publisher's
/// `onlyConstraints=true` + `constraintPrefix=canonical` gate is applied.
fn build_spanning_table<'a>(
    name: &str,
    cardinality: &str,
    profile: &Sd,
    ctx: &IgContext,
    processed: &mut HashSet<String>,
) -> SpanEntry {
    let mut res = build_span_entry_from_profile(name, cardinality, profile, ctx);
    let url = sd_url(profile);
    let want_process = !processed.contains(&url);
    processed.insert(url);
    if want_process && profile.derivation() == "constraint" {
        let elements = profile.snapshot_elements();
        for ed in &elements {
            if ed.max() != Some("0") && !ed.types().is_empty() {
                let card = get_cardinality(*ed, &elements);
                if !card.ends_with(".0") {
                    let refs = list_reference_profiles(*ed);
                    if let Some(uri) = refs.first() {
                        // findProfileStr(uri, profile) -> the target SD. Only a
                        // CONSTRAINT whose url starts with the IG canonical is
                        // spanned (onlyConstraints=true; constraintPrefix set).
                        if let Some(child_rc) = ctx.load_resource(uri) {
                            let cd = Sd::from_value((*child_rc).clone());
                            let canonical = ctx.own_canonical_prefix();
                            let ok = cd.derivation() == "constraint"
                                && (canonical.is_none()
                                    || sd_url(&cd).starts_with(canonical.as_deref().unwrap_or("")));
                            if ok {
                                res.children.push(build_spanning_table(
                                    &name_for_element(*ed),
                                    &card,
                                    &cd,
                                    ctx,
                                    processed,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    res
}

/// `buildSpanEntryFromProfile` (SDR:3600).
fn build_span_entry_from_profile(
    name: &str,
    cardinality: &str,
    profile: &Sd,
    ctx: &IgContext,
) -> SpanEntry {
    let res_type = profile.type_name().to_string();
    // profileLink = profile.getWebPath(); own IG resource -> resolve(url).
    let profile_link = ctx.resolve(&sd_url(profile)).map(|r| r.web_path);
    let is_profile = profile.derivation() == "constraint";
    let id = profile.id().to_string();

    let mut type_b = String::from(&res_type);
    let description;
    if is_profile {
        description = sd_name(profile);
        // Observation.code key-property fixed-value summary (SDR:3617-3632).
        let mut first = true;
        let mut open = false;
        for ed in profile.snapshot_elements() {
            let base_path = ed.base_path().unwrap_or("");
            if is_key_property(base_path) {
                if let Some((suffix, val)) = ed.fixed() {
                    if first {
                        open = true;
                        first = false;
                        type_b.push('[');
                    } else {
                        type_b.push_str(", ");
                    }
                    type_b.push_str(tail(base_path));
                    type_b.push('=');
                    type_b.push_str(&summarize(suffix, val));
                }
            }
        }
        if open {
            type_b.push(']');
        }
    } else {
        // STRUC_DEF_FHIR = "{0} FHIR Resource" ... actually the phrase (checked
        // below) — non-constraint focus is never hit for these IGs (span profiles
        // are always constraints), but keep faithful: "FHIR " + name + " ".
        description = format!("{} {} ", "FHIR", sd_name(profile));
    }

    SpanEntry {
        name: name.to_string(),
        cardinality: cardinality.to_string(),
        profile_link,
        type_text: type_b,
        description,
        id,
        is_profile,
        children: Vec::new(),
    }
}

/// `genSpanEntry` (SDR:3690): one row (icon by isProfile), 4 cells, recurse.
fn gen_span_entry(rows: &mut Vec<Row>, span: &SpanEntry) {
    let mut row = Row::new();
    // row.setId(prefixAnchor("??")) then setAnchor(prefixAnchor(span.id))
    // (SDR:3692-3694). NOTE: genSpanEntry does NOT call makeAnchorUnique — the
    // SAME profile appearing under two references gets the SAME anchor (golden:
    // `sp-us-core-patient` twice, no `.2` dedup). The "sp-"/"spall-" prefix is
    // applied by the HTG in renderCell; here the anchor is the bare span id.
    row.set_id("??".to_string());
    row.set_anchor(span.id.clone());
    if span.is_profile {
        row.set_icon("icon_profile.png", Some("Profile".into()));
    } else {
        row.set_icon("icon_resource.png", Some("Resource".into()));
    }
    row.cells.push(Cell::with(None, None, Some(span.name.clone()), None, None));
    row.cells.push(Cell::with(None, None, Some(span.cardinality.clone()), None, None));
    row.cells.push(Cell::with(None, span.profile_link.clone(), Some(span.type_text.clone()), None, None));
    row.cells.push(Cell::with(None, None, Some(span.description.clone()), None, None));
    rows.push(row);
    let idx = rows.len() - 1;
    for child in &span.children {
        let mut sub = std::mem::take(&mut rows[idx].sub_rows);
        gen_span_entry(&mut sub, child);
        rows[idx].sub_rows = sub;
    }
}

/// `getCardinality` (SDR:3751): min/max, then walk parents tightening.
fn get_cardinality(ed: Ed<'_>, list: &[Ed<'_>]) -> String {
    let mut min = ed.min().unwrap_or(0);
    let mut max: i64 = match ed.max() {
        None | Some("*") => i64::MAX,
        Some(m) => m.parse().unwrap_or(i64::MAX),
    };
    let mut cur = Some(ed);
    while let Some(c) = cur {
        if !c.path().contains('.') {
            break;
        }
        cur = find_parent(c, list);
        if let Some(ned) = cur {
            if ned.max() == Some("0") {
                max = 0;
            } else if ned.max() != Some("1") && !ned.has_slicing() {
                max = i64::MAX;
            }
            if ned.min() == Some(0) || ned.min().is_none() {
                min = 0;
            }
        }
    }
    format!("{}..{}", min, if max == i64::MAX { "*".into() } else { max.to_string() })
}

/// `findParent` (SDR:3771): the nearest preceding element whose path is a strict
/// prefix (with `.`) of `ed`'s path.
fn find_parent<'a>(ed: Ed<'a>, list: &[Ed<'a>]) -> Option<Ed<'a>> {
    let pos = list.iter().position(|e| std::ptr::eq(e.v, ed.v))?;
    let mut i = pos as isize - 1;
    let want = format!("{}", ed.path());
    while i >= 0 {
        let p = format!("{}.", list[i as usize].path());
        if want.starts_with(&p) {
            return Some(list[i as usize]);
        }
        i -= 1;
    }
    None
}

/// `listReferenceProfiles` (SDR:3782): the targetProfile uris of Reference types.
fn list_reference_profiles(ed: Ed<'_>) -> Vec<String> {
    let mut res = Vec::new();
    for tr in ed.types() {
        if tr.has_target() && !tr.target_profiles().is_empty() {
            for u in tr.target_profiles() {
                res.push(u.to_string());
            }
        }
    }
    res
}

/// `nameForElement` (SDR:3794): path after the first `.`.
fn name_for_element(ed: Ed<'_>) -> String {
    let p = ed.path();
    match p.find('.') {
        Some(i) => p[i + 1..].to_string(),
        None => p.to_string(),
    }
}

/// `isKeyProperty` (SDR:3669): only `Observation.code`.
fn is_key_property(path: &str) -> bool {
    path == "Observation.code"
}

/// `summarize` (SDR:3640): Coding -> "system code"; CodeableConcept -> first
/// coding / text; else buildJson.
fn summarize(_suffix: &str, value: &serde_json::Value) -> String {
    // Only reached for Observation.code fixed values (rare). Coding/CC shapes:
    if let Some(system) = value.get("system").and_then(|x| x.as_str()) {
        let code = value.get("code").and_then(|x| x.as_str()).unwrap_or("");
        return format!("{} {}", display_system(system), code);
    }
    if let Some(coding) = value.get("coding").and_then(|x| x.as_array()).and_then(|a| a.first()) {
        let system = coding.get("system").and_then(|x| x.as_str()).unwrap_or("");
        let code = coding.get("code").and_then(|x| x.as_str()).unwrap_or("");
        return format!("{} {}", display_system(system), code);
    }
    if let Some(text) = value.get("text").and_then(|x| x.as_str()) {
        return text.to_string();
    }
    crate::table::build_json(value)
}

/// `displaySystem` for the common code systems the span summary hits.
fn display_system(uri: &str) -> String {
    match uri {
        "http://loinc.org" => "LOINC".to_string(),
        "http://snomed.info/sct" => "SNOMED CT".to_string(),
        _ => uri.to_string(),
    }
}

fn tail(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

fn sd_url(sd: &Sd) -> String {
    sd.root.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn sd_name(sd: &Sd) -> String {
    sd.root.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string()
}
