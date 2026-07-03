//! Inline (span-level) rendering: emphasis, code spans, links, images,
//! autolinks, entities, GFM strikethrough, hard line breaks, and kramdown's
//! auto-typographic substitutions (--, ---, ..., <<, >>) under the FHIR config.
//!
//! Raw HTML inline tags are passed through untouched (mandatory per survey).

use crate::util::escape_html_text;

/// Render inline markdown `src` to an HTML fragment.
pub fn render_inline(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 16);
    let chars: Vec<char> = src.chars().collect();
    render_inline_chars(&chars, &mut out);
    out
}

/// Extract the kramdown "raw text" of an inline string for auto-id generation:
/// the plain text with markup removed but typographic substitutions applied
/// (em/en dash, ellipsis, guillemets) and entities decoded to chars.
pub fn raw_text(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    raw_text_chars(&chars, &mut out);
    out
}

// ---------------------------------------------------------------------------

fn render_inline_chars(chars: &[char], out: &mut String) {
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '\\' if i + 1 < n && is_escapable(chars[i + 1]) => {
                out.push_str(&escape_char_text(chars[i + 1]));
                i += 2;
            }
            '`' => {
                if let Some((html, ni)) = try_code_span(chars, i) {
                    out.push_str(&html);
                    i = ni;
                } else {
                    out.push('`');
                    i += 1;
                }
            }
            '<' if chars.get(i + 1) == Some(&'<') && chars.get(i + 2) == Some(&' ') => {
                // kramdown laquo_space: `<< ` -> `«` + U+00A0 (nbsp). Only when a
                // space follows; bare `<<` is escaped as `&lt;&lt;`.
                out.push('\u{00ab}');
                out.push('\u{00a0}');
                i += 3;
            }
            '<' if chars.get(i + 1) == Some(&'<') => {
                // bare `<<` (no following space): both escape to &lt;&lt; — do
                // NOT let the second `<` be misread as an HTML tag start.
                out.push_str("&lt;&lt;");
                i += 2;
            }
            '<' => {
                if let Some((html, ni)) = try_autolink(chars, i) {
                    out.push_str(&html);
                    i = ni;
                } else if let Some((raw, ni)) = try_raw_inline_html(chars, i) {
                    out.push_str(&raw);
                    i = ni;
                } else {
                    out.push_str("&lt;");
                    i += 1;
                }
            }
            '&' => {
                if let Some((decoded, ni)) = try_entity(chars, i) {
                    out.push_str(&decoded);
                    i = ni;
                } else {
                    out.push_str("&amp;");
                    i += 1;
                }
            }
            '>' if chars.get(i + 1) == Some(&'>') && out.ends_with(' ') => {
                // kramdown raquo_space: ` >>` -> U+00A0 (nbsp) + `»`. Replace the
                // preceding literal space with nbsp. Bare `>>` is `&gt;&gt;`.
                out.pop();
                out.push('\u{00a0}');
                out.push('\u{00bb}');
                i += 2;
            }
            '>' => {
                out.push_str("&gt;");
                i += 1;
            }
            '!' if i + 1 < n && chars[i + 1] == '[' => {
                if let Some((html, ni)) = try_image(chars, i) {
                    out.push_str(&html);
                    i = ni;
                } else {
                    out.push('!');
                    i += 1;
                }
            }
            '[' => {
                if let Some((html, ni)) = try_link(chars, i) {
                    out.push_str(&html);
                    i = ni;
                } else {
                    out.push('[');
                    i += 1;
                }
            }
            '*' | '_' => {
                if let Some((html, ni)) = try_emphasis(chars, i) {
                    out.push_str(&html);
                    i = ni;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            '~' if i + 1 < n && chars[i + 1] == '~' => {
                if let Some((html, ni)) = try_strike(chars, i) {
                    out.push_str(&html);
                    i = ni;
                } else {
                    out.push('~');
                    i += 1;
                }
            }
            '-' if chars.get(i + 1) == Some(&'-') => {
                // --- em dash, -- en dash
                if chars.get(i + 2) == Some(&'-') {
                    out.push('\u{2014}');
                    i += 3;
                } else {
                    out.push('\u{2013}');
                    i += 2;
                }
            }
            '.' if chars.get(i + 1) == Some(&'.') && chars.get(i + 2) == Some(&'.') => {
                out.push('\u{2026}');
                i += 3;
            }
            '\n' => {
                // Hard line break: 2+ trailing spaces before the newline
                // (hard_wrap is false, so a bare single newline stays a soft
                // break = literal '\n').
                if out.ends_with("  ") {
                    // strip all trailing spaces then emit <br />
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push_str("<br />\n");
                } else {
                    // trailing single space is dropped by kramdown; keep newline
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    out.push('\n');
                }
                i += 1;
            }
            _ => {
                out.push_str(&escape_char_text(c));
                i += 1;
            }
        }
    }
}

