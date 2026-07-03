//! Block-level parsing into a flat list of blocks, with IAL attachment.

use crate::ial::{parse_block_ial_line, Attrs};

/// kramdown's HTML_SPAN_ELEMENTS (kramdown-2.5.0 parser/html.rb:57-59). A raw
/// HTML tag whose name is in this set is treated as INLINE (span) content: it
/// does NOT start an HTML block and does NOT interrupt a paragraph.
const HTML_SPAN_ELEMENTS: &[&str] = &[
    "a", "abbr", "acronym", "b", "big", "bdo", "br", "button", "cite", "code", "del", "dfn", "em",
    "i", "img", "input", "ins", "kbd", "label", "mark", "option", "q", "rb", "rbc", "rp", "rt",
    "rtc", "ruby", "samp", "select", "small", "span", "strong", "sub", "sup", "time", "tt", "u",
    "var",
];

fn is_span_element(name: &str) -> bool {
    HTML_SPAN_ELEMENTS.contains(&name)
}

/// kramdown HTML_CONTENT_MODEL_SPAN (parser/html.rb:42-44): elements whose
/// `markdown="1"` content is parsed at SPAN level.
fn is_span_content_model(name: &str) -> bool {
    matches!(
        name,
        "a" | "abbr" | "acronym" | "b" | "bdo" | "big" | "button" | "cite" | "caption" | "del"
            | "dfn" | "dt" | "em" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "i" | "ins"
            | "label" | "legend" | "optgroup" | "p" | "q" | "rb" | "rbc" | "rp" | "rt" | "rtc"
            | "ruby" | "select" | "small" | "span" | "strong" | "sub" | "sup" | "th" | "tt"
    )
}

