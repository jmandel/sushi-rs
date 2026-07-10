//! `render_sd::pseudojson` — the publisher's `pseudoJson()` SD leaf fragment
//! (a JSON-shape walk of the SD snapshot), ported byte-exact.
//!
//! Citations: `psdr` = the publisher's
//! `org.hl7.fhir.igtools.renderers.StructureDefinitionRenderer` (pseudoJson:1722
//! and generateCoreElem/Sliced/Extension + helpers). Phrases = fhir-core-6911
//! rendering-phrases.properties (English). `allInvariants` defaults true
//! (PublisherFields:195); no corpus IG sets show-inherited-invariants.
//!
//! The body is a raw HTML StringBuilder (\r\n line endings), NOT an XhtmlNode
//! tree — emitted directly like `leaf::summary`. Wrapped in `{% raw %}` by caller.

use serde_json::Value;

use crate::context::IgContext;
use crate::leaf::escape_xml;
use crate::sdmodel::{Ed, Sd, TypeRef};

/// `pseudoJson()` (psdr:1722). corePath = `http://hl7.org/fhir/R4/` etc.
pub fn pseudo_json(sd: &Sd, ctx: &IgContext, core_path: &str) -> String {
    let elements = sd.snapshot_elements();
    if elements.is_empty() {
        return String::new();
    }
    let mut b = String::new();
    let rn = elements[0].path().to_string();
    let title = sd.root.get("title").and_then(|x| x.as_str()).unwrap_or("");
    b.push_str(&format!(
        " // <span style=\"color: navy; opacity: 0.8\">{}</span>\r\n {{\r\n",
        escape_xml(title)
    ));
    if sd.kind() == "resource" {
        b.push_str(&format!(
            "   \"resourceType\" : \"{}\",\r\n",
            sd.type_name()
        ));
    }

    let root = elements[0];
    let children = get_children(&elements, 0);
    let complex = is_complex(&elements, &children);
    if !complex && !has_extension_child(&elements, &children) {
        // SDR_FROM_ELEM = // from Element: <a href="{0}">extension</a>
        b.push_str(&format!(
            "// from Element: <a href=\"{}extensibility.html\">extension</a>\r\n",
            core_path
        ));
    }

    let ctxr = Ctx { sd, ctx, core_path };
    let mut c = 0i32;
    let l = last_child(&elements, &children);
    let mut ext_done = false;
    for &ci in &children {
        let child = elements[ci];
        if is_extension(&child) {
            if !ext_done {
                c += 1;
                ctxr.generate_core_elem_extension(
                    &mut b,
                    &elements,
                    ci,
                    &children,
                    2,
                    &rn,
                    false,
                    child.types().first().copied(),
                    c == l,
                    complex,
                );
            }
            ext_done = true;
        } else if child.has_slicing() {
            c += 1;
            ctxr.generate_core_elem_sliced(
                &mut b,
                &elements,
                ci,
                &children,
                2,
                &rn,
                false,
                child.types().first().copied(),
                c == l,
                complex,
            );
        } else if was_sliced(&elements, &children, ci) {
            // nothing
        } else if child.types().len() <= 1 || all_types_are_reference(&child) {
            c += 1;
            ctxr.generate_core_elem(
                &mut b,
                &elements,
                ci,
                2,
                &rn,
                false,
                child.types().first().copied(),
                c == l,
                complex,
            );
        } else {
            if child.max() != Some("0") {
                b.push_str(&format!(
                    "<span style=\"color: Gray\">// {}: <span style=\"color: navy; opacity: 0.8\">{}</span>. One of these {}:</span>\r\n",
                    tail(child.path()),
                    escape_xml(child.short().unwrap_or("")),
                    child.types().len()
                ));
                for t in child.types() {
                    c += 1;
                    ctxr.generate_core_elem(
                        &mut b,
                        &elements,
                        ci,
                        2,
                        &rn,
                        false,
                        Some(t),
                        c == l,
                        false,
                    );
                }
            }
        }
    }
    let _ = root;
    b.push_str("  }\r\n");
    b
}

struct Ctx<'a> {
    sd: &'a Sd,
    ctx: &'a IgContext,
    core_path: &'a str,
}

impl<'a> Ctx<'a> {
    /// `getLinkForProfile(this.sd, this.sd.getUrl())` for the OWN sd = local page.
    /// Java returns webPath+"|"+name; callers strip at `|`. We return just the
    /// webPath (own IG resource -> `StructureDefinition-<id>.html`).
    fn def_page(&self) -> String {
        self.ctx
            .resolve(&self.sd.url())
            .map(|r| r.web_path)
            .unwrap_or_else(|| "unknown.html".to_string())
    }

