//! Port of the render half of `HierarchicalTableGenerator` (C2): `generate`,
//! `renderRow`, `renderCell`, the `init*Table` factories, `checkModel`/`check`
//! (which assign row ids), `srcFor`/`checkExists` (image-path strings),
//! `pathURL`, `prefixAnchor`/`prefixLocalHref`/`nmTokenize`.
//!
//! Source: HierarchicalTableGenerator.java (6.9.10-SNAPSHOT). The generated tree
//! is a `render_xhtml::XhtmlNode`; the publisher serializes it with
//! `new XhtmlComposer(XhtmlComposer.HTML)` == HTML, non-pretty (verified at the
//! publisher SDR wrappers, e.g. StructureDefinitionRenderer.java:516).
//!
//! We do NOT render tree-line PNGs or help16 base64: for the fragment path
//! `inLineGraphics=false` and `mode=XHTML`, so `docoImg`/tree-line srcs are just
//! filename strings (`help16.png`, `tbl_bckNNN.png`). Image files already exist
//! in the publisher output; byte parity needs only the correct src attribute.

use render_xhtml::XhtmlNode;

use crate::build::{text_node, Elem};
use crate::model::*;

/// The context a HierarchicalTableGenerator carries for the fragment path.
pub struct Gen {
    /// `mode` is set ONLY by `initNormalTable` (HTG:865); `initGridTable` and
    /// `initComparisonTable` leave it null. So it is `None` for the grid path,
    /// which is why grid `<a>`s never get the `no-external`/`data-no-external`
    /// attributes (the guard at HTG:1160 is `mode == XHTML`).
    pub mode: Option<TableGenerationMode>,
    pub unique_local_prefix: Option<String>,
    pub treelines: bool,
    /// `defPath` — set by initNormalTable's caller; only affects makeTargets
    /// anchors, unused in our path (makeTargets is true but anchors come from
    /// the SDR rows). Retained for completeness.
    pub def_path: String,
    /// `makeTargets` — true for the fragment path (HTG:810).
    pub make_targets: bool,
    /// `HierarchicalTableGenerator.uuid` (HTG:128): a per-JVM-run random UUID
    /// emitted as a comment in `treeFilterJS`. Genuinely non-deterministic in
    /// the publisher; supplied here as run context (quirk-registry entry: the
    /// corpus harness passes each IG's harvested run UUID).
    pub run_uuid: String,
}

impl Gen {
    /// The publisher builds `new HierarchicalTableGenerator(context, destDir,
    /// inlineGraphics=false, makeTargets=true, uniqueLocalPrefix)`.
    /// Grid path: `mode` unset (None). Use `new_normal(mode)` for the normal
    /// table path where `initNormalTable` sets the mode.
    pub fn new(unique_local_prefix: Option<String>) -> Gen {
        Gen {
            mode: None,
            unique_local_prefix,
            treelines: true,
            def_path: String::new(),
            make_targets: true,
            run_uuid: String::new(),
        }
    }

    /// Normal-table path: `initNormalTable` sets `this.mode = mode`.
    pub fn new_normal(unique_local_prefix: Option<String>, mode: TableGenerationMode) -> Gen {
        Gen {
            mode: Some(mode),
            unique_local_prefix,
            treelines: true,
            def_path: String::new(),
            make_targets: true,
            run_uuid: String::new(),
        }
    }

    /// `prefixAnchor` (HTG:1488).
    pub fn prefix_anchor(&self, anchor: &str) -> String {
        match &self.unique_local_prefix {
            Some(p) if !p.is_empty() => format!("{}-{}", p, anchor),
            _ => anchor.to_string(),
        }
    }

    /// `prefixLocalHref` (HTG:1492).
    pub fn prefix_local_href(&self, url: Option<&str>) -> Option<String> {
        let url = url?;
        match &self.unique_local_prefix {
            Some(p) if !p.is_empty() && url.starts_with('#') => {
                Some(format!("#{}-{}", p, &url[1..]))
            }
            _ => Some(url.to_string()),
        }
    }
}

/// `Utilities.pathURL(a, b)` for two args (HTG's usage). Reproduces the join
/// rule (Utilities.java:621).
pub fn path_url(a: &str, b: &str) -> String {
    // First non-empty arg turns on `d`; subsequent args get a "/" unless the
    // buffer already ends with "/" or the arg starts with "/", "?", "&".
    let mut s = String::new();
    let mut d = false;
    for arg in [a, b] {
        if !d {
            if !arg.is_empty() {
                d = true;
            }
        } else if !s.ends_with('/')
            && !arg.starts_with('/')
            && !arg.starts_with('?')
            && !arg.starts_with('&')
        {
            s.push('/');
        }
        s.push_str(arg);
    }
    s
}

/// `ManagedWebAccess.makeSecureRef` (ManagedWebAccess.java:195-201).
pub fn make_secure_ref(url: &str) -> String {
    if url.starts_with("http://") {
        url.replacen("http://", "https://", 1)
    } else {
        url.to_string()
    }
}

