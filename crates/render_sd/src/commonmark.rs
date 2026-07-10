//! A commonmark-java-subset renderer that reproduces, byte-for-byte over the SD
//! description corpus, the HTML string that fhir-core's `Cell.addMarkdown` gets
//! from `HtmlRenderer.builder().escapeHtml(true).build().render(parser.parse(md))`
//! (HierarchicalTableGenerator.java:343-346, plain `Parser.builder().build()` —
//! NO TablesExtension, NO preProcess).
//!
//! This is the ONLY new engine component; the resulting HTML string is fed back
//! through `render_xhtml`'s parser + a port of `htmlToParagraphPieces`
//! (see `markdown.rs`), so exact `\n` placement in this output is load-bearing
//! (it becomes whitespace text nodes that drive the `<br><br>` inter-block
//! inserts and appear verbatim inside `<ul>`/`<ol>` Piece children).
//!
//! ## Scope (measured across the full us-core+plan-net+cycle corpus)
//! Block: paragraphs (blank-line separated), tight bullet lists (`-`/`*`/`+`),
//! tight ordered lists (`1.`). Inline: `[text](url)` links, `` `code` `` spans,
//! `**strong**`/`*em*`/`_em_`/`__strong__` with CommonMark flanking rules, and
//! `escapeHtml(true)` of `& < > "` in text. No headings, blockquotes, thematic
//! breaks, fenced code, HTML blocks, tables, hard-break handling, or reference
//! links occur in the corpus; anything the block splitter does not recognize as
//! a list falls into a paragraph and is rendered as inline text (which is what
//! CommonMark does for a bare line anyway).
//!
//! commonmark-java `HtmlRenderer` output shape (the parts we reproduce):
//!   - paragraph:  `<p>` + inline + `</p>\n`
//!   - bullet list: `<ul>\n` + items + `</ul>\n`; ordered: `<ol>\n` ... `</ol>\n`
//!     (with `start` attr when != 1)
//!   - tight list item: `<li>` + inline + `</li>\n`
//! Verified against golden bytes (us-core-ethnicity Extension definition ->
//! `<p>...</p>\n<ul>\n<li>Hispanic or Latino</li>\n<li>Not Hispanic or
//! Latino.</li>\n</ul>\n`).

/// Render `md` to the HTML string commonmark-java's escapeHtml(true) HtmlRenderer
/// would produce for it (over the supported subset).
pub fn render_html(md: &str) -> String {
    let blocks = parse_blocks(md);
    let mut out = String::new();
    for b in &blocks {
        render_block(b, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// Block layer
// ---------------------------------------------------------------------------

enum Block {
    Paragraph(String),
    List {
        ordered: bool,
        start: u64,
        items: Vec<Vec<Block>>,
    },
}

/// Split `md` into blocks. CommonMark line-oriented parsing, restricted to the
/// corpus subset: paragraphs and (possibly paragraph-interrupting) tight lists.
fn parse_blocks(md: &str) -> Vec<Block> {
    // Normalize line endings the way commonmark-java does (CRLF/CR -> LF).
    let normalized = md.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut blocks = Vec::new();
    let mut i = 0usize;
    let mut para: Vec<&str> = Vec::new();

    let flush_para = |para: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        // Trim leading/trailing blank lines.
        while para.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
            para.remove(0);
        }
        while para.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
            para.pop();
        }
        if para.is_empty() {
            return;
        }
        // Paragraph inline content: each line's leading whitespace is stripped; a
        // line ending in >= 2 spaces (or a backslash) before the next line is a
        // CommonMark HARD line break, encoded here as `\u{0}` so the inline layer
        // can emit `<br />`; otherwise lines join with a `\n` softbreak. Trailing
        // spaces are then removed from each line. (CommonMark 6.7/6.8.)
        let n = para.len();
        let mut joined = String::new();
        for (k, raw) in para.iter().enumerate() {
            let l = raw.trim_start();
            let last = k + 1 == n;
            let trailing_spaces = l.len() - l.trim_end_matches(' ').len();
            let content = l.trim_end();
            joined.push_str(content);
            if !last {
                if trailing_spaces >= 2 {
                    joined.push('\u{0}'); // hard break
                } else {
                    joined.push('\n'); // soft break
                }
            }
        }
        blocks.push(Block::Paragraph(joined));
        para.clear();
    };

    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            flush_para(&mut para, &mut blocks);
            i += 1;
            continue;
        }
        if let Some((ordered, start)) = list_marker(line) {
            // A list starts here (may interrupt a paragraph). Collect the whole
            // list.
            flush_para(&mut para, &mut blocks);
            let (list, next) = parse_list(&lines, i, ordered, start);
            blocks.push(list);
            i = next;
            continue;
        }
        para.push(line);
        i += 1;
    }
    flush_para(&mut para, &mut blocks);
    blocks
}

