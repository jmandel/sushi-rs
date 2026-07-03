//! Top-level renderer: walk the block tree and emit kramdown-style HTML,
//! matching the indentation/whitespace quirks of kramdown's HTML converter.

use crate::block::{parse_doc, Align, Block, BlockNode, ListItem};
use crate::ial::Attrs;
use crate::inline::{raw_text, render_inline};
use crate::util::{escape_html_attr, IdGen};

/// Rendering options. Defaults mirror the FHIR IG Publisher's Jekyll config.
#[derive(Debug, Clone)]
pub struct Options {
    /// Generate heading ids via the kramdown GFM algorithm.
    pub auto_ids: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { auto_ids: true }
    }
}

/// Render markdown `src` to an HTML string, kramdown/GFM parity.
pub fn render(src: &str) -> String {
    render_with(src, &Options::default())
}

pub fn render_with(src: &str, opts: &Options) -> String {
    // Normalize CRLF.
    let src = src.replace("\r\n", "\n").replace('\r', "\n");
    let doc = parse_doc(&src);
    let mut r = Renderer {
        idgen: IdGen::new(),
        opts: opts.clone(),
        footnotes: Vec::new(),
        toc_headings: Vec::new(),
    };
    let mut top = doc.nodes;
    // First pass: assign heading ids (needed for TOC).
    r.assign_ids(&mut top);
    let mut out = String::new();
    r.render_blocks(&top, &mut out, 0, true);
    // Append footnotes section if any were defined and referenced.
    r.render_footnotes(&mut out);
    let trimmed = out.trim_end_matches('\n');
    let mut result = trimmed.to_string();
    if result.is_empty() {
        return result;
    }
    // kramdown mirrors leading/trailing source blank lines.
    if doc.leading_blank {
        result.insert(0, '\n');
    }
    result.push('\n');
    if doc.trailing_blank {
        result.push('\n');
    }
    result
}

struct Renderer {
    idgen: IdGen,
    opts: Options,
    footnotes: Vec<(String, Vec<BlockNode>)>,
    toc_headings: Vec<(u8, String, String)>, // (level, id, inner_html)
}

impl Renderer {
    /// Walk headings in document order, assigning kramdown GFM ids (respecting
    /// explicit IAL ids), so that {:toc} and cross-refs resolve. Recurses into
    /// containers.
    fn assign_ids(&mut self, blocks: &mut [BlockNode]) {
        for node in blocks.iter_mut() {
            match &mut node.block {
                Block::Heading { text, attrs, level } => {
                    let id = if let Some(id) = &attrs.id {
                        id.clone()
                    } else if self.opts.auto_ids {
                        let rt = raw_text(text);
                        let id = self.idgen.generate(&rt);
                        attrs.id = Some(id.clone());
                        id
                    } else {
                        String::new()
                    };
                    if !attrs.has_ref("no_toc") && !id.is_empty() {
                        let inner = render_inline(text);
                        self.toc_headings.push((*level, id, inner));
                    }
                }
                Block::BlockQuote { blocks, .. } => self.assign_ids(blocks),
                Block::HtmlBlockMd { inner, .. } => self.assign_ids(inner),
                Block::List { items, .. } => {
                    for it in items.iter_mut() {
                        self.assign_ids(&mut it.blocks);
                    }
                }
                _ => {}
            }
        }
    }

    fn render_blocks(&mut self, blocks: &[BlockNode], out: &mut String, indent: usize, _top: bool) {
        let mut first = true;
        for node in blocks {
            let b = &node.block;
            if matches!(b, Block::Blank) {
                continue;
            }
            if matches!(b, Block::FootnoteDef { .. }) {
                if let Block::FootnoteDef { label, blocks } = b {
                    self.footnotes.push((label.clone(), blocks.clone()));
                }
                continue;
            }
            if !first {
                // kramdown: single '\n' when adjacent in source, '\n\n' when a
                // blank line separated the two blocks.
                out.push('\n');
                if node.blank_before {
                    out.push('\n');
                }
            }
            first = false;
            self.render_block(b, out, indent);
        }
    }

