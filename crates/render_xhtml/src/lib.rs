//! `render_xhtml` — a byte-exact Rust port of fhir-core's
//! `org.hl7.fhir.utilities.xhtml` XhtmlNode / XhtmlParser / XhtmlComposer.
//!
//! This is the C3 substrate for the stock-template renderer (see
//! docs/stock-template-renderer-plan.md, phase F1a). Its job is byte-parity:
//! parse a Publisher-emitted xhtml fragment and re-serialize it to identical
//! bytes.
//!
//! See `node.rs` for the one deliberate divergence from Java (insertion-ordered
//! attributes) and why it is required for the round-trip gate.

pub mod composer;
pub mod entities;
pub mod node;
pub mod ordermap;
pub mod parser;

pub use composer::{Config, XhtmlComposer};
pub use node::{NodeType, XhtmlNode};
pub use parser::{ParseError, XhtmlParser};

/// Parse a fragment's inner content (the top-level node sequence, e.g. the
/// content of a `_includes/*.xhtml` file after the `{% raw %}` wrapper is
/// stripped) and re-compose it with `Config::xml_compact()` — the config the
/// round-trip gate uses. For a well-formed Publisher fragment this returns
/// bytes identical to the input.
pub fn roundtrip_fragment(source: &str) -> Result<String, ParseError> {
    let mut p = XhtmlParser::new();
    let children = p.parse_fragment_children(source)?;
    let mut c = XhtmlComposer::new(Config::xml_compact());
    Ok(c.compose_nodes(&children))
}

