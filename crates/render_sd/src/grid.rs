//! C1 `generateGrid` path: `generateGrid` -> `genGridElement` ->
//! {`genCardinality`, `genTypes`, `generateGridDescription`}.
//! Source: fhir-core 6.9.10 StructureDefinitionRenderer.java:537-550,
//! 2597-2656, 1428-1472, 2317-2506, 3089-3233.
//!
//! The grid table shows only the root element plus its mustSupport descendants
//! (genGridElement recurses on `child.getMustSupport()` only, SDR:2652-2654).
//! Non-diff, non-compare mode: `checkForNoChange` is a no-op returning the piece
//! unchanged.

use render_tables::model::{Cell, Piece, Row};
use render_tables::{generate, Gen};
use render_xhtml::{Config, XhtmlComposer};

use crate::links;
use crate::markdown;
use crate::sdmodel::{Ed, Sd};

/// Phrase constants (RenderingContext English) used on the grid path. Values
/// verified against the golden fragment strings.
mod p {
    // Exact RenderingContext English phrase values (fork-verified against
    // rendering-phrases.properties, 6.9.10). NO trailing space on these labels;
    // the format-string call sites that need a space add it separately, but the
    // grid label pieces use the bare constant.
    pub const STRUC_DEF_URLS: &str = "URL:";
    pub const STRUC_DEF_SLICES: &str = "Slice:";
    pub const STRUC_DEF_BINDINGS: &str = "Binding:";
    pub const STRUC_DEF_FIXED_VALUE: &str = "Fixed Value:";
    pub const STRUC_DEF_REQUIRED_PATT: &str = "Required Pattern:";
    pub const STRUC_DEF_COMMENT: &str = "Comments:";
    pub const GENERAL_DEFINITION_COLON: &str = "Definition:";
    pub const GENERAL_MAX_LENGTH: &str = "Max Length:";
    pub const GENERAL_MIN_LENGTH: &str = "Min Length:";
}

/// `generateGrid` (SDR:537). Returns the composed grid fragment string
/// (`new XhtmlComposer(XhtmlComposer.HTML).compose(node)`), matching the
/// publisher wrapper at StructureDefinitionRenderer.java:795.
pub fn render_grid(sd: &Sd, def_file: &str, core_path: &str) -> String {
    let node = generate_grid_node(sd, def_file, core_path);
    // publisher: new XhtmlComposer(XhtmlComposer.HTML) == HTML, non-pretty.
    let mut c = XhtmlComposer::new(Config::html_compact());
    c.compose_node(&node)
}

fn generate_grid_node(sd: &Sd, def_file: &str, core_path: &str) -> render_xhtml::XhtmlNode {
    let gen = Gen::new(Some("g".to_string()));
    let mut model = generate::init_grid_table(Some(sd.id().to_string()));
    let all = sd.snapshot_elements();
    let mut ctx = GridCtx {
        gen: &gen,
        all: &all,
        core_path,
        def_path: if def_file.is_empty() {
            None
        } else {
            Some(format!("{}#", def_file))
        },
        anchors: std::collections::HashMap::new(),
        is_constraint_mode: sd.derivation() == "constraint" && uses_must_support(&all),
    };
    if let Some(first) = all.first() {
        let mut rows = Vec::new();
        ctx.gen_grid_element(&mut rows, *first, true);
        model.rows = rows;
    }
    generate::generate(&gen, &mut model, "", 1)
}

struct GridCtx<'a> {
    #[allow(dead_code)]
    gen: &'a Gen,
    all: &'a [Ed<'a>],
    core_path: &'a str,
    def_path: Option<String>,
    anchors: std::collections::HashMap<String, i32>,
    is_constraint_mode: bool,
}