fn is_escapable(c: char) -> bool {
    matches!(
        c,
        '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')'
            | '#' | '+' | '-' | '.' | '!' | '<' | '>' | '"' | '\'' | ':' | '|' | '~'
    )
}

fn escape_char_text(c: char) -> String {
    match c {
        '&' => "&amp;".to_string(),
        '<' => "&lt;".to_string(),
        '>' => "&gt;".to_string(),
        _ => c.to_string(),
    }
}

fn try_code_span(chars: &[char], i: usize) -> Option<(String, usize)> {
    // Count opening backticks.
    let n = chars.len();
    let mut fence = 0;
    let mut j = i;
    while j < n && chars[j] == '`' {
        fence += 1;
        j += 1;
    }
    let content_start = j;
    // Find closing run of exactly `fence` backticks.
    let mut k = content_start;
    while k < n {
        if chars[k] == '`' {
            let mut run = 0;
            let mut m = k;
            while m < n && chars[m] == '`' {
                run += 1;
                m += 1;
            }
            if run == fence {
                let content: String = chars[content_start..k].iter().collect();
                let trimmed = trim_code_span(&content);
                let escaped = escape_html_text(&trimmed);
                return Some((format!("<code>{escaped}</code>"), m));
            }
            k = m;
        } else {
            k += 1;
        }
    }
    None
}

fn trim_code_span(s: &str) -> String {
    // CommonMark/kramdown: if content begins and ends with a space (and is not
    // all spaces), strip one leading and trailing space.
    let trimmed = s.replace('\n', " ");
    if trimmed.len() >= 2
        && trimmed.starts_with(' ')
        && trimmed.ends_with(' ')
        && trimmed.trim().len() != 0
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed
    }
}

fn try_autolink(chars: &[char], i: usize) -> Option<(String, usize)> {
    // <scheme:...> or <email>
    let n = chars.len();
    let mut j = i + 1;
    let start = j;
    while j < n && chars[j] != '>' && chars[j] != ' ' && chars[j] != '<' {
        j += 1;
    }
    if j >= n || chars[j] != '>' {
        return None;
    }
    let inner: String = chars[start..j].iter().collect();
    if inner.contains("://") || is_scheme_url(&inner) {
        let esc = escape_html_text(&inner);
        return Some((format!("<a href=\"{esc}\">{esc}</a>"), j + 1));
    }
    if is_email(&inner) {
        let esc = escape_html_text(&inner);
        return Some((format!("<a href=\"mailto:{esc}\">{esc}</a>"), j + 1));
    }
    None
}

fn is_scheme_url(s: &str) -> bool {
    if let Some(idx) = s.find(':') {
        let scheme = &s[..idx];
        !scheme.is_empty()
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-')
            && scheme.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
    } else {
        false
    }
}

fn is_email(s: &str) -> bool {
    let at = s.find('@');
    match at {
        Some(idx) => idx > 0 && idx < s.len() - 1 && s[idx + 1..].contains('.'),
        None => false,
    }
}

fn try_raw_inline_html(chars: &[char], i: usize) -> Option<(String, usize)> {
    // Pass through an inline HTML tag: <tag ...>, </tag>, <!-- comment -->, <br/>
    let n = chars.len();
    if chars.get(i + 1) == Some(&'!') && chars.get(i + 2) == Some(&'-') && chars.get(i + 3) == Some(&'-')
    {
        // comment
        let mut j = i + 4;
        while j + 2 < n {
            if chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>' {
                let raw: String = chars[i..j + 3].iter().collect();
                return Some((raw, j + 3));
            }
            j += 1;
        }
        return None;
    }
    // tag start: <letter or </letter
    let mut j = i + 1;
    if chars.get(j) == Some(&'/') {
        j += 1;
    }
    if j >= n || !chars[j].is_ascii_alphabetic() {
        return None;
    }
    // scan to matching >
    let mut k = j;
    let mut in_str: Option<char> = None;
    while k < n {
        let c = chars[k];
        match in_str {
            Some(q) => {
                if c == q {
                    in_str = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    in_str = Some(c);
                } else if c == '>' {
                    let raw: String = chars[i..k + 1].iter().collect();
                    return Some((raw, k + 1));
                } else if c == '<' {
                    return None;
                }
            }
        }
        k += 1;
    }
    None
}

