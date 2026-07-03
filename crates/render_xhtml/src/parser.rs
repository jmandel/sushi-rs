//! Port of `org.hl7.fhir.utilities.xhtml.XhtmlParser` (the lenient, text-based
//! parser path — the DOM/XmlPullParser paths are out of scope).
//!
//! Source of truth (READ-ONLY, cited as XhtmlParser.java:<line>):
//!   fhir-core/org.hl7.fhir.utilities/src/main/java/org/hl7/fhir/utilities/xhtml/XhtmlParser.java
//!
//! ## Lenience
//!
//! Configured as `new XhtmlParser()` sets `policy = Accept` (XhtmlParser.java:303),
//! so unknown ELEMENTS and ATTRIBUTES are accepted (`elementIsOk`/`attributeIsOk`
//! return true for Accept, XhtmlParser.java:445, 465-473). Because Accept keeps
//! everything, we do NOT need the ELEMENTS/ATTRIBUTES whitelist tables at all for
//! parsing: every element and attribute is kept. `mustBeWellFormed` defaults to
//! true (XhtmlParser.java:315); the fragment entry point `parseFragment` runs the
//! same `parseElementInner` loop, which for well-formed golden input never hits
//! the unwind path.
//!
//! The parser is entity-aware (numeric refs, declared DOCTYPE entities, the big
//! HTML5 named-entity table, plus the hardcoded fallbacks), handles comments,
//! CDATA, processing instructions/doctype (in the document path), and unknown
//! elements. On an unrecognized `&...;` with `!mustBeWellFormed` it treats it as
//! an accidental literal `&`; with `mustBeWellFormed` it errors.

use crate::entities::defined_entity;
use crate::node::{NodeType, XhtmlNode, NBSP};

const END_OF_CHARS: char = '\u{FFFF}'; // Java uses (char)-1 == 0xFFFF sentinel.

#[derive(Debug)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for ParseError {}

type PResult<T> = Result<T, ParseError>;

pub struct XhtmlParser {
    /// Remaining input as a char buffer. Java uses a Reader + a `cache` String
    /// for pushback; we hold the whole input in a Vec<char> with a cursor plus
    /// a small pushback stack, which is behaviorally identical for our inputs.
    chars: Vec<char>,
    pos: usize,
    /// Pushback characters (Java `pushChar` prepends to `cache`).
    pushback: Vec<char>,
    trim_whitespace: bool,
    must_be_well_formed: bool,
    xml_mode: bool,
    last_text: String,
    declared_entities: std::collections::HashMap<String, String>,
}

impl Default for XhtmlParser {
    fn default() -> Self {
        Self::new()
    }
}

impl XhtmlParser {
    /// Java `new XhtmlParser()` (XhtmlParser.java:301): policy=Accept,
    /// trimWhitespace=false, mustBeWellFormed=true, xmlMode=false.
    pub fn new() -> Self {
        XhtmlParser {
            chars: Vec::new(),
            pos: 0,
            pushback: Vec::new(),
            trim_whitespace: false,
            must_be_well_formed: true,
            xml_mode: false,
            last_text: String::new(),
            declared_entities: std::collections::HashMap::new(),
        }
    }

    pub fn set_trim_whitespace(&mut self, v: bool) -> &mut Self {
        self.trim_whitespace = v;
        self
    }

    pub fn set_must_be_well_formed(&mut self, v: bool) -> &mut Self {
        self.must_be_well_formed = v;
        self
    }

    pub fn set_xml_mode(&mut self, v: bool) -> &mut Self {
        self.xml_mode = v;
        self
    }

    fn load(&mut self, source: &str) {
        self.chars = source.chars().collect();
        self.pos = 0;
        self.pushback.clear();
    }

    // --- char reader (XhtmlParser.java:811-855) ---

    fn peek_char(&self) -> char {
        if let Some(c) = self.pushback.last() {
            *c
        } else if self.pos < self.chars.len() {
            self.chars[self.pos]
        } else {
            END_OF_CHARS
        }
    }

