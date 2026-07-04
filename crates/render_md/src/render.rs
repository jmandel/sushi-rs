//! Top-level renderer: walk the block tree and emit kramdown-style HTML,
//! matching the indentation/whitespace quirks of kramdown's HTML converter.

use crate::block::{parse_doc, Align, Block, BlockNode, ListItem};
use crate::ial::Attrs;
use crate::inline::{
    normalize_open_tag,
    collect_footnote_refs, normalize_html_block, raw_text, render_inline,
};
use crate::util::{escape_html_attr, IdGen};

/// Rendering options. Defaults mirror the FHIR IG Publisher's Jekyll config.
#[derive(Debug, Clone)]
pub struct Options {
    /// Generate heading ids via the kramdown GFM algorithm.
    pub auto_ids: bool,
    /// Emit Jekyll's rouge highlighter wrappers on code spans / fenced blocks.
    ///
    /// render_md is used two ways: (a) the F1b differential gate compares
    /// against RAW kramdown (plain `<code>` / `<pre><code>`), and (b) the F5
    /// page pass uses render_md as Jekyll's `markdownify`, which post-processes
    /// kramdown output through Jekyll's default rouge integration
    /// (`syntax_highlighter: rouge`). DEFAULT FALSE keeps (a) byte-untouched;
    /// the page pass sets it true. Verified against Jekyll's real markdownify
    /// converter (NOT bare kramdown — bare kramdown does not add these classes).
    ///
    /// Covers the DETERMINISTIC (lexer-free) cases: every inline `<code>` gets
    /// `class="language-plaintext highlighter-rouge"`; a fenced block with no
    /// language (or a token-less lexer name — `plaintext`/`text`) gets the
    /// `<div class="language-X highlighter-rouge"><div class="highlight"><pre
    /// class="highlight"><code>…</code></pre></div></div>` wrapper. Fenced
    /// blocks in a REAL-lexer language (json/js/http/…) need a rouge tokenizer
    /// and are DEFERRED — they keep kramdown's plain form and fire a loud gap
    /// marker so the deferred pages classify (see `ROUGE_TOKENLESS_LANGS`).
    pub rouge_wrappers: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { auto_ids: true, rouge_wrappers: false }
    }
}

/// Fence languages that rouge tokenizes to PLAINTEXT (no token spans) — the
/// wrapper is deterministic (same as no-language, but the `language-X` class
/// carries the given name). Everything else in a real lexer language is deferred.
pub const ROUGE_TOKENLESS_LANGS: &[&str] = &["plaintext", "text"];

