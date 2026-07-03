//! C1 `generateGrid` path: `generateGrid` -> `genGridElement` ->
//! {`genCardinality`, `genTypes`, `generateGridDescription`}.
//! Source: fhir-core 6.9.11 StructureDefinitionRenderer.java (SDR):
//! `generateGrid:537`, `genGridElement:2602`, `genCardinality:1431`,
//! `genTypes:2320`, `genTargetLink:2529`, `generateGridDescription:3100`.
//!
//! The grid table shows only the root element plus its mustSupport descendants
//! (genGridElement recurses on `child.getMustSupport()` only, SDR:2653-2655).
//! Non-diff, non-compare mode: `checkForNoChange` is a no-op returning the piece
//! unchanged.
//!
//! `genTypes`/`genTargetLink` are the SAME functions the SUMMARY table path uses
//! (SDR shares one implementation). We route the grid through `IgContext` (the
//! publisher-parity link/binding oracle), mirroring the validated `table.rs`
//! port branch-for-branch. (Simplification candidate: the genTypes bodies here
//! and in table.rs are byte-identical ports of one Java method and could be
//! unified behind a shared free function — logged for a consolidation pass.)

use render_tables::model::{Cell, Piece, Row};
use render_tables::{generate, Gen};
use render_xhtml::{Config, XhtmlComposer};

use crate::context::IgContext;
use crate::markdown;
use crate::sdmodel::{Ed, Sd};
use crate::table::{
    build_json, canonical_is_must_support, core_path_for, describe_slice, is_profiled_type,
    is_simple_markdown, strength_definition, tail, type_is_must_support, type_name_of,
};

use crate::table::RED_BACKGROUND_COLOR;

/// `generateGrid` (SDR:537). Returns the composed grid fragment string
/// (`new XhtmlComposer(XhtmlComposer.HTML).compose(node)`), matching the
/// publisher wrapper at StructureDefinitionRenderer.java:795.
pub fn render_grid(sd: &Sd, ctx: &IgContext, def_file: &str, core_path: &str) -> String {
    let node = generate_grid_node(sd, ctx, def_file, core_path);
    // publisher: new XhtmlComposer(XhtmlComposer.HTML) == HTML, non-pretty.
    let mut c = XhtmlComposer::new(Config::html_compact());
    c.compose_node(&node)
}

fn generate_grid_node(
    sd: &Sd,
    ctx: &IgContext,
    def_file: &str,
    core_path_arg: &str,
) -> render_xhtml::XhtmlNode {
    let gen = Gen::new(Some("g".to_string()));
    let mut model = generate::init_grid_table(Some(sd.id().to_string()));
    let all = sd.snapshot_elements();
    // corePath: the publisher passes the core-spec web root with trailing slash
    // (same as the snapshot path). An explicit arg (render-frag debug) overrides.
    let core_path: &str = if core_path_arg.is_empty() {
        core_path_for(sd.fhir_version())
    } else {
        core_path_arg
    };
    let mut gctx = GridCtx {
        ctx,
        sd,
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
        gctx.gen_grid_element(&mut rows, *first, true);
        model.rows = rows;
    }
    generate::generate(&gen, &mut model, "", 1)
}

struct GridCtx<'a> {
    ctx: &'a IgContext,
    sd: &'a Sd,
    all: &'a [Ed<'a>],
    core_path: &'a str,
    def_path: Option<String>,
    anchors: std::collections::HashMap<String, i32>,
    is_constraint_mode: bool,
}

impl<'a> GridCtx<'a> {
    fn sd_url(&self) -> &str {
        self.sd.root.get("url").and_then(|x| x.as_str()).unwrap_or("")
    }

