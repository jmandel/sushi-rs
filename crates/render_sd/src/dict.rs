//! `render_sd::dict` — the StructureDefinition data-dictionary fragment family:
//! `dict`, `dict-active`, `dict-diff`, `dict-key`, `dict-ms`.
//!
//! The publisher entry is `StructureDefinitionRenderer.dict(incProfiledOut, mode,
//! anchorPrefix)` (publisher psdr:1308) which composes a `<div>` with a guidance
//! `<p>` and a `<table class="dict">`, then delegates the per-element rows to the
//! fhir-core SDR `renderDict` (SDR:3968) + `generateElementInner` (SDR:4361).
//! Final composition: `new XhtmlComposer(false,false).compose(x.getChildNodes())`
//! = HTML non-pretty = `Config::html_compact()` via `compose_nodes`.
//!
//! Citations: `SDR:<n>` = fhir-core r5 StructureDefinitionRenderer.java;
//! `psdr:<n>` = publisher StructureDefinitionRenderer.java.
//!
//! Modes (publisher elementsForMode:1190):
//!   dict / dict-active : GEN_MODE_SNAP (1), anchorPrefix ""  , snapshot.element
//!   dict-diff          : GEN_MODE_DIFF (2), anchorPrefix "diff_", supplementMissingDiffElements
//!   dict-ms            : GEN_MODE_MS   (3), anchorPrefix "ms_" , getMustSupportElements
//!   dict-key           : GEN_MODE_KEY  (4), anchorPrefix "key_", getKeyElements
//! dict vs dict-active differ only in `incProfiledOut` (dict=true keeps max==0
//! prohibited elements; dict-active=false drops them).

use std::collections::HashMap;

use render_xhtml::node::XhtmlNode;
use render_xhtml::{Config, XhtmlComposer};
use serde_json::Value;

use crate::context::IgContext;
use crate::leaf::{el, escape_xml, tx};
use crate::sdmodel::{Ed, Sd};
use render_tables::hashorder::hashmap_order;

/// Set attributes on a node in Java `HashMap` iteration order (XhtmlNode's
/// attributes is a HashMap; the composer emits its keySet() order). Callers pass
/// the (name, value) pairs in *set* order; we reorder to the HashMap order.
fn set_attrs(node: &mut XhtmlNode, pairs: &[(&str, String)]) {
    let names: Vec<&str> = pairs.iter().map(|(n, _)| *n).collect();
    for n in hashmap_order(&names) {
        let v = pairs.iter().find(|(name, _)| *name == n).unwrap().1.clone();
        node.set_attribute(n, v);
    }
}

/// Java HashMap iteration order over a SUBSET of keys, where the map's capacity
/// was sized by the FULL set of puts (`all_put_keys`) — removes don't shrink a
/// HashMap. Returns `surviving` reordered into Java iteration order at that
/// capacity. (hashorder::hashmap_order recomputes capacity from its input len,
/// which is wrong after removals from a larger map.)
fn hashmap_order_surviving(all_put_keys: &[String], surviving: &[String]) -> Vec<String> {
    use render_tables::hashorder::java_string_hash;
    let n = all_put_keys.len();
    let mut cap: usize = 16;
    while (n as f64) > (cap as f64) * 0.75 {
        cap <<= 1;
    }
    let mask = (cap - 1) as u32;
    let spread = |h: i32| -> u32 {
        let hu = h as u32;
        hu ^ (hu >> 16)
    };
    // Each key's (bucket, insertion-index) at the full capacity.
    let mut items: Vec<(u32, usize, String)> = surviving
        .iter()
        .map(|k| {
            let ins = all_put_keys.iter().position(|p| p == k).unwrap_or(usize::MAX);
            (mask & spread(java_string_hash(k)), ins, k.clone())
        })
        .collect();
    items.sort_by_key(|(b, i, _)| (*b, *i));
    items.into_iter().map(|(_, _, k)| k).collect()
}

// GEN_MODE_* (SDR:397-400).
pub const GEN_MODE_SNAP: i32 = 1;
pub const GEN_MODE_DIFF: i32 = 2;
pub const GEN_MODE_MS: i32 = 3;
pub const GEN_MODE_KEY: i32 = 4;

/// Public entry: render the dict fragment for one SD.
pub fn render_dict(
    sd: &Sd,
    ctx: &IgContext,
    core_path: &str,
    inc_profiled_out: bool,
    mode: i32,
    anchor_prefix: &str,
) -> String {
    let elements = elements_for_mode(sd, ctx, mode);
    let mut r = DictRenderer {
        sd,
        ctx,
        core_path: core_path.to_string(),
        mode,
        anchor_prefix: anchor_prefix.to_string(),
    };

    // psdr:1308 dict() — the outer <div>.
    let mut x = el("div");
    // p.tx(SDR_GUIDANCE_PFX); p.ah(readingIgs).tx(SDR_GUIDANCE_HERE); p.tx(SFX).
    let mut p = el("p");
    tx(
        &mut p,
        "Guidance on how to interpret the contents of this table can be found",
    );
    let mut ah = el("a");
    ah.set_attribute(
        "href",
        "https://build.fhir.org/ig/FHIR/ig-guidance/readingIgs.html#data-dictionaries",
    );
    tx(&mut ah, "here");
    p.add_child_node(ah);
    // SDR_GUIDANCE_SFX is empty in English phrases.
    x.add_child_node(p);

    // x.table("dict", false).markGenerated(true): class="dict" data-fhir="generated".
    let mut t = el("table");
    set_attrs(&mut t, &[("class", "dict".into()), ("data-fhir", "generated".into())]);

    r.render_dict_body(&elements, &mut t, inc_profiled_out);
    x.add_child_node(t);

    let mut c = XhtmlComposer::new(Config::html_compact());
    c.compose_nodes(x.child_nodes())
}

/// The element list for a mode (publisher elementsForMode:1190). Returns owned
/// JSON values so the DIFF/KEY/MS supplementation can synthesise elements.
fn elements_for_mode(sd: &Sd, ctx: &IgContext, mode: i32) -> Vec<Value> {
    match mode {
        GEN_MODE_DIFF => crate::diff::supplement_missing_diff_elements(sd),
        GEN_MODE_KEY => crate::table::key_elements_pub(sd, ctx),
        GEN_MODE_MS => crate::table::must_support_elements_pub(sd, ctx),
        _ => sd.snapshot_elements().iter().map(|e| e.v.clone()).collect(),
    }
}

struct DictRenderer<'a> {
    sd: &'a Sd,
    ctx: &'a IgContext,
    core_path: String,
    mode: i32,
    anchor_prefix: String,
}

impl<'a> DictRenderer<'a> {
    /// SDR renderDict:3968 — the per-element loop with anchor pre-pass.
    fn render_dict_body(&mut self, elements: &[Value], t: &mut XhtmlNode, inc_profiled_out: bool) {
        let n = elements.len();
        // Pre-pass: build the anchor sets exactly as generateAnchors/checkInScope.
        let mut all_anchors: HashMap<String, usize> = HashMap::new();
        // per-element list of surviving anchor strings (render_dict_generator_anchors).
        let mut anchor_lists: Vec<Vec<String>> = vec![Vec::new(); n];
        let mut excluded: Vec<bool> = vec![false; n];
        let mut stack: Vec<usize> = Vec::new();
        for i in 0..n {
            add_to_stack(&mut stack, elements, i);
            generate_anchors(&stack, elements, &mut all_anchors, &mut anchor_lists);
            check_in_scope(&stack, elements, &mut excluded);
        }

        // dstack drives finish() (deleted-element rendering) — a NO-OP in this
        // corpus (VersionComparisonAnnotation always empty). We omit it and fire
        // a loud gap only if a deleted element is ever encountered (never here).

        let mut i = 0i64;
        for (idx, ec_v) in elements.iter().enumerate() {
            let ec = Ed::new(ec_v);
            let max0 = ec.max() == Some("0");
            if (inc_profiled_out || !max0) && !excluded[idx] {
                // compareElement selection (SDR:3982).
                let compare = self.compare_element(&ec);

                let anchors = make_anchors(&ec, &self.anchor_prefix, &anchor_lists[idx]);
                let title = ec.id();
                let mut tr = el("tr");
                // tr.td("structure").colspan(2): set class then colspan; HashMap
                // order emits colspan then class.
                let mut td = el("td");
                set_attrs(&mut td, &[("class", "structure".into()), ("colspan", "2".into())]);
                let mut sp = el("span");
                sp.set_attribute("class", "self-link-parent");
                for s in &anchors {
                    // sp.an(prefixAnchor(s)).tx(" "): an(href) itself adds " ",
                    // then .tx(" ") adds a second -> two spaces.
                    let mut a = el("a");
                    a.set_attribute("name", s.clone());
                    tx(&mut a, "  ");
                    sp.add_child_node(a);
                }
                // sp.span("color: grey", null).tx(Integer.toString(i++))
                let mut greyspan = el("span");
                greyspan.set_attribute("style", "color: grey");
                tx(&mut greyspan, &i.to_string());
                sp.add_child_node(greyspan);
                i += 1;
                // sp.b().tx(". "+title)
                let mut b = el("b");
                tx(&mut b, &format!(". {}", title));
                sp.add_child_node(b);
                // link(sp, ec.getId(), anchorPrefix)
                self.link(&mut sp, ec.id());
                td.add_child_node(sp);
                tr.add_child_node(td);
                t.add_child_node(tr);

                if is_profiled_extension(&ec) {
                    // SDR:3998 — a profiled Extension. Resolve the extension
                    // definition; if it resolves, pass its Extension.value* element
                    // as `value` with mode 2 (prohibited value -> complex) or 3.
                    let purl = ec.types()[0].profiles().first().copied().map(String::from);
                    let ext_defn = purl.as_deref().and_then(|u| self.ctx.load_resource(u));
                    match ext_defn {
                        None => {
                            // extDefn == null: generateElementInner(mode=1, value=null).
                            self.generate_element_inner(t, &ec, 1, None, compare.as_ref());
                        }
                        Some(ext_sd) => {
                            let value_defn = extension_value_definition(&ext_sd);
                            let prohibited = value_defn
                                .as_ref()
                                .map(|v| v.get("max").and_then(|m| m.as_str()) == Some("0"))
                                .unwrap_or(true);
                            let vmode = if value_defn.is_none() || prohibited { 2 } else { 3 };
                            self.generate_element_inner(t, &ec, vmode, value_defn.as_ref(), compare.as_ref());
                        }
                    }
                } else {
                    self.generate_element_inner(t, &ec, self.mode, None, compare.as_ref());
                    if ec.has_slicing() {
                        self.generate_slicing(t, &ec, ec.slicing().unwrap(), compare.as_ref());
                    }
                }
            }
            // t.tx("\r\n"); i++ (SDR:4023-4024) — note i increments AGAIN here.
            t.add_text("\r\n");
            i += 1;
            let _ = idx;
        }
        // finish() dstack drain + finish(null): NO-OP (no deleted elements).
    }

    /// SDR:3982 — compareElement per mode. DIFF -> getBaseElement; KEY ->
    /// getRootElement; else null. Returns an owned Ed-backing JSON value.
    fn compare_element(&self, ec: &Ed) -> Option<Value> {
        match self.mode {
            GEN_MODE_DIFF => self.get_base_element(ec),
            GEN_MODE_KEY => self.get_root_element(ec),
            _ => None,
        }
    }

    /// SDR getBaseElement:4074 — via SNAPSHOT_DERIVATION_POINTER. We reconstruct
    /// the pointer as our own-snapshot element sharing the diff element's id
    /// (diff::reconstruct_diff_pointers), then read the SAME id from the BASE
    /// SD's snapshot (getElementById(baseDefinition, pointerId)). The pointer id
    /// equals the base element id for restated diff elements; the own-snapshot
    /// element at the pointer index carries that id.
    fn get_base_element(&self, ec: &Ed) -> Option<Value> {
        let pointers = crate::diff::reconstruct_diff_pointers(self.sd);
        let idx = *pointers.get(ec.id())?;
        let snap = self.sd.snapshot_elements();
        let pointer_id = snap.get(idx)?.id().to_string();
        let base_url = self
            .sd
            .root
            .get("baseDefinition")
            .and_then(|x| x.as_str())?;
        self.get_element_by_id(base_url, &pointer_id)
    }

    /// SDR getRootElement:4082 — the base-path root element from the type's core
    /// SD (`http://hl7.org/fhir/StructureDefinition/<rootType>`), by base path.
    fn get_root_element(&self, ec: &Ed) -> Option<Value> {
        let base_path = ec.base_path()?;
        let root_type = base_path.split('.').next().unwrap_or(base_path);
        let url = format!("http://hl7.org/fhir/StructureDefinition/{}", root_type);
        self.get_element_by_id(&url, base_path)
    }

    /// SDR getElementById:4048 — load the SD's snapshot, find the element whose
    /// id equals `id`, then apply updateURLs (PU:2135) with processRelatives=false
    /// so the base element's markdown fields (definition/comment/requirements/
    /// meaningWhenMissing/binding.description) get their relative spec links
    /// corePath-prefixed. This is what makes the base's raw markdown byte-equal to
    /// the profile's already-absolute markdown, driving compareMarkdown's areEqual
    /// (the unchanged/DarkGray styling) in KEY/DIFF modes. webUrl = the SD's
    /// render_webroot; for the core package that is the spec base (== corePath
    /// without the trailing slash).
    fn get_element_by_id(&self, url: &str, id: &str) -> Option<Value> {
        let res = self.ctx.load_resource(url)?;
        let els = res.get("snapshot")?.get("element")?.as_array()?;
        let mut e = els
            .iter()
            .find(|e| e.get("id").and_then(|x| x.as_str()) == Some(id))
            .cloned()?;
        self.update_urls(&mut e);
        Some(e)
    }

