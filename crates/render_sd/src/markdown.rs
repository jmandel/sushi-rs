//! `Cell.addMarkdown(md)` (HTG:336-353) and friends — the commonmark -> Pieces
//! path, ported faithfully.
//!
//! Path (HierarchicalTableGenerator.java):
//!   1. `Parser.builder().build()` + `HtmlRenderer.escapeHtml(true)` render the
//!      markdown to an HTML string (HTG:343-346). commonmark-java, plain profile
//!      (NO TablesExtension, NO preProcess). Reproduced by `crate::commonmark`.
//!   2. `htmlToParagraphPieces(html, style)` (HTG:392-425) re-parses that HTML
//!      with the XhtmlParser (`<html>`+html+`</html>`) and walks the top-level
//!      children into Pieces, inserting `Piece("br"); Piece("br")` before every
//!      child after the first; `<p>` children contribute their inline children
//!      via `addNode`, text children (if non-whitespace) via `addNode`, and any
//!      other element (ul/ol/pre/h*/...) becomes a `Piece(tagName)` carrying that
//!      element's child XhtmlNodes.
//!   3. `addNode(list, node, style)` (HTG:439-472) maps each inline node to a
//!      Piece (a->link, b/em/strong->bold, code->code style, i->italic, etc.).
//!
//! The downstream `render_piece` (render_tables) + XhtmlComposer reproduce the
//! final bytes, including the `\r\n` that `breakBlocksWithLines` inserts before a
//! block-level Piece (`<ul>`/`<li>`) whose previous sibling is not text.

use render_tables::model::{Cell, Piece};
use render_xhtml::{NodeType, XhtmlNode, XhtmlParser};

use crate::commonmark;

const CODE_STYLE: &str =
    "padding: 2px 4px; color: #005c00; background-color: #f9f2f4; white-space: nowrap; border-radius: 4px";

/// `addMarkdown(md)` (HTG:336) -> append pieces to the cell.
pub fn add_markdown(cell: &mut Cell, md: &str) {
    if md.is_empty() {
        return;
    }
    let html = commonmark::render_html(md);
    let pieces = html_to_paragraph_pieces(&html);
    cell.pieces.extend(pieces);
}

/// `addMarkdownNoPara(role, md, style)` (HTG:372): markdown -> pieces, trailing
/// `br` pieces trimmed, `role` (class) set on every piece. Used for binding
/// descriptions in the SUMMARY description cell (SDR:2000).
pub fn add_markdown_no_para_role(cell: &mut Cell, md: &str, role: &str) {
    let html = commonmark::render_html(md);
    let mut pieces = html_to_paragraph_pieces(&html);
    // Trim unwanted trailing line-breaks (HTG:380-381).
    while pieces
        .last()
        .map(|p| p.get_tag() == Some("br"))
        .unwrap_or(false)
    {
        pieces.pop();
    }
    for p in &mut pieces {
        p.set_class(role);
    }
    cell.pieces.extend(pieces);
}

/// Port of `htmlToParagraphPieces(html, style=null)` (HTG:392-425).
fn html_to_paragraph_pieces(html: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let wrapped = format!("<html>{html}</html>");
    let mut parser = XhtmlParser::new();
    let node = match parser.parse_fragment(&wrapped) {
        Ok(n) => n,
        Err(_) => {
            // Faithful to Java's try/catch-then-throw is a hard error; but for
            // robustness we degrade to a single text piece (loud, visible).
            pieces.push(Piece::ref_text(None, Some(html.to_string()), None));
            return pieces;
        }
    };
    let mut first = true;
    for c in node.child_nodes() {
        if first {
            first = false;
        } else {
            pieces.push(Piece::tag("br"));
            pieces.push(Piece::tag("br"));
        }
        match c.node_type() {
            NodeType::Text => {
                if !is_whitespace(c.content().unwrap_or("")) {
                    add_node(&mut pieces, c);
                }
            }
            NodeType::Element if c.name() == Some("p") => {
                for g in c.child_nodes() {
                    add_node(&mut pieces, g);
                }
            }
            NodeType::Element => {
                // HTG else-branch: Piece(name) carrying the element's children.
                let mut x = Piece::tag(c.name().unwrap_or(""));
                for g in c.child_nodes() {
                    x.add_html(g.clone());
                }
                pieces.push(x);
            }
            _ => {}
        }
    }
    pieces
}