/// If `line` begins a list item (after up to 3 leading spaces), return
/// (ordered, start-number). CommonMark bullet markers: `-`, `+`, `*` followed by
/// a space/tab; ordered: 1-9 digits then `.` or `)` then a space/tab.
fn list_marker(line: &str) -> Option<(bool, u64)> {
    let indent = leading_spaces(line);
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let c = bytes[0];
    if c == b'-' || c == b'+' || c == b'*' {
        // Must be followed by a space/tab or be the whole line.
        if rest.len() == 1 || matches!(bytes.get(1), Some(b' ') | Some(b'\t')) {
            return Some((false, 1));
        }
        return None;
    }
    if c.is_ascii_digit() {
        let mut j = 0usize;
        while j < bytes.len() && bytes[j].is_ascii_digit() && j < 9 {
            j += 1;
        }
        if j < bytes.len() && (bytes[j] == b'.' || bytes[j] == b')') {
            if j + 1 == bytes.len() || matches!(bytes.get(j + 1), Some(b' ') | Some(b'\t')) {
                let num: u64 = rest[..j].parse().unwrap_or(1);
                return Some((true, num));
            }
        }
    }
    None
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|&b| b == b' ').count()
}

/// Parse a (tight) list beginning at `lines[start]`. Returns the Block and the
/// index of the first line after the list. Restricted to the corpus subset:
/// single-line or lazy-continued items, tight (no blank lines between items),
/// no nested lists (none occur in the corpus).
fn parse_list(lines: &[&str], start: usize, ordered: bool, first_start: u64) -> (Block, usize) {
    let mut items: Vec<Vec<Block>> = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            // A blank line: could end the list (tight) if the next non-blank line
            // is not a list marker at this level. For the corpus, treat a blank
            // line as ending the list unless the next line is another marker.
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j < lines.len()
                && list_marker(lines[j])
                    .map(|(o, _)| o == ordered)
                    .unwrap_or(false)
            {
                // loose list continues; skip blanks
                i = j;
                continue;
            }
            break;
        }
        let Some((o, _)) = list_marker(line) else {
            // A non-marker, non-blank line: lazy continuation of the current item.
            if let Some(last) = items.last_mut() {
                if let Some(Block::Paragraph(p)) = last.last_mut() {
                    p.push('\n');
                    p.push_str(line.trim());
                    i += 1;
                    continue;
                }
            }
            break;
        };
        if o != ordered {
            // A different list type starts: end this list.
            break;
        }
        // Item content: strip the marker and following space.
        let content = strip_marker(line);
        items.push(vec![Block::Paragraph(content.trim().to_string())]);
        i += 1;
    }
    (
        Block::List {
            ordered,
            start: first_start,
            items,
        },
        i,
    )
}

/// Remove the list marker (and one following space/tab) from a line.
fn strip_marker(line: &str) -> String {
    let indent = leading_spaces(line);
    let rest = &line[indent..];
    let bytes = rest.as_bytes();
    let c = bytes[0];
    let marker_len = if c == b'-' || c == b'+' || c == b'*' {
        1
    } else {
        // ordered: digits + . or )
        let mut j = 0usize;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        j + 1 // include the . or )
    };
    let after = &rest[marker_len..];
    // Drop exactly one following space/tab (CommonMark: 1-4 spaces of item
    // indent; the corpus uses a single space).
    after
        .strip_prefix(' ')
        .or_else(|| after.strip_prefix('\t'))
        .unwrap_or(after)
        .to_string()
}