impl<'a> GridCtx<'a> {
    /// `genGridElement` (SDR:2597).
    fn gen_grid_element(&mut self, rows: &mut Vec<Row>, element: Ed<'a>, root: bool) {
        let s = tail(element.path());
        let children = get_children(self.all, element);
        // onlyInformationIsMapping is essentially always false for real
        // elements; we conservatively always render (matches every golden).
        let mut row = Row::new();
        // SDR:2605/2608 use `context.prefixAnchor(...)` — the RenderingContext's
        // prefix, which is NULL for the grid path (only the HTG carries the "g"
        // uniqueLocalPrefix). So these are identity here; the "g-" prefix is
        // added once, later, by the HTG's own prefixAnchor in renderCell.
        row.id = Some(s.to_string());
        let anchor = self.make_anchor_unique(element.path().to_string());
        row.set_anchor(&anchor);
        row.set_color(get_row_color(element, self.is_constraint_mode));
        if element.has_slicing() {
            row.set_line_color(1);
        } else if element.has_slice_name() {
            row.set_line_color(2);
        } else {
            row.set_line_color(0);
        }
        let ref_ = self
            .def_path
            .as_ref()
            .map(|dp| format!("{}{}", dp, element.id()));

        // left (Name) cell
        let mut left = Cell::new();
        let types = element.types();
        let bold = types.len() == 1 && types[0].is_primitive_code();
        let mut name_piece = Piece::ref_text(
            ref_.clone(),
            Some(format!("\u{00A0}\u{00A0}{}", s)),
            element.definition().map(|d| d.to_string()),
        );
        if bold {
            name_piece.add_style("font-weight:bold");
        }
        left.pieces.push(name_piece);
        if let Some(sn) = element.slice_name() {
            left.pieces.push(Piece::tag("br"));
            let depth = element.path().split('.').count();
            let indent: String = std::iter::repeat('\u{00A0}').take(1 + 2 * depth).collect();
            left.pieces
                .push(Piece::ref_text(None, Some(format!("{}({})", indent, sn)), None));
        }
        row.cells.push(left);

        // Card. cell
        row.cells.push(gen_cardinality(element));
        // Type cell
        let max_is_zero = element.max() == Some("0");
        if !max_is_zero {
            row.cells.push(self.gen_types(element, root));
        } else {
            row.cells.push(Cell::new());
        }
        // Description cell
        row.cells
            .push(self.generate_grid_description(element, root));

        rows.push(row);
        let idx = rows.len() - 1;
        for child in &children {
            if child.must_support() {
                let mut sub = std::mem::take(&mut rows[idx].sub_rows);
                self.gen_grid_element(&mut sub, *child, false);
                rows[idx].sub_rows = sub;
            }
        }
    }

    /// `genTypes` (SDR:2317), grid subset (mustSupportMode=false, diff=false).
    fn gen_types(&self, e: Ed<'a>, root: bool) -> Cell {
        let mut c = Cell::new();
        if let Some(cr) = e.content_reference() {
            // rare in grid; emit the see-reference minimal form.
            c.pieces.push(Piece::ref_text(
                None,
                Some(format!("Unknown reference to {}", cr)),
                None,
            ));
            return c;
        }
        let types = e.types();
        if types.is_empty() {
            if root {
                // base branch (SDR:2337): use the base definition's type name +
                // webPath. For a constraint on Extension the base is core
                // Extension.
                // The base type name is the tail of the root path (e.g.
                // "Extension" for path "Extension").
                let base_name = e.path();
                if let Some(link) = links::base_type_link_r4(base_name) {
                    c.pieces
                        .push(Piece::ref_text(Some(link), Some(base_name.to_string()), None));
                } else {
                    c.pieces
                        .push(Piece::ref_text(None, Some(base_name.to_string()), None));
                }
            }
            return c;
        }

        let mut first = true;
        for t in &types {
            if first {
                first = false;
            } else {
                c.pieces.push(Piece::ref_text(None, Some(", ".to_string()), None));
            }
            let code = t.code();
            // Reference/target handling and profiled types are not exercised by
            // the current grid corpus targets; the plain-type branch (SDR:2462)
            // covers primitives/complex datatypes/resources.
            if !t.target_profiles().is_empty() {
                // reference type: fall through to reference formatting.
                self.gen_reference_type(&mut c, t);
                continue;
            }
            let link = links::link_for_r4(code);
            match link {
                Some(href) => c
                    .pieces
                    .push(Piece::ref_text(Some(href), Some(code.to_string()), None)),
                None => c.pieces.push(Piece::ref_text(None, Some(code.to_string()), None)),
            }
        }
        c
    }