thread_local! {
    /// Whether the current render should emit rouge wrappers. Set for the
    /// duration of a `render_with` call (render_inline is a free fn with no
    /// options channel, so a thread-local carries the flag to `try_code_span`).
    pub(crate) static ROUGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn rouge_on() -> bool {
    ROUGE.with(|r| r.get())
}

/// Render markdown `src` to an HTML string, kramdown/GFM parity.
pub fn render(src: &str) -> String {
    render_with(src, &Options::default())
}

pub fn render_with(src: &str, opts: &Options) -> String {
    // Normalize CRLF.
    let src = src.replace("\r\n", "\n").replace('\r', "\n");
    // Carry the rouge-wrapper flag to the free inline renderer for this call.
    ROUGE.with(|r| r.set(opts.rouge_wrappers));
    let doc = parse_doc(&src);
    let mut r = Renderer {
        idgen: IdGen::new(),
        opts: opts.clone(),
        footnotes: Vec::new(),
        footnote_numbers: std::collections::HashMap::new(),
        toc_headings: Vec::new(),
    };
    // Install link reference definitions for reference-style links.
    crate::inline::set_link_refs(doc.link_refs.clone());
    let mut top = doc.nodes;
    // First pass: assign heading ids (needed for TOC).
    r.assign_ids(&mut top);
    // Footnote pre-pass: number references in first-reference order across all
    // inline text, so `[^label]` refs and the endnotes section agree.
    let mut fn_order: Vec<String> = Vec::new();
    collect_footnote_refs_in_blocks(&top, &mut fn_order);
    let mut fn_numbers = std::collections::HashMap::new();
    for (idx, label) in fn_order.iter().enumerate() {
        fn_numbers.insert(label.clone(), idx + 1);
    }
    r.footnote_numbers = fn_numbers.clone();
    crate::inline::set_footnote_numbers(fn_numbers);
    let mut out = String::new();
    r.render_blocks(&top, &mut out, 0, true);
    let had_footnotes_before = out.len();
    // Append footnotes section if any were defined and referenced.
    r.render_footnotes(&mut out);
    let footnotes_rendered = out.len() != had_footnotes_before;
    // kramdown trims trailing whitespace (spaces/newlines) at the very end of
    // the document (a raw HTML block's last line loses trailing spaces).
    let trimmed = out.trim_end_matches(['\n', ' ', '\t']);
    let mut result = trimmed.to_string();
    if result.is_empty() {
        // kramdown emits a single newline for empty / whitespace-only input.
        return "\n".to_string();
    }
    // kramdown mirrors leading/trailing source blank lines.
    if doc.leading_blank {
        result.insert(0, '\n');
    }
    result.push('\n');
    // The footnotes endnote section is always the last block and carries no
    // extra trailing blank, so the source-trailing-blank mirror is suppressed
    // when footnotes were emitted.
    if doc.trailing_blank && !footnotes_rendered {
        result.push('\n');
    }
    result
}

struct Renderer {
    idgen: IdGen,
    opts: Options,
    footnotes: Vec<(String, Vec<BlockNode>)>,
    footnote_numbers: std::collections::HashMap<String, usize>,
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
                    let id = if let Some(id) = attrs.id() {
                        id.to_string()
                    } else if self.opts.auto_ids {
                        let rt = raw_text(text);
                        let id = self.idgen.generate(&rt);
                        // Auto-id is APPENDED after any IAL classes (kramdown
                        // emits `class="no_toc" id="..."` for `{:.no_toc}` +
                        // auto-id), so use set_id which appends when absent.
                        attrs.set_id(id.clone());
                        id
                    } else {
                        String::new()
                    };
                    // A heading is excluded from the TOC if it carries the
                    // `no_toc` marker — kramdown accepts it as a class
                    // (`{:.no_toc}`) or a bare ref (`{:no_toc}`).
                    let no_toc = attrs.has_ref("no_toc")
                        || attrs
                            .ordered
                            .iter()
                            .any(|(k, v)| k == "class" && v.split_whitespace().any(|c| c == "no_toc"));
                    if !no_toc && !id.is_empty() {
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
            Block::BlockQuote {
                blocks,
                attrs,
                inner_leading_blank,
                inner_trailing_blank,
            } => {
                out.push_str(&pad);
                out.push_str("<blockquote");
                out.push_str(&attr_string(attrs, false));
                out.push_str(">\n");
                if *inner_leading_blank {
                    out.push('\n');
                }
                self.render_blocks(blocks, out, indent + 2, false);
                out.push('\n');
                if *inner_trailing_blank {
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push_str("</blockquote>");
            }
            Block::Table {
                header,
                aligns,
                body,
                footer,
                attrs,
            } => {
                self.render_table(header, aligns, body, footer, attrs, out, indent);
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
                    self.render_toc(*ordered, attrs, out, indent);
                } else {
                    self.render_list(*ordered, *start, items, *tight, attrs, out, indent);
                }
            }
            Block::HtmlBlock { raw } => {
                // kramdown parses raw HTML blocks and re-serializes: tag names
                // lowercased, void tags self-closed as ` />`. Interior
                // indentation/whitespace is preserved, but trailing whitespace
                // on the block's final (closing) line is trimmed. Comments pass
                // through verbatim.
                if raw.trim_start().starts_with("<!--") {
                    // Comment blocks pass through verbatim, re-indented by the
                    // nesting pad (like other raw HTML first lines).
                    out.push_str(&pad);
                    out.push_str(raw.trim_start_matches([' ', '\t']));
                } else {
                    let norm = normalize_html_block(raw);
                    // kramdown's block-start regex consumes the opening line's
                    // leading indent, so strip leading whitespace on the first
                    // line only; interior lines keep their indentation. When the
                    // block is nested (inside a markdown="1" element), the FIRST
                    // line is re-indented by the nesting pad; interior lines are
                    // NOT (verified against oracle). Trailing whitespace on the
                    // final line is trimmed.
                    let norm = norm.trim_start_matches([' ', '\t']);
                    out.push_str(&pad);
                    out.push_str(norm.trim_end_matches([' ', '\t']));
                }
            }
            Block::HtmlBlockMd {
                open_tag,
                inner,
                inner_leading_blank,
                inner_trailing_blank,
                close_tag,
            } => {
                // kramdown consumes the `markdown="1"` attribute (it triggers
                // re-parsing) and does NOT emit it. Also normalize the tag.
                let cleaned = strip_markdown_attr(open_tag);
                let open_norm = normalize_open_tag(&cleaned);
                out.push_str(&pad);
                out.push_str(open_norm.trim_end());
                out.push('\n');
                // A blank line after the open tag in the source is mirrored.
                if *inner_leading_blank {
                    out.push('\n');
                }
                // Inner content indented by 2 within the markdown="1" element.
                self.render_blocks(inner, out, indent + 2, false);
                out.push('\n');
                if *inner_trailing_blank {
                    out.push('\n');
                }
                // kramdown's Converter::Toc serializes the generated toc list
                // with a trailing blank line. When the toc is the LAST child of a
                // `markdown="1"` element, that blank lands before the closing tag
                // (`</ul>\n\n</div>`) — a plain list as the last child does NOT
                // (verified against kramdown 2.5.0). Emit the toc's trailing blank
                // here (it is absorbed by the block separator when the toc is
                // followed by another block, so it only needs adding at the tail).
                if inner
                    .iter()
                    .rev()
                    .find(|n| !matches!(n.block, Block::Blank))
                    .map(|n| matches!(n.block, Block::List { is_toc: true, .. }))
                    .unwrap_or(false)
                    && !*inner_trailing_blank
                {
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push_str(close_tag);
            }
            Block::HtmlBlockMdSpan {
                open_tag,
                inner_text,
                close_tag,
            } => {
                // SPAN content model (p, h1-h6, span, …): the inner text is
                // rendered at span level with newlines preserved verbatim; no
                // nested block elements, no re-indentation.
                let cleaned = strip_markdown_attr(open_tag);
                let open_norm = normalize_open_tag(&cleaned);
                out.push_str(&pad);
                out.push_str(open_norm.trim_end());
                out.push_str(&render_inline(inner_text));
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
        // Jekyll's rouge markdownify (Options::rouge_wrappers) post-processes a
        // fenced block into a div wrapper. For NO language or a TOKEN-LESS lexer
        // (plaintext/text) the transform is deterministic (content only escaped,
        // NO token spans) — verified against Jekyll's converter:
        //   <div class="language-X highlighter-rouge"><div class="highlight">
        //   <pre class="highlight"><code>{escaped}</code></pre></div></div>
        // A REAL-lexer language (json/js/http/…) needs a rouge tokenizer and is
        // DEFERRED — kept in kramdown's plain form so the affected pages classify
        // (the wrapper won't match, which is the intended deferral signal).
        let tokenless = lang.is_empty() || ROUGE_TOKENLESS_LANGS.contains(&lang);
        if self.opts.rouge_wrappers && tokenless {
            let cls = if lang.is_empty() { "plaintext" } else { lang };
            out.push_str(&pad);
            out.push_str(&format!(
                "<div class=\"language-{} highlighter-rouge\"><div class=\"highlight\"><pre class=\"highlight\"><code>",
                escape_html_attr(cls)
            ));
            out.push_str(&escaped);
            out.push_str("</code></pre></div></div>");
            return;
        }
        // Real-lexer languages (json/js): a rouge tokenizer produces the inner
        // token spans; the wrapper is identical to the tokenless one.
        if self.opts.rouge_wrappers {
            if let Some(inner) = crate::rouge::highlight(lang, code) {
                out.push_str(&pad);
                out.push_str(&format!(
                    "<div class=\"language-{} highlighter-rouge\"><div class=\"highlight\"><pre class=\"highlight\"><code>",
                    escape_html_attr(lang)
                ));
                out.push_str(&inner);
                out.push_str("</code></pre></div></div>");
                return;
            }
        }
        out.push_str(&pad);
        if lang.is_empty() {
            out.push_str("<pre><code>");
        } else {
            out.push_str(&format!("<pre><code class=\"language-{}\">", escape_html_attr(lang)));
        }
        out.push_str(&escaped);
        out.push_str("</code></pre>");
    }

    #[allow(clippy::too_many_arguments)]
    fn render_table(
        &self,
        header: &Option<Vec<String>>,
        aligns: &[Align],
        body: &[Vec<Vec<String>>],
        footer: &Option<Vec<Vec<String>>>,
        attrs: &Attrs,
        out: &mut String,
        indent: usize,
    ) {
        let pad = " ".repeat(indent);
        // Column count = the MAX column count across the HEADER and every
        // body/footer row. The SEPARATOR (alignment) row does NOT widen the
        // table: kramdown reads `|---|---|---|` under a 2-col header as a 2-col
        // table (the extra `---` is ignored) — verified against the oracle. A
        // header/body row with more cells DOES widen it. (Alignment beyond
        // `ncols` is simply unused.)
        let hcols = header.as_ref().map(|h| h.len()).unwrap_or(0);
        let bcols = body
            .iter()
            .flatten()
            .chain(footer.iter().flatten())
            .map(|r| r.len())
            .max()
            .unwrap_or(0);
        let ncols = hcols.max(bcols);
        out.push_str(&pad);
        out.push_str("<table");
        out.push_str(&attr_string(attrs, false));
        out.push_str(">\n");
        if let Some(h) = header {
            out.push_str(&pad);
            out.push_str("  <thead>\n");
            self.render_table_row(h, aligns, ncols, "th", &pad, out);
            out.push_str(&pad);
            out.push_str("  </thead>\n");
        }
        for group in body {
            out.push_str(&pad);
            out.push_str("  <tbody>\n");
            for row in group {
                self.render_table_row(row, aligns, ncols, "td", &pad, out);
            }
            out.push_str(&pad);
            out.push_str("  </tbody>\n");
        }
        if let Some(f) = footer {
            out.push_str(&pad);
            out.push_str("  <tfoot>\n");
            for row in f {
                self.render_table_row(row, aligns, ncols, "td", &pad, out);
            }
            out.push_str(&pad);
            out.push_str("  </tfoot>\n");
        }
        out.push_str(&pad);
        out.push_str("</table>");
    }

    fn render_table_row(
        &self,
        row: &[String],
        aligns: &[Align],
        ncols: usize,
        cell_tag: &str,
        pad: &str,
        out: &mut String,
    ) {
        out.push_str(pad);
        out.push_str("    <tr>\n");
        for i in 0..ncols {
            let missing = i >= row.len();
            let cell = row.get(i).map(|s| s.as_str()).unwrap_or("");
            let a = aligns.get(i).copied().unwrap_or(Align::None);
            out.push_str(pad);
            out.push_str(&format!("      <{cell_tag}"));
            out.push_str(&align_style(a));
            out.push('>');
            // kramdown cell filling (verified against oracle): an empty or
            // missing cell within the ALIGNMENT range renders as a
            // non-breaking space (the parser pads rows to the separator's
            // column count with empty cells); a column that exists only
            // beyond the alignment range is padded with a regular space.
            if missing && i >= aligns.len() {
                out.push(' ');
            } else if missing || cell.trim().is_empty() {
                out.push('\u{a0}');
            } else {
                out.push_str(&render_inline(cell));
            }
            out.push_str(&format!("</{cell_tag}>\n"));
        }
        out.push_str(pad);
        out.push_str("    </tr>\n");
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
        // kramdown does NOT emit a `start` attribute for ordered lists that
        // begin at a number other than 1 — the marker value is ignored in the
        // HTML output. (Verified against oracle: `7.`-started list -> `<ol>`.)
        let _ = (start, tight);
        out.push_str(&format!("<{tag}"));
        // GFM task list: the list element gets class="task-list" when any item
        // is a task item.
        let is_task_list = items.iter().any(|it| it.task.is_some());
        if is_task_list {
            out.push_str(" class=\"task-list\"");
        }
        out.push_str(&attr_string(attrs, false));
        out.push_str(">\n");

        // Compute per-item first-paragraph transparency, replicating kramdown
        // (parser/kramdown/list.rb:132-139). An item's first paragraph is
        // transparent (no <p>) when it is a paragraph not immediately followed
        // by a blank line, with a special rule for the LAST item: it is
        // transparent only if some EARLIER item is itself non-transparent-first
        // (i.e. the list is not uniformly tight-simple). See below.
        let n = items.len();
        // base condition per item: first is a paragraph, AND it is not directly
        // followed by a blank line — either an internal blank before its 2nd
        // block, or (for a non-last item) a blank separating it from the next
        // sibling (kramdown appends a trailing `:blank`, forcing a real <p>).
        let base: Vec<bool> = items
            .iter()
            .enumerate()
            .map(|(idx, it)| {
                let nodes: Vec<&BlockNode> = it
                    .blocks
                    .iter()
                    .filter(|nd| !matches!(nd.block, Block::Blank))
                    .collect();
                let first_is_para = nodes
                    .first()
                    .map(|nd| matches!(nd.block, Block::Paragraph { .. }))
                    .unwrap_or(false);
                let second_blank = nodes.get(1).map(|nd| nd.blank_before).unwrap_or(false);
                // A trailing blank (sibling separator) appends a `:blank` as the
                // item's LAST child. It only forces a real <p> when the item is a
                // lone paragraph (so the `:blank` becomes the 2nd child); if the
                // item already has a following block (e.g. a nested list) that
                // block is the 2nd child and governs transparency instead.
                let sibling_blank = it.followed_by_blank && idx + 1 < n && nodes.len() == 1;
                first_is_para && !second_blank && !sibling_blank
            })
            .collect();
        let mut transparent = base.clone();
        if n >= 2 {
            // For the last item, transparency also requires that some earlier
            // item's first child is NOT a plain (non-transparent) paragraph.
            let earlier_has_non_p_or_transparent = (0..n - 1).any(|i| {
                let it = &items[i];
                let first = it
                    .blocks
                    .iter()
                    .find(|nd| !matches!(nd.block, Block::Blank));
                match first {
                    None => true,
                    Some(nd) => !matches!(nd.block, Block::Paragraph { .. }) || transparent[i],
                }
            });
            if !earlier_has_non_p_or_transparent {
                transparent[n - 1] = false;
            }
        }

        for (i, item) in items.iter().enumerate() {
            self.render_list_item(item, transparent[i], out, indent + 2);
        }
        out.push_str(&pad);
        out.push_str(&format!("</{tag}>"));
    }

    fn render_list_item(&mut self, item: &ListItem, transparent: bool, out: &mut String, indent: usize) {
        let pad = " ".repeat(indent);
        out.push_str(&pad);
        out.push_str("<li");
        if item.task.is_some() {
            out.push_str(" class=\"task-list-item\"");
        }
        out.push_str(&attr_string(&item.attrs, false));
        out.push('>');
        if let Some(checked) = item.task {
            out.push_str("<input type=\"checkbox\" class=\"task-list-item-checkbox\" disabled=\"disabled\"");
            if checked {
                out.push_str(" checked=\"checked\"");
            }
            out.push_str(" />");
        }
        let nodes: Vec<&BlockNode> = item
            .blocks
            .iter()
            .filter(|n| !matches!(n.block, Block::Blank))
            .collect();

        if nodes.is_empty() {
            out.push_str("</li>\n");
            return;
        }

        if transparent {
            // First paragraph inline; remaining blocks (if any) on their own
            // lines with their source-derived separators.
            if let Block::Paragraph { text, .. } = &nodes[0].block {
                out.push_str(&render_inline(text));
            }
            if nodes.len() == 1 {
                out.push_str("</li>\n");
                return;
            }
            for n in &nodes[1..] {
                out.push('\n');
                if n.blank_before {
                    out.push('\n');
                }
                self.render_block(&n.block, out, indent + 2);
            }
            out.push('\n');
            out.push_str(&pad);
            out.push_str("</li>\n");
            return;
        }

        // Non-transparent: block content on its own lines.
        out.push('\n');
        let owned: Vec<BlockNode> = nodes.iter().map(|n| (*n).clone()).collect();
        self.render_blocks(&owned, out, indent + 2, false);
        out.push('\n');
        out.push_str(&pad);
        out.push_str("</li>\n");
    }

    fn render_toc(&mut self, ordered: bool, attrs: &Attrs, out: &mut String, indent: usize) {
        // kramdown replaces a {:toc} list with a nested list of heading links,
        // filtered to toc_levels (1..3 per FHIR config), given id="markdown-toc".
        // IAL attributes on the {:toc} list (e.g. class="no_toc") are emitted
        // BEFORE the auto id.
        //
        // The generated toc `<ul>` is ALWAYS emitted at BASE indent 0, even when
        // the `{:toc}` list sits inside an indented `markdown="1"` HTML block
        // (verified against kramdown 2.5.0: a toc nested in one or two
        // `<div markdown="1">`s still opens `<ul id="markdown-toc">` at column 0,
        // `<li>` at 2). kramdown's Converter::Toc builds the replacement list as a
        // fresh block tree whose serialization starts from the converter's base
        // indent, NOT the enclosing HTML block's `indent`. So we ignore the passed
        // `indent` for the toc wrapper. (Normal block children of a markdown="1"
        // div DO inherit indent+2 — this reset is toc-specific.)
        let _ = indent;
        let indent = 0usize;
        let pad = " ".repeat(indent);
        let tag = if ordered { "ol" } else { "ul" };
        let entries: Vec<(u8, String, String)> = self
            .toc_headings
            .iter()
            .filter(|(lvl, _, _)| *lvl >= 1 && *lvl <= 3)
            .cloned()
            .collect();
        out.push_str(&pad);
        out.push_str(&format!("<{tag}"));
        out.push_str(&attr_string(attrs, false));
        if attrs.id().is_none() {
            out.push_str(" id=\"markdown-toc\"");
        }
        out.push_str(">\n");
        let mut pos = 0;
        render_toc_level(&entries, &mut pos, u8::MAX, tag, indent + 2, out);
        out.push_str(&pad);
        out.push_str(&format!("</{tag}>"));
    }

    fn render_footnotes(&mut self, out: &mut String) {
        // Only footnotes that are actually referenced appear, in reference
        // order (kramdown numbers/orders footnotes by first reference).
        let defs: std::collections::HashMap<String, Vec<BlockNode>> =
            self.footnotes.iter().cloned().collect();
        let mut ordered: Vec<(usize, String)> = self
            .footnote_numbers
            .iter()
            .filter(|(label, _)| defs.contains_key(*label))
            .map(|(label, n)| (*n, label.clone()))
            .collect();
        ordered.sort_by_key(|(n, _)| *n);
        if ordered.is_empty() {
            return;
        }
        // kramdown emits <div class="footnotes" role="doc-endnotes"><ol>...
        out.push('\n');
        out.push_str("\n<div class=\"footnotes\" role=\"doc-endnotes\">\n  <ol>\n");
        for (_num, label) in &ordered {
            let esc = escape_html_attr(label);
            out.push_str(&format!("    <li id=\"fn:{esc}\">\n"));
            let blocks = defs.get(label).cloned().unwrap_or_default();
            let mut inner = String::new();
            self.render_blocks(&blocks, &mut inner, 6, false);
            // kramdown separates the footnote text from the backlink with a
            // non-breaking space (U+00A0), not an ordinary space.
            let backlink = format!(
                "\u{a0}<a href=\"#fnref:{esc}\" class=\"reversefootnote\" role=\"doc-backlink\">&#8617;</a>"
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

/// Render one level of the TOC. Consumes entries whose level is `> parent_lvl`
/// until an entry at `<= parent_lvl` is reached. Matches kramdown's exact
/// layout: a child `<ul>` opens on the SAME line as the parent link (preceded
/// by 4 spaces), and each link gets `id="markdown-toc-<heading-id>"`.
fn render_toc_level(
    entries: &[(u8, String, String)],
    pos: &mut usize,
    parent_lvl: u8,
    tag: &str,
    indent: usize,
    out: &mut String,
) {
    let pad = " ".repeat(indent);
    while *pos < entries.len() {
        let (lvl, id, inner) = entries[*pos].clone();
        // Stop this nested level when we reach a heading at the parent's level
        // or shallower (u8::MAX = top level, never stops).
        if parent_lvl != u8::MAX && lvl <= parent_lvl {
            return;
        }
        *pos += 1;
        out.push_str(&pad);
        out.push_str(&format!(
            "<li><a href=\"#{id}\" id=\"markdown-toc-{id}\">{inner}</a>"
        ));
        // Does the next entry go deeper? Then open a nested list on this line.
        let has_child = *pos < entries.len() && entries[*pos].0 > lvl;
        if has_child {
            // The nested list opens on the same line, preceded by (indent + 2)
            // spaces (matching kramdown's layout).
            out.push_str(&" ".repeat(indent + 2));
            out.push_str(&format!("<{tag}>\n"));
            render_toc_level(entries, pos, lvl, tag, indent + 4, out);
            out.push_str(&pad);
            out.push_str(&format!("  </{tag}>\n"));
            out.push_str(&pad);
            out.push_str("</li>\n");
        } else {
            out.push_str("</li>\n");
        }
    }
}

/// Remove a `markdown="1"` / `markdown='1'` attribute (and its surrounding
/// whitespace) from an opening tag string.
fn strip_markdown_attr(open_tag: &str) -> String {
    let mut s = open_tag.to_string();
    for pat in [" markdown=\"1\"", " markdown='1'", "markdown=\"1\"", "markdown='1'"] {
        s = s.replace(pat, "");
    }
    s
}

/// Walk the block tree collecting footnote reference labels in first-reference
/// order (footnote DEFINITION bodies are excluded — a footnote referenced only
/// inside another footnote is numbered when first cited in the main text).
fn collect_footnote_refs_in_blocks(nodes: &[BlockNode], order: &mut Vec<String>) {
    for node in nodes {
        match &node.block {
            Block::Paragraph { text, .. } | Block::Heading { text, .. } => {
                collect_footnote_refs(text, order);
            }
            Block::Table { header, body, footer, .. } => {
                for cell in header.iter().flatten() {
                    collect_footnote_refs(cell, order);
                }
                for row in body.iter().flatten().chain(footer.iter().flatten()) {
                    for cell in row {
                        collect_footnote_refs(cell, order);
                    }
                }
            }
            Block::BlockQuote { blocks, .. } | Block::HtmlBlockMd { inner: blocks, .. } => {
                collect_footnote_refs_in_blocks(blocks, order);
            }
            Block::List { items, .. } => {
                for it in items {
                    collect_footnote_refs_in_blocks(&it.blocks, order);
                }
            }
            _ => {}
        }
    }
}

fn align_style(a: Align) -> String {
    match a {
        Align::None => String::new(),
        Align::Left => " style=\"text-align: left\"".to_string(),
        Align::Center => " style=\"text-align: center\"".to_string(),
        Align::Right => " style=\"text-align: right\"".to_string(),
    }
}

/// Build the HTML attribute string for a block. kramdown emits attributes in
/// the insertion order of its attribute Hash — which `Attrs::ordered` records
/// exactly (`{:.no_toc #id}` -> `class="no_toc" id="id"`; auto-ids appended
/// after IAL classes).
fn attr_string(attrs: &Attrs, _is_heading: bool) -> String {
    let mut s = String::new();
    for (k, v) in &attrs.ordered {
        s.push_str(&format!(" {}=\"{}\"", k, escape_html_attr(v)));
    }
    s
}

