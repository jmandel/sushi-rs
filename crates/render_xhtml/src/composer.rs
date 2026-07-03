//! Port of `org.hl7.fhir.utilities.xhtml.XhtmlComposer`.
//!
//! Source of truth (READ-ONLY, cited as XhtmlComposer.java:<line>):
//!   fhir-core/org.hl7.fhir.utilities/src/main/java/org/hl7/fhir/utilities/xhtml/XhtmlComposer.java
//!
//! Only the string-producing write path is ported (the DOM/`IXMLWriter`/plain-
//! text variants are out of scope for byte-parity of `_includes/*.xhtml`).
//!
//! ## Composer configuration used by the publisher fragment path
//!
//! The publisher writes each fragment file as:
//!   `wrapLiquid(fixedContent)` == "{% raw %}" + content + "{% endraw %}"
//!   (PublisherGenerator.java:2454, 2463, 6510-6512)
//! where `content` is the RAW STRING produced by a per-fragment renderer. The
//! fragment file is NOT re-serialized through XhtmlComposer at write time.
//! The composer is used UPSTREAM by individual renderers to turn XhtmlNode
//! trees into those strings, with per-renderer configuration. Tallying every
//! `new XhtmlComposer(...)` across fhir-core + ig-publisher, the two dominant
//! configs are `XhtmlComposer(XML)` (xml=true, pretty=false) and
//! `XhtmlComposer(false, true)` (HTML pretty). Table generators
//! (HierarchicalTableGenerator, C2) also emit via the composer.
//!
//! For the C3 round-trip gate — parse a golden then re-compose to the same
//! bytes — the decisive config is `xml=true, pretty=false`
//! (`Config::xml_compact()`): with insertion-ordered attributes it reproduces
//! any well-formed fragment verbatim, because parsing records structure/order
//! from the already-serialized bytes and this config performs no reflowing,
//! no pretty indentation, and no HTML self-closing rewrites. See
//! `roundtrip.rs` / the gate test for the empirical confirmation across the
//! corpus.

use crate::node::{NodeType, XhtmlNode, NBSP};

/// Composer configuration. Mirrors the four boolean flags on XhtmlComposer
/// (XhtmlComposer.java:51-54).
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// `xml` (XhtmlComposer.java:52). XML mode vs HTML mode.
    pub xml: bool,
    /// `pretty` (XhtmlComposer.java:51).
    pub pretty: bool,
    /// `autoLinks` (XhtmlComposer.java:53).
    pub auto_links: bool,
    /// `canonical` (XhtmlComposer.java:54).
    pub canonical: bool,
}

impl Config {
    /// `new XhtmlComposer(XhtmlComposer.XML)` == `XhtmlComposer(true)`:
    /// xml=true, pretty=false (XhtmlComposer.java:56, 65-69). This is the
    /// config the round-trip gate uses.
    pub fn xml_compact() -> Config {
        Config {
            xml: true,
            pretty: false,
            auto_links: false,
            canonical: false,
        }
    }

    /// `new XhtmlComposer(false, true)`: HTML, pretty.
    pub fn html_pretty() -> Config {
        Config {
            xml: false,
            pretty: true,
            auto_links: false,
            canonical: false,
        }
    }

    /// `new XhtmlComposer(false)`: HTML, not pretty.
    pub fn html_compact() -> Config {
        Config {
            xml: false,
            pretty: false,
            auto_links: false,
            canonical: false,
        }
    }
}

pub struct XhtmlComposer {
    cfg: Config,
    dst: String,
}

impl XhtmlComposer {
    pub fn new(cfg: Config) -> Self {
        XhtmlComposer {
            cfg,
            dst: String::new(),
        }
    }

    /// Java `compose(XhtmlNode node)` (XhtmlComposer.java:105) — the fragment
    /// entry point. `!xml && !pretty` triggers `breakBlocksWithLines`.
    pub fn compose_node(&mut self, node: &XhtmlNode) -> String {
        let mut node = node.clone();
        if !self.cfg.xml && !self.cfg.pretty {
            break_blocks_with_lines_node(&mut node);
        }
        self.dst.clear();
        self.write_node("", &node, false);
        std::mem::take(&mut self.dst)
    }