fn is_no_string(s: Option<&str>) -> bool {
    match s {
        None => true,
        Some(v) => v.is_empty(),
    }
}

/// `srcFor(corePrefix, filename)` (HTG:1291) for the fragment path
/// (`inLineGraphics=false`, `treelines=true`).
fn src_for(gen: &Gen, core_prefix: &str, filename: &str) -> String {
    let mut filename = filename.to_string();
    if !gen.treelines && filename.starts_with("tbl") {
        if filename.contains("-open") {
            filename = "tbl-open.png".to_string();
        } else if filename.contains("-closed") {
            filename = "tbl-closed.png".to_string();
        } else {
            filename = "tbl_blank.png".to_string();
        }
    }
    path_url(core_prefix, &filename)
}

/// `checkExists(indents, hasChildren, lineColor)` (HTG:1377) for the fragment
/// path: returns `tbl_bck<indents...><indent>.png`.
fn check_exists(indents: &[i32], has_children: bool, line_color: i32) -> String {
    let mut b = String::from("tbl_bck");
    for i in indents {
        b.push_str(&i.to_string());
    }
    let indent = line_color * 2 + if has_children { 1 } else { 0 };
    b.push_str(&indent.to_string());
    b.push_str(".png");
    b
}

fn nm_tokenize(anchor: &str) -> String {
    anchor.replace('[', "_").replace(']', "_")
}

// ---- table factories ----

/// `initGridTable(prefix, id)` (HTG:918). Titles use RenderingI18nContext
/// English phrases; hard-coded to the strings the goldens carry.
pub fn init_grid_table(id: Option<String>) -> TableModel {
    let mut model = TableModel::new(id, false);
    let dr = model.doco_ref.clone();
    model.titles.push(Title::new(
        None,
        dr.clone(),
        Some(phrase::GENERAL_NAME.into()),
        Some(phrase::SD_GRID_HEAD_NAME_DESC.into()),
        None,
        0,
    ));
    model.titles.push(Title::new(
        None,
        dr.clone(),
        Some(phrase::GENERAL_CARD.into()),
        Some(phrase::SD_GRID_HEAD_CARD_DESC.into()),
        None,
        0,
    ));
    model.titles.push(Title::new(
        None,
        dr.clone(),
        Some(phrase::GENERAL_TYPE.into()),
        Some(phrase::SD_GRID_HEAD_TYPE_DESC.into()),
        None,
        100,
    ));
    model.titles.push(Title::new(
        None,
        dr,
        Some(phrase::SD_GRID_HEAD_DESC.into()),
        Some(phrase::SD_GRID_HEAD_DESC_DESC.into()),
        None,
        0,
    ));
    model
}

/// `initNormalTable(prefix, isLogical, alternating, id, isActive, mode)`
/// (HTG:864). `prefix` is corePath; for fragments corePath is "" so doco_img is
/// `help16.png` and doco_ref points at the ig-guidance page.
pub fn init_normal_table(
    prefix: &str,
    is_logical: bool,
    alternating: bool,
    id: Option<String>,
    is_active: bool,
) -> TableModel {
    let mut model = TableModel::new(id, is_active);
    model.alternating = alternating;
    // mode == XHTML -> docoImg = pathURL(makeSecureRef(prefix), "help16.png")
    // (HTG:873). makeSecureRef (ManagedWebAccess.java:195): http:// -> https://.
    model.doco_img = Some(path_url(&make_secure_ref(prefix), "help16.png"));
    model.doco_ref = Some(path_url(
        "https://build.fhir.org/ig/FHIR/ig-guidance",
        "readingIgs.html#table-views",
    ));
    let dr = model.doco_ref.clone();
    model.titles.push(Title::new(
        None,
        dr.clone(),
        Some(phrase::GENERAL_NAME.into()),
        Some(phrase::GENERAL_LOGICAL_NAME.into()),
        None,
        0,
    ));
    model.titles.push(Title::new(
        None,
        dr.clone(),
        Some(phrase::GENERAL_FLAGS.into()),
        Some(phrase::SD_HEAD_FLAGS_DESC.into()),
        None,
        0,
    ));
    model.titles.push(Title::new(
        None,
        dr.clone(),
        Some(phrase::GENERAL_CARD.into()),
        Some(phrase::SD_HEAD_CARD_DESC.into()),
        None,
        0,
    ));
    model.titles.push(Title::new(
        None,
        dr.clone(),
        Some(phrase::GENERAL_TYPE.into()),
        Some(phrase::SD_GRID_HEAD_TYPE_DESC.into()),
        None,
        100,
    ));
    let mut t = Title::new(
        None,
        dr,
        Some(phrase::GENERAL_DESC_CONST.into()),
        Some(phrase::SD_HEAD_DESC_DESC.into()),
        None,
        0,
    );
    t.filter = true;
    // checkboxes inserted in Java put-order: OBLIGATIONS, CONSTRAINTS, BINDINGS.
    t.put_checkbox(phrase::GENERAL_OBLIGATIONS, "obligation");
    t.put_checkbox(phrase::GENERAL_CONSTRAINTS, "constraint");
    t.put_checkbox(phrase::GENERAL_BINDINGS, "binding");
    model.titles.push(t);
    if is_logical {
        model.titles.push(Title::new(
            None,
            Some(format!("{}structuredefinition.html#logical", prefix)),
            Some("Implemented As".into()),
            Some("How this logical data item is implemented in a concrete resource".into()),
            None,
            0,
        ));
    }
    model
}

