//! C1 `generateTable` path (SUMMARY mode): the snapshot/diff element table.
//! Source: fhir-core 6.9.10 StructureDefinitionRenderer.java (SDR):
//! `generateTable:575`, `generateTableInner:610`, `genElement:917`,
//! `genElementNameCell:1318`, `genElementCells:1348`, `genCardinality:1428`,
//! `genTypes:2317`, `genTargetLink:2529`, `generateDescription:1541`,
//! `makeChoiceRows:3362`, plus HierarchicalTableGenerator (HTG).
//!
//! Publisher wrapper flags (scratchpad fhir-ig-publisher SDR wrapper):
//!   snapshot():510  -> generateTable(diff=F, snapshot=T, allInv=T,  ms=F, prefix "s"/"sa",  idSfx S/SA)
//!   diff():487      -> generateTable(diff=T, snapshot=F, allInv=F,  ms=F, prefix ""/"a",    idSfx D/DA)
//!   byKey():532     -> generateTable(diff=F, snapshot=T, allInv=T,  ms=F, prefix "k"/"ka",  idSfx K/KA) on key elements
//!   byMustSupport():547 -> generateTable(diff=F, snapshot=T, allInv=F, ms=T, prefix "m"/"ma", idSfx M/MA) on MS elements
//! All composed with `new XhtmlComposer(XhtmlComposer.HTML)` (HTML, non-pretty)
//! and border=0 (SDR:583 `gen.generate(model, imagePath, 0, tracker)`).
//!
//! Known gaps (marked in `gaps`): obligations tables (C5), additional-bindings
//! tables (C5), complex fixed/pattern partner rows (genFixedValue), choice
//! groups (readChoices/processConstraint), logical models, mappings/bindings/
//! obligations table modes.

use std::collections::HashMap;

use render_tables::model::{Cell, Piece, Row, TableGenerationMode, TableModel};
use render_tables::{generate, Gen};
use render_xhtml::{Config, XhtmlComposer};

use crate::context::{IgContext, Resolved};
use crate::markdown;
use crate::sdmodel::{Ed, Sd, TypeRef};

pub const RED_BACKGROUND_COLOR: &str = "#D50000"; // SDR:104
pub const CONSTRAINT_CHAR: &str = "C"; // SDR:392
pub const CONSTRAINT_STYLE: &str = "padding-left: 3px; padding-right: 3px; border: 1px maroon solid; font-weight: bold; color: #301212; background-color: #fdf4f4;"; // SDR:393

/// Per-fragment configuration (the publisher wrapper flags).
#[derive(Debug, Clone)]
pub struct TableConfig {
    pub diff: bool,
    pub snapshot: bool,
    pub all_invariants: bool,
    pub must_support: bool,
    /// uniqueLocalPrefix on the HTG ("s"/"sa"/"k"/"ka"/"m"/"ma"; "" for diff).
    pub prefix: String,
    /// id suffix on the table model id ("S","SA","D","DA","K","KA","M","MA").
    pub id_sfx: String,
    /// The per-run HTG uuid (quirk: harvested per IG).
    pub run_uuid: String,
    /// The IG's `active-tables` parameter (template-injected;
    /// PublisherIGLoader.java:443 sets HTG.ACTIVE_TABLES from it).
    pub active_tables: bool,
}

impl TableConfig {
    pub fn snapshot(run_uuid: &str) -> TableConfig {
        TableConfig {
            diff: false,
            snapshot: true,
            all_invariants: true,
            must_support: false,
            prefix: "s".into(),
            id_sfx: "S".into(),
            run_uuid: run_uuid.into(),
            active_tables: false,
        }
    }
    pub fn snapshot_all(run_uuid: &str) -> TableConfig {
        TableConfig {
            prefix: "sa".into(),
            id_sfx: "SA".into(),
            ..TableConfig::snapshot(run_uuid)
        }
    }
}

/// Render one SD table fragment body (unwrapped).
pub fn render_table(
    sd: &Sd,
    ctx: &IgContext,
    def_file: &str,
    cfg: &TableConfig,
) -> (String, Vec<String>) {
    let mut gen = Gen::new_normal(
        if cfg.prefix.is_empty() {
            None
        } else {
            Some(cfg.prefix.clone())
        },
        TableGenerationMode::Xhtml,
    );
    gen.run_uuid = cfg.run_uuid.clone();

    // corePath: the publisher passes the core-spec web root with trailing
    // slash (verified in goldens: terminologies.html/conformance-rules links
    // and the https help16.png all live under http://hl7.org/fhir/R4/).
    let core_path = core_path_for(sd.fhir_version());
    // initNormalTable(corePath, isLogical=false, alternating=true,
    // id=profile.id+idSfx, isActive=IG_PUBLISHER(true), mode=XHTML) (SDR:641).
    let mut model = generate::init_normal_table(
        core_path,
        false,
        true,
        Some(format!("{}{}", sd.id(), cfg.id_sfx)),
        true,
    );
    model.active_tables = cfg.active_tables;

    let all: Vec<Ed> = sd.snapshot_elements();
    let mut t = TCtx {
        ctx,
        sd,
        all: &all,
        cfg,
        gen: &gen,
        anchors: HashMap::new(),
        def_path: if def_file.is_empty() {
            None
        } else {
            Some(format!("{}#", def_file))
        },
        core_path,
        is_constraint_mode: sd.derivation() == "constraint" && uses_must_support(&all),
        key_rows: Vec::new(),
        gaps: Vec::new(),
        merged_pattern_values: HashMap::new(),
    };

    let mut rows: Vec<Row> = Vec::new();
    if let Some(first) = all.first() {
        t.gen_element(&mut rows, *first, true);
    }
    model.rows = rows;

    let node = generate::generate(&gen, &mut model, "", 0);
    let mut c = XhtmlComposer::new(Config::html_compact());
    (c.compose_node(&node), t.gaps)
}

struct TCtx<'a> {
    ctx: &'a IgContext,
    sd: &'a Sd,
    all: &'a [Ed<'a>],
    cfg: &'a TableConfig,
    gen: &'a Gen,
    anchors: HashMap<String, i32>,
    def_path: Option<String>,
    core_path: &'static str,
    is_constraint_mode: bool,
    key_rows: Vec<String>,
    gaps: Vec<String>,
    /// `mergedPatternValues` (SDR:611, 2927-2942): element index (in `all`) ->
    /// merged pattern child values.
    merged_pattern_values: HashMap<usize, Vec<serde_json::Value>>,
}

struct UnusedTracker {
    used: bool,
}

impl<'a> TCtx<'a> {
    fn gap(&mut self, what: &str) {
        self.gaps.push(what.to_string());
    }

    /// `makeAnchorUnique` (SDR:1201).
    fn make_anchor_unique(&mut self, anchor: &str) -> String {
        if let Some(cnt) = self.anchors.get(anchor).copied() {
            let c = cnt + 1;
            self.anchors.insert(anchor.to_string(), c);
            format!("{}.{}", anchor, c)
        } else {
            self.anchors.insert(anchor.to_string(), 1);
            anchor.to_string()
        }
    }