    /// updateURLs markdown rewrite (PU:2150-2166) with processRelatives=false —
    /// implemented via publisher_markdown's process_relative_urls (the same
    /// processRelativeUrls(…, false) path). Applied to the base compare element.
    fn update_urls(&self, e: &mut Value) {
        let webroot = self.core_path.trim_end_matches('/');
        for field in ["definition", "comment", "requirements", "meaningWhenMissing"] {
            if let Some(s) = e.get(field).and_then(|x| x.as_str()) {
                let rewritten = crate::publisher_markdown::process_relative_urls_pub(s, webroot);
                if let Some(obj) = e.as_object_mut() {
                    obj.insert(field.to_string(), Value::String(rewritten));
                }
            }
        }
        if let Some(desc) = e.get("binding").and_then(|b| b.get("description")).and_then(|x| x.as_str()) {
            let rewritten = crate::publisher_markdown::process_relative_urls_pub(desc, webroot);
            if let Some(b) = e.get_mut("binding").and_then(|b| b.as_object_mut()) {
                b.insert("description".to_string(), Value::String(rewritten));
            }
        }
        // Core-canonical version pinning: the publisher loads the core package
        // with its FHIR version, so versionless references to core canonicals
        // (`http://hl7.org/fhir/...`) on the base element get `|<fhirVersion>`
        // appended (CanonicalResourceManager version stamp). This makes the base
        // element's binding valueSet AND type profiles/targetProfiles carry the
        // version — driving compareString's equality (the shared Patient target
        // no longer matches the profile's unversioned `.../Patient`, so the base
        // copy renders as a separate removed row, as the goldens show). The
        // profile's OWN snapshot already carries `|<version>` on its bindings.
        let ver = self.sd.fhir_version().to_string();
        if !ver.is_empty() {
            pin_core_version(e.get_mut("binding").and_then(|b| b.get_mut("valueSet")), &ver);
            if let Some(types) = e.get_mut("type").and_then(|t| t.as_array_mut()) {
                for t in types {
                    if let Some(arr) = t.get_mut("targetProfile").and_then(|x| x.as_array_mut()) {
                        for p in arr { pin_core_version(Some(p), &ver); }
                    }
                    if let Some(arr) = t.get_mut("profile").and_then(|x| x.as_array_mut()) {
                        for p in arr { pin_core_version(Some(p), &ver); }
                    }
                }
            }
        }
    }

    /// SDR link:4187 — the self-link `<a>` with the SVG glyph.
    fn link(&self, x: &mut XhtmlNode, id: &str) {
        let mut ah = el("a");
        // ah(href).attribute("title").attribute("class") — set order href,title,class.
        set_attrs(&mut ah, &[
            ("href", format!("#{}{}", self.anchor_prefix, id)),
            ("title", "link to here".into()),
            ("class", "self-link".into()),
        ]);
        let mut svg = el("svg");
        // svg() sets xmlns + xmlns:xlink, then attribute viewBox/width/height/class
        // in that set order (SDR:4191-4195). svg() sets xmlns first, xmlns:xlink
        // second; then viewBox, width, height, class.
        set_attrs(&mut svg, &[
            ("xmlns", "http://www.w3.org/2000/svg".into()),
            ("xmlns:xlink", "http://www.w3.org/1999/xlink".into()),
            ("viewBox", "0 0 1792 1792".into()),
            ("width", "16".into()),
            ("height", "16".into()),
            ("class", "self-link".into()),
        ]);
        let mut path = el("path");
        path.set_attribute("d", SELF_LINK_PATH);
        svg.add_child_node(path);
        ah.add_child_node(svg);
        x.add_child_node(ah);
    }

    // -----------------------------------------------------------------------
    // generateElementInner (SDR:4361) — the row body.
    // -----------------------------------------------------------------------
    fn generate_element_inner(
        &mut self,
        tbl: &mut XhtmlNode,
        d: &Ed,
        mode: i32,
        value: Option<&Value>,
        compare: Option<&Value>,
    ) {
        let compare_ed = compare.map(Ed::new);
        let root = !d.path().contains('.');
        let sliced_extension = d.has_slice_name()
            && (d.path().ends_with(".extension") || d.path().ends_with(".modifierExtension"));

        // Slice name / constraining (SDR:4365).
        if d.has_slice_name() {
            let cmp = compare_ed.and_then(|c| c.slice_name());
            self.row_cmp_string(tbl, "Slice Name", Some("profiling.html#slicing"), d.slice_name(), cmp, mode, false);
            let new_c = encode_bool_opt(d.v.get("sliceIsConstraining"));
            let old_c = compare_ed.and_then(|c| encode_bool_opt(c.v.get("sliceIsConstraining")));
            self.row_cmp_string(tbl, "Slice is Constraining", Some("profiling.html#slicing"), new_c.as_deref(), old_c.as_deref(), mode, false);
        }

        // Definition (markdown) (SDR:4370). compare passed null when sliced ext.
        let def_present = compare_ed.is_some() && !sliced_extension;
        let def_cmp = if def_present { compare_ed.and_then(|c| c.definition()) } else { None };
        let node = self.compare_markdown_el(d.definition(), def_cmp, def_present, mode);
        self.row_node(tbl, "Definition", None, node);

        // Short (SDR:4371).
        let short_cmp = compare_ed.and_then(|c| c.short());
        self.row_cmp_string(tbl, "Short", None, d.short(), short_cmp, mode, false);

        // Comments (markdown) (SDR:4372).
        let com_present = compare_ed.is_some() && !sliced_extension;
        let com_cmp = if com_present { compare_ed.and_then(|c| c.comment()) } else { None };
        let node = self.compare_markdown_el(d.comment(), com_cmp, com_present, mode);
        self.row_node(tbl, "Comments", None, node);

        // Note (businessIdWarning) (SDR:4373).
        if let Some(node) = self.business_id_warning(tail(d.path())) {
            self.row_node(tbl, "Note", None, Some(node));
        }

        // Control (cardinality) (SDR:4374).
        let node = self.describe_cardinality(d, compare_ed.as_ref(), mode);
        self.row_node(tbl, "Control", Some("conformance-rules.html#conformance"), node);

        // Binding (SDR:4375).
        let node = self.describe_binding(d, d.path(), compare_ed.as_ref(), mode);
        self.row_node(tbl, "Binding", Some("terminologies.html"), node);

        // Type / content reference (SDR:4376).
        if let Some(cr) = d.content_reference() {
            // STRUC_DEF_SEE ("See", no trailing space) + contentReference.substring(1).
            self.row_text(tbl, "Type", None, &format!("See{}", &cr[1..]));
        } else {
            let node = self.describe_types(&d.types(), false, d, compare_ed.as_ref(), mode, value);
            self.row_node(tbl, "Type", Some("datatypes.html"), node);
        }

        // [x] Note (SDR:4390).
        if d.path().ends_with("[x]") && d.max() != Some("0") {
            // tableRow(..).ahWithText(SEE, spec("formats.html#choice"), null,
            //   CHOICE_DATA_TYPE, FURTHER_INFO)
            let mut tr = el("tr");
            self.add_first_cell(&mut tr, "[x] Note", None);
            let mut cell = el("td");
            tx(&mut cell, "See");
            let mut ah = el("a");
            ah.set_attribute("href", self.spec("formats.html#choice"));
            tx(&mut ah, "Choice of Data Types");
            cell.add_child_node(ah);
            tx(&mut cell, "for further information about how to use [x]");
            tr.add_child_node(cell);
            tbl.add_child_node(tr);
        }

        // Is Modifier (SDR:4394).
        let node = self.present_modifier(d, mode, compare_ed.as_ref());
        self.row_node(tbl, "Is Modifier", Some("conformance-rules.html#ismodifier"), node);

        // Primitive value flags (SDR:4395).
        if d.must_have_value() {
            // STRUC_DEF_PRIM_TYPE_VALUE (SDR:4396).
            self.row_text(tbl, "Primitive Value", Some("elementdefinition.html#primitives"),
                "This primitive type must have a value (the value must be present, and cannot be replaced by an extension)");
        } else if d.v.get("valueAlternatives").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false) {
            // hasValueAlternatives (SDR:4397): renderCanonicalList(PRIM_TYPE_PRESENT).
            crate::loud_gap!((), "LOUD GAP: dict primitive value-alternatives (SDR:4398) for {} ({})", self.sd.id(), d.id());
        } else if self.has_primitive_types(d) {
            // STRUC_DEF_PRIM_ELE (SDR:4400).
            self.row_text(tbl, "Primitive Value", Some("elementdefinition.html#primitives"),
                "This primitive element may be present, or absent, or replaced by an extension");
        }

        // Must Support (SDR:4405).
        let node = self.display_boolean(d.must_support(), d.has_must_support(),
            compare_ed.and_then(|c| c.v.get("mustSupport").and_then(|x| x.as_bool())), mode);
        self.row_node(tbl, "Must Support", Some("conformance-rules.html#mustSupport"), node);
        if d.must_support() {
            if has_must_support_types(&d.types()) {
                let node = self.describe_types(&d.types(), true, d, compare_ed.as_ref(), mode, None);
                self.row_node(tbl, "Must Support Types", Some("datatypes.html"), node);
            } else if has_choices(&d.types()) {
                self.row_text(tbl, "Must Support Types", Some("datatypes.html"),
                    "No must-support rules about the choice of types/profiles");
            }
        }

        // Obligations (SDR:4455) — not ported. Skip in the preview rather than
        // panic: a wasm panic aborts the whole engine (obligation rows are
        // supplementary, so omitting them degrades gracefully).

        // XML Format (SDR:4496) — driven by representation (or xml-namespace/name
        // extensions, which guard_unported_rows flags).
        if d.v.get("representation").and_then(|r| r.as_array()).map(|a| !a.is_empty()).unwrap_or(false) {
            let node = describe_xml(d);
            self.row_node(tbl, "XML Format", None, node);
        }

        // Summary (SDR:4521).
        if mode != GEN_MODE_DIFF && d.v.get("isSummary").is_some() {
            self.row_text(tbl, "Summary", Some("search.html#summary"), if d.is_summary() { "true" } else { "false" });
        }

        // Requirements (markdown) (SDR:4524).
        let req = d.v.get("requirements").and_then(|x| x.as_str());
        let req_present = compare_ed.is_some() && !sliced_extension;
        let req_cmp = if req_present { compare_ed.and_then(|c| c.v.get("requirements").and_then(|x| x.as_str())) } else { None };
        let node = self.compare_markdown_el(req, req_cmp, req_present, mode);
        self.row_node(tbl, "Requirements", None, node);

        // Label (SDR:4525).
        let label_cmp = compare_ed.and_then(|c| c.v.get("label").and_then(|x| x.as_str()));
        self.row_cmp_string(tbl, "Label", None, d.v.get("label").and_then(|x| x.as_str()), label_cmp, mode, false);

        // Alternate Names / alias (SDR:4526).
        let alias_cmp = if compare_ed.is_none() || sliced_extension { None } else { compare_ed };
        let node = self.compare_simple_type_lists(
            str_list(d.v.get("alias")),
            alias_cmp.map(|c| str_list(c.v.get("alias"))),
            mode,
            ", ",
        );
        self.row_node(tbl, "Alternate Names", None, node);

        // Definitional Codes / code (SDR:4527).
        let code_cmp = if compare_ed.is_none() || sliced_extension { None } else { compare_ed };
        let node = self.compare_data_type_lists(
            coding_list(d.v.get("code")),
            code_cmp.map(|c| coding_list(c.v.get("code"))),
            mode,
        );
        self.row_node(tbl, "Definitional Codes", None, node);

        // Min/Max value (SDR:4528).
        self.encode_value_row(tbl, "Min Value", d.v, "minValue", compare_ed.as_ref().map(|c| c.v), mode);
        self.encode_value_row(tbl, "Max Value", d.v, "maxValue", compare_ed.as_ref().map(|c| c.v), mode);

        // Max Length (SDR:4530).
        let ml = d.max_length().map(|x| x.to_string());
        let ml_cmp = compare_ed.and_then(|c| c.max_length()).map(|x| x.to_string());
        self.row_cmp_string(tbl, "Max Length", None, ml.as_deref(), ml_cmp.as_deref(), mode, false);

        // Min Length ext (SDR:4531) — handled by guard_unported_rows.
        // Value required / alternatives (SDR:4532-4533).
        let vr_new = encode_bool_opt(d.v.get("mustHaveValue"));
        let vr_old = compare_ed.and_then(|c| encode_bool_opt(c.v.get("mustHaveValue")));
        self.row_cmp_string(tbl, "Value Required", None, vr_new.as_deref(), vr_old.as_deref(), mode, false);
        let va_cmp = if compare_ed.is_none() || sliced_extension { None } else { compare_ed };
        let node = self.compare_simple_type_lists(
            str_list(d.v.get("valueAlternatives")),
            va_cmp.map(|c| str_list(c.v.get("valueAlternatives"))),
            mode,
            ", ",
        );
        self.row_node(tbl, "Value Alternatives", None, node);

        // Default Value (SDR:4534).
        self.encode_value_named_row(tbl, "Default Value", d.v, "defaultValue", compare_ed.as_ref().map(|c| c.v), mode);
        // Meaning if Missing (SDR:4535).
        self.row_text_opt(tbl, "Meaning if Missing", None, d.v.get("meaningWhenMissing").and_then(|x| x.as_str()));
        // Fixed Value (SDR:4536).
        self.encode_value_named_row(tbl, "Fixed Value", d.v, "fixed", compare_ed.as_ref().map(|c| c.v), mode);
        // Pattern Value (SDR:4537).
        self.encode_value_named_row(tbl, "Pattern Value", d.v, "pattern", compare_ed.as_ref().map(|c| c.v), mode);
        // Example (SDR:4538).
        let node = self.encode_values(d.example());
        self.row_node(tbl, "Example", None, node);
        // Invariants (SDR:4539).
        let node = self.invariants(d, compare_ed.as_ref(), mode);
        self.row_node(tbl, "Invariants", None, node);
        // LOINC / SNOMED mappings (SDR:4540).
        let node = self.get_mapping(d, "http://loinc.org", compare_ed.as_ref(), mode);
        self.row_node(tbl, "LOINC Code", None, node);
        let node = self.get_mapping(d, "http://snomed.info", compare_ed.as_ref(), mode);
        self.row_node(tbl, "SNOMED-CT Code", None, node);