fn try_entity(chars: &[char], i: usize) -> Option<(String, usize)> {
    let n = chars.len();
    // &name; or &#123; or &#xAB;
    let mut j = i + 1;
    if j < n && chars[j] == '#' {
        j += 1;
        let hex = j < n && (chars[j] == 'x' || chars[j] == 'X');
        if hex {
            j += 1;
        }
        let start = j;
        while j < n && chars[j] != ';' {
            let valid = if hex {
                chars[j].is_ascii_hexdigit()
            } else {
                chars[j].is_ascii_digit()
            };
            if !valid {
                return None;
            }
            j += 1;
        }
        if j >= n || j == start {
            return None;
        }
        let num: String = chars[start..j].iter().collect();
        let code = u32::from_str_radix(&num, if hex { 16 } else { 10 }).ok()?;
        let ch = char::from_u32(code)?;
        return Some((escape_char_text(ch), j + 1));
    }
    // named entity
    let start = j;
    while j < n && chars[j] != ';' && (chars[j].is_ascii_alphanumeric()) {
        j += 1;
    }
    if j >= n || chars[j] != ';' || j == start {
        return None;
    }
    let name: String = chars[start..j].iter().collect();
    // as_char output: decode known entities to chars; but &amp; &lt; &gt; stay
    // as entities (they represent reserved chars).
    match name.as_str() {
        "amp" => Some(("&amp;".to_string(), j + 1)),
        "lt" => Some(("&lt;".to_string(), j + 1)),
        "gt" => Some(("&gt;".to_string(), j + 1)),
        _ => {
            if let Some(ch) = named_entity(&name) {
                Some((escape_char_text(ch), j + 1))
            } else {
                // Unknown named entity: kramdown leaves it as-is.
                Some((format!("&{name};"), j + 1))
            }
        }
    }
}

fn try_image(chars: &[char], i: usize) -> Option<(String, usize)> {
    // ![alt](url "title")
    let n = chars.len();
    let mut j = i + 2; // skip ![
    let alt_start = j;
    let mut depth = 1;
    while j < n && depth > 0 {
        match chars[j] {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            break;
        }
        j += 1;
    }
    if j >= n || chars[j] != ']' {
        return None;
    }
    let alt: String = chars[alt_start..j].iter().collect();
    j += 1;
    if j >= n || chars[j] != '(' {
        return None;
    }
    let (url, title, nj) = parse_link_dest(chars, j)?;
    let alt_esc = escape_attr_inline(&alt);
    let url_esc = escape_attr_inline(&url);
    let mut tag = format!("<img src=\"{url_esc}\" alt=\"{alt_esc}\"");
    if let Some(t) = title {
        tag.push_str(&format!(" title=\"{}\"", escape_attr_inline(&t)));
    }
    tag.push_str(" />");
    Some((tag, nj))
}

fn try_link(chars: &[char], i: usize) -> Option<(String, usize)> {
    // [text](url "title")
    let n = chars.len();
    let mut j = i + 1;
    let text_start = j;
    let mut depth = 1;
    while j < n && depth > 0 {
        match chars[j] {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            break;
        }
        j += 1;
    }
    if j >= n || chars[j] != ']' {
        return None;
    }
    let text: Vec<char> = chars[text_start..j].to_vec();
    j += 1;
    if j >= n || chars[j] != '(' {
        return None;
    }
    let (url, title, nj) = parse_link_dest(chars, j)?;
    let mut inner = String::new();
    render_inline_chars(&text, &mut inner);
    let url_esc = escape_attr_inline(&url);
    let mut tag = format!("<a href=\"{url_esc}\"");
    if let Some(t) = title {
        tag.push_str(&format!(" title=\"{}\"", escape_attr_inline(&t)));
    }
    tag.push('>');
    tag.push_str(&inner);
    tag.push_str("</a>");
    Some((tag, nj))
}

fn parse_link_dest(chars: &[char], open_paren: usize) -> Option<(String, Option<String>, usize)> {
    let n = chars.len();
    let mut j = open_paren + 1;
    // skip leading ws
    while j < n && chars[j].is_whitespace() {
        j += 1;
    }
    let url_start = j;
    let mut url_end = j;
    if j < n && chars[j] == '<' {
        j += 1;
        let s = j;
        while j < n && chars[j] != '>' {
            j += 1;
        }
        let url: String = chars[s..j].iter().collect();
        if j < n {
            j += 1;
        }
        // optional title
        let (title, nj) = parse_optional_title(chars, j)?;
        return Some((url, title, nj));
    }
    // bare url: up to whitespace or closing paren (balancing parens)
    let mut depth = 0i32;
    while j < n {
        let c = chars[j];
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            if depth == 0 {
                break;
            }
            depth -= 1;
        } else if c.is_whitespace() {
            break;
        }
        j += 1;
        url_end = j;
    }
    let url: String = chars[url_start..url_end].iter().collect();
    let (title, nj) = parse_optional_title(chars, j)?;
    Some((url, title, nj))
}