/// kramdown HTML_CONTENT_MODEL_BLOCK (parser/html.rb:38-41): elements whose
/// `markdown="1"` content is parsed at BLOCK level. Everything not block/span
/// (table, ul, ol, tr, tbody, pre, script, …) has RAW content model.
fn is_block_content_model(name: &str) -> bool {
    matches!(
        name,
        "address" | "applet" | "article" | "aside" | "blockquote" | "body" | "dd" | "details"
            | "div" | "dl" | "fieldset" | "figure" | "figcaption" | "footer" | "form" | "header"
            | "hgroup" | "iframe" | "li" | "main" | "map" | "menu" | "nav" | "noscript"
            | "object" | "section" | "summary" | "td"
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ContentModel {
    Block,
    Span,
    Raw,
}

fn default_content_model(name: &str) -> ContentModel {
    if is_block_content_model(name) {
        ContentModel::Block
    } else if is_span_content_model(name) {
        ContentModel::Span
    } else {
        ContentModel::Raw
    }
}

/// kramdown block-extension line, e.g. `{::options toc_levels="1..4"/}` or
/// `{::comment}`/`{::nomarkdown}`. Distinguished from a block IAL (`{: ... }`)
/// by the DOUBLE colon `{::`. These extensions produce no direct HTML output;
/// we consume the line (treated like a blank line for block boundaries). Full
/// extension semantics (comment/nomarkdown bodies) are out of scope — the
/// corpus uses only the self-contained `{::options .../}` / `{::download}`
/// forms.
fn is_kramdown_ext_line(l: &str) -> bool {
    l.trim_start().starts_with("{::")
}

/// kramdown LAZY_END_HTML_START/STOP (parser/kramdown/paragraph.rb:21-22):
/// a lazy continuation (of a paragraph, list item, or indented code block)
/// stops at a line beginning with an HTML tag whose name is NOT a span
/// element (e.g. `<figure>`, `</div>`), within OPT_SPACE.
fn is_lazy_end_html(l: &str) -> bool {
    if leading_spaces(l) > 3 {
        return false;
    }
    let t = l.trim_start();
    if !t.starts_with('<') {
        return false;
    }
    match html_tag_name(t) {
        Some(name) => !is_span_element(&name),
        None => false,
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // `Blank` is a defensive variant; filtered but not built.
pub enum Block {
    Heading {
        level: u8,
        text: String,
        attrs: Attrs,
    },
    Paragraph {
        text: String,
        attrs: Attrs,
    },
    /// Fenced or indented code. `lang` is the info string (may be empty).
    CodeBlock {
        lang: String,
        code: String,
    },
    /// kramdown pipe table. `header` is present only when the source had a
    /// separator (`|---|`) row; otherwise all rows are body rows (no <thead>).
    Table {
        header: Option<Vec<String>>,
        aligns: Vec<Align>,
        /// Body row groups. Each group becomes a `<tbody>`; a `=` footer line
        /// splits groups and the last becomes `<tfoot>`.
        body: Vec<Vec<Vec<String>>>,
        footer: Option<Vec<Vec<String>>>,
        attrs: Attrs,
    },
    /// A bullet/ordered list.
    List {
        ordered: bool,
        start: Option<u64>,
        items: Vec<ListItem>,
        tight: bool,
        attrs: Attrs,
        /// True if this list carried the `{:toc}` ref.
        is_toc: bool,
    },
    BlockQuote {
        blocks: Vec<BlockNode>,
        attrs: Attrs,
    },
    /// Raw HTML block passed through. If `reparse` is set, inner content between
    /// the outer tags is re-parsed as markdown (markdown="1").
    HtmlBlock {
        raw: String,
    },
    HtmlBlockMd {
        open_tag: String,
        inner: Vec<BlockNode>,
        /// Whether the inner content ended with a blank line before the close
        /// tag (kramdown emits a corresponding trailing blank inside).
        inner_trailing_blank: bool,
        close_tag: String,
    },
    /// A `markdown="1"` element whose tag has SPAN content model (p, h1-h6,
    /// span, th, … — kramdown parser/html.rb:42-44): the inner text is parsed
    /// at SPAN level only, newlines preserved verbatim.
    HtmlBlockMdSpan {
        open_tag: String,
        inner_text: String,
        close_tag: String,
    },
    HorizontalRule,
    /// Footnote definition, collected and rendered at the end.
    FootnoteDef {
        label: String,
        blocks: Vec<BlockNode>,
    },
    Blank,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone)]
pub struct ListItem {
    /// The item's child blocks.
    pub blocks: Vec<BlockNode>,
    /// Whether this item's paragraph should be tight (no <p>) — decided at list
    /// level, but kept per-item for flexibility.
    #[allow(dead_code)]
    pub tight: bool,
    /// True if a blank line separated this item from the next item (or the end
    /// of the list). kramdown appends a trailing `:blank` to such an item,
    /// which forces its first paragraph to render as a real `<p>` (loose).
    pub followed_by_blank: bool,
    /// Attributes from a LIST_ITEM_IAL — an IAL at the very start of the
    /// item's content (`- {:#id}text`), applied to the `<li>`
    /// (kramdown parser/kramdown/list.rb:19-20, 76-80).
    pub attrs: Attrs,
    /// GFM task-list item: `- [ ] x` (Some(false)) / `- [x] x` (Some(true)).
    pub task: Option<bool>,
}

/// A block plus whether it was preceded by a blank line in the source. The
/// `blank_before` flag drives kramdown's inter-block separator: adjacent blocks
/// (no blank line between) are joined with a single `\n`; blank-separated
/// blocks with `\n\n`.
#[derive(Debug, Clone)]
pub struct BlockNode {
    pub block: Block,
    pub blank_before: bool,
}

/// Parsed document: the block nodes plus whether the source began / ended with
/// blank line(s) (kramdown mirrors these as a leading `\n` and an extra
/// trailing `\n`).
#[derive(Debug, Clone)]
pub struct Doc {
    pub nodes: Vec<BlockNode>,
    pub leading_blank: bool,
    pub trailing_blank: bool,
    /// Link reference definitions found in this document: normalized label ->
    /// (destination, optional title).
    pub link_refs: std::collections::HashMap<String, (String, Option<String>)>,
}

/// Extract link reference definitions (`[label]: dest "title"`) from `src`,
/// returning the source with those lines removed and the collected map. Only
/// definition lines at a block boundary (not indented as code) are taken; a
/// `[//]: # (...)` "markdown comment" is also a link definition and is removed.
fn extract_link_refs(
    src: &str,
) -> (
    String,
    std::collections::HashMap<String, (String, Option<String>)>,
) {
    let mut map = std::collections::HashMap::new();
    let mut kept: Vec<&str> = Vec::new();
    for line in src.split('\n') {
        if let Some((label, dest, title)) = parse_link_ref_def(line) {
            map.entry(crate::inline::normalize_ref_label(&label))
                .or_insert((dest, title));
            // drop the line
            continue;
        }
        kept.push(line);
    }
    (kept.join("\n"), map)
}

/// Parse a single line as a link reference definition. Returns
/// (label, destination, title) if it matches `[label]: dest ["title"]` with at
/// most 3 leading spaces.
fn parse_link_ref_def(line: &str) -> Option<(String, String, Option<String>)> {
    let indent = line.len() - line.trim_start().len();
    if indent > 3 {
        return None;
    }
    let t = line.trim_start();
    if !t.starts_with('[') {
        return None;
    }
    // A `[^...]:` line is a FOOTNOTE definition, not a link reference.
    if t.starts_with("[^") {
        return None;
    }
    let close = t.find("]:")?;
    let label = &t[1..close];
    if label.is_empty() || label.contains('[') {
        return None;
    }
    let rest = t[close + 2..].trim();
    if rest.is_empty() {
        return None;
    }
    // destination = first whitespace-delimited token (or <...>)
    let (dest, after) = if let Some(stripped) = rest.strip_prefix('<') {
        let end = stripped.find('>')?;
        (stripped[..end].to_string(), stripped[end + 1..].trim())
    } else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        (rest[..end].to_string(), rest[end..].trim())
    };
    // optional title: "..." '...' (...)
    let title = if after.is_empty() {
        None
    } else if (after.starts_with('"') && after.ends_with('"') && after.len() >= 2)
        || (after.starts_with('\'') && after.ends_with('\'') && after.len() >= 2)
    {
        Some(after[1..after.len() - 1].to_string())
    } else if after.starts_with('(') && after.ends_with(')') && after.len() >= 2 {
        Some(after[1..after.len() - 1].to_string())
    } else {
        // Trailing junk (e.g. `[//]: # ## heading`) — still a definition; the
        // remainder is treated as an (ignored) title.
        Some(after.to_string())
    };
    Some((label.to_string(), dest, title))
}

/// Parse a full document. `lines` is the raw source split on '\n'.
pub fn parse_doc(src: &str) -> Doc {
    parse_doc_with_opts(src, false)
}

/// Parse with an initial `parse_block_html` state (inherited by nested
/// markdown="1"/parse_block_html re-entry).
fn parse_doc_with_opts(src: &str, parse_block_html: bool) -> Doc {
    let (src_owned, link_refs) = extract_link_refs(src);
    let src = src_owned.as_str();
    let lines: Vec<String> = src.split('\n').map(String::from).collect();
    let mut p = Parser {
        lines,
        i: 0,
        block_start: 0,
        parse_block_html,
    };
    // Leading blank: any blank line(s) — or consumed kramdown extension lines —
    // before the first real block.
    let mut leading_blank = false;
    {
        let mut k = 0;
        while k < p.lines.len()
            && (p.lines[k].trim().is_empty() || is_kramdown_ext_line(&p.lines[k]))
        {
            leading_blank = true;
            k += 1;
        }
        // Only count as leading blank if there IS a first block.
        if k >= p.lines.len() {
            leading_blank = false;
        }
    }
    let nodes = p.parse_until(|_| false);
    // Trailing blank: the source ended with a blank line after the last
    // content. Strip ONE trailing '\n' (the file terminator) so it isn't
    // mistaken for a blank line, then check whether a blank line remains at the
    // end. Any blank line beyond the terminator => kramdown emits an extra
    // trailing '\n'.
    let trailing_blank = {
        let body = src.strip_suffix('\n').unwrap_or(src);
        // remove trailing spaces/tabs on the last line
        let trimmed = body.trim_end_matches([' ', '\t']);
        trimmed.ends_with('\n')
    };
    Doc {
        nodes,
        leading_blank,
        trailing_blank,
        link_refs,
    }
}

/// Back-compat: parse to a plain block list (used by nested contexts that
/// don't need the doc-level flags).
#[allow(dead_code)]
pub fn parse_blocks(src: &str) -> Vec<Block> {
    parse_doc(src).nodes.into_iter().map(|n| n.block).collect()
}

fn parse_block_nodes_with(src: &str, parse_block_html: bool) -> Vec<BlockNode> {
    parse_doc_with_opts(src, parse_block_html).nodes
}

struct Parser {
    /// Owned lines so the parser can SPLICE: when a raw-HTML block's close tag
    /// is followed by more content on the same line, the remainder is inserted
    /// as a new line and parsed as the next block (kramdown behavior).
    lines: Vec<String>,
    i: usize,
    /// Set by read_open_tag to allow raw-HTML re-scan from the element start.
    block_start: usize,
    /// kramdown's `parse_block_html` option, toggled by
    /// `{::options parse_block_html="true|false" /}` mid-document. When true,
    /// block HTML elements' content is parsed per their default content model
    /// even without a `markdown` attribute (html.rb:32-33).
    parse_block_html: bool,
}

impl Parser {
    fn parse_until(&mut self, stop: impl Fn(&str) -> bool) -> Vec<BlockNode> {
        let mut nodes: Vec<BlockNode> = Vec::new();
        let mut pending_blank = false;
        // An IAL that could not attach to a PRECEDING block (start of doc, or
        // separated from the previous block by a blank line) is held and
        // applied to the NEXT block (kramdown stores it in @block_ial).
        let mut pending_ial: Option<Attrs> = None;
        macro_rules! push {
            ($b:expr) => {{
                let mut b = $b;
                if let Some(a) = pending_ial.take() {
                    set_block_attrs(&mut b, a);
                }
                nodes.push(BlockNode {
                    block: b,
                    blank_before: pending_blank,
                });
                pending_blank = false;
            }};
        }
        while self.i < self.lines.len() {
            let line = self.lines[self.i].clone();
            if stop(&line) {
                break;
            }
            // kramdown block extension `{::...}` — consume, no output, treated
            // as a block boundary (like a blank line). `{::options .../}` can
            // toggle parser options; the corpus uses `parse_block_html`.
            if is_kramdown_ext_line(&line) {
                if line.contains("{::options") {
                    if line.contains("parse_block_html=\"true\"")
                        || line.contains("parse_block_html='true'")
                    {
                        self.parse_block_html = true;
                    } else if line.contains("parse_block_html=\"false\"")
                        || line.contains("parse_block_html='false'")
                    {
                        self.parse_block_html = false;
                    }
                }
                pending_blank = true;
                self.i += 1;
                continue;
            }
            // Standalone block IAL: attaches to the previous block when
            // DIRECTLY adjacent (no blank line between); otherwise held for
            // the NEXT block (kramdown block-IAL placement rules).
            if let Some(attrs) = parse_block_ial_line(&line) {
                self.i += 1;
                if !pending_blank && !nodes.is_empty() {
                    attach_ial_nodes(&mut nodes, attrs);
                } else {
                    pending_ial = Some(match pending_ial.take() {
                        None => attrs,
                        Some(mut prev) => {
                            let mut a = attrs;
                            merge_attrs(&mut prev, &mut a);
                            prev
                        }
                    });
                }
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                pending_blank = true;
                self.i += 1;
                continue;
            }
            // Parser order mirrors kramdown's @block_parsers
            // (kramdown-2.5.0/lib/kramdown/parser/kramdown.rb:75-78):
            // codeblock, codeblock_fenced, blockquote, atx_header, hr, list,
            // block_html, setext_header, table, footnote_definition, paragraph.
            if let Some(b) = self.try_indented_code() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_fenced_code() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_blockquote() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_atx_heading() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_hr() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_list() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_html_block() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_table() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_footnote_def() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_setext_or_paragraph() {
                push!(b);
                continue;
            }
            // fallback: consume line as paragraph
            self.i += 1;
        }
        nodes
    }

    fn try_atx_heading(&mut self) -> Option<Block> {
        let line = self.lines[self.i].clone();
        // kramdown/GFM ATX headers must start at COLUMN 0 (gfm.rb:146
        // `/^(?<level>\#{1,6})[\t ]+/` — no OPT_SPACE). ` # x` is a paragraph.
        if !line.starts_with('#') {
            return None;
        }
        let t = line.as_str();
        let mut level = 0;
        for c in t.chars() {
            if c == '#' {
                level += 1;
            } else {
                break;
            }
        }
        if level == 0 || level > 6 {
            return None;
        }
        let rest = &t[level..];
        // ATX requires a space after # (GFM).
        if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
            return None;
        }
        let mut text = rest.trim().to_string();
        // strip trailing closing hashes
        text = strip_trailing_hashes(&text);
        // kramdown HEADER_ID (parser/kramdown/header.rb):
        // `/(?:[ \t]+\{#((?:\w|[\w-:.])+)\})?/` — a trailing ` {#id}` on the
        // header line sets an explicit id.
        let mut attrs = Attrs::default();
        if text.ends_with('}') {
            if let Some(open) = text.rfind("{#") {
                let id = &text[open + 2..text.len() - 1];
                if !id.is_empty()
                    && id
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
                    && text[..open].ends_with([' ', '\t'])
                {
                    attrs.set_id(id.to_string());
                    text = text[..open].trim_end().to_string();
                }
            }
        }
        self.i += 1;
        Some(Block::Heading {
            level: level as u8,
            text,
            attrs,
        })
    }

    fn try_hr(&mut self) -> Option<Block> {
        let t = self.lines[self.i].trim();
        if is_hr(t) {
            self.i += 1;
            Some(Block::HorizontalRule)
        } else {
            None
        }
    }

    /// GFM fenced code block (kramdown-parser-gfm gfm.rb:160-161):
    ///   `^[ ]{0,3}(([~`]){3,})\s*?(info)?\s*?\n(.*?)^[ ]{0,3}\1\2*\s*?\n`
    /// The fence may be indented at most 3 spaces; the CONTENT between the
    /// fences is taken RAW (no dedenting). A 4+-space "fence" is an indented
    /// code block instead.
    fn try_fenced_code(&mut self) -> Option<Block> {
        let line = self.lines[self.i].clone();
        let indent = leading_spaces(&line);
        if indent > 3 {
            return None;
        }
        let t = &line[indent..];
        let fence_char = if t.starts_with("```") {
            '`'
        } else if t.starts_with("~~~") {
            '~'
        } else {
            return None;
        };
        let mut fence_len = 0;
        for c in t.chars() {
            if c == fence_char {
                fence_len += 1;
            } else {
                break;
            }
        }
        if fence_len < 3 {
            return None;
        }
        let lang = t[fence_len..].trim().to_string();
        // GFM: info string can't contain the fence char for backticks.
        if fence_char == '`' && lang.contains('`') {
            return None;
        }
        // Find the closing fence first — an unterminated fence is not a code
        // block in kramdown (the regex requires the closing line).
        let mut close_idx = None;
        for k in self.i + 1..self.lines.len() {
            let l = &self.lines[k];
            let li = leading_spaces(l);
            if li > 3 {
                continue;
            }
            let s = l.trim();
            if !s.is_empty()
                && s.chars().all(|c| c == fence_char)
                && s.len() >= fence_len
            {
                close_idx = Some(k);
                break;
            }
        }
        let close_idx = close_idx?;
        // Content: RAW lines between the fences (no dedent).
        let mut code = self.lines[self.i + 1..close_idx].join("\n");
        code.push('\n');
        if close_idx == self.i + 1 {
            code = "\n".to_string();
        }
        self.i = close_idx + 1;
        Some(Block::CodeBlock {
            lang: lang.split_whitespace().next().unwrap_or("").to_string(),
            code,
        })
    }

    /// kramdown indented code block (codeblock.rb:19-31; INDENT = 4 spaces or
    /// tab, kramdown.rb:345). The match is
    ///   `(BLANK_LINE? (INDENT [ \t]*\S.*\n)+ (lazy non-blank line)*)*`
    /// followed by two normalization quirks (codeblock.rb:26-27):
    ///   1. `data.gsub!(/\n( {0,3}\S)/, ' \1')` — a LAZY line (0-3 space
    ///      indent) is JOINED to the previous line with a single space;
    ///   2. `data.gsub!(INDENT, '')` — the 4-space/tab indent is stripped
    ///      from every line.
    fn try_indented_code(&mut self) -> Option<Block> {
        let line = self.lines[self.i].clone();
        if !is_indent_line(&line) || line.trim().is_empty() {
            return None;
        }
        let mut raw: Vec<String> = Vec::new();
        loop {
            // optional run of blank lines, only kept if more indented lines follow
            let mut j = self.i;
            let mut blanks = 0;
            while j < self.lines.len() && self.lines[j].trim().is_empty() {
                blanks += 1;
                j += 1;
            }
            if j >= self.lines.len()
                || !is_indent_line(&self.lines[j])
                || self.lines[j].trim().is_empty()
            {
                break;
            }
            for _ in 0..blanks {
                raw.push(String::new());
            }
            self.i = j;
            // 1+ indented non-blank lines
            while self.i < self.lines.len()
                && is_indent_line(&self.lines[self.i])
                && !self.lines[self.i].trim().is_empty()
            {
                raw.push(self.lines[self.i].to_string());
                self.i += 1;
            }
            // 0+ lazy non-blank continuation lines (any indent), stopping at
            // block IALs / extensions / block-HTML tag lines (kramdown's
            // CODEBLOCK_MATCH negative lookaheads, codeblock.rb:20).
            while self.i < self.lines.len() {
                let l = self.lines[self.i].clone();
                if l.trim().is_empty()
                    || parse_block_ial_line(&l).is_some()
                    || is_kramdown_ext_line(&l)
                    || is_lazy_end_html(&l)
                {
                    break;
                }
                if is_indent_line(&l) {
                    // still indented — handled by the inner loop next round
                    raw.push(l.to_string());
                    self.i += 1;
                    continue;
                }
                raw.push(l.to_string());
                self.i += 1;
            }
        }
        if raw.is_empty() {
            return None;
        }
        // Quirk 1: join lazy lines (0-3 space indent, non-blank) to the
        // previous line with a single space.
        let mut joined: Vec<String> = Vec::new();
        for l in raw {
            let li = leading_spaces(&l);
            let lazy = !l.trim().is_empty() && li <= 3 && !l.starts_with('\t');
            if lazy && !joined.is_empty() && !joined.last().unwrap().is_empty() {
                let last = joined.last_mut().unwrap();
                last.push(' ');
                last.push_str(&l[li..]);
            } else {
                joined.push(l);
            }
        }
        // Quirk 2: strip the INDENT (4 spaces or a tab) from every line.
        let mut code = String::new();
        for l in &joined {
            if let Some(stripped) = l.strip_prefix("    ") {
                code.push_str(stripped);
            } else if let Some(stripped) = l.strip_prefix('\t') {
                code.push_str(stripped);
            } else {
                code.push_str(l);
            }
            code.push('\n');
        }
        Some(Block::CodeBlock {
            lang: String::new(),
            code,
        })
    }

    fn try_footnote_def(&mut self) -> Option<Block> {
        let line = self.lines[self.i].clone();
        let t = line.trim_start();
        // [^label]: text
        if !t.starts_with("[^") {
            return None;
        }
        let close = t.find("]:")?;
        let label = t[2..close].to_string();
        let mut first = t[close + 2..].trim_start().to_string();
        self.i += 1;
        // continuation lines: indented (>=4 spaces) or blank-then-indented
        let mut cont: Vec<String> = Vec::new();
        while self.i < self.lines.len() {
            let l = self.lines[self.i].clone();
            if l.trim().is_empty() {
                // peek next
                if self.i + 1 < self.lines.len() && leading_spaces(&self.lines[self.i + 1]) >= 4 {
                    cont.push(String::new());
                    self.i += 1;
                    continue;
                }
                break;
            }
            if leading_spaces(&l) >= 4 {
                cont.push(l[4..].to_string());
                self.i += 1;
            } else {
                break;
            }
        }
        let mut body = first.clone();
        if !cont.is_empty() {
            body.push('\n');
            body.push_str(&cont.join("\n"));
        }
        let _ = &mut first;
        let blocks = parse_block_nodes_with(&body, self.parse_block_html);
        Some(Block::FootnoteDef { label, blocks })
    }

    fn try_table(&mut self) -> Option<Block> {
        // kramdown pipe table (parser/kramdown/table.rb). A table starts at a
        // block boundary on a line containing an unescaped `|`. The separator
        // (`|---|`) is OPTIONAL: without it there is no <thead> and all rows are
        // body rows.
        let line = self.lines[self.i].clone();
        if !table_line_has_pipe(&line) {
            return None;
        }
        // Don't misfire on a standalone block IAL.
        if parse_block_ial_line(&line).is_some() {
            return None;
        }

        let mut header: Option<Vec<String>> = None;
        let mut aligns: Vec<Align> = Vec::new();
        let mut groups: Vec<Vec<Vec<String>>> = Vec::new();
        let mut footer: Option<Vec<Vec<String>>> = None;
        let mut pending: Vec<Vec<String>> = Vec::new();
        let mut has_footer = false;
        let start = self.i;

        while self.i < self.lines.len() {
            let l = self.lines[self.i].clone();
            if l.trim().is_empty() {
                break;
            }
            if parse_block_ial_line(&l).is_some() {
                break;
            }
            if !table_line_has_pipe(&l) {
                break;
            }
            if is_table_sep_line(&l) {
                if pending.is_empty() {
                    // ignore consecutive separators
                    self.i += 1;
                    continue;
                }
                if aligns.is_empty() && header.is_none() && !has_footer {
                    // first separator: preceding rows become the header.
                    header = Some(pending.remove(0));
                    // Any extra pending rows before the separator are unusual;
                    // fold them into the first body group.
                    if !pending.is_empty() {
                        groups.push(std::mem::take(&mut pending));
                    }
                    aligns = parse_sep_aligns(&l);
                } else {
                    groups.push(std::mem::take(&mut pending));
                }
                self.i += 1;
                continue;
            }
            if is_table_footer_line(&l) {
                if !pending.is_empty() {
                    groups.push(std::mem::take(&mut pending));
                }
                has_footer = true;
                self.i += 1;
                continue;
            }
            pending.push(split_table_row(&l));
            self.i += 1;
        }
        // kramdown table.rb:106-109: after the table lines, the parser must be
        // at a BLOCK BOUNDARY (blank line, EOF, or a block IAL). If the table
        // ran into a plain text line (e.g. a lazy continuation without a
        // pipe), the whole table is rejected and re-parsed as a paragraph.
        if self.i < self.lines.len() {
            let next = &self.lines[self.i];
            if !next.trim().is_empty()
                && parse_block_ial_line(next).is_none()
                && !is_kramdown_ext_line(next)
            {
                self.i = start;
                return None;
            }
        }
        // finalize remaining rows
        if !pending.is_empty() {
            if has_footer {
                footer = Some(std::mem::take(&mut pending));
            } else {
                groups.push(std::mem::take(&mut pending));
            }
        }
        // A table must have a body (kramdown ignores a table with no body).
        let total_body: usize = groups.iter().map(|g| g.len()).sum();
        if total_body == 0 && footer.is_none() {
            // not a real table; rewind so the paragraph parser handles it.
            self.i = start;
            return None;
        }
        Some(Block::Table {
            header,
            aligns,
            body: groups,
            footer,
            attrs: Attrs::default(),
        })
    }

    fn try_html_block(&mut self) -> Option<Block> {
        let line = self.lines[self.i].clone();
        // kramdown allows up to 3 leading spaces for an HTML element block
        // (OPT_SPACE in HTML_BLOCK_START, parser/kramdown/html.rb:70).
        if leading_spaces(&line) > 3 {
            return None;
        }
        let t = line.trim_start();
        if !t.starts_with('<') {
            return None;
        }
        // Comment block — must start at COLUMN 0 (an indented `<!--` falls
        // through to the paragraph parser and renders as an inline comment
        // inside a <p>; verified against the oracle).
        if t.starts_with("<!--") {
            if !line.starts_with("<!--") {
                return None;
            }
            let mut collected: Vec<String> = Vec::new();
            while self.i < self.lines.len() {
                let l = self.lines[self.i].clone();
                collected.push(l.to_string());
                if l.contains("-->") {
                    self.i += 1;
                    break;
                }
                self.i += 1;
            }
            return Some(Block::HtmlBlock {
                raw: collected.join("\n"),
            });
        }
        // A line starting with a CLOSING tag `</...>` does not open a block at
        // the top level — an orphan close tag (e.g. left by a stripped Liquid
        // include) is rendered as a paragraph with the tag escaped inline
        // (kramdown behavior). Matched closes are consumed within their open
        // element's block, so they never reach here.
        if t.starts_with("</") {
            return None;
        }
        // Must look like an HTML tag <name ...>
        let tagname = html_tag_name(t)?;
        // Span/inline elements (<br>, <img>, <span>, <a>, …) do NOT start an
        // HTML block — they are handled inline within a paragraph. (kramdown
        // parser/kramdown/html.rb:79 `!HTML_SPAN_ELEMENTS.include?`.)
        if is_span_element(&tagname) {
            return None;
        }
        // Detect the `markdown` attribute on the opening tag (possibly
        // spanning lines) and resolve the effective content model
        // (kramdown parser/kramdown/html.rb:26-43):
        //   model = markdown-attr override
        //           OR (parse_block_html ? default_model(tag) : raw)
        // where markdown="1" selects the tag's DEFAULT model, "0" raw,
        // "span"/"block" explicit.
        let (open_tag, after_open_line, after_open_col) = self.read_open_tag()?;
        let model = if open_tag.contains("markdown=\"0\"") || open_tag.contains("markdown='0'") {
            ContentModel::Raw
        } else if open_tag.contains("markdown=\"span\"") || open_tag.contains("markdown='span'") {
            ContentModel::Span
        } else if open_tag.contains("markdown=\"block\"") || open_tag.contains("markdown='block'")
        {
            ContentModel::Block
        } else if open_tag.contains("markdown=\"1\"") || open_tag.contains("markdown='1'") {
            default_content_model(&tagname)
        } else if self.parse_block_html {
            default_content_model(&tagname)
        } else {
            ContentModel::Raw
        };
        let has_md = model != ContentModel::Raw;

        if is_void_tag(&tagname) {
            // Void element block (e.g. `<link .../>`, `<hr>`, `<img …>`).
            // read_open_tag already advanced past the open tag line, so rewind
            // to the block start and collect from there until a blank line.
            self.i = self.block_start;
            let mut collected: Vec<String> = Vec::new();
            while self.i < self.lines.len() {
                let l = self.lines[self.i].clone();
                if l.trim().is_empty() {
                    break;
                }
                collected.push(l.to_string());
                self.i += 1;
            }
            return Some(Block::HtmlBlock {
                raw: collected.join("\n"),
            });
        }

        let close = format!("</{tagname}>");
        if has_md {
            // Collect inner up to the matching close tag (same tag name, naive
            // depth counting on this tag name).
            let (inner_text, close_tag) =
                self.collect_until_close(&tagname, after_open_line, after_open_col);
            // Elements with SPAN content model (kramdown parser/html.rb:42-44)
            // get span-level parsing of the inner text — newlines verbatim.
            if model == ContentModel::Span {
                return Some(Block::HtmlBlockMdSpan {
                    open_tag,
                    inner_text,
                    close_tag,
                });
            }
            // Block model: inner parsed as blocks, inheriting the current
            // parse_block_html state.
            let inner_doc = parse_doc_with_opts(&inner_text, self.parse_block_html);
            return Some(Block::HtmlBlockMd {
                open_tag,
                inner: inner_doc.nodes,
                inner_trailing_blank: inner_doc.trailing_blank,
                close_tag,
            });
        }

        // Raw HTML block: collect until the matching close tag line, or blank
        // line at depth 0 (kramdown treats block-level raw HTML until the
        // corresponding end tag). We collect until we see the close tag.
        let mut collected: Vec<String> = Vec::new();
        // reset to block start
        // We consumed lines in read_open_tag; reconstruct by re-reading from block start.
        // Simplify: re-scan from the original start index.
        // (read_open_tag advanced self.i to after the open tag line.)
        // For raw passthrough we want the whole element verbatim.
        let start = self.block_start;
        self.i = start;
        let open_pat = format!("<{tagname}");
        let mut depth = 0i32;
        while self.i < self.lines.len() {
            let l = self.lines[self.i].clone();
            // Scan this line's open/close occurrences of `tagname` in order so
            // we can find the exact position where depth returns to 0.
            let mut pos = 0usize;
            let mut end_at: Option<usize> = None;
            loop {
                let next_open = l[pos..].find(&open_pat).map(|p| p + pos);
                let next_close = l[pos..].find(&close).map(|p| p + pos);
                match (next_open, next_close) {
                    (Some(o), Some(c)) if o < c => {
                        depth += 1;
                        pos = o + open_pat.len();
                    }
                    (_, Some(c)) => {
                        depth -= 1;
                        pos = c + close.len();
                        if depth <= 0 {
                            end_at = Some(pos);
                            break;
                        }
                    }
                    (Some(o), None) => {
                        depth += 1;
                        pos = o + open_pat.len();
                    }
                    (None, None) => break,
                }
            }
            if let Some(endp) = end_at {
                // Block ends at the close tag. Content AFTER it on the same
                // line becomes the next line to parse (kramdown continues
                // block parsing right after the end tag — e.g.
                // `</div> <!-- x -->` yields the div block then a paragraph).
                let (head, rest) = l.split_at(endp);
                collected.push(head.to_string());
                let rest = rest.to_string();
                self.i += 1;
                if !rest.trim().is_empty() {
                    self.lines.insert(self.i, rest);
                }
                break;
            }
            collected.push(l.to_string());
            self.i += 1;
            if depth <= 0 && l.trim().is_empty() {
                break;
            }
        }
        Some(Block::HtmlBlock {
            raw: collected.join("\n"),
        })
    }

    fn try_blockquote(&mut self) -> Option<Block> {
        let line = self.lines[self.i].clone();
        if !line.trim_start().starts_with('>') {
            return None;
        }
        let mut inner_lines: Vec<String> = Vec::new();
        while self.i < self.lines.len() {
            let l = self.lines[self.i].clone();
            let ts = l.trim_start();
            if ts.starts_with('>') {
                let stripped = ts[1..].strip_prefix(' ').unwrap_or(&ts[1..]);
                inner_lines.push(stripped.to_string());
                self.i += 1;
            } else if l.trim().is_empty() {
                break;
            } else if parse_block_ial_line(&l).is_some() || is_kramdown_ext_line(&l) {
                // A block IAL / extension on a non-`>` line ends the blockquote
                // so it attaches to the blockquote itself (e.g. `{: .dragon}`).
                break;
            } else {
                // lazy continuation
                inner_lines.push(l.to_string());
                self.i += 1;
            }
        }
        let inner = inner_lines.join("\n");
        let blocks = parse_block_nodes_with(&inner, self.parse_block_html);
        Some(Block::BlockQuote {
            blocks,
            attrs: Attrs::default(),
        })
    }

    fn try_list(&mut self) -> Option<Block> {
        let line = self.lines[self.i].clone();
        let (ordered, start, first_ml) = list_marker(&line)?;
        let mut items: Vec<ListItem> = Vec::new();
        let mut tight = true;
        let base_indent = leading_spaces(&line);
        // A sibling item's marker may be indented deeper than the list start,
        // as long as it sits BEFORE the item content column (kramdown accepts
        // markers within the list indent zone). Track the current content
        // indent to draw that line.
        let mut cur_content_indent = base_indent + first_ml;
        loop {
            if self.i >= self.lines.len() {
                break;
            }
            let l = self.lines[self.i].clone();
            if l.trim().is_empty() {
                // blank line: could be between items (loose) or end of list.
                // Peek ahead.
                let mut k = self.i;
                while k < self.lines.len() && self.lines[k].trim().is_empty() {
                    k += 1;
                }
                if k < self.lines.len()
                    && leading_spaces(&self.lines[k]) >= base_indent
                    && leading_spaces(&self.lines[k]) < cur_content_indent
                    && list_marker(&self.lines[k]).map(|(o, _, _)| o == ordered).unwrap_or(false)
                {
                    // Same-type marker after a blank line: loose separator.
                    tight = false;
                    self.i = k;
                    continue;
                }
                // Otherwise the list ends here. Do NOT consume the blank
                // line(s): the block loop must see them so the following
                // block records blank_before (separator parity).
                let _ = &mut tight;
                break;
            }
            let this_indent = leading_spaces(&l);
            if this_indent < base_indent {
                break;
            }
            if this_indent >= base_indent && this_indent < cur_content_indent {
                if let Some((o2, _s2, ml)) = list_marker(&l) {
                    if o2 != ordered {
                        // different list type -> stop
                        if items.is_empty() {
                            // shouldn't happen
                        }
                        break;
                    }
                    // Start a new item.
                    let content_indent = this_indent + ml;
                    cur_content_indent = content_indent;
                    let mut item_lines: Vec<String> = Vec::new();
                    // first line content after marker
                    item_lines.push(l[content_indent.min(l.len())..].to_string());
                    self.i += 1;
                    // gather continuation lines belonging to this item
                    while self.i < self.lines.len() {
                        let cl = self.lines[self.i].clone();
                        if cl.trim().is_empty() {
                            // could be loose separator; look ahead
                            let mut k = self.i;
                            while k < self.lines.len() && self.lines[k].trim().is_empty() {
                                k += 1;
                            }
                            if k < self.lines.len()
                                && leading_spaces(&self.lines[k]) >= content_indent
                                && list_marker_at_indent(&self.lines[k], base_indent).is_none()
                            {
                                // Blank line WITHIN an item (continuation still
                                // indented under this item). This does NOT make
                                // the list loose — kramdown looseness is decided
                                // by blank lines BETWEEN sibling items. It does
                                // give the item multiple blocks.
                                item_lines.push(String::new());
                                self.i = k;
                                continue;
                            } else {
                                break;
                            }
                        }
                        let ci = leading_spaces(&cl);
                        if ci >= content_indent {
                            item_lines.push(cl[content_indent.min(cl.len())..].to_string());
                            self.i += 1;
                        } else if list_marker(&cl)
                            .map(|(o2, _, _)| o2 == ordered)
                            .unwrap_or(false)
                        {
                            // A SAME-type marker before the content column is a
                            // SIBLING item (kramdown list.rb:71 — list_start_re
                            // is type-specific). A DIFFERENT-type marker falls
                            // through to the lazy-continuation branch and stays
                            // in this item (list.rb:87), later re-parsed as a
                            // nested list.
                            break;
                        } else if parse_block_ial_line(&cl).is_some()
                            || is_kramdown_ext_line(&cl)
                        {
                            // A block IAL / extension line terminates the item so
                            // it can attach to the list itself (e.g. `{:toc}`).
                            break;
                        } else if item_lines
                            .last()
                            .map(|l| !l.trim().is_empty())
                            .unwrap_or(false)
                            && !is_lazy_end_html(&cl)
                        {
                            // Lazy paragraph continuation: an under-indented,
                            // non-marker line directly following item text stays
                            // part of the item's paragraph. kramdown's lazy_re
                            // does NOT strip its indentation (indent_re only
                            // strips a FULL content-indent prefix), so the line
                            // is kept verbatim (verified against oracle). Lazy
                            // continuation stops at block-HTML tag lines
                            // (LAZY_END_HTML_START/STOP).
                            let _ = ci;
                            item_lines.push(cl.to_string());
                            self.i += 1;
                        } else {
                            break;
                        }
                    }
                    let mut item_src = item_lines.join("\n");
                    // LIST_ITEM_IAL (list.rb:19-20): an IAL at the very start of
                    // the item content applies to the <li> itself.
                    let mut item_attrs = Attrs::default();
                    {
                        let t = item_src.trim_start();
                        if t.starts_with("{:") && !t.starts_with("{::") {
                            if let Some(close) = t.find('}') {
                                let candidate = &t[..=close];
                                if let Some(a) = parse_block_ial_line(candidate) {
                                    item_attrs = a;
                                    let skip = item_src.len() - t.len() + close + 1;
                                    item_src = item_src[skip..].trim_start().to_string();
                                }
                            }
                        }
                    }
                    // GFM task-list item: `[ ] ` / `[x] ` at content start.
                    let mut task = None;
                    if let Some(rest) = item_src.strip_prefix("[ ] ") {
                        task = Some(false);
                        item_src = rest.to_string();
                    } else if let Some(rest) = item_src
                        .strip_prefix("[x] ")
                        .or_else(|| item_src.strip_prefix("[X] "))
                    {
                        task = Some(true);
                        item_src = rest.to_string();
                    }
                    let blocks = parse_block_nodes_with(&item_src, self.parse_block_html);
                    // The item is "followed by blank" if the line that ended it
                    // is a blank line (kramdown then appends a trailing :blank to
                    // the item, forcing a loose <p> rendering).
                    let followed_by_blank = self.i < self.lines.len()
                        && self.lines[self.i].trim().is_empty();
                    items.push(ListItem {
                        blocks,
                        tight: true,
                        followed_by_blank,
                        attrs: item_attrs,
                        task,
                    });
                    continue;
                }
            }
            break;
        }
        if items.is_empty() {
            return None;
        }
        Some(Block::List {
            ordered,
            start,
            items,
            tight,
            attrs: Attrs::default(),
            is_toc: false,
        })
    }

    fn try_setext_or_paragraph(&mut self) -> Option<Block> {
        let mut para_lines: Vec<String> = Vec::new();
        let start_idx = self.i;
        while self.i < self.lines.len() {
            let l = self.lines[self.i].clone();
            let t = l.trim();
            if t.is_empty() {
                break;
            }
            if parse_block_ial_line(&l).is_some() {
                break;
            }
            // setext underline?
            if !para_lines.is_empty() {
                if is_setext_underline(t) {
                    let level = if t.starts_with('=') { 1 } else { 2 };
                    let text = para_lines.join("\n");
                    self.i += 1;
                    return Some(Block::Heading {
                        level,
                        text: text.trim().to_string(),
                        attrs: Attrs::default(),
                    });
                }
            }
            // interruptors: a new block start ends the paragraph.
            if !para_lines.is_empty() && self.paragraph_interrupted(&l) {
                break;
            }
            para_lines.push(l.to_string());
            self.i += 1;
        }
        if para_lines.is_empty() {
            // avoid infinite loop
            if self.i == start_idx {
                self.i += 1;
            }
            return None;
        }
        Some(Block::Paragraph {
            text: para_lines.join("\n").trim().to_string(),
            attrs: Attrs::default(),
        })
    }

    fn paragraph_interrupted(&self, l: &str) -> bool {
        let t = l.trim_start();
        if l.starts_with('#') {
            // atx heading (column 0 only, per kramdown/GFM)
            let hashes = l.chars().take_while(|&c| c == '#').count();
            let rest = &l[hashes..];
            if hashes >= 1 && hashes <= 6 && (rest.is_empty() || rest.starts_with(' ')) {
                return true;
            }
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            return true;
        }
        if t.starts_with('>') {
            return true;
        }
        if is_hr(l.trim()) {
            return true;
        }
        if t.starts_with('<') {
            if let Some(name) = html_tag_name(t) {
                // Only block-level HTML interrupts a paragraph; inline/span tags
                // (<br>, <img>, <span>, <a>, …) flow with the paragraph.
                if !is_span_element(&name) {
                    return true;
                }
            }
        }
        // GFM: a list can interrupt a paragraph. A bullet (`-`/`*`/`+`) always
        // may; an ordered list may only if it starts at 1 and (GFM) the item is
        // non-empty. kramdown+GFM here follows suit for our corpus.
        if let Some((ordered, start, _)) = list_marker(l) {
            // The content after the marker must be non-empty to interrupt.
            let content = list_item_content_nonempty(l);
            if !content {
                return false;
            }
            if !ordered {
                return true;
            }
            if start == Some(1) {
                return true;
            }
        }
        // A blockquote / table delimiter also interrupts; tables handled by the
        // next-line delimiter check in try_table, so a header row alone won't
        // interrupt (kramdown needs the delimiter line).
        false
    }

    // --- helpers for HTML block scanning ---
    fn read_open_tag(&mut self) -> Option<(String, usize, usize)> {
        // record start
        self.block_start = self.i;
        let start_line = self.lines[self.i].clone();
        let col = leading_spaces(&start_line);
        let mut acc = String::new();
        let mut line_idx = self.i;
        let mut ci = col;
        loop {
            let l = self.lines[line_idx].clone();
            let slice = &l[ci..];
            if let Some(gt) = slice.find('>') {
                acc.push_str(&slice[..=gt]);
                let after_col = ci + gt + 1;
                self.i = line_idx + 1;
                return Some((acc, line_idx, after_col));
            } else {
                acc.push_str(slice);
                acc.push('\n');
                line_idx += 1;
                ci = 0;
                if line_idx >= self.lines.len() {
                    return None;
                }
            }
        }
    }

    fn collect_until_close(
        &mut self,
        tagname: &str,
        after_line: usize,
        after_col: usize,
    ) -> (String, String) {
        let open_pat = format!("<{tagname}");
        let close_pat = format!("</{tagname}>");
        let mut depth = 1i32;
        let mut inner: Vec<String> = Vec::new();
        let mut line_idx = after_line;
        let mut col = after_col;
        loop {
            if line_idx >= self.lines.len() {
                self.i = line_idx;
                return (inner.join("\n"), close_pat);
            }
            let l = self.lines[line_idx].clone();
            let slice = &l[col.min(l.len())..];
            // Look for close tag on this slice.
            if let Some(pos) = slice.find(&close_pat) {
                // account for nested opens before pos
                let before = &slice[..pos];
                depth += count_occurrences(before, &open_pat) as i32;
                depth -= 1; // this close
                if depth <= 0 {
                    inner.push(before.to_string());
                    self.i = line_idx + 1;
                    // Content AFTER the close tag on the same line becomes the
                    // next line to parse (kramdown continues block parsing
                    // right after the end tag).
                    let rest = &slice[pos + close_pat.len()..];
                    if !rest.trim().is_empty() {
                        let rest = rest.to_string();
                        self.lines.insert(self.i, rest);
                    }
                    return (inner.join("\n"), close_pat);
                } else {
                    inner.push(slice.to_string());
                }
            } else {
                depth += count_occurrences(slice, &open_pat) as i32;
                inner.push(slice.to_string());
            }
            line_idx += 1;
            col = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// free helpers

fn attach_ial_nodes(nodes: &mut [BlockNode], attrs: Attrs) {
    if let Some(last) = nodes.last_mut() {
        set_block_attrs(&mut last.block, attrs);
    }
}

fn set_block_attrs(b: &mut Block, mut a: Attrs) {
    match b {
        Block::Heading { attrs, .. }
        | Block::Paragraph { attrs, .. }
        | Block::Table { attrs, .. }
        | Block::BlockQuote { attrs, .. } => merge_attrs(attrs, &mut a),
        Block::List { attrs, is_toc, .. } => {
            if a.has_ref("toc") {
                *is_toc = true;
            }
            merge_attrs(attrs, &mut a);
        }
        _ => {}
    }
}

fn merge_attrs(dst: &mut Attrs, src: &mut Attrs) {
    // Append the source IAL's ordered attributes, merging classes and keeping
    // kramdown's insertion order.
    for (k, v) in src.ordered.drain(..) {
        if k == "class" {
            if let Some(slot) = dst.ordered.iter_mut().find(|(kk, _)| kk == "class") {
                slot.1.push(' ');
                slot.1.push_str(&v);
            } else {
                dst.ordered.push(("class".to_string(), v));
            }
        } else if let Some(slot) = dst.ordered.iter_mut().find(|(kk, _)| kk == &k) {
            slot.1 = v;
        } else {
            dst.ordered.push((k, v));
        }
    }
    dst.refs.append(&mut src.refs);
}

fn strip_trailing_hashes(s: &str) -> String {
    let t = s.trim_end();
    let without = t.trim_end_matches('#');
    if without.len() < t.len() && (without.ends_with(' ') || without.is_empty()) {
        without.trim_end().to_string()
    } else {
        t.to_string()
    }
}

/// kramdown INDENT (kramdown.rb:345): `/^(?:\t| {4})/` — a tab or 4 spaces.
fn is_indent_line(l: &str) -> bool {
    l.starts_with("    ") || l.starts_with('\t')
}

fn is_hr(t: &str) -> bool {
    let compact: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() < 3 {
        return false;
    }
    (compact.chars().all(|c| c == '*') || compact.chars().all(|c| c == '-') || compact.chars().all(|c| c == '_'))
        && compact.len() >= 3
}

fn is_setext_underline(t: &str) -> bool {
    (!t.is_empty() && t.chars().all(|c| c == '=')) || (!t.is_empty() && t.chars().all(|c| c == '-') && t.len() >= 1)
}

fn leading_spaces(l: &str) -> usize {
    let mut n = 0;
    for c in l.chars() {
        if c == ' ' {
            n += 1;
        } else if c == '\t' {
            n += 4;
        } else {
            break;
        }
    }
    // return byte count of leading whitespace (spaces only) for slicing safety
    let mut bytes = 0;
    for c in l.chars() {
        if c == ' ' {
            bytes += 1;
        } else if c == '\t' {
            bytes += 1;
        } else {
            break;
        }
    }
    let _ = n;
    bytes
}

fn strip_up_to(l: &str, n: usize) -> &str {
    let mut removed = 0;
    let mut idx = 0;
    for (bi, c) in l.char_indices() {
        if removed >= n {
            idx = bi;
            return &l[idx..];
        }
        if c == ' ' || c == '\t' {
            removed += 1;
            idx = bi + c.len_utf8();
        } else {
            return &l[bi..];
        }
    }
    &l[idx..]
}

fn count_occurrences(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = hay[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

fn html_tag_name(t: &str) -> Option<String> {
    let bytes = t.as_bytes();
    if bytes.first() != Some(&b'<') {
        return None;
    }
    let mut i = 1;
    if bytes.get(i) == Some(&b'/') {
        i += 1;
    }
    let start = i;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphanumeric() || c == b'-' {
            i += 1;
        } else {
            break;
        }
    }
    if i == start {
        return None;
    }
    Some(t[start..i].to_lowercase())
}

fn is_void_tag(name: &str) -> bool {
    matches!(
        name,
        "br" | "hr" | "img" | "input" | "meta" | "link" | "area" | "base" | "col" | "embed"
            | "param" | "source" | "track" | "wbr"
    )
}

fn is_void_like(_l: &str, _tag: &str) -> bool {
    false
}

fn list_marker(l: &str) -> Option<(bool, Option<u64>, usize)> {
    let indent = leading_spaces(l);
    let rest = &l[indent..];
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    // bullet — third tuple value is the marker WIDTH relative to the content
    // after the line's leading indent: marker char + the WHOLE run of spaces
    // that follows (kramdown computes the item's content indent from the full
    // marker match, so `-   x` has content indent 4).
    if (bytes[0] == b'-' || bytes[0] == b'*' || bytes[0] == b'+')
        && bytes.get(1).map(|&c| c == b' ' || c == b'\t').unwrap_or(rest.len() == 1)
    {
        let mut w = 1;
        while bytes.get(w).map(|&c| c == b' ' || c == b'\t').unwrap_or(false) {
            w += 1;
        }
        if w == 1 {
            w = 2; // marker at end of line: nominal single-space width
        }
        return Some((false, None, w));
    }
    // ordered: digits then '.' — kramdown LIST_START_OL is `\d+\.` ONLY
    // (list.rb:49); `1)` is NOT a list marker in kramdown.
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i <= 9 && bytes.get(i) == Some(&b'.') {
        let has_space = bytes.get(i + 1).map(|&c| c == b' ' || c == b'\t').unwrap_or(rest.len() == i + 1);
        if has_space {
            let num: u64 = rest[..i].parse().unwrap_or(1);
            // width = digits + delimiter + the whole run of spaces
            let mut w = i + 1;
            while bytes.get(w).map(|&c| c == b' ' || c == b'\t').unwrap_or(false) {
                w += 1;
            }
            if w == i + 1 {
                w = i + 2;
            }
            return Some((true, Some(num), w));
        }
    }
    None
}

/// True if the list item on `l` has non-empty content after its marker.
fn list_item_content_nonempty(l: &str) -> bool {
    if let Some((_, _, ml)) = list_marker(l) {
        let indent = leading_spaces(l);
        let start = indent + ml;
        l.len() > start && !l[start.min(l.len())..].trim().is_empty()
    } else {
        false
    }
}

fn list_marker_at_indent(l: &str, indent: usize) -> Option<(bool, Option<u64>, usize)> {
    if leading_spaces(l) == indent {
        list_marker(l)
    } else {
        None
    }
}

/// kramdown TABLE_PIPE_CHECK: a line with a leading `|` OR an unescaped `|`
/// somewhere. A `|` that occurs only inside a backtick code span does NOT
/// count (kramdown parses code spans before splitting table cells), so a line
/// whose only pipes are inside `` `...` `` is a paragraph, not a table.
fn table_line_has_pipe(l: &str) -> bool {
    let t = l.trim_start();
    let chars: Vec<char> = t.chars().collect();
    let n = chars.len();
    // A leading pipe (outside code) always qualifies.
    if chars.first() == Some(&'|') {
        return true;
    }
    let mut i = 0;
    let mut prev = '\0';
    while i < n {
        let c = chars[i];
        if c == '`' {
            // skip a code span: run of `fence` backticks to matching run.
            let mut fence = 0;
            while i < n && chars[i] == '`' {
                fence += 1;
                i += 1;
            }
            // find closing run of exactly fence
            let mut closed = false;
            while i < n {
                if chars[i] == '`' {
                    let mut run = 0;
                    while i < n && chars[i] == '`' {
                        run += 1;
                        i += 1;
                    }
                    if run == fence {
                        closed = true;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            if !closed {
                // unterminated code span — treat rest as literal text
            }
            prev = '`';
            continue;
        }
        if c == '|' && prev != '\\' {
            return true;
        }
        prev = c;
        i += 1;
    }
    false
}

/// kramdown TABLE_SEP_LINE: `^([+|: \t-]*?-[+|: \t-]*?)[ \t]*\n` — a line made
/// only of `+ | : space tab -` and containing at least one `-`.
fn is_table_sep_line(l: &str) -> bool {
    let t = l.trim_end();
    if t.trim().is_empty() {
        return false;
    }
    let mut has_dash = false;
    for c in t.chars() {
        match c {
            '-' => has_dash = true,
            '+' | '|' | ':' | ' ' | '\t' => {}
            _ => return false,
        }
    }
    has_dash
}

/// kramdown TABLE_FSEP_LINE: only `+ | : space tab =` and at least one `=`.
fn is_table_footer_line(l: &str) -> bool {
    let t = l.trim_end();
    if t.trim().is_empty() {
        return false;
    }
    let mut has_eq = false;
    for c in t.chars() {
        match c {
            '=' => has_eq = true,
            '+' | '|' | ':' | ' ' | '\t' => {}
            _ => return false,
        }
    }
    has_eq
}

/// Parse alignment from a separator line, per kramdown TABLE_HSEP_ALIGN:
/// `:---` = left, `---:` = right, `:--:` = center, `---` = default.
fn parse_sep_aligns(l: &str) -> Vec<Align> {
    let cells = split_table_row(l);
    cells
        .iter()
        .filter(|c| !c.trim().is_empty())
        .map(|cell| {
            let c = cell.trim();
            let left = c.starts_with(':');
            let right = c.ends_with(':');
            match (left, right) {
                (true, true) => Align::Center,
                (true, false) => Align::Left,
                (false, true) => Align::Right,
                (false, false) => Align::None,
            }
        })
        .collect()
}

/// Split a table row into cells. A `|` inside a backtick code span is NOT a
/// cell separator (kramdown table.rb:69 splits around `<code>` elements).
fn split_table_row(l: &str) -> Vec<String> {
    let mut t = l.trim();
    if t.starts_with('|') {
        t = &t[1..];
    }
    if t.ends_with('|') && !t.ends_with("\\|") {
        t = &t[..t.len() - 1];
    }
    let chars: Vec<char> = t.chars().collect();
    let n = chars.len();
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c == '\\' && i + 1 < n && chars[i + 1] == '|' {
            cur.push('|');
            i += 2;
            continue;
        }
        if c == '`' {
            // opaque code span: copy through the matching backtick run
            let mut fence = 0;
            let start = i;
            while i < n && chars[i] == '`' {
                fence += 1;
                i += 1;
            }
            let mut closed = false;
            while i < n {
                if chars[i] == '`' {
                    let mut run = 0;
                    while i < n && chars[i] == '`' {
                        run += 1;
                        i += 1;
                    }
                    if run == fence {
                        closed = true;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            if closed {
                for &ch in &chars[start..i] {
                    cur.push(ch);
                }
            } else {
                // unmatched backticks: treat literally, reprocess after run
                for &ch in &chars[start..start + fence] {
                    cur.push(ch);
                }
                i = start + fence;
            }
            continue;
        }
        if c == '|' {
            cells.push(cur.trim().to_string());
            cur = String::new();
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    cells.push(cur.trim().to_string());
    cells
}