fn render_block(b: &Block, out: &mut String) {
    match b {
        Block::Paragraph(text) => {
            out.push_str("<p>");
            out.push_str(&render_inline(text));
            out.push_str("</p>\n");
        }
        Block::List {
            ordered,
            start,
            items,
        } => {
            if *ordered {
                if *start != 1 {
                    out.push_str(&format!("<ol start=\"{}\">\n", start));
                } else {
                    out.push_str("<ol>\n");
                }
            } else {
                out.push_str("<ul>\n");
            }
            for item in items {
                render_item(item, out);
            }
            if *ordered {
                out.push_str("</ol>\n");
            } else {
                out.push_str("</ul>\n");
            }
        }
    }
}

/// Render one (tight) list item. commonmark-java emits `<li>` + inline + `</li>\n`
/// for a tight item whose content is a single paragraph.
fn render_item(blocks: &[Block], out: &mut String) {
    out.push_str("<li>");
    // Tight item: the single paragraph's inline content is emitted directly
    // (no wrapping <p>).
    for (k, b) in blocks.iter().enumerate() {
        match b {
            Block::Paragraph(text) => {
                if k > 0 {
                    out.push('\n');
                }
                out.push_str(&render_inline(text));
            }
            other => render_block(other, out),
        }
    }
    out.push_str("</li>\n");
}

// ---------------------------------------------------------------------------
// Inline layer
// ---------------------------------------------------------------------------

/// Render paragraph/item inline content to HTML: code spans, links, emphasis,
/// with escapeHtml(true) of literal text. Newlines inside the content become
/// softbreaks (rendered as `\n` by commonmark-java).
fn render_inline(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let tokens = tokenize_inline(&chars);
    let mut out = String::new();
    render_tokens(&tokens, &mut out);
    out
}

/// An inline token stream after code-span/link extraction. Emphasis is resolved
/// in a second pass over the remaining literal text runs.
enum Inline {
    /// Literal text (NOT yet emphasis-processed nor escaped).
    Text(String),
    Code(String),
    Link {
        text_tokens: Vec<Inline>,
        url: String,
    },
    SoftBreak,
    HardBreak,
}

/// CommonMark "ASCII punctuation" (spec §2.1) — the set escapable by backslash.
fn is_ascii_punct(c: char) -> bool {
    matches!(
        c,
        '!' | '"'
            | '#'
            | '$'
            | '%'
            | '&'
            | '\''
            | '('
            | ')'
            | '*'
            | '+'
            | ','
            | '-'
            | '.'
            | '/'
            | ':'
            | ';'
            | '<'
            | '='
            | '>'
            | '?'
            | '@'
            | '['
            | '\\'
            | ']'
            | '^'
            | '_'
            | '`'
            | '{'
            | '|'
            | '}'
            | '~'
    )
}

