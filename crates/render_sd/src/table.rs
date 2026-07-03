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

use render_tables::model::{Cell, Piece, Row, TableGenerationMode};
use render_tables::{generate, Gen};
use render_xhtml::{Config, XhtmlComposer};

use crate::context::{strip_version, BindingRes, IgContext, Resolved};
use crate::gentypes::TypesHost;
use crate::markdown;
use crate::sdmodel::{Ed, Sd, TypeRef};

pub const RED_BACKGROUND_COLOR: &str = "#D50000"; // SDR:104
pub const OPACITY: &str = "opacity: 0.5"; // RenderingContext.getOpacity() (RenderingContext.java:76, wcagConformant=false)
pub const CONSTRAINT_CHAR: &str = "C"; // SDR:392
pub const CONSTRAINT_STYLE: &str = "padding-left: 3px; padding-right: 3px; border: 1px maroon solid; font-weight: bold; color: #301212; background-color: #fdf4f4;"; // SDR:393

/// `context.getStructureMode()` (StructureDefinitionRendererMode). Selects the
/// column model in generateTableInner (SDR:627-648) and the per-element cell
/// builder in genElement (SDR:1022-1035). SUMMARY = initNormalTable (Flags/
/// Card/Type/Description). BINDINGS / OBLIGATIONS = initCustomTable (Name +
/// scanned columns). MAPPINGS is not exercised by this corpus (no goldens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureMode {
    Summary,
    Bindings,
    Obligations,
}

/// Per-fragment configuration (the publisher wrapper flags).
#[derive(Debug, Clone)]
pub struct TableConfig {
    pub diff: bool,
    pub snapshot: bool,
    pub all_invariants: bool,
    pub must_support: bool,
    /// byKey view: filter to the key-element set (constraint SDs only).
    pub key: bool,
    /// `context.getStructureMode()` — the column/cell model (SDR:627).
    pub mode: StructureMode,
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
            key: false,
            mode: StructureMode::Summary,
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
    /// `byMustSupport()` (publisher SDR:552): generateTable on the MS-filtered
    /// element copy with diff=F, snapshot=T, allInv=F, mustSupport=T, prefix
    /// "m"/"ma", idSfx M/MA.
    pub fn snapshot_by_mustsupport(run_uuid: &str) -> TableConfig {
        TableConfig {
            diff: false,
            snapshot: true,
            all_invariants: false,
            must_support: true,
            key: false,
            mode: StructureMode::Summary,
            prefix: "m".into(),
            id_sfx: "M".into(),
            run_uuid: run_uuid.into(),
            active_tables: false,
        }
    }
    pub fn snapshot_by_mustsupport_all(run_uuid: &str) -> TableConfig {
        TableConfig {
            prefix: "ma".into(),
            id_sfx: "MA".into(),
            ..TableConfig::snapshot_by_mustsupport(run_uuid)
        }
    }
    /// `byKey()` (publisher SDR:532): generateTable on the key-element copy with
    /// diff=F, snapshot=T, allInv=T, mustSupport=F, prefix "k"/"ka", idSfx K/KA.
    pub fn snapshot_by_key(run_uuid: &str) -> TableConfig {
        TableConfig {
            diff: false,
            snapshot: true,
            all_invariants: true,
            must_support: false,
            key: true,
            mode: StructureMode::Summary,
            prefix: "k".into(),
            id_sfx: "K".into(),
            run_uuid: run_uuid.into(),
            active_tables: false,
        }
    }
    pub fn snapshot_by_key_all(run_uuid: &str) -> TableConfig {
        TableConfig {
            prefix: "ka".into(),
            id_sfx: "KA".into(),
            ..TableConfig::snapshot_by_key(run_uuid)
        }
    }
    /// `diff()` (publisher SDR:487): generateTable(diff=T, snapshot=F,
    /// allInv=F, ms=F, prefix "", idSfx D). Element list =
    /// supplementMissingDiffElements (SDR:617).
    pub fn diff_view(run_uuid: &str) -> TableConfig {
        TableConfig {
            diff: true,
            snapshot: false,
            all_invariants: false,
            must_support: false,
            key: false,
            mode: StructureMode::Summary,
            prefix: "".into(),
            id_sfx: "D".into(),
            run_uuid: run_uuid.into(),
            active_tables: false,
        }
    }
    pub fn diff_all(run_uuid: &str) -> TableConfig {
        TableConfig {
            prefix: "a".into(),
            id_sfx: "DA".into(),
            ..TableConfig::diff_view(run_uuid)
        }
    }