fn parse_optional_title(chars: &[char], mut j: usize) -> Option<(Option<String>, usize)> {
    let n = chars.len();
    while j < n && chars[j].is_whitespace() {
        j += 1;
    }
    let mut title = None;
    if j < n && (chars[j] == '"' || chars[j] == '\'') {
        let q = chars[j];
        j += 1;
        let s = j;
        while j < n && chars[j] != q {
            j += 1;
        }
        title = Some(chars[s..j].iter().collect());
        if j < n {
            j += 1;
        }
    }
    while j < n && chars[j].is_whitespace() {
        j += 1;
    }
    if j >= n || chars[j] != ')' {
        return None;
    }
    Some((title, j + 1))
}

fn try_emphasis(chars: &[char], i: usize) -> Option<(String, usize)> {
    let n = chars.len();
    let marker = chars[i];
    // count run
    let mut run = 0;
    let mut j = i;
    while j < n && chars[j] == marker {
        run += 1;
        j += 1;
    }
    let want = if run >= 2 { 2 } else { 1 };
    let content_start = i + want;
    // For `_`, kramdown (GFM) requires it not be intra-word; keep simple:
    // require the char before opening not be alnum for `_`.
    if marker == '_' {
        if i > 0 && chars[i - 1].is_alphanumeric() {
            return None;
        }
    }
    // opening must be followed by non-space
    if content_start >= n || chars[content_start].is_whitespace() {
        return None;
    }
    // find closing run of `want` markers, not preceded by space
    let mut k = content_start;
    while k < n {
        if chars[k] == marker {
            let mut rr = 0;
            let mut m = k;
            while m < n && chars[m] == marker {
                rr += 1;
                m += 1;
            }
            if rr >= want && k > content_start && !chars[k - 1].is_whitespace() {
                // for `_`, closing must not be followed by alnum
                if marker == '_' && m < n && chars[m].is_alphanumeric() {
                    k = m;
                    continue;
                }
                let inner_chars = &chars[content_start..k];
                let mut inner = String::new();
                render_inline_chars(inner_chars, &mut inner);
                let (tag_open, tag_close) = if want == 2 {
                    ("<strong>", "</strong>")
                } else {
                    ("<em>", "</em>")
                };
                let consumed = k + want;
                return Some((format!("{tag_open}{inner}{tag_close}"), consumed));
            }
            k = m;
        } else {
            k += 1;
        }
    }
    None
}

/// Return (inner_chars, next_index) if an emphasis span opens at `i`.
fn emphasis_inner(chars: &[char], i: usize) -> Option<(Vec<char>, usize)> {
    let n = chars.len();
    let marker = chars[i];
    let mut run = 0;
    let mut j = i;
    while j < n && chars[j] == marker {
        run += 1;
        j += 1;
    }
    let want = if run >= 2 { 2 } else { 1 };
    let content_start = i + want;
    if marker == '_' && i > 0 && chars[i - 1].is_alphanumeric() {
        return None;
    }
    if content_start >= n || chars[content_start].is_whitespace() {
        return None;
    }
    let mut k = content_start;
    while k < n {
        if chars[k] == marker {
            let mut rr = 0;
            let mut m = k;
            while m < n && chars[m] == marker {
                rr += 1;
                m += 1;
            }
            if rr >= want && k > content_start && !chars[k - 1].is_whitespace() {
                if marker == '_' && m < n && chars[m].is_alphanumeric() {
                    k = m;
                    continue;
                }
                return Some((chars[content_start..k].to_vec(), k + want));
            }
            k = m;
        } else {
            k += 1;
        }
    }
    None
}

fn strike_inner(chars: &[char], i: usize) -> Option<(Vec<char>, usize)> {
    let n = chars.len();
    let content_start = i + 2;
    let mut k = content_start;
    while k + 1 < n {
        if chars[k] == '~' && chars[k + 1] == '~' {
            return Some((chars[content_start..k].to_vec(), k + 2));
        }
        k += 1;
    }
    None
}

fn try_strike(chars: &[char], i: usize) -> Option<(String, usize)> {
    let n = chars.len();
    let content_start = i + 2;
    let mut k = content_start;
    while k + 1 < n {
        if chars[k] == '~' && chars[k + 1] == '~' {
            let inner_chars = &chars[content_start..k];
            let mut inner = String::new();
            render_inline_chars(inner_chars, &mut inner);
            return Some((format!("<del>{inner}</del>"), k + 2));
        }
        k += 1;
    }
    None
}