    /// `genElement` (SDR:917), SUMMARY mode.
    ///
    /// Java threads a `slicingRow` pointer: the function RETURNS the row that
    /// owns subsequent slice siblings (SDR:1174 `return slicingRow`), and the
    /// parent's child loop nests a sliceName child under the previous slicer
    /// when `noTail(slicer.getId()) == child.getPath()` (SDR:1126). We return
    /// the INDEX (in `rows`) of the row this element pushed iff it became the
    /// slicing row, plus its id, so the caller can route slice siblings into
    /// `rows[idx].sub_rows`.
    fn gen_element(&mut self, rows: &mut Vec<Row>, element: Ed<'a>, root: bool) -> Option<(usize, String)> {
        let children = get_children(self.all, element);
        // onlyInformationIsMapping ~ never true for real corpora.
        let mut row = Row::new();
        // 6.9.11 (the golden jar's fhir-core): anchor = element ID, path
        // fallback (SDR@6.9.11:933; the ONLY behavioral 6.9.10->6.9.11 change
        // in this file). Scaffold rows keep PATH anchors.
        let raw_anchor = if element.id().is_empty() {
            element.path()
        } else {
            element.id()
        };
        let anchor = self.make_anchor_unique(raw_anchor);
        // context.prefixAnchor is the BASE RenderingContext (no prefix) — the
        // "s"/"k" prefix comes from the HTG in renderCell (see grid notes).
        row.set_id(anchor.clone());
        row.set_anchor(anchor.clone());
        // getRowColor (ProfileUtilities:4897): always null absent validation
        // userdata -> leave None (lets alternating background apply).
        if element.has_slicing() {
            row.set_line_color(1);
        } else if element.has_slice_name() {
            row.set_line_color(2);
        } else {
            row.set_line_color(0);
        }
        let types = element.types();
        let s_tail = tail(element.path());
        let mut ext = false;
        // icon chain (SDR:943-992)
        if s_tail == "extension" && is_extension_elem(element) {
            if !types.is_empty() && !types[0].profiles().is_empty()
                && self.extension_is_complex(types[0].profiles()[0])
            {
                row.set_icon("icon_extension_complex.png", Some("Complex Extension".into()));
            } else {
                row.set_icon("icon_extension_simple.png", Some("Simple Extension".into()));
            }
            ext = true;
        } else if s_tail == "modifierExtension" {
            if !types.is_empty() && !types[0].profiles().is_empty()
                && self.extension_is_complex(types[0].profiles()[0])
            {
                row.set_icon("icon_modifier_extension_complex.png", Some("Complex Extension".into()));
            } else {
                row.set_icon("icon_modifier_extension_simple.png", Some("Simple Extension".into()));
            }
            ext = true;
        } else if types.is_empty() {
            if root && self.is_resource_type(self.sd_type()) {
                row.set_icon("icon_resource.png", Some("Resource".into()));
            } else {
                row.set_icon("icon_element.gif", Some("Element".into()));
            }
        } else if types.len() > 1 {
            if all_are_reference(&types) {
                row.set_icon("icon_reference.png", Some("Reference to another Resource".into()));
            } else {
                row.set_icon("icon_choice.gif", Some("Choice of Types".into()));
                // typesRow = row (choice [x] handling below)
            }
        } else if types[0].working_code().starts_with('@') {
            row.set_icon("icon_reuse.png", Some("Reference to another Element".into()));
        } else if self.ctx.is_primitive_type(types[0].working_code()) {
            if self.key_rows.contains(&element.id().to_string()) {
                row.set_icon("icon-key.png", Some("JSON Key Value".into()));
            } else {
                row.set_icon("icon_primitive.png", Some("Primitive Data Type".into()));
            }
        } else if types[0].has_target() {
            row.set_icon("icon_reference.png", Some("Reference to another Resource".into()));
        } else if self.ctx.is_data_type(types[0].working_code()) {
            row.set_icon("icon_datatype.gif", Some("Data Type".into()));
        } else if matches!(types[0].working_code(), "Base" | "Element" | "BackboneElement") {
            row.set_icon("icon_element.gif", Some("Element".into()));
        } else {
            row.set_icon("icon_resource.png", Some("Resource".into()));
        }
        let types_row = types.len() > 1 && !all_are_reference(&types);

        let mut used = UnusedTracker { used: true };
        let ref_ = self
            .def_path
            .as_ref()
            .map(|dp| format!("{}{}", dp, element.id()));
        // PREFIX_SLICES = true (SDR:402): sName = tail[:sliceName]
        let mut s_name = s_tail.to_string();
        if let Some(sn) = element.slice_name() {
            s_name = format!("{}:{}", s_name, sn);
        }

        // name cell (SDR:1318)
        let name_cell_idx = self.gen_element_name_cell(&mut row, element, ref_.clone(), s_name.clone());
        // SUMMARY cells (SDR:1030)
        self.gen_element_cells(
            &mut row,
            element,
            &types,
            ext,
            types_row,
            root,
            &mut used,
            name_cell_idx,
            !children.is_empty(),
        );

        // slicing icon overrides (SDR:1033-1048)
        let mut this_is_slicer = false;
        if element.has_slicing() {
            if standard_extension_slicing(element) {
                used.used = true;
                this_is_slicer = true;
            } else {
                row.set_icon("icon_slice.png", Some("Slice Definition".into()));
                this_is_slicer = true;
                for cell in &mut row.cells {
                    for p in &mut cell.pieces {
                        p.add_style("font-style: italic");
                    }
                }
            }
        } else if element.has_slice_name() {
            row.set_icon("icon_slice_item.png", Some("Slice Item".into()));
        }

        // showMissing = the table-level `diff` flag (generateTableInner:651
        // passes `diff` as genElement's showMissing). For snapshot tables a
        // max=0 element (tracker.used=false) is DROPPED (unless it set up
        // standard extension slicing, which forces used=true above).
        let mut show_missing = self.cfg.diff;
        if this_is_slicer && standard_extension_slicing(element) {
            show_missing = false;
        }
        if !(used.used || show_missing) {
            return None;
        }
        rows.push(row);
        let row_idx = rows.len() - 1;
        if !used.used && !element.has_slicing() {
            // (SDR:1051-1059) kept-but-unused rows: strike through pieces.
            for cell in &mut rows[row_idx].cells {
                for p in &mut cell.pieces {
                    if p.underived {
                        p.set_style("font-style: italic");
                    } else {
                        p.set_style("text-decoration:line-through");
                    }
                }
            }
            return None;
        }

        // ":All Slices" holder (SDR:1061-1088): created when THIS element
        // changed the slicing row (this_is_slicer) and it has structural
        // children — the children then nest under the holder, while slice
        // SIBLINGS (handled by our caller) nest under our top row.
        let mut has_holder = false;
        if this_is_slicer && !children.is_empty() {
            let mut hrow = Row::new();
            let anchor_e = self.make_anchor_unique(element.path());
            hrow.set_id(anchor_e.clone());
            hrow.set_anchor(anchor_e);
            hrow.set_line_color(1);
            hrow.set_icon("icon_element.gif", Some("Element".into()));
            hrow.cells.push(Cell::with(
                None,
                None,
                Some(format!("{}{}", s_name, ":All Slices")),
                Some("".into()),
                None,
            ));
            hrow.cells.push(Cell::new());
            hrow.cells.push(Cell::new());
            hrow.cells.push(Cell::new());
            hrow.cells.push(Cell::with(
                None,
                None,
                Some("Content/Rules for all slices".into()),
                Some("".into()),
                None,
            ));
            rows[row_idx].sub_rows.push(hrow);
            has_holder = true;
        }
        // typesRow holder (choice with children) (SDR:1089-1116)
        if types_row && !children.is_empty() {
            let mut hrow = Row::new();
            let anchor_e = self.make_anchor_unique(element.path());
            hrow.set_id(anchor_e.clone());
            hrow.set_anchor(anchor_e);
            hrow.set_line_color(1);
            hrow.set_icon("icon_element.gif", Some("Element".into()));
            hrow.cells.push(Cell::with(
                None,
                None,
                Some(format!("{}{}", s_name, ":All Types")),
                Some("".into()),
                None,
            ));
            hrow.cells.push(Cell::new());
            hrow.cells.push(Cell::new());
            hrow.cells.push(Cell::new());
            hrow.cells.push(Cell::with(
                None,
                None,
                Some("Content/Rules for all Types".into()),
                Some("".into()),
                None,
            ));
            rows[row_idx].sub_rows.push(hrow);
            has_holder = true;
        }

        // children walk (SDR:1118-1163). `target` = the holder row if one was
        // pushed (Java reassigns the local `row` to hrow), else our row. The
        // children push into target.sub_rows; a slice child nests under the
        // current `slicer` row (a previous child's slicing-entry row), keyed by
        // noTail(slicer.id) == child.path (SDR:1126).
        if !prohibited(element) {
            // slicer location: index path within target.sub_rows + the row id.
            let mut slicer: Option<(Vec<usize>, String)> = None;
            for child in &children {
                // .id children skipped unless logicalModel/constraint (SDR:1160)
                if child.path().ends_with(".id") && self.sd.derivation() != "constraint" {
                    continue;
                }
                // route: compute the parent container path (within target.sub_rows)
                let parent_path: Vec<usize> = if child.has_slice_name() {
                    let need_new = match &slicer {
                        Some((_, sid)) => no_tail(sid) != child.path(),
                        None => true,
                    };
                    if need_new {
                        // "Slices for X" scaffold row (SDR:1127-1152)
                        let mut parent = Row::new();
                        let anchor_e = self.make_anchor_unique(child.path());
                        parent.set_id(anchor_e.clone());
                        parent.set_anchor(anchor_e.clone());
                        parent.set_line_color(1);
                        parent.set_icon("icon_slice.png", Some("Slice Definition".into()));
                        parent.cells.push(Cell::with(
                            None,
                            None,
                            Some(format!("Slices for {}", tail(child.path()))),
                            Some("".into()),
                            None,
                        ));
                        parent.cells.push(Cell::new());
                        parent.cells.push(Cell::new());
                        parent.cells.push(Cell::new());
                        parent.cells.push(Cell::with(
                            None,
                            None,
                            Some("Content/Rules for all slices".into()),
                            Some("".into()),
                            None,
                        ));
                        let target = target_subrows(rows, row_idx, has_holder);
                        target.push(parent);
                        let loc = vec![target.len() - 1];
                        slicer = Some((loc.clone(), anchor_e));
                        loc
                    } else {
                        slicer.as_ref().unwrap().0.clone()
                    }
                } else {
                    Vec::new()
                };

                // fetch the container Vec<Row> at parent_path and recurse.
                let container: &mut Vec<Row> = {
                    let target = target_subrows(rows, row_idx, has_holder);
                    descend(target, &parent_path)
                };
                let ret = self.gen_element(container, *child, false);
                if let Some((idx_in_container, id)) = ret {
                    let mut loc = parent_path.clone();
                    loc.push(idx_in_container);
                    slicer = Some((loc, id));
                }
            }
        }

        // choice [x] type rows (SDR:1170-1172): appended to typesRow.getSubRows()
        // — the element's TOP row's sub_rows (typesRow == row; the holder is a
        // child within it, so choice rows come after the holder).
        if types_row && !prohibited(element) {
            let container = &mut rows[row_idx].sub_rows;
            let mut choice_rows = std::mem::take(container);
            self.make_choice_rows(&mut choice_rows, element, &types);
            *container = choice_rows;
        }

        if this_is_slicer {
            let id = rows[row_idx].id.clone().unwrap_or_default();
            return Some((row_idx, id));
        }
        None
    }

    fn sd_type(&self) -> &str {
        self.sd
            .root
            .get("type")
            .and_then(|x| x.as_str())
            .unwrap_or("")
    }

    fn is_resource_type(&self, t: &str) -> bool {
        self.ctx
            .resolve_type(t)
            .and_then(|r| r.kind)
            .map(|k| k == "resource")
            .unwrap_or(false)
    }

    /// `extensionIsComplex` (SDR:2683): the extension SD has > 5 elements
    /// (or > 5 between the sliceName element and its next sibling for #frag).
    fn extension_is_complex(&self, url: &str) -> bool {
        if let Some((base, frag)) = url.split_once('#') {
            let Some(sd) = self.ctx.load_resource(base) else { return false };
            let elems = sd
                .get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(|e| e.as_array());
            let Some(elems) = elems else { return false };
            let mut i = None;
            for (idx, e) in elems.iter().enumerate() {
                if e.get("sliceName").and_then(|x| x.as_str()) == Some(frag) {
                    i = Some(idx);
                    break;
                }
            }
            let Some(i) = i else { return false };
            let path = elems[i].get("path").and_then(|x| x.as_str()).unwrap_or("");
            let mut j = i + 1;
            while j < elems.len()
                && elems[j].get("path").and_then(|x| x.as_str()) != Some(path)
            {
                j += 1;
            }
            j - i > 5
        } else {
            let Some(sd) = self.ctx.load_resource(url) else { return false };
            sd.get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(|e| e.as_array())
                .map(|a| a.len() > 5)
                .unwrap_or(false)
        }
    }

    /// `genElementNameCell` (SDR:1318). Returns the cell index in row.cells.
    fn gen_element_name_cell(
        &mut self,
        row: &mut Row,
        element: Ed<'a>,
        ref_: Option<String>,
        mut s_name: String,
    ) -> usize {
        let mut hint = String::new();
        if let Some(sn) = element.slice_name() {
            hint.push_str(&format!("Slice {}", sn));
        }
        if element.definition().map(|d| !d.is_empty()).unwrap_or(false) {
            if element.has_slice_name() {
                hint.push_str(": ");
            }
            hint.push_str(element.definition().unwrap_or(""));
        }
        if element.has_slicing() && slices_exist(self.all, element) {
            s_name = format!("Slices for {}", s_name);
        }
        let left = Cell::with(
            None,
            ref_,
            Some(s_name),
            if hint.is_empty() { None } else { Some(hint) },
            None,
        );
        row.cells.push(left);
        row.cells.len() - 1
    }