/// A scanned custom column (StructureDefinitionRenderer.Column). `title` and
/// `hint` become a `Title` in `init_custom_table`; `id` is the scan key used to
/// gather each element's cell content.
#[derive(Debug, Clone)]
pub struct Column {
    pub id: String,
    pub title: String,
    pub hint: String,
}

impl Column {
    pub fn new(id: impl Into<String>, title: impl Into<String>, hint: impl Into<String>) -> Column {
        Column {
            id: id.into(),
            title: title.into(),
            hint: hint.into(),
        }
    }
}

/// `initCustomTable(prefix, isLogical, alternating, id, isActive, columns)`
/// (SDR:885). The BINDINGS / OBLIGATIONS column model: a Name title (same
/// GENERAL_NAME/GENERAL_LOGICAL_NAME as normal) plus one Title per scanned
/// Column. NOTE (load-bearing): docoImg = `pathURL(prefix, "help16.png")`
/// WITHOUT `makeSecureRef` (SDR:892) — so the custom-table help16 src stays
/// `http://` where the normal table's is upgraded to `https://` (golden-
/// confirmed: -bindings/-obligations carry http://, -snapshot carries https://).
pub fn init_custom_table(
    prefix: &str,
    _is_logical: bool,
    alternating: bool,
    id: Option<String>,
    is_active: bool,
    columns: &[Column],
) -> TableModel {
    let mut model = TableModel::new(id, is_active);
    model.alternating = alternating;
    // SDR:889-893: VALID_RESOURCE/inlineGraphics -> help16AsData; else pathURL
    // (NO makeSecureRef). Our fragment path is always IG_PUBLISHER (not
    // VALID_RESOURCE) and inlineGraphics=false, so the pathURL branch.
    model.doco_img = Some(path_url(prefix, "help16.png"));
    model.doco_ref = Some(path_url(
        "https://build.fhir.org/ig/FHIR/ig-guidance",
        "readingIgs.html#table-views",
    ));
    let dr = model.doco_ref.clone();
    model.titles.push(Title::new(
        None,
        dr.clone(),
        Some(phrase::GENERAL_NAME.into()),
        Some(phrase::GENERAL_LOGICAL_NAME.into()),
        None,
        0,
    ));
    for col in columns {
        model.titles.push(Title::new(
            None,
            dr.clone(),
            Some(col.title.clone()),
            Some(col.hint.clone()),
            None,
            0,
        ));
    }
    model
}

// ---- checkModel: assigns row ids (a., b., ... or index.) ----

/// `checkModel` (HTG:1330) -> `check(Row,...)` (HTG:1354). The only observable
/// effect for byte parity is `r.setId(path)`, but ids are only emitted when
/// `model.isActive()` (false in our path), so this pass is a no-op for output.
/// We still run it to stay faithful and to fail loud on malformed models.
pub fn check_model(model: &mut TableModel) {
    // Assign ids to rows (mirrors check()), harmless when inactive.
    let total = model.rows.len();
    for (i, r) in model.rows.iter_mut().enumerate() {
        assign_row_id(r, "", i, total);
    }
}

fn assign_row_id(r: &mut Row, path: &str, index: usize, total: usize) {
    let id = if total <= 26 {
        ((b'a' + index as u8) as char).to_string()
    } else {
        format!("{}.", index)
    };
    let path = format!("{}{}", path, id);
    r.id = Some(path.clone());
    let sub_total = r.sub_rows.len();
    for (i, c) in r.sub_rows.iter_mut().enumerate() {
        assign_row_id(c, &path, i, sub_total);
    }
}

// ---- generate ----