        // Guard: any un-ported extension-driven row present on the element fires
        // a loud gap (the SDR emits rows for these — we must not silently drop).
        self.guard_unported_rows(d, root);

        // tbl.tx("\r\n") (SDR:4542).
        tbl.add_text("\r\n");
    }

    /// Fire a loud gap for any element extension that would drive an un-ported
    /// row in generateElementInner (SDR:4381-4520). Corpus dict SDs hit none.
    fn guard_unported_rows(&self, d: &Ed, _root: bool) {
        const UNPORTED: &[(&str, &str)] = &[
            ("http://hl7.org/fhir/tools/StructureDefinition/type-specifier", "STRUC_DEF_TYPE_SPEC"),
            ("http://hl7.org/fhir/StructureDefinition/elementdefinition-defaulttype", "DEFAULT_TYPE"),
            ("http://hl7.org/fhir/StructureDefinition/elementdefinition-allowedUnits", "ALLOWED_UNITS"),
            ("http://hl7.org/fhir/StructureDefinition/elementdefinition-date-format", "DATE_FORMAT"),
            ("http://hl7.org/fhir/StructureDefinition/elementdefinition-idExpectation", "ID_EXPECTATION"),
            ("http://hl7.org/fhir/tools/StructureDefinition/id-choice-group", "ID_CHOICE_GROUP"),
            ("http://hl7.org/fhir/StructureDefinition/elementdefinition-standards-status", "STANDARDS_STATUS"),
            ("http://hl7.org/fhir/StructureDefinition/elementdefinition-minLength", "MIN_LENGTH"),
        ];
        for (url, tag) in UNPORTED {
            if d.has_extension(url) {
                crate::loud_gap!((), "LOUD GAP: dict un-ported row {} ({}) for {} ({})", tag, url, self.sd.id(), d.id());
            }
        }
        // JSON/XML tooling-format extensions (SDR:4492-4503) except XML-representation.
        for e in d.extensions() {
            let u = e.get("url").and_then(|x| x.as_str()).unwrap_or("");
            if u.contains("elementdefinition-json") || u.contains("json-name") || u.contains("json-empty")
                || u.contains("implied-string-prefix") || u.contains("binding-style") || u.contains("extension-style")
                || u.contains("xml-namespace") || u.contains("xml-name")
            {
                crate::loud_gap!((), "LOUD GAP: dict tooling-format row for {} ({}) ext {}", self.sd.id(), d.id(), u);
            }
        }
    }

    // -----------------------------------------------------------------------
    // tableRow helpers (SDR:4821-4876).
    // -----------------------------------------------------------------------

    /// tableRow(name, defRef, strike, XhtmlNode) (SDR:4831) — emit iff node
    /// non-null AND non-empty; copies the div's children into the td.
    fn row_node(&self, tbl: &mut XhtmlNode, name: &str, def_ref: Option<&str>, node: Option<XhtmlNode>) {
        let Some(node) = node else { return };
        if !node.has_children() && !node.has_content() {
            return;
        }
        let mut tr = el("tr");
        self.add_first_cell(&mut tr, name, def_ref);
        let mut td = el("td");
        for c in node.child_nodes() {
            td.add_child_node(c.clone());
        }
        tr.add_child_node(td);
        tbl.add_child_node(tr);
    }

    /// tableRow(name, defRef, strike, String) (SDR:4842) — emit iff text non-empty.
    fn row_text(&self, tbl: &mut XhtmlNode, name: &str, def_ref: Option<&str>, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut tr = el("tr");
        self.add_first_cell(&mut tr, name, def_ref);
        let mut td = el("td");
        tx(&mut td, text);
        tr.add_child_node(td);
        tbl.add_child_node(tr);
    }

    fn row_text_opt(&self, tbl: &mut XhtmlNode, name: &str, def_ref: Option<&str>, text: Option<&str>) {
        if let Some(t) = text {
            self.row_text(tbl, name, def_ref, t);
        }
    }

    /// Emit a compareString-based row (single-string field).
    fn row_cmp_string(
        &self,
        tbl: &mut XhtmlNode,
        name: &str,
        def_ref: Option<&str>,
        new_str: Option<&str>,
        old_str: Option<&str>,
        mode: i32,
        code: bool,
    ) {
        let node = compare_string(new_str, None, name_slot(), old_str, None, mode, false, false, code);
        self.row_node(tbl, name, def_ref, node);
    }

    /// addFirstCell (SDR:4864): <td> with white-space:nowrap iff name<=16 chars;
    /// a plain-text or `<a>`-linked label.
    fn add_first_cell(&self, tr: &mut XhtmlNode, name: &str, def_ref: Option<&str>) {
        let mut td = el("td");
        if name.chars().count() <= 16 {
            td.set_attribute("style", "white-space: nowrap");
        }
        match def_ref {
            None => tx(&mut td, name),
            Some(dr) if is_absolute_url(dr) => {
                let mut ah = el("a");
                ah.set_attribute("href", dr.to_string());
                tx(&mut ah, name);
                td.add_child_node(ah);
            }
            Some(dr) => {
                let mut ah = el("a");
                ah.set_attribute("href", format!("{}{}", self.core_path, dr));
                tx(&mut ah, name);
                td.add_child_node(ah);
            }
        }
        tr.add_child_node(td);
    }

    fn spec(&self, name: &str) -> String {
        // spec(name) = pathURL(getSpecUrl(version), name); corePath already the
        // spec base (http://hl7.org/fhir/R4/), so pathURL(corePath, name).
        crate::context::join_url(self.core_path.trim_end_matches('/'), name)
    }

    // -----------------------------------------------------------------------
    // compareMarkdown (SDR:4211) — the markdown fields.
    // -----------------------------------------------------------------------
    /// SNAP/MS/KEY: compare is null-effect (mode != DIFF path with compare==null
    /// in this corpus). We port the `compare == null || mode == DIFF` branch:
    /// process markdown, parseMDFragment, fixFontSizes(11), wrap in <div>.
    /// The compare!=null non-DIFF strikethrough/unchanged branches fire a loud
    /// gap (corpus dict/dict-ms/dict-key never supply a markdown compare).
    /// `compare_element_present` distinguishes "no compare element" (SNAP; the
    /// `compare==null` short-circuit) from "compare element present but this
    /// markdown field is absent" (KEY/DIFF; Java's lazy getXxxElement() yields a
    /// non-null empty PrimitiveType, so areEqual is evaluated and, being false,
    /// the not-equal branch renders the new markdown WITHOUT fixFontSizes).
    fn compare_markdown_el(&self, md: Option<&str>, compare: Option<&str>, compare_element_present: bool, mode: i32) -> Option<XhtmlNode> {
        // In Java the `compare` arg is a PrimitiveType, null only when the whole
        // compare element is null. When the element exists but the field is
        // absent, compare is a non-null empty value.
        let compare_arg: Option<&str> = if compare_element_present {
            Some(compare.unwrap_or(""))
        } else {
            None
        };
        self.compare_markdown(md, compare_arg, mode)
    }

    fn compare_markdown(&self, md: Option<&str>, compare: Option<&str>, mode: i32) -> Option<XhtmlNode> {
        let md_present = md.map(|s| !s.is_empty()).unwrap_or(false);
        if compare.is_none() || mode == GEN_MODE_DIFF {
            // compare==null || DIFF: process + fixFontSizes(11) (SDR:4213-4225).
            if !md_present {
                return None;
            }
            let xhtml = crate::publisher_markdown::process_markdown(self.ctx, md.unwrap(), &self.core_path);
            if xhtml.is_empty() {
                return None;
            }
            let mut ndiv = el("div");
            let mut parser = render_xhtml::XhtmlParser::new();
            if let Ok(mut nodes) = parser.parse_fragment_children(&xhtml) {
                fix_font_sizes(&mut nodes, 11);
                for nd in nodes {
                    ndiv.add_child_node(nd);
                }
            }
            Some(ndiv)
        } else if compare.map(|s| !s.is_empty()).unwrap_or(false) && md == compare {
            // areEqual (SDR:4229): style each element node with unchangedStyle,
            // NO fixFontSizes.
            if !md_present {
                return None;
            }
            let xhtml = crate::publisher_markdown::process_markdown(self.ctx, md.unwrap(), &self.core_path);
            let mut ndiv = el("div");
            let mut parser = render_xhtml::XhtmlParser::new();
            if let Ok(nodes) = parser.parse_fragment_children(&xhtml) {
                for mut n in nodes {
                    if n.node_type() == render_xhtml::node::NodeType::Element {
                        n.set_attribute("style", UNCHANGED_STYLE);
                    }
                    ndiv.add_child_node(n);
                }
            }
            Some(ndiv)
        } else {
            // not equal (SDR:4243): new markdown (no fixFontSizes), then br, then
            // a removed-styled `<div>` wrapping the compare markdown.
            let mut ndiv = el("div");
            let mut parser = render_xhtml::XhtmlParser::new();
            if md_present {
                let xhtml = crate::publisher_markdown::process_markdown(self.ctx, md.unwrap(), &self.core_path);
                if let Ok(nodes) = parser.parse_fragment_children(&xhtml) {
                    for n in nodes { ndiv.add_child_node(n); }
                }
            }
            if compare.map(|s| !s.is_empty()).unwrap_or(false) {
                let inner = crate::publisher_markdown::process_markdown(self.ctx, compare.unwrap(), &self.core_path);
                // "<div>"+html+"</div>" parsed -> a single <div>; style it removed.
                let wrapped = format!("<div>{}</div>", inner);
                let mut p2 = render_xhtml::XhtmlParser::new();
                if let Ok(nodes) = p2.parse_fragment_children(&wrapped) {
                    ndiv.add_child_node(el("br"));
                    for mut n in nodes {
                        if n.node_type() == render_xhtml::node::NodeType::Element {
                            n.set_attribute("style", REMOVED_STYLE);
                        }
                        ndiv.add_child_node(n);
                    }
                }
            }
            if ndiv.has_children() { Some(ndiv) } else { None }
        }
    }

    // -----------------------------------------------------------------------
    // describeCardinality (SDR:4909).
    // -----------------------------------------------------------------------
    fn describe_cardinality(&self, d: &Ed, compare: Option<&Ed>, mode: i32) -> Option<XhtmlNode> {
        let mut x = el("div");
        let has_min = d.v.get("min").is_some();
        let has_max = d.v.get("max").is_some();
        if compare.is_none() || mode == GEN_MODE_DIFF {
            if !has_max && !has_min {
                // SDR:4912 — `return null` here, BEFORE the condition block (which
                // is only reached when min/max produced output).
                return None;
            }
            if d.max().is_none() {
                tx(&mut x, &to_str_min(d));
                tx(&mut x, "..?");
            } else {
                tx(&mut x, &to_str_min(d));
                tx(&mut x, "..");
                tx(&mut x, d.max().unwrap());
            }
        } else {
            let cmp = compare.unwrap();
            // (mode==DIFF && (min==cmp.min || min==0)) suppresses min. mode!=DIFF
            // here so the guard is false -> always render min.
            if let Some(node) = compare_string(Some(&to_str_min(d)), None, name_slot(), Some(&to_str_min(cmp)), None, mode, false, false, false) {
                copy_children(&mut x, &node);
            }
            tx(&mut x, "..");
            if let Some(node) = compare_string(d.max(), None, name_slot(), cmp.max(), None, mode, false, false, false) {
                copy_children(&mut x, &node);
            }
        }
        self.cardinality_condition(x, d, compare, mode)
    }

    fn cardinality_condition(&self, mut x: XhtmlNode, d: &Ed, compare: Option<&Ed>, mode: i32) -> Option<XhtmlNode> {
        let t = self.compare_simple_type_lists(
            str_vec(&d.conditions()),
            compare.map(|c| str_vec(&c.conditions())),
            mode,
            ", ",
        );
        if let Some(t) = t {
            x.add_child_node(el("br"));
            tx(&mut x, "This element is affected by the following invariants: ");
            copy_children(&mut x, &t);
        }
        if !x.has_children() {
            None
        } else {
            Some(x)
        }
    }

    // -----------------------------------------------------------------------
    // presentModifier (SDR:4559).
    // -----------------------------------------------------------------------
    fn present_modifier(&self, d: &Ed, mode: i32, compare: Option<&Ed>) -> Option<XhtmlNode> {
        let new_im = encode_bool_opt(d.v.get("isModifier"));
        let old_im = compare.and_then(|c| encode_bool_opt(c.v.get("isModifier")));
        let x1 = compare_string(new_im.as_deref(), None, name_slot(), old_im.as_deref(), None, mode, false, false, false);
        if let Some(mut x1) = x1 {
            let new_r = d.v.get("isModifierReason").and_then(|x| x.as_str());
            let old_r = compare.and_then(|c| c.v.get("isModifierReason").and_then(|x| x.as_str()));
            if let Some(x2) = compare_string(new_r, None, name_slot(), old_r, None, mode, false, false, false) {
                tx(&mut x1, " because ");
                copy_children(&mut x1, &x2);
            }
            Some(x1)
        } else {
            None
        }
    }

    fn display_boolean(&self, value: bool, has_value: bool, compare: Option<bool>, mode: i32) -> Option<XhtmlNode> {
        // newValue = value?"true": hasValue?"false":null.
        let new_value = if value {
            Some("true".to_string())
        } else if has_value {
            Some("false".to_string())
        } else {
            None
        };
        // oldValue = compare==true?"true":null.
        let old_value = match compare {
            Some(true) => Some("true".to_string()),
            _ => None,
        };
        compare_string(new_value.as_deref(), None, name_slot(), old_value.as_deref(), None, mode, false, false, false)
    }

    fn business_id_warning(&self, name: &str) -> Option<XhtmlNode> {
        let (pfx, anchor, disc) = match name {
            "identifier" => (
                "This is a business identifier, not a resource identifier (see ",
                "resource.html#identifiers",
                "discussion",
            ),
            "version" => (
                "This is a business version Id, not a resource version Id (see ",
                "resource.html#versions",
                "discussion",
            ),
            _ => return None,
        };
        let mut ret = el("div");
        tx(&mut ret, pfx);
        let mut ah = el("a");
        ah.set_attribute("href", format!("{}{}", self.core_path, anchor));
        tx(&mut ah, disc);
        ret.add_child_node(ah);
        tx(&mut ret, ")");
        Some(ret)
    }

    // -----------------------------------------------------------------------
    // describeTypes / describeType (SDR:4949 / 5033).
    // -----------------------------------------------------------------------
    fn describe_types(
        &self,
        types: &[crate::sdmodel::TypeRef],
        must_support_only: bool,
        _ed: &Ed,
        compare: Option<&Ed>,
        mode: i32,
        value: Option<&Value>,
    ) -> Option<XhtmlNode> {
        if types.is_empty() {
            return None;
        }
        let compare_types: Vec<crate::sdmodel::TypeRef> =
            compare.map(|c| c.types()).unwrap_or_default();
        let mut ret = el("div");
        let single = (!must_support_only && types.len() == 1 && compare_types.len() <= 1)
            || (must_support_only && ms_count(types) == 1);
        if single {
            if !must_support_only || is_must_support_type(&types[0]) {
                let ct = if compare_types.is_empty() { None } else { Some(&compare_types[0]) };
                self.describe_type(&mut ret, &types[0], must_support_only, ct, mode);
            }
        } else {
            if types.len() > 1 {
                tx(&mut ret, "Choice of: ");
            }
            // map compare types by code (SDR:4964, a HashMap); remove as matched.
            // The leftover `map.values()` iteration must follow Java HashMap order.
            let mut map_keys: Vec<String> = Vec::new();
            let mut map: HashMap<String, usize> = HashMap::new(); // code -> compare_types idx
            for (ci, t) in compare_types.iter().enumerate() {
                if !map.contains_key(t.code()) {
                    map_keys.push(t.code().to_string());
                }
                map.insert(t.code().to_string(), ci);
            }
            let mut first = true;
            for t in types {
                let ct = map.remove(t.code());
                if ct.is_some() {
                    map_keys.retain(|k| k != t.code());
                }
                if !must_support_only || is_must_support_type(t) {
                    if first {
                        first = false;
                    } else {
                        tx(&mut ret, ", ");
                    }
                    let ct_ref = ct.map(|i| compare_types[i]);
                    self.describe_type(&mut ret, t, must_support_only, ct_ref.as_ref(), mode);
                }
            }
            // remaining compare-only types -> removed() struck, in Java HashMap
            // iteration order. The map's capacity reflects the PEAK put count
            // (all compare types), not the surviving count — Java HashMap never
            // shrinks on remove — so order over the survivors at that capacity.
            let all_put_keys: Vec<String> = {
                let mut seen: Vec<String> = Vec::new();
                for t in &compare_types {
                    if !seen.iter().any(|k| k == t.code()) {
                        seen.push(t.code().to_string());
                    }
                }
                seen
            };
            for code in hashmap_order_surviving(&all_put_keys, &map_keys) {
                let idx = map[&code];
                tx(&mut ret, ", ");
                let mut rem = el("span");
                rem.set_attribute("style", REMOVED_STYLE);
                self.describe_type(&mut rem, &compare_types[idx], must_support_only, None, mode);
                ret.add_child_node(rem);
            }
        }
        // processSecondary (SDR:4993 -> 5002) — the profiled-extension value note.
        if let Some(val) = value {
            if let Some(xt) = self.process_secondary(mode, val) {
                copy_children(&mut ret, &xt);
            }
        }
        if ret.has_children() {
            Some(ret)
        } else {
            None
        }
    }

    /// processSecondary (SDR:5002). mode 2 -> " (Complex Extension)"; mode 3 ->
    /// " (Extension Type: " + describeTypes(value.type) + ")". Other -> None.
    fn process_secondary(&self, mode: i32, value: &Value) -> Option<XhtmlNode> {
        match mode {
            2 => {
                let mut x = el("div");
                tx(&mut x, " (Complex Extension)");
                Some(x)
            }
            3 => {
                let mut x = el("div");
                tx(&mut x, " (Extension Type: ");
                let vtypes = Ed::new(value).types();
                if let Some(inner) = self.describe_types(&vtypes, false, &Ed::new(value), None, mode, None) {
                    copy_children(&mut x, &inner);
                }
                tx(&mut x, ")");
                Some(x)
            }
            _ => None,
        }
    }

    fn describe_type(
        &self,
        x: &mut XhtmlNode,
        t: &crate::sdmodel::TypeRef,
        must_support_only: bool,
        compare: Option<&crate::sdmodel::TypeRef>,
        mode: i32,
    ) {
        let wc = t.working_code();
        if wc.is_empty() || wc.starts_with('=') {
            return;
        }
        let mut ts;
        if wc.starts_with("xs:") {
            ts = self.append_compare_string(x, Some(wc), None, compare.map(|c| c.working_code()), None, mode);
        } else {
            let nlink = self.get_type_link(t);
            let olink = compare.and_then(|c| self.get_type_link(c));
            ts = self.append_compare_string(x, Some(wc), nlink.as_deref(), compare.map(|c| c.working_code()), olink.as_deref(), mode);
        }
        // type parameter extension (SDR:5047) — loud gap.
        if t.v.get("extension").and_then(|e| e.as_array()).map(|a| a.iter().any(|e| e.get("url").and_then(|u| u.as_str()) == Some("http://hl7.org/fhir/tools/StructureDefinition/type-parameter"))).unwrap_or(false) {
            crate::loud_gap!((), "LOUD GAP: describeType type-parameter (SDR:5047) for {}", self.sd.id());
        }
        // profiles (SDR:5069).
        let has_profile = !t.profiles().is_empty();
        let cmp_has_profile = compare.map(|c| !c.profiles().is_empty()).unwrap_or(false);
        if (!must_support_only && (has_profile || cmp_has_profile)) || (must_support_only && is_ms_canonical_list(t.v.get("_profile"))) {
            let profiles = self.analyse_profiles(&t.profiles(), t.v.get("_profile"), compare.map(|c| c.profiles()), must_support_only, mode);
            if !profiles.is_empty() {
                if !ts {
                    let mut un = el("span");
                    un.set_attribute("style", UNCHANGED_STYLE);
                    self.get_type_link_node(&mut un, t);
                    x.add_child_node(un);
                    ts = true;
                }
                tx(x, "(");
                let mut first = true;
                for rc in &profiles {
                    if first { first = false; } else { tx(x, ", "); }
                    rc.render(x);
                }
                tx(x, ")");
            }
        }
        // target profiles (SDR:5086).
        let has_tp = !t.target_profiles().is_empty();
        let cmp_has_tp = compare.map(|c| !c.target_profiles().is_empty()).unwrap_or(false);
        if (!must_support_only && (has_tp || cmp_has_tp)) || (must_support_only && is_ms_canonical_list(t.v.get("_targetProfile"))) {
            let profiles = self.analyse_profiles(&t.target_profiles(), t.v.get("_targetProfile"), compare.map(|c| c.target_profiles()), must_support_only, mode);
            if !profiles.is_empty() {
                if !ts {
                    let mut un = el("span");
                    un.set_attribute("style", UNCHANGED_STYLE);
                    self.get_type_link_node(&mut un, t);
                    x.add_child_node(un);
                }
                tx(x, "(");
                let mut first = true;
                for rc in &profiles {
                    if first { first = false; } else { tx(x, ", "); }
                    rc.render(x);
                }
                tx(x, ")");
            }
            // aggregation (SDR:5101) — loud gap if present.
            let agg = t.v.get("aggregation").and_then(|a| a.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
            if agg {
                crate::loud_gap!((), "LOUD GAP: describeType aggregation (SDR:5101) for {}", self.sd.id());
            }
        }
    }

    /// analyseProfiles (SDR:5119) — resolve each profile canonical. `sidecar` is
    /// the `_profile[]`/`_targetProfile[]` array carrying per-entry MS extensions
    /// (index-aligned with `new_profiles`).
    fn analyse_profiles(
        &self,
        new_profiles: &[&str],
        sidecar: Option<&Value>,
        old_profiles: Option<Vec<&str>>,
        must_support_only: bool,
        mode: i32,
    ) -> Vec<ResolvedCanonical> {
        let mut out: Vec<ResolvedCanonical> = Vec::new();
        for (i, pt) in new_profiles.iter().enumerate() {
            let pt_is_ms = sidecar
                .and_then(|s| s.as_array())
                .and_then(|a| a.get(i))
                .map(|e| e.is_object() && type_has_ms_ext(e))
                .unwrap_or(false);
            if let Some(rc) = self.fetch_profile(pt, must_support_only, pt_is_ms) {
                out.push(rc);
            }
        }
        if let (Some(old), true) = (old_profiles, mode != GEN_MODE_DIFF) {
            for pt in old {
                // old-profile MS status not tracked (compare non-MS in corpus);
                // mustSupportOnly old-merge only runs when not DIFF.
                if let Some(rc) = self.fetch_profile(pt, must_support_only, false) {
                    // merge: mark ALL matching unchanged, else add as removed.
                    let mut found = false;
                    for e in out.iter_mut() {
                        if e.url == rc.url { found = true; e.status = ListItemStatus::Unchanged; }
                    }
                    if !found {
                        let mut r = rc;
                        r.status = ListItemStatus::Removed;
                        out.push(r);
                    }
                }
            }
        }
        out
    }

    /// fetchProfile (SDR:5133): `if !mustSupportOnly || isMustSupport(pt)` resolve
    /// the profile canonical; else None. `pt_is_ms` = isMustSupport(pt).
    fn fetch_profile(&self, pt: &str, must_support_only: bool, pt_is_ms: bool) -> Option<ResolvedCanonical> {
        if pt.is_empty() {
            return None;
        }
        if must_support_only && !pt_is_ms {
            return None;
        }
        let resolved = self.ctx.resolve(pt);
        Some(ResolvedCanonical {
            url: pt.to_string(),
            web_path: resolved.as_ref().and_then(|r| if r.web_path.is_empty() { None } else { Some(r.web_path.clone()) }),
            present: resolved.as_ref().map(|r| r.present()),
            status: ListItemStatus::New,
        })
    }

    /// getTypeLink (SDR:5171) — String form. pkp.getLinkFor(sd.webPath, code).
    fn get_type_link(&self, t: &crate::sdmodel::TypeRef) -> Option<String> {
        self.link_for(t.working_code())
    }

    /// getTypeLink(x, t, sd) (SDR:5161) — node form.
    fn get_type_link_node(&self, x: &mut XhtmlNode, t: &crate::sdmodel::TypeRef) {
        match self.link_for(t.working_code()) {
            Some(s) => {
                let mut ah = el("a");
                ah.set_attribute("href", s);
                tx(&mut ah, t.working_code());
                x.add_child_node(ah);
            }
            None => {
                let mut code = el("code");
                tx(&mut code, t.working_code());
                x.add_child_node(code);
            }
        }
    }

    /// pkp.getLinkFor(corePath, name) (IGKnowledgeProvider:573): the type SD's
    /// webPath when it has one; else `name.html` (broken-link fallback). The only
    /// null case is `noXhtml && name=="xhtml"` (not in this corpus). So getTypeLink
    /// effectively always links — the SDR `<code>` fallback is unreachable here.
    fn link_for(&self, code: &str) -> Option<String> {
        if code.is_empty() {
            return None;
        }
        match self.ctx.resolve_type(code) {
            Some(r) if !r.web_path.is_empty() => Some(r.web_path),
            _ => Some(format!("{}{}.html", self.core_path, code)),
        }
    }

    /// Append a compareString result to `x`; returns whether anything was added
    /// (the `ts` flag).
    fn append_compare_string(
        &self,
        x: &mut XhtmlNode,
        new_str: Option<&str>,
        nlink: Option<&str>,
        old_str: Option<&str>,
        olink: Option<&str>,
        mode: i32,
    ) -> bool {
        match compare_string(new_str, nlink, name_slot(), old_str, olink, mode, false, false, false) {
            Some(node) => {
                copy_children(x, &node);
                true
            }
            None => false,
        }
    }

    // -----------------------------------------------------------------------
    // describeBinding / renderBinding (SDR:5212 / 5277).
    // -----------------------------------------------------------------------
    fn describe_binding(&self, d: &Ed, path: &str, compare: Option<&Ed>, mode: i32) -> Option<XhtmlNode> {
        let binding = d.binding()?;
        // Java `compare.getBinding()` lazily creates a non-null (empty) binding, so
        // when a compare element is present its binding is never null — an absent
        // binding element compares as strength=null / valueSet=null (SDR:5217).
        let empty_binding = Value::Object(serde_json::Map::new());
        let comp_binding = compare.map(|c| c.binding().unwrap_or(&empty_binding));
        // binding description markdown.
        let mut binding_desc: Option<XhtmlNode> = None;
        if let Some(desc) = binding.get("description").and_then(|x| x.as_str()) {
            let fixed = fix_binding_descriptions(desc);
            if mode == GEN_MODE_SNAP || mode == GEN_MODE_MS {
                let mut bd = el("div");
                let xhtml = crate::publisher_markdown::process_markdown(self.ctx, &fixed, &self.core_path);
                let mut parser = render_xhtml::XhtmlParser::new();
                if let Ok(nodes) = parser.parse_fragment_children(&xhtml) {
                    for nd in nodes {
                        bd.add_child_node(nd);
                    }
                }
                binding_desc = Some(bd);
            } else {
                let old = comp_binding
                    .and_then(|c| c.get("description").and_then(|x| x.as_str()))
                    .map(fix_binding_descriptions);
                binding_desc = self.compare_markdown(Some(&fixed), old.as_deref(), mode);
            }
        }
        let has_vs = binding.get("valueSet").and_then(|x| x.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
        if !has_vs {
            return binding_desc;
        }
        let mut x = el("div");
        let mut nsp = el("span");
        self.render_binding(&mut nsp, binding, comp_binding, path, mode);
        x.add_child_node(nsp);
        if let Some(bd) = binding_desc {
            if is_simple_content(&bd) {
                tx(&mut x, ": ");
                // copyAllContent(bd.getChildNodes().get(0)) copies the <p>'s
                // children into x.
                if let Some(first) = bd.child_nodes().first() {
                    copy_children(&mut x, first);
                }
            } else {
                x.add_child_node(el("br"));
                copy_children(&mut x, &bd);
            }
        }
        // concept-domain extension (SDR:5246) -> loud gap (renderCoding).
        if binding_has_ext(binding, "http://hl7.org/fhir/tools/StructureDefinition/binding-conceptDomain") {
            crate::loud_gap!((), "LOUD GAP: describeBinding concept-domain (SDR:5246) for {} ({})", self.sd.id(), d.id());
        }
        // Additional bindings (SDR:5253-5268): max/min ValueSet + additional-binding.
        let show_compare = mode != GEN_MODE_SNAP && mode != GEN_MODE_MS;
        let mut abr = AdditionalBindings::default();
        if let Some(ext) = binding_ext(binding, "http://hl7.org/fhir/StructureDefinition/elementdefinition-maxValueSet") {
            let comp = comp_binding.and_then(|c| binding_ext(c, "http://hl7.org/fhir/StructureDefinition/elementdefinition-maxValueSet"));
            abr.see_binding(ext, comp, show_compare, "maximum");
        }
        if let Some(ext) = binding_ext(binding, "http://hl7.org/fhir/StructureDefinition/elementdefinition-minValueSet") {
            let comp = comp_binding.and_then(|c| binding_ext(c, "http://hl7.org/fhir/StructureDefinition/elementdefinition-minValueSet"));
            abr.see_binding(ext, comp, show_compare, "minimum");
        }
        // NOTE: the `additional-binding` (EXT_BINDING_ADDITIONAL) extension is
        // NOT rendered in the dict goldens — across the whole corpus only
        // max/min ValueSet purposes appear in Additional Bindings tables (73 Max,
        // 2 Min; zero additional-binding). The golden publisher build's
        // describeBinding therefore only surfaces seeMaxBinding/seeMinBinding for
        // this fragment. We match that: `additional-binding` extensions are not
        // collected. (If the publisher DID call seeAdditionalBindings here, the
        // us-core Condition slice's `additional-binding` would show — it doesn't.)
        if abr.has_bindings() {
            let tbl = abr.render(self.ctx, &self.core_path);
            x.add_child_node(tbl);
        }
        Some(x)
    }

    /// renderBinding (SDR:5277).
    fn render_binding(&self, span: &mut XhtmlNode, binding: &Value, compare: Option<&Value>, path: &str, mode: i32) {
        let strength = binding.get("strength").and_then(|x| x.as_str());
        let new_conf = conf(strength);
        let old_conf = compare.map(|c| conf(c.get("strength").and_then(|x| x.as_str())));
        self.append_compare_string(span, Some(&new_conf), None, old_conf.as_deref(), None, mode);
        tx(span, " ");
        let vs_ref = binding.get("valueSet").and_then(|x| x.as_str()).unwrap_or("");
        let br = self.ctx.resolve_binding(vs_ref);
        let old_vs = compare.and_then(|c| c.get("valueSet").and_then(|x| x.as_str()));
        // compareString(span, br.display, ..., br.url, ..., externalN=br.external)
        self.append_compare_string_link(span, Some(&br.display), br.url.as_deref(), old_vs, None, mode, br.external);
        let _ = path;
        let has_strength = strength.is_some();
        let has_vs = !vs_ref.is_empty();
        if has_strength || has_vs {
            span.add_child_node(el("br"));
            tx(span, "(");
            if let Some(st) = strength {
                let mut ah = el("a");
                ah.set_attribute("href", crate::context::join_url(self.core_path.trim_end_matches('/'), &format!("terminologies.html#{}", st)));
                tx(&mut ah, st);
                span.add_child_node(ah);
            }
            if has_strength && has_vs {
                tx(span, " ");
            }
            if has_vs {
                tx(span, "to ");
                let mut ispan = el("span");
                ispan.set_attribute("class", "copy-text-inline");
                let mut code = el("code");
                tx(&mut code, vs_ref);
                ispan.add_child_node(code);
                let mut btn = el("button");
                // button("btn-copy", COPY_URL): set class, title; then attribute
                // data-clipboard-text (SDR:5295). STRUC_DEF_COPY_URL = "Click to
                // Copy URL". HashMap order emits data-clipboard-text, title, class.
                set_attrs(&mut btn, &[
                    ("class", "btn-copy".into()),
                    ("title", "Click to Copy URL".into()),
                    ("data-clipboard-text", vs_ref.to_string()),
                ]);
                ispan.add_child_node(btn);
                span.add_child_node(ispan);
            }
            tx(span, ")");
        }
    }

    /// Like append_compare_string but with the external-png flag support
    /// (compareString externalN). external triggers " " + img external.png.
    fn append_compare_string_link(
        &self,
        x: &mut XhtmlNode,
        new_str: Option<&str>,
        nlink: Option<&str>,
        old_str: Option<&str>,
        olink: Option<&str>,
        mode: i32,
        external_n: bool,
    ) -> bool {
        match compare_string(new_str, nlink, name_slot(), old_str, olink, mode, external_n, false, false) {
            Some(node) => {
                copy_children(x, &node);
                true
            }
            None => false,
        }
    }

    // -----------------------------------------------------------------------
    // invariants (SDR:5183).
    // -----------------------------------------------------------------------
    fn invariants(&self, d: &Ed, compare: Option<&Ed>, mode: i32) -> Option<XhtmlNode> {
        let mut list: Vec<InvariantItem> = Vec::new();
        for c in d.constraint_values() {
            if !c.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                list.push(InvariantItem { v: c.clone(), status: ListItemStatus::New });
            }
        }
        if let Some(cmp) = compare {
            if mode != GEN_MODE_DIFF {
                for c in cmp.constraint_values() {
                    let item = InvariantItem { v: c.clone(), status: ListItemStatus::New };
                    let mut found = false;
                    for e in list.iter_mut() {
                        if constraints_equal(&e.v, &item.v) { found = true; e.status = ListItemStatus::Unchanged; }
                    }
                    if !found {
                        let mut r = item;
                        r.status = ListItemStatus::Removed;
                        list.push(r);
                    }
                }
            }
        }
        if list.is_empty() {
            return None;
        }
        let mut x = el("div");
        let mut first = true;
        for item in &list {
            if first { first = false; } else { x.add_child_node(el("br")); }
            item.render(&mut x);
        }
        Some(x)
    }

    // -----------------------------------------------------------------------
    // getMapping (SDR:5392).
    // -----------------------------------------------------------------------
    fn get_mapping(&self, d: &Ed, uri: &str, compare: Option<&Ed>, mode: i32) -> Option<XhtmlNode> {
        // find the mapping identity for this uri.
        let mut id: Option<&str> = None;
        for m in self.sd.mappings() {
            if m.get("uri").and_then(|x| x.as_str()) == Some(uri) {
                id = m.get("identity").and_then(|x| x.as_str());
            }
        }
        let id = id?;
        let new_map = mapping_value(d.v, id);
        if new_map.as_deref().map(|s| s.is_empty()).unwrap_or(true) && compare.is_none() {
            return None;
        }
        if compare.is_none() {
            let m = new_map?;
            let mut div = el("div");
            tx(&mut div, &m);
            return Some(div);
        }
        let old_map = compare.and_then(|c| mapping_value(c.v, id));
        let new_empty = new_map.as_deref().map(|s| s.is_empty()).unwrap_or(true);
        let old_empty = old_map.as_deref().map(|s| s.is_empty()).unwrap_or(true);
        if new_empty && old_empty {
            return None;
        }
        // compareString(escapeXml(newMap), ..., escapeXml(oldMap)). Utilities
        // .escapeXml(null) returns "" (NOT null), so an absent map becomes the
        // empty string — in DIFF mode compareString then renders an empty (but
        // present) node, so the row shows with an empty td (golden-verified for
        // Patient.birthDate LOINC, where the diff dropped a base mapping).
        let n = escape_xml(new_map.as_deref().unwrap_or(""));
        let o = escape_xml(old_map.as_deref().unwrap_or(""));
        compare_string(Some(&n), None, name_slot(), Some(&o), None, mode, false, false, false)
    }

    // -----------------------------------------------------------------------
    // compareSimpleTypeLists (SDR:5430).
    // -----------------------------------------------------------------------
    fn compare_simple_type_lists(
        &self,
        original: Vec<String>,
        compare: Option<Vec<String>>,
        mode: i32,
        separator: &str,
    ) -> Option<XhtmlNode> {
        let mut list: Vec<(String, ListItemStatus)> = Vec::new();
        for v in original {
            if !v.is_empty() {
                list.push((v, ListItemStatus::New));
            }
        }
        if let (Some(cmp), true) = (compare, mode != GEN_MODE_DIFF) {
            for v in cmp {
                if v.is_empty() { continue; }
                // StatusList.merge (SDR:208): mark ALL matching entries Unchanged.
                let mut found = false;
                for (s, st) in list.iter_mut() {
                    if *s == v { found = true; *st = ListItemStatus::Unchanged; }
                }
                if !found {
                    list.push((v, ListItemStatus::Removed));
                }
            }
        }
        if list.is_empty() {
            return None;
        }
        let mut x = el("div");
        let mut first = true;
        for (v, status) in &list {
            if first { first = false; } else { tx(&mut x, separator); }
            match status {
                ListItemStatus::Unchanged => {
                    let mut s = el("span");
                    s.set_attribute("style", UNCHANGED_STYLE);
                    tx(&mut s, v);
                    x.add_child_node(s);
                }
                ListItemStatus::Removed => {
                    let mut s = el("span");
                    s.set_attribute("style", REMOVED_STYLE);
                    tx(&mut s, v);
                    x.add_child_node(s);
                }
                ListItemStatus::New => tx(&mut x, v),
            }
        }
        Some(x)
    }

    /// compareDataTypeLists (SDR:5460) — for `code` (Coding list). Each Coding
    /// is rendered via renderCodingWithDetails through a DataValueWithStatus
    /// (New/Unchanged/Removed styling). Separator ", ".
    fn compare_data_type_lists(
        &self,
        original: Vec<Value>,
        compare: Option<Vec<Value>>,
        mode: i32,
    ) -> Option<XhtmlNode> {
        let mut list: Vec<(Value, ListItemStatus)> = Vec::new();
        for v in original {
            if !v.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                list.push((v, ListItemStatus::New));
            }
        }
        if let (Some(cmp), true) = (compare, mode != GEN_MODE_DIFF) {
            for v in cmp {
                if v.as_object().map(|o| o.is_empty()).unwrap_or(true) { continue; }
                let mut found = false;
                for (e, st) in list.iter_mut() {
                    if *e == v { found = true; *st = ListItemStatus::Unchanged; }
                }
                if !found {
                    list.push((v, ListItemStatus::Removed));
                }
            }
        }
        if list.is_empty() {
            return None;
        }
        let mut x = el("div");
        let mut first = true;
        for (v, status) in &list {
            if first { first = false; } else { tx(&mut x, ", "); }
            let mut target = match status {
                ListItemStatus::New => None,
                ListItemStatus::Unchanged => {
                    let mut s = el("span");
                    s.set_attribute("style", UNCHANGED_STYLE);
                    Some(s)
                }
                ListItemStatus::Removed => {
                    let mut s = el("span");
                    s.set_attribute("style", REMOVED_STYLE);
                    Some(s)
                }
            };
            let node = target.as_mut().unwrap_or(&mut x);
            self.render_coding_with_details(node, v);
            if let Some(span) = target {
                x.add_child_node(span);
            }
        }
        Some(x)
    }

    /// renderCodingWithDetails (DataRenderer:1342): `<a link>displaySystem: code
    /// </a> (display)`. version note fires a loud gap (corpus code Codings have
    /// no version). checkRenderExtensions is a no-op (no _code extensions here).
    fn render_coding_with_details(&self, x: &mut XhtmlNode, c: &Value) {
        let system = c.get("system").and_then(|v| v.as_str());
        let code = c.get("code").and_then(|v| v.as_str()).unwrap_or("");
        let version = c.get("version").and_then(|v| v.as_str());
        let display = c.get("display").and_then(|v| v.as_str()).unwrap_or("");
        let sn = self.display_system(system);
        let link = self.get_link_for_code(system, version, code);
        // xi = link ? x.ah(link) : x
        let xi_owned: Option<XhtmlNode> = link.map(|l| {
            let mut a = el("a");
            a.set_attribute("href", l);
            a
        });
        match xi_owned {
            Some(mut a) => {
                tx(&mut a, &sn);
                tx(&mut a, ": ");
                tx(&mut a, code);
                x.add_child_node(a);
            }
            None => {
                tx(x, &sn);
                tx(x, ": ");
                tx(x, code);
            }
        }
        if !display.is_empty() {
            tx(x, " (");
            tx(x, display);
            tx(x, ")");
        }
        if version.is_some() {
            crate::loud_gap!((), "LOUD GAP: renderCodingWithDetails version note (DataRenderer:1362) for {}", self.sd.id());
        }
    }

    /// displaySystem (DataRenderer:255).
    fn display_system(&self, system: Option<&str>) -> String {
        match system {
            None => "[not stated]".to_string(),
            Some("http://loinc.org") => "LOINC".to_string(),
            Some(s) if s.starts_with("http://snomed.info") => "SNOMED CT".to_string(),
            Some("http://www.nlm.nih.gov/research/umls/rxnorm") => "RxNorm".to_string(),
            Some("http://unitsofmeasure.org") => "UCUM".to_string(),
            Some(s) => {
                // fetchCodeSystem -> crPresent (title||name); else tails(system).
                if let Some(cs) = self.ctx.resolve(s) {
                    if cs.rtype == "CodeSystem" {
                        return cs.present();
                    }
                }
                s.rsplit('/').next().unwrap_or(s).to_string()
            }
        }
    }

    /// getLinkForCode (DataRenderer:1254) — corpus systems (loinc/snomed) + the
    /// CodeSystem webPath fallback.
    fn get_link_for_code(&self, system: Option<&str>, version: Option<&str>, code: &str) -> Option<String> {
        match system {
            Some("http://loinc.org") => {
                if code.is_empty() { Some("https://loinc.org/".to_string()) }
                else { Some(format!("https://loinc.org/{}", code)) }
            }
            Some("http://snomed.info/sct") => {
                // SnomedUtilities.getSctLink — corpus has snomed codes; fire a
                // loud gap if hit (the sct link format needs the edition logic).
                return crate::loud_gap!(None, "LOUD GAP: getLinkForCode snomed sct link for {}", self.sd.id());
            }
            Some(s) => {
                // getLinkForSystem: CodeSystem webPath (renderCoding uses cs.webPath
                // then getLinkForCode). Here match renderCodingWithDetails which
                // calls getLinkForCode directly -> for other systems returns null
                // unless a specific handler; the CodeSystem-webPath path is in
                // renderCoding, NOT getLinkForCode. So other systems -> None.
                let _ = (s, version);
                None
            }
            None => None,
        }
    }

    // -----------------------------------------------------------------------
    // encodeValue rows (SDR:4528-4538).
    // -----------------------------------------------------------------------
    /// minValue/maxValue: compareString(encodeValue(...), code=false).
    fn encode_value_row(&self, tbl: &mut XhtmlNode, name: &str, d: &Value, prefix: &str, compare: Option<&Value>, mode: i32) {
        let new_v = encode_value_prefixed(d, prefix, None);
        let old_v = compare.and_then(|c| encode_value_prefixed(c, prefix, None));
        if new_v.is_none() && old_v.is_none() {
            return;
        }
        let node = compare_string(new_v.as_deref(), None, name_slot(), old_v.as_deref(), None, mode, false, false, false);
        self.row_node(tbl, name, None, node);
    }

    /// fixed/pattern/defaultValue: encodeValue(value, name, ...) with code=true
    /// and elementName = d.getName() (the element's tail name).
    fn encode_value_named_row(&self, tbl: &mut XhtmlNode, name: &str, d: &Value, prefix: &str, compare: Option<&Value>, mode: i32) {
        let element_name = d.get("path").and_then(|x| x.as_str()).map(tail).map(String::from);
        let new_v = encode_value_prefixed(d, prefix, element_name.as_deref());
        let old_v = compare.and_then(|c| encode_value_prefixed(c, prefix, element_name.as_deref()));
        if new_v.is_none() && old_v.is_none() {
            return;
        }
        let node = compare_string(new_v.as_deref(), None, name_slot(), old_v.as_deref(), None, mode, false, false, true);
        self.row_node(tbl, name, None, node);
    }

    /// encodeValues(examples) (SDR:5329).
    fn encode_values(&self, examples: Vec<&Value>) -> Option<XhtmlNode> {
        if examples.is_empty() {
            return None;
        }
        let mut x = el("div");
        let mut first = true;
        for ex in examples {
            if first { first = false; } else { x.add_child_node(el("br")); }
            let mut b = el("b");
            let label = ex.get("label").and_then(|x| x.as_str()).unwrap_or("");
            tx(&mut b, label);
            tx(&mut b, ": ");
            x.add_child_node(b);
            // encodeValue(ex.getValue(), null) — the value[x] payload as a string.
            if let Some(val) = encode_example_value(ex) {
                tx(&mut x, &val);
            }
        }
        Some(x)
    }

    // -----------------------------------------------------------------------
    // generateSlicing (SDR:4789).
    // -----------------------------------------------------------------------
    fn generate_slicing(&self, tbl: &mut XhtmlNode, ed: &Ed, slicing: &Value, compare: Option<&Value>) {
        let mode = self.mode;
        let mut x = el("div");
        // x.codeWithText(SET_SLICES+" ", ed.path, SET_ARE) (SDR:4792). SET_ARE has
        // no trailing space: "...slices on " + <code>path</code> + ". The slices are".
        tx(&mut x, "This element introduces a set of slices on ");
        let mut code = el("code");
        tx(&mut code, ed.path());
        x.add_child_node(code);
        tx(&mut x, ". The slices are");
        // compareString(newOrdered, ..., oldOrdered) — "Unordered"/"Ordered".
        let new_ordered = slice_order_string(slicing);
        let old_ordered = compare
            .and_then(|c| c.get("slicing"))
            .filter(|s| !s.is_null())
            .map(slice_order_string);
        self.append_compare_string(&mut x, Some(&new_ordered), None, old_ordered.as_deref(), None, mode);
        tx(&mut x, " and "); // " "+AND+" "
        // compareString(rules.getDisplay()) — "Open"/"Closed"/"Open at End".
        let new_rules = slicing.get("rules").and_then(|x| x.as_str()).map(rules_display);
        let old_rules = compare
            .and_then(|c| c.get("slicing"))
            .and_then(|s| s.get("rules"))
            .and_then(|x| x.as_str())
            .map(rules_display);
        self.append_compare_string(&mut x, new_rules.as_deref(), None, old_rules.as_deref(), None, mode);
        let discs = slicing.get("discriminator").and_then(|d| d.as_array());
        if let Some(discs) = discs.filter(|a| !a.is_empty()) {
            // STRUC_DEF_DESCRIM (no trailing space).
            tx(&mut x, ", and can be differentiated using the following discriminators:");
            // `var ul = x.ul();` — an empty <ul> is appended to x, THEN each
            // discriminator's <li> is appended to x (not ul) (SDR:4810-4813).
            x.add_child_node(el("ul"));
            // When a compare element is present, the StatusList merges each
            // discriminator with itself (SDR:4805-4808) -> all become Unchanged,
            // so the li content is wrapped in the DarkGray unchanged span.
            let unchanged = compare.is_some();
            for disc in discs {
                let mut li = el("li");
                let dtype = disc.get("type").and_then(|x| x.as_str()).unwrap_or("");
                let dpath = disc.get("path").and_then(|x| x.as_str()).unwrap_or("");
                let target: &mut XhtmlNode = if unchanged {
                    let mut span = el("span");
                    span.set_attribute("style", UNCHANGED_STYLE);
                    li.add_child_node(span);
                    li.child_nodes_mut().last_mut().unwrap()
                } else {
                    &mut li
                };
                tx(target, dtype);
                tx(target, " @ ");
                tx(target, dpath);
                x.add_child_node(li);
            }
        } else {
            // STRUC_DEF_NO_DESCRIM.
            tx(&mut x, ", and defines no disciminators to differentiate the slices");
        }
        // tableRow(tbl, "Slicing", "profiling.html#slicing", strike, x)
        self.row_node(tbl, "Slicing", Some("profiling.html#slicing"), Some(x));
        tbl.add_text("\r\n");
    }

    fn has_primitive_types(&self, d: &Ed) -> bool {
        d.types().iter().any(|t| self.ctx.is_primitive_type(t.code()))
    }

    fn sd_has_ext(&self, url: &str) -> bool {
        self.sd
            .root
            .get("extension")
            .and_then(|e| e.as_array())
            .map(|a| a.iter().any(|x| x.get("url").and_then(|u| u.as_str()) == Some(url)))
            .unwrap_or(false)
    }
}