    /// `genElementCells` SUMMARY path (SDR:1348).
    #[allow(clippy::too_many_arguments)]
    fn gen_element_cells(
        &mut self,
        row: &mut Row,
        element: Ed<'a>,
        types: &[TypeRef<'a>],
        ext: bool,
        types_row: bool,
        root: bool,
        used: &mut UnusedTracker,
        name_cell_idx: usize,
        walks_into_this: bool,
    ) {
        // flags cell
        let mut gc = Cell::new();
        if element.is_modifier() {
            gc.add_styled_text(
                Some("This element is a modifier element".into()),
                Some("?!".into()),
                None,
                None,
                None,
                false,
            );
        }
        let has_oblig = element.has_extension(EXT_OBLIGATION_CORE)
            || element.has_extension(EXT_OBLIGATION_TOOLS);
        if element.must_support() && has_oblig {
            gc.add_styled_text(
                Some("This element has obligations and must be supported".into()),
                Some("SO".into()),
                Some("white"),
                Some(RED_BACKGROUND_COLOR),
                None,
                false,
            );
        } else if element.must_support() {
            gc.add_styled_text(
                Some("This element must be supported".into()),
                Some("S".into()),
                Some("white"),
                Some(RED_BACKGROUND_COLOR),
                None,
                false,
            );
        } else if has_oblig {
            gc.add_styled_text(
                Some("This element has obligations".into()),
                Some("O".into()),
                Some("white"),
                Some(RED_BACKGROUND_COLOR),
                None,
                false,
            );
        }
        if element.is_summary() {
            gc.add_styled_text(
                Some("This element is included in summaries".into()),
                Some("\u{03A3}".into()),
                None,
                None,
                None,
                false,
            );
        }
        if element.must_have_value() {
            gc.add_styled_text(
                Some("This primitive element must have a value".into()),
                Some("V".into()),
                Some("maroon"),
                None,
                None,
                true,
            );
        }
        if has_non_base_constraints(element) || has_non_base_conditions(element) {
            let idx = gc.add_text(CONSTRAINT_CHAR);
            let p = &mut gc.pieces[idx];
            p.set_hint(format!(
                "This element has or is affected by constraints ( {} )",
                list_constraints_and_conditions(element)
            ));
            p.add_style(CONSTRAINT_STYLE);
            // pathURL(VersionUtilities.getSpecUrl(version), "conformance-rules...")
            p.set_reference(format!("{}conformance-rules.html#constraints", self.core_path));
        }
        if element.has_extension(EXT_STANDARDS_STATUS) {
            self.gap("standards-status flag");
        }
        row.cells.push(gc);

        // extension branch (SDR:1385-1416) vs plain (SDR:1417-1424)
        if ext {
            if types.len() == 1 && !types[0].profiles().is_empty() {
                let eurl = types[0].profiles()[0].to_string();
                match self.locate_extension(&eurl) {
                    None => {
                        row.cells.push(gen_cardinality(element, used));
                        row.cells.push(Cell::with(
                            None,
                            None,
                            Some(format!("?gen-e1? {:?}", types[0].profiles())),
                            None,
                            None,
                        ));
                        let (c, prs) = self.generate_description(element, root, None, None, walks_into_this);
                        row.cells.push(c);
                        row.sub_rows.extend(prs);
                    }
                    Some(ext_defn) => {
                        // nameCell hint override (SDR:1398)
                        row.cells[name_cell_idx].pieces[0]
                            .set_hint(format!("Extension URL = {}", ext_defn.url));
                        row.cells
                            .push(gen_cardinality_fb(element, used, Some(&ext_defn)));
                        let value_defn = if walks_into_this {
                            None
                        } else {
                            self.extension_value_definition(&ext_defn)
                        };
                        match &value_defn {
                            Some(vd) if vd.max.as_deref() != Some("0") => {
                                let c = self.gen_types_for_value(vd, element);
                                row.cells.push(c);
                            }
                            _ => {
                                row.cells.push(Cell::with(
                                    None,
                                    None,
                                    Some("(Complex)".into()),
                                    None,
                                    None,
                                ));
                            }
                        }
                        let (c, prs) = self.generate_description(
                            element,
                            root,
                            Some(&ext_defn),
                            value_defn.as_ref(),
                            walks_into_this,
                        );
                        row.cells.push(c);
                        row.sub_rows.extend(prs);
                    }
                }
            } else {
                row.cells.push(gen_cardinality(element, used));
                if element.max() == Some("0") {
                    row.cells.push(Cell::new());
                } else {
                    let c = self.gen_types(element, types, root);
                    row.cells.push(c);
                }
                let (c, prs) = self.generate_description(element, root, None, None, walks_into_this);
                row.cells.push(c);
                row.sub_rows.extend(prs);
            }
        } else {
            row.cells.push(gen_cardinality(element, used));
            if element.max() != Some("0") && !types_row {
                let c = self.gen_types(element, types, root);
                row.cells.push(c);
            } else {
                row.cells.push(Cell::new());
            }
            let (c, prs) = self.generate_description(element, root, None, None, walks_into_this);
            row.cells.push(c);
            row.sub_rows.extend(prs);
        }
    }

    /// `genTypes` (SDR:2317), SUMMARY/mustSupportMode=false.
    fn gen_types(&mut self, e: Ed<'a>, types: &[TypeRef<'a>], root: bool) -> Cell {
        let mut c = Cell::new();
        if let Some(cr) = e.content_reference() {
            // (SDR:2320-2334 + getElementByName): the snapshot generator writes
            // absolute contentReferences ("http://...#Path"); a bare "#Path"
            // resolves in this profile.
            let (url, frag) = match cr.split_once('#') {
                Some((u, f)) => (u, f),
                None => ("", cr),
            };
            if url.is_empty() || url == self.sd_url() {
                c.pieces.push(Piece::ref_text(None, Some("See ".into()), None));
                c.pieces.push(Piece::ref_text(
                    Some(format!("#{}", frag)),
                    Some(tail(frag).to_string()),
                    Some(frag.to_string()),
                ));
            } else if let Some(src) = self.ctx.resolve(url) {
                let type_name = src
                    .name
                    .clone()
                    .unwrap_or_else(|| tail(url).to_string());
                c.pieces.push(Piece::ref_text(None, Some("See ".into()), None));
                c.pieces.push(Piece::ref_text(
                    Some(format!("{}#{}", src.web_path, frag)),
                    Some(format!("{} ({})", tail(frag), type_name)),
                    Some(frag.to_string()),
                ));
            } else {
                self.gap("unresolved contentReference");
            }
            return c;
        }
        if types.is_empty() {
            if root {
                // base branch (SDR:2337-2350)
                let base_url = self
                    .sd
                    .root
                    .get("baseDefinition")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                if let Some(bsd) = self.ctx.resolve(base_url) {
                    let name = bsd.name.clone().unwrap_or_default();
                    let wp = bsd.web_path.clone();
                    // isAbsoluteUrl(webPath) ? webPath : imagePath+webPath;
                    // imagePath="" so relative stays relative.
                    c.pieces.push(Piece::ref_text(Some(wp), Some(name), None));
                }
            }
            return c;
        }
        let mut first = true;
        for t in types {
            // mustSupportMode=false -> all types pass
            if first {
                first = false;
            } else {
                c.pieces.push(Piece::ref_text(None, Some(", ".into()), None));
            }
            if t.has_target() {
                // Reference/canonical (SDR:2379-2427)
                if !t.profiles().is_empty() {
                    let ref_ = t.profiles()[0];
                    if let Some(tsd) = self.ctx.resolve(ref_) {
                        // SDR:2385-2389: "(version)" when multiple versions.
                        let name = if ref_.contains('|') || self.ctx.version_count(ref_) > 1 {
                            tsd.name.clone().map(|n| format!("{}({})", n, tsd.version))
                        } else {
                            tsd.name.clone()
                        };
                        c.pieces.push(Piece::ref_text(
                            Some(tsd.web_path.clone()),
                            name,
                            Some(tsd.present()),
                        ));
                    } else {
                        c.pieces.push(Piece::ref_text(
                            Some(format!("{}references.html", self.core_path)),
                            Some(t.working_code().to_string()),
                            None,
                        ));
                    }
                } else {
                    c.pieces.push(Piece::ref_text(
                        Some(format!("{}references.html", self.core_path)),
                        Some(t.working_code().to_string()),
                        None,
                    ));
                }
                // " S" flag when isMustSupportDirect(t) && e.mustSupport
                if type_is_must_support(t) && e.must_support() {
                    c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                    c.add_styled_text(
                        Some("This type must be supported".into()),
                        Some("S".into()),
                        Some("white"),
                        Some(RED_BACKGROUND_COLOR),
                        None,
                        false,
                    );
                }
                c.pieces.push(Piece::ref_text(None, Some("(".into()), None));
                let mut tfirst = true;
                for u in t.target_profiles() {
                    if tfirst {
                        tfirst = false;
                    } else {
                        c.pieces.push(Piece::ref_text(None, Some(" | ".into()), None));
                    }
                    self.gen_target_link(&mut c, t, u);
                    if canonical_is_must_support(t, u) && e.must_support() {
                        c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                        // SDR:2414: targetProfile S also uses STRUC_DEF_TYPE_SUPP.
                        c.add_styled_text(
                            Some("This type must be supported".into()),
                            Some("S".into()),
                            Some("white"),
                            Some(RED_BACKGROUND_COLOR),
                            None,
                            false,
                        );
                    }
                }
                c.pieces.push(Piece::ref_text(None, Some(")".into()), None));
                // aggregation modes (SDR:2416-2427) — rare; gap if present
                if t.v.get("aggregation").is_some() {
                    self.gap("aggregation modes");
                }
            } else if !t.profiles().is_empty()
                && (t.working_code() != "Extension" || is_profiled_type(&t.profiles()))
            {
                // profiled type (SDR:2428-2461)
                let mut pfirst = true;
                for p in t.profiles() {
                    if pfirst {
                        pfirst = false;
                    } else {
                        c.pieces.push(Piece::ref_text(None, Some(", ".into()), None));
                    }
                    // getLinkForProfile -> webPath|name, name gains
                    // "(version)" when multiple versions of the canonical are
                    // loaded (IGKP:719-723).
                    if let Some(psd) = self.ctx.resolve(p) {
                        let name = if p.contains('|') || self.ctx.version_count(p) > 1 {
                            psd.name.clone().map(|n| format!("{}({})", n, psd.version))
                        } else {
                            psd.name.clone()
                        };
                        c.pieces.push(Piece::ref_text(
                            Some(psd.web_path.clone()),
                            name,
                            Some(t.working_code().to_string()),
                        ));
                    } else {
                        c.pieces.push(Piece::ref_text(
                            None,
                            Some(t.working_code().to_string()),
                            None,
                        ));
                    }
                    if canonical_is_must_support(t, p) && e.must_support() {
                        c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                        c.add_styled_text(
                            Some("This profile must be supported".into()),
                            Some("S".into()),
                            Some("white"),
                            Some(RED_BACKGROUND_COLOR),
                            None,
                            false,
                        );
                    }
                }
            } else {
                // plain type (SDR:2462-2501)
                let tc = t.working_code();
                if tc.starts_with("http://") || tc.starts_with("https://") {
                    if let Some(sd) = self.ctx.resolve_type(tc) {
                        // getLinkFor(corePath, tc) -> webPath; text = typeName
                        let tn = type_name_of(&sd, tc);
                        c.pieces.push(Piece::ref_text(Some(sd.web_path.clone()), Some(tn), None));
                    } else {
                        c.pieces.push(Piece::ref_text(None, Some(tc.to_string()), None));
                    }
                } else if self.ctx.has_link_for(tc) {
                    // pkp.hasLinkFor gate (IGKP:568): derivation must be
                    // specialization — base abstract types (Resource, Element)
                    // render as plain text.
                    let sd = self.ctx.resolve_type(tc).unwrap();
                    c.pieces.push(Piece::ref_text(
                        Some(sd.web_path.clone()),
                        Some(tc.to_string()),
                        None,
                    ));
                } else {
                    c.pieces.push(Piece::ref_text(None, Some(tc.to_string()), None));
                }
                if type_is_must_support(t) && e.must_support() {
                    c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                    c.add_styled_text(
                        Some("This type must be supported".into()),
                        Some("S".into()),
                        Some("white"),
                        Some(RED_BACKGROUND_COLOR),
                        None,
                        false,
                    );
                }
            }
        }
        c
    }

    /// `genTargetLink` (SDR:2529).
    fn gen_target_link(&mut self, c: &mut Cell, _t: &TypeRef<'a>, u: &str) {
        if u.starts_with("http://hl7.org/fhir/StructureDefinition/") {
            if let Some(sd) = self.ctx.resolve(u) {
                let disp = sd.title.clone().or(sd.name.clone()).unwrap_or_default();
                c.pieces
                    .push(Piece::ref_text(Some(sd.web_path.clone()), Some(disp), None));
            } else {
                let rn = &u[40..];
                let link = self.ctx.resolve_type(rn).map(|r| r.web_path);
                c.pieces.push(Piece::ref_text(link, Some(rn.to_string()), None));
            }
        } else if u.starts_with("http://") || u.starts_with("https://") {
            if let Some(sd) = self.ctx.resolve(u) {
                let disp = sd.present();
                // href = getLinkForProfile == webPath (| stripped)
                let mut href = sd.web_path.clone();
                if let Some(i) = href.find('|') {
                    href.truncate(i);
                }
                c.pieces.push(Piece::ref_text(Some(href), Some(disp), None));
            } else {
                c.pieces.push(Piece::ref_text(None, Some(u.to_string()), None));
            }
        } else if u.starts_with('#') {
            self.gap("contained target profile link");
        }
    }

    /// The value[x] cell for a simple extension (SDR:1402): genTypes on the
    /// extension's value definition.
    fn gen_types_for_value(&mut self, vd: &ValueDefn, e: Ed<'a>) -> Cell {
        // Build a synthetic Ed over the stored JSON.
        let ed = Ed::new(&vd.json);
        let types = ed.types();
        self.gen_types_inner_for_ext(ed, &types, e)
    }