/// Port of `addNode(list, c, style=null)` (HTG:439-472).
fn add_node(list: &mut Vec<Piece>, c: &XhtmlNode) {
    match c.node_type() {
        NodeType::Text => {
            list.push(Piece::ref_text(None, Some(c.content().unwrap_or("").to_string()), None));
        }
        NodeType::Element => {
            let name = c.name().unwrap_or("");
            match name {
                "a" => {
                    let href = attr(c, "href");
                    let title = attr(c, "title");
                    list.push(Piece::ref_text(href, Some(all_text(c)), title));
                }
                "b" | "em" | "strong" => {
                    let mut p = Piece::ref_text(None, Some(all_text(c)), None);
                    p.set_style("font-face: bold");
                    list.push(p);
                }
                "code" => {
                    let mut p = Piece::ref_text(None, Some(all_text(c)), None);
                    p.set_style(CODE_STYLE);
                    list.push(p);
                }
                "i" => {
                    let mut p = Piece::ref_text(None, Some(all_text(c)), None);
                    p.set_style("font-style: italic");
                    list.push(p);
                }
                "pre" => {
                    let mut p = Piece::tag("pre");
                    p.set_style("white-space: pre; font-family: courier");
                    for g in c.child_nodes() {
                        p.add_html(g.clone());
                    }
                    list.push(p);
                }
                "ul" | "ol" => {
                    let mut p = Piece::tag(name);
                    for g in c.child_nodes() {
                        p.add_html(g.clone());
                    }
                    list.push(p);
                }
                "h1" | "h2" | "h3" | "h4" => {
                    let mut p = Piece::tag(name);
                    for g in c.child_nodes() {
                        p.add_html(g.clone());
                    }
                    list.push(p);
                }
                "br" => {
                    list.push(Piece::tag("br"));
                }
                other => {
                    // HTG throws `new Error("Not handled yet: "+name)`. We keep a
                    // loud, visible marker instead of panicking the render.
                    list.push(Piece::ref_text(
                        None,
                        Some(format!("[unhandled markdown element: {other}]")),
                        None,
                    ));
                }
            }
        }
        _ => {}
    }
}

/// `XhtmlNode.getAttribute(name)` -> Option (Java returns null when absent, which
/// `new Piece(href, text, title)` stores as null).
fn attr(node: &XhtmlNode, name: &str) -> Option<String> {
    node.attributes().get(name).and_then(|v| v.clone())
}

/// Port of `XhtmlNode.allText()` (XhtmlNode.java:381): recursive concatenation of
/// descendant text, `* ` prefix before each `li` child, `img` skipped.
fn all_text(node: &XhtmlNode) -> String {
    if !node.has_children() {
        return node.content().unwrap_or("").to_string();
    }
    let mut b = String::new();
    for n in node.child_nodes() {
        if n.node_type() == NodeType::Element && n.name() == Some("li") {
            b.push_str("* ");
        }
        if n.node_type() == NodeType::Text {
            if let Some(c) = n.content() {
                b.push_str(c);
            }
        }
        if n.node_type() == NodeType::Element {
            if n.name() != Some("img") {
                b.push_str(&all_text(n));
            }
        }
    }
    b
}

/// Java `StringUtils.isWhitespace`: true for empty or all-whitespace strings.
fn is_whitespace(s: &str) -> bool {
    s.chars().all(|c| c.is_whitespace())
}