    fn render_block(&mut self, b: &Block, out: &mut String, indent: usize) {
        let pad = " ".repeat(indent);
        match b {
            Block::Heading { level, text, attrs } => {
                let inner = render_inline(text);
                out.push_str(&pad);
                out.push_str(&format!("<h{level}"));
                out.push_str(&attr_string(attrs, true));
                out.push('>');
                out.push_str(&inner);
                out.push_str(&format!("</h{level}>"));
            }
            Block::Paragraph { text, attrs } => {
                let inner = render_inline(text);
                out.push_str(&pad);
                out.push_str("<p");
                out.push_str(&attr_string(attrs, false));
                out.push('>');
                out.push_str(&inner);
                out.push_str("</p>");
            }
            Block::HorizontalRule => {
                out.push_str(&pad);
                out.push_str("<hr />");
            }
            Block::CodeBlock { lang, code } => {
                self.render_code(lang, code, out, indent);
            }
            Block::BlockQuote { blocks, attrs } => {
                out.push_str(&pad);
                out.push_str("<blockquote");
                out.push_str(&attr_string(attrs, false));
                out.push_str(">\n");
                self.render_blocks(blocks, out, indent + 2, false);
                out.push('\n');
                out.push_str(&pad);
                out.push_str("</blockquote>");
            }
            Block::Table {
                header,
                aligns,
                rows,
                attrs,
            } => {
                self.render_table(header, aligns, rows, attrs, out, indent);
            }
            Block::List {
                ordered,
                start,
                items,
                tight,
                attrs,
                is_toc,
            } => {
                if *is_toc {
                    self.render_toc(*ordered, out, indent);
                } else {
                    self.render_list(*ordered, *start, items, *tight, attrs, out, indent);
                }
            }
            Block::HtmlBlock { raw } => {
                // Raw passthrough, verbatim (no re-indent).
                out.push_str(raw);
            }
            Block::HtmlBlockMd {
                open_tag,
                inner,
                close_tag,
            } => {
                out.push_str(&pad);
                out.push_str(open_tag.trim_end());
                out.push('\n');
                // kramdown renders inner content indented by 2 within the
                // markdown="1" element (matching blockquote-style nesting).
                self.render_blocks(inner, out, indent + 2, false);
                out.push('\n');
                out.push_str(&pad);
                out.push_str(close_tag);
            }
            Block::FootnoteDef { .. } | Block::Blank => {}
        }
    }

    fn render_code(&self, lang: &str, code: &str, out: &mut String, indent: usize) {
        // render_md emits kramdown's un-highlighted fence form (Rouge is out of
        // scope — see lib.rs). Body is HTML-escaped; NO trailing newline is
        // added beyond the code's own.
        let pad = " ".repeat(indent);
        let escaped = crate::util::escape_html_text(code);
        out.push_str(&pad);
        if lang.is_empty() {
            out.push_str("<pre><code>");
        } else {
            out.push_str(&format!("<pre><code class=\"language-{}\">", escape_html_attr(lang)));
        }
        out.push_str(&escaped);
        out.push_str("</code></pre>");
    }

    fn render_table(
        &self,
        header: &[String],
        aligns: &[Align],
        rows: &[Vec<String>],
        attrs: &Attrs,
        out: &mut String,
        indent: usize,
    ) {
        let pad = " ".repeat(indent);
        out.push_str(&pad);
        out.push_str("<table");
        out.push_str(&attr_string(attrs, false));
        out.push_str(">\n");
        // thead
        out.push_str(&pad);
        out.push_str("  <thead>\n");
        out.push_str(&pad);
        out.push_str("    <tr>\n");
        for (i, cell) in header.iter().enumerate() {
            let a = aligns.get(i).copied().unwrap_or(Align::None);
            out.push_str(&pad);
            out.push_str("      <th");
            out.push_str(&align_style(a));
            out.push('>');
            out.push_str(&render_inline(cell));
            out.push_str("</th>\n");
        }
        out.push_str(&pad);
        out.push_str("    </tr>\n");
        out.push_str(&pad);
        out.push_str("  </thead>\n");
        // tbody
        out.push_str(&pad);
        out.push_str("  <tbody>\n");
        for row in rows {
            out.push_str(&pad);
            out.push_str("    <tr>\n");
            for i in 0..header.len() {
                let cell = row.get(i).map(|s| s.as_str()).unwrap_or("");
                let a = aligns.get(i).copied().unwrap_or(Align::None);
                out.push_str(&pad);
                out.push_str("      <td");
                out.push_str(&align_style(a));
                out.push('>');
                out.push_str(&render_inline(cell));
                out.push_str("</td>\n");
            }
            out.push_str(&pad);
            out.push_str("    </tr>\n");
        }
        out.push_str(&pad);
        out.push_str("  </tbody>\n");
        out.push_str(&pad);
        out.push_str("</table>");
    }