    fn gen_types_inner_for_ext(&mut self, ed: Ed<'_>, types: &[TypeRef<'_>], outer: Ed<'a>) -> Cell {
        // genTypes(gen, row, valueDefn, ...) with root=false, ms=false. The
        // mustSupport "S" flags check the VALUE DEFN's types but the OUTER
        // element's mustSupport; Java passes the value defn as `e` so
        // e.getMustSupport() is the value defn's (usually absent). Reproduce
        // Java exactly: use the value defn for both.
        let _ = outer;
        // Reuse gen_types via a temporary TCtx borrow — duplicated logic kept
        // in gen_types; call it with root=false.
        // SAFETY: gen_types only uses self + args.
        // (We simply transmute lifetimes by re-borrowing the JSON value.)
        // Simplest: inline call.
        let e2: Ed<'_> = ed;
        // gen_types expects Ed<'a>; the JSON lives in ValueDefn which outlives
        // the call. We do a raw call through a helper that doesn't capture.
        self.gen_types_erased(e2, types)
    }

    fn gen_types_erased(&mut self, e: Ed<'_>, types: &[TypeRef<'_>]) -> Cell {
        // A lifetime-erased duplicate of gen_types' body for non-'a elements.
        // To avoid divergence, we only support the common cases the extension
        // value path hits (plain types, references, profiled datatypes).
        let mut c = Cell::new();
        let mut first = true;
        for t in types {
            if first {
                first = false;
            } else {
                c.pieces.push(Piece::ref_text(None, Some(", ".into()), None));
            }
            if t.has_target() {
                c.pieces.push(Piece::ref_text(
                    Some(format!("{}references.html", self.core_path)),
                    Some(t.working_code().to_string()),
                    None,
                ));
                c.pieces.push(Piece::ref_text(None, Some("(".into()), None));
                let mut tfirst = true;
                for u in t.target_profiles() {
                    if tfirst {
                        tfirst = false;
                    } else {
                        c.pieces.push(Piece::ref_text(None, Some(" | ".into()), None));
                    }
                    if u.starts_with("http://hl7.org/fhir/StructureDefinition/") {
                        if let Some(sd) = self.ctx.resolve(u) {
                            let disp = sd.title.clone().or(sd.name.clone()).unwrap_or_default();
                            c.pieces.push(Piece::ref_text(
                                Some(sd.web_path.clone()),
                                Some(disp),
                                None,
                            ));
                            continue;
                        }
                    }
                    if let Some(sd) = self.ctx.resolve(u) {
                        c.pieces.push(Piece::ref_text(
                            Some(sd.web_path.clone()),
                            Some(sd.present()),
                            None,
                        ));
                    } else {
                        c.pieces.push(Piece::ref_text(None, Some(u.to_string()), None));
                    }
                }
                if t.target_profiles().is_empty() {
                    // Java: `Any` only in makeChoiceRows; genTypes prints ()
                }
                c.pieces.push(Piece::ref_text(None, Some(")".into()), None));
            } else {
                let tc = t.working_code();
                if let Some(sd) = self.ctx.resolve_type(tc) {
                    c.pieces.push(Piece::ref_text(
                        Some(sd.web_path.clone()),
                        Some(tc.to_string()),
                        None,
                    ));
                } else {
                    c.pieces.push(Piece::ref_text(None, Some(tc.to_string()), None));
                }
            }
        }
        c
    }

    /// `makeChoiceRows` (SDR:3362), mustSupportMode=false.
    fn make_choice_rows(&mut self, sub_rows: &mut Vec<Row>, element: Ed<'a>, types: &[TypeRef<'a>]) {
        for tr in types {
            let mut used = false;
            let mut choicerow = Row::new();
            choicerow.set_id(element.path().to_string());
            let mut ts = tr.working_code().to_string();
            let tu = tr.working_code().to_string();
            if ts.starts_with("http://") || ts.starts_with("https://") {
                if let Some(sd) = self.ctx.resolve(&ts) {
                    if let Some(t) = sd.rtype.eq("StructureDefinition").then(|| sd.clone()) {
                        // sd.getType() — we need the type field of the SD; load
                        if let Some(full) = self.ctx.load_resource(&ts) {
                            if let Some(ty) = full.get("type").and_then(|x| x.as_str()) {
                                ts = ty.to_string();
                            }
                        }
                        let _ = t;
                    }
                }
            }
            if ts.starts_with("http://") || ts.starts_with("https://") {
                ts = ts.rsplit('/').next().unwrap_or(&ts).to_string();
            }
            let name = tail(element.path()).replace("[x]", &capitalize(&ts));
            if tu == "Reference" || tu == "canonical" {
                used = true;
                choicerow.cells.push(Cell::with(None, None, Some(name), None, None));
                choicerow.cells.push(Cell::new());
                choicerow
                    .cells
                    .push(Cell::with(None, None, Some("".into()), None, None));
                choicerow.set_icon("icon_reference.png", Some("Reference to another Resource".into()));
                let mut c = Cell::new();
                // ADD_REFERENCE_TO_TABLE = true (constant in SDR)
                if tu == "canonical" {
                    c.pieces.push(Piece::ref_text(
                        Some(format!("{}datatypes.html#canonical", self.core_path)),
                        Some("canonical".into()),
                        None,
                    ));
                } else {
                    c.pieces.push(Piece::ref_text(
                        Some(format!("{}references.html#Reference", self.core_path)),
                        Some("Reference".into()),
                        None,
                    ));
                }
                c.pieces.push(Piece::ref_text(None, Some("(".into()), None));
                let mut first = true;
                for rt in tr.target_profiles() {
                    if !first {
                        c.pieces.push(Piece::ref_text(None, Some(" | ".into()), None));
                    }
                    self.gen_target_link(&mut c, tr, rt);
                    first = false;
                }
                if first {
                    c.pieces.push(Piece::ref_text(None, Some("Any".into()), None));
                }
                c.pieces.push(Piece::ref_text(None, Some(")".into()), None));
                choicerow.cells.push(c);
            } else {
                let sd = self.ctx.resolve_type(&tu);
                match sd {
                    Some(sd) if sd.kind.as_deref() == Some("primitive-type") => {
                        used = true;
                        let desc = self.type_description(&tu);
                        choicerow
                            .cells
                            .push(Cell::with(None, None, Some(name), desc, None));
                        choicerow.cells.push(Cell::new());
                        choicerow
                            .cells
                            .push(Cell::with(None, None, Some("".into()), None, None));
                        choicerow.set_icon("icon_primitive.png", Some("Primitive Data Type".into()));
                        let mut c = Cell::with(
                            None,
                            Some(format!("{}datatypes.html#{}", self.core_path, tu)),
                            Some(type_name_of(&sd, &tu)),
                            None,
                            None,
                        );
                        // SDR:3435-3438: " S" when the type is must-support.
                        if type_is_must_support(tr) && element.must_support() {
                            c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                            c.add_styled_text(
                                Some("This target must be supported".into()),
                                Some("S".into()),
                                Some("white"),
                                Some(RED_BACKGROUND_COLOR),
                                None,
                                false,
                            );
                        }
                        choicerow.cells.push(c);
                    }
                    Some(sd) => {
                        used = true;
                        let desc = self.type_description(&tu);
                        choicerow
                            .cells
                            .push(Cell::with(None, None, Some(name), desc, None));
                        choicerow.cells.push(Cell::new());
                        choicerow
                            .cells
                            .push(Cell::with(None, None, Some("".into()), None, None));
                        choicerow.set_icon("icon_datatype.gif", Some("Data Type".into()));
                        let mut c = Cell::with(
                            None,
                            Some(sd.web_path.clone()),
                            Some(type_name_of(&sd, &tu)),
                            None,
                            None,
                        );
                        // SDR:3447-3450: " S" when the type is must-support.
                        if type_is_must_support(tr) && element.must_support() {
                            c.pieces.push(Piece::ref_text(None, Some(" ".into()), None));
                            c.add_styled_text(
                                Some("This type must be supported".into()),
                                Some("S".into()),
                                Some("white"),
                                Some(RED_BACKGROUND_COLOR),
                                None,
                                false,
                            );
                        }
                        choicerow.cells.push(c);
                    }
                    None => {}
                }
                if !tr.profiles().is_empty() && used {
                    let type_cell = choicerow.cells.last_mut().unwrap();
                    type_cell.pieces.push(Piece::ref_text(None, Some("(".into()), None));
                    let mut first = true;
                    for pt in tr.profiles() {
                        if first {
                            first = false;
                        } else {
                            type_cell
                                .pieces
                                .push(Piece::ref_text(None, Some(" | ".into()), None));
                        }
                        if let Some(psd) = self.ctx.resolve(pt) {
                            type_cell.pieces.push(Piece::ref_text(
                                Some(psd.web_path.clone()),
                                psd.name.clone(),
                                Some(psd.present()),
                            ));
                        } else {
                            type_cell
                                .pieces
                                .push(Piece::ref_text(None, Some("?gen-e2?".into()), None));
                        }
                    }
                    type_cell.pieces.push(Piece::ref_text(None, Some(")".into()), None));
                }
            }
            if used {
                choicerow.cells.push(Cell::new());
                sub_rows.push(choicerow);
            }
        }
    }

    /// SD.description for a type (used as choice-row name hint).
    fn type_description(&self, code: &str) -> Option<String> {
        let url = format!("http://hl7.org/fhir/StructureDefinition/{}", code);
        let full = self.ctx.load_resource(&url)?;
        full.get("description")
            .and_then(|x| x.as_str())
            .map(String::from)
    }

    /// `locateExtension` (SDR:2659).
    fn locate_extension(&mut self, url: &str) -> Option<ExtDefn> {
        if let Some((base, frag)) = url.split_once('#') {
            let sd = self.ctx.load_resource(base)?;
            let elems = sd
                .get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(|e| e.as_array())?;
            let el = elems
                .iter()
                .find(|e| e.get("sliceName").and_then(|x| x.as_str()) == Some(frag))?
                .clone();
            Some(ExtDefn {
                url: sd.get("url").and_then(|x| x.as_str()).unwrap_or(base).to_string(),
                sd: sd.clone(),
                element: el,
            })
        } else {
            let sd = self.ctx.load_resource(url)?;
            let el = sd
                .get("snapshot")
                .and_then(|s| s.get("element"))
                .and_then(|e| e.as_array())
                .and_then(|a| a.first())?
                .clone();
            Some(ExtDefn {
                url: sd.get("url").and_then(|x| x.as_str()).unwrap_or(url).to_string(),
                sd: sd.clone(),
                element: el,
            })
        }
    }

    /// `ExtensionContext.getExtensionValueDefinition` — the `Extension.value[x]`
    /// element right after the ext element.
    fn extension_value_definition(&self, ext: &ExtDefn) -> Option<ValueDefn> {
        let elems = ext
            .sd
            .get("snapshot")
            .and_then(|s| s.get("element"))
            .and_then(|e| e.as_array())?;
        let epath = ext.element.get("path").and_then(|x| x.as_str())?;
        let mut idx = elems
            .iter()
            .position(|e| std::ptr::eq(e, &ext.element) || e == &ext.element)?;
        idx += 1;
        while idx < elems.len() {
            let p = elems[idx].get("path").and_then(|x| x.as_str()).unwrap_or("");
            if !p.starts_with(&format!("{}.", epath)) {
                break;
            }
            if p == format!("{}.value[x]", epath) || (p.starts_with(&format!("{}.value", epath))) {
                return Some(ValueDefn {
                    json: elems[idx].clone(),
                    max: elems[idx].get("max").and_then(|x| x.as_str()).map(String::from),
                });
            }
            idx += 1;
        }
        None
    }

    /// `generateDescription` (SDR:1541), SUMMARY mode. `ext_defn` set for the
    /// simple-extension case (SDR:1406 — fallback=ext element, url=ext url);
    /// None for the plain case (SDR:1423).
    fn generate_description(
        &mut self,
        definition: Ed<'a>,
        root: bool,
        ext_defn: Option<&ExtDefn>,
        _value_defn: Option<&ValueDefn>,
        allow_sub_rows_hint: bool,
    ) -> (Cell, Vec<Row>) {
        let _ = allow_sub_rows_hint;
        let mut partner_rows: Vec<Row> = Vec::new();
        let mut c = Cell::new();
        let url: Option<String> = ext_defn.map(|e| e.url.clone());

        // root abstract profile block (SDR:1555-1577)
        if root
            && self
                .sd
                .root
                .get("abstract")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
        {
            self.gap("abstract root description block");
        }

        // url-fixed short circuit (SDR:1579)
        if definition.path().ends_with("url") && definition.fixed().is_some() {
            let (_, v) = definition.fixed().unwrap();
            let mut p =
                Piece::ref_text(None, Some(format!("\"{}\"", build_json(v))), None);
            p.add_style("color: darkgreen");
            c.pieces.push(p);
            return (c, partner_rows);
        }

        // short (SDR:1582)
        if let Some(short) = definition.short() {
            if !short.is_empty() {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                c.pieces.push(Piece::ref_text(None, Some(short.to_string()), None));
            }
        }
        // URL line for extensions (SDR:1601-1639)
        if let Some(url) = &url {
            let full_url = url.clone();
            let ref_ = self.ctx.resolve(url).map(|r| r.web_path);
            // getFixedUrl profiled-extension handling: only when the extension
            // element is a sub-extension with fixed url differing — gap for now.
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("URL: ".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces.push(Piece::ref_text(ref_, Some(full_url), None));
        }

        // slicing (SDR:1692)
        if definition.has_slicing() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("Slice: ".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces.push(Piece::ref_text(
                None,
                Some(describe_slice(definition.slicing().unwrap())),
                None,
            ));
        }

        // Narrative special text (SDR@6.9.11:1745-1796)
        if definition
            .types()
            .first()
            .map(|t| t.working_code() == "Narrative")
            .unwrap_or(false)
            && definition.types().len() == 1
        {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let status_codes = self.determine_narrative_status(definition);
            // langCtrl/source-control extensions: none in the corpus; if
            // present they are a flagged gap below.
            let lang_ctrl_present = definition
                .has_extension("http://hl7.org/fhir/StructureDefinition/narrative-language-control");
            let level_present = definition
                .has_extension("http://hl7.org/fhir/StructureDefinition/narrative-source-control");
            if lang_ctrl_present || level_present {
                self.gap("narrative language/source control extensions");
            }
            let unconstrained = status_codes.is_empty() || status_codes.len() == 4;
            if unconstrained && !level_present && !lang_ctrl_present {
                let mut p = Piece::ref_text(
                    None,
                    Some("This profile does not constrain the narrative in regard to content, language, or traceability to data elements".into()),
                    None,
                );
                p.add_style("font-weight:bold");
                c.pieces.push(p);
            } else {
                let mut p = Piece::ref_text(
                    None,
                    Some(if unconstrained {
                        "This profile does not constrain the narrative content by fixing the status codes".to_string()
                    } else {
                        format!(
                            "This profile constrains the narrative content by fixing the status codes to {}",
                            join2(", ", " and ", &status_codes)
                        )
                    }),
                    None,
                );
                p.add_style("font-weight:bold");
                c.pieces.push(p);
                c.pieces.push(Piece::tag("br"));
                let mut p = Piece::ref_text(
                    None,
                    Some("This profile does not constrain the narrative in regard to language specific sections".into()),
                    None,
                );
                p.add_style("font-weight:bold");
                c.pieces.push(p);
                c.pieces.push(Piece::tag("br"));
                let mut p = Piece::ref_text(
                    None,
                    Some("This profile does not constrain the narrative in regard to traceability to data elements".into()),
                    None,
                );
                p.add_style("font-weight:bold");
                c.pieces.push(p);
            }
        }

        // binding (SDR@6.9.11:1975-2027): the VALUE DEFN's binding wins for
        // simple extensions (SDR:1980-1983).
        let binding_owner: Option<&serde_json::Value> = match _value_defn {
            Some(vd) => {
                let b = vd.json.get("binding");
                if b.map(|x| x.as_object().map(|o| !o.is_empty()).unwrap_or(false)).unwrap_or(false) {
                    b
                } else {
                    definition.binding()
                }
            }
            None => definition.binding(),
        };
        if let Some(binding) = binding_owner {
            if binding.get("valueSet").is_some() {
                self.render_binding_summary(&mut c, definition, binding);
            } else if binding.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                self.gap("binding without valueSet");
            }
        }

        // constraints (SDR:2029-2043)
        let mut first_constraint = true;
        for inv in definition.constraints() {
            let source = inv.v.get("source").and_then(|x| x.as_str());
            let show = match source {
                None => true,
                Some(src) => {
                    src == self.sd_url()
                        || (self.cfg.all_invariants
                            && !self.is_abstract_base_profile(src)
                            && src != "http://hl7.org/fhir/StructureDefinition/Extension"
                            && src != "http://hl7.org/fhir/StructureDefinition/Element")
                }
            };
            if show {
                if first_constraint {
                    if !c.pieces.is_empty() {
                        let mut br = Piece::tag("br");
                        br.set_class("constraint");
                        c.pieces.push(br);
                    }
                    let mut lbl = Piece::ref_text(None, Some("Constraints: ".into()), None);
                    lbl.set_class("constraint");
                    c.pieces.push(lbl);
                    first_constraint = false;
                } else {
                    let mut sep = Piece::ref_text(None, Some(", ".into()), None);
                    sep.set_class("constraint");
                    c.pieces.push(sep);
                }
                let mut p = Piece::ref_text(
                    None,
                    Some(inv.key().to_string()),
                    Some(inv.human().to_string()),
                );
                p.set_class("constraint");
                p.add_style("font-weight:bold");
                c.pieces.push(p);
            }
        }

        // repeating-element order (SDR:2044-2052) — the dangling-br quirk.
        let base_max_star = definition.base_max() == Some("*");
        let max_star = definition.max() == Some("*");
        if base_max_star || max_star {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            if let Some(om) = definition.order_meaning() {
                c.pieces.push(Piece::ref_text(
                    None,
                    Some(format!("This repeating element order: {}", om)),
                    None,
                ));
            }
        }

        // fixed (SDR:2053-2072)
        if let Some((_, v)) = definition.fixed() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("Fixed Value: ".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            if is_primitive_value(v) {
                // link = pkp.getLinkForUrl(corePath, s) — ContextUtilities
                // .getLinkForUrl gates on hasResource(CanonicalResource.class,
                // url) which never matches (abstract class fetch), so fixed
                // values are NEVER linked (empirically 193/193 unlinked spans
                // across the us-core golden snapshots).
                let mut val = Piece::ref_text(None, Some(build_json(v)), None);
                val.add_style("color: darkgreen");
                c.pieces.push(val);
            } else {
                let mut val = Piece::ref_text(None, Some("As shown".into()), None);
                val.add_style("color: darkgreen");
                c.pieces.push(val);
                let (ty, _) = definition.fixed().unwrap();
                self.gen_fixed_value(&mut partner_rows, ty, v, false, false, None, None);
            }
        } else if let Some((_, v)) = definition.pattern() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("Required Pattern: ".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            if is_primitive_value(v) {
                let mut val = Piece::ref_text(None, Some(build_json(v)), None);
                val.add_style("color: darkgreen");
                c.pieces.push(val);
            } else {
                let mut val = Piece::ref_text(None, Some("At least the following".into()), None);
                val.add_style("color: darkgreen");
                c.pieces.push(val);
                let (ty, _) = definition.pattern().unwrap();
                self.gen_fixed_value(
                    &mut partner_rows,
                    ty,
                    v,
                    true,
                    false,
                    Some(definition.path().to_string()),
                    Some(definition.id().to_string()),
                );
            }
        } else if let Some(merged) = self
            .merged_pattern_values
            .get(&self.element_index(definition))
            .cloned()
            .filter(|m| !m.is_empty())
        {
            // hasMergedPatternValues (SDR:2087-2110)
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("Fixed Value: ".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            let mut complex_values: Vec<(String, serde_json::Value)> = Vec::new();
            let mut first = true;
            for b in &merged {
                if !first {
                    c.pieces.push(Piece::ref_text(None, Some(", ".into()), None));
                }
                let s = if is_primitive_value(b) {
                    build_json(b)
                } else {
                    "(Complex)".to_string()
                };
                let mut val = Piece::ref_text(None, Some(s), None);
                val.add_style("color: darkgreen");
                c.pieces.push(val);
                if !is_primitive_value(b) {
                    // fhirType of the merged value: derived from the property
                    // type recorded at merge time (stored alongside).
                    complex_values.push(("".into(), b.clone()));
                }
                first = false;
            }
            if !complex_values.is_empty() {
                self.gap("merged complex pattern values partner rows");
            }
        } else {
            // example (SDR:2108)
            for ex in definition.example() {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                let label = ex.get("label").and_then(|x| x.as_str()).unwrap_or("");
                let mut lbl = Piece::ref_text(
                    None,
                    Some(format!("Example {}: ", label)),
                    Some("".into()),
                );
                lbl.add_style("font-weight:bold");
                c.pieces.push(lbl);
                let value = ex
                    .as_object()
                    .and_then(|o| o.iter().find(|(k, _)| k.starts_with("value")))
                    .map(|(_, v)| v);
                if let Some(v) = value {
                    let mut val = Piece::ref_text(None, Some(build_json(v)), None);
                    val.add_style("color: darkgreen");
                    c.pieces.push(val);
                }
            }
        }

        // obligations (SDR:2118) — C5, gap when present
        if definition.has_extension(EXT_OBLIGATION_CORE)
            || definition.has_extension(EXT_OBLIGATION_TOOLS)
            || (root
                && (self.sd.root.get("extension").is_some()
                    && sd_has_obligations(&self.sd.root)))
        {
            self.gap("obligations table");
        }

        // maxLength (SDR:2126)
        if let Some(ml) = definition.max_length() {
            if ml != 0 {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                let mut lbl = Piece::ref_text(None, Some("Max Length:".into()), None);
                lbl.add_style("font-weight:bold");
                c.pieces.push(lbl);
                let mut val = Piece::ref_text(None, Some(ml.to_string()), None);
                val.add_style("color: darkgreen");
                c.pieces.push(val);
            }
        }
        let _ = ext_defn;
        (c, partner_rows)
    }

    fn element_index(&self, e: Ed<'a>) -> usize {
        self.all
            .iter()
            .position(|x| std::ptr::eq(x.v, e.v))
            .unwrap_or(usize::MAX)
    }

    /// `genFixedValue` (SDR@6.9.11:2760-2901). Appends partner rows to `rows`
    /// (the element row's sub_rows). `pattern`: Required Pattern vs Fixed.
    #[allow(clippy::too_many_arguments)]
    fn gen_fixed_value(
        &mut self,
        rows: &mut Vec<Row>,
        type_code: &str,
        value: &serde_json::Value,
        pattern: bool,
        skip_no_value: bool,
        path: Option<String>,
        id: Option<String>,
    ) {
        // ref = getLinkFor(corePath, fhirType) with .html -> -definitions.html#
        let type_link = self
            .ctx
            .resolve_type(type_code)
            .map(|r| r.web_path)
            .unwrap_or_else(|| format!("{}.html", type_code));
        let ref_ = if type_link.contains(".html") {
            format!("{}-definitions.html#", &type_link[..type_link.find(".html").unwrap()])
        } else {
            "?gen-fv?".to_string()
        };
        let type_url = format!("http://hl7.org/fhir/StructureDefinition/{}", type_code);
        let Some(type_sd) = self.ctx.load_resource(&type_url) else { return };
        let props = type_properties(&type_sd);
        for prop in &props {
            let child_path = path.as_ref().map(|p| format!("{}.{}", p, prop.name));
            let child_id = id.as_ref().map(|i| format!("{}.{}", i, prop.name));
            // instance values for this property
            let raw = value.get(&prop.name);
            let values: Vec<&serde_json::Value> = match raw {
                Some(serde_json::Value::Array(a)) => a.iter().collect(),
                Some(v) => vec![v],
                None => Vec::new(),
            };
            if pattern && child_path.is_some() {
                let cp = child_path.as_deref().unwrap();
                let cid = child_id.as_deref();
                // NB: the in-scope skip applies even when the property has NO
                // values (SDR:2773-2777 has no values-size gate; the merge
                // itself no-ops on empties but the `continue` still fires).
                if self.has_path_in_scope(cp, cid) {
                    self.merge_pattern_values(cp, cid, &values, prop);
                    continue;
                } else if self.has_descendant_path_in_scope(cp, cid) {
                    for v in &values {
                        if !is_primitive_value(v) {
                            // recurse with the property's (single) complex type
                            let tc = prop.type_codes.first().cloned().unwrap_or_default();
                            self.gen_fixed_value(
                                rows,
                                &tc,
                                v,
                                true,
                                skip_no_value,
                                child_path.clone(),
                                child_id.clone(),
                            );
                        }
                    }
                    continue;
                }
            }
            // snapshot=true in our path, so empty properties render too.
            if values.is_empty() {
                if !skip_no_value {
                    let mut row = Row::new();
                    row.set_id(prop.path.clone());
                    let mut name_cell = Cell::new();
                    let href = if prop.base_path == prop.path {
                        format!("{}{}", ref_, prop.path)
                    } else {
                        format!("{}element-definitions.html#{}", self.core_path, prop.base_path)
                    };
                    name_cell
                        .pieces
                        .push(Piece::ref_text(Some(href), Some(prop.name.clone()), None));
                    row.cells.push(name_cell);
                    let mut flags = Cell::new();
                    flags.pieces.push(Piece::ref_text(None, None, None));
                    row.cells.push(flags);
                    let mut card = Cell::new();
                    if !pattern {
                        card.pieces.push(Piece::ref_text(None, Some("0..0".into()), None));
                        row.set_icon("icon_fixed.gif", Some("Fixed Value:".into()));
                    } else if self.ctx.is_primitive_type(&prop.type_codes.first().cloned().unwrap_or_default()) {
                        row.set_icon("icon_primitive.png", Some("Primitive Data Type".into()));
                        card.pieces.push(Piece::ref_text(None, Some(format!("0..{}", prop.max)), None));
                    } else if matches!(prop.type_codes.first().map(String::as_str), Some("Reference") | Some("canonical")) {
                        row.set_icon("icon_reference.png", Some("Reference to another Resource".into()));
                        card.pieces.push(Piece::ref_text(None, Some(format!("0..{}", prop.max)), None));
                    } else {
                        row.set_icon("icon_datatype.gif", Some("Data Type".into()));
                        card.pieces.push(Piece::ref_text(None, Some(format!("0..{}", prop.max)), None));
                    }
                    row.cells.push(card);
                    let mut ty = Cell::new();
                    let tc0 = prop.type_codes.first().cloned().unwrap_or_default();
                    let tlink = self.ctx.resolve_type(&tc0).map(|r| r.web_path);
                    ty.pieces.push(Piece::ref_text(tlink, Some(tc0.clone()), None));
                    row.cells.push(ty);
                    let mut desc = Cell::new();
                    desc.pieces
                        .push(Piece::ref_text(None, Some(prop.short.clone()), None));
                    row.cells.push(desc);
                    rows.push(row);
                }
            } else {
                for b in &values {
                    let mut row = Row::new();
                    row.set_id(prop.path.clone());
                    row.set_icon("icon_fixed.gif", Some("Fixed Value".into()));
                    let mut name_cell = Cell::new();
                    let href = if prop.base_path == prop.path {
                        format!("{}{}", ref_, prop.path)
                    } else {
                        format!("{}element-definitions.html#{}", self.core_path, prop.base_path)
                    };
                    name_cell
                        .pieces
                        .push(Piece::ref_text(Some(href), Some(prop.name.clone()), None));
                    row.cells.push(name_cell);
                    let mut flags = Cell::new();
                    flags.pieces.push(Piece::ref_text(None, None, None));
                    row.cells.push(flags);
                    let mut card = Cell::new();
                    card.pieces.push(Piece::ref_text(
                        None,
                        Some(if pattern {
                            format!("1..{}", prop.max)
                        } else {
                            "1..1".to_string()
                        }),
                        None,
                    ));
                    row.cells.push(card);
                    let mut ty = Cell::new();
                    // b.fhirType() = the property's concrete type
                    let tc0 = prop.type_codes.first().cloned().unwrap_or_default();
                    let tlink = self.ctx.resolve_type(&tc0).map(|r| r.web_path);
                    ty.pieces.push(Piece::ref_text(tlink, Some(tc0.clone()), None));
                    row.cells.push(ty);
                    let mut desc = Cell::new();
                    desc.pieces
                        .push(Piece::ref_text(None, Some(prop.short.clone()), None));
                    desc.pieces.push(Piece::tag("br"));
                    if is_primitive_value(b) {
                        let mut lbl =
                            Piece::ref_text(None, Some("Fixed Value: ".into()), None);
                        lbl.add_style("font-weight: bold");
                        desc.pieces.push(lbl);
                        let sv = build_json(b);
                        let mut val = Piece::ref_text(None, Some(sv), None);
                        val.add_style("color: darkgreen");
                        desc.pieces.push(val);
                        row.cells.push(desc);
                        rows.push(row);
                    } else {
                        let mut lbl =
                            Piece::ref_text(None, Some("Fixed Value: ".into()), None);
                        lbl.add_style("font-weight: bold");
                        desc.pieces.push(lbl);
                        let mut val = Piece::ref_text(None, Some("(Complex)".into()), None);
                        val.add_style("color: darkgreen");
                        desc.pieces.push(val);
                        row.cells.push(desc);
                        let mut sub = Vec::new();
                        let tc0 = prop.type_codes.first().cloned().unwrap_or_default();
                        self.gen_fixed_value(
                            &mut sub,
                            &tc0,
                            b,
                            pattern,
                            skip_no_value,
                            child_path.clone(),
                            child_id.clone(),
                        );
                        row.sub_rows = sub;
                        rows.push(row);
                    }
                }
            }
        }
    }

    fn has_path_in_scope(&self, path: &str, id: Option<&str>) -> bool {
        self.all.iter().any(|ed| matches_in_scope_element(path, id, *ed))
    }

    fn has_descendant_path_in_scope(&self, path: &str, id: Option<&str>) -> bool {
        self.all.iter().any(|ed| {
            let cand = ed.path();
            if cand.len() > path.len() && cand.starts_with(path) && cand[path.len()..].starts_with('.') {
                match id {
                    None => true,
                    Some(i) if !i.contains(':') => true,
                    Some(i) => ed.id().starts_with(&format!("{}.", i)),
                }
            } else {
                false
            }
        })
    }

    fn merge_pattern_values(
        &mut self,
        path: &str,
        id: Option<&str>,
        values: &[&serde_json::Value],
        _prop: &PropDef,
    ) {
        for (idx, ed) in self.all.iter().enumerate() {
            if !matches_in_scope_element(path, id, *ed) {
                continue;
            }
            let merged = self.merged_pattern_values.entry(idx).or_default();
            for v in values {
                let renderable = if is_primitive_value(v) {
                    !build_json(v).is_empty()
                } else {
                    v.as_object().map(|o| !o.is_empty()).unwrap_or(false)
                };
                if renderable && !merged.contains(v) {
                    merged.push((*v).clone());
                }
            }
        }
    }

    /// The SUMMARY binding block (SDR:2001-2027, fork spec §7).
    fn render_binding_summary(
        &mut self,
        c: &mut Cell,
        definition: Ed<'a>,
        binding: &serde_json::Value,
    ) {
        if !c.pieces.is_empty() {
            let mut br = Piece::tag("br");
            br.set_class("binding");
            c.pieces.push(br);
        }
        let mut lbl = Piece::ref_text(None, Some("Binding: ".into()), None);
        lbl.set_class("binding");
        lbl.add_style("font-weight:bold");
        c.pieces.push(lbl);

        let vs_ref = binding.get("valueSet").and_then(|x| x.as_str()).unwrap_or("");
        let br = self.resolve_binding(vs_ref);
        let mut p = Piece::ref_text(br.url.clone(), Some(br.display.clone()), br.uri.clone());
        p.set_class("binding");
        if br.external {
            p.set_tag_img("external.png");
        }
        c.pieces.push(p);

        if let Some(strength) = binding.get("strength").and_then(|x| x.as_str()) {
            let mut p1 = Piece::ref_text(None, Some(" (".into()), None);
            p1.set_class("binding");
            c.pieces.push(p1);
            let mut p2 = Piece::ref_text(
                Some(format!("{}terminologies.html#{}", self.core_path, strength)),
                Some(strength.to_string()),
                Some(strength_definition(strength).to_string()),
            );
            p2.set_class("binding");
            c.pieces.push(p2);
            let mut p3 = Piece::ref_text(None, Some(")".into()), None);
            p3.set_class("binding");
            c.pieces.push(p3);
        }
        if let Some(desc) = binding.get("description").and_then(|x| x.as_str()) {
            if is_simple_markdown(desc) {
                let mut p = Piece::ref_text(None, Some(": ".into()), None);
                p.set_class("binding");
                c.pieces.push(p);
                markdown::add_markdown_no_para_role(c, desc, "binding");
            }
        }
        // additional bindings (SDR:2015-2026 + AdditionalBindingsRenderer):
        // rows from binding.additional / the tools additional-binding extension
        // (converted R4), then maxValueSet, then minValueSet.
        let details = collect_additional_bindings(binding);
        if !details.is_empty() {
            let trs = self.render_additional_bindings_table(&details);
            let mut p = Piece::tag("table");
            p.set_class("binding");
            p.set_class("grid");
            for tr in trs {
                p.add_html(tr);
            }
            c.pieces.push(p);
        }
    }

    /// AdditionalBindingsRenderer.render (ABR:223-325), fullDoco=false.
    fn render_additional_bindings_table(
        &mut self,
        details: &[AddBindingDetail],
    ) -> Vec<render_xhtml::XhtmlNode> {
        use render_tables::build::Elem;
        let doco = details.iter().any(|d| d.doco_short.is_some());
        let usage = details.iter().any(|d| d.has_usage);
        let any = details.iter().any(|d| d.any);

        let mut rows_out: Vec<render_xhtml::XhtmlNode> = Vec::new();
        // header (ABR:233-245)
        let mut tr = Elem::new("tr");
        let mut td = Elem::new("td");
        td.style("font-size: 11px");
        let mut b = Elem::new("b");
        b.tx("Additional Bindings");
        td.push_elem(b);
        tr.push_elem(td);
        let mut td = Elem::new("td");
        td.style("font-size: 11px");
        td.tx("Purpose");
        tr.push_elem(td);
        if usage {
            let mut td = Elem::new("td");
            td.style("font-size: 11px");
            td.tx("Usage");
            tr.push_elem(td);
        }
        if any {
            let mut td = Elem::new("td");
            td.style("font-size: 11px");
            td.tx("Any");
            tr.push_elem(td);
        }
        if doco {
            let mut td = Elem::new("td");
            td.style("font-size: 11px");
            td.tx("Documentation");
            tr.push_elem(td);
        }
        rows_out.push(tr.build());

        for d in details {
            let mut tr = Elem::new("tr");
            // VS cell (ABR:259-271)
            let mut td = Elem::new("td");
            td.style("font-size: 11px");
            let br = self.resolve_binding(&d.value_set);
            match &br.url {
                Some(url) => {
                    let mut a = Elem::new("a");
                    a.set_attr("href", url.clone());
                    if let Some(uri) = &br.uri {
                        a.set_attr("title", uri.clone());
                    }
                    a.tx(br.display.clone());
                    if br.external {
                        a.tx(" ");
                        let mut img = Elem::new("img");
                        img.set_attr("src", "external.png");
                        img.set_attr("alt", ".");
                        a.push_elem(img);
                    }
                    td.push_elem(a);
                }
                None => {
                    let mut sp = Elem::new("span");
                    sp.set_attr("title", d.value_set.clone());
                    sp.tx(br.display.clone());
                    td.push_elem(sp);
                }
            }
            tr.push_elem(td);
            // Purpose cell (ABR:282-290, renderPurpose ABR:375-424, r5=false)
            let mut td = Elem::new("td");
            td.style("font-size: 11px");
            let cp = self.core_path;
            let mut link =
                |href: String, title: &str, text: &str, td: &mut Elem| {
                    let mut a = Elem::new("a");
                    a.set_attr("href", href);
                    a.set_attr("title", title);
                    a.tx(text);
                    td.push_elem(a);
                };
            match d.purpose.as_str() {
                "maximum" => link(
                    format!("{}extension-elementdefinition-maxvalueset.html", cp),
                    "A required binding, for use when the binding strength is 'extensible' or 'preferred'",
                    "Max Binding",
                    &mut td,
                ),
                "minimum" => link(
                    format!("{}extension-elementdefinition-minvalueset.html", cp),
                    "The minimum allowable value set - any conformant system SHALL support all these codes",
                    "Min Binding",
                    &mut td,
                ),
                "required" => link(
                    format!("{}terminologies.html#strength", cp),
                    "Validators will check this binding (strength = required)",
                    "Required",
                    &mut td,
                ),
                "extensible" => link(
                    format!("{}terminologies.html#strength", cp),
                    "Validators will check this binding (strength = extensible)",
                    "Extensible",
                    &mut td,
                ),
                "preferred" => link(
                    format!("{}terminologies.html#strength", cp),
                    "This is the value set that is recommended (documentation should explain why)",
                    "Preferred",
                    &mut td,
                ),
                other => {
                    let (title, text) = match other {
                        "current" => ("New records are required to use this value set, but legacy records may use other codes", "Current"),
                        "ui" => ("This value set is provided to user look up in a given context", "UI"),
                        "starter" => ("This value set is a good set of codes to start with when designing your system", "Starter"),
                        "component" => ("This value set is a component of the base value set", "Component"),
                        _ => ("Unknown code for purpose", other),
                    };
                    let mut sp = Elem::new("span");
                    sp.set_attr("title", title);
                    sp.tx(text);
                    td.push_elem(sp);
                }
            }
            tr.push_elem(td);
            if usage {
                // usage column (ABR:291-309): bare td; complex — gap if content.
                if d.has_usage {
                    self.gap("additional-binding usage cell content");
                }
                tr.push_elem(Elem::new("td"));
            }
            if any {
                let mut td = Elem::new("td");
                td.style("font-size: 11px");
                td.tx(if d.any { "any repeat" } else { "All repeats" });
                tr.push_elem(td);
            }
            if doco {
                let mut td = Elem::new("td");
                td.style("font-size: 11px");
                if let Some(ds) = &d.doco_short {
                    // innerHTML(docoShort): XhtmlFluent.innerHTML parses
                    // "<div>"+html+"</div>" and appends the parse root's
                    // children — which is THE DIV ITSELF (the parse returns a
                    // document whose child is the div). So the cell carries a
                    // <div> wrapper (golden-confirmed).
                    let mut div = Elem::new("div");
                    div.tx(ds.clone());
                    td.push_elem(div);
                }
                tr.push_elem(td);
            }
            rows_out.push(tr.build());
        }
        rows_out
    }

    /// `resolveBinding` (IGKnowledgeProvider:587-701).
    fn resolve_binding(&mut self, vs_ref: &str) -> BindingRes {
        if vs_ref.is_empty() {
            return BindingRes {
                url: Some("terminologies.html#unbound".into()),
                display: "(unbound)".into(),
                uri: None,
                external: false,
            };
        }
        // v3 special (branch 1)
        if let Some(rest) = vs_ref.strip_prefix("http://hl7.org/fhir/ValueSet/v3-") {
            return BindingRes {
                url: Some(format!("http://hl7.org/fhir/R4/v3/{}/vs.html", rest)),
                display: rest.to_string(),
                uri: Some(vs_ref.to_string()),
                external: false,
            };
        }
        if let Some(rest) = vs_ref.strip_prefix("http://hl7.org/fhir/ValueSet/v2-") {
            return BindingRes {
                url: Some(format!("http://hl7.org/fhir/R4/v2/{}/index.html", rest)),
                display: rest.to_string(),
                uri: Some(vs_ref.to_string()),
                external: false,
            };
        }
        // core VS (branch 3): display = getName, uri = vs.getUrl()
        if vs_ref.starts_with("http://hl7.org/fhir/ValueSet/") {
            if let Some(vs) = self.ctx.resolve(vs_ref) {
                return BindingRes {
                    url: Some(vs.web_path.clone()),
                    display: vs.name.clone().unwrap_or_default(),
                    uri: Some(strip_version(vs_ref)),
                    external: false,
                };
            }
            let rest = &vs_ref[29..];
            return BindingRes {
                url: None,
                display: format!("{} (??)", rest),
                uri: None,
                external: false,
            };
        }
        // LOINC vs
        if vs_ref.starts_with("http://loinc.org/vs/") {
            let code = &vs_ref[20..];
            let display = if code.starts_with("LL") {
                format!("LOINC Answer List {}", code)
            } else {
                format!("LOINC {}", code)
            };
            return BindingRes {
                url: Some(format!("https://loinc.org/{}/", code)),
                display,
                uri: Some(vs_ref.to_string()),
                external: false,
            };
        }
        // general (branch 6, IGKP:669-683): `url|ver` -> "Name (ver)";
        // else present() when webPath set.
        if let Some(vs) = self.ctx.resolve(vs_ref) {
            let display = if vs_ref.contains('|') {
                format!("{} ({})", vs.name.clone().unwrap_or_default(), vs.version)
            } else {
                vs.present()
            };
            return BindingRes {
                url: Some(vs.web_path.clone()),
                display,
                uri: Some(strip_version(vs_ref)),
                external: vs.external,
            };
        }
        // VSAC
        if vs_ref.contains("cts.nlm.nih.gov") {
            let oid = vs_ref.rsplit('/').next().unwrap_or("");
            return BindingRes {
                url: Some(format!("https://vsac.nlm.nih.gov/valueset/{}/expansion", oid)),
                display: format!("VSAC {}", oid),
                uri: Some(vs_ref.to_string()),
                external: true,
            };
        }
        if vs_ref.starts_with("http://") || vs_ref.starts_with("https://") {
            return BindingRes {
                url: Some(vs_ref.to_string()),
                display: vs_ref.to_string(),
                uri: None,
                external: false,
            };
        }
        BindingRes {
            url: None,
            display: vs_ref.to_string(),
            uri: None,
            external: false,
        }
    }

    /// `determineNarrativeStatus` (SDR@6.9.11): expand the narrative element's
    /// `status` child binding VS and return the code set in Java HashSet
    /// iteration order (the join order the golden carries).
    fn determine_narrative_status(&mut self, definition: Ed<'a>) -> Vec<String> {
        let status_path = format!("{}.status", definition.path());
        let status = self.all.iter().find(|e| e.path() == status_path);
        let Some(status) = status else { return Vec::new() };
        let Some(binding) = status.binding() else { return Vec::new() };
        let Some(vs_url) = binding.get("valueSet").and_then(|x| x.as_str()) else {
            return Vec::new();
        };
        // Tier-0 local expansion: enumerated compose.include concepts only.
        let Some(vs) = self.ctx.load_resource(&strip_version(vs_url)) else {
            self.gap("narrative status VS unresolved");
            return Vec::new();
        };
        let mut codes: Vec<String> = Vec::new();
        let compose = vs.get("compose");
        let includes = compose
            .and_then(|c| c.get("include"))
            .and_then(|x| x.as_array());
        let excludes = compose.and_then(|c| c.get("exclude")).is_some();
        let Some(includes) = includes else { return Vec::new() };
        for inc in includes {
            if inc.get("filter").is_some() || inc.get("valueSet").is_some() || excludes {
                self.gap("narrative status VS needs real expansion");
                return Vec::new();
            }
            if let Some(concepts) = inc.get("concept").and_then(|x| x.as_array()) {
                for con in concepts {
                    if let Some(code) = con.get("code").and_then(|x| x.as_str()) {
                        if !codes.contains(&code.to_string()) {
                            codes.push(code.to_string());
                        }
                    }
                }
            } else {
                // whole-system include: the narrative-status CS has 4 codes ->
                // statusCodes.size()==4 -> treated as unconstrained.
                return vec![
                    "generated".into(),
                    "extensions".into(),
                    "additional".into(),
                    "empty".into(),
                ];
            }
        }
        // Java collects into a HashSet and iterates it: reorder accordingly.
        render_tables::hashorder::hashmap_order(&codes)
    }

    fn sd_url(&self) -> &str {
        self.sd.root.get("url").and_then(|x| x.as_str()).unwrap_or("")
    }

    /// `isAbstractBaseProfile` (SDR:2210): resolved SD is abstract AND core url.
    fn is_abstract_base_profile(&self, url: &str) -> bool {
        if !url.starts_with("http://hl7.org/fhir/StructureDefinition/") {
            return false;
        }
        self.ctx
            .load_resource(url)
            .and_then(|sd| sd.get("abstract").and_then(|x| x.as_bool()))
            .unwrap_or(false)
    }
}