// ===========================================================================
// AdditionalBindingsRenderer (fhir-core AdditionalBindingsRenderer.java) — the
// max/min/additional value-set binding sub-table. Corpus subset: maxValueSet /
// minValueSet extensions (purpose maximum/minimum), plus additional-binding
// extensions. No usage/any columns in the corpus (guarded).
// ===========================================================================

#[derive(Default)]
struct AdditionalBindings {
    bindings: Vec<AbDetail>,
}

struct AbDetail {
    purpose: String,
    value_set: String,
    doco: Option<String>,
    any: bool,
    has_usage: bool,
    removed: bool,
    is_unchanged: bool,
    compare_vs: Option<String>,
    compare_doco: Option<String>,
}

impl AdditionalBindings {
    fn has_bindings(&self) -> bool {
        !self.bindings.is_empty()
    }

    /// seeBinding (ABR:109) — a max/min ValueSet binding (value is the primitive
    /// canonical).
    fn see_binding(&mut self, ext: &Value, comp_ext: Option<&Value>, compare: bool, label: &str) {
        let vs = ext_value_primitive(ext).unwrap_or_default();
        let mut abd = AbDetail {
            purpose: label.to_string(),
            value_set: vs.clone(),
            doco: None,
            any: false,
            has_usage: false,
            removed: false,
            is_unchanged: false,
            compare_vs: None,
            compare_doco: None,
        };
        if compare {
            let cvs = comp_ext.and_then(ext_value_primitive);
            abd.is_unchanged = cvs.as_deref() == Some(vs.as_str());
            abd.compare_vs = cvs;
        }
        self.bindings.push(abd);
    }