    /// `generateCoreElem` (psdr:1816).
    #[allow(clippy::too_many_arguments)]
    fn generate_core_elem(
        &self,
        b: &mut String,
        elements: &[Ed],
        idx: usize,
        indent: usize,
        path_name: &str,
        as_value: bool,
        type_: Option<TypeRef>,
        last: bool,
        complex: bool,
    ) {
        let elem = elements[idx];
        let path = elem.path();
        // skip nested .id
        if path.ends_with(".id") && path.rfind('.').unwrap_or(0) > path.find('.').unwrap_or(0) {
            return;
        }
        if !complex && path.ends_with(".extension") {
            return;
        }
        if elem.max() == Some("0") {
            return;
        }
        let indent_s = "  ".repeat(indent);
        b.push_str(&indent_s);

        let children = get_children(elements, idx);
        let name = tail(path);
        let mut en = if as_value {
            "value[x]".to_string()
        } else {
            name.to_string()
        };
        if en.contains("[x]") {
            let wc = type_.map(|t| t.working_code()).unwrap_or("");
            en = en.replace("[x]", &up_first(wc));
        }
        let unbounded = match elem.base_max() {
            Some(m) => m == "*",
            None => elem.max() == Some("*"),
        };
        let def_page = self.def_page();

        // 1. name
        b.push_str(&format!(
            "\"<a href=\"{}#{}.{}\" title=\"{}\" class=\"dict\"><span style=\"text-decoration: underline\">{}</span></a>\" : ",
            def_page, path_name, en,
            escape_xml(&get_enhanced_definition(&elem)),
            en
        ));

        // 2. value
        let mut delayed_close = false;
        if unbounded {
            b.push('[');
        }
        // Java: `if (type == null || children.size() > 0)` -> inline `{`.
        // BUT a contentReference leaf (empty type array, no children) is rendered
        // by the publisher as `<n/a>` (golden-universal across the observation
        // profiles' component.referenceRange). Its in-memory type is a single
        // null-code TypeRefComponent, which the finished JSON drops (no `type`
        // key), so we detect empty-type-leaf here and emit the null-code branch.
        // Real backbone elements (children>0) still take the inline-object path.
        let na_leaf = type_.is_none() && children.is_empty();
        if !na_leaf && (type_.is_none() || !children.is_empty()) {
            b.push('{');
            delayed_close = true;
        } else if na_leaf {
            b.push_str("&lt;<span style=\"color: darkgreen\">n/a</span>&gt;");
        } else {
            let type_ = type_.unwrap();
            let wc = type_.working_code();
            if wc.is_empty() {
                b.push_str("&lt;<span style=\"color: darkgreen\">n/a</span>&gt;");
            } else if self.is_primitive(wc) {
                let is_bare = wc == "integer" || wc == "boolean" || wc == "decimal";
                if !is_bare {
                    b.push('"');
                }
                if let Some((_ty, fv)) = elem.fixed() {
                    b.push_str(&escape_json(primitive_str(fv).as_deref().unwrap_or("")));
                } else {
                    match self.get_src_file(wc) {
                        None => b.push_str(&format!(
                            "&lt;<span style=\"color: darkgreen\">{}</span>&gt;",
                            wc
                        )),
                        Some(l) => b.push_str(&format!(
                            "&lt;<span style=\"color: darkgreen\"><a href=\"{}\">{}</a></span>&gt;",
                            suffix(&l, wc),
                            wc
                        )),
                    }
                }
                if !is_bare {
                    b.push('"');
                }
            } else {
                b.push('{');
                let src = self.get_src_file(wc).unwrap_or_default();
                b.push_str(&format!(
                    "<span style=\"color: darkgreen\"><a href=\"{}\">{}</a></span>",
                    escape_xml(&suffix(&src, wc)),
                    wc
                ));
                if let Some(prof) = type_.profiles().first() {
                    match self.ctx.resolve(prof) {
                        Some(tsd) => b.push_str(&format!(
                            " (as <span style=\"color: darkgreen\"><a href=\"{}#{}\">{}</a></span>)",
                            escape_xml(&tsd.web_path),
                            self.resolved_type(&tsd),
                            tsd.name.clone().unwrap_or_default()
                        )),
                        None => b.push_str(&format!(
                            " (as <span style=\"color: darkgreen\">{}</span>)",
                            canonical_list_to_string(&type_.profiles())
                        )),
                    }
                }
                if let Some(tp) = type_.target_profiles().first() {
                    if let Some(t) = tp.strip_prefix("http://hl7.org/fhir/StructureDefinition/") {
                        if self.has_type(t) {
                            b.push_str(&format!(
                                "(<span style=\"color: darkgreen\"><a href=\"{}\">{}</a></span>)",
                                escape_xml(&suffix(&self.get_src_file(t).unwrap_or_default(), t)),
                                t
                            ));
                        } else if self.has_resource(t) {
                            b.push_str(&format!(
                                "(<span style=\"color: darkgreen\"><a href=\"{}{}.html\">{}</a></span>)",
                                escape_xml(self.core_path),
                                t.to_lowercase(),
                                t
                            ));
                        } else {
                            b.push_str(&format!("({})", t));
                        }
                    } else {
                        // Java `b.append("(" + type.getTargetProfile() + ")")` on a
                        // List<CanonicalType> emits List.toString() =
                        // `[CanonicalType[url], ...]`.
                        b.push_str(&format!(
                            "({})",
                            canonical_list_to_string(&type_.target_profiles())
                        ));
                    }
                }
                b.push('}');
            }
        }

        if !delayed_close {
            if unbounded {
                b.push(']');
            }
            if !last {
                b.push(',');
            }
        }

        b.push_str(" <span style=\"color: Gray\">//</span>");

        // 3. optionality
        self.write_cardinality(unbounded, b, &elem);

        // 4. doco
        if elem.fixed().is_none() {
            if let Some(vs_ref) = binding_valueset(&elem) {
                // QUIRK: the resolved vs's render_filename userdata is ALWAYS null
                // in this path (tx-resolved VS carries no rendering filename at
                // pseudoJson time), so the Java `corePath + null + ".html"`
                // literally emits `{corePath}null.html`. Golden-verified: all 834
                // binding links in the corpus are `.../R4/null.html`. Only when
                // findTxResource returns null (VS unresolvable) does it fall back
                // to `{vsUrl}.html`.
                if self.ctx.resolve(&vs_ref).is_some() {
                    b.push_str(&format!(
                        " <span style=\"color: navy; opacity: 0.8\"><a href=\"{}null.html\" style=\"color: navy\">{}</a></span>",
                        self.core_path,
                        escape_xml(elem.short().unwrap_or(""))
                    ));
                } else {
                    b.push_str(&format!(
                        " <span style=\"color: navy; opacity: 0.8\"><a href=\"{}.html\" style=\"color: navy\">{}</a></span>",
                        vs_ref,
                        escape_xml(elem.short().unwrap_or(""))
                    ));
                }
            } else {
                b.push_str(&format!(
                    " <span style=\"color: navy; opacity: 0.8\">{}</span>",
                    escape_xml(elem.short().unwrap_or(""))
                ));
            }
        }
        b.push_str("\r\n");

        if delayed_close {
            let mut c = 0i32;
            let l = last_child(elements, &children);
            let mut ext_done = false;
            for &ci in &children {
                let child = elements[ci];
                if is_extension(&child) {
                    if !ext_done {
                        c += 1;
                        self.generate_core_elem_extension(
                            b,
                            elements,
                            ci,
                            &children,
                            indent + 1,
                            &format!("{}.{}", path_name, name),
                            false,
                            child.types().first().copied(),
                            c == l,
                            complex,
                        );
                    }
                    ext_done = true;
                } else if child.has_slicing() {
                    c += 1;
                    self.generate_core_elem_sliced(
                        b,
                        elements,
                        ci,
                        &children,
                        indent + 1,
                        &format!("{}.{}", path_name, name),
                        false,
                        child.types().first().copied(),
                        c == l,
                        complex,
                    );
                } else if was_sliced(elements, &children, ci) {
                    // nothing
                } else if child.types().len() <= 1 || all_types_are_reference(&child) {
                    c += 1;
                    self.generate_core_elem(
                        b,
                        elements,
                        ci,
                        indent + 1,
                        &format!("{}.{}", path_name, name),
                        false,
                        child.types().first().copied(),
                        c == l,
                        false,
                    );
                } else if child.max() != Some("0") {
                    b.push_str(&format!(
                        "<span style=\"color: Gray\">// value[x]: <span style=\"color: navy; opacity: 0.8\">{}</span>. One of these {}:</span>\r\n",
                        escape_xml(child.short().unwrap_or("")),
                        child.types().len()
                    ));
                    for t in child.types() {
                        c += 1;
                        self.generate_core_elem(
                            b,
                            elements,
                            ci,
                            indent + 1,
                            &format!("{}.{}", path_name, name),
                            false,
                            Some(t),
                            c == l,
                            false,
                        );
                    }
                }
            }
            b.push_str(&indent_s);
            b.push('}');
            if unbounded {
                b.push(']');
            }
            if !last {
                b.push(',');
            }
            b.push_str("\r\n");
        }
    }