/// `generate(model, imagePath, border, outputTracker)` (HTG:941).
pub fn generate(gen: &Gen, model: &mut TableModel, image_path: &str, border: i32) -> XhtmlNode {
    check_model(model);
    // HTG:943-950: script = any title has filter or checkboxes; capture the
    // checkbox map (label -> role) from the title that carries one.
    let mut script = false;
    let mut checkboxes: Option<Vec<(String, String)>> = None;
    for t in &model.titles {
        script = script || t.filter || !t.checkboxes.is_empty();
        if !t.checkboxes.is_empty() {
            checkboxes = Some(t.checkboxes.clone());
        }
    }
    let mut table = Elem::new("table");
    table
        .set_attr("border", border.to_string())
        .set_attr("cellspacing", "0")
        .set_attr("cellpadding", "0");
    // model.active (the raw flag) gates fhir/data-fhir attrs (HTG:952). In the
    // publisher fragment path the raw `active` flag is false for both grid and
    // normal tables (initGridTable false; initNormalTable isActive=false), so
    // these branches never fire. We keep them for faithfulness.
    if model.active {
        table.set_attr("fhir", "generated-heirarchy");
        table.set_attr("data-fhir", "generated-heirarchy");
    }
    if model.is_active() {
        if let Some(id) = &model.id {
            table.set_attr("id", id);
        }
    }
    if model.border {
        table.style(
            "border: 2px black solid; font-size: 11px; font-family: verdana; vertical-align: top;",
        );
    } else {
        table.style(&format!(
            "border: {}px #F0F0F0 solid; font-size: 11px; font-family: verdana; vertical-align: top;",
            border
        ));
    }

    if model.show_headings {
        let mut tr = Elem::new("tr");
        if model.active {
            tr.set_attr("fhir", "generated-heirarchy");
            tr.set_attr("data-fhir", "generated-heirarchy");
        }
        tr.style(&format!(
            "border: {}px #F0F0F0 solid; font-size: 11px; font-family: verdana; vertical-align: top",
            1 + border
        ));
        let ntitles = model.titles.len();
        let mut last_th: Option<Elem> = None;
        for (i, t) in model.titles.iter().enumerate() {
            // flush the previous last_th (all but the final title).
            if let Some(prev) = last_th.take() {
                tr.push_elem(prev);
            }
            // HTG:973: header renderCell passes suppressExternals=true, filter =
            // model.active && t.isFilter(), mid = model.id, cbs = t.checkboxes.
            let mut th = build_cell(
                gen,
                &t.cell,
                "th",
                None,
                None,
                None,
                false,
                None,
                "white",
                0,
                image_path,
                border,
                model,
                None,
                true,
                model.active && t.filter,
                model.id.as_deref(),
                // Java passes t.getCheckboxes() unconditionally (never null for
                // a Title), so the checkbox sub-block gate is always "non-null".
                Some(&t.checkboxes),
            );
            // width (HTG:974): th.style("width: Npx") appended.
            if t.width != 0 {
                th.style(&format!("width: {}px", t.width));
            }
            // On the LAST title, attach the doco link (HTG:977).
            if i == ntitles - 1 {
                if let Some(dr) = &model.doco_ref {
                    attach_doco_link(gen, &mut th, dr, model);
                }
            }
            last_th = Some(th);
        }
        if let Some(prev) = last_th.take() {
            tr.push_elem(prev);
        }
        table.push_elem(tr);
    }

    let mut counter = Counter::new();
    // clone rows out to avoid borrow conflicts; the model is otherwise read-only
    // during row rendering.
    let rows = model.rows.clone();
    for r in &rows {
        render_row(
            gen,
            &mut table,
            r,
            &[],
            image_path,
            border,
            &mut counter,
            model,
        );
    }

    if let Some(dr) = model.doco_ref.clone() {
        let mut tr = Elem::new("tr");
        if model.active {
            tr.set_attr("fhir", "generated-heirarchy");
            tr.set_attr("data-fhir", "generated-heirarchy");
        }
        let mut tc = Elem::new("td");
        tc.set_attr("class", "hierarchy");
        tc.set_attr("colspan", model.titles.len().to_string());
        tc.push_elem(Elem::new("br"));
        let mut a = Elem::new("a");
        a.set_attr("title", phrase::SD_LEGEND);
        a.set_attr("href", dr);
        if let Some(di) = &model.doco_img {
            let mut img = Elem::new("img");
            img.set_attr("alt", "doco")
                .style("background-color: inherit")
                .set_attr("src", di.clone());
            a.push_elem(img);
        }
        a.text(format!(" {}", phrase::SD_DOCO));
        tc.push_elem(a);
        tr.push_elem(tc);
        table.push_elem(tr);
    }

    // HTG:1009-1011: the tree-filter script (raw model.active, not isActive()).
    if model.active && script {
        let mut sc = Elem::new("script");
        sc.set_attr("type", "text/javascript");
        sc.text(tree_filter_js(
            gen,
            model.id.as_deref().unwrap_or(""),
            checkboxes.as_deref().unwrap_or(&[]),
        ));
        table.push_elem(sc);
    }

    table.build()
}

