//! `Cell.addMarkdown(md)` (HTG:333) — the commonmark -> pieces path.
//!
//! Full path: commonmark `Parser` -> `HtmlRenderer.escapeHtml(true)` -> HTML,
//! then `htmlToParagraphPieces` (HTG:389) re-parses that HTML with XhtmlParser
//! and turns each top-level node into Pieces, inserting `Piece("br");Piece("br")`
//! between paragraphs.
//!
//! For the SD description cells the definitions/comments the golden corpus shows
//! are overwhelmingly plain prose (a single paragraph, no markdown syntax). For
//! that case the result is exactly ONE text Piece whose text is the definition
//! verbatim (the commonmark HTML-escape is undone when the HTML is re-parsed by
//! XhtmlParser, and the final XhtmlComposer re-escapes). We implement that case
//! faithfully and route anything containing markdown/HTML syntax through a
//! commonmark-subset renderer (F3 leaf work) — flagged so a non-plain definition
//! is caught as an explicit gap rather than silently wrong.

use render_tables::model::{Cell, Piece};

/// True if `md` is a single paragraph whose only inline constructs are ones
/// `inline_pieces` handles ([links], `code`). Anything else routes to the
/// verbatim fallback (visible divergence rather than silent wrongness).
fn is_plain_prose(md: &str) -> bool {
    if md.contains("\n\n") || md.contains("\r\n\r\n") {
        return false;
    }
    // Tokenize first: specials INSIDE link urls/text or code spans are fine;
    // only text runs containing unhandled markdown constructs reject.
    const SPECIAL: &[char] = &['*', '_', '#', '<', '>', '|', '\\', '~'];
    for run in text_runs(md) {
        if run.chars().any(|c| SPECIAL.contains(&c)) {
            return false;
        }
    }
    let t = md.trim_start();
    if t.starts_with("- ") || t.starts_with("+ ") {
        return false;
    }
    true
}

/// The plain-text runs of `md` after removing [text](url) links and `code`
/// spans (mirrors inline_pieces' tokenization).
fn text_runs(md: &str) -> Vec<String> {
    let chars: Vec<char> = md.chars().collect();
    let mut runs = Vec::new();
    let mut buf = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '[' {
            if let Some(close) = find_from(&chars, i + 1, ']') {
                if close + 1 < chars.len() && chars[close + 1] == '(' {
                    if let Some(end) = find_from(&chars, close + 2, ')') {
                        runs.push(std::mem::take(&mut buf));
                        i = end + 1;
                        continue;
                    }
                }
            }
            buf.push(ch);
            i += 1;
        } else if ch == '`' {
            if let Some(end) = find_from(&chars, i + 1, '`') {
                runs.push(std::mem::take(&mut buf));
                i = end + 1;
                continue;
            }
            buf.push(ch);
            i += 1;
        } else {
            buf.push(ch);
            i += 1;
        }
    }
    runs.push(buf);
    runs
}

/// Convert one paragraph's inline markdown to pieces: text runs, `[t](url)`
/// links (HTG addNode "a" branch: Piece(href, text, title=null)) and `code`
/// spans (HTG addNode "code" branch with its literal style string).
fn inline_pieces(cell: &mut Cell, md: &str) {
    let mut buf = String::new();
    let chars: Vec<char> = md.chars().collect();
    let mut i = 0usize;
    let flush = |buf: &mut String, cell: &mut Cell| {
        if !buf.is_empty() {
            cell.pieces.push(Piece::ref_text(None, Some(std::mem::take(buf)), None));
        }
    };
    while i < chars.len() {
        let ch = chars[i];
        if ch == '[' {
            // find ](
            if let Some(close) = find_from(&chars, i + 1, ']') {
                if close + 1 < chars.len() && chars[close + 1] == '(' {
                    if let Some(end) = find_from(&chars, close + 2, ')') {
                        let text: String = chars[i + 1..close].iter().collect();
                        let url: String = chars[close + 2..end].iter().collect();
                        flush(&mut buf, cell);
                        cell.pieces.push(Piece::ref_text(Some(url), Some(text), None));
                        i = end + 1;
                        continue;
                    }
                }
            }
            buf.push(ch);
            i += 1;
        } else if ch == '`' {
            if let Some(end) = find_from(&chars, i + 1, '`') {
                let text: String = chars[i + 1..end].iter().collect();
                flush(&mut buf, cell);
                let mut p = Piece::ref_text(None, Some(text), None);
                p.set_style("padding: 2px 4px; color: #005c00; background-color: #f9f2f4; white-space: nowrap; border-radius: 4px");
                cell.pieces.push(p);
                i = end + 1;
                continue;
            }
            buf.push(ch);
            i += 1;
        } else {
            buf.push(ch);
            i += 1;
        }
    }
    let mut buf2 = buf;
    if !buf2.is_empty() {
        cell.pieces.push(Piece::ref_text(None, Some(std::mem::take(&mut buf2)), None));
    }
}

fn find_from(chars: &[char], start: usize, target: char) -> Option<usize> {
    chars[start..].iter().position(|&c| c == target).map(|p| p + start)
}

/// `addMarkdownNoPara(role, md, style)` (HTG:369): markdown -> pieces, trailing
/// `br` pieces trimmed, `role` (class) set on every piece. Used for binding
/// descriptions in the SUMMARY description cell (SDR:2009).
pub fn add_markdown_no_para_role(cell: &mut Cell, md: &str, role: &str) {
    let start = cell.pieces.len();
    add_markdown(cell, md);
    // trim trailing br pieces
    while cell.pieces.len() > start {
        let last = cell.pieces.last().unwrap();
        if last.get_tag() == Some("br") {
            cell.pieces.pop();
        } else {
            break;
        }
    }
    for p in &mut cell.pieces[start..] {
        p.set_class(role);
    }
}

/// `addMarkdown(md)` -> append pieces to the cell.
pub fn add_markdown(cell: &mut Cell, md: &str) {
    if md.is_empty() {
        return;
    }
    if is_plain_prose(md) {
        // Single paragraph -> inline pieces, THEN two `Piece("br")`. The
        // commonmark HtmlRenderer emits "<p>text</p>\n"; when
        // htmlToParagraphPieces re-parses "<html><p>text</p>\n</html>", the <p>
        // yields the inline pieces, and the trailing "\n" text child (now the
        // non-first sibling) triggers the `Piece("br"); Piece("br")` insert
        // before it, while the whitespace-only text itself is skipped
        // (HTG:398-403). Net: [pieces..., br, br].
        let text = md.trim_matches(|c| c == '\n' || c == '\r').to_string();
        inline_pieces(cell, &text);
        cell.pieces.push(Piece::tag("br"));
        cell.pieces.push(Piece::tag("br"));
    } else {
        // Rich markdown (lists, emphasis, code, links, multi-paragraph). This is
        // F3 leaf work; for now emit the raw text so the divergence is visible
        // and classifiable rather than a panic. Marked as a known gap.
        cell.pieces
            .push(Piece::ref_text(None, Some(md.to_string()), None));
    }
}