    /// `generateCoreElemSliced` (psdr:1976).
    #[allow(clippy::too_many_arguments)]
    fn generate_core_elem_sliced(
        &self,
        b: &mut String,
        elements: &[Ed],
        idx: usize,
        children: &[usize],
        indent: usize,
        path_name: &str,
        as_value: bool,
        type_: Option<TypeRef>,
        last: bool,
        complex: bool,
    ) {
        let elem = elements[idx];
        if elem.max() == Some("0") {
            return;
        }
        let name = tail(elem.path());
        let mut en = if as_value {
            "value[x]".to_string()
        } else {
            name.to_string()
        };
        if en.contains("[x]") {
            let t = type_.expect("Type cannot be unknown for element with [x] in the name");
            en = en.replace("[x]", &up_first(t.working_code()));
        }
        let unbounded = elem.max() == Some("*");
        let indent_s = "  ".repeat(indent);
        let def_page = self.def_page();
        b.push_str(&indent_s);
        b.push_str(&format!(
            "\"<a href=\"{}#{}.{}\" title=\"{}\" class=\"dict\"><span style=\"text-decoration: underline\">{}</span></a>\" : ",
            def_page, path_name, en,
            escape_xml(&get_enhanced_definition(&elem)),
            en
        ));
        b.push_str(&format!(
            "[ <span style=\"color: navy\">{}</span>",
            describe_slicing(elem.slicing())
        ));
        b.push_str("\r\n");

        let slices = get_slices(elements, children, idx);
        let mut c = 0usize;
        for &si in &slices {
            let slice = elements[si];
            b.push_str(&format!("{}  ", indent_s));
            b.push_str(&format!(
                "{{ // <span style=\"color: navy; opacity: 0.8\">{}</span>",
                escape_xml(slice.short().unwrap_or(""))
            ));
            self.write_cardinality(unbounded, b, &slice);
            b.push_str("\r\n");

            let extchildren = get_children(elements, si);
            let extcomplex = is_complex(elements, &extchildren) && complex;
            if !extcomplex && !has_extension_child(elements, &extchildren) {
                b.push_str(&format!("{}  ", indent_s));
                b.push_str(&format!(
                    "// from Element: <a href=\"{}extensibility.html\">extension</a>\r\n",
                    self.core_path
                ));
            }
            let mut cc = 0i32;
            let el = last_child(elements, &extchildren);
            for &cci in &extchildren {
                let cchild = elements[cci];
                if cchild.has_slicing() {
                    cc += 1;
                    self.generate_core_elem_sliced(
                        b,
                        elements,
                        cci,
                        children,
                        indent + 2,
                        &format!("{}.{}", path_name, en),
                        false,
                        cchild.types().first().copied(),
                        cc == el,
                        extcomplex,
                    );
                } else if was_sliced(elements, children, cci) {
                    // nothing
                } else if cchild.types().len() <= 1 {
                    // <=1: empty-type contentReference leaf -> na_leaf (see
                    // generate_core_elem); size==1 -> normal.
                    cc += 1;
                    self.generate_core_elem(
                        b,
                        elements,
                        cci,
                        indent + 2,
                        &format!("{}.{}", path_name, en),
                        false,
                        cchild.types().first().copied(),
                        cc == el,
                        extcomplex,
                    );
                } else {
                    b.push_str(&format!(
                        "<span style=\"color: Gray\">// value[x]: <span style=\"color: navy; opacity: 0.8\">{}</span>. One of these {}:</span>\r\n",
                        escape_xml(cchild.short().unwrap_or("")),
                        cchild.types().len()
                    ));
                    for t in cchild.types() {
                        cc += 1;
                        self.generate_core_elem(
                            b,
                            elements,
                            cci,
                            indent + 2,
                            &format!("{}.{}", path_name, en),
                            false,
                            Some(t),
                            cc == el,
                            false,
                        );
                    }
                }
            }
            c += 1;
            b.push_str(&indent_s);
            if c == slices.len() {
                b.push_str("  }\r\n");
            } else {
                b.push_str("  },\r\n");
            }
        }
        b.push_str(&indent_s);
        if last {
            b.push_str("]\r\n");
        } else {
            b.push_str("],\r\n");
        }
    }