/// First pass: pull out code spans and links (highest precedence after
/// backslash escapes). Everything else is Text.
fn tokenize_inline(chars: &[char]) -> Vec<Inline> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    let mut i = 0usize;
    let flush = |buf: &mut String, tokens: &mut Vec<Inline>| {
        if !buf.is_empty() {
            tokens.push(Inline::Text(std::mem::take(buf)));
        }
    };
    while i < chars.len() {
        let ch = chars[i];
        // CommonMark backslash escape: `\` before an ASCII-punctuation char makes
        // that char literal and drops the backslash (spec §2.4). `\` before any
        // other char is a literal backslash.
        // KNOWN LIMITATION (corpus-safe, measured): the unescaped char joins the
        // Text buffer, so the LATER emphasis pass can't distinguish an escaped
        // `*`/`_` from a real delimiter — a PAIR of escaped emphasis chars on one
        // line (`\*a\*`) would still emphasize. The corpus has only single
        // escaped chars per field (`\-`, lone `\*`), where no pairing occurs.
        if ch == '\\' && i + 1 < chars.len() && is_ascii_punct(chars[i + 1]) {
            buf.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if ch == '`' {
            // Code span: opening run of N backticks, closing run of exactly N.
            let open = run_len(chars, i, '`');
            if let Some(close) = find_backtick_run(chars, i + open, open) {
                let content: String = chars[i + open..close].iter().collect();
                flush(&mut buf, &mut tokens);
                tokens.push(Inline::Code(normalize_code(&content)));
                i = close + open;
                continue;
            }
            // no closing run: literal backticks
            for _ in 0..open {
                buf.push('`');
            }
            i += open;
            continue;
        }
        if ch == '[' {
            if let Some((text_tokens, url, next)) = try_link(chars, i) {
                flush(&mut buf, &mut tokens);
                tokens.push(Inline::Link { text_tokens, url });
                i = next;
                continue;
            }
            buf.push(ch);
            i += 1;
            continue;
        }
        if ch == '\n' {
            flush(&mut buf, &mut tokens);
            tokens.push(Inline::SoftBreak);
            i += 1;
            continue;
        }
        if ch == '\u{0}' {
            flush(&mut buf, &mut tokens);
            tokens.push(Inline::HardBreak);
            i += 1;
            continue;
        }
        buf.push(ch);
        i += 1;
    }
    flush(&mut buf, &mut tokens);
    tokens
}

fn run_len(chars: &[char], start: usize, c: char) -> usize {
    let mut n = 0usize;
    while start + n < chars.len() && chars[start + n] == c {
        n += 1;
    }
    n
}

/// Find the start index of a backtick run of exactly `len` (not longer) at or
/// after `from`.
fn find_backtick_run(chars: &[char], from: usize, len: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '`' {
            let r = run_len(chars, i, '`');
            if r == len {
                return Some(i);
            }
            i += r;
        } else {
            i += 1;
        }
    }
    None
}

/// CommonMark code-span content normalization: line endings -> spaces, and if the
/// result both begins and ends with a space (and is not all spaces) strip one
/// space from each end.
fn normalize_code(s: &str) -> String {
    let mut t: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if t.len() >= 2 && t.starts_with(' ') && t.ends_with(' ') && t.chars().any(|c| c != ' ') {
        t = t[1..t.len() - 1].to_string();
    }
    t
}

/// Try to parse an inline link `[text](url)` starting at `chars[i] == '['`.
/// Returns (text tokens, url, index after the closing `)`). Link text may itself
/// contain code/emphasis; the destination is taken verbatim (the corpus has no
/// angle-bracket destinations or titles).
fn try_link(chars: &[char], i: usize) -> Option<(Vec<Inline>, String, usize)> {
    // Find the matching `]` accounting for nested brackets and code spans.
    let mut depth = 1i32;
    let mut j = i + 1;
    while j < chars.len() {
        match chars[j] {
            '`' => {
                let r = run_len(chars, j, '`');
                if let Some(close) = find_backtick_run(chars, j + r, r) {
                    j = close + r;
                    continue;
                }
                j += 1;
            }
            '[' => {
                depth += 1;
                j += 1;
            }
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                j += 1;
            }
            _ => j += 1,
        }
    }
    if j >= chars.len() || chars[j] != ']' {
        return None;
    }
    let close_bracket = j;
    // Must be immediately followed by '('.
    if close_bracket + 1 >= chars.len() || chars[close_bracket + 1] != '(' {
        return None;
    }
    // Destination: balanced parens, no unescaped spaces stop it here (corpus urls
    // have no spaces); read to the matching ')'.
    let mut k = close_bracket + 2;
    let mut pdepth = 1i32;
    let url_start = k;
    while k < chars.len() {
        match chars[k] {
            '(' => {
                pdepth += 1;
                k += 1;
            }
            ')' => {
                pdepth -= 1;
                if pdepth == 0 {
                    break;
                }
                k += 1;
            }
            _ => k += 1,
        }
    }
    if k >= chars.len() || chars[k] != ')' {
        return None;
    }
    let url: String = chars[url_start..k].iter().collect();
    let text_slice = &chars[i + 1..close_bracket];
    let text_tokens = tokenize_inline(text_slice);
    Some((text_tokens, url.trim().to_string(), k + 1))
}