/// `treeFilterJS(mid, checkboxes)` (HTG:929-939). Iterates checkbox LABELS in
/// sorted order (Utilities.sorted over keySet).
fn tree_filter_js(gen: &Gen, mid: &str, checkboxes: &[(String, String)]) -> String {
    let mut js = format!("  // {}\n", gen.run_uuid);
    let mut labels: Vec<&String> = checkboxes.iter().map(|(k, _)| k).collect();
    labels.sort();
    for s in labels {
        let role = &checkboxes.iter().find(|(k, _)| k == s).unwrap().1;
        let id = format!("cb{}-{}", mid, role);
        js.push_str(&format!(
            "document.getElementById('{}').checked = 'false' != localStorage.getItem('ht-table-states-{}');\n",
            id, role
        ));
        js.push_str(&format!(
            "filterDesc(document.getElementById('{}'), '{}', document.getElementById('cb{}-{}').checked, document.getElementById('pp{}'));\n",
            mid, role, mid, role, mid
        ));
    }
    js
}

/// The doco `<span style="float: right"><a title=... href=doco_ref><img
/// alt=doco style=... src=doco_img [onLoad]/></a></span>` appended to the last
/// header cell (HTG:977-988).
fn attach_doco_link(gen: &Gen, th: &mut Elem, doco_ref: &str, model: &TableModel) {
    let mut span = Elem::new("span");
    span.style("float: right");
    let mut a = Elem::new("a");
    a.set_attr("title", "Legend for this format");
    a.set_attr("href", doco_ref);
    if gen.mode == Some(TableGenerationMode::Xhtml) {
        a.set_attr("no-external", "true");
        a.set_attr("data-no-external", "true");
    }
    let mut img = Elem::new("img");
    img.set_attr("alt", "doco")
        .style("background-color: inherit")
        .set_attr("src", model.doco_img.clone().unwrap_or_default());
    if model.is_active() {
        img.set_attr("onLoad", "fhirTableInit(this)");
    }
    a.push_elem(img);
    span.push_elem(a);
    th.push_elem(span);
}

// ---- row / cell rendering ----

#[allow(clippy::too_many_arguments)]
fn render_row(
    gen: &Gen,
    table: &mut Elem,
    r: &Row,
    indents: &[i32],
    image_path: &str,
    border: i32,
    counter: &mut Counter,
    model: &TableModel,
) {
    if !r.partner_row {
        counter.row();
    }
    let mut tr = Elem::new("tr");
    if model.active {
        tr.set_attr("fhir", "generated-heirarchy");
        tr.set_attr("data-fhir", "generated-heirarchy");
    }
    let mut color = "white".to_string();
    if let Some(c) = &r.color {
        color = c.clone();
    } else if model.alternating && counter.is_odd() {
        color = BACKGROUND_ALT_COLOR.to_string();
    }
    let line_style = match &r.top_line {
        None => String::new(),
        Some(tl) => format!("; border-top: 1px solid {}", tl),
    };
    tr.style(&format!(
        "border: {}px #F0F0F0 solid; padding:0px; vertical-align: top; background-color: {}{}{}",
        border,
        color,
        match &r.opacity {
            None => String::new(),
            Some(o) => format!("; opacity: {}", o),
        },
        line_style
    ));
    if model.is_active() {
        if let Some(id) = &r.id {
            tr.set_attr("id", id);
        }
    }
    let mut first = true;
    let has_children = !r.sub_rows.is_empty();
    for t in &r.cells {
        let tc = build_cell(
            gen,
            t,
            "td",
            if first { r.icon.as_deref() } else { None },
            if first { r.hint.as_deref() } else { None },
            if first { Some(indents) } else { None },
            has_children,
            if first { r.anchor.as_deref() } else { None },
            &color,
            r.line_color,
            image_path,
            border,
            model,
            Some(r),
            first,
            false,
            model.id.as_deref(),
            None,
        );
        tr.push_elem(tc);
        first = false;
    }
    table.push_elem(tr);
    table.push(text_node("\r\n"));

    let n = r.sub_rows.len();
    for (i, c) in r.sub_rows.iter().enumerate() {
        let mut ind: Vec<i32> = indents.to_vec();
        if i == n - 1 {
            ind.push(r.line_color * 2);
        } else {
            ind.push(r.line_color * 2 + 1);
        }
        render_row(gen, table, c, &ind, image_path, border, counter, model);
    }
}

