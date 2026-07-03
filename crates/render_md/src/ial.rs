//! Inline Attribute Lists (IAL) — kramdown `{: .class #id key="val" ref}`.
//!
//! Two placements matter for the corpus:
//!  * **Block IAL**: a line whose entire content is `{: ... }`, sitting on the
//!    line immediately after a block. It attaches to the preceding block.
//!    (This is how the corpus attaches classes to tables/paragraphs and
//!    `{:.no_toc}`/`{:toc}` to lists/headings.)
//!  * **Header IAL**: kramdown also accepts a block IAL on the line after an
//!    ATX header; handled the same way (attach to preceding header).
//!
//! Span IALs (`text{: .c}` right after an inline span) exist in kramdown but
//! are essentially unused in the authored corpus; not implemented (documented
//! boundary).

/// Parsed IAL attributes.
///
/// kramdown stores attributes in an insertion-ordered Hash: `#id` sets the
/// `id` key; each `.class` appends to a `class` key (created at the first
/// class); `k="v"` sets key `k`. The HTML is emitted in that Hash order.
/// `ordered` records exactly that order so the renderer reproduces kramdown's
/// attribute sequence (e.g. `{:.no_toc #id}` -> `class="no_toc" id="id"`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attrs {
    /// Final attributes in kramdown emission order: (name, value).
    pub ordered: Vec<(String, String)>,
    /// Bare references (`{:ref}`) — e.g. `toc`, `no_toc`.
    pub refs: Vec<String>,
}

impl Attrs {
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty() && self.refs.is_empty()
    }

    pub fn has_ref(&self, name: &str) -> bool {
        self.refs.iter().any(|r| r == name)
    }

    pub fn id(&self) -> Option<&str> {
        self.ordered
            .iter()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.as_str())
    }

    /// Set the auto-generated id. kramdown appends the auto-id AFTER any IAL
    /// attributes already present (e.g. `class`), so we append when absent.
    pub fn set_id(&mut self, id: String) {
        if let Some(slot) = self.ordered.iter_mut().find(|(k, _)| k == "id") {
            slot.1 = id;
        } else {
            self.ordered.push(("id".to_string(), id));
        }
    }

    fn push_class(&mut self, cls: &str) {
        if let Some(slot) = self.ordered.iter_mut().find(|(k, _)| k == "class") {
            slot.1.push(' ');
            slot.1.push_str(cls);
        } else {
            self.ordered.push(("class".to_string(), cls.to_string()));
        }
    }

    fn set_kv(&mut self, k: &str, v: &str) {
        if let Some(slot) = self.ordered.iter_mut().find(|(kk, _)| kk == k) {
            slot.1 = v.to_string();
        } else {
            self.ordered.push((k.to_string(), v.to_string()));
        }
    }

    fn set_id_ordered(&mut self, id: &str) {
        if let Some(slot) = self.ordered.iter_mut().find(|(k, _)| k == "id") {
            slot.1 = id.to_string();
        } else {
            self.ordered.push(("id".to_string(), id.to_string()));
        }
    }
}

/// Parse the inside of an IAL (the text between `{:` and `}`), e.g.
/// `.no_toc #translations key="v"`.
pub fn parse_ial_body(body: &str) -> Attrs {
    let mut attrs = Attrs::default();
    let mut chars = body.char_indices().peekable();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    let n = body.len();
    // Manual scan to handle quoted values.
    let _ = (&mut chars, &bytes);
    while i < n {
        let c = body[i..].chars().next().unwrap();
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        match c {
            '.' => {
                i += 1;
                let start = i;
                while i < n {
                    let ch = body[i..].chars().next().unwrap();
                    if ch.is_whitespace() {
                        break;
                    }
                    i += ch.len_utf8();
                }
                let cls = &body[start..i];
                if !cls.is_empty() {
                    attrs.push_class(cls);
                }
            }
            '#' => {
                i += 1;
                let start = i;
                while i < n {
                    let ch = body[i..].chars().next().unwrap();
                    if ch.is_whitespace() {
                        break;
                    }
                    i += ch.len_utf8();
                }
                let id = &body[start..i];
                if !id.is_empty() {
                    attrs.set_id_ordered(id);
                }
            }
            _ => {
                // key or key="value" or bare ref
                let start = i;
                while i < n {
                    let ch = body[i..].chars().next().unwrap();
                    if ch.is_whitespace() || ch == '=' {
                        break;
                    }
                    i += ch.len_utf8();
                }
                let key = &body[start..i];
                // skip spaces
                while i < n && body[i..].chars().next().unwrap().is_whitespace() {
                    i += 1;
                }
                if i < n && body[i..].starts_with('=') {
                    i += 1; // consume '='
                    while i < n && body[i..].chars().next().unwrap().is_whitespace() {
                        i += 1;
                    }
                    let val = if i < n && (body[i..].starts_with('"') || body[i..].starts_with('\'')) {
                        let quote = body[i..].chars().next().unwrap();
                        i += 1;
                        let vstart = i;
                        while i < n && !body[i..].starts_with(quote) {
                            i += body[i..].chars().next().unwrap().len_utf8();
                        }
                        let v = &body[vstart..i];
                        if i < n {
                            i += 1; // consume closing quote
                        }
                        v.to_string()
                    } else {
                        let vstart = i;
                        while i < n && !body[i..].chars().next().unwrap().is_whitespace() {
                            i += body[i..].chars().next().unwrap().len_utf8();
                        }
                        body[vstart..i].to_string()
                    };
                    if !key.is_empty() {
                        attrs.set_kv(key, &val);
                    }
                } else if !key.is_empty() {
                    attrs.refs.push(key.to_string());
                }
            }
        }
    }
    attrs
}

/// If `line` (already trimmed) is a standalone block IAL `{: ... }`, return the
/// parsed Attrs. kramdown accepts `{:` optionally followed by a space.
pub fn parse_block_ial_line(line: &str) -> Option<Attrs> {
    let t = line.trim();
    if !t.starts_with("{:") || !t.ends_with('}') {
        return None;
    }
    let inner = &t[2..t.len() - 1];
    Some(parse_ial_body(inner))
}
