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
    let (src_owned, link_refs) = extract_link_refs(src);
    let src = src_owned.as_str();
    let lines: Vec<&str> = src.split('\n').collect();
    let mut p = Parser {
        lines: &lines,
        i: 0,
        block_start: 0,
    };
    // Leading blank: any blank line(s) — or consumed kramdown extension lines —
    // before the first real block.
    let mut leading_blank = false;
    {
        let mut k = 0;
        while k < lines.len()
            && (lines[k].trim().is_empty() || is_kramdown_ext_line(lines[k]))
        {
            leading_blank = true;
            k += 1;
        }
        // Only count as leading blank if there IS a first block.
        if k >= lines.len() {
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

fn parse_block_nodes(src: &str) -> Vec<BlockNode> {
    parse_doc(src).nodes
}

struct Parser<'a> {
    lines: &'a [&'a str],
    i: usize,
    /// Set by read_open_tag to allow raw-HTML re-scan from the element start.
    block_start: usize,
}

impl<'a> Parser<'a> {
    fn parse_until(&mut self, stop: impl Fn(&str) -> bool) -> Vec<BlockNode> {
        let mut nodes: Vec<BlockNode> = Vec::new();
        let mut pending_blank = false;
        macro_rules! push {
            ($b:expr) => {{
                nodes.push(BlockNode {
                    block: $b,
                    blank_before: pending_blank,
                });
                pending_blank = false;
            }};
        }
        while self.i < self.lines.len() {
            let line = self.lines[self.i];
            if stop(line) {
                break;
            }
            // kramdown block extension `{::...}` — consume, no output, treated
            // as a block boundary (like a blank line).
            if is_kramdown_ext_line(line) {
                pending_blank = true;
                self.i += 1;
                continue;
            }
            // Standalone block IAL attaches to previous block.
            if let Some(attrs) = parse_block_ial_line(line) {
                self.i += 1;
                attach_ial_nodes(&mut nodes, attrs);
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                pending_blank = true;
                self.i += 1;
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
            if let Some(b) = self.try_fenced_code() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_footnote_def() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_table() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_html_block() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_blockquote() {
                push!(b);
                continue;
            }
            if let Some(b) = self.try_list() {
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
        let line = self.lines[self.i];
        let t = line.trim_start();
        if !t.starts_with('#') {
            return None;
        }
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
        self.i += 1;
        Some(Block::Heading {
            level: level as u8,
            text,
            attrs: Attrs::default(),
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

    fn try_fenced_code(&mut self) -> Option<Block> {
        let line = self.lines[self.i];
        let indent = leading_spaces(line);
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
        self.i += 1;
        let mut code_lines: Vec<String> = Vec::new();
        while self.i < self.lines.len() {
            let l = self.lines[self.i];
            let lt = &l[leading_spaces(l).min(indent)..];
            let stripped = lt.trim_end_matches(|_c| false);
            let s = stripped.trim_start();
            if s.chars().take_while(|&c| c == fence_char).count() >= fence_len
                && s.chars().all(|c| c == fence_char)
                && !s.is_empty()
            {
                self.i += 1;
                break;
            }
            // strip up to `indent` leading spaces
            let line_content = strip_up_to(l, indent);
            code_lines.push(line_content.to_string());
            self.i += 1;
        }
        let mut code = code_lines.join("\n");
        if !code.is_empty() {
            code.push('\n');
        } else {
            code.push('\n');
        }
        Some(Block::CodeBlock {
            lang: lang.split_whitespace().next().unwrap_or("").to_string(),
            code,
        })
    }

    fn try_footnote_def(&mut self) -> Option<Block> {
        let line = self.lines[self.i];
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
            let l = self.lines[self.i];
            if l.trim().is_empty() {
                // peek next
                if self.i + 1 < self.lines.len() && leading_spaces(self.lines[self.i + 1]) >= 4 {
                    cont.push(String::new());
                    self.i += 1;
                    continue;
                }
                break;
            }
            if leading_spaces(l) >= 4 {
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
        let blocks = parse_block_nodes(&body);
        Some(Block::FootnoteDef { label, blocks })
    }

    fn try_table(&mut self) -> Option<Block> {
        // kramdown pipe table (parser/kramdown/table.rb). A table starts at a
        // block boundary on a line containing an unescaped `|`. The separator
        // (`|---|`) is OPTIONAL: without it there is no <thead> and all rows are
        // body rows.
        let line = self.lines[self.i];
        if !table_line_has_pipe(line) {
            return None;
        }
        // Don't misfire on a standalone block IAL.
        if parse_block_ial_line(line).is_some() {
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
            let l = self.lines[self.i];
            if l.trim().is_empty() {
                break;
            }
            if parse_block_ial_line(l).is_some() {
                break;
            }
            if !table_line_has_pipe(l) {
                break;
            }
            if is_table_sep_line(l) {
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
                    aligns = parse_sep_aligns(l);
                } else {
                    groups.push(std::mem::take(&mut pending));
                }
                self.i += 1;
                continue;
            }
            if is_table_footer_line(l) {
                if !pending.is_empty() {
                    groups.push(std::mem::take(&mut pending));
                }
                has_footer = true;
                self.i += 1;
                continue;
            }
            pending.push(split_table_row(l));
            self.i += 1;
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
        let line = self.lines[self.i];
        let t = line.trim_start();
        if !t.starts_with('<') {
            return None;
        }
        // Comment block
        if t.starts_with("<!--") {
            let mut collected: Vec<String> = Vec::new();
            while self.i < self.lines.len() {
                let l = self.lines[self.i];
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
        // Must look like an HTML tag <name ...>
        let tagname = html_tag_name(t)?;
        // Span/inline elements (<br>, <img>, <span>, <a>, …) do NOT start an
        // HTML block — they are handled inline within a paragraph. (kramdown
        // parser/kramdown/html.rb:79 `!HTML_SPAN_ELEMENTS.include?`.)
        if is_span_element(&tagname) {
            return None;
        }
        // Detect markdown="1" on the opening tag (possibly spanning the block).
        // First, gather the full opening tag (could span lines).
        let (open_tag, after_open_line, after_open_col) = self.read_open_tag()?;
        let has_md = open_tag.contains("markdown=\"1\"") || open_tag.contains("markdown='1'");

        if is_void_tag(&tagname) {
            // Void element block (e.g. `<link .../>`, `<hr>`, `<img …>`).
            // read_open_tag already advanced past the open tag line, so rewind
            // to the block start and collect from there until a blank line.
            self.i = self.block_start;
            let mut collected: Vec<String> = Vec::new();
            while self.i < self.lines.len() {
                let l = self.lines[self.i];
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
            let inner_doc = parse_doc(&inner_text);
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
        let mut depth = 0i32;
        while self.i < self.lines.len() {
            let l = self.lines[self.i];
            let opens = count_occurrences(l, &format!("<{tagname}"));
            let closes = count_occurrences(l, &close);
            collected.push(l.to_string());
            depth += opens as i32 - closes as i32;
            self.i += 1;
            if depth <= 0 && (l.contains(&close) || is_void_like(l, &tagname)) {
                break;
            }
            if depth <= 0 && l.trim().is_empty() {
                break;
            }
        }
        Some(Block::HtmlBlock {
            raw: collected.join("\n"),
        })
    }

    fn try_blockquote(&mut self) -> Option<Block> {
        let line = self.lines[self.i];
        if !line.trim_start().starts_with('>') {
            return None;
        }
        let mut inner_lines: Vec<String> = Vec::new();
        while self.i < self.lines.len() {
            let l = self.lines[self.i];
            let ts = l.trim_start();
            if ts.starts_with('>') {
                let stripped = ts[1..].strip_prefix(' ').unwrap_or(&ts[1..]);
                inner_lines.push(stripped.to_string());
                self.i += 1;
            } else if l.trim().is_empty() {
                break;
            } else {
                // lazy continuation
                inner_lines.push(l.to_string());
                self.i += 1;
            }
        }
        let inner = inner_lines.join("\n");
        let blocks = parse_block_nodes(&inner);
        Some(Block::BlockQuote {
            blocks,
            attrs: Attrs::default(),
        })
    }

    fn try_list(&mut self) -> Option<Block> {
        let line = self.lines[self.i];
        let (ordered, start, _marker_len) = list_marker(line)?;
        let mut items: Vec<ListItem> = Vec::new();
        let mut tight = true;
        let base_indent = leading_spaces(line);
        loop {
            if self.i >= self.lines.len() {
                break;
            }
            let l = self.lines[self.i];
            if l.trim().is_empty() {
                // blank line: could be between items (loose) or end of list.
                // Peek ahead.
                let mut k = self.i;
                while k < self.lines.len() && self.lines[k].trim().is_empty() {
                    k += 1;
                }
                if k < self.lines.len()
                    && leading_spaces(self.lines[k]) == base_indent
                    && list_marker(self.lines[k]).map(|(o, _, _)| o == ordered).unwrap_or(false)
                {
                    // Same-type marker after a blank line: loose separator.
                    tight = false;
                    self.i = k;
                    continue;
                }
                if k < self.lines.len() && leading_spaces(self.lines[k]) > base_indent {
                    // continuation of current item after blank -> loose
                    tight = false;
                    self.i = k;
                    // fallthrough to collect continuation into last item
                }
                break;
            }
            let this_indent = leading_spaces(l);
            if this_indent < base_indent {
                break;
            }
            if this_indent == base_indent {
                if let Some((o2, _s2, ml)) = list_marker(l) {
                    if o2 != ordered {
                        // different list type -> stop
                        if items.is_empty() {
                            // shouldn't happen
                        }
                        break;
                    }
                    // Start a new item.
                    let content_indent = base_indent + ml;
                    let mut item_lines: Vec<String> = Vec::new();
                    // first line content after marker
                    item_lines.push(l[content_indent.min(l.len())..].to_string());
                    self.i += 1;
                    // gather continuation lines belonging to this item
                    while self.i < self.lines.len() {
                        let cl = self.lines[self.i];
                        if cl.trim().is_empty() {
                            // could be loose separator; look ahead
                            let mut k = self.i;
                            while k < self.lines.len() && self.lines[k].trim().is_empty() {
                                k += 1;
                            }
                            if k < self.lines.len()
                                && leading_spaces(self.lines[k]) >= content_indent
                                && list_marker_at_indent(self.lines[k], base_indent).is_none()
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
                        let ci = leading_spaces(cl);
                        if ci >= content_indent {
                            item_lines.push(cl[content_indent.min(cl.len())..].to_string());
                            self.i += 1;
                        } else if list_marker(cl).is_some() && ci <= base_indent {
                            break;
                        } else if ci > base_indent {
                            // nested or lazy: keep relative indent
                            item_lines.push(cl[content_indent.min(cl.len())..].to_string());
                            self.i += 1;
                        } else if parse_block_ial_line(cl).is_some()
                            || is_kramdown_ext_line(cl)
                        {
                            // A block IAL / extension line terminates the item so
                            // it can attach to the list itself (e.g. `{:toc}`).
                            break;
                        } else if item_lines
                            .last()
                            .map(|l| !l.trim().is_empty())
                            .unwrap_or(false)
                        {
                            // Lazy paragraph continuation: an under-indented,
                            // non-marker line directly following item text stays
                            // part of the item's paragraph (kramdown/CommonMark
                            // lazy continuation). Preserve its leading indent so
                            // inline soft-break spacing matches.
                            item_lines.push(cl[content_indent.min(ci)..].to_string());
                            self.i += 1;
                        } else {
                            break;
                        }
                    }
                    let item_src = item_lines.join("\n");
                    let blocks = parse_block_nodes(&item_src);
                    // The item is "followed by blank" if the line that ended it
                    // is a blank line (kramdown then appends a trailing :blank to
                    // the item, forcing a loose <p> rendering).
                    let followed_by_blank = self.i < self.lines.len()
                        && self.lines[self.i].trim().is_empty();
                    items.push(ListItem {
                        blocks,
                        tight: true,
                        followed_by_blank,
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
            let l = self.lines[self.i];
            let t = l.trim();
            if t.is_empty() {
                break;
            }
            if parse_block_ial_line(l).is_some() {
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
            if !para_lines.is_empty() && self.paragraph_interrupted(l) {
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
        if t.starts_with('#') {
            // atx heading
            let hashes = t.chars().take_while(|&c| c == '#').count();
            let rest = &t[hashes..];
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
        let start_line = self.lines[self.i];
        let col = leading_spaces(start_line);
        let mut acc = String::new();
        let mut line_idx = self.i;
        let mut ci = col;
        loop {
            let l = self.lines[line_idx];
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
            let l = self.lines[line_idx];
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
                    // Note: content after close tag on same line is dropped
                    // (rare in corpus; blockquote close is on its own line).
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
    // after the line's leading indent (marker char + one space = 2).
    if (bytes[0] == b'-' || bytes[0] == b'*' || bytes[0] == b'+')
        && bytes.get(1).map(|&c| c == b' ' || c == b'\t').unwrap_or(rest.len() == 1)
    {
        return Some((false, None, 2));
    }
    // ordered: digits then '.' or ')'
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i <= 9 && (bytes.get(i) == Some(&b'.') || bytes.get(i) == Some(&b')')) {
        let has_space = bytes.get(i + 1).map(|&c| c == b' ' || c == b'\t').unwrap_or(rest.len() == i + 1);
        if has_space {
            let num: u64 = rest[..i].parse().unwrap_or(1);
            // width = digits + delimiter + space
            return Some((true, Some(num), i + 2));
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

fn split_table_row(l: &str) -> Vec<String> {
    let mut t = l.trim();
    if t.starts_with('|') {
        t = &t[1..];
    }
    if t.ends_with('|') && !t.ends_with("\\|") {
        t = &t[..t.len() - 1];
    }
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if next == '|' {
                    cur.push('|');
                    chars.next();
                    continue;
                }
            }
            cur.push('\\');
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur = String::new();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}