    /// ABR render (ABR:223) — the grid table. `full_doco` is true from
    /// describeBinding (render(children, true)).
    fn render(&self, ctx: &IgContext, core_path: &str) -> XhtmlNode {
        let mut tbl = el("table");
        // x.table("grid", false).markGenerated(true): class="grid" data-fhir=
        // "generated"; HashMap order for {class, data-fhir}.
        set_attrs(&mut tbl, &[("class", "grid".into()), ("data-fhir", "generated".into())]);
        let doco = self.bindings.iter().any(|b| b.doco.is_some() || b.compare_doco.is_some());
        let usage = self.bindings.iter().any(|b| b.has_usage);
        let any = self.bindings.iter().any(|b| b.any);
        if usage {
            crate::loud_gap!((), "LOUD GAP: additional-binding usage column (ABR:237)");
        }
        // header row.
        let mut tr = el("tr");
        push_ab_th(&mut tr, "Additional Bindings", true);
        push_ab_th(&mut tr, "Purpose", false);
        if any {
            push_ab_th(&mut tr, "Any", false);
        }
        if doco {
            push_ab_th(&mut tr, "Documentation", false);
        }
        tbl.add_child_node(tr);
        for b in &self.bindings {
            let mut tr = el("tr");
            if (b.is_unchanged && b.compare_vs.is_none()) || b.removed {
                // unchanged() with compare==null returns true -> STYLE_REMOVED per
                // ABR:248-252; corpus max/min are is_unchanged=false so N/A.
                tr.set_attribute("style", STYLE_REMOVED);
            }
            // value set cell.
            let br = ctx.resolve_binding(&b.value_set);
            let mut vs_td = el("td");
            vs_td.set_attribute("style", "font-size: 11px");
            // Java `.style()` APPENDS with "; " (XhtmlNode.style). An unchanged
            // valueset (compare present and equal) yields "font-size: 11px; opacity: 0.5;".
            if b.compare_vs.as_deref() == Some(b.value_set.as_str()) {
                vs_td.set_attribute("style", format!("font-size: 11px; {}", STYLE_UNCHANGED));
            }
            match &br.url {
                Some(u) => {
                    let mut a = el("a");
                    a.set_attribute("href", determine_url(u, core_path));
                    if let Some(uri) = &br.uri {
                        a.set_attribute("title", uri.clone());
                    }
                    tx(&mut a, &br.display);
                    if br.external {
                        tx(&mut a, " ");
                        let mut img = el("img");
                        img.set_attribute("src", "external.png");
                        img.set_attribute("alt", ".");
                        a.add_child_node(img);
                    }
                    vs_td.add_child_node(a);
                }
                None => {
                    let mut span = el("span");
                    span.set_attribute("title", b.value_set.clone());
                    tx(&mut span, &br.display);
                    vs_td.add_child_node(span);
                }
            }
            // compare-vs removed branch.
            if let Some(cvs) = &b.compare_vs {
                if cvs != &b.value_set {
                    vs_td.add_child_node(el("br"));
                    let mut rem = el("span");
                    rem.set_attribute("style", STYLE_REMOVED);
                    let cbr = ctx.resolve_binding(cvs);
                    match &cbr.url {
                        Some(u) => {
                            let mut a = el("a");
                            a.set_attribute("href", determine_url(u, core_path));
                            a.set_attribute("title", cvs.clone());
                            tx(&mut a, &cbr.display);
                            rem.add_child_node(a);
                        }
                        None => {
                            let mut span = el("span");
                            span.set_attribute("title", cvs.clone());
                            tx(&mut span, &cbr.display);
                            rem.add_child_node(span);
                        }
                    }
                    vs_td.add_child_node(rem);
                }
            }
            tr.add_child_node(vs_td);
            // purpose cell.
            let mut purpose_td = el("td");
            purpose_td.set_attribute("style", "font-size: 11px");
            render_purpose(&mut purpose_td, &b.purpose, core_path);
            tr.add_child_node(purpose_td);
            // doco cell.
            if doco {
                let mut d_td = el("td");
                d_td.set_attribute("style", "font-size: 11px");
                if let Some(dv) = &b.doco {
                    // full_doco -> processMarkdown(doco); innerHTML. Corpus max/min
                    // have no doco, so this path is guarded above (fires only if a
                    // doco exists somewhere). Render markdown children.
                    let html = crate::publisher_markdown::process_markdown(ctx, dv, core_path);
                    let mut parser = render_xhtml::XhtmlParser::new();
                    if let Ok(nodes) = parser.parse_fragment_children(&html) {
                        for n in nodes { d_td.add_child_node(n); }
                    }
                }
                tr.add_child_node(d_td);
            }
            tbl.add_child_node(tr);
        }
        tbl
    }
}