    // --- BINDINGS mode (publisher SDR wrapper `snapshot(...BINDINGS...)`:510) ---
    // Same snapshot flags (diff=F, snapshot=T, allInv=T, ms=F) as `snapshot`,
    // with StructureMode::BINDINGS. uniqueLocalPrefix = mc(BINDINGS)+"s" = "bs"
    // / "bsa"; idSfx S/SA.
    pub fn snapshot_bindings(run_uuid: &str) -> TableConfig {
        TableConfig {
            mode: StructureMode::Bindings,
            prefix: "bs".into(),
            ..TableConfig::snapshot(run_uuid)
        }
    }
    pub fn snapshot_bindings_all(run_uuid: &str) -> TableConfig {
        TableConfig {
            prefix: "bsa".into(),
            id_sfx: "SA".into(),
            ..TableConfig::snapshot_bindings(run_uuid)
        }
    }
    // --- snapshot-obligations: publisher `snapshot(..., OBLIGATIONS, all)`
    // (PublisherGenerator:1920) — the SNAPSHOT wrapper with the OBLIGATIONS mode
    // param, NOT the `obligations()` wrapper (which makes the distinct
    // `-obligations` fragment). So: snapshot flags (diff=F, snapshot=T, allInv=T,
    // ms=F), uniqueLocalPrefix = mc(OBLIGATIONS)+"s" = "os"/"osa", idSfx S/SA.
    pub fn snapshot_obligations(run_uuid: &str) -> TableConfig {
        TableConfig {
            mode: StructureMode::Obligations,
            prefix: "os".into(),
            ..TableConfig::snapshot(run_uuid)
        }
    }
    pub fn snapshot_obligations_all(run_uuid: &str) -> TableConfig {
        TableConfig {
            prefix: "osa".into(),
            id_sfx: "SA".into(),
            ..TableConfig::snapshot_obligations(run_uuid)
        }
    }
    // --- diff + mode variants (publisher `diff(...mode...)`:487) ---
    // diff flags (diff=T, snapshot=F, allInv=F, ms=F). uniqueLocalPrefix =
    // mc(mode) [+ "a"]; idSfx D/DA.
    pub fn diff_bindings(run_uuid: &str) -> TableConfig {
        TableConfig {
            mode: StructureMode::Bindings,
            prefix: "b".into(),
            ..TableConfig::diff_view(run_uuid)
        }
    }
    pub fn diff_bindings_all(run_uuid: &str) -> TableConfig {
        TableConfig {
            prefix: "ba".into(),
            id_sfx: "DA".into(),
            ..TableConfig::diff_bindings(run_uuid)
        }
    }
    pub fn diff_obligations(run_uuid: &str) -> TableConfig {
        TableConfig {
            mode: StructureMode::Obligations,
            prefix: "o".into(),
            ..TableConfig::diff_view(run_uuid)
        }
    }
    pub fn diff_obligations_all(run_uuid: &str) -> TableConfig {
        TableConfig {
            prefix: "oa".into(),
            id_sfx: "DA".into(),
            ..TableConfig::diff_obligations(run_uuid)
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
    let prefix = if cfg.prefix.is_empty() {
        None
    } else {
        Some(cfg.prefix.clone())
    };
    // HTG.mode: initNormalTable (SUMMARY) sets this.mode = XHTML (HTG:858), which
    // gates the `no-external`/`data-no-external` link attrs (HTG:972/1153).
    // initCustomTable (BINDINGS/OBLIGATIONS, SDR:885) NEVER sets this.mode, so it
    // stays null — hence those tables carry NO no-external attrs (golden-
    // confirmed: 0 no-external in -bindings/-obligations, same as grid).
    let mut gen = match cfg.mode {
        StructureMode::Summary => Gen::new_normal(prefix, TableGenerationMode::Xhtml),
        StructureMode::Bindings | StructureMode::Obligations => Gen::new(prefix),
    };
    gen.run_uuid = cfg.run_uuid.clone();

    // corePath: the publisher passes the core-spec web root with trailing
    // slash (verified in goldens: terminologies.html/conformance-rules links
    // and the https help16.png all live under http://hl7.org/fhir/R4/).
    let core_path = core_path_for(sd.fhir_version());

    // Element list. For byMustSupport the publisher renders a `sdCopy` whose
    // snapshot is `getMustSupportElements()` (MS elements + ancestors, with
    // example cleared and non-MS elements dimmed via render_opaque + binding/
    // constraints cleared). We build owned modified element JSON for that case.
    let use_owned = cfg.must_support || cfg.key || cfg.diff;
    let owned: Vec<serde_json::Value>;
    let all: Vec<Ed> = if use_owned {
        owned = if cfg.diff {
            // diff view: differential + synthetic root/sparse fill
            // (supplementMissingDiffElements, SGPP:1102; SDR:617).
            crate::diff::supplement_missing_diff_elements(sd)
        } else if cfg.must_support {
            must_support_elements(sd)
        } else {
            key_elements(sd, ctx)
        };
        owned.iter().map(Ed::new).collect()
    } else {
        Vec::new()
    };
    let borrowed: Vec<Ed> = if use_owned {
        Vec::new()
    } else {
        sd.snapshot_elements()
    };
    let all: &[Ed] = if use_owned { &all } else { &borrowed };
    // render_opaque ids (SDR:996): non-MS elements below the root in the MS view.
    let opaque_ids: std::collections::HashSet<String> = if cfg.must_support {
        owned_opaque_ids(sd)
    } else {
        std::collections::HashSet::new()
    };
    // diff-mode pointer reconstruction. The publisher's diff render reads
    // `SNAPSHOT_DERIVATION_POINTER` userData off each differential element —
    // stamped during snapshot generation (PU:2591: derived.setUserData(POINTER,
    // base), base = the base clone that BECOMES the output snapshot element).
    // Our JSON input carries no userData, so we reconstruct: pointer(diffElem)
    // = the element in the profile's OWN snapshot with the same id. For any
    // property the diff did not restate, snapshot[id].prop == base[id].prop,
    // so the own-snapshot element reproduces the base value byte-for-byte.
    // Synthetic elements (supplementMissingDiffElements roots/sparse fill)
    // never went through updateFromDefinition => no pointer.
    let pointers: HashMap<String, Ed> = if cfg.diff {
        let snap = sd.snapshot_elements();
        let mut exact: HashMap<&str, Ed> = HashMap::new();
        let mut alias: HashMap<String, Ed> = HashMap::new();
        for e in &snap {
            exact.insert(e.id(), *e);
            // Choice-rename alias: the differential may write the RENAMED
            // choice id (`Observation.valueQuantity.code`) where the generated
            // snapshot holds the sliced form (`Observation.value[x]:valueQuantity.code`).
            // The walk matched them during generation (PPP:887-909), so the
            // stamped pointer crosses this rename; reproduce by aliasing every
            // `base[x]:baseType` segment to `baseType`.
            let mut changed = false;
            let alias_id: Vec<String> = e
                .id()
                .split('.')
                .map(|seg| {
                    if let Some((l, r)) = seg.split_once("[x]:") {
                        if r.starts_with(l) {
                            changed = true;
                            return r.to_string();
                        }
                    }
                    seg.to_string()
                })
                .collect();
            if changed {
                alias.insert(alias_id.join("."), *e);
            }
        }
        let mut map: HashMap<String, Ed> = HashMap::new();
        for d in sd.differential_elements() {
            let id = d.id();
            if let Some(e) = exact.get(id).or_else(|| alias.get(id)) {
                map.insert(id.to_string(), *e);
                continue;
            }
            // Unsliced choice rename: diff `…component:systolic.valueQuantity.value`
            // vs snapshot `…component:systolic.value[x].value` (the walk's
            // isSameBase match, PU:2507 — `p` ends [x] and the renamed segment
            // starts with its stem). Try rewriting each camelCase segment back
            // to its `stem[x]` form.
            'outer: for cand in dechoice_candidates(id) {
                if let Some(e) = exact.get(cand.as_str()) {
                    map.insert(id.to_string(), *e);
                    break 'outer;
                }
            }
        }
        map
    } else {
        HashMap::new()
    };
    // Column model + table factory (generateTableInner:623-648). SUMMARY ->
    // initNormalTable (Flags/Card/Type/Description). BINDINGS/OBLIGATIONS ->
    // scan the element list for columns, then initCustomTable (Name + columns).
    let columns: Vec<render_tables::Column> = match cfg.mode {
        StructureMode::Summary => Vec::new(),
        StructureMode::Bindings => scan_bindings(all),
        StructureMode::Obligations => scan_obligations(ctx, all),
    };
    let mut model = match cfg.mode {
        StructureMode::Summary => generate::init_normal_table(
            core_path,
            false,
            true,
            Some(format!("{}{}", sd.id(), cfg.id_sfx)),
            true,
        ),
        StructureMode::Bindings | StructureMode::Obligations => generate::init_custom_table(
            core_path,
            false,
            true,
            Some(format!("{}{}", sd.id(), cfg.id_sfx)),
            true,
            &columns,
        ),
    };
    model.active_tables = cfg.active_tables;

    let mut t = TCtx {
        ctx,
        sd,
        all,
        cfg,
        gen: &gen,
        pointers,
        anchors: HashMap::new(),
        def_path: if def_file.is_empty() {
            None
        } else {
            Some(format!("{}#", def_file))
        },
        core_path,
        is_constraint_mode: sd.derivation() == "constraint" && uses_must_support(all),
        key_rows: Vec::new(),
        gaps: Vec::new(),
        merged_pattern_values: HashMap::new(),
        opaque_ids,
        columns,
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
    /// diff mode: reconstructed SNAPSHOT_DERIVATION_POINTER (diff element id ->
    /// own-snapshot element). Empty for non-diff kinds.
    pointers: HashMap<String, Ed<'a>>,
    anchors: HashMap<String, i32>,
    def_path: Option<String>,
    core_path: &'static str,
    is_constraint_mode: bool,
    key_rows: Vec<String>,
    gaps: Vec<String>,
    /// `mergedPatternValues` (SDR:611, 2927-2942): element index (in `all`) ->
    /// merged pattern child values.
    merged_pattern_values: HashMap<usize, Vec<serde_json::Value>>,
    /// Element ids carrying `render_opaque` (byMustSupport non-MS rows, SDR:996).
    opaque_ids: std::collections::HashSet<String>,
    /// BINDINGS/OBLIGATIONS custom columns (scanBindings/scanObligations); empty
    /// for SUMMARY. genElementBindings/genElementObligations add one cell per
    /// column to each row (SDR:1024/1027).
    columns: Vec<render_tables::Column>,
}

struct UnusedTracker {
    used: bool,
}

impl<'a> TCtx<'a> {
    fn gap(&mut self, what: &str) {
        self.gaps.push(what.to_string());
    }

    /// diff mode: `element.getUserData(SNAPSHOT_DERIVATION_POINTER)`
    /// (reconstructed as the own-snapshot id match; see render_table).
    fn pointer(&self, e: Ed<'_>) -> Option<Ed<'a>> {
        if self.cfg.diff {
            self.pointers.get(e.id()).copied()
        } else {
            None
        }
    }

    /// `genCardinality` (SDR:1431-1475). In diff mode, a missing min/max is
    /// filled from the DERIVATION_POINTER's element and DIMMED (SDR:1434-1447:
    /// `min.setUserData(SNAPSHOT_DERIVATION_EQUALS, true)` -> checkForNoChange
    /// adds `context.getOpacity()` = "opacity: 0.5", RenderingContext.java:76),
    /// then from the extension fallback element WITHOUT dimming (SDR:1448-1451).
    /// The ".." piece dims only when BOTH min and max carry EQUALS (the two-arg
    /// checkForNoChange, SDR:3509-3514).
    fn gen_cardinality(&self, e: Ed<'_>, tracker: &mut UnusedTracker, fb: Option<&ExtDefn>) -> Cell {
        let mut min = e.min();
        let mut max: Option<String> = e.max().map(String::from);
        let mut min_eq = false;
        let mut max_eq = false;
        if min.is_none() {
            if let Some(p) = self.pointer(e) {
                if let Some(m) = p.min() {
                    min = Some(m);
                    min_eq = true;
                }
            }
        }
        if max.is_none() {
            if let Some(p) = self.pointer(e) {
                if let Some(m) = p.max() {
                    max = Some(m.to_string());
                    max_eq = true;
                }
            }
        }
        if min.is_none() {
            if let Some(f) = fb {
                min = f.element.get("min").and_then(|x| x.as_i64());
            }
        }
        if max.is_none() {
            if let Some(f) = fb {
                max = f.element.get("max").and_then(|x| x.as_str()).map(String::from);
            }
        }
        if let Some(m) = &max {
            tracker.used = m != "0";
        }
        let mut cell = Cell::with(None, None, None, None, None);
        if min.is_some() || max.is_some() {
            let mut p1 = Piece::ref_text(
                None,
                Some(min.map(|m| m.to_string()).unwrap_or_default()),
                None,
            );
            if min_eq {
                p1.add_style(OPACITY);
            }
            cell.pieces.push(p1);
            let mut p2 = Piece::ref_text(None, Some("..".into()), None);
            if min_eq && max_eq {
                p2.add_style(OPACITY);
            }
            cell.pieces.push(p2);
            let mut p3 = Piece::ref_text(None, Some(max.unwrap_or_default()), None);
            if max_eq {
                p3.add_style(OPACITY);
            }
            cell.pieces.push(p3);
        }
        cell
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
        // SDR:930: the whole element (row + children walk) is emitted only when
        // NOT (onlyInformationIsMapping || (OBLIGATIONS && no obligations here or
        // below)). onlyInformationIsMapping ~ never true for real corpora. In
        // OBLIGATIONS mode, an element with no obligations on it or any
        // descendant is skipped ENTIRELY (no row, no anchor bump, no recursion).
        if self.cfg.mode == StructureMode::Obligations
            && !self.element_or_descendents_have_obligations(element)
        {
            return None;
        }
        let children = get_children(self.all, element);
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
        // render_opaque dimming (SDR:996): byMustSupport non-MS rows.
        if self.opaque_ids.contains(element.id()) {
            row.opacity = Some("0.5".into());
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
        // Per-mode cells (SDR:1022-1035).
        match self.cfg.mode {
            StructureMode::Summary => {
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
            }
            StructureMode::Bindings => {
                self.gen_element_bindings(&mut row, element);
            }
            StructureMode::Obligations => {
                self.gen_element_obligations(&mut row, element);
            }
        }

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
            self.push_scaffold_tail(&mut hrow, "Content/Rules for all slices");
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
            self.push_scaffold_tail(&mut hrow, "Content/Rules for all Types");
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
                        self.push_scaffold_tail(&mut parent, "Content/Rules for all slices");
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

        // choice [x] type rows (SDR:1173): appended to typesRow.getSubRows() —
        // the element's TOP row's sub_rows (typesRow == row; the holder is a
        // child within it, so choice rows come after the holder). Java gates
        // this on `context.getStructureMode() == SUMMARY` — BINDINGS/OBLIGATIONS
        // never emit the per-type choice rows.
        if types_row && !prohibited(element) && self.cfg.mode == StructureMode::Summary {
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

    /// Push the mode-appropriate trailing cells to a scaffold row (:All Slices /
    /// :All Types / Slices-for holder). SUMMARY (SDR:1082-1087/1111-1116/1147-
    /// 1152) pushes 3 empty cells + a "Content/Rules …" cell. BINDINGS/
    /// OBLIGATIONS/MAPPINGS (SDR:1078-1080 etc) push ONE empty cell per column.
    /// The name cell is assumed already pushed.
    fn push_scaffold_tail(&self, row: &mut Row, content_rules_text: &str) {
        match self.cfg.mode {
            StructureMode::Summary => {
                row.cells.push(Cell::new());
                row.cells.push(Cell::new());
                row.cells.push(Cell::new());
                row.cells.push(Cell::with(
                    None,
                    None,
                    Some(content_rules_text.into()),
                    Some("".into()),
                    None,
                ));
            }
            StructureMode::Bindings | StructureMode::Obligations => {
                for _ in 0..self.columns.len() {
                    row.cells.push(Cell::new());
                }
            }
        }
    }

    /// `elementOrDescendentsHaveObligations` (SDR:1180): this element, or any
    /// descendant in `all` (path prefix match), carries an obligation extension.
    fn element_or_descendents_have_obligations(&self, element: Ed<'a>) -> bool {
        if element.has_extension(EXT_OBLIGATION_CORE) || element.has_extension(EXT_OBLIGATION_TOOLS)
        {
            return true;
        }
        let prefix = format!("{}.", element.path());
        let start = self.all.iter().position(|e| e.id() == element.id());
        let Some(start) = start else { return false };
        for e in &self.all[start + 1..] {
            if !e.path().starts_with(&prefix) {
                break;
            }
            if e.has_extension(EXT_OBLIGATION_CORE) || e.has_extension(EXT_OBLIGATION_TOOLS) {
                return true;
            }
        }
        false
    }

    /// `genElementObligations` (SDR:1228): one cell per column, each rendering
    /// the element's obligations for that actor via ObligationsRenderer.renderList.
    /// GAP: the ObligationsRenderer body (renderCodes + CodeResolver) is NOT
    /// ported — no golden in this corpus has obligation columns (all 3 IGs use
    /// zero obligation extensions), so every obligations table is Name-only and
    /// this loop runs zero times. If a future IG hits it, this fires a gap.
    fn gen_element_obligations(&mut self, row: &mut Row, _element: Ed<'a>) {
        if !self.columns.is_empty() {
            self.gap("genElementObligations: obligation-column content (ObligationsRenderer.renderList) not ported");
        }
        for _ in 0..self.columns.len() {
            row.cells.push(Cell::new());
        }
    }

    /// `genElementBindings` (SDR:1259): one cell per column; each gathers the
    /// element's bindings for that purpose (collectBindings) and renders them
    /// via AdditionalBindingsRenderer.render(children, list, sd) (ABR:437).
    fn gen_element_bindings(&mut self, row: &mut Row, element: Ed<'a>) {
        // Clone columns to avoid borrowing self immutably across the mutable
        // resolve_binding calls below.
        let col_ids: Vec<String> = self.columns.iter().map(|c| c.id.clone()).collect();
        for col_id in &col_ids {
            let mut gc = Cell::new();
            let bindings = collect_bindings(element, col_id);
            if !bindings.is_empty() {
                // gen.new Piece(null): a bare tag=null piece whose children carry
                // the rendered nodes (a null-tag Piece composes as just its
                // children — HTG Piece with tag==null && reference==null).
                let mut piece = Piece::ref_text(None, None, None);
                let children = self.render_binding_list(&bindings);
                for ch in children {
                    piece.add_html(ch);
                }
                gc.pieces.push(piece);
            }
            row.cells.push(gc);
        }
    }

    /// `AdditionalBindingsRenderer.render(children, list, sd)` (ABR:437-480):
    /// one binding -> inline; many -> a `<ul><li>` list. Each binding renders as
    /// `<a href title>display</a>` (or `<code>` when unlinked) + optional
    /// `: shortDoco` + optional ` (…)` usage. This is the CELL-column path, NOT
    /// the SUMMARY additional-bindings TABLE (render_additional_bindings_table).
    fn render_binding_list(&mut self, list: &[BindingColDetail]) -> Vec<render_xhtml::XhtmlNode> {
        use render_tables::build::Elem;
        if list.len() == 1 {
            // ABR:439 appends directly to the piece's children (no wrapper).
            let mut holder = Elem::new("span"); // transient; we lift its children out
            self.render_one_binding(&mut holder, &list[0]);
            let mut node = holder.build();
            std::mem::take(node.child_nodes_mut())
        } else {
            let mut ul = Elem::new("ul");
            for b in list {
                let mut li = Elem::new("li");
                self.render_one_binding(&mut li, b);
                ul.push_elem(li);
            }
            vec![ul.build()]
        }
    }

    /// `render(children, b, sd)` (ABR:448-480) for one additional binding.
    fn render_one_binding(&mut self, parent: &mut render_tables::build::Elem, b: &BindingColDetail) {
        use render_tables::build::Elem;
        if b.value_set.is_empty() {
            return; // ABR:449 — no valueSet, nothing rendered.
        }
        let br = self.resolve_binding(&b.value_set);
        // ABR:453: ahOrCode(url, title). url==None -> <code>; else <a href>.
        // determineUrl/prependLinks: our resolve_binding already returns the
        // absolute-or-relative webPath the publisher would emit.
        let title = b.documentation.clone().or_else(|| br.uri.clone());
        match &br.url {
            Some(url) => {
                let mut a = Elem::new("a");
                a.set_attr("href", url.clone());
                if let Some(t) = &title {
                    a.set_attr("title", t.clone());
                }
                a.tx(br.display.clone());
                parent.push_elem(a);
            }
            None => {
                // ahOrCode with null url -> <code> (title carried, but code has
                // no href; the title becomes a code with no attr in Java).
                let mut code = Elem::new("code");
                code.tx(br.display.clone());
                parent.push_elem(code);
            }
        }
        if let Some(sd) = &b.short_doco {
            parent.tx(": ");
            parent.tx(sd.clone());
        }
        if b.any || b.has_usage {
            // ABR:463-479: " (…)" — any-repeat marker + usage. Usage rendering
            // needs CodeResolver (not ported); fire a gap if present.
            parent.tx(" (");
            if b.any {
                parent.tx("any repeat");
            }
            if b.has_usage {
                self.gap("genElementBindings: additional-binding usage context (CodeResolver) not ported");
            }
            parent.tx(")");
        }
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
                        row.cells.push(self.gen_cardinality(element, used, None));
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
                            .push(self.gen_cardinality(element, used, Some(&ext_defn)));
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
                row.cells.push(self.gen_cardinality(element, used, None));
                if element.max() == Some("0") {
                    row.cells.push(Cell::new());
                } else {
                    let c = self.gen_types(element, types, root, false);
                    row.cells.push(c);
                }
                let (c, prs) = self.generate_description(element, root, None, None, walks_into_this);
                row.cells.push(c);
                row.sub_rows.extend(prs);
            }
        } else {
            row.cells.push(self.gen_cardinality(element, used, None));
            if element.max() != Some("0") && !types_row {
                let c = self.gen_types(element, types, root, false);
                row.cells.push(c);
            } else {
                row.cells.push(Cell::new());
            }
            let (c, prs) = self.generate_description(element, root, None, None, walks_into_this);
            row.cells.push(c);
            row.sub_rows.extend(prs);
        }
    }

    // `genTypes` (SDR:2317) and `genTargetLink` (SDR:2534) now live in the
    // shared `gentypes::TypesHost` trait (impl below) — the grid path uses the
    // SAME code with must_support_mode=false / pointer=None / dim=false.

    /// The value[x] cell for a simple extension (SDR:1402): genTypes on the
    /// extension's value definition.
    fn gen_types_for_value(&mut self, vd: &ValueDefn, _e: Ed<'a>) -> Cell {
        // genTypes(gen, row, valueDefn, ..., root=false, mustSupport, diff)
        // (SDR:1402): Java passes the VALUE DEFN as `e`, so the mustSupport "S"
        // flags read the value defn's own mustSupport (usually absent) and its
        // types. The value defn's JSON (vd.json) outlives this call but not the
        // borrowed snapshot `'a`; the shared `gen_types` is generic over the
        // element lifetime, so we call it directly (root=false, dim=false — value
        // defn types are never pointer-derived; must_support_mode threads via the
        // host, matching Java's ambient `mustSupport` arg).
        let ed = Ed::new(&vd.json);
        let types = ed.types();
        self.gen_types(ed, &types, false, false)
    }

    /// `makeChoiceRows` (SDR:3362). In mustSupportMode a type is shown iff the
    /// mode is off, no types are MS-marked, or this type is MS (SDR:3376).
    fn make_choice_rows(&mut self, sub_rows: &mut Vec<Row>, element: Ed<'a>, types: &[TypeRef<'a>]) {
        let ms_mode = self.cfg.must_support;
        let all_types_ms = all_types_must_support(types);
        for tr in types {
            if ms_mode && !all_types_ms && !type_is_must_support_full(tr) {
                continue;
            }
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
                // SDR:3393-3396: type-level S before "(".
                if !ms_mode && type_is_must_support(tr) && element.must_support() {
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
                let ctp_all_ms = all_canonicals_must_support(tr, &tr.target_profiles());
                let mut first = true;
                for rt in tr.target_profiles() {
                    // targetProfile MS filter (SDR:3411).
                    if ms_mode && !ctp_all_ms && !canonical_is_must_support(tr, rt) {
                        continue;
                    }
                    if !first {
                        c.pieces.push(Piece::ref_text(None, Some(" | ".into()), None));
                    }
                    // makeChoiceRows renders the element's OWN (restated)
                    // types — never pointer-derived, so no EQUALS dim.
                    self.gen_target_link(&mut c, tr, rt, false);
                    // SDR:3405-3408: per-target S.
                    if !ms_mode && canonical_is_must_support(tr, rt) && element.must_support() {
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
                        if !ms_mode && type_is_must_support(tr) && element.must_support() {
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
                        if !ms_mode && type_is_must_support(tr) && element.must_support() {
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

        // root abstract profile block (SDR@6.9.11:1558-1580)
        if root
            && self
                .sd
                .root
                .get("abstract")
                .and_then(|x| x.as_bool())
                .unwrap_or(false)
        {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let kind_word = if self.sd.derivation() == "constraint" {
                "profile"
            } else {
                "type"
            };
            c.pieces.push(Piece::ref_text(
                None,
                Some(format!("This is an abstract {}. ", kind_word)),
                None,
            ));
            // children: all SDs in context with baseDefinition == this url.
            // QUIRK: Java iterates CanonicalResourceManager.getList() — a
            // HashSet with identity hashCodes — so the publisher's child ORDER
            // is JVM-run-dependent (non-deterministic). We use the IG's own
            // resource order (deterministic); a divergence here is classified
            // as unstable-oracle, not a content bug.
            let children = self.ctx.own_sds_derived_from(self.sd_url());
            if !children.is_empty() {
                c.pieces.push(Piece::ref_text(
                    None,
                    Some(format!(
                        "Child {}: ",
                        if self.sd.derivation() == "constraint" {
                            "profiles"
                        } else {
                            "types"
                        }
                    )),
                    None,
                ));
                let mut first = true;
                for (wp, name) in children {
                    if first {
                        first = false;
                    } else {
                        c.pieces.push(Piece::ref_text(None, Some(", ".into()), None));
                    }
                    c.pieces.push(Piece::ref_text(Some(wp), Some(name), None));
                }
            }
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

        // short (SDR:1585-1603). In diff mode, an element that does not restate
        // `short` shows the FALLBACK's short dimmed (SDR:1594-1602: fallback =
        // the DERIVATION_POINTER at the plain call sites 1396/1417/1426, or the
        // located extension's element at 1409; the piece gets
        // addStyle(getOpacity()) unconditionally). Both branches set
        // `underived` when the short lacks SNAPSHOT_DERIVATION_EQUALS — always
        // true in our reconstruction — which flips the unused-row strike-through
        // to italic (SDR:1054-1062).
        if let Some(short) = definition.short() {
            if !short.is_empty() {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                let mut p = Piece::ref_text(None, Some(short.to_string()), None);
                p.underived = true;
                c.pieces.push(p);
            }
        } else {
            let fb_short: Option<String> = match ext_defn {
                Some(ed) => ed
                    .element
                    .get("short")
                    .and_then(|x| x.as_str())
                    .map(String::from),
                None => self
                    .pointer(definition)
                    .and_then(|p| p.short())
                    .map(String::from),
            };
            if let Some(short) = fb_short {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                let mut p = Piece::ref_text(None, Some(short), None);
                p.add_style(OPACITY);
                p.underived = true;
                c.pieces.push(p);
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
        let binding_from_defn: bool;
        let binding_owner: Option<&serde_json::Value> = match _value_defn {
            Some(vd) => {
                let b = vd.json.get("binding");
                if b.map(|x| x.as_object().map(|o| !o.is_empty()).unwrap_or(false)).unwrap_or(false) {
                    binding_from_defn = false;
                    b
                } else {
                    binding_from_defn = true;
                    definition.binding()
                }
            }
            None => {
                binding_from_defn = true;
                definition.binding()
            }
        };
        // makeUnifiedBinding (SDR:2726-2758): in diff mode the element's
        // binding is merged with its DERIVATION_POINTER's — parts pulled from
        // the base are stamped SNAPSHOT_DERIVATION_EQUALS and render dimmed.
        // A valueDefn never has a pointer (SDR:2727-2729 no-op).
        let mut vs_eq = false;
        let mut str_eq = false;
        let mut desc_eq = false;
        let unified_storage: Option<serde_json::Value> = match binding_owner {
            Some(b) if binding_from_defn => self.pointer(definition).and_then(|p| {
                p.binding().map(|ob| {
                    let mut nb = serde_json::Map::new();
                    if let Some(vs) = b.get("valueSet") {
                        nb.insert("valueSet".into(), vs.clone());
                    } else if let Some(vs) = ob.get("valueSet") {
                        nb.insert("valueSet".into(), vs.clone());
                        vs_eq = true;
                    }
                    if let Some(st) = b.get("strength") {
                        nb.insert("strength".into(), st.clone());
                    } else if let Some(st) = ob.get("strength") {
                        nb.insert("strength".into(), st.clone());
                        str_eq = true;
                    }
                    if let Some(d) = b.get("description") {
                        nb.insert("description".into(), d.clone());
                    } else if let Some(d) = ob.get("description") {
                        nb.insert("description".into(), d.clone());
                        desc_eq = true;
                    }
                    // b.getExtension().addAll(binding.getExtension()) (SDR:2756)
                    if let Some(ext) = b.get("extension") {
                        nb.insert("extension".into(), ext.clone());
                    }
                    serde_json::Value::Object(nb)
                })
            }),
            _ => None,
        };
        let binding_owner: Option<&serde_json::Value> =
            unified_storage.as_ref().or(binding_owner);
        if let Some(binding) = binding_owner {
            if binding.get("valueSet").is_some() {
                self.render_binding_summary(&mut c, definition, binding, vs_eq, str_eq, desc_eq);
            } else if binding.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                // no-valueSet branch (SDR@6.9.11:1987-2003)
                if !c.pieces.is_empty() {
                    let mut br = Piece::tag("br");
                    br.set_class("binding");
                    c.pieces.push(br);
                }
                let mut lbl =
                    Piece::ref_text(None, Some("Binding Description: ".into()), None);
                lbl.set_class("binding");
                lbl.add_style("font-weight:bold");
                c.pieces.push(lbl);
                if let Some(strength) = binding.get("strength").and_then(|x| x.as_str()) {
                    let mut p1 = Piece::ref_text(None, Some(" (".into()), None);
                    p1.set_class("binding");
                    if str_eq {
                        p1.add_style(OPACITY);
                    }
                    c.pieces.push(p1);
                    let mut p2 = Piece::ref_text(
                        Some(format!("{}terminologies.html#{}", self.core_path, strength)),
                        Some(strength.to_string()),
                        Some(strength_definition(strength).to_string()),
                    );
                    p2.set_class("binding");
                    if str_eq {
                        p2.add_style(OPACITY);
                    }
                    c.pieces.push(p2);
                    let mut p3 = Piece::ref_text(None, Some(")".into()), None);
                    p3.set_class("binding");
                    if str_eq {
                        p3.add_style(OPACITY);
                    }
                    c.pieces.push(p3);
                    if matches!(strength, "required" | "extensible") {
                        let mut sp = Piece::ref_text(None, Some(" ".into()), None);
                        sp.set_class("binding");
                        c.pieces.push(sp);
                        let mut warn = Piece::ref_text(
                            None,
                            Some("\u{26A0}".into()),
                            Some("This binding doesn't define a testable ValueSet".into()),
                        );
                        warn.set_class("binding");
                        warn.add_style("font-weight:bold; color: #c97a18");
                        c.pieces.push(warn);
                    }
                }
                let mut sep = Piece::ref_text(None, Some(": ".into()), None);
                sep.set_class("binding");
                c.pieces.push(sep);
                let desc = binding
                    .get("description")
                    .and_then(|x| x.as_str())
                    .filter(|d| !d.contains('\n'));
                match desc {
                    // SDR:2000: style = checkForNoChange(descriptionElement).
                    Some(d) => markdown::add_markdown_no_para_role_styled(
                        &mut c,
                        d,
                        "binding",
                        if desc_eq { Some(OPACITY) } else { None },
                    ),
                    // SDR:2002: no-description phrase, no style.
                    None => markdown::add_markdown_no_para_role(
                        &mut c,
                        "No description provided",
                        "binding",
                    ),
                }
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
                // skipnoValue = mustSupportOnly (SDR:2085): in the MS view,
                // empty pattern properties are suppressed.
                self.gen_fixed_value(
                    &mut partner_rows,
                    ty,
                    v,
                    true,
                    self.cfg.must_support,
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
            // SDR:2786 `if (t.getValues().size() > 0 || snapshot)`: empty
            // properties render only in the SNAPSHOT views; the diff view
            // (snapshot=false) skips them regardless of skipnoValue.
            if values.is_empty() {
                if self.cfg.snapshot && !skip_no_value {
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
                    // Java tests Property.getTypeCode() — the full signature —
                    // so "Reference(X)" is NOT isReference and takes the
                    // datatype icon (SDR:2803-2812).
                    let tc = &prop.type_code_full;
                    if !pattern {
                        card.pieces.push(Piece::ref_text(None, Some("0..0".into()), None));
                        row.set_icon("icon_fixed.gif", Some("Fixed Value:".into()));
                    } else if self.ctx.is_primitive_type(tc) {
                        row.set_icon("icon_primitive.png", Some("Primitive Data Type".into()));
                        card.pieces.push(Piece::ref_text(None, Some(format!("0..{}", prop.max)), None));
                    } else if tc == "Reference" || tc == "canonical" {
                        row.set_icon("icon_reference.png", Some("Reference to another Resource".into()));
                        card.pieces.push(Piece::ref_text(None, Some(format!("0..{}", prop.max)), None));
                    } else {
                        row.set_icon("icon_datatype.gif", Some("Data Type".into()));
                        card.pieces.push(Piece::ref_text(None, Some(format!("0..{}", prop.max)), None));
                    }
                    row.cells.push(card);
                    let mut ty = Cell::new();
                    self.fixed_value_type_cell(&mut ty, tc);
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
                    // b.fhirType(): the value's concrete type — for non-choice
                    // properties that's the single declared type (no parens
                    // in fhirType, so no split branch here) (SDR:2858-2872).
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

    /// genFixedValue type cell (SDR:2815-2829): "Ref(A|B)" splits into linked
    /// pieces; plain codes link via getLinkFor.
    fn fixed_value_type_cell(&mut self, ty: &mut Cell, tc: &str) {
        if let Some(i) = tc.find('(') {
            let tn = &tc[..i];
            let inner = &tc[i + 1..tc.rfind(')').unwrap_or(tc.len())];
            let tn_link = self.ctx.resolve_type(tn).map(|r| r.web_path);
            ty.pieces.push(Piece::ref_text(tn_link, Some(tn.to_string()), None));
            ty.pieces.push(Piece::ref_text(None, Some("(".into()), None));
            for s in inner.split('|') {
                let link = self.ctx.resolve_type(s).map(|r| r.web_path);
                ty.pieces.push(Piece::ref_text(link, Some(s.to_string()), None));
            }
            ty.pieces.push(Piece::ref_text(None, Some(")".into()), None));
        } else {
            let link = self.ctx.resolve_type(tc).map(|r| r.web_path);
            ty.pieces.push(Piece::ref_text(link, Some(tc.to_string()), None));
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

    /// The SUMMARY binding block (SDR:2001-2027, fork spec §7). The `*_eq`
    /// flags are the reconstructed SNAPSHOT_DERIVATION_EQUALS marks from
    /// makeUnifiedBinding (SDR:2741/2747/2753) — checkForNoChange dims the
    /// valueSet piece (SDR:2007), the strength pieces (SDR:2009-2011) and
    /// styles the description markdown (SDR:2015).
    fn render_binding_summary(
        &mut self,
        c: &mut Cell,
        _definition: Ed<'a>,
        binding: &serde_json::Value,
        vs_eq: bool,
        str_eq: bool,
        desc_eq: bool,
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
        if vs_eq {
            p.add_style(OPACITY);
        }
        if br.external {
            p.set_tag_img("external.png");
        }
        c.pieces.push(p);

        if let Some(strength) = binding.get("strength").and_then(|x| x.as_str()) {
            let mut p1 = Piece::ref_text(None, Some(" (".into()), None);
            p1.set_class("binding");
            if str_eq {
                p1.add_style(OPACITY);
            }
            c.pieces.push(p1);
            let mut p2 = Piece::ref_text(
                Some(format!("{}terminologies.html#{}", self.core_path, strength)),
                Some(strength.to_string()),
                Some(strength_definition(strength).to_string()),
            );
            p2.set_class("binding");
            if str_eq {
                p2.add_style(OPACITY);
            }
            c.pieces.push(p2);
            let mut p3 = Piece::ref_text(None, Some(")".into()), None);
            p3.set_class("binding");
            if str_eq {
                p3.add_style(OPACITY);
            }
            c.pieces.push(p3);
        }
        if let Some(desc) = binding.get("description").and_then(|x| x.as_str()) {
            if is_simple_markdown(desc) {
                let mut p = Piece::ref_text(None, Some(": ".into()), None);
                p.set_class("binding");
                c.pieces.push(p);
                markdown::add_markdown_no_para_role_styled(
                    c,
                    desc,
                    "binding",
                    if desc_eq { Some(OPACITY) } else { None },
                );
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
            let link =
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
        self.ctx.resolve_binding(vs_ref)
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

impl<'a> crate::gentypes::TypesHost<'a> for TCtx<'a> {
    fn ctx(&self) -> &IgContext {
        self.ctx
    }
    fn core_path(&self) -> &str {
        self.core_path
    }
    fn sd_root(&self) -> &serde_json::Value {
        &self.sd.root
    }
    fn gap(&mut self, what: &str) {
        self.gaps.push(what.to_string());
    }
    fn pointer(&self, e: Ed<'_>) -> Option<Ed<'a>> {
        if self.cfg.diff {
            self.pointers.get(e.id()).copied()
        } else {
            None
        }
    }
    fn must_support_mode(&self) -> bool {
        self.cfg.must_support
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
    /// `Property.getTypeCode()`: the spec type signature, e.g.
    /// "Reference(Organization)" or "dateTime|Period".
    type_code_full: String,
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
        let mut sigs: Vec<String> = Vec::new();
        for t in ed.types() {
            let mut sig = t.working_code().to_string();
            let targets = t.target_profiles();
            if !targets.is_empty() {
                let tails: Vec<&str> = targets
                    .iter()
                    .map(|u| u.rsplit('/').next().unwrap_or(u))
                    .collect();
                sig = format!("{}({})", sig, tails.join("|"));
            }
            sigs.push(sig);
        }
        out.push(PropDef {
            name,
            type_codes: ed.types().iter().map(|t| t.working_code().to_string()).collect(),
            type_code_full: sigs.join("|"),
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

// ---- BINDINGS mode: scanBindings + collectBindings (SDR:762-832, 1272) ----

/// The two additional-binding container extension urls (R5 tools + R4 shadow).
const EXT_BINDING_ADDITIONAL: &str =
    "http://hl7.org/fhir/tools/StructureDefinition/additional-binding";
const EXT_BINDING_ADDITIONAL_R4: &str =
    "http://hl7.org/fhir/5.0/StructureDefinition/extension-ElementDefinition.binding.additional";
const EXT_MAX_VALUESET: &str =
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-maxValueSet";
const EXT_MIN_VALUESET: &str =
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-minValueSet";

/// `scanBindings(columns, list)` (SDR:762): recurse the element tree collecting
/// the set of column keys, then emit columns in the FIXED order (SDR:765-797).
/// Each Column carries (id=code, title, hint) — the phrase strings.
fn scan_bindings(all: &[Ed<'_>]) -> Vec<render_tables::Column> {
    use std::collections::HashSet;
    let mut cols: HashSet<String> = HashSet::new();
    if let Some(root) = all.first() {
        scan_bindings_rec(all, *root, &mut cols);
    }
    let mut out = Vec::new();
    // The fixed emission order + phrase strings (SDR:765-797). `id` is the code
    // scanBindings/collectBindings key: strengths use the lowercase strength
    // code; purposes use the AdditionalBindingPurpose code.
    let mk = |id: &str, title: &str, hint: &str| render_tables::Column::new(id, title, hint);
    if cols.contains("required") {
        out.push(mk("required", "Required", "Concepts must come from this value set"));
    }
    if cols.contains("extensible") {
        out.push(mk("extensible", "Extensible", "Concepts must come from this value set if appropriate concept is in this value set"));
    }
    if cols.contains("maximum") {
        out.push(mk("maximum", "Maximum", "A required binding, for use when the binding strength is 'extensible' or 'preferred'"));
    }
    if cols.contains("minimum") {
        out.push(mk("minimum", "Minimum", "The minimum allowable value set - any conformant system SHALL support all these codes"));
    }
    if cols.contains("candidate") {
        out.push(mk("candidate", "Candidate", "This value set is a candidate to substitute for the overall conformance value set in some situations; usually these are defined in the documentation"));
    }
    if cols.contains("current") {
        out.push(mk("current", "Current", "New records are required to use this value set, but legacy records may use other codes. The definition of ''new record'' is difficult, since systems often create new records based on pre-existing data. Usually ''current'' bindings are mandated by an external authority that makes clear rules around this"));
    }
    if cols.contains("preferred") {
        out.push(mk("preferred", "Preferred", "This is the value set that is preferred in a given context (documentation should explain why)"));
    }
    if cols.contains("ui") {
        out.push(mk("ui", "UI", "This value set is provided for user look up in a given context. Typically, these valuesets only include a subset of codes relevant for input in a context"));
    }
    if cols.contains("starter") {
        out.push(mk("starter", "Starter", "This value set is a good set of codes to start with when designing your system"));
    }
    if cols.contains("component") {
        out.push(mk("component", "Component", "This value set is a component of the base value set. Usually this is called out so that documentation can be written about a portion of the value set"));
    }
    if cols.contains("example") {
        out.push(mk("example", "Example", "Instances are not expected or even encouraged to draw from the specified value set. The value set merely provides examples of the types of concepts intended to be included."));
    }
    out
}

/// `scanBindings(cols, list, ed)` (SDR:800): add this element's binding strength
/// (as the lowercase strength code) + additional-binding purposes, then recurse.
fn scan_bindings_rec(all: &[Ed<'_>], ed: Ed<'_>, cols: &mut std::collections::HashSet<String>) {
    if let Some(binding) = ed.binding() {
        let vs = binding.get("valueSet").and_then(|x| x.as_str());
        let strength = binding.get("strength").and_then(|x| x.as_str());
        if let (Some(_), Some(s)) = (vs, strength) {
            // SDR:803-818 maps each strength to its LOWERCASE strength phrase
            // (STRUC_DEF_EXAM="example", ...). default (other strengths): none.
            match s {
                "example" => cols.insert("example".into()),
                "extensible" => cols.insert("extensible".into()),
                "preferred" => cols.insert("preferred".into()),
                "required" => cols.insert("required".into()),
                _ => false,
            };
        }
        // additional bindings (native + ext) contribute their purpose code.
        for ab in binding.get("additional").and_then(|x| x.as_array()).into_iter().flatten() {
            if let Some(p) = ab.get("purpose").and_then(|x| x.as_str()) {
                cols.insert(p.to_string());
            }
        }
        for ext in binding.get("extension").and_then(|x| x.as_array()).into_iter().flatten() {
            let url = ext.get("url").and_then(|x| x.as_str()).unwrap_or("");
            if url == EXT_BINDING_ADDITIONAL || url == EXT_BINDING_ADDITIONAL_R4 {
                if let Some(p) = ext_sub_string(ext, "purpose") {
                    cols.insert(p);
                }
            }
        }
    }
    for child in get_children(all, ed) {
        scan_bindings_rec(all, child, cols);
    }
}

/// `scanObligations(columns, list)` (SDR:834): recurse collecting actor ids /
/// `$all`, then emit columns. Since NO IG in this corpus uses obligations, this
/// returns empty in practice; kept faithful (fires a gap if it ever populates).
fn scan_obligations(_ctx: &IgContext, all: &[Ed<'_>]) -> Vec<render_tables::Column> {
    use std::collections::HashSet;
    let mut cols: HashSet<String> = HashSet::new();
    if let Some(root) = all.first() {
        scan_obligations_rec(all, *root, &mut cols);
    }
    // Emission (SDR:838-850): $all first, then actor columns. Actor resolution
    // needs ActorDefinition fetch (not ported). No corpus golden exercises this.
    let mut out = Vec::new();
    if cols.contains("$all") {
        out.push(render_tables::Column::new("$all", "All Actors", "Obligations that apply to all actors"));
    }
    for c in &cols {
        if c != "$all" {
            // GAP: actor-column title/hint need ActorDefinition resolution.
            let tail = c.rsplit('/').next().unwrap_or(c);
            out.push(render_tables::Column::new(c.clone(), tail.to_string(), String::new()));
        }
    }
    out
}

fn scan_obligations_rec(all: &[Ed<'_>], ed: Ed<'_>, cols: &mut std::collections::HashSet<String>) {
    for ob in ed.extensions() {
        let url = ob.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if url == EXT_OBLIGATION_CORE || url == EXT_OBLIGATION_TOOLS {
            let actors: Vec<&str> = ob
                .get("extension")
                .and_then(|x| x.as_array())
                .into_iter()
                .flatten()
                .filter(|e| {
                    matches!(e.get("url").and_then(|u| u.as_str()), Some("actor") | Some("actorId"))
                })
                .filter_map(|e| e.get("valueCanonical").and_then(|x| x.as_str()))
                .collect();
            if actors.is_empty() {
                cols.insert("$all".into());
            } else {
                for a in actors {
                    cols.insert(a.to_string());
                }
            }
        }
    }
    for child in get_children(all, ed) {
        scan_obligations_rec(all, child, cols);
    }
}

/// One binding gathered for a BINDINGS column (a projection of
/// ElementDefinitionBindingAdditionalComponent — the fields ABR:448-480 reads).
pub struct BindingColDetail {
    value_set: String,
    documentation: Option<String>,
    short_doco: Option<String>,
    any: bool,
    has_usage: bool,
}

/// `collectBindings(element, type)` (SDR:1272): gather the bindings whose
/// purpose matches `type` for this element, in the publisher's order —
/// (1) the strength binding itself when its strength code == type (as a
/// synthetic ab with the binding's description + valueSet),
/// (2) maxValueSet ext for "maximum", minValueSet ext for "minimum",
/// (3) native binding.additional filtered by purpose,
/// (4) ext-additional filtered by purpose.
fn collect_bindings(element: Ed<'_>, type_: &str) -> Vec<BindingColDetail> {
    let mut res = Vec::new();
    let Some(b) = element.binding() else { return res };
    let strength = b.get("strength").and_then(|x| x.as_str());
    if let Some(s) = strength {
        if s == type_ {
            res.push(BindingColDetail {
                value_set: b.get("valueSet").and_then(|x| x.as_str()).unwrap_or("").into(),
                documentation: b.get("description").and_then(|x| x.as_str()).map(String::from),
                short_doco: None,
                any: false,
                has_usage: false,
            });
        }
    }
    if type_ == "maximum" {
        if let Some(vs) = ext_value_string(b, EXT_MAX_VALUESET) {
            res.push(BindingColDetail { value_set: vs, documentation: None, short_doco: None, any: false, has_usage: false });
        }
    }
    if type_ == "minimum" {
        if let Some(vs) = ext_value_string(b, EXT_MIN_VALUESET) {
            res.push(BindingColDetail { value_set: vs, documentation: None, short_doco: None, any: false, has_usage: false });
        }
    }
    for ab in b.get("additional").and_then(|x| x.as_array()).into_iter().flatten() {
        if ab.get("purpose").and_then(|x| x.as_str()) == Some(type_) {
            res.push(BindingColDetail {
                value_set: ab.get("valueSet").and_then(|x| x.as_str()).unwrap_or("").into(),
                documentation: ab.get("documentation").and_then(|x| x.as_str()).map(String::from),
                short_doco: ab.get("shortDoco").and_then(|x| x.as_str()).map(String::from),
                any: ab.get("any").and_then(|x| x.as_bool()).unwrap_or(false),
                has_usage: ab.get("usage").and_then(|x| x.as_array()).map(|a| !a.is_empty()).unwrap_or(false),
            });
        }
    }
    for ext in b.get("extension").and_then(|x| x.as_array()).into_iter().flatten() {
        let url = ext.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if url == EXT_BINDING_ADDITIONAL || url == EXT_BINDING_ADDITIONAL_R4 {
            if ext_sub_string(ext, "purpose").as_deref() == Some(type_) {
                res.push(BindingColDetail {
                    value_set: ext_sub_string(ext, "valueSet").unwrap_or_default(),
                    documentation: ext_sub_string(ext, "documentation"),
                    short_doco: ext_sub_string(ext, "shortDoco"),
                    any: ext_sub_bool(ext, "any"),
                    has_usage: ext_sub_present(ext, "usage"),
                });
            }
        }
    }
    res
}

/// Read a nested sub-extension's primitive value[x] as a string.
fn ext_sub_string(ext: &serde_json::Value, name: &str) -> Option<String> {
    ext.get("extension")
        .and_then(|x| x.as_array())
        .and_then(|a| a.iter().find(|s| s.get("url").and_then(|u| u.as_str()) == Some(name)))
        .and_then(|s| {
            for k in ["valueCode", "valueCanonical", "valueUri", "valueString", "valueMarkdown", "valueBoolean"] {
                if let Some(v) = s.get(k) {
                    return v.as_str().map(String::from).or_else(|| v.as_bool().map(|b| b.to_string()));
                }
            }
            None
        })
}

fn ext_sub_bool(ext: &serde_json::Value, name: &str) -> bool {
    ext.get("extension")
        .and_then(|x| x.as_array())
        .and_then(|a| a.iter().find(|s| s.get("url").and_then(|u| u.as_str()) == Some(name)))
        .and_then(|s| s.get("valueBoolean").and_then(|x| x.as_bool()))
        .unwrap_or(false)
}

fn ext_sub_present(ext: &serde_json::Value, name: &str) -> bool {
    ext.get("extension")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().any(|s| s.get("url").and_then(|u| u.as_str()) == Some(name)))
        .unwrap_or(false)
}

/// A top-level extension's value[x] as a string (for max/minValueSet).
fn ext_value_string(binding: &serde_json::Value, url: &str) -> Option<String> {
    binding
        .get("extension")
        .and_then(|x| x.as_array())
        .and_then(|a| a.iter().find(|e| e.get("url").and_then(|u| u.as_str()) == Some(url)))
        .and_then(|e| {
            e.get("valueCanonical")
                .or_else(|| e.get("valueUri"))
                .or_else(|| e.get("valueString"))
                .and_then(|x| x.as_str())
                .map(String::from)
        })
}

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

/// Candidate un-renamings of a diff element id whose choice segments were
/// concretized (`valueQuantity` -> `value[x]`): for every camelCase boundary in
/// every segment, propose the `stem[x]` rewrite (checked against the snapshot
/// id set by the caller). Segments with a slice marker keep their slice.
/// Public wrapper for the shared summary nested-split pointer reconstruction.
pub fn dechoice_candidates_pub(id: &str) -> Vec<String> {
    dechoice_candidates(id)
}

fn dechoice_candidates(id: &str) -> Vec<String> {
    let segs: Vec<&str> = id.split('.').collect();
    let mut out = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        if seg.contains(':') || seg.contains("[x]") {
            continue;
        }
        for (j, ch) in seg.char_indices().skip(1) {
            if ch.is_ascii_uppercase() {
                let mut v: Vec<String> = segs.iter().map(|s| s.to_string()).collect();
                v[i] = format!("{}[x]", &seg[..j]);
                out.push(v.join("."));
            }
        }
    }
    out
}

/// checkForNoChange (SDR:2305-2310): add `opacity: 0.5` when the source
/// carries SNAPSHOT_DERIVATION_EQUALS (reconstructed as a bool here).
pub(crate) fn dim_piece(mut p: Piece, dim: bool) -> Piece {
    if dim {
        p.add_style(OPACITY);
    }
    p
}

pub fn is_profiled_type(profiles: &[&str]) -> bool {
    profiles.iter().any(|p| p.contains(':'))
}

/// `getKeyElements()` (publisher SDR:532). If the profile is a non-logical
/// constraint, the elements are filtered to the "key" set (scanForKeyElements);
/// otherwise ALL snapshot elements are returned. Elements are returned as
/// copies (no clearing — byKey keeps binding/constraint intact).
fn key_elements(sd: &Sd, ctx: &IgContext) -> Vec<serde_json::Value> {
    let elems = sd.snapshot_elements();
    let key_eligible = sd.derivation() == "constraint" && !sd.is_logical();
    let mut key_set: std::collections::HashSet<usize> = std::collections::HashSet::new();
    if key_eligible {
        let ms = must_support_id_set(sd);
        let differential = differential_id_set(sd);
        if let Some(root) = elems.first() {
            scan_for_key_elements(&elems, *root, &ms, &differential, ctx, &mut key_set);
        }
    }
    let mut out = Vec::new();
    for (i, ed) in elems.iter().enumerate() {
        if !key_eligible || key_set.contains(&i) {
            out.push(ed.v.clone());
        }
    }
    out
}

/// The differential-id set (publisher getDifferential): every differential
/// element id plus its ancestor ids (marked present with null).
fn differential_id_set(sd: &Sd) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    if let Some(diff) = sd
        .root
        .get("differential")
        .and_then(|d| d.get("element"))
        .and_then(|e| e.as_array())
    {
        for e in diff {
            if let Some(id) = e.get("id").and_then(|x| x.as_str()) {
                set.insert(id.to_string());
            }
        }
    }
    set
}

const SIGNIFICANT_EXTENSIONS: &[&str] = &[
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-allowedUnits",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-bestPractice",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-graphConstraint",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-maxDecimalPlaces",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-maxSize",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-mimeType",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-minLength",
    "http://hl7.org/fhir/StructureDefinition/elementdefinition-obligation",
];

/// `scanForKeyElements` (publisher SDR:685). Adds `element`, then for each child
/// computes the oldMS/newMS predicate (comparing against the base type element)
/// and recurses when true.
fn scan_for_key_elements(
    all: &[Ed<'_>],
    element: Ed<'_>,
    ms: &std::collections::HashSet<String>,
    differential: &std::collections::HashSet<String>,
    ctx: &IgContext,
    key_set: &mut std::collections::HashSet<usize>,
) {
    if let Some(idx) = all.iter().position(|e| std::ptr::eq(e.v, element.v)) {
        key_set.insert(idx);
    }
    for child in get_children(all, element) {
        if child_is_key(child, ms, differential, ctx) {
            scan_for_key_elements(all, child, ms, differential, ctx, key_set);
        }
    }
}

/// The `oldMS || newMS` key predicate (publisher SDR:730-764).
fn child_is_key(
    child: Ed<'_>,
    ms: &std::collections::HashSet<String>,
    differential: &std::collections::HashSet<String>,
    ctx: &IgContext,
) -> bool {
    // significant extensions present?
    let has_sig_ext = child
        .extensions()
        .iter()
        .any(|e| {
            e.get("url")
                .and_then(|x| x.as_str())
                .map(|u| SIGNIFICANT_EXTENSIONS.contains(&u))
                .unwrap_or(false)
        });
    // base type element lookup (by base.path).
    let base_path = child.base().and_then(|b| b.get("path")).and_then(|x| x.as_str());
    let base_element = base_path.and_then(|bp| lookup_base_element(bp, ctx));

    // bindingChanged (SDR:731-751)
    let mut binding_changed = false;
    if let Some(be) = &base_element {
        let base_binding = be.get("binding");
        if base_binding.is_none() {
            binding_changed = true;
        } else if let Some(binding) = child.binding() {
            let bb = base_binding.unwrap();
            let strength = binding.get("strength").and_then(|x| x.as_str());
            let has_vs = binding.get("valueSet").is_some();
            if has_vs && matches!(strength, Some("required") | Some("extensible")) {
                let base_strength = bb.get("strength").and_then(|x| x.as_str());
                if base_strength.is_none() || base_strength != strength {
                    binding_changed = true;
                } else {
                    let base_vs = bb.get("valueSet").and_then(|x| x.as_str());
                    let vs = binding.get("valueSet").and_then(|x| x.as_str());
                    if base_vs.is_none() || base_vs != vs {
                        binding_changed = true;
                    }
                }
            }
            // additionalBindings comparison in the publisher compares a value to
            // itself (a no-op bug: getAdditional(binding.getAdditional()) twice),
            // so it never flips bindingChanged. Reproduced by omission.
        }
    }
    let _ = binding_changed; // folded into oldMS/newMS below via the child flags.

    let child_min = child.min();
    let base_min = base_element
        .as_ref()
        .and_then(|b| b.get("min"))
        .and_then(|x| x.as_i64());
    let child_max = child.max();
    let base_max = child.base().and_then(|b| b.get("max")).and_then(|x| x.as_str());

    let old_ms = ms.contains(child.id())
        || child_min.map(|m| m != 0).unwrap_or(false)
        || (child.conditions().len() > 1)
        || child.is_modifier()
        || (child.has_slicing()
            && !child.path().ends_with(".extension")
            && !child.path().ends_with(".modifierExtension"))
        || child.has_slice_name()
        || differential.contains(child.id())
        || (child_max != base_max);

    let new_ms = (child_min != base_min)
        || child.fixed().is_some()
        || child.pattern().is_some()
        || has_min_max_value_change(child, base_element.as_ref(), "minValue")
        || has_min_max_value_change(child, base_element.as_ref(), "maxValue")
        || has_max_length_change(child, base_element.as_ref())
        || child.must_have_value()
        || child.has_extension("http://hl7.org/fhir/StructureDefinition/elementdefinition-value-alternatives")
        || has_sig_ext;

    old_ms || new_ms
}

/// Load the base type's snapshot element for a base path (e.g. "Observation.code"
/// → the core Observation SD's element with that path).
fn lookup_base_element(base_path: &str, ctx: &IgContext) -> Option<serde_json::Value> {
    let type_name = base_path.split('.').next()?;
    let url = format!("http://hl7.org/fhir/StructureDefinition/{}", type_name);
    let sd = ctx.load_resource(&url)?;
    let elems = sd.get("snapshot")?.get("element")?.as_array()?;
    elems
        .iter()
        .find(|e| e.get("path").and_then(|x| x.as_str()) == Some(base_path))
        .cloned()
}

fn has_min_max_value_change(child: Ed<'_>, base: Option<&serde_json::Value>, kind: &str) -> bool {
    let has_child = child
        .v
        .as_object()
        .map(|o| o.keys().any(|k| k.starts_with(kind)))
        .unwrap_or(false);
    if !has_child {
        return false;
    }
    let cv = child
        .v
        .as_object()
        .and_then(|o| o.iter().find(|(k, _)| k.starts_with(kind)).map(|(_, v)| v));
    let bv = base
        .and_then(|b| b.as_object())
        .and_then(|o| o.iter().find(|(k, _)| k.starts_with(kind)).map(|(_, v)| v));
    match (cv, bv) {
        (Some(_), None) => true,
        (Some(c), Some(b)) => c != b,
        _ => false,
    }
}

fn has_max_length_change(child: Ed<'_>, base: Option<&serde_json::Value>) -> bool {
    let Some(cl) = child.max_length() else { return false };
    let _ = cl;
    if child.max_length().is_none() {
        return false;
    }
    let bl = base
        .and_then(|b| b.get("maxLength"))
        .and_then(|x| x.as_i64());
    match bl {
        None => true,
        Some(b) => child.max_length() != Some(b),
    }
}

/// The set of element ids in the `getMustSupport()` map (publisher SDR:602 →
/// scanForMustSupport): every MS element plus all its ancestors. The root
/// (empty parent list) is always included.
fn must_support_id_set(sd: &Sd) -> std::collections::HashSet<String> {
    let elems = sd.snapshot_elements();
    let mut set = std::collections::HashSet::new();
    // scanForMustSupport(element, parents): if parents empty OR element is MS,
    // add element + all parents. Recurse into getChildren.
    fn scan(
        all: &[Ed<'_>],
        element: Ed<'_>,
        parents: &[Ed<'_>],
        set: &mut std::collections::HashSet<String>,
    ) {
        if parents.is_empty() || (element.has_must_support() && element.must_support()) {
            set.insert(element.id().to_string());
            for p in parents {
                set.insert(p.id().to_string());
            }
        }
        let children = get_children(all, element);
        for child in children {
            let mut np: Vec<Ed> = parents.to_vec();
            np.push(element);
            scan(all, child, &np, set);
        }
    }
    if let Some(first) = elems.first() {
        scan(&elems, *first, &[], &mut set);
    }
    set
}

/// `getMustSupportElements()` (publisher SDR:562): the snapshot elements whose
/// id is in the MS set, each COPIED with example cleared and — for non-MS
/// elements below the root — binding/constraint cleared (render_opaque dimming
/// handled separately). mustSupport flag itself is cleared on every copy.
fn must_support_elements(sd: &Sd) -> Vec<serde_json::Value> {
    let ms = must_support_id_set(sd);
    let elems = sd.snapshot_elements();
    let mut out = Vec::new();
    for ed in &elems {
        if !ms.contains(ed.id()) {
            continue;
        }
        let mut copy = ed.v.clone();
        let obj = copy.as_object_mut().unwrap();
        obj.remove("example");
        let is_ms = ed.has_must_support() && ed.must_support();
        if !is_ms {
            // render_opaque is gated on path.contains(".") (owned_opaque_ids),
            // but binding + constraint are cleared for ALL non-MS copies
            // (SDR:574-577), including the root.
            obj.remove("binding");
            obj.remove("constraint");
        }
        obj.remove("mustSupport");
        obj.insert("mustSupport".into(), serde_json::Value::Bool(false));
        out.push(copy);
    }
    out
}

/// The ids that get `render_opaque` in the MS view (SDR:574): non-MS elements
/// below the root that are in the MS set (kept as ancestors of MS elements).
fn owned_opaque_ids(sd: &Sd) -> std::collections::HashSet<String> {
    let ms = must_support_id_set(sd);
    let elems = sd.snapshot_elements();
    let mut out = std::collections::HashSet::new();
    for ed in &elems {
        if !ms.contains(ed.id()) {
            continue;
        }
        let is_ms = ed.has_must_support() && ed.must_support();
        if !is_ms && ed.path().contains('.') {
            out.insert(ed.id().to_string());
        }
    }
    out
}

/// `isMustSupport(TypeRefComponent)` (SDR): the type ext is true, OR any
/// profile/targetProfile canonical carries the type-must-support ext.
pub(crate) fn type_is_must_support_full(t: &TypeRef<'_>) -> bool {
    if type_is_must_support(t) {
        return true;
    }
    for u in t.profiles() {
        if canonical_is_must_support(t, u) {
            return true;
        }
    }
    for u in t.target_profiles() {
        if canonical_is_must_support(t, u) {
            return true;
        }
    }
    false
}

/// `allProfilesMustSupport(profiles)` (SDR): returns true when NO canonical in
/// the list is MS-marked (`!all && !any`).
pub(crate) fn all_canonicals_must_support(t: &TypeRef<'_>, canonicals: &[&str]) -> bool {
    let mut all = true;
    let mut any = false;
    for u in canonicals {
        let ms = canonical_is_must_support(t, u);
        all = all && ms;
        any = any || ms;
    }
    !all && !any
}

/// `allTypesMustSupport(e)` (SDR): returns true when NO type is MS-marked
/// (`!all && !any`) — the "the MS filter shouldn't apply" case.
pub(crate) fn all_types_must_support(types: &[TypeRef<'_>]) -> bool {
    let mut all = true;
    let mut any = false;
    for t in types {
        let ms = type_is_must_support_full(t);
        all = all && ms;
        any = any || ms;
    }
    !all && !any
}

/// isMustSupportDirect(t)/isMustSupport(t): the type carries the
/// elementdefinition-type-must-support extension = true.
pub fn type_is_must_support(t: &TypeRef<'_>) -> bool {
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
pub fn canonical_is_must_support(t: &TypeRef<'_>, u: &str) -> bool {
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
pub fn build_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

pub fn is_primitive_value(v: &serde_json::Value) -> bool {
    !matches!(v, serde_json::Value::Object(_) | serde_json::Value::Array(_))
}

/// `describeSlice` (SDR:3514): "{Ordered|Unordered}, {rules} by {discriminators}".
pub fn describe_slice(slicing: &serde_json::Value) -> String {
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
pub fn strength_definition(code: &str) -> &'static str {
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

/// `MarkDownProcessor.isSimpleMarkdown` — a description with no markdown block
/// structure. Conservative approximation aligned with the plain-prose test.
pub fn is_simple_markdown(s: &str) -> bool {
    !s.contains('\n')
}

/// sd.getTypeName() for a resolved type (name field of the SD).
pub fn type_name_of(sd: &Resolved, fallback: &str) -> String {
    sd.name.clone().unwrap_or_else(|| fallback.to_string())
}

fn capitalize(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
        None => String::new(),
    }
}

/// Public wrapper for `key_elements` (getKeyElements) — reused by leaf.rs
/// (inv-key, tx-key). Same semantics as byKey.
pub fn key_elements_pub(sd: &Sd, ctx: &IgContext) -> Vec<serde_json::Value> {
    key_elements(sd, ctx)
}

/// Public wrapper for `must_support_elements` (getMustSupportElements) — reused
/// by leaf.rs (dict-ms). NOTE takes no ctx (matches the private fn).
pub fn must_support_elements_pub(sd: &Sd, _ctx: &IgContext) -> Vec<serde_json::Value> {
    must_support_elements(sd)
}