    fn read_char(&mut self) -> char {
        if let Some(c) = self.pushback.pop() {
            c
        } else if self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            self.pos += 1;
            c
        } else {
            END_OF_CHARS
        }
    }

    fn push_char(&mut self, ch: char) {
        // Java prepends to `cache`; our pushback is a LIFO whose top is the next
        // char, so a plain push is correct.
        self.pushback.push(ch);
    }

    // --- public entry points ---

    /// Java `parseFragment(String)` -> `parseFragment()` (XhtmlParser.java:1351,
    /// 1368). This is the dominant path for `_includes/*.xhtml`: the outer tag's
    /// attributes are read and DISCARDED (`readToTagEnd`), a namespace prefix on
    /// the root name is stripped, and the inner content is parsed leniently.
    pub fn parse_fragment(&mut self, source: &str) -> PResult<XhtmlNode> {
        self.load(source);
        self.declared_entities.clear();
        self.skip_white_space();
        if self.peek_char() != '<' {
            return Err(ParseError(format!(
                "Unable to Parse HTML - does not start with tag. Found {}",
                self.peek_char()
            )));
        }
        self.read_char();
        if self.peek_char() == '?' {
            self.read_to_tag_end()?;
            self.skip_white_space_internal();
            if self.peek_char() != '<' {
                return Err(ParseError(
                    "Unable to Parse HTML - does not start with tag after processing instruction"
                        .to_string(),
                ));
            }
            self.read_char();
        }
        let mut n = self.read_name().to_lowercase();
        self.read_to_tag_end()?;
        let mut result = XhtmlNode::new(NodeType::Element);
        if let Some(i) = n.find(':') {
            n = n[i + 1..].to_string();
        }
        result.set_name(n);
        let mut parents: Vec<XhtmlNode> = Vec::new();
        let mut unwind: Option<usize> = None;
        self.parse_element_inner(&mut result, &mut parents, &mut unwind)?;
        Ok(result)
    }

    /// Parse a fragment's INNER content — the sequence of top-level nodes — by
    /// treating `source` as the children of a synthetic root, mirroring Java's
    /// `parseMDFragment`, which does `parseFragment("<div>"+source+"</div>")`
    /// then `.getChildNodes()` (XhtmlParser.java:1337-1339). Unlike
    /// `parse_fragment`, this PRESERVES the attributes of the top-level
    /// element(s), which is what the byte round-trip gate requires (the
    /// `_includes/*.xhtml` content is exactly this inner node sequence).
    pub fn parse_fragment_children(&mut self, source: &str) -> PResult<Vec<XhtmlNode>> {
        let wrapped = format!("<div>{}</div>", source);
        let root = self.parse_fragment(&wrapped)?;
        Ok(root.child_nodes().to_vec())
    }

    // --- inner parsing (XhtmlParser.java:602-674) ---

    fn parse_element_inner(
        &mut self,
        node: &mut XhtmlNode,
        parents: &mut Vec<XhtmlNode>,
        unwind: &mut Option<usize>,
    ) -> PResult<()> {
        // `unwind` holds a *depth marker* into `parents` when set. In the
        // well-formed path (mustBeWellFormed=true) it is never set, so the loop
        // terminates on the matching close tag via the early `return`. We model
        // the unwind trigger conservatively: if a mismatched close tag is seen
        // and mustBeWellFormed is true we error (matching Java); the full
        // reparent-on-unwind machinery of the !wellFormed path is not needed for
        // the golden corpus and is documented as a known gap below.
        let mut s = String::new();
        while self.peek_char() != END_OF_CHARS && unwind.is_none() {
            let c = self.peek_char();
            if c == '<' {
                self.add_text_node(node, &mut s);
                self.read_char();
                let p = self.peek_char();
                if p == '!' {
                    self.read_char();
                    if self.peek_char() == '[' {
                        let sc = self.read_cdata()?;
                        node.add_cdata(sc);
                    } else {
                        self.push_char('!');
                        let sc = self.read_to_comment_end(true)?;
                        node.add_comment(sc);
                    }
                } else if p == '?' {
                    let sc = self.read_to_tag_end()?;
                    node.add_comment(sc);
                } else if p == '/' {
                    self.read_char();
                    let n_full = self.read_to_tag_end()?;
                    let n = element_name(&n_full);
                    if node.name() == Some(n.as_str()) {
                        return Ok(());
                    } else if self.must_be_well_formed {
                        return Err(ParseError(format!(
                            "Malformed XHTML: Found \"</{}>\" expecting \"</{}>\"",
                            n,
                            node.name().unwrap_or("")
                        )));
                    } else {
                        // !wellFormed unwind path — see known-gap note above.
                        // Best-effort: scan parents for a matching open tag; if
                        // found, signal unwind to that depth so ancestors close.
                        for (i, par) in parents.iter().enumerate() {
                            if par.name() == Some(n.as_str()) {
                                *unwind = Some(i);
                            }
                        }
                        if unwind.is_some() {
                            return Ok(());
                        }
                    }
                } else if p.is_alphanumeric() {
                    self.parse_element(node, parents, unwind)?;
                } else {
                    return Err(ParseError(format!(
                        "Unable to Parse HTML - node '{}' has unexpected content '{}' (last text = '{}')",
                        node.name().unwrap_or(""),
                        p,
                        self.last_text
                    )));
                }
            } else if c == '&' {
                self.parse_literal(&mut s)?;
            } else {
                s.push(self.read_char());
            }
        }
        self.add_text_node(node, &mut s);
        Ok(())
    }

    /// Java `parseElement` (XhtmlParser.java:693). Note the `script` special
    /// case reads raw until `</script>`.
    fn parse_element(
        &mut self,
        parent: &mut XhtmlNode,
        parents: &mut Vec<XhtmlNode>,
        unwind: &mut Option<usize>,
    ) -> PResult<()> {
        let name_full = self.read_name();
        let name = element_name(&name_full);
        // Build the child node with makeTag semantics (sets notPretty for inline).
        let mut node = parent.add_tag(name.clone()).clone();
        // We assemble into a fresh node, then push. `add_tag` already pushed a
        // node; instead take that reference. To keep ownership simple, we mutate
        // the just-pushed node in place.
        // Remove the placeholder we just added and rebuild explicitly:
        parent.child_nodes_mut().pop();

        self.parse_attributes(&mut node)?;

        // Java: readChar() == '/'  => self-closing
        let ch = self.read_char();
        if ch == '/' {
            if self.peek_char() != '>' {
                return Err(ParseError(format!(
                    "unexpected non-end of element {}",
                    name
                )));
            }
            self.read_char();
            node.set_empty_expanded(false);
            parent.add_child_node(node);
        } else if name == "script" {
            self.parse_script_inner(&mut node)?;
            parent.add_child_node(node);
        } else {
            node.set_empty_expanded(true);
            // newParents = parents + parent (Java line 699-701). Java passes a
            // COPY of `parent` at its current state; children accumulate into
            // `node`, then `node` is added to `parent`. We push a snapshot of
            // `parent` as an ancestor marker (only its NAME is consulted on the
            // unwind path).
            let mut new_parents = parents.clone();
            let mut parent_marker = XhtmlNode::new(NodeType::Element);
            if let Some(nm) = parent.name() {
                parent_marker.set_name(nm.to_string());
            }
            new_parents.push(parent_marker);
            self.parse_element_inner(&mut node, &mut new_parents, unwind)?;
            parent.add_child_node(node);
        }
        Ok(())
    }

    /// Java `parseScriptInner` (XhtmlParser.java:677): read raw until the buffer
    /// ends with `</script>`, then strip that 9-char suffix.
    fn parse_script_inner(&mut self, node: &mut XhtmlNode) -> PResult<()> {
        let mut s = String::new();
        while self.peek_char() != END_OF_CHARS && !s.ends_with("</script>") {
            s.push(self.read_char());
        }
        let mut ss = s;
        if ss.chars().count() >= 9 {
            // strip trailing "</script>" (9 chars)
            let total = ss.chars().count();
            ss = ss.chars().take(total - 9).collect();
        }
        let t = if self.trim_whitespace {
            ss.trim().to_string()
        } else {
            ss
        };
        if !t.is_empty() {
            self.last_text = t.clone();
            node.add_text(t);
        }
        Ok(())
    }

    /// Java `parseAttributes` (XhtmlParser.java:718).
    fn parse_attributes(&mut self, node: &mut XhtmlNode) -> PResult<()> {
        while is_java_whitespace(self.peek_char()) {
            self.read_char();
        }
        while self.peek_char() != '>'
            && self.peek_char() != '/'
            && self.peek_char() != END_OF_CHARS
        {
            let name = self.read_name();
            if name.is_empty() {
                return Err(ParseError(format!(
                    "Unable to read attribute on <{}>",
                    node.name().unwrap_or("")
                )));
            }
            while is_java_whitespace(self.peek_char()) {
                self.read_char();
            }
            let pc = self.peek_char();
            if is_name_char(pc) || pc == '>' || pc == '/' {
                // value-less attribute -> null (Java line 733).
                node.put_attribute_null(name);
            } else if pc != '=' {
                return Err(ParseError(format!(
                    "Unable to read attribute '{}' value on <{}>",
                    name,
                    node.name().unwrap_or("")
                )));
            } else {
                self.read_char();
                while is_java_whitespace(self.peek_char()) {
                    self.read_char();
                }
                let pc2 = self.peek_char();
                if pc2 == '"' || pc2 == '\'' {
                    let term = self.read_char();
                    let v = self.parse_attribute_value(term)?;
                    node.put_attribute_opt(name, Some(v));
                } else {
                    let v = self.parse_attribute_value(END_OF_CHARS)?;
                    node.put_attribute_opt(name, Some(v));
                }
            }
            while is_java_whitespace(self.peek_char()) {
                self.read_char();
            }
        }
        Ok(())
    }

    /// Java `parseAttributeValue` (XhtmlParser.java:753).
    fn parse_attribute_value(&mut self, term: char) -> PResult<String> {
        let mut b = String::new();
        while self.peek_char() != END_OF_CHARS
            && self.peek_char() != '>'
            && (term != END_OF_CHARS || self.peek_char() != '/')
            && self.peek_char() != term
        {
            if self.peek_char() == '&' {
                self.parse_literal(&mut b)?;
            } else {
                b.push(self.read_char());
            }
        }
        if self.peek_char() == term {
            self.read_char();
        }
        Ok(b)
    }

    /// Java `addTextNode` (XhtmlParser.java:593).
    fn add_text_node(&mut self, node: &mut XhtmlNode, s: &mut String) {
        let t = if self.trim_whitespace {
            s.trim().to_string()
        } else {
            s.clone()
        };
        if !t.is_empty() {
            self.last_text = t.clone();
            node.add_text(t);
            s.clear();
        }
    }

    /// Java `skipWhiteSpace` (XhtmlParser.java:800): only skips if trimWhitespace.
    fn skip_white_space(&mut self) {
        if self.trim_whitespace {
            while is_java_whitespace(self.peek_char()) || self.peek_char() == '\u{feff}' {
                self.read_char();
            }
        }
    }

    /// Java `skipWhiteSpaceInternal` (XhtmlParser.java:806).
    fn skip_white_space_internal(&mut self) {
        while is_java_whitespace(self.peek_char()) || self.peek_char() == '\u{feff}' {
            self.read_char();
        }
    }

    /// Java `readToTagEnd` (XhtmlParser.java:857).
    fn read_to_tag_end(&mut self) -> PResult<String> {
        let mut s = String::new();
        while self.peek_char() != '>' && self.peek_char() != END_OF_CHARS {
            s.push(self.read_char());
        }
        if self.peek_char() != END_OF_CHARS {
            self.read_char();
            self.skip_white_space();
        } else if self.must_be_well_formed {
            return Err(ParseError("Unexpected termination of html source".to_string()));
        }
        Ok(s)
    }

    /// Java `readToCommentEnd` (XhtmlParser.java:889).
    fn read_to_comment_end(&mut self, mut simple: bool) -> PResult<String> {
        if self.peek_char() == '!' {
            self.read_char();
        }
        let mut s = String::new();
        if self.peek_char() == '-' {
            self.read_char();
            simple = self.peek_char() != '-';
            if simple {
                s.push('-');
            } else {
                self.read_char();
            }
        }

        let mut doctype_entities = false;
        let mut done = false;
        while !done {
            let c = self.peek_char();
            if c == '-' {
                self.read_char();
                if self.peek_char() == '-' {
                    self.read_char();
                    if self.peek_char() == '>' {
                        done = true;
                    } else {
                        self.push_char('-');
                        s.push('-');
                    }
                } else {
                    s.push('-');
                }
            } else if doctype_entities && c == ']' {
                s.push(self.read_char());
                if self.peek_char() == '>' {
                    done = true;
                }
            } else if simple && self.peek_char() == '>' && !doctype_entities {
                done = true;
            } else if c == '[' && s.starts_with("DOCTYPE ") {
                doctype_entities = true;
                s.push(self.read_char());
            } else if c != END_OF_CHARS {
                s.push(self.read_char());
            } else if self.must_be_well_formed {
                return Err(ParseError("Unexpected termination of html source".to_string()));
            } else {
                // Java loop would spin; break to avoid infinite loop at EOF.
                break;
            }
        }
        if self.peek_char() != END_OF_CHARS {
            self.read_char();
            self.skip_white_space();
        }
        if doctype_entities {
            self.parse_doctype_entities(&s);
        }
        Ok(s)
    }

    /// Java `readCData` (XhtmlParser.java:946): reads until `]]>`, returns the
    /// inner content (strips the leading `[CDATA[` (7 chars incl the `[` already
    /// consumed context) and trailing `]]>`).
    fn read_cdata(&mut self) -> PResult<String> {
        let mut b = String::new();
        loop {
            let c = self.read_char();
            if c == END_OF_CHARS {
                return Err(ParseError(
                    "Stream ended before finding ']]>' terminator".to_string(),
                ));
            }
            b.push(c);
            let len = b.chars().count();
            if c == '>' && len >= 3 {
                let cv: Vec<char> = b.chars().collect();
                if cv[len - 2] == ']' && cv[len - 3] == ']' {
                    break;
                }
            }
        }
        // Java: b.substring(7, b.length() - 3). At entry the buffer began after
        // '<' and '!' were consumed and peek was '['; the first read is '[', so
        // b starts with "[CDATA[" (7 chars) and ends with "]]>" (3 chars).
        let cv: Vec<char> = b.chars().collect();
        let inner: String = cv[7..cv.len() - 3].iter().collect();
        Ok(inner)
    }

    /// Java `parseDoctypeEntities` (XhtmlParser.java:964).
    fn parse_doctype_entities(&mut self, s: &str) {
        let mut s = s.to_string();
        while let Some(idx) = s.find("<!ENTITY") {
            s = s[idx..].to_string();
            let e = match s.find('>') {
                Some(e) => e,
                None => break,
            };
            let ed = s[..e + 1].to_string();
            s = s[e + 1..].to_string();
            let ed = ed[8..].trim().to_string(); // strip "<!ENTITY"
            let sp = match ed.find(' ') {
                Some(p) => p,
                None => break,
            };
            let n = ed[..sp].trim().to_string();
            let ed = ed[sp..].trim().to_string();
            // skip SYSTEM token
            let sp2 = match ed.find(' ') {
                Some(p) => p,
                None => break,
            };
            let ed = ed[sp2..].trim().to_string();
            if ed.is_empty() {
                break;
            }
            let v = ed[..ed.len() - 1].to_string(); // drop trailing '>'
            self.declared_entities.insert(n, v);
        }
    }

    /// Java `readName` (XhtmlParser.java:987).
    fn read_name(&mut self) -> String {
        let mut s = String::new();
        while is_name_char(self.peek_char()) {
            s.push(self.read_char());
        }
        s
    }

    /// Java `readUntil(String sc)` (XhtmlParser.java:1005): read while the next
    /// char is not in `sc` and not the `\0` sentinel, then consume the delimiter.
    fn read_until_any(&mut self, sc: &str) -> String {
        let mut s = String::new();
        // Java condition: peekChar() != 0 && sc.indexOf(peekChar()) == -1.
        // Note: this is '\0', NOT END_OF_CHARS. At real EOF peek is END_OF_CHARS
        // (0xFFFF), which is not '\0', so Java would loop; we also stop at EOF
        // to avoid spinning (documented divergence, unreachable for goldens).
        while self.peek_char() != '\u{0}'
            && self.peek_char() != END_OF_CHARS
            && !sc.contains(self.peek_char())
        {
            s.push(self.read_char());
        }
        self.read_char(); // consume the delimiter (Java always readChar())
        s
    }

    /// Java `parseLiteral` (XhtmlParser.java:1014): entity/char-reference decode.
    fn parse_literal(&mut self, s: &mut String) -> PResult<()> {
        self.read_char(); // consume '&'
        let c = self.read_until_any(";&'\"><");
        if c.is_empty() {
            return Err(ParseError(format!(
                "Invalid literal declaration following text: {}",
                s
            )));
        }
        let first = c.chars().next().unwrap();
        if first == '#' {
            let rest: String = c.chars().skip(1).collect();
            if let Ok(n) = i64::from_str_radix(&rest, 10) {
                push_code_point(s, n);
            } else if c.chars().nth(1) == Some('x') {
                let hex: String = c.chars().skip(2).collect();
                if let Ok(n) = i64::from_str_radix(&hex, 16) {
                    push_code_point(s, n);
                }
            }
            // (Java: if neither parses, nothing is appended.)
        } else if let Some(v) = self.declared_entities.get(&c).cloned() {
            s.push_str(&v);
        } else {
            if self.xml_mode
                && !matches!(c.as_str(), "quot" | "amp" | "apos" | "lt" | "gt")
            {
                // Java records a validation issue; we ignore (Accept policy).
            }
            let token = format!("&{};", c);
            if let Some(v) = defined_entity(&token) {
                s.push_str(v);
            } else {
                // Hardcoded fallbacks (XhtmlParser.java:1039-1318). These are
                // mostly redundant with DEFINED_ENTITIES, but a few keys are NOT
                // in the table and MUST be handled here for parity:
                match c.as_str() {
                    "apos" => s.push('\''),
                    "quot" => s.push('"'),
                    "nbsp" => s.push(NBSP),
                    "amp" => s.push('&'),
                    "lsquo" => s.push('\u{2018}'),
                    "rsquo" => s.push('\u{2019}'),
                    "gt" => s.push('>'),
                    "lt" => s.push('<'),
                    "copy" => s.push('\u{A9}'),
                    "reg" => s.push('\u{AE}'),
                    "sect" => s.push('\u{A7}'),
                    // The Greek/math/arrow block (1061-1318) is fully covered by
                    // DEFINED_ENTITIES; if we reach here for one of those, the
                    // table lookup above already handled it. Remaining unknown:
                    _ => {
                        if !self.must_be_well_formed {
                            // Guess an accidentally unescaped '&' (Java 1319-1321).
                            s.push('&');
                            s.push_str(&c);
                        } else {
                            return Err(ParseError(format!(
                                "unable to parse character reference '{}' (last text = '{}')",
                                c, self.last_text
                            )));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Java `ElementName` (XhtmlParser.java:134): strip a `ns:` prefix, keep local.
fn element_name(src: &str) -> String {
    match src.find(':') {
        Some(i) => src[i + 1..].to_string(),
        None => src.to_string(),
    }
}

/// Push a Unicode code point from an integer, matching Java's
/// `Character.toString(int)` (used by parseLiteral numeric refs). Java accepts
/// the full code-point range; invalid values are dropped here (Java would throw
/// on an out-of-range code point, which never happens for valid golden input).
fn push_code_point(s: &mut String, n: i64) {
    if let Ok(u) = u32::try_from(n) {
        if let Some(ch) = char::from_u32(u) {
            s.push(ch);
        }
    }
}

/// Java `isNameChar` (XhtmlParser.java:982).
fn is_name_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == ':' || ch == '.'
}

/// Java `Character.isWhitespace` — the parser calls it in several loops. Java's
/// definition includes space, tab, LF, CR, FF, and the Unicode space
/// separators (excluding NBSP). The characters that actually appear in the
/// corpus are ASCII whitespace; we approximate with Rust's `is_whitespace`
/// MINUS NBSP (Java's isWhitespace excludes U+00A0), which matches on the
/// relevant inputs.
fn is_java_whitespace(ch: char) -> bool {
    if ch == END_OF_CHARS {
        return false;
    }
    if ch == '\u{A0}' || ch == '\u{2007}' || ch == '\u{202F}' {
        // Java isWhitespace excludes the non-breaking spaces.
        return false;
    }
    ch.is_whitespace()
}