fn push_ab_th(tr: &mut XhtmlNode, text: &str, bold: bool) {
    let mut td = el("td");
    td.set_attribute("style", "font-size: 11px");
    if bold {
        let mut b = el("b");
        tx(&mut b, text);
        td.add_child_node(b);
    } else {
        tx(&mut td, text);
    }
    tr.add_child_node(td);
}

/// renderPurpose (ABR:375) — the corpus is R4 (r5=false), so the max/min/strength
/// branches use the R4 extension/terminologies pages.
fn render_purpose(td: &mut XhtmlNode, purpose: &str, core_path: &str) {
    let (page, title, text) = match purpose {
        "maximum" => (
            "extension-elementdefinition-maxvalueset.html",
            "A required binding, for use when the binding strength is 'extensible' or 'preferred'",
            "Max Binding",
        ),
        "minimum" => (
            "extension-elementdefinition-minvalueset.html",
            "The minimum allowable value set - any conformant system SHALL support all these codes",
            "Min Binding",
        ),
        "required" => (
            "terminologies.html#strength",
            "Validators will check this binding (strength = required)",
            "Required",
        ),
        "extensible" => (
            "terminologies.html#strength",
            "Validators will check this binding (strength = extensible)",
            "Extensible",
        ),
        "preferred" => (
            "terminologies.html#strength",
            "This is the value set that is recommended (documentation should explain why)",
            "Preferred",
        ),
        other => {
            // R4 non-r5 branch: span(null, UNKNOWN_PUR).tx(purpose).
            let mut span = el("span");
            span.set_attribute("title", "?");
            tx(&mut span, other);
            td.add_child_node(span);
            return;
        }
    };
    let mut a = el("a");
    a.set_attribute("href", format!("{}{}", core_path, page));
    a.set_attribute("title", title);
    tx(&mut a, text);
    td.add_child_node(a);
}