    #[allow(clippy::too_many_arguments)]
    fn render_list(
        &mut self,
        ordered: bool,
        start: Option<u64>,
        items: &[ListItem],
        tight: bool,
        attrs: &Attrs,
        out: &mut String,
        indent: usize,
    ) {
        let pad = " ".repeat(indent);
        let tag = if ordered { "ol" } else { "ul" };
        out.push_str(&pad);
        out.push_str(&format!("<{tag}"));
        if ordered {
            if let Some(s) = start {
                if s != 1 {
                    out.push_str(&format!(" start=\"{s}\""));
                }
            }
        }
        out.push_str(&attr_string(attrs, false));
        out.push_str(">\n");
        for item in items {
            self.render_list_item(item, tight, out, indent + 2);
        }
        out.push_str(&pad);
        out.push_str(&format!("</{tag}>"));
    }

    fn render_list_item(&mut self, item: &ListItem, tight: bool, out: &mut String, indent: usize) {
        let pad = " ".repeat(indent);
        out.push_str(&pad);
        out.push_str("<li>");
        // Determine content rendering: for a tight item whose content is a
        // single paragraph, kramdown emits the inline content directly with no
        // <p> and no surrounding newlines. Otherwise block content is rendered
        // indented.
        let non_blank: Vec<&Block> = item
            .blocks
            .iter()
            .map(|n| &n.block)
            .filter(|b| !matches!(b, Block::Blank))
            .collect();
        if tight && non_blank.len() == 1 {
            if let Block::Paragraph { text, .. } = non_blank[0] {
                out.push_str(&render_inline(text));
                out.push_str("</li>\n");
                return;
            }
        }
        if tight
            && non_blank
                .first()
                .map(|b| matches!(b, Block::Paragraph { .. }))
                .unwrap_or(false)
        {
            // First block paragraph inline, rest block-rendered (e.g. nested list).
            let mut idx = 0;
            if let Block::Paragraph { text, .. } = non_blank[0] {
                out.push_str(&render_inline(text));
                idx = 1;
            }
            for b in &non_blank[idx..] {
                out.push('\n');
                self.render_block(b, out, indent + 2);
            }
            out.push('\n');
            out.push_str(&pad);
            out.push_str("</li>\n");
            return;
        }
        // Loose or complex: block content on its own lines.
        out.push('\n');
        let owned: Vec<BlockNode> = item
            .blocks
            .iter()
            .filter(|n| !matches!(n.block, Block::Blank))
            .cloned()
            .collect();
        self.render_blocks(&owned, out, indent + 2, false);
        out.push('\n');
        out.push_str(&pad);
        out.push_str("</li>\n");
    }

    fn render_toc(&mut self, ordered: bool, out: &mut String, indent: usize) {
        // kramdown replaces a {:toc} list with a nested list of heading links,
        // filtered to toc_levels (1..3 per FHIR config), given id="markdown-toc".
        let pad = " ".repeat(indent);
        let tag = if ordered { "ol" } else { "ul" };
        let entries: Vec<(u8, String, String)> = self
            .toc_headings
            .iter()
            .filter(|(lvl, _, _)| *lvl >= 1 && *lvl <= 3)
            .cloned()
            .collect();
        out.push_str(&pad);
        out.push_str(&format!("<{tag} id=\"markdown-toc\">\n"));
        // Build nested structure.
        render_toc_entries(&entries, 0, tag, out, indent + 2);
        out.push_str(&pad);
        out.push_str(&format!("</{tag}>"));
    }

