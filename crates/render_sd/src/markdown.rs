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

/// True if `md` is "plain" — no commonmark block/inline syntax that would change
/// the single-paragraph, single-text-piece outcome. Conservative: any of the
/// commonmark-significant characters routes to the (currently unhandled) rich
/// path.
fn is_plain_prose(md: &str) -> bool {
    // Reject anything with markdown structure. Note: '.' and ',' are fine.
    // We also must reject blank-line paragraph breaks.
    if md.contains("\n\n") || md.contains("\r\n\r\n") {
        return false;
    }
    // Inline/block markers that commonmark would interpret.
    const SPECIAL: &[char] = &['*', '_', '`', '[', ']', '#', '<', '>', '|', '\\', '~'];
    if md.chars().any(|c| SPECIAL.contains(&c)) {
        return false;
    }
    // A leading list/heading marker or numbered list.
    let t = md.trim_start();
    if t.starts_with("- ") || t.starts_with("+ ") {
        return false;
    }
    true
}

/// `addMarkdown(md)` -> append pieces to the cell.
pub fn add_markdown(cell: &mut Cell, md: &str) {
    if md.is_empty() {
        return;
    }
    if is_plain_prose(md) {
        // Single paragraph -> one text Piece with the verbatim content, THEN two
        // `Piece("br")`. The commonmark HtmlRenderer emits "<p>text</p>\n"; when
        // htmlToParagraphPieces re-parses "<html><p>text</p>\n</html>", the <p>
        // yields the text piece, and the trailing "\n" text child (now the
        // non-first sibling) triggers the `Piece("br"); Piece("br")` insert
        // before it, while the whitespace-only text itself is skipped
        // (HTG:398-403). Net: [text, br, br]. (fork-verified against the grid
        // golden's `...verified.<br/><br/>` tail.)
        let text = md.trim_matches(|c| c == '\n' || c == '\r').to_string();
        cell.pieces.push(Piece::ref_text(None, Some(text), None));
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