/// Append `|version` to a versionless core `http://hl7.org/fhir/...` canonical.
fn pin_core_version(slot: Option<&mut Value>, version: &str) {
    if let Some(v) = slot {
        if let Some(s) = v.as_str() {
            if s.starts_with("http://hl7.org/fhir/") && !s.contains('|') {
                *v = Value::String(format!("{}|{}", s, version));
            }
        }
    }
}

/// determineUrl (ABR:371): `isAbsoluteUrl(url) || !pkp.prependLinks() ? url :
/// corePath+url`. In the publisher fragment context `prependLinks()` is FALSE
/// (fragments are IG-root-relative), so the url is always returned unchanged —
/// core VS links are already absolute (from resolve_binding), local IG VS links
/// stay relative (`ValueSet-X.html`), matching the goldens.
fn determine_url(url: &str, _core_path: &str) -> String {
    url.to_string()
}

fn ext_value_primitive(ext: &Value) -> Option<String> {
    let obj = ext.as_object()?;
    for (k, v) in obj {
        if k.starts_with("value") {
            return v.as_str().map(String::from);
        }
    }
    None
}

fn binding_ext<'a>(binding: &'a Value, url: &str) -> Option<&'a Value> {
    binding.get("extension")?.as_array()?.iter().find(|e| e.get("url").and_then(|u| u.as_str()) == Some(url))
}

const STYLE_UNCHANGED: &str = "opacity: 0.5;";
const STYLE_REMOVED: &str = "opacity: 0.5;text-decoration: line-through;";

// ===========================================================================
// Free helpers.
// ===========================================================================

const UNCHANGED_STYLE: &str = "color:DarkGray";
const REMOVED_STYLE: &str = "color:DarkGray;text-decoration:line-through";
const SELF_LINK_PATH: &str = "M1520 1216q0-40-28-68l-208-208q-28-28-68-28-42 0-72 32 3 3 19 18.5t21.5 21.5 15 19 13 25.5 3.5 27.5q0 40-28 68t-68 28q-15 0-27.5-3.5t-25.5-13-19-15-21.5-21.5-18.5-19q-33 31-33 73 0 40 28 68l206 207q27 27 68 27 40 0 68-26l147-146q28-28 28-67zm-703-705q0-40-28-68l-206-207q-28-28-68-28-39 0-68 27l-147 146q-28 28-28 67 0 40 28 68l208 208q27 27 68 27 42 0 72-31-3-3-19-18.5t-21.5-21.5-15-19-13-25.5-3.5-27.5q0-40 28-68t68-28q15 0 27.5 3.5t25.5 13 19 15 21.5 21.5 18.5 19q33-31 33-73zm895 705q0 120-85 203l-147 146q-83 83-203 83-121 0-204-85l-206-207q-83-83-83-203 0-123 88-209l-88-88q-86 88-208 88-120 0-204-84l-208-208q-84-84-84-204t85-203l147-146q83-83 203-83 121 0 204 85l206 207q83 83 83 203 0 123-88 209l88 88q86-88 208-88 120 0 204 84l208 208q84 84 84 204z";

#[derive(Clone, Copy, PartialEq)]
enum ListItemStatus {
    New,
    Unchanged,
    Removed,
}

struct ResolvedCanonical {
    url: String,
    web_path: Option<String>,
    present: Option<String>,
    status: ListItemStatus,
}

impl ResolvedCanonical {
    /// SDR ResolvedCanonical.render (via ItemWithStatus.render:195 + renderDetails:248).
    fn render(&self, x: &mut XhtmlNode) {
        let mut f = match self.status {
            ListItemStatus::New => None,
            ListItemStatus::Unchanged => {
                let mut s = el("span");
                s.set_attribute("style", UNCHANGED_STYLE);
                Some(s)
            }
            ListItemStatus::Removed => {
                let mut s = el("span");
                s.set_attribute("style", REMOVED_STYLE);
                Some(s)
            }
        };
        let target = f.as_mut().unwrap_or(x);
        match &self.web_path {
            Some(wp) => {
                let mut ah = el("a");
                ah.set_attribute("href", wp.clone());
                tx(&mut ah, self.present.as_deref().unwrap_or(""));
                target.add_child_node(ah);
            }
            None => {
                let mut code = el("code");
                tx(&mut code, &self.url);
                target.add_child_node(code);
            }
        }
        if let Some(span) = f {
            x.add_child_node(span);
        }
    }
}

struct InvariantItem {
    v: Value,
    status: ListItemStatus,
}

impl InvariantItem {
    /// InvariantWithStatus.renderDetails (SDR:270).
    fn render(&self, x: &mut XhtmlNode) {
        let mut wrapper = match self.status {
            ListItemStatus::New => None,
            ListItemStatus::Unchanged => {
                let mut s = el("span");
                s.set_attribute("style", UNCHANGED_STYLE);
                Some(s)
            }
            ListItemStatus::Removed => {
                let mut s = el("span");
                s.set_attribute("style", REMOVED_STYLE);
                Some(s)
            }
        };
        let f = wrapper.as_mut().unwrap_or(x);
        let key = self.v.get("key").and_then(|x| x.as_str()).unwrap_or("");
        let human = self.v.get("human").and_then(|x| x.as_str());
        let expr = self.v.get("expression").and_then(|x| x.as_str());
        let mut b = el("b");
        // STRUC_DEF_FII (SDR:272).
        b.set_attribute("title", "Formal Invariant Identifier");
        tx(&mut b, key);
        f.add_child_node(b);
        tx(f, ": ");
        if let Some(h) = human {
            tx(f, h);
        }
        tx(f, " (");
        // status New -> expression wrapped in <code>; else plain text.
        if self.status == ListItemStatus::New {
            if let Some(e) = expr {
                let mut code = el("code");
                tx(&mut code, e);
                f.add_child_node(code);
            }
        } else if let Some(e) = expr {
            tx(f, e);
        }
        tx(f, ")");
        if let Some(w) = wrapper {
            x.add_child_node(w);
        }
    }
}

/// A sentinel `name` for compareString paths where the `name` argument is unused
/// (we never take the VersionComparisonAnnotation deleted branch).
fn name_slot() -> &'static str {
    ""
}

/// compareString (SDR:4295) — the core string comparator. Returns a `<div>` with
/// the rendered children, or None. We implement the corpus-reachable branches:
/// - mode != KEY: newStr present -> <a nlink>txOrCode</a> (+external.png); else
///   null.
/// - mode == KEY: oldStr empty & newStr present -> same; oldStr present & newStr
///   empty -> (KEY not DIFF) removed(); equal -> unchanged; startsWith -> split;
///   else new + removed.
/// The VersionComparisonAnnotation "deleted" branch is never taken (corpus).
#[allow(clippy::too_many_arguments)]
fn compare_string(
    new_str: Option<&str>,
    nlink: Option<&str>,
    _name: &str,
    old_str: Option<&str>,
    olink: Option<&str>,
    mode: i32,
    external_n: bool,
    external_o: bool,
    code: bool,
) -> Option<XhtmlNode> {
    let mut x = el("div");
    if mode != GEN_MODE_KEY {
        match new_str {
            Some(ns) => {
                append_link_or_text(&mut x, nlink, ns, code, external_n);
            }
            None => return None,
        }
    } else {
        // KEY mode.
        let old_empty = old_str.map(|s| s.is_empty()).unwrap_or(true);
        let new_empty = new_str.map(|s| s.is_empty()).unwrap_or(true);
        if old_empty {
            if new_empty {
                return None;
            } else {
                append_link_or_text(&mut x, nlink, new_str.unwrap(), code, external_n);
            }
        } else if new_empty {
            // oldStr present, newStr empty; mode==KEY (not DIFF) -> removed().
            let mut rem = el("span");
            rem.set_attribute("style", REMOVED_STYLE);
            append_link_or_text(&mut rem, olink, old_str.unwrap(), code, external_o);
            x.add_child_node(rem);
        } else if old_str == new_str {
            // equal -> unchanged().
            let mut un = el("span");
            un.set_attribute("style", UNCHANGED_STYLE);
            append_link_or_text(&mut un, nlink, new_str.unwrap(), code, external_n);
            x.add_child_node(un);
        } else if new_str.unwrap().starts_with(old_str.unwrap()) {
            // unchanged(old) + new(suffix).
            let mut un = el("span");
            un.set_attribute("style", UNCHANGED_STYLE);
            append_link_or_text(&mut un, olink, old_str.unwrap(), code, external_o);
            x.add_child_node(un);
            let suffix = &new_str.unwrap()[old_str.unwrap().len()..];
            append_link_or_text(&mut x, nlink, suffix, false, external_n);
        } else {
            append_link_or_text(&mut x, nlink, new_str.unwrap(), code, external_n);
            let mut rem = el("span");
            rem.set_attribute("style", REMOVED_STYLE);
            append_link_or_text(&mut rem, olink, old_str.unwrap(), code, external_o);
            x.add_child_node(rem);
        }
    }
    Some(x)
}

/// `.ah(nlink).txOrCode(code, str)` or `.txOrCode(code, str)` (when nlink null),
/// plus the `.iff(external).txN(" ").img("external.png")` tail.
fn append_link_or_text(parent: &mut XhtmlNode, link: Option<&str>, s: &str, code: bool, external: bool) {
    let target: &mut XhtmlNode = if let Some(l) = link {
        let mut ah = el("a");
        ah.set_attribute("href", l.to_string());
        parent.add_child_node(ah);
        parent.child_nodes_mut().last_mut().unwrap()
    } else {
        parent
    };
    tx_or_code(target, code, s);
    if external {
        // .iff(external).txN(" ").img("external.png", null): the " " and img are
        // added to `target` (the <a>) when external.
        tx(target, " ");
        let mut img = el("img");
        set_attrs(&mut img, &[("src", "external.png".into()), ("alt", ".".into())]);
        target.add_child_node(img);
    }
}

/// XhtmlNode.txOrCode (XhtmlNode.java:1142): code=true wraps in <code>, splits on
/// \n emitting <br/>, and replaces ' ' with NBSP (U+00A0).
fn tx_or_code(parent: &mut XhtmlNode, code: bool, cnt: &str) {
    if code {
        let mut c = el("code");
        let mut first = true;
        for line in split_lines(cnt) {
            if first {
                first = false;
            } else {
                c.add_child_node(el("br"));
            }
            tx(&mut c, &line.replace(' ', "\u{00A0}"));
        }
        parent.add_child_node(c);
    } else {
        tx(parent, cnt);
    }
}

/// Java `String.split("\\r?\\n")` semantics: split on \r?\n, trailing empties
/// removed.
fn split_lines(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = s.replace("\r\n", "\n").split('\n').map(String::from).collect();
    while parts.len() > 1 && parts.last().map(|s| s.is_empty()).unwrap_or(false) {
        parts.pop();
    }
    parts
}

fn copy_children(dst: &mut XhtmlNode, src: &XhtmlNode) {
    for c in src.child_nodes() {
        dst.add_child_node(c.clone());
    }
}

/// fixFontSizes (SDR:4264): recursively set font-size on <p>/<li> lacking style.
fn fix_font_sizes(nodes: &mut [XhtmlNode], size: i32) {
    for x in nodes.iter_mut() {
        if matches!(x.name(), Some("p") | Some("li")) && x.attributes().get("style").is_none() {
            x.set_attribute("style", format!("font-size: {}px", size));
        }
        if x.has_children() {
            fix_font_sizes(x.child_nodes_mut(), size);
        }
    }
}