struct ExtDefn {
    url: String,
    sd: std::rc::Rc<serde_json::Value>,
    element: serde_json::Value,
}

struct ValueDefn {
    json: serde_json::Value,
    max: Option<String>,
}

/// A property of a FHIR type (from the type SD's snapshot direct children).
pub struct PropDef {
    name: String,
    type_codes: Vec<String>,
    max: String,
    short: String,
    path: String,
    base_path: String,
}

/// `value.children()` property model: the type SD's root children, in order.
fn type_properties(type_sd: &serde_json::Value) -> Vec<PropDef> {
    let mut out = Vec::new();
    let Some(elems) = type_sd
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(|x| x.as_array())
    else {
        return out;
    };
    let Some(root) = elems.first() else { return out };
    let root_path = root.get("path").and_then(|x| x.as_str()).unwrap_or("");
    let prefix = format!("{}.", root_path);
    for e in &elems[1..] {
        let p = e.get("path").and_then(|x| x.as_str()).unwrap_or("");
        if !p.starts_with(&prefix) || p[prefix.len()..].contains('.') {
            continue;
        }
        let ed = Ed::new(e);
        let name = p[prefix.len()..].to_string();
        // Property names for [x] keep the [x] (rare in patterns; JSON lookup
        // by the bare name will miss — flagged by absence).
        out.push(PropDef {
            name,
            type_codes: ed.types().iter().map(|t| t.working_code().to_string()).collect(),
            max: ed.max().unwrap_or("1").to_string(),
            short: ed.short().unwrap_or("").to_string(),
            path: p.to_string(),
            base_path: ed.base_path().unwrap_or(p).to_string(),
        });
    }
    out
}