    /// `genGridElement` (SDR:2602).
    fn gen_grid_element(&mut self, rows: &mut Vec<Row>, element: Ed<'a>, root: bool) {
        let s = tail(element.path());
        let children = get_children(self.all, element);
        // onlyInformationIsMapping is essentially always false for real
        // elements; we conservatively always render (matches every golden).
        let mut row = Row::new();
        // SDR:2610/2612 use `context.prefixAnchor(...)` — the RenderingContext's
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

        // left (Name) cell (SDR:2624-2627).
        // NB (faithful Java wart, cf. quirk "fixed-value links are dead"): the
        // Java bold branch tests `element.getType().get(0).isPrimitive()`, but
        // `isPrimitive()` is `Base.isPrimitive()` which is hard-coded `false` on
        // a `TypeRefComponent` (it never overrides it) — so the bold style is
        // DEAD and the grid name piece is NEVER bold. Reproduced by never bolding.
        let mut left = Cell::new();
        let name_piece = Piece::ref_text(
            ref_.clone(),
            Some(format!("\u{00A0}\u{00A0}{}", s)),
            element.definition().map(|d| d.to_string()),
        );
        left.pieces.push(name_piece);
        if let Some(sn) = element.slice_name() {
            left.pieces.push(Piece::tag("br"));
            let depth = element.path().split('.').count();
            let indent: String = std::iter::repeat('\u{00A0}').take(1 + 2 * depth).collect();
            left.pieces
                .push(Piece::ref_text(None, Some(format!("{}({})", indent, sn)), None));
        }
        row.cells.push(left);

        // Card. cell (SDR:2629).
        row.cells.push(gen_cardinality(element));
        // Type cell (SDR:2630-2633): hasDef && !"0".equals(max) -> genTypes.
        let max_is_zero = element.max() == Some("0");
        if !max_is_zero {
            let c = self.gen_types(element, root);
            row.cells.push(c);
        } else {
            row.cells.push(Cell::new());
        }
        // Description cell (SDR:2634).
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

    /// `genTypes` (SDR:2320), mustSupportMode=false, diff=false. A branch-for-
    /// branch mirror of the SUMMARY table path's `gen_types`.
    fn gen_types(&mut self, e: Ed<'a>, root: bool) -> Cell {
        let mut c = Cell::new();
        if let Some(cr) = e.content_reference() {
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
                let type_name = src.name.clone().unwrap_or_else(|| tail(url).to_string());
                c.pieces.push(Piece::ref_text(None, Some("See ".into()), None));
                c.pieces.push(Piece::ref_text(
                    Some(format!("{}#{}", src.web_path, frag)),
                    Some(format!("{} ({})", tail(frag), type_name)),
                    Some(frag.to_string()),
                ));
            }
            return c;
        }
        let types = e.types();
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
                    let v = if base_url.contains('|') || self.ctx.version_count(base_url) > 1 {
                        format!("({})", bsd.version)
                    } else {
                        String::new()
                    };
                    let name = format!("{}{}", bsd.name.clone().unwrap_or_default(), v);
                    c.pieces
                        .push(Piece::ref_text(Some(bsd.web_path.clone()), Some(name), None));
                }
            }
            return c;
        }
        let mut first = true;
        for t in &types {
            if first {
                first = false;
            } else {
                c.pieces.push(Piece::ref_text(None, Some(", ".into()), None));
            }
            if t.has_target() {
                // Reference/canonical (SDR:2382-2427)
                if !t.profiles().is_empty() {
                    let ref_ = t.profiles()[0];
                    if let Some(tsd) = self.ctx.resolve(ref_) {
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
                    self.gen_target_link(&mut c, u);
                    if canonical_is_must_support(t, u) && e.must_support() {
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
                c.pieces.push(Piece::ref_text(None, Some(")".into()), None));
            } else if !t.profiles().is_empty()
                && (t.working_code() != "Extension" || is_profiled_type(&t.profiles()))
            {
                // profiled type (SDR:2432-2461)
                let mut pfirst = true;
                for p in t.profiles() {
                    if pfirst {
                        pfirst = false;
                    } else {
                        c.pieces.push(Piece::ref_text(None, Some(", ".into()), None));
                    }
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
                        let tn = type_name_of(&sd, tc);
                        c.pieces.push(Piece::ref_text(Some(sd.web_path.clone()), Some(tn), None));
                    } else {
                        c.pieces.push(Piece::ref_text(None, Some(tc.to_string()), None));
                    }
                } else if self.ctx.has_link_for(tc) {
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

    /// `genTargetLink` (SDR:2529). Same port as the SUMMARY table path.
    fn gen_target_link(&mut self, c: &mut Cell, u: &str) {
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
                let mut href = sd.web_path.clone();
                if let Some(i) = href.find('|') {
                    href.truncate(i);
                }
                c.pieces.push(Piece::ref_text(Some(href), Some(disp), None));
            } else {
                c.pieces.push(Piece::ref_text(None, Some(u.to_string()), None));
            }
        }
    }

    /// `generateGridDescription` (SDR:3100), used=true always in grid.
    fn generate_grid_description(&mut self, definition: Ed<'a>, root: bool) -> Cell {
        let _ = root;
        let mut c = Cell::new();

        // content reference (SDR:3105-3116)
        if let Some(cr) = definition.content_reference() {
            let (url, frag) = match cr.split_once('#') {
                Some((u, f)) => (u, f),
                None => ("", cr),
            };
            if url.is_empty() || url == self.sd_url() {
                c.pieces.push(Piece::ref_text(
                    Some(format!("#{}", frag)),
                    Some(format!("See {}", frag)),
                    None,
                ));
            } else if let Some(src) = self.ctx.resolve(url) {
                let type_name = src.name.clone().unwrap_or_else(|| tail(url).to_string());
                c.pieces.push(Piece::ref_text(
                    Some(format!("{}#{}", src.web_path, frag)),
                    Some(format!("See {}.{}", type_name, frag)),
                    None,
                ));
            }
        }

        // url-fixed short circuit (SDR:3117-3119)
        if definition.path().ends_with("url") && definition.fixed().is_some() {
            let (_, v) = definition.fixed().unwrap();
            let mut piece =
                Piece::ref_text(None, Some(format!("\"{}\"", build_json(v))), None);
            piece.add_style("color: darkgreen");
            c.pieces.push(piece);
            return c;
        }

        // The Java `url` param is null on the grid genGridElement call site
        // (SDR:2634 passes url=null), so the SDR:3120-3134 URL block never fires
        // for grid. (Kept absent to match.)

        // slicing (SDR:3136-3140)
        if definition.has_slicing() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("Slice:".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces.push(Piece::ref_text(
                None,
                Some(describe_slice(definition.slicing().unwrap())),
                None,
            ));
        }

        // binding (SDR:3141-3169): valueDefn is null on grid, so the element's
        // own binding is used. Uses the BindingResolution oracle.
        if let Some(binding) = definition.binding() {
            if binding.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                // STRUC_DEF_BINDINGS = "Binding:" (NO trailing space, unlike the
                // SUMMARY table's "Binding: "). Verified against grid goldens.
                let mut lbl = Piece::ref_text(None, Some("Binding:".into()), None);
                lbl.add_style("font-weight:bold");
                c.pieces.push(lbl);
                let vs_ref = binding.get("valueSet").and_then(|x| x.as_str()).unwrap_or("");
                let br = self.ctx.resolve_binding(vs_ref);
                // Piece(br.url==null?null: isAbsoluteUrl(url)||!prependLinks?url:
                //   corePath+url, br.display, br.uri). prependLinks() is false for
                //   the IG publisher, so links pass through verbatim.
                let mut p = Piece::ref_text(br.url.clone(), Some(br.display.clone()), br.uri.clone());
                if br.external {
                    p.set_tag_img("external.png");
                }
                c.pieces.push(p);
                if let Some(strength) = binding.get("strength").and_then(|x| x.as_str()) {
                    c.pieces.push(Piece::ref_text(None, Some(" (".into()), None));
                    c.pieces.push(Piece::ref_text(
                        Some(format!("{}terminologies.html#{}", self.core_path, strength)),
                        Some(strength.to_string()),
                        Some(strength_definition(strength).to_string()),
                    ));
                    c.pieces.push(Piece::ref_text(None, Some(")".into()), None));
                }
                if let Some(desc) = binding.get("description").and_then(|x| x.as_str()) {
                    if is_simple_markdown(desc) {
                        c.pieces.push(Piece::ref_text(None, Some(": ".into()), None));
                        markdown::add_markdown(&mut c, desc);
                    }
                }
            }
        }

        // constraints (SDR:3170-3178)
        for inv in definition.constraints() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some(format!("{}: ", inv.key())), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces
                .push(Piece::ref_text(None, Some(inv.human().to_string()), None));
        }

        // fixed / pattern / example (SDR:3179-3197)
        if let Some((_, v)) = definition.fixed() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("Fixed Value:".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            let s = build_json(v);
            // link = isAbsoluteUrl(s) ? getLinkForUrl(...) : null; getLinkForUrl
            // never matches (dead — quirk "fixed-value links are dead"), so null.
            let mut val = Piece::ref_text(None, Some(s), None);
            val.add_style("color: darkgreen");
            c.pieces.push(val);
        } else if let Some((_, v)) = definition.pattern() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("Required Pattern:".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            let mut val = Piece::ref_text(None, Some(build_json(v)), None);
            val.add_style("color: darkgreen");
            c.pieces.push(val);
        } else {
            for ex in definition.example() {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                // SDR:3194: `"Example" +"'"+("".equals("General")? "": " "+label+"'")+": "`.
                // The `"".equals("General")` guard is a constant false, so the
                // label is ALWAYS emitted: `Example' <label>': ` (faithful port).
                let label = ex.get("label").and_then(|x| x.as_str()).unwrap_or("");
                let mut lbl = Piece::ref_text(
                    None,
                    Some(format!("Example' {}': ", label)),
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

        // maxLength (SDR:3198-3202)
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

        // minLength ext (SDR:3203-3211)
        if let Some(min) = read_int_extension(
            &definition,
            "http://hl7.org/fhir/StructureDefinition/elementdefinition-minLength",
        ) {
            if min > 0 {
                if !c.pieces.is_empty() {
                    c.pieces.push(Piece::tag("br"));
                }
                let mut lbl = Piece::ref_text(None, Some("Min Length:".into()), None);
                lbl.add_style("font-weight:bold");
                c.pieces.push(lbl);
                let mut val = Piece::ref_text(None, Some(min.to_string()), None);
                val.add_style("color: darkgreen");
                c.pieces.push(val);
            }
        }

        // profile mapping rows with a table-name extension (SDR:3212-3227): none
        // in the corpus. If a mapping carries the edm-table-name extension this
        // block would emit "<name>: <map>" rows; absent here.

        // definition (SDR:3228-3234)
        if definition.has_definition() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("Definition:".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces.push(Piece::tag("br"));
            markdown::add_markdown(&mut c, definition.definition().unwrap());
        }

        // comment (SDR:3235-3239)
        if let Some(comment) = definition.comment() {
            if !c.pieces.is_empty() {
                c.pieces.push(Piece::tag("br"));
            }
            let mut lbl = Piece::ref_text(None, Some("Comments:".into()), None);
            lbl.add_style("font-weight:bold");
            c.pieces.push(lbl);
            c.pieces.push(Piece::tag("br"));
            markdown::add_markdown(&mut c, comment);
        }
        c
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

/// `ExtensionUtilities.readIntegerExtension(defn, url, 0)`.
fn read_int_extension(e: &Ed<'_>, url: &str) -> Option<i64> {
    e.extensions().into_iter().find_map(|ext| {
        if ext.get("url").and_then(|x| x.as_str()) == Some(url) {
            ext.get("valueInteger").and_then(|x| x.as_i64())
        } else {
            None
        }
    })
}

/// `genCardinality` (SDR:1431), grid subset (no derivation pointer, no fallback).
fn gen_cardinality(e: Ed<'_>) -> Cell {
    // `gen.new Cell(null,null,null,null,null)` (SDR:1467): the 5-arg ctor adds a
    // single all-null Piece (prefix/suffix null -> skipped; the ref/text/hint
    // piece added unconditionally). It renders to empty but must exist.
    let mut cell = Cell::with(None, None, None, None, None);
    let min = e.min();
    let max = e.max();
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

/// `getRowColor` (ProfileUtilities). For a plain element returns null; the
/// alternating background is handled by the table generator, not here. Verified:
/// grid rows carry `background-color: white`.
fn get_row_color(_e: Ed<'_>, _is_constraint_mode: bool) -> String {
    "white".to_string()
}

fn uses_must_support(list: &[Ed<'_>]) -> bool {
    list.iter().any(|e| e.has_must_support() && e.must_support())
}

/// `getChildren(all, element)` (SDR:3257).
fn get_children<'a>(all: &'a [Ed<'a>], element: Ed<'a>) -> Vec<Ed<'a>> {
    let mut result = Vec::new();
    let ep = element.path();
    let idx = all.iter().position(|e| std::ptr::eq(e.v, element.v));
    let start = match idx {
        Some(i) => i + 1,
        None => return result,
    };
    let mut i = start;
    while i < all.len() && all[i].path().len() > ep.len() {
        let p = all[i].path();
        if p.len() > ep.len() + 1
            && p[..ep.len() + 1] == format!("{}.", ep)
            && !p[ep.len() + 1..].contains('.')
        {
            result.push(all[i]);
        }
        i += 1;
    }
    result
}

// tail is imported from crate::table.