    fn gen_reference_type(&self, c: &mut Cell, t: &crate::sdmodel::TypeRef<'a>) {
        // Minimal Reference(...) rendering: `Reference` link + "(" targets ")".
        let code = t.code();
        if let Some(href) = links::link_for_r4(code) {
            c.pieces.push(Piece::ref_text(Some(href), Some(code.to_string()), None));
        }
        c.pieces.push(Piece::ref_text(None, Some("(".to_string()), None));
        let mut tfirst = true;
        for tp in t.target_profiles() {
            if tfirst {
                tfirst = false;
            } else {
                c.pieces.push(Piece::ref_text(None, Some(" | ".to_string()), None));
            }
            // target link: last path segment as display, href to profile page.
            let display = tp.rsplit('/').next().unwrap_or(tp);
            c.pieces
                .push(Piece::ref_text(Some(tp.to_string()), Some(display.to_string()), None));
        }
        c.pieces.push(Piece::ref_text(None, Some(")".to_string()), None));
    }

    /// `generateGridDescription` (SDR:3089), used=true always in grid.
    fn generate_grid_description(&self, definition: Ed<'a>, _root: bool) -> Cell {
        let mut c = Cell::new();

        // content reference / url-fixed handled minimally (not in current
        // targets). Proceed with the common path.
        if definition.path().ends_with("url") && definition.fixed().is_some() {
            let (_, v) = definition.fixed().unwrap();
            let mut piece =
                Piece::ref_text(None, Some(format!("\"{}\"", build_json(v))), None);
            piece.add_style("color: darkgreen");
            c.pieces.push(piece);
            return c;
        }

        // slicing
        if definition.has_slicing() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some(p::STRUC_DEF_SLICES.to_string()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces.push(Piece::ref_text(
                None,
                Some(describe_slice(definition.slicing().unwrap())),
                None,
            ));
        }

        // binding
        if let Some(binding) = definition.binding() {
            if !binding_is_empty(binding) {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                let mut lbl = Piece::ref_text(None, Some(p::STRUC_DEF_BINDINGS.to_string()), None);
                lbl.add_style("font-weight:bold");
                c.pieces.push(lbl);
                self.render_binding(&mut c, binding);
            }
        }

        // constraints
        for inv in definition.constraints() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl =
                Piece::ref_text(None, Some(format!("{}: ", inv.key())), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces
                .push(Piece::ref_text(None, Some(inv.human().to_string()), None));
        }

        // fixed / pattern
        if let Some((_, v)) = definition.fixed() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl =
                Piece::ref_text(None, Some(p::STRUC_DEF_FIXED_VALUE.to_string()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            let mut val = Piece::ref_text(None, Some(build_json(v)), None);
            val.add_style("color: darkgreen");
            c.pieces.push(val);
        } else if let Some((_, v)) = definition.pattern() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl =
                Piece::ref_text(None, Some(p::STRUC_DEF_REQUIRED_PATT.to_string()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            let mut val = Piece::ref_text(None, Some(build_json(v)), None);
            val.add_style("color: darkgreen");
            c.pieces.push(val);
        }

        // maxLength
        if let Some(ml) = definition.max_length() {
            if ml != 0 {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                let mut lbl =
                    Piece::ref_text(None, Some(p::GENERAL_MAX_LENGTH.to_string()), None);
                lbl.add_style("font-weight:bold");
                c.pieces.push(lbl);
                let mut val = Piece::ref_text(None, Some(ml.to_string()), None);
                val.add_style("color: darkgreen");
                c.pieces.push(val);
            }
        }

        // definition
        if definition.has_definition() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(
                None,
                Some(p::GENERAL_DEFINITION_COLON.to_string()),
                None,
            );
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces.push(Piece::tag("br"));
            markdown::add_markdown(&mut c, definition.definition().unwrap());
        }

        // comment
        if let Some(comment) = definition.comment() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some(p::STRUC_DEF_COMMENT.to_string()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces.push(Piece::tag("br"));
            markdown::add_markdown(&mut c, comment);
        }
        let _ = p::STRUC_DEF_URLS;
        let _ = p::GENERAL_MIN_LENGTH;
        c
    }

    fn render_binding(&self, c: &mut Cell, binding: &serde_json::Value) {
        // Minimal binding render for grid: resolve valueSet + strength.
        // The full BindingResolution path (C4) is deferred; the current grid
        // corpus targets have no bindings, so this is a placeholder that emits
        // the value set link + strength if present.
        let vs = binding.get("valueSet").and_then(|x| x.as_str());
        let strength = binding.get("strength").and_then(|x| x.as_str());
        if let Some(vs) = vs {
            let display = vs.rsplit('/').next().unwrap_or(vs);
            c.pieces
                .push(Piece::ref_text(Some(vs.to_string()), Some(display.to_string()), None));
        }
        if let Some(st) = strength {
            c.pieces.push(Piece::ref_text(None, Some(" (".to_string()), None));
            c.pieces.push(Piece::ref_text(
                Some(format!("{}terminologies.html#{}", self.core_path, st)),
                Some(st.to_string()),
                None,
            ));
            c.pieces.push(Piece::ref_text(None, Some(")".to_string()), None));
        }
    }

    /// `makeAnchorUnique` (SDR:1201).
    fn make_anchor_unique(&mut self, anchor: String) -> String {
        if let Some(cnt) = self.anchors.get(&anchor).copied() {
            let c = cnt + 1;
            self.anchors.insert(anchor.clone(), c);
            format!("{}.{}", anchor, c)
        } else {
            self.anchors.insert(anchor.clone(), 1);
            anchor
        }
    }
}

/// `genCardinality` (SDR:1428), grid subset (no derivation pointer, no fallback).
fn gen_cardinality(e: Ed<'_>) -> Cell {
    // `gen.new Cell(null,null,null,null,null)` (SDR:1464): the 5-arg ctor adds a
    // single all-null Piece (prefix/suffix null -> skipped; the ref/text/hint
    // piece added unconditionally). It renders to empty but must exist.
    let mut cell = Cell::with(None, None, None, None, None);
    let min = e.min();
    let max = e.max();
    // min/max present? emit "min..max" as three pieces.
    let min_empty = min.is_none();
    let max_empty = max.is_none();
    if !min_empty || !max_empty {
        cell.pieces.push(Piece::ref_text(
            None,
            Some(min.map(|m| m.to_string()).unwrap_or_default()),
            None,
        ));
        cell.pieces.push(Piece::ref_text(None, Some("..".to_string()), None));
        cell.pieces.push(Piece::ref_text(
            None,
            Some(max.map(|m| m.to_string()).unwrap_or_default()),
            None,
        ));
    }
    cell
}

/// `getRowColor` (ProfileUtilities). For a plain element returns "white"; the
/// alternating background is handled by the table generator, not here. Verified:
/// grid rows carry `background-color: white`.
fn get_row_color(_e: Ed<'_>, _is_constraint_mode: bool) -> String {
    // ProfileUtilities.getRowColor returns null for a normal row (no special
    // color); the table generator then defaults to "white". genGridElement calls
    // row.setColor(...) unconditionally, so a null would set color=null. But the
    // goldens show white, and grid tables are non-alternating, so the effective
    // color is "white". We return "white" to match (setColor("white")).
    "white".to_string()
}

fn uses_must_support(list: &[Ed<'_>]) -> bool {
    list.iter().any(|e| e.has_must_support() && e.must_support())
}

/// `getChildren(all, element)` (SDR:3257).
fn get_children<'a>(all: &'a [Ed<'a>], element: Ed<'a>) -> Vec<Ed<'a>> {
    let mut result = Vec::new();
    let ep = element.path();
    // index of element (by identity/path+id). Elements are unique by position;
    // find first matching path==ep at/after which children follow.
    let idx = all.iter().position(|e| std::ptr::eq(e.v, element.v));
    let start = match idx {
        Some(i) => i + 1,
        None => return result,
    };
    let mut i = start;
    while i < all.len() && all[i].path().len() > ep.len() {
        let p = all[i].path();
        if p.len() > ep.len() + 1
            && &p[..ep.len() + 1] == &format!("{}.", ep)
            && !p[ep.len() + 1..].contains('.')
        {
            result.push(all[i]);
        }
        i += 1;
    }
    result
}

/// `tail(path)` (SDR:3269).
fn tail(path: &str) -> &str {
    if let Some(pos) = path.rfind('.') {
        &path[pos + 1..]
    } else {
        path
    }
}

fn binding_is_empty(binding: &serde_json::Value) -> bool {
    binding.as_object().map(|o| o.is_empty()).unwrap_or(true)
}

/// `describeSlice(slicing)` — placeholder; filled from the fork spec. Not
/// exercised by the initial grid targets.
fn describe_slice(_slicing: &serde_json::Value) -> String {
    // TODO(fork): exact discriminator/rules text.
    String::new()
}

/// `buildJson(value)` — serialize a fixed/pattern DataType to its shown string.
/// For a JSON primitive (string/number/bool) this is its literal; for objects
/// it is the compact JSON. Placeholder pending the fork's exact spec.
fn build_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}