/// `matchesInScopeElement` (SDR:2977-2988).
fn matches_in_scope_element(path: &str, id: Option<&str>, ed: Ed<'_>) -> bool {
    if !matches_in_scope_path(path, ed.path()) {
        return false;
    }
    match id {
        None => true,
        Some(i) if !i.contains(':') => true,
        Some(i) => matches_in_scope_path(i, ed.id()),
    }
}

fn matches_in_scope_path(path: &str, candidate: &str) -> bool {
    if path == candidate {
        return true;
    }
    candidate.ends_with("[x]") && path.starts_with(&candidate[..candidate.len() - 3])
}

pub struct AddBindingDetail {
    purpose: String,
    value_set: String,
    doco_short: Option<String>,
    has_usage: bool,
    any: bool,
}

/// Collect additional-binding rows in the publisher's order (SDR:2015-2026):
/// binding.additional / tools+R5-shadow extensions (converted R4, in extension
/// order), then maxValueSet, then minValueSet.
fn collect_additional_bindings(binding: &serde_json::Value) -> Vec<AddBindingDetail> {
    let mut out = Vec::new();
    // R5-native binding.additional
    if let Some(adds) = binding.get("additional").and_then(|x| x.as_array()) {
        for ab in adds {
            out.push(AddBindingDetail {
                purpose: ab.get("purpose").and_then(|x| x.as_str()).unwrap_or("").into(),
                value_set: ab.get("valueSet").and_then(|x| x.as_str()).unwrap_or("").into(),
                doco_short: ab.get("shortDoco").and_then(|x| x.as_str()).map(String::from),
                has_usage: ab.get("usage").and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(false),
                any: ab.get("any").and_then(|x| x.as_bool()).unwrap_or(false),
            });
        }
    }
    let exts = binding.get("extension").and_then(|x| x.as_array());
    if let Some(exts) = exts {
        // converted-R4 additional-binding extensions (ElementDefinition40_50
        // .java:618-660 folds these into binding.additional in ext order).
        for e in exts {
            let url = e.get("url").and_then(|x| x.as_str()).unwrap_or("");
            if url == "http://hl7.org/fhir/tools/StructureDefinition/additional-binding"
                || url == "http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.binding.additional"
            {
                let sub = |name: &str| -> Option<&serde_json::Value> {
                    e.get("extension")
                        .and_then(|x| x.as_array())
                        .and_then(|a| a.iter().find(|s| s.get("url").and_then(|u| u.as_str()) == Some(name)))
                };
                let val = |v: Option<&serde_json::Value>| -> Option<String> {
                    v.and_then(|x| {
                        x.get("valueCode")
                            .or_else(|| x.get("valueCanonical"))
                            .or_else(|| x.get("valueUri"))
                            .or_else(|| x.get("valueString"))
                            .or_else(|| x.get("valueMarkdown"))
                            .and_then(|y| y.as_str())
                            .map(String::from)
                    })
                };
                out.push(AddBindingDetail {
                    purpose: val(sub("purpose")).unwrap_or_default(),
                    value_set: val(sub("valueSet")).unwrap_or_default(),
                    doco_short: val(sub("shortDoco")),
                    has_usage: sub("usage").is_some(),
                    any: val(sub("scope")).as_deref() == Some("any")
                        || sub("any")
                            .and_then(|x| x.get("valueBoolean"))
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false),
                });
            }
        }
        // maxValueSet -> "maximum"; minValueSet -> "minimum"
        for (url, purpose) in [
            ("http://hl7.org/fhir/StructureDefinition/elementdefinition-maxValueSet", "maximum"),
            ("http://hl7.org/fhir/StructureDefinition/elementdefinition-minValueSet", "minimum"),
        ] {
            if let Some(e) = exts.iter().find(|e| e.get("url").and_then(|x| x.as_str()) == Some(url)) {
                let vs = e
                    .get("valueCanonical")
                    .or_else(|| e.get("valueUri"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                out.push(AddBindingDetail {
                    purpose: purpose.into(),
                    value_set: vs.into(),
                    doco_short: None,
                    has_usage: false,
                    any: false,
                });
            }
        }
    }
    out
}

struct BindingRes {
    url: Option<String>,
    display: String,
    uri: Option<String>,
    external: bool,
}

// ---- free helpers ----

/// The core-spec web root (with trailing slash) for an IG's fhirVersion.
/// This is VersionUtilities.getSpecUrl data (core-spec structure, not IG
/// behavior): 4.0.x -> R4, 4.3.x -> R4B, 5.0.x -> R5, 3.0.x -> STU3.
pub fn core_path_for(fhir_version: &str) -> &'static str {
    if fhir_version.starts_with("4.0") {
        "http://hl7.org/fhir/R4/"
    } else if fhir_version.starts_with("4.3") {
        "http://hl7.org/fhir/R4B/"
    } else if fhir_version.starts_with("5.0") {
        "http://hl7.org/fhir/R5/"
    } else if fhir_version.starts_with("3.0") {
        "http://hl7.org/fhir/STU3/"
    } else {
        "http://hl7.org/fhir/R4/"
    }
}