#[allow(clippy::too_many_arguments)]
fn build_cell(
    gen: &Gen,
    c: &Cell,
    name: &str,
    icon: Option<&str>,
    hint: Option<&str>,
    indents: Option<&[i32]>,
    has_children: bool,
    anchor: Option<&str>,
    color: &str,
    line_color: i32,
    image_path: &str,
    border: i32,
    _model: &TableModel,
    row: Option<&Row>,
    suppress_externals: bool,
    filter: bool,
    mid: Option<&str>,
    checkboxes: Option<&[(String, String)]>,
) -> Elem {
    let mut tc = Elem::new(name);
    tc.set_attr("class", "hierarchy");
    if c.span > 1 {
        tc.set_attr("colspan", c.span.to_string());
    }
    if let Some(id) = &c.id {
        tc.set_attr("id", id);
    }
    // lineStyle (HTG:1068): "" if row has no topLine, else padding.
    let line_style = match row {
        Some(r) if r.top_line.is_none() => String::new(),
        _ => "; padding-top: 3px; padding-bottom: 3px".to_string(),
    };

    // innerTable path (HTG:1072) — not exercised by SD tables; unsupported for
    // now (would panic loudly if a cell set inner_table).
    assert!(!c.inner_table, "innerTable cells not yet ported");

    // The "itc" content target == tc for non-inner tables.
    let mut content = Elem::new("__itc__"); // temp container; children merged into tc

    if let Some(indents) = indents {
        // spacer img
        let mut spacer = Elem::new("img");
        spacer
            .set_attr("src", src_for(gen, image_path, "tbl_spacer.png"))
            .style("background-color: inherit")
            .set_attr("class", "hierarchy")
            .set_attr("alt", ".");
        content.push_elem(spacer);
        // tc style with background-image
        let bg = if c
            .cell_style
            .as_deref()
            .map(|s| s.contains("background-color"))
            .unwrap_or(false)
        {
            String::new()
        } else {
            format!("background-color: {}; ", color)
        };
        let bg_image = if gen.treelines {
            format!(
                "; background-image: url({}{})",
                image_path,
                check_exists(indents, has_children, line_color)
            )
        } else {
            String::new()
        };
        let cs = match &c.cell_style {
            Some(s) => format!(";{}", s),
            None => String::new(),
        };
        tc.style(&format!(
            "vertical-align: top; text-align : var(--ig-left,left); {}border: {}px #F0F0F0 solid; padding:0px 4px 0px 4px; white-space: nowrap{}{}{}",
            bg, border, bg_image, cs, line_style
        ));
        // indent images for indents[0..len-1]
        for i in 0..indents.len().saturating_sub(1) {
            let file = match indents[i] {
                x if x == NEW_REGULAR || x == NEW_SLICER || x == NEW_SLICE => "tbl_blank.png",
                CONTINUE_REGULAR => "tbl_vline.png",
                CONTINUE_SLICER => "tbl_vline_slicer.png",
                CONTINUE_SLICE => "tbl_vline_slice.png",
                other => panic!("Unrecognized indent level: {}", other),
            };
            let mut img = Elem::new("img");
            img.set_attr("src", src_for(gen, image_path, file))
                .style("background-color: inherit")
                .set_attr("class", "hierarchy")
                .set_attr("alt", ".");
            content.push_elem(img);
        }
        // final join image (HTG:1100-1127). sfx "-open" + onClick for active
        // tables with children.
        if !indents.is_empty() {
            let sfx = if _model.is_active() && has_children {
                "-open"
            } else {
                ""
            };
            let last = indents[indents.len() - 1];
            let base = match last {
                NEW_REGULAR => "tbl_vjoin_end",
                NEW_SLICER => "tbl_vjoin_end_slicer",
                NEW_SLICE => "tbl_vjoin_end_slice",
                CONTINUE_REGULAR => "tbl_vjoin",
                CONTINUE_SLICER => "tbl_vjoin_slicer",
                CONTINUE_SLICE => "tbl_vjoin_slice",
                other => panic!("Unrecognized indent level: {}", other),
            };
            let file = format!("{}{}.png", base, sfx);
            let mut img = Elem::new("img");
            img.set_attr("src", src_for(gen, image_path, &file))
                .style("background-color: inherit")
                .set_attr("class", "hierarchy")
                .set_attr("alt", ".");
            if _model.is_active() && has_children {
                img.set_attr("onClick", "tableRowAction(this)");
            }
            content.push_elem(img);
        }
    } else {
        let bg = if c
            .cell_style
            .as_deref()
            .map(|s| s.contains("background-color"))
            .unwrap_or(false)
        {
            String::new()
        } else {
            format!("background-color: {}; ", color)
        };
        let cs = match &c.cell_style {
            Some(s) => format!(";{}", s),
            None => String::new(),
        };
        tc.style(&format!(
            "vertical-align: top; text-align : var(--ig-left,left); {}border: {}px #F0F0F0 solid; padding:0px 4px 0px 4px{}{}",
            bg, border, cs, line_style
        ));
    }

    // icon (HTG:1136)
    if !is_no_string(icon) {
        let icon = icon.unwrap();
        let mut img = Elem::new("img");
        img.set_attr("alt", "icon")
            .set_attr("src", src_for(gen, image_path, icon))
            .set_attr("class", "hierarchy")
            .style(&format!(
                "background-color: {}; background-color: inherit",
                color
            ))
            .set_attr("alt", ".");
        if let Some(h) = hint {
            img.set_attr("title", h);
        }
        content.push_elem(img);
        content.text(" ");
    }

    // pieces (HTG:1142)
    for p in &c.pieces {
        render_piece(gen, &mut content, p, suppress_externals);
    }

    // The filter UI (HTG:1209-1235) is appended to itc BEFORE the itc content
    // merges into tc, but AFTER the anchor... order in Java: pieces (itc),
    // anchor (tc), filter (itc). Since itc == tc for non-inner cells, the
    // emitted order is pieces, anchor, filter-UI.
    if gen.make_targets && !is_no_string(anchor) {
        merge(&mut tc, content);
        let mut a = Elem::new("a");
        a.set_attr("name", gen.prefix_anchor(&nm_tokenize(anchor.unwrap())));
        a.text(" ");
        tc.push_elem(a);
        // filter never co-occurs with an anchor (headers have no anchor), but
        // keep Java's order if it ever did.
        if filter {
            append_filter_ui(&mut tc, mid.unwrap_or(""), checkboxes);
        }
        return tc;
    }

    if filter {
        append_filter_ui(&mut content, mid.unwrap_or(""), checkboxes);
    }
    merge(&mut tc, content);
    tc
}