    /// Java `compose(XhtmlNodeList nodes)` (XhtmlComposer.java:490) — the
    /// method the publisher fragment path actually invokes via
    /// `composer.compose(x.getChildNodes())` (e.g. dict at
    /// StructureDefinitionRenderer.java:1320). Overload resolution selects the
    /// `XhtmlNodeList` variant over the `List<XhtmlNode>` variant, and CRUCIALLY
    /// this variant does NOT call `breakBlocksWithLines` — it just writes each
    /// node with indent "". This is the correct behavior for byte-parity of
    /// `_includes/*.xhtml` fragments.
    pub fn compose_nodes(&mut self, nodes: &[XhtmlNode]) -> String {
        self.dst.clear();
        for n in nodes {
            self.write_node("", n, false);
        }
        std::mem::take(&mut self.dst)
    }

    /// Java `compose(List<XhtmlNode> nodes)` (XhtmlComposer.java:118) — the
    /// OTHER list overload, which DOES run `breakBlocksWithLines` in HTML
    /// non-pretty mode. Provided for completeness; the fragment path above uses
    /// the `XhtmlNodeList` variant instead.
    pub fn compose_node_list_with_breaks(&mut self, nodes: &[XhtmlNode]) -> String {
        let mut nodes = nodes.to_vec();
        if !self.cfg.xml && !self.cfg.pretty {
            break_blocks_with_lines_list(&mut nodes);
        }
        self.dst.clear();
        for n in &nodes {
            self.write_node("", n, false);
        }
        std::mem::take(&mut self.dst)
    }

    /// Java `compose(XhtmlDocument doc)` (XhtmlComposer.java:73) -> composeDoc.
    /// composeDoc writes each child with indent "  " (XhtmlComposer.java:144).
    pub fn compose_document(&mut self, doc: &XhtmlNode) -> String {
        let mut doc = doc.clone();
        if !self.cfg.xml && !self.cfg.pretty {
            // compose() calls breakBlocksWithLines(doc); composeDoc() also does
            // when !xml (XhtmlComposer.java:74,139-141). Idempotent enough for
            // our corpus; we mirror the compose() guard.
            break_blocks_with_lines_node(&mut doc);
        }
        self.dst.clear();
        // composeDoc: for !xml, breakBlocksWithLines(doc) again (line 140).
        if !self.cfg.xml {
            break_blocks_with_lines_node(&mut doc);
        }
        for c in doc.child_nodes() {
            self.write_node("  ", c, false);
        }
        std::mem::take(&mut self.dst)
    }

    /// Java `writeNode` (XhtmlComposer.java:150).
    fn write_node(&mut self, indent: &str, node: &XhtmlNode, no_pretty_override: bool) {
        match node.node_type() {
            NodeType::Comment => self.write_comment(indent, node, no_pretty_override),
            NodeType::DocType => self.write_doc_type(node),
            NodeType::Instruction => self.write_instruction(node),
            NodeType::Element => self.write_element(indent, node, no_pretty_override),
            NodeType::Document => self.write_document(node),
            NodeType::Text => self.write_text(node),
            NodeType::CData => self.write_cdata(indent, node, no_pretty_override),
        }
    }