pub const EXT_OBLIGATION_CORE: &str =
    "http://hl7.org/fhir/StructureDefinition/obligation";
pub const EXT_OBLIGATION_TOOLS: &str =
    "http://hl7.org/fhir/tools/StructureDefinition/obligation";
pub const EXT_STANDARDS_STATUS: &str =
    "http://hl7.org/fhir/StructureDefinition/structuredefinition-standards-status";

fn sd_has_obligations(root: &serde_json::Value) -> bool {
    root.get("extension")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter().any(|e| {
                matches!(
                    e.get("url").and_then(|x| x.as_str()),
                    Some(EXT_OBLIGATION_CORE) | Some(EXT_OBLIGATION_TOOLS)
                )
            })
        })
        .unwrap_or(false)
}

/// `noTail(id)` (SDR:1191): strip a trailing ".<int>" (the makeAnchorUnique
/// dedup suffix).
fn no_tail(id: &str) -> &str {
    if let Some(i) = id.rfind('.') {
        if id[i + 1..].chars().all(|c| c.is_ascii_digit()) && !id[i + 1..].is_empty() {
            return &id[..i];
        }
    }
    id
}

/// The container children push into: the holder row's sub_rows when a holder
/// was created (Java reassigns local `row` to hrow), else the row's sub_rows.
fn target_subrows<'r>(rows: &'r mut [Row], row_idx: usize, has_holder: bool) -> &'r mut Vec<Row> {
    if has_holder {
        &mut rows[row_idx].sub_rows.last_mut().unwrap().sub_rows
    } else {
        &mut rows[row_idx].sub_rows
    }
}