/// The tree-filter UI block (HTG:1209-1235).
fn append_filter_ui(itc: &mut Elem, mid: &str, checkboxes: Option<&[(String, String)]>) {
    // itc.nbsp() x4
    for _ in 0..4 {
        itc.nbsp();
    }
    let mut span = Elem::new("span");
    span.style("font-weight: normal");
    span.tx("Filter: ");
    // input("filter", "text", null, 10) (XhtmlNode.java:796): attrs name, type,
    // size (placeholder null -> skipped).
    let mut input = Elem::new("input");
    input.set_attr("name", "filter");
    input.set_attr("type", "text");
    input.set_attr("size", "10");
    input.style("border: 1px #F0F0F0 solid; background-color: rgb(254, 254, 231);");
    input.set_attr(
        "onInput",
        format!(
            "filterTree(document.getElementById('{}'), event.target.value)",
            mid
        ),
    );
    span.push_elem(input);
    if let Some(cbs) = checkboxes {
        span.tx(" ");
        // span.img("tree-filter.png", "Filters") (XhtmlFluent.java:224).
        let mut img = Elem::new("img");
        img.set_attr("src", "tree-filter.png");
        img.set_attr("alt", "Filters");
        img.set_attr(
            "onClick",
            format!(
                "showPanel(event.target, document.getElementById('{}'), document.getElementById('pp{}'))",
                mid, mid
            ),
        );
        span.push_elem(img);
        let mut panel = Elem::new("div");
        panel.set_attr("id", format!("pp{}", mid));
        panel.style("display: none; position: fixed; opacity : 1.0; background-color: rgb(254, 254, 231); border: 1px solid #ccc; padding: 10px; boxShadow: 0 2px 5px rgba(0,0,0,0.2); zIndex: 1000; borderRadius: 4px");
        let mut labels: Vec<&String> = cbs.iter().map(|(k, _)| k).collect();
        labels.sort();
        for s in labels {
            let v = &cbs.iter().find(|(k, _)| k == s).unwrap().1;
            panel.tx(s.as_str());
            panel.tx(" ");
            let mut input = Elem::new("input");
            input.set_attr("name", v.as_str());
            input.set_attr("type", "checkbox");
            input.set_attr("size", "1");
            input.set_attr("id", format!("cb{}-{}", mid, v));
            input.set_attr("checked", "true");
            input.set_attr(
                "onClick",
                format!(
                    "filterDesc(document.getElementById('{}'), '{}',event.target.checked, document.getElementById('pp{}'))",
                    mid, v, mid
                ),
            );
            panel.push_elem(input);
            panel.push_elem(Elem::new("br"));
        }
        span.push_elem(panel);
    }
    itc.push_elem(span);
}

/// Move `content`'s children into `tc`.
fn merge(tc: &mut Elem, content: Elem) {
    let node = content.build();
    // node is the temp __itc__ element; splice its children into tc.
    let mut kids = node;
    // XhtmlNode gives child_nodes_mut; drain into tc.
    for child in std::mem::take(kids.child_nodes_mut()) {
        tc.push(child);
    }
}