/// Second pass: resolve emphasis within Text tokens and emit HTML. Emphasis is
/// applied per-Text-run (CommonMark's delimiter stack does not cross code spans
/// or link boundaries, which are already separate tokens here).
fn render_tokens(tokens: &[Inline], out: &mut String) {
    for t in tokens {
        match t {
            Inline::Text(s) => out.push_str(&render_emphasis(s)),
            Inline::Code(s) => {
                out.push_str("<code>");
                out.push_str(&escape_html(s));
                out.push_str("</code>");
            }
            Inline::Link { text_tokens, url } => {
                out.push_str("<a href=\"");
                out.push_str(&escape_href(url));
                out.push_str("\">");
                render_tokens(text_tokens, out);
                out.push_str("</a>");
            }
            Inline::SoftBreak => out.push('\n'),
            // commonmark-java HtmlRenderer emits a hard break as `<br />\n`.
            Inline::HardBreak => out.push_str("<br />\n"),
        }
    }
}

/// escapeHtml(true): the four HTML-significant characters in text content.
/// commonmark-java's HtmlRenderer escapes `&`,`<`,`>` and `"` (`"` only matters
/// in attributes, but its Escaping.escapeHtml escapes it in text too). Matches
/// `Escaping.ESCAPE_HTML` (`[&<>\"]`).
fn escape_html(s: &str) -> String {
    let mut b = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => b.push_str("&amp;"),
            '<' => b.push_str("&lt;"),
            '>' => b.push_str("&gt;"),
            '"' => b.push_str("&quot;"),
            _ => b.push(c),
        }
    }
    b
}

/// Link destination escaping: commonmark-java percent-encodes/escapes the URL via
/// `Escaping.escapeHtml` after URI normalization. For the corpus urls (already
/// clean http(s) with query strings) the only transform that fires is escapeHtml
/// of `&` -> `&amp;`. We reproduce that (and `<`,`>`,`"` for safety).
fn escape_href(url: &str) -> String {
    escape_html(url)
}

// --- emphasis (CommonMark delimiter-stack, restricted subset) ---

/// A run of `*` or `_` delimiter chars, with its flanking classification.
struct Delim {
    ch: char,
    /// index of the first delimiter char in `chars`
    pos: usize,
    /// number of delimiters still available in this run
    count: usize,
    can_open: bool,
    can_close: bool,
}

/// A resolved emphasis span [open_end..close_start), with a level (1=em,
/// 2=strong) — stored as byte-free char-index ranges to render in order.
struct Emph {
    /// char index where the inner content begins (just after the openers used)
    inner_lo: usize,
    /// char index where the inner content ends (just before the closers used)
    inner_hi: usize,
    /// char index range of the whole span including delimiters
    span_lo: usize,
    span_hi: usize,
    strong: bool,
}

/// Resolve `*`/`_` emphasis in a literal text run and emit escaped HTML.
/// Implements CommonMark's delimiter-stack `process_emphasis` for the single-run
/// case (no links/code inside — those were already split out upstream), honoring
/// left/right flanking and the `_`-intraword restriction.
fn render_emphasis(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let delims = scan_delims(&chars);
    let emphs = process_emphasis(&chars, delims);
    let mut out = String::new();
    emit_with_emphasis(&chars, 0, chars.len(), &emphs, &mut out);
    out
}

fn is_unicode_ws(c: char) -> bool {
    c.is_whitespace()
}

/// CommonMark "punctuation" for flanking: ASCII punctuation + Unicode P*.
fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(
            c,
            '\u{00A1}'
                | '\u{00BF}'
                | '\u{2013}'
                | '\u{2014}'
                | '\u{2018}'
                | '\u{2019}'
                | '\u{201C}'
                | '\u{201D}'
                | '\u{2026}'
        )
}