    /// Java `writeText` (XhtmlComposer.java:176). Iterates by Unicode code point.
    fn write_text(&mut self, node: &XhtmlNode) {
        let src = node.content().unwrap_or("");
        let cfg = self.cfg;
        // Java indexes UTF-16; the only place surrogate handling matters is the
        // `ci > 65535` branch (astral -> numeric char ref). We iterate over
        // Rust chars (Unicode scalar values), which is equivalent: a scalar
        // > 0xFFFF is exactly Java's `ci > 65535` case.
        let chars: Vec<char> = src.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let c = chars[i];
            let ci = c as u32;
            if cfg.canonical {
                match c {
                    '&' => self.dst.push_str("&amp;"),
                    '<' => self.dst.push_str("&lt;"),
                    '>' => self.dst.push_str("&gt;"),
                    '\r' => self.dst.push_str("#xD;"),
                    _ => self.dst.push(c),
                }
                i += 1;
            } else if ci > 65535 {
                // Java: "&#x" + Integer.toHexString(ci).toUpperCase() + ";"
                self.dst.push_str("&#x");
                self.dst.push_str(&format!("{:X}", ci));
                self.dst.push(';');
                i += 1;
            } else {
                if cfg.auto_links && c == 'h' && {
                    let rest: String = chars[i..].iter().collect();
                    rest.starts_with("http://") || rest.starts_with("https://")
                } {
                    let j = i;
                    while i < chars.len() && is_valid_url_char(chars[i]) {
                        i += 1;
                    }
                    let mut url: String = chars[j..i].iter().collect();
                    if url.ends_with('.') || url.ends_with(',') {
                        i -= 1;
                        url.pop();
                    }
                    let url = escape_xml(&url);
                    self.dst.push_str(&format!("<a href=\"{0}\">{0}</a>", url));
                } else {
                    i += 1;
                    match c {
                        '&' => self.dst.push_str("&amp;"),
                        '<' => self.dst.push_str("&lt;"),
                        '>' => self.dst.push_str("&gt;"),
                        _ if cfg.xml => {
                            if c == '"' {
                                self.dst.push_str("&quot;");
                            } else {
                                self.dst.push(c);
                            }
                        }
                        _ => {
                            // HTML mode named-entity substitutions
                            // (XhtmlComposer.java:228-241).
                            if c == NBSP {
                                self.dst.push_str("&nbsp;");
                            } else if c == '\u{00A7}' {
                                self.dst.push_str("&sect;");
                            } else if c == '\u{00A9}' {
                                self.dst.push_str("&copy;");
                            } else if c == '\u{2122}' {
                                self.dst.push_str("&trade;");
                            } else if c == '\u{03BC}' {
                                self.dst.push_str("&mu;");
                            } else if c == '\u{00AE}' {
                                self.dst.push_str("&reg;");
                            } else {
                                self.dst.push(c);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Java `writeComment` (XhtmlComposer.java:289).
    fn write_comment(&mut self, indent: &str, node: &XhtmlNode, no_pretty_override: bool) {
        let trimmed = node.content().unwrap_or("").trim();
        self.dst.push_str(indent);
        self.dst.push_str("<!-- ");
        self.dst.push_str(trimmed);
        self.dst.push_str(" -->");
        if self.cfg.pretty && !no_pretty_override {
            self.dst.push_str("\r\n");
        }
    }

    /// Java `writeCData` (XhtmlComposer.java:293).
    fn write_cdata(&mut self, indent: &str, node: &XhtmlNode, no_pretty_override: bool) {
        self.dst.push_str(indent);
        self.dst.push_str("<![CDATA[");
        self.dst.push_str(node.content().unwrap_or(""));
        self.dst.push_str("]]>");
        if self.cfg.pretty && !no_pretty_override {
            self.dst.push_str("\r\n");
        }
    }

    /// Java `writeDocType` (XhtmlComposer.java:297).
    fn write_doc_type(&mut self, node: &XhtmlNode) {
        self.dst.push_str("<!");
        self.dst.push_str(node.content().unwrap_or(""));
        self.dst.push_str(">\r\n");
    }

    /// Java `writeInstruction` (XhtmlComposer.java:301).
    fn write_instruction(&mut self, node: &XhtmlNode) {
        self.dst.push_str("<?");
        self.dst.push_str(node.content().unwrap_or(""));
        self.dst.push_str("?>\r\n");
    }

    /// Java `attributes` (XhtmlComposer.java:306).
    fn attributes(&self, node: &XhtmlNode) -> String {
        let mut s = String::new();
        for (n, v) in node.attributes().iter() {
            s.push(' ');
            s.push_str(n);
            s.push_str("=\"");
            // Java: escapeHtml(node.getAttributes().get(n)). escapeHtml returns
            // null for null/empty input (XhtmlComposer.java:249-251), and
            // `StringBuilder.append((String)null)` prints "null". So an empty
            // or null attribute value serializes as the literal `null`.
            let raw = v.as_deref();
            match escape_html(raw, self.cfg.canonical) {
                Some(e) => s.push_str(&e),
                None => s.push_str("null"),
            }
            s.push('"');
        }
        s
    }

    /// Java `writeElement` (XhtmlComposer.java:313).
    fn write_element(&mut self, indent: &str, node: &XhtmlNode, no_pretty_override: bool) {
        let cfg = self.cfg;
        // Java reassigns `indent = ""` when not pretty / overridden (line 314).
        let indent: &str = if !cfg.pretty || no_pretty_override {
            ""
        } else {
            indent
        };

        let mut concise = false;
        if !node.has_children() && !node.has_content() {
            if cfg.xml {
                concise = true;
            } else if !(node.has_empty_expanded() && node.empty_expanded() == Some(true))
                && is_void_element(node.name().unwrap_or(""))
            {
                concise = true;
            }
        }

        let name = node.name().unwrap_or("");
        let attrs = self.attributes(node);

        if concise {
            self.dst.push_str(indent);
            self.dst.push('<');
            self.dst.push_str(name);
            self.dst.push_str(&attrs);
            self.dst.push_str("/>");
            if cfg.pretty && !no_pretty_override {
                self.dst.push_str("\r\n");
            }
        } else {
            let act = node.all_children_are_text();
            // opening tag
            self.dst.push_str(indent);
            self.dst.push('<');
            self.dst.push_str(name);
            self.dst.push_str(&attrs);
            self.dst.push('>');
            if !(act || !cfg.pretty || no_pretty_override) {
                self.dst.push_str("\r\n");
            }

            // head/meta special case (XhtmlComposer.java:337).
            if name == "head" && node.get_element("meta").is_none() {
                self.dst.push_str(indent);
                self.dst.push_str(
                    "  <meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\"/>",
                );
                if cfg.pretty && !no_pretty_override {
                    self.dst.push_str("\r\n");
                }
            }

            if act && (name == "script" || name == "style") {
                self.dst.push_str(&node.all_text());
            } else {
                let child_indent = format!("{}  ", indent);
                let child_override = no_pretty_override || node.is_no_pretty();
                for c in node.child_nodes() {
                    self.write_node(&child_indent, c, child_override);
                }
            }

            // closing tag (XhtmlComposer.java:346-351).
            if act {
                self.dst.push_str("</");
                self.dst.push_str(name);
                self.dst.push('>');
                if cfg.pretty && !no_pretty_override {
                    self.dst.push_str("\r\n");
                }
            } else if node
                .child_nodes()
                .last()
                .map(|c| c.node_type() == NodeType::Text)
                .unwrap_or(false)
            {
                if cfg.pretty && !no_pretty_override {
                    self.dst.push_str("\r\n");
                    self.dst.push_str(indent);
                }
                self.dst.push_str("</");
                self.dst.push_str(name);
                self.dst.push('>');
                if cfg.pretty && !no_pretty_override {
                    self.dst.push_str("\r\n");
                }
            } else {
                self.dst.push_str(indent);
                self.dst.push_str("</");
                self.dst.push_str(name);
                self.dst.push('>');
                if cfg.pretty && !no_pretty_override {
                    self.dst.push_str("\r\n");
                }
            }
        }
    }

    /// Java `writeDocument` (XhtmlComposer.java:355): indent forced to "".
    fn write_document(&mut self, node: &XhtmlNode) {
        for c in node.child_nodes() {
            self.write_node("", c, false);
        }
    }
}

/// Java `isValidUrlChar` (XhtmlComposer.java:172).
fn is_valid_url_char(c: char) -> bool {
    c.is_alphabetic()
        || c.is_ascii_digit()
        || matches!(
            c,
            ';' | ','
                | '/'
                | '?'
                | ':'
                | '@'
                | '&'
                | '='
                | '+'
                | '$'
                | '-'
                | '_'
                | '.'
                | '!'
                | '~'
                | '*'
                | '\''
                | '('
                | ')'
        )
}

/// Java `escapeHtml` (XhtmlComposer.java:249). Returns None for null/empty
/// (Java returns Java-null), matching the `null` literal handling in
/// `attributes`.
fn escape_html(s: Option<&str>, canonical: bool) -> Option<String> {
    let s = s?;
    if s.is_empty() {
        return None;
    }
    let mut b = String::new();
    for c in s.chars() {
        if canonical {
            match c {
                '<' => b.push_str("&lt;"),
                '"' => b.push_str("&quot;"),
                '&' => b.push_str("&amp;"),
                '\t' => b.push_str("#x9;"),
                '\n' => b.push_str("#xA;"),
                '\r' => b.push_str("#xD;"),
                _ => b.push(c),
            }
        } else {
            match c {
                '<' => b.push_str("&lt;"),
                '>' => b.push_str("&gt;"),
                '"' => b.push_str("&quot;"),
                '&' => b.push_str("&amp;"),
                _ => b.push(c),
            }
        }
    }
    Some(b)
}

/// Java `Utilities.escapeXml` used by the auto-link path (XhtmlComposer.java:212).
/// Utilities.escapeXml escapes &, <, >, ", and control-ish chars; for URL text
/// the relevant characters are &, <, >, ". Ported minimally.
fn escape_xml(s: &str) -> String {
    let mut b = String::new();
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

/// The HTML5 void-element set (XhtmlComposer.java:322).
fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "command"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "menuitem"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Java `BLOCK_NAMES` (XhtmlComposer.java:49).
fn is_block_name(name: &str) -> bool {
    matches!(
        name,
        "li" | "ul" | "ol" | "tr" | "td" | "th" | "div" | "table"
    )
}

/// Java `breakBlocksWithLines(XhtmlNode)` (XhtmlComposer.java:86).
fn break_blocks_with_lines_node(node: &mut XhtmlNode) {
    if node.has_children() {
        break_blocks_with_lines_list(node.child_nodes_mut());
    }
}

/// Java `breakBlocksWithLines(List<XhtmlNode>)` (XhtmlComposer.java:92).
///
/// Faithful port: Java captures `node = list.get(i)` (the block) BEFORE any
/// insert, then always recurses into THAT node (line 101). So the sibling-insert
/// at `i` never diverts the recursion onto the freshly-inserted text node. We
/// reproduce this by recursing into `list[i]` (the block) FIRST — recursion
/// touches only the block's children, which the sibling insert does not affect —
/// and only THEN inserting the `\r\n` sibling before it.
fn break_blocks_with_lines_list(list: &mut Vec<XhtmlNode>) {
    // Java: for (i = size-1; i > 0; i--). i ranges over len-1..=1.
    let mut i = list.len();
    while i > 1 {
        i -= 1;
        // Recurse into the current node (the captured `node` in Java) first.
        break_blocks_with_lines_node(&mut list[i]);
        let is_block =
            list[i].node_type() == NodeType::Element && is_block_name(list[i].name().unwrap_or(""));
        if is_block {
            let prev = &list[i - 1];
            let prev_ends_nl = prev.node_type() == NodeType::Text
                && prev
                    .content()
                    .map(|c| c.ends_with('\r') || c.ends_with('\n'))
                    .unwrap_or(false);
            let prev_is_text = prev.node_type() == NodeType::Text;
            // Java condition: prev not Text, or content null, or not ending in \r/\n
            if !prev_is_text || prev.content().is_none() || !prev_ends_nl {
                let mut t = XhtmlNode::new(NodeType::Text);
                t.set_content("\r\n");
                list.insert(i, t);
            }
        }
    }
}