    fn render_footnotes(&mut self, out: &mut String) {
        if self.footnotes.is_empty() {
            return;
        }
        // kramdown emits <div class="footnotes" role="doc-endnotes"><ol>...
        out.push('\n');
        out.push_str("\n<div class=\"footnotes\" role=\"doc-endnotes\">\n  <ol>\n");
        for (i, (label, blocks)) in self.footnotes.clone().iter().enumerate() {
            let num = i + 1;
            let _ = label;
            out.push_str(&format!("    <li id=\"fn:{num}\">\n"));
            // Render blocks; append backlink to last paragraph.
            let mut inner = String::new();
            self.render_blocks(blocks, &mut inner, 6, false);
            // Insert backlink before closing </p> of last paragraph.
            let backlink = format!(
                " <a href=\"#fnref:{num}\" class=\"reversefootnote\" role=\"doc-backlink\">\u{21a9}</a>"
            );
            if let Some(pos) = inner.rfind("</p>") {
                inner.insert_str(pos, &backlink);
            } else {
                inner.push_str(&backlink);
            }
            out.push_str(&inner);
            out.push('\n');
            out.push_str("    </li>\n");
        }
        out.push_str("  </ol>\n</div>");
    }
}

fn render_toc_entries(
    entries: &[(u8, String, String)],
    start: usize,
    tag: &str,
    out: &mut String,
    indent: usize,
) -> usize {
    let pad = " ".repeat(indent);
    let mut i = start;
    while i < entries.len() {
        let (lvl, id, inner) = &entries[i];
        out.push_str(&pad);
        out.push_str(&format!("<li><a href=\"#{id}\">{inner}</a>"));
        // Look ahead for deeper children.
        if i + 1 < entries.len() && entries[i + 1].0 > *lvl {
            out.push('\n');
            out.push_str(&pad);
            out.push_str(&format!("  <{tag}>\n"));
            let next = render_toc_entries(entries, i + 1, tag, out, indent + 4);
            out.push_str(&pad);
            out.push_str(&format!("  </{tag}>\n"));
            out.push_str(&pad);
            out.push_str("</li>\n");
            i = next;
        } else {
            out.push_str("</li>\n");
            i += 1;
        }
        // Stop if the next entry is shallower than this branch's level.
        if i < entries.len() && entries[i].0 < *lvl {
            break;
        }
    }
    i
}

fn align_style(a: Align) -> String {
    match a {
        Align::None => String::new(),
        Align::Left => " style=\"text-align: left\"".to_string(),
        Align::Center => " style=\"text-align: center\"".to_string(),
        Align::Right => " style=\"text-align: right\"".to_string(),
    }
}

/// Build the HTML attribute string for a block, matching kramdown's emission
/// order. kramdown emits `id` first (when present), then other attributes in
/// insertion order, then `class` last. Verified against oracle output
/// (`<h2 id="x" class="y">`, `<p class="cls">`).
fn attr_string(attrs: &Attrs, _is_heading: bool) -> String {
    let mut s = String::new();
    if let Some(id) = &attrs.id {
        s.push_str(&format!(" id=\"{}\"", escape_html_attr(id)));
    }
    for (k, v) in &attrs.kv {
        // Skip kramdown control refs that never become HTML attrs.
        s.push_str(&format!(" {}=\"{}\"", k, escape_html_attr(v)));
    }
    if let Some(cls) = attrs.class_attr() {
        s.push_str(&format!(" class=\"{}\"", escape_html_attr(&cls)));
    }
    s
}

/// Re-export for tests / harness convenience.
pub fn render_document(src: &str) -> String {
    render(src)
}

// Silence unused import warning path for parse_blocks re-use in tests.
#[allow(unused_imports)]
use crate::block as _block;