/// Walk an index path (each step descends into sub_rows).
fn descend<'r>(base: &'r mut Vec<Row>, path: &[usize]) -> &'r mut Vec<Row> {
    let mut cur = base;
    for &i in path {
        cur = &mut cur[i].sub_rows;
    }
    cur
}

fn uses_must_support(list: &[Ed<'_>]) -> bool {
    list.iter().any(|e| e.has_must_support() && e.must_support())
}

pub fn tail(path: &str) -> &str {
    match path.rfind('.') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

fn get_children<'a>(all: &'a [Ed<'a>], element: Ed<'a>) -> Vec<Ed<'a>> {
    let mut result = Vec::new();
    let ep = element.path();
    let idx = all.iter().position(|e| std::ptr::eq(e.v, element.v));
    let Some(i0) = idx else { return result };
    let mut i = i0 + 1;
    let prefix = format!("{}.", ep);
    while i < all.len() && all[i].path().len() > ep.len() {
        let p = all[i].path();
        if p.starts_with(&prefix) && !p[prefix.len()..].contains('.') {
            result.push(all[i]);
        }
        i += 1;
    }
    result
}

fn is_extension_elem(e: Ed<'_>) -> bool {
    let types = e.types();
    if types.is_empty() {
        return true;
    }
    types[0].working_code() == "Extension"
}

fn all_are_reference(types: &[TypeRef<'_>]) -> bool {
    types.iter().all(|t| t.has_target())
}

/// `standardExtensionSlicing` (SDR:1527).
fn standard_extension_slicing(e: Ed<'_>) -> bool {
    let t = tail(e.path());
    if t != "extension" && t != "modifierExtension" {
        return false;
    }
    let Some(sl) = e.slicing() else { return false };
    let rules = sl.get("rules").and_then(|x| x.as_str());
    let disc = sl.get("discriminator").and_then(|x| x.as_array());
    rules != Some("closed")
        && disc.map(|d| d.len() == 1).unwrap_or(false)
        && disc
            .and_then(|d| d[0].get("path"))
            .and_then(|x| x.as_str())
            == Some("url")
        && disc
            .and_then(|d| d[0].get("type"))
            .and_then(|x| x.as_str())
            == Some("value")
}

/// `element.prohibited()`: max == "0".
fn prohibited(e: Ed<'_>) -> bool {
    e.max() == Some("0")
}

/// `slicesExist` (SDR:3278).
fn slices_exist(all: &[Ed<'_>], element: Ed<'_>) -> bool {
    let Some(start) = all.iter().position(|e| std::ptr::eq(e.v, element.v)) else {
        return false;
    };
    let ep = element.path();
    let mut found = false;
    for e in &all[start..] {
        if e.path() == ep && e.has_slice_name() {
            found = true;
        }
        if e.path().len() < ep.len() {
            break;
        }
    }
    found
}

fn is_base_key(key: &str) -> bool {
    key.starts_with("ele-")
        || key.starts_with("res-")
        || key.starts_with("ext-")
        || key.starts_with("dom-")
        || key.starts_with("dr-")
}

fn has_non_base_constraints(e: Ed<'_>) -> bool {
    e.constraints().iter().any(|c| !is_base_key(c.key()))
}

fn has_non_base_conditions(e: Ed<'_>) -> bool {
    e.conditions().iter().any(|c| !is_base_key(c))
}

fn list_constraints_and_conditions(e: Ed<'_>) -> String {
    let mut ids: Vec<String> = Vec::new();
    for con in e.constraints() {
        if !is_base_key(con.key()) && !ids.contains(&con.key().to_string()) {
            ids.push(con.key().to_string());
        }
    }
    for id in e.conditions() {
        if !is_base_key(id) && !ids.contains(&id.to_string()) {
            ids.push(id.to_string());
        }
    }
    ids.join(", ")
}

/// `genCardinality` (SDR:1428) without fallback.
fn gen_cardinality(e: Ed<'_>, tracker: &mut UnusedTracker) -> Cell {
    gen_cardinality_impl(e.min(), e.max(), tracker)
}

/// with extension fallback element (SDR:1399).
fn gen_cardinality_fb(e: Ed<'_>, tracker: &mut UnusedTracker, fb: Option<&ExtDefn>) -> Cell {
    let mut min = e.min();
    let mut max = e.max();
    if let Some(fb) = fb {
        if min.is_none() {
            min = fb.element.get("min").and_then(|x| x.as_i64());
        }
        if max.is_none() {
            // borrow issue: read into owned below
        }
    }
    let max_owned: Option<String>;
    if max.is_none() {
        max_owned = fb
            .and_then(|f| f.element.get("max").and_then(|x| x.as_str()))
            .map(String::from);
        max = max_owned.as_deref();
        gen_cardinality_impl(min, max, tracker)
    } else {
        gen_cardinality_impl(min, max, tracker)
    }
}

fn gen_cardinality_impl(min: Option<i64>, max: Option<&str>, tracker: &mut UnusedTracker) -> Cell {
    if let Some(m) = max {
        tracker.used = m != "0";
    }
    let mut cell = Cell::with(None, None, None, None, None);
    if min.is_some() || max.is_some() {
        cell.pieces.push(Piece::ref_text(
            None,
            Some(min.map(|m| m.to_string()).unwrap_or_default()),
            None,
        ));
        cell.pieces.push(Piece::ref_text(None, Some("..".into()), None));
        cell.pieces.push(Piece::ref_text(
            None,
            Some(max.map(String::from).unwrap_or_default()),
            None,
        ));
    }
    cell
}

fn is_profiled_type(profiles: &[&str]) -> bool {
    profiles.iter().any(|p| p.contains(':'))
}

/// isMustSupportDirect(t)/isMustSupport(t): the type carries the
/// elementdefinition-type-must-support extension = true.
fn type_is_must_support(t: &TypeRef<'_>) -> bool {
    t.v.get("extension")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter().any(|e| {
                e.get("url").and_then(|x| x.as_str())
                    == Some("http://hl7.org/fhir/StructureDefinition/elementdefinition-type-must-support")
                    && e.get("valueBoolean").and_then(|x| x.as_bool()) == Some(true)
            })
        })
        .unwrap_or(false)
}

/// isMustSupport(CanonicalType u): the canonical (profile/targetProfile entry)
/// carries the type-must-support extension. In JSON these live in the parallel
/// `_targetProfile`/`_profile` arrays.
fn canonical_is_must_support(t: &TypeRef<'_>, u: &str) -> bool {
    for key in ["_targetProfile", "_profile"] {
        let Some(shadow) = t.v.get(key).and_then(|x| x.as_array()) else { continue };
        let Some(vals) = t
            .v
            .get(key.trim_start_matches('_'))
            .and_then(|x| x.as_array())
        else {
            continue;
        };
        for (i, val) in vals.iter().enumerate() {
            if val.as_str() == Some(u) {
                if let Some(sh) = shadow.get(i) {
                    if sh
                        .get("extension")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter().any(|e| {
                                e.get("url").and_then(|x| x.as_str())
                                    == Some("http://hl7.org/fhir/StructureDefinition/elementdefinition-type-must-support")
                                    && e.get("valueBoolean").and_then(|x| x.as_bool())
                                        == Some(true)
                            })
                        })
                        .unwrap_or(false)
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// `buildJson` (SDR:3506): primitives as string value; complex as JSON.
fn build_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn is_primitive_value(v: &serde_json::Value) -> bool {
    !matches!(v, serde_json::Value::Object(_) | serde_json::Value::Array(_))
}

/// `describeSlice` (SDR:3514): "{Ordered|Unordered}, {rules} by {discriminators}".
fn describe_slice(slicing: &serde_json::Value) -> String {
    let ordered = slicing
        .get("ordered")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let rules = match slicing.get("rules").and_then(|x| x.as_str()) {
        Some("closed") => "Closed",
        Some("open") => "Open",
        Some("openAtEnd") => "Open At End",
        _ => "Unspecified",
    };
    let discs: Vec<String> = slicing
        .get("discriminator")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|d| {
                    format!(
                        "{}:{}",
                        d.get("type").and_then(|x| x.as_str()).unwrap_or("??"),
                        d.get("path").and_then(|x| x.as_str()).unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    format!(
        "{}, {} by {}",
        if ordered { "Ordered" } else { "Unordered" },
        rules,
        discs.join(", ")
    )
}

/// BindingStrength.getDefinition() (Enumerations.java:1515-1518).
fn strength_definition(code: &str) -> &'static str {
    match code {
        "required" => "To be conformant, the concept in this element SHALL be from the specified value set.",
        "extensible" => "To be conformant, the concept in this element SHALL be from the specified value set if any of the codes within the value set can apply to the concept being communicated.  If the value set does not cover the concept (based on human review), alternate codings (or, data type allowing, text) may be included instead.",
        "preferred" => "Instances are encouraged to draw from the specified codes for interoperability purposes but are not required to do so to be considered conformant.",
        "example" => "Instances are not expected or even encouraged to draw from the specified value set.  The value set merely provides examples of the types of concepts intended to be included.",
        _ => "",
    }
}

/// `CommaSeparatedStringBuilder.join2(", ", " and ", items)`.
fn join2(sep: &str, last_sep: &str, items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{}{}{}", items[..n - 1].join(sep), last_sep, items[n - 1]),
    }
}

fn strip_version(url: &str) -> String {
    match url.split_once('|') {
        Some((u, _)) => u.to_string(),
        None => url.to_string(),
    }
}

/// `MarkDownProcessor.isSimpleMarkdown` — a description with no markdown block
/// structure. Conservative approximation aligned with the plain-prose test.
fn is_simple_markdown(s: &str) -> bool {
    !s.contains('\n')
}

/// sd.getTypeName() for a resolved type (name field of the SD).
fn type_name_of(sd: &Resolved, fallback: &str) -> String {
    sd.name.clone().unwrap_or_else(|| fallback.to_string())
}

fn capitalize(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
        None => String::new(),
    }
}