/// `renderCell` piece loop body (HTG:1142-1205).
fn render_piece(gen: &Gen, itc: &mut Elem, p: &Piece, suppress_externals: bool) {
    if !is_no_string(p.get_tag()) {
        let mut tag = Elem::new(p.get_tag().unwrap());
        if let Some(attrs) = &p.attributes {
            for (n, v) in attrs {
                tag.set_attr(n, v.clone());
            }
        }
        if let Some(h) = p.get_hint() {
            tag.set_attr("title", h);
        }
        if let Some(s) = p.get_style() {
            tag.style(s);
        }
        if p.has_children() {
            for c in &p.children {
                tag.push(c.clone());
            }
        }
        itc.push_elem(tag);
    } else if !is_no_string(p.get_reference()) {
        let mut a = Elem::new("a");
        if let Some(s) = p.get_style() {
            a.style(s);
        }
        if let Some(attrs) = &p.attributes {
            for (n, v) in attrs {
                a.set_attr(n, v.clone());
            }
        }
        if let Some(href) = gen.prefix_local_href(p.get_reference()) {
            a.set_attr("href", href);
        }
        if gen.mode == Some(TableGenerationMode::Xhtml) && suppress_externals {
            a.set_attr("no-external", "true");
            a.set_attr("data-no-external", "true");
        }
        if !is_no_string(p.get_hint()) {
            a.set_attr("title", p.get_hint().unwrap());
        }
        if let Some(t) = p.get_text() {
            a.text(t);
        } else {
            for c in &p.children {
                a.push(c.clone());
            }
        }
        // addStyle(a, p) is called again (HTG:1171) — no-op if already styled
        // (style() would double it, but Java's addStyle only sets style attr
        // via node.style, appending). Faithful: Java calls addStyle twice, so
        // the style attribute gets the piece style appended a SECOND time.
        if let Some(s) = p.get_style() {
            a.style(s);
        }
        if let Some(ti) = p.get_tag_img() {
            a.text(" ");
            // a.img(src, null) (XhtmlFluent.java:224): src then alt=".".
            let mut img = Elem::new("img");
            img.set_attr("src", ti);
            img.set_attr("alt", ".");
            a.push_elem(img);
        }
        itc.push_elem(a);
        if p.has_children() {
            for c in &p.children {
                itc.push(c.clone());
            }
        }
    } else if !is_no_string(p.get_hint()) || p.has_attributes() {
        let mut s = Elem::new("span");
        if let Some(st) = p.get_style() {
            s.style(st);
        }
        if let Some(attrs) = &p.attributes {
            for (n, v) in attrs {
                s.set_attr(n, v.clone());
            }
        }
        // s.setAttribute("title", p.getHint()) — may be null -> literal "null".
        match p.get_hint() {
            Some(h) => {
                s.set_attr("title", h);
            }
            None => {
                s.set_attr("title", "null");
            }
        }
        s.text(p.get_text().unwrap_or(""));
        itc.push_elem(s);
        if p.has_children() {
            for c in &p.children {
                itc.push(c.clone());
            }
        }
    } else if p.get_style().is_some() {
        let mut s = Elem::new("span");
        s.style(p.get_style().unwrap());
        if let Some(attrs) = &p.attributes {
            for (n, v) in attrs {
                s.set_attr(n, v.clone());
            }
        }
        s.text(p.get_text().unwrap_or(""));
        itc.push_elem(s);
        if p.has_children() {
            for c in &p.children {
                itc.push(c.clone());
            }
        }
    } else {
        itc.text(p.get_text().unwrap_or(""));
        if p.has_children() {
            for c in &p.children {
                itc.push(c.clone());
            }
        }
    }
    if let Some(ti) = p.get_tag_img() {
        // only reached in the else branches (the reference branch handled its
        // own tagImg above and returned into itc). Faithful to HTG:1200.
        if is_no_string(p.get_reference()) && is_no_string(p.get_tag()) {
            itc.text(" ");
            // itc.img(src, null): src then alt="." (XhtmlFluent.java:224).
            let mut img = Elem::new("img");
            img.set_attr("src", ti);
            img.set_attr("alt", ".");
            itc.push_elem(img);
        }
    }
}

/// English phrase constants from RenderingI18nContext (the strings the goldens
/// carry). Only the ones the table headers use.
pub mod phrase {
    pub const GENERAL_NAME: &str = "Name";
    pub const GENERAL_FLAGS: &str = "Flags";
    pub const GENERAL_CARD: &str = "Card.";
    pub const GENERAL_TYPE: &str = "Type";
    pub const GENERAL_DESC_CONST: &str = "Description & Constraints";
    pub const GENERAL_LOGICAL_NAME: &str = "The logical name of the element";
    pub const GENERAL_OBLIGATIONS: &str = "Obligations";
    pub const GENERAL_CONSTRAINTS: &str = "Constraints";
    pub const GENERAL_BINDINGS: &str = "Bindings";

    pub const SD_HEAD_FLAGS_DESC: &str = "Information about the use of the element";
    pub const SD_HEAD_CARD_DESC: &str =
        "Minimum and Maximum # of times the element can appear in the instance";
    pub const SD_HEAD_DESC_DESC: &str = "Additional information about the element";

    pub const SD_GRID_HEAD_NAME_DESC: &str =
        "The name of the element (Slice name in brackets).  Mouse-over provides definition";
    pub const SD_GRID_HEAD_CARD_DESC: &str =
        "Minimum and Maximum # of times the element can appear in the instance. Super-scripts indicate additional constraints on appearance";
    pub const SD_GRID_HEAD_TYPE_DESC: &str = "Reference to the type of the element";
    pub const SD_GRID_HEAD_DESC: &str = "Constraints and Usage";
    pub const SD_GRID_HEAD_DESC_DESC: &str =
        "Fixed values, length limits, vocabulary bindings and other usage notes";

    pub const SD_LEGEND: &str = "Legend for this format";
    pub const SD_DOCO: &str = "Documentation for this format";
}