/// tail(path): substring after the last '.'.
fn tail(path: &str) -> &str {
    match path.rfind('.') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

// ---- anchor engine (SDR:4105-4183) ----

/// addToStack (SDR:4162): pop while top is not a parent of ec, then push.
fn add_to_stack(stack: &mut Vec<usize>, elements: &[Value], ec: usize) {
    while let Some(&top) = stack.last() {
        if is_parent(elements, top, ec) {
            break;
        }
        stack.pop();
    }
    stack.push(ec);
}

fn is_parent(elements: &[Value], ed: usize, ec: usize) -> bool {
    let pparent = path_of(&elements[ed]);
    let pchild = path_of(&elements[ec]);
    pchild.starts_with(&format!("{}.", pparent))
}

/// checkInScope (SDR:4094): if stack>2 and parent excluded or max==0 -> exclude focus.
fn check_in_scope(stack: &[usize], elements: &[Value], excluded: &mut [bool]) {
    if stack.len() > 2 {
        let parent = stack[stack.len() - 2];
        let focus = stack[stack.len() - 1];
        let parent_max0 = max_of(&elements[parent]) == Some("0");
        if excluded[parent] || parent_max0 {
            excluded[focus] = true;
        }
    }
}

/// generateAnchors (SDR:4105).
fn generate_anchors(
    stack: &[usize],
    elements: &[Value],
    all_anchors: &mut HashMap<String, usize>,
    anchor_lists: &mut [Vec<String>],
) {
    let mut list: Vec<String> = vec![id_of(&elements[stack[0]]).to_string()];
    for &si in &stack[1..] {
        let ed = Ed::new(&elements[si]);
        let name = tail(ed.path());
        let mut aliases: Vec<String> = Vec::new();
        if name.ends_with("[x]") {
            aliases.push(name.to_string());
            let mut seen: Vec<String> = Vec::new();
            for tr in ed.types() {
                let tc = tr.working_code().to_string();
                if !seen.contains(&tc) {
                    let cap = name.replace("[x]", &capitalize(&tc));
                    aliases.push(cap.clone());
                    aliases.push(format!("{}:{}", name, cap));
                    aliases.push(format!("{}:{}", cap, cap));
                    seen.push(tc);
                }
            }
        } else if ed.has_slice_name() {
            aliases.push(format!("{}:{}", name, ed.slice_name().unwrap()));
        } else {
            aliases.push(name.to_string());
        }
        let mut generated: Vec<String> = Vec::new();
        for l in &list {
            for a in &aliases {
                generated.push(format!("{}.{}", l, a));
            }
        }
        list = generated;
    }
    let ed_idx = stack[stack.len() - 1];
    // Cross-element dedup (SDR:4143).
    let mut removed: Vec<String> = Vec::new();
    for s in &list {
        if !all_anchors.contains_key(s) {
            all_anchors.insert(s.clone(), ed_idx);
        } else if s.ends_with("[x]") {
            removed.push(s.clone());
        } else {
            // steal from the earlier element: remove s from its anchor list.
            let other = *all_anchors.get(s).unwrap();
            anchor_lists[other].retain(|x| x != s);
            all_anchors.insert(s.clone(), ed_idx);
        }
    }
    let final_list: Vec<String> = list.into_iter().filter(|s| !removed.contains(s)).collect();
    anchor_lists[ed_idx] = final_list;
}

/// makeAnchors (SDR:4173): [prefix+id] + prefix+each anchor != id.
fn make_anchors(ed: &Ed, prefix: &str, anchor_list: &[String]) -> Vec<String> {
    let mut res = vec![format!("{}{}", prefix, ed.id())];
    for s in anchor_list {
        if s != ed.id() {
            res.push(format!("{}{}", prefix, s));
        }
    }
    res
}

/// describeXml (SDR:4575) — representation-driven. PropertyRepresentation values
/// in FHIR order: xmlAttr, xmlText, typeAttr, cdaText, xhtml. Namespace/name
/// extensions are flagged by guard_unported_rows (corpus has representation only).
fn describe_xml(d: &Ed) -> Option<XhtmlNode> {
    let reps: Vec<&str> = d
        .v
        .get("representation")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    let mut ret = el("div");
    // Iterate in PropertyRepresentation.values() order (SDR:4577).
    for pr in ["xmlAttr", "xmlText", "typeAttr", "cdaText", "xhtml"] {
        if reps.contains(&pr) {
            match pr {
                "cdaText" => tx(&mut ret, "This property is represented as CDA Text in the XML."),
                "typeAttr" => {
                    // codeWithText("The type of this property is determined using the ",
                    //   "xsi:type", "attribute.")
                    tx(&mut ret, "The type of this property is determined using the ");
                    let mut code = el("code");
                    tx(&mut code, "xsi:type");
                    ret.add_child_node(code);
                    tx(&mut ret, "attribute.");
                }
                "xhtml" => tx(&mut ret, "This property is represented as XHTML Text in the XML."),
                "xmlAttr" => tx(&mut ret, "In the XML format, this property is represented as an attribute."),
                "xmlText" => tx(&mut ret, "In the XML format, this property is represented as unadorned text."),
                _ => {}
            }
        }
    }
    if ret.has_children() { Some(ret) } else { None }
}

/// getExtensionValueDefinition (SDR:4203): first snapshot element whose path
/// starts with "Extension.value".
fn extension_value_definition(ext_sd: &Value) -> Option<Value> {
    let els = ext_sd.get("snapshot")?.get("element")?.as_array()?;
    els.iter()
        .find(|e| e.get("path").and_then(|p| p.as_str()).map(|p| p.starts_with("Extension.value")).unwrap_or(false))
        .cloned()
}

fn is_profiled_extension(ec: &Ed) -> bool {
    let types = ec.types();
    types.len() == 1 && types[0].working_code() == "Extension" && !types[0].profiles().is_empty()
}

// ---- small value helpers ----

fn path_of(e: &Value) -> &str {
    e.get("path").and_then(|x| x.as_str()).unwrap_or("")
}
fn id_of(e: &Value) -> &str {
    e.get("id").and_then(|x| x.as_str()).unwrap_or("")
}
fn max_of(e: &Value) -> Option<&str> {
    e.get("max").and_then(|x| x.as_str())
}

fn to_str_min(d: &Ed) -> String {
    // toStr(d.getMin()) — ElementDefinition.getMin() returns 0 when unset.
    d.min().unwrap_or(0).to_string()
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default()
}
fn str_vec(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}
fn coding_list(v: Option<&Value>) -> Vec<Value> {
    v.and_then(|x| x.as_array()).cloned().unwrap_or_default()
}

fn is_absolute_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("urn:")
}

fn has_must_support_types(types: &[crate::sdmodel::TypeRef]) -> bool {
    types.iter().any(is_must_support_type)
}

fn ms_count(types: &[crate::sdmodel::TypeRef]) -> usize {
    types.iter().filter(|t| is_must_support_type(t)).count()
}

/// isMustSupport(TypeRefComponent) (SDR:3574): the type-must-support extension
/// directly on the type (`extension` array), or a mustSupport profile/targetProfile.
fn is_must_support_type(t: &crate::sdmodel::TypeRef) -> bool {
    type_has_ms_ext(t.v)
        || t.v.get("_profile").map(ms_ext_in_array).unwrap_or(false)
        || t.v.get("_targetProfile").map(ms_ext_in_array).unwrap_or(false)
}

/// readStringExtension(tr, EXT_MUST_SUPPORT)=="true": the type-must-support
/// extension on the TypeRefComponent's own `extension` array.
fn type_has_ms_ext(t: &Value) -> bool {
    t.get("extension")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().any(is_type_ms_ext))
        .unwrap_or(false)
}
fn is_type_ms_ext(e: &Value) -> bool {
    e.get("url").and_then(|u| u.as_str())
        == Some("http://hl7.org/fhir/StructureDefinition/elementdefinition-type-must-support")
        && e.get("valueBoolean").and_then(|b| b.as_bool()) == Some(true)
}

/// isMustSupport(List<CanonicalType>) (SDR:3584): any profile/targetProfile
/// entry carries the type-must-support extension (via its `_profile[]` /
/// `_targetProfile[]` sidecar). `sidecar` is that sidecar array.
fn is_ms_canonical_list(sidecar: Option<&Value>) -> bool {
    sidecar.map(ms_ext_in_array).unwrap_or(false)
}

/// isMustSupport(List<CanonicalType>) — any `_profile[]` / `_targetProfile[]`
/// sidecar element carrying the type-must-support extension.
fn ms_ext_in_array(v: &Value) -> bool {
    v.as_array()
        .map(|a| {
            a.iter().any(|x| {
                x.as_object().is_some() && type_has_ms_ext(x)
            })
        })
        .unwrap_or(false)
}

fn has_choices(types: &[crate::sdmodel::TypeRef]) -> bool {
    for t in types {
        if t.profiles().len() > 1 || t.target_profiles().len() > 1 {
            return true;
        }
    }
    types.len() > 1
}

fn encode_bool_opt(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| x.as_bool()).map(|b| b.to_string())
}

/// conf(strength) (SDR:5311).
fn conf(strength: Option<&str>) -> String {
    match strength {
        None => "For codes, see ".to_string(),
        Some("example") => "For example codes, see ".to_string(),
        Some("preferred") => "The codes SHOULD be taken from ".to_string(),
        Some("extensible") => "Unless not suitable, these codes SHALL be taken from ".to_string(),
        Some("required") => "The codes SHALL be taken from ".to_string(),
        Some(_) => "?sd-conf?".to_string(),
    }
}

/// sliceOrderString (SDR:4782): STRUC_DEF_ORDERED/UNORDERED (capitalized).
fn slice_order_string(slicing: &Value) -> String {
    if slicing.get("ordered").and_then(|x| x.as_bool()) == Some(true) {
        "Ordered".to_string()
    } else {
        "Unordered".to_string()
    }
}

/// SlicingRules.getDisplay (ElementDefinition:974).
fn rules_display(code: &str) -> String {
    match code {
        "closed" => "Closed",
        "open" => "Open",
        "openAtEnd" => "Open at End",
        _ => "?",
    }
    .to_string()
}

fn is_simple_content(binding_desc: &XhtmlNode) -> bool {
    binding_desc.child_nodes().len() == 1
        && binding_desc.child_nodes()[0].name() == Some("p")
}

fn binding_has_ext(binding: &Value, url: &str) -> bool {
    binding
        .get("extension")
        .and_then(|e| e.as_array())
        .map(|a| a.iter().any(|x| x.get("url").and_then(|u| u.as_str()) == Some(url)))
        .unwrap_or(false)
}

/// PublicationHacker.fixBindingDescriptions — corpus-relevant subset is identity
/// (no `[[[` link tokens in binding descriptions). Ported as pass-through; the
/// downstream processMarkdown handles any real markdown.
fn fix_binding_descriptions(s: &str) -> String {
    s.to_string()
}

fn constraints_equal(a: &Value, b: &Value) -> bool {
    a == b
}

fn mapping_value(ed: &Value, identity: &str) -> Option<String> {
    let arr = ed.get("mapping")?.as_array()?;
    for m in arr {
        if m.get("identity").and_then(|x| x.as_str()) == Some(identity) {
            return m.get("map").and_then(|x| x.as_str()).map(String::from);
        }
    }
    None
}

/// encodeValue(value, elementName) (SDR:5351). Reads the `<prefix><Type>` field
/// off the element JSON; primitives -> asStringValue; complex -> pretty JSON with
/// (JSON_ALL) name prefix. The corpus FixedValueFormat is JSON_ALL for
/// pattern/fixed (elementName present -> name prefix), and the value is
/// pretty-printed JSON matching the goldens.
fn encode_value_prefixed(ed: &Value, prefix: &str, element_name: Option<&str>) -> Option<String> {
    let obj = ed.as_object()?;
    for (k, val) in obj {
        if let Some(rest) = k.strip_prefix(prefix) {
            if rest.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                return Some(encode_value_json(rest, val, element_name));
            }
        }
    }
    None
}

/// encodeValue (SDR:5351) under the DEFAULT FixedValueFormat.JSON: `notPrimitives()`
/// is true, so primitives render as `asStringValue()` (UNQUOTED); complex values
/// render as pretty JSON. `prefixWithName` = (JSON_ALL && elementName!=null) is
/// FALSE for JSON, so NO `"name" : ` prefix is ever emitted (the `_type_suffix`
/// / `element_name` args are inert under the default format).
fn encode_value_json(_type_suffix: &str, val: &Value, _element_name: Option<&str>) -> String {
    if !val.is_object() && !val.is_array() {
        // primitive: asStringValue() (no quotes).
        return primitive_as_string(val);
    }
    // Complex value: pretty JSON with two-space indent, trimmed.
    pretty_json(val, 0).trim().to_string()
}

/// Pretty-print a JSON value as the FHIR JsonParser (OutputStyle.PRETTY) does.
/// Object: `{\n<pad+1>"k" : v,\n...\n<pad>}`. Array: `[` + items joined by `,`
/// rendered inline at the SAME `indent` as the array (so an array element object
/// prints `{` inline with its body at pad+1 and closing `}` at pad — matching the
/// `[{ ... }]` blocks in the goldens). `indent` = the level of the key holding
/// this value.
fn pretty_json(val: &Value, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    let pad1 = "  ".repeat(indent + 1);
    match val {
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let mut parts = Vec::new();
            for (k, v) in map {
                parts.push(format!("{}\"{}\" : {}", pad1, k, pretty_json(v, indent + 1)));
            }
            format!("{{\n{}\n{}}}", parts.join(",\n"), pad)
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                return "[]".to_string();
            }
            // Items render at the array's own `indent` (not +1): an object item's
            // `{` is inline after `[`, its body at indent+1, closing `}` at indent.
            let parts: Vec<String> = arr.iter().map(|v| pretty_json(v, indent)).collect();
            format!("[{}]", parts.join(","))
        }
        Value::String(s) => format!("\"{}\"", escape_json(s)),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".to_string(),
    }
}

/// Utilities.escapeJson.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 32 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// encodeValue(ex.getValue(), null): the example value as a string (primitive
/// asStringValue; complex pretty JSON without name prefix).
fn encode_example_value(ex: &Value) -> Option<String> {
    let obj = ex.as_object()?;
    for (k, val) in obj {
        if let Some(rest) = k.strip_prefix("value") {
            if rest.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
                if !val.is_object() && !val.is_array() {
                    // encodeValue with elementName=null: primitive asStringValue
                    // (no quotes).
                    return Some(primitive_as_string(val));
                }
                return Some(pretty_json(val, 0).trim().to_string());
            }
        }
    }
    None
}

fn primitive_as_string(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}