    /// `generateCoreElemExtension` (psdr:2051).
    #[allow(clippy::too_many_arguments)]
    fn generate_core_elem_extension(
        &self,
        b: &mut String,
        elements: &[Ed],
        idx: usize,
        children: &[usize],
        indent: usize,
        path_name: &str,
        as_value: bool,
        type_: Option<TypeRef>,
        last: bool,
        _complex: bool,
    ) {
        let elem = elements[idx];
        if elem.max() == Some("0") {
            return;
        }
        let name = tail(elem.path());
        let mut en = if as_value {
            "value[x]".to_string()
        } else {
            name.to_string()
        };
        if en.contains("[x]") {
            let wc = type_.map(|t| t.working_code()).unwrap_or("");
            en = en.replace("[x]", &up_first(wc));
        }
        let _ = en;
        let unbounded = elem.max() == Some("*");
        let indent_s = "  ".repeat(indent);
        b.push_str(&format!("{}\"extension\": [\r\n", indent_s));

        let slices = get_slices(elements, children, idx);
        let mut c = 0usize;
        for &si in &slices {
            let slice = elements[si];
            let url = slice
                .types()
                .first()
                .and_then(|t| t.profiles().first().map(|s| s.to_string()));
            let sd_ext = url.as_deref().and_then(|u| self.ctx.resolve(u));
            b.push_str(&format!("{}  ", indent_s));
            b.push_str("{ // <span style=\"color: navy; opacity: 0.8\">");
            self.write_cardinality(unbounded, b, &slice);
            b.push_str(&format!(
                "{}</span>",
                escape_xml(slice.short().unwrap_or(""))
            ));
            b.push_str("\r\n");
            b.push_str(&format!("{}    ", indent_s));
            match &sd_ext {
                None => b.push_str(&format!(
                    "\"url\": \"{}\",\r\n",
                    url.as_deref().unwrap_or("null")
                )),
                Some(r) => b.push_str(&format!(
                    "\"url\": \"<a href=\"{}\">{}</a>\",\r\n",
                    r.web_path,
                    url.as_deref().unwrap_or("")
                )),
            }

            // extchildren: the slice's own children, or (if empty) the referenced
            // extension SD's snapshot children.
            let own = get_children(elements, si);
            if !own.is_empty() {
                let value = get_value(elements, &own);
                self.emit_ext_value(
                    b,
                    elements,
                    value,
                    indent,
                    path_name,
                    "extension",
                    url.as_deref(),
                );
            } else {
                match &sd_ext {
                    None => {
                        // SDR_UNK_EXT = Not handled yet: unknown extension '{0}'
                        b.push_str(&format!(
                            "Not handled yet: unknown extension '{}'\r\n",
                            url.as_deref().unwrap_or("")
                        ));
                    }
                    Some(r) => {
                        // Load the referenced extension SD's snapshot and walk it.
                        if let Some(res) = self.load_ext_sd(r) {
                            let ext_sd = res;
                            let ext_elems = ext_sd.snapshot_elements();
                            if !ext_elems.is_empty() {
                                let ext_children = get_children(&ext_elems, 0);
                                let value = get_value(&ext_elems, &ext_children);
                                // NOTE: the value element belongs to ext_sd's
                                // snapshot; render via a temporary Ctx bound to
                                // the extension SD would change def_page. But the
                                // publisher passes `sd`-owned `elements`/`this.sd`
                                // to generateCoreElem, so links still point at the
                                // PROFILE. We keep `self` (this.sd) and emit from
                                // ext_elems.
                                self.emit_ext_value(
                                    b,
                                    &ext_elems,
                                    value,
                                    indent,
                                    path_name,
                                    "extension",
                                    url.as_deref(),
                                );
                            }
                        }
                    }
                }
            }

            c += 1;
            b.push_str(&indent_s);
            if c == slices.len() {
                b.push_str("  }\r\n");
            } else {
                b.push_str("  },\r\n");
            }
        }
        b.push_str(&indent_s);
        if last {
            b.push_str("]\r\n");
        } else {
            b.push_str("],\r\n");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_ext_value(
        &self,
        b: &mut String,
        elements: &[Ed],
        value: Option<usize>,
        indent: usize,
        path_name: &str,
        en: &str,
        url: Option<&str>,
    ) {
        let Some(vi) = value else {
            // psdr:2111: SDR_NOT_HANDLED_EXT = "Not handled yet: complex extension
            // '{0}'" with {0} = "Not handled yet: complex extension " + url (a
            // hardcoded Java string prefix — reproduces the double-nesting the
            // goldens show for complex extensions like us-core-race).
            b.push_str(&format!(
                "Not handled yet: complex extension 'Not handled yet: complex extension {}'\r\n",
                url.unwrap_or("null")
            ));
            return;
        };
        let value = elements[vi];
        let types = value.types();
        if types.len() == 1 {
            self.generate_core_elem(
                b,
                elements,
                vi,
                indent + 2,
                &format!("{}.{}", path_name, en),
                false,
                types.first().copied(),
                true,
                false,
            );
        } else {
            b.push_str(&format!(
                "<span style=\"color: Gray\">// value[x]: <span style=\"color: navy; opacity: 0.8\">{}</span>. One of these {}:</span>\r\n",
                escape_xml(value.short().unwrap_or("")),
                types.len()
            ));
            let n = types.len();
            for (i, t) in types.iter().enumerate() {
                self.generate_core_elem(
                    b,
                    elements,
                    vi,
                    indent + 2,
                    &format!("{}.{}", path_name, en),
                    false,
                    Some(*t),
                    i + 1 == n,
                    false,
                );
            }
        }
    }

    /// `tsd.getType()` — the SD's base `type`, loaded from the resource file
    /// (used for the `(as ...)#{type}` anchor). Rare branch.
    fn resolved_type(&self, r: &crate::context::Resolved) -> String {
        self.load_ext_sd(r)
            .map(|s| s.type_name().to_string())
            .unwrap_or_else(|| r.rtype.clone())
    }

    fn load_ext_sd(&self, r: &crate::context::Resolved) -> Option<Sd> {
        let f = r.file.clone()?;
        let text = self.ctx.tree().read(&f)?;
        Sd::from_json(&text).ok()
    }

    /// `writeCardinality` (psdr:2170).
    fn write_cardinality(&self, unbounded: bool, b: &mut String, elem: &Ed) {
        if !elem.constraints().is_empty() {
            b.push_str(&format!(
                " <span style=\"color: brown\" title=\"{}\"><b>C?</b></span>",
                escape_xml(&self.get_invariants(elem))
            ));
        }
        if elem.min().unwrap_or(0) > 0 {
            b.push_str(
                " <span style=\"color: brown\" title=\"This element is required\"><b>R!</b></span>",
            );
        }
        if unbounded && elem.max() == Some("1") {
            b.push_str(" <span style=\"color: brown\" title=\"This element is an array in the base standard, but the profile only allows one element\"><b>Only One!</b></span> ");
        }
    }

    /// `getInvariants` (psdr:2178): includes a constraint iff
    /// `!hasSource || source==sd.url || allInvariants`. Unlike `inv`
    /// (invOldMode:1241, which has an extra `genMode != DIFF` escape that always
    /// fires for snapshot mode), getInvariants has NO such escape — so the
    /// effective gate here is source-only. Golden-verified: inherited `ele-1`
    /// (source=.../Element) is EXCLUDED (empty C? title), own `us-core-*`
    /// constraints included. => allInvariants is FALSE for this path.
    fn get_invariants(&self, elem: &Ed) -> String {
        let url = self.sd.url();
        let mut parts: Vec<String> = Vec::new();
        for c in elem.constraint_values() {
            let source = c.get("source").and_then(|x| x.as_str());
            let include = source.is_none() || source == Some(url.as_str());
            if !include {
                continue;
            }
            let key = c.get("key").and_then(|x| x.as_str()).unwrap_or("");
            let human = c.get("human").and_then(|x| x.as_str()).unwrap_or("");
            parts.push(format!("{}: {}", key, human));
        }
        parts.join("; ")
    }

    /// `isPrimitive(code)`: fetchTypeDefinition(code).kind == primitive-type.
    fn is_primitive(&self, code: &str) -> bool {
        self.ctx.is_primitive_type(code)
    }

    /// `hasType(code)`: kind primitive-type or complex-type. A version-suffixed
    /// `code` (e.g. `CarePlan|4.0.1`) is fetched as the bare `sdNs(code)` URL,
    /// which the publisher's fetch does not match -> false (golden-verified:
    /// cycle CarePlan|4.0.1 renders raw, no link).
    fn has_type(&self, code: &str) -> bool {
        if code.contains('|') {
            return false;
        }
        self.ctx
            .resolve_type(code)
            .map(|r| {
                matches!(
                    r.kind.as_deref(),
                    Some("primitive-type") | Some("complex-type")
                )
            })
            .unwrap_or(false)
    }

    /// `hasResource(code)`: kind resource. Same version-suffix guard.
    fn has_resource(&self, code: &str) -> bool {
        if code.contains('|') {
            return false;
        }
        self.ctx
            .resolve_type(code)
            .map(|r| r.kind.as_deref() == Some("resource"))
            .unwrap_or(false)
    }

    /// `getSrcFile(code)` (psdr:2150): resolve the type SD, then
    /// getLinkForProfile(this.sd, typeSd.url) = the type's webPath (strip `|name`).
    fn get_src_file(&self, code: &str) -> Option<String> {
        let r = self.ctx.resolve_type(code)?;
        if r.web_path.is_empty() {
            return None;
        }
        Some(r.web_path.clone())
    }
}

// --- free helpers (index-based over the snapshot list) ---

/// `getChildren(elements, elem)` (psdr:657): direct children (path == elem.path +
/// "." + one segment), contiguous after `idx`.
fn get_children(elements: &[Ed], idx: usize) -> Vec<usize> {
    let base = elements[idx].path();
    let prefix = format!("{}.", base);
    let mut res = Vec::new();
    let mut i = idx + 1;
    while i < elements.len() {
        let p = elements[i].path();
        if p.starts_with(&prefix) {
            if !p[prefix.len()..].contains('.') {
                res.push(i);
            }
        } else {
            return res;
        }
        i += 1;
    }
    res
}

/// `isComplex(children)` (psdr:1799): more than one `Extension.extension` child.
fn is_complex(elements: &[Ed], children: &[usize]) -> bool {
    children
        .iter()
        .filter(|&&i| elements[i].path() == "Extension.extension")
        .count()
        > 1
}

/// `lastChild(children)` (psdr:1808): size minus trailing max==0 children (1-based).
fn last_child(elements: &[Ed], children: &[usize]) -> i32 {
    let mut l = children.len();
    while l > 0 && elements[children[l - 1]].max() == Some("0") {
        l -= 1;
    }
    l as i32
}

/// `isExtension(child)` (psdr:1782).
fn is_extension(child: &Ed) -> bool {
    child.path().ends_with(".extension") || child.path().ends_with(".modifierExtension")
}

/// `hasExtensionChild(children)` (psdr:1789).
fn has_extension_child(elements: &[Ed], children: &[usize]) -> bool {
    children
        .iter()
        .any(|&i| elements[i].path().ends_with(".extension"))
}

/// `allTypesAreReference(child)` (psdr:1775).
fn all_types_are_reference(child: &Ed) -> bool {
    let types = child.types();
    !types.is_empty() && types.iter().all(|t| t.working_code() == "Reference")
}

/// `wasSliced(child, children)` (psdr:2208): an earlier child with the same path
/// hasSlicing.
fn was_sliced(elements: &[Ed], children: &[usize], idx: usize) -> bool {
    let path = elements[idx].path();
    for &ci in children {
        if ci == idx {
            break;
        }
        if elements[ci].path() == path && elements[ci].has_slicing() {
            return true;
        }
    }
    false
}

/// `getSlices(elem, children)` (psdr:2218): children (other than elem) with the
/// same path as elem.
fn get_slices(elements: &[Ed], children: &[usize], idx: usize) -> Vec<usize> {
    let path = elements[idx].path();
    children
        .iter()
        .copied()
        .filter(|&ci| ci != idx && elements[ci].path() == path)
        .collect()
}

/// `getValue(extchildren)` (psdr:2130): first child whose path contains `.value`
/// and max != 0.
fn get_value(elements: &[Ed], children: &[usize]) -> Option<usize> {
    children
        .iter()
        .copied()
        .find(|&ci| elements[ci].path().contains(".value") && elements[ci].max() != Some("0"))
}

/// `getEnhancedDefinition(elem)` (psdr:2196): removePeriod(definition) + a
/// modifier/must-support suffix phrase.
fn get_enhanced_definition(elem: &Ed) -> String {
    let def = remove_period(elem.definition().unwrap_or(""));
    if elem.is_modifier() && elem.must_support() {
        format!(
            "{} (this element modifies the meaning of other elements, and must be supported)",
            def
        )
    } else if elem.is_modifier() {
        format!(
            "{} (this element modifies the meaning of other elements)",
            def
        )
    } else if elem.must_support() {
        format!("{} (this element must be supported)", def)
    } else {
        def
    }
}

/// `describeSlicing(slicing)` (psdr:2226).
fn describe_slicing(slicing: Option<&Value>) -> String {
    let Some(sl) = slicing else {
        return String::new();
    };
    let rules = sl.get("rules").and_then(|x| x.as_str()).unwrap_or("");
    if rules == "closed" {
        return String::new();
    }
    let mut discs: Vec<String> = Vec::new();
    if let Some(arr) = sl.get("discriminator").and_then(|x| x.as_array()) {
        for d in arr {
            let ty = d.get("type").and_then(|x| x.as_str()).unwrap_or("");
            let p = d.get("path").and_then(|x| x.as_str()).unwrap_or("");
            discs.push(format!("{}:{}", ty, p));
        }
    }
    let csv = discs.join(", ");
    let ordered = sl.get("ordered").and_then(|x| x.as_bool()).unwrap_or(false);
    // Java: `" " + formatPhrase(ordered ? SDR_ANY_ORDER : SDR_SORTED, csv, rulesDisplay)`
    // (the ternary is counter-intuitive but literal). Note the leading space.
    let has_rules = sl.get("rules").is_some();
    let rd = if has_rules { rules_display(rules) } else { "" };
    if ordered {
        // SDR_ANY_ORDER = // sliced by {0} in any order  ({1} arg dropped — no placeholder)
        format!(" // sliced by {} in any order", csv)
    } else {
        // SDR_SORTED = // sliced by {0} in the specified order {1}
        format!(" // sliced by {} in the specified order {}", csv, rd)
    }
}

fn rules_display(code: &str) -> &'static str {
    match code {
        "closed" => "Closed",
        "open" => "Open",
        "openAtEnd" => "Open At End",
        _ => "",
    }
}

/// `suffix(link, code)` (psdr:1966): if link has `|` strip it; if it already has
/// `#` return as-is, else append `#code`.
fn suffix(link: &str, code: &str) -> String {
    let link = match link.split_once('|') {
        Some((l, _)) => l,
        None => link,
    };
    if link.contains('#') {
        link.to_string()
    } else {
        format!("{}#{}", link, code)
    }
}

fn tail(path: &str) -> &str {
    match path.rfind('.') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

fn up_first(s: &str) -> String {
    let mut ch = s.chars();
    match ch.next() {
        Some(c) => c.to_uppercase().collect::<String>() + ch.as_str(),
        None => String::new(),
    }
}

/// `Utilities.removePeriod`: drop a single trailing `.`.
fn remove_period(s: &str) -> String {
    match s.strip_suffix('.') {
        Some(r) => r.to_string(),
        None => s.to_string(),
    }
}

/// `Utilities.escapeJson`: \r \n \t " \ escaped, space kept, <32 -> \u.
fn escape_json(v: &str) -> String {
    let mut b = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\r' => b.push_str("\\r"),
            '\n' => b.push_str("\\n"),
            '\t' => b.push_str("\\t"),
            '"' => b.push_str("\\\""),
            '\\' => b.push_str("\\\\"),
            ' ' => b.push(' '),
            c if (c as u32) < 32 => b.push_str(&format!("\\u{:04x}", c as u32)),
            c => b.push(c),
        }
    }
    b
}

/// The primitive string value of a fixed[x] JSON value.
fn primitive_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Java `List<CanonicalType>.toString()` = `[CanonicalType[v1], CanonicalType[v2]]`.
/// The publisher appends this raw for the non-core targetProfile / null-tsd
/// profile branches.
fn canonical_list_to_string(items: &[&str]) -> String {
    let inner: Vec<String> = items
        .iter()
        .map(|u| format!("CanonicalType[{}]", u))
        .collect();
    format!("[{}]", inner.join(", "))
}

/// `elem.getBinding().getValueSet()` when present.
fn binding_valueset(elem: &Ed) -> Option<String> {
    elem.binding()
        .and_then(|b| b.get("valueSet"))
        .and_then(|x| x.as_str())
        .map(String::from)
}
