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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attrs {
    pub id: Option<String>,
    pub classes: Vec<String>,
    /// key="value" pairs, in source order.
    pub kv: Vec<(String, String)>,
    /// Bare references (`{:ref}`) — e.g. `toc`, `no_toc`. kramdown resolves
    /// these against ALDs; we track them so callers can special-case
    /// `toc`/`no_toc`.
    pub refs: Vec<String>,
}

impl Attrs {
    pub fn is_empty(&self) -> bool {
        self.id.is_none()
            && self.classes.is_empty()
            && self.kv.is_empty()
            && self.refs.is_empty()
    }

    pub fn has_ref(&self, name: &str) -> bool {
        self.refs.iter().any(|r| r == name)
    }

    /// Serialize to HTML attribute string (leading space included when
    /// non-empty), matching kramdown attribute ordering: kramdown stores attrs
    /// in a hash and emits them sorted by key, with `id` and `class` handled
    /// specially. Empirically kramdown emits `id` first (when set via IAL it is
    /// stored under key "id"), then remaining attrs in insertion/sorted order,
    /// then class. To keep parity we emit in the order: other kv (sorted),
    /// then id, then class — see html_attr_string for the exact rule used by
    /// the renderer.
    pub fn class_attr(&self) -> Option<String> {
        if self.classes.is_empty() {
            None
        } else {
            Some(self.classes.join(" "))
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
                    attrs.classes.push(cls.to_string());
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
                    attrs.id = Some(id.to_string());
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
                        attrs.kv.push((key.to_string(), val));
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