fn escape_attr_inline(s: &str) -> String {
    crate::util::escape_html_attr(s)
}

// --- raw text (for ids) -----------------------------------------------------

fn raw_text_chars(chars: &[char], out: &mut String) {
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '\\' if i + 1 < n && is_escapable(chars[i + 1]) => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '`' => {
                if let Some((_, ni)) = try_code_span(chars, i) {
                    // code span raw text is its content
                    let content: String = chars[i..ni].iter().collect();
                    // strip backticks
                    let stripped = content.trim_matches('`');
                    out.push_str(&stripped.replace('\n', " "));
                    i = ni;
                } else {
                    out.push('`');
                    i += 1;
                }
            }
            '&' => {
                if let Some((decoded, ni)) = try_entity(chars, i) {
                    // decoded is HTML-escaped; convert back to raw char best-effort
                    out.push_str(&html_unescape(&decoded));
                    i = ni;
                } else {
                    out.push('&');
                    i += 1;
                }
            }
            '*' | '_' => {
                // If this starts a real emphasis span, contribute only the
                // inner text; otherwise the marker is a literal char (e.g. the
                // underscore in `foo_bar`).
                if let Some((inner_chars, ni)) = emphasis_inner(chars, i) {
                    raw_text_chars(&inner_chars, out);
                    i = ni;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            '~' if i + 1 < n && chars[i + 1] == '~' => {
                if let Some((inner, ni)) = strike_inner(chars, i) {
                    raw_text_chars(&inner, out);
                    i = ni;
                } else {
                    out.push('~');
                    i += 1;
                }
            }
            '[' => {
                // link text
                if let Some((_, _ni)) = try_link(chars, i) {
                    // find text portion
                    if let Some((text, after_text)) = link_text(chars, i) {
                        raw_text_chars(&text, out);
                        // skip the (...) dest
                        if let Some((_, _, nj)) = parse_link_dest(chars, after_text) {
                            i = nj;
                            continue;
                        }
                    }
                }
                out.push('[');
                i += 1;
            }
            '-' if chars.get(i + 1) == Some(&'-') => {
                if chars.get(i + 2) == Some(&'-') {
                    out.push('\u{2014}');
                    i += 3;
                } else {
                    out.push('\u{2013}');
                    i += 2;
                }
            }
            '.' if chars.get(i + 1) == Some(&'.') && chars.get(i + 2) == Some(&'.') => {
                out.push('\u{2026}');
                i += 3;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
}

fn link_text(chars: &[char], i: usize) -> Option<(Vec<char>, usize)> {
    let n = chars.len();
    let mut j = i + 1;
    let text_start = j;
    let mut depth = 1;
    while j < n && depth > 0 {
        match chars[j] {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            break;
        }
        j += 1;
    }
    if j >= n || chars[j] != ']' {
        return None;
    }
    let text = chars[text_start..j].to_vec();
    if chars.get(j + 1) != Some(&'(') {
        return None;
    }
    Some((text, j + 1))
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
}

/// Minimal named-entity table for entities that appear in the corpus. kramdown
/// resolves ALL HTML5 named entities; we cover the common ones and pass through
/// unknown ones unchanged (documented boundary — see lib.rs).
fn named_entity(name: &str) -> Option<char> {
    let c = match name {
        "copy" => '\u{00A9}',
        "reg" => '\u{00AE}',
        "trade" => '\u{2122}',
        "nbsp" => '\u{00A0}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "hellip" => '\u{2026}',
        "laquo" => '\u{00AB}',
        "raquo" => '\u{00BB}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "deg" => '\u{00B0}',
        "plusmn" => '\u{00B1}',
        "times" => '\u{00D7}',
        "divide" => '\u{00F7}',
        "micro" => '\u{00B5}',
        "para" => '\u{00B6}',
        "sect" => '\u{00A7}',
        "bull" => '\u{2022}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "hearts" => '\u{2665}',
        "check" => '\u{2713}',
        "cross" => '\u{2717}',
        "rarr" => '\u{2192}',
        "larr" => '\u{2190}',
        "harr" => '\u{2194}',
        "uarr" => '\u{2191}',
        "darr" => '\u{2193}',
        "hArr" => '\u{21D4}',
        "le" => '\u{2264}',
        "ge" => '\u{2265}',
        "ne" => '\u{2260}',
        "euro" => '\u{20AC}',
        "pound" => '\u{00A3}',
        "cent" => '\u{00A2}',
        "yen" => '\u{00A5}',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    };
    Some(c)
}