/// Round-trip a fragment's inner content, trying each composer configuration a
/// publisher renderer might have used, and return the recomposed bytes for the
/// FIRST config that reproduces the input exactly (plus which config matched).
///
/// Fragments are raw strings produced by many different renderers, each with
/// its own `new XhtmlComposer(...)` flags (see composer.rs docs). A fragment
/// carries no marker of the mode it was composed in, so the byte-parity gate
/// tries the observed configs in order:
///   1. XML compact  — `XhtmlComposer(XML)`  (dominant for -html/table frags)
///   2. HTML compact — `XhtmlComposer(false)` (narrative/-html/status frags:
///      keeps `&copy;`, does not escape `"`, keeps empty `<td></td>`/`<a></a>`)
///   3. HTML pretty  — `XhtmlComposer(false, true)`
/// If none reproduces the input, returns the XML-compact output for diffing.
pub fn roundtrip_fragment_multi(
    source: &str,
) -> Result<(String, Option<&'static str>), ParseError> {
    let configs: [(Config, &'static str); 3] = [
        (Config::xml_compact(), "xml-compact"),
        (Config::html_compact(), "html-compact"),
        (Config::html_pretty(), "html-pretty"),
    ];
    let mut first_out: Option<String> = None;
    for (cfg, label) in configs {
        let mut p = XhtmlParser::new();
        let children = p.parse_fragment_children(source)?;
        let mut c = XhtmlComposer::new(cfg);
        let out = c.compose_nodes(&children);
        if out == source {
            return Ok((out, Some(label)));
        }
        if first_out.is_none() {
            first_out = Some(out);
        }
    }
    Ok((first_out.unwrap_or_default(), None))
}

/// Fragment basename classes that fhir-core's OWN XhtmlParser+XhtmlComposer
/// also cannot round-trip byte-exact (verified with a Java oracle against
/// fhir-core 6.9.10-SNAPSHOT over the cycle+plan-net corpora: for every one of
/// these, no `XhtmlComposer` config reproduces the golden, and several throw in
/// the parser). They are hand-assembled RAW STRINGS (syntax-highlighted
/// json/xml/ttl dumps, pseudo-* templates, narrative/summary tables that mix
/// XML-style self-close with HTML-style expanded empties, StatusRenderer's
/// malformed `class="` tables, and ant-injected `<!--$$N$$-->` page
/// aggregates) that never passed through the composer. A non-parity file is a
/// real regression ONLY if it is NOT in this documented set.
pub fn is_known_non_roundtrippable(basename: &str) -> bool {
    let stem = basename.trim_end_matches(".xhtml");
    const BASENAMES: &[&str] = &[
        "ip-statements",
        "globals-table",
        "dependency-table",
        "dependency-table-short",
        "dependency-table-nontech",
        "cross-version-analysis",
        "cross-version-analysis-inline",
    ];
    if BASENAMES.contains(&stem) {
        return true;
    }
    if stem.starts_with("list-") || stem.starts_with("table-") {
        return true;
    }
    const SUFFIXES: &[&str] = &[
        "-json-html",
        "-xml-html",
        "-ttl-html",
        "-html",
        "-pseudo-json",
        "-pseudo-xml",
        "-pseudo-ttl",
        "-summary",
        "-summary-all",
        "-summary-table",
        "-header",
        "-status",
        "-validate",
    ];
    SUFFIXES.iter().any(|s| stem.ends_with(s))
}

/// The publisher wraps every fragment in `{% raw %}...{% endraw %}`
/// (PublisherGenerator.java:6510-6512). Strip that wrapper to recover the inner
/// xhtml. Returns `None` if the wrapper is absent.
pub fn strip_raw_wrapper(file_content: &str) -> Option<&str> {
    const PREFIX: &str = "{% raw %}";
    const SUFFIX: &str = "{% endraw %}";
    let inner = file_content.strip_prefix(PREFIX)?;
    // Use rfind for the suffix: fragment content itself never contains the
    // liquid endraw token (it is inside {% raw %}), and the publisher appends it
    // once at the very end.
    let end = inner.rfind(SUFFIX)?;
    Some(&inner[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(inner: &str) -> String {
        // Wrap the inner content in a fragment root the way the publisher does
        // implicitly: parseFragment reads (and discards) the outer tag, then
        // parses children. We give it a <div> wrapper so the composed output is
        // the wrapper + inner; for asserting on inner alone we parse the exact
        // string.
        roundtrip_fragment(inner).unwrap()
    }

    #[test]
    fn simple_element_roundtrips() {
        let s = "<div>hello</div>";
        assert_eq!(rt(s), s);
    }

    #[test]
    fn attribute_order_is_preserved() {
        // The decisive case: raw-string attribute order must survive
        // parse->compose (Java's HashMap would reorder these). Uses a
        // non-empty element so XML mode does not collapse it to self-close.
        let s = "<pre class=\"xml\" data-fhir=\"generated\" style=\"white-space: pre;\">x</pre>";
        assert_eq!(rt(s), s);
    }

    #[test]
    fn empty_element_collapses_to_self_close_in_xml() {
        // In XML mode the composer emits `<pre/>` for an empty element
        // (XhtmlComposer.java:318-319 concise=true when !hasChildren &&
        // !hasContent). This is faithful to Java, so `<pre></pre>` -> `<pre/>`.
        let mut p = XhtmlParser::new();
        let kids = p.parse_fragment_children("<pre></pre>").unwrap();
        let mut c = XhtmlComposer::new(Config::xml_compact());
        assert_eq!(c.compose_nodes(&kids), "<pre/>");
    }

    #[test]
    fn nested_and_text() {
        let s = "<div><b>a</b> and <i>b</i></div>";
        assert_eq!(rt(s), s);
    }

    #[test]
    fn entities_escape_on_output() {
        // `<`, `>`, `&`, and `"` (xml mode) are escaped by the composer.
        let s = "<code>&lt;a&gt; &amp; &quot;b&quot;</code>";
        assert_eq!(rt(s), s);
    }

    #[test]
    fn nbsp_entity_roundtrips_in_xml_mode() {
        // In XML mode NBSP (U+00A0) is emitted literally, NOT as &nbsp;.
        // Parsing &nbsp; yields U+00A0; composing in xml mode emits the literal
        // char. So a golden that used the literal NBSP round-trips; one that
        // wrote &nbsp; would not (documented: XML-mode composers emit literal).
        let mut p = XhtmlParser::new();
        let node = p.parse_fragment("<span>&nbsp;</span>").unwrap();
        let mut c = XhtmlComposer::new(Config::xml_compact());
        assert_eq!(c.compose_node(&node), "<span>\u{A0}</span>");
    }

    #[test]
    fn empty_element_self_closes_in_xml() {
        let s = "<br/>";
        assert_eq!(rt(s), s);
    }

    #[test]
    fn comment_inside_element_gets_two_space_indent() {
        // Faithful Java quirk: even in non-pretty mode, child nodes are written
        // with indent = parentIndent+"  " (XhtmlComposer.java:344), and
        // writeComment prepends that indent unconditionally
        // (XhtmlComposer.java:290). So a comment child gains a "  " prefix.
        // Input has NO literal whitespace around the comment; the two spaces in
        // the output come purely from the composer's child indent.
        let s = "<div><!-- hi --></div>";
        assert_eq!(rt(s), "<div>  <!-- hi --></div>");
    }

    #[test]
    fn unknown_element_accepted() {
        let s = "<div><custom-tag foo=\"bar\">x</custom-tag></div>";
        assert_eq!(rt(s), s);
    }
}