/// Scan all `*`/`_` delimiter runs with flanking classification
/// (CommonMark `InlineParserImpl.scanDelimiters`).
fn scan_delims(chars: &[char]) -> Vec<Delim> {
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < n {
        let c = chars[i];
        if c != '*' && c != '_' {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && chars[i] == c {
            i += 1;
        }
        let count = i - start;
        let before = if start == 0 { ' ' } else { chars[start - 1] };
        let after = if i >= n { ' ' } else { chars[i] };
        let before_ws = is_unicode_ws(before);
        let after_ws = is_unicode_ws(after);
        let before_punct = is_punct(before);
        let after_punct = is_punct(after);
        // left-flanking: not followed by ws, and (not followed by punct OR
        // preceded by ws/punct).
        let left_flanking = !after_ws && (!after_punct || before_ws || before_punct);
        let right_flanking = !before_ws && (!before_punct || after_ws || after_punct);
        let (can_open, can_close) = if c == '_' {
            (
                left_flanking && (!right_flanking || before_punct),
                right_flanking && (!left_flanking || after_punct),
            )
        } else {
            (left_flanking, right_flanking)
        };
        out.push(Delim {
            ch: c,
            pos: start,
            count,
            can_open,
            can_close,
        });
    }
    out
}

/// CommonMark `process_emphasis`: pair openers/closers, preferring strong (2)
/// then em (1). Returns resolved spans (non-overlapping, innermost first is not
/// guaranteed; we render by recursion over ranges).
fn process_emphasis(chars: &[char], mut delims: Vec<Delim>) -> Vec<Emph> {
    let mut emphs: Vec<Emph> = Vec::new();
    // closer index walk
    let mut ci = 0usize;
    while ci < delims.len() {
        if !delims[ci].can_close || delims[ci].count == 0 {
            ci += 1;
            continue;
        }
        let cch = delims[ci].ch;
        // find nearest opener before ci with same char that can open
        let mut oi_opt = None;
        let mut oj = ci;
        while oj > 0 {
            oj -= 1;
            if delims[oj].ch == cch && delims[oj].can_open && delims[oj].count > 0 {
                // rule of 3: if either the opener or closer count is a multiple
                // of 3 restriction — apply the "sum multiple of 3" check.
                let oc = &delims[oj];
                let cc = &delims[ci];
                let both_can = oc.can_close || cc.can_open;
                let sum_mult3 = (oc.count + cc.count) % 3 == 0;
                let each_mult3 = oc.count % 3 == 0 && cc.count % 3 == 0;
                if both_can && sum_mult3 && !each_mult3 {
                    continue;
                }
                oi_opt = Some(oj);
                break;
            }
        }
        let Some(oi) = oi_opt else {
            ci += 1;
            continue;
        };
        // determine strength: strong if both have >=2 left
        let use_count = if delims[oi].count >= 2 && delims[ci].count >= 2 {
            2
        } else {
            1
        };
        let strong = use_count == 2;
        let open_pos = delims[oi].pos + (delims[oi].count - use_count);
        let close_pos = delims[ci].pos + 0;
        // inner content is between openers-used-end and closers-start
        let inner_lo = delims[oi].pos + delims[oi].count; // will fix after consuming
        let _ = inner_lo;
        // Consume delimiters
        delims[oi].count -= use_count;
        delims[ci].count -= use_count;
        let inner_lo2 = open_pos + use_count;
        let inner_hi2 = close_pos;
        emphs.push(Emph {
            inner_lo: inner_lo2,
            inner_hi: inner_hi2,
            span_lo: open_pos,
            span_hi: close_pos + use_count,
            strong,
        });
        // stay on same closer if it still has delimiters
        if delims[ci].count == 0 {
            ci += 1;
        }
        let _ = chars;
    }
    emphs.sort_by_key(|e| e.span_lo);
    emphs
}

/// Emit `chars[lo..hi]`, wrapping any emphasis span fully contained in the range
/// with `<em>`/`<strong>` and escaping the rest. Renders top-level spans in this
/// range in order and recurses into their inner content.
fn emit_with_emphasis(chars: &[char], lo: usize, hi: usize, emphs: &[Emph], out: &mut String) {
    let mut i = lo;
    // find top-level spans within [lo,hi)
    let mut spans: Vec<&Emph> = emphs
        .iter()
        .filter(|e| e.span_lo >= lo && e.span_hi <= hi)
        .collect();
    spans.sort_by_key(|e| e.span_lo);
    // keep only non-nested (top-level relative to this range)
    let mut top: Vec<&Emph> = Vec::new();
    let mut cursor = lo;
    for e in spans {
        if e.span_lo >= cursor {
            top.push(e);
            cursor = e.span_hi;
        }
    }
    for e in top {
        if e.span_lo > i {
            out.push_str(&escape_html(&collect(chars, i, e.span_lo)));
        }
        let tag = if e.strong { "strong" } else { "em" };
        out.push('<');
        out.push_str(tag);
        out.push('>');
        emit_with_emphasis(chars, e.inner_lo, e.inner_hi, emphs, out);
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
        i = e.span_hi;
    }
    if i < hi {
        out.push_str(&escape_html(&collect(chars, i, hi)));
    }
}

fn collect(chars: &[char], lo: usize, hi: usize) -> String {
    chars[lo..hi].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::render_html;

    #[test]
    fn plain_paragraph() {
        assert_eq!(render_html("Hello world."), "<p>Hello world.</p>\n");
    }

    #[test]
    fn escape_html_in_text() {
        // reference range is >=5 - <=9  -> literal <, > escaped exactly once.
        assert_eq!(
            render_html("a < b & c > d"),
            "<p>a &lt; b &amp; c &gt; d</p>\n"
        );
    }

    #[test]
    fn link_with_amp_in_text_and_query_url() {
        assert_eq!(
            render_html("see [Race & Ethnicity](https://x.gov/a?id=1.2.3) here"),
            "<p>see <a href=\"https://x.gov/a?id=1.2.3\">Race &amp; Ethnicity</a> here</p>\n"
        );
    }

    #[test]
    fn strong_and_em() {
        assert_eq!(
            render_html("a **SHALL** b"),
            "<p>a <strong>SHALL</strong> b</p>\n"
        );
        assert_eq!(render_html("x *All* y"), "<p>x <em>All</em> y</p>\n");
    }

    #[test]
    fn code_span() {
        assert_eq!(
            render_html("the `content` element"),
            "<p>the <code>content</code> element</p>\n"
        );
    }

    #[test]
    fn intraword_underscore_is_literal() {
        assert_eq!(render_html("a_b_c"), "<p>a_b_c</p>\n");
        assert_eq!(
            render_html(".meta.lastUpdated"),
            "<p>.meta.lastUpdated</p>\n"
        );
    }

    #[test]
    fn two_paragraphs() {
        assert_eq!(render_html("one\n\ntwo"), "<p>one</p>\n<p>two</p>\n");
    }

    #[test]
    fn para_then_bullets() {
        // us-core-ethnicity shape: paragraph, blank, 3-space-indented `- ` list.
        assert_eq!(
            render_html("cats:\n\n   - Hispanic or Latino\n   - Not Hispanic or Latino."),
            "<p>cats:</p>\n<ul>\n<li>Hispanic or Latino</li>\n<li>Not Hispanic or Latino.</li>\n</ul>\n"
        );
    }

    #[test]
    fn star_bullets_interrupt_paragraph() {
        assert_eq!(
            render_html("Guidelines:\n* a\n* b"),
            "<p>Guidelines:</p>\n<ul>\n<li>a</li>\n<li>b</li>\n</ul>\n"
        );
    }

    #[test]
    fn hard_break_two_trailing_spaces() {
        assert_eq!(
            render_html("line one.  \nline two"),
            "<p>line one.<br />\nline two</p>\n"
        );
    }

    #[test]
    fn soft_break_single_newline() {
        assert_eq!(
            render_html("line one.\nline two"),
            "<p>line one.\nline two</p>\n"
        );
    }
}
