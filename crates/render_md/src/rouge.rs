//! Minimal Rouge-compatible tokenizers for the code fences the FHIR IG page
//! corpus exercises under Jekyll's `markdownify` (rouge highlighter): `json`
//! and `javascript`/`js`. Ported from the Rouge 4.7.0 lexer specs
//! (`rouge/lexers/{json,javascript}.rb`) — each `rule` is reproduced with the
//! token class it emits, and the token→CSS-shortname table
//! (`rouge/token.rb`) gives the `class="…"` string.
//!
//! Output shape matches Jekyll's rouge HTML formatter: a run of tokens where a
//! token with a non-empty shortname becomes `<span class="XX">escaped</span>`
//! and a `Text` token (shortname `""`) is emitted as raw escaped text with NO
//! span. Escaping is HTML-text escaping (`&`,`<`,`>`; quotes are left literal —
//! matching the goldens' `<span class="s2">"Parameters"</span>`).
//!
//! Ported lexers: `json`, `javascript`/`js`, and the `http` REQUEST/RESPONSE
//! LINE + HEADERS subset (rouge/lexers/http.rb; us-core scopes.html has the
//! corpus's one `http` fence — a header-only response). An http block whose
//! shape the subset does not model returns None (the tokenless deferral
//! signal) rather than emitting wrong tokens. An unhandled language falls
//! back to the caller's tokenless wrapper.

use crate::util::escape_html_text;

/// Is `lang` a language we can tokenize with a real lexer?
pub fn has_lexer(lang: &str) -> bool {
    matches!(lang, "json" | "javascript" | "js" | "http")
}

/// Tokenize `code` for `lang` and render the inner `<code>` body (token spans),
/// or None if we have no lexer for `lang`. The caller wraps it in the
/// `<div class="language-X highlighter-rouge">…` shell.
pub fn highlight(lang: &str, code: &str) -> Option<String> {
    let toks = match lang {
        "json" => tokenize_json(code),
        "javascript" | "js" => tokenize_js(code),
        "http" => tokenize_http(code)?,
        _ => return None,
    };
    // Rouge's HTML formatter coalesces CONSECUTIVE tokens of the SAME type into
    // one `<span>` run (rouge/formatters/html.rb streams `[tok, val]` pairs and
    // the theme emits one span per contiguous same-token run). E.g. a JSON
    // string value `"` + `Parameters` + `"` (all `s2`) becomes a single
    // `<span class="s2">"Parameters"</span>`.
    let mut merged: Vec<(&'static str, String)> = Vec::with_capacity(toks.len());
    for (short, text) in toks {
        if let Some(last) = merged.last_mut() {
            if last.0 == short {
                last.1.push_str(&text);
                continue;
            }
        }
        merged.push((short, text));
    }
    let mut out = String::new();
    for (short, text) in merged {
        let esc = escape_html_text(&text);
        if short.is_empty() {
            out.push_str(&esc);
        } else {
            out.push_str("<span class=\"");
            out.push_str(short);
            out.push_str("\">");
            out.push_str(&esc);
            out.push_str("</span>");
        }
    }
    Some(out)
}

// ----------------------------------------------------------------- JSON lexer
//
// rouge/lexers/json.rb state machine. States: root/object/array/value/string;
// mixins name/constants/whitespace. Token shortnames: whitespace `w`, punct
// `p`, object-key `nl` (Name.Label, the WHOLE quoted key), string value `s2`
// with `se` escapes, constants `kc`, float `mf`, integer `mi`.

#[derive(Clone, Copy)]
enum JsonState {
    Root,
    Object,
    Array,
    Value,
    JString, // inside a string value (opening `"` already emitted as s2)
}

fn tokenize_json(src: &str) -> Vec<(&'static str, String)> {
    let ch: Vec<char> = src.chars().collect();
    let n = ch.len();
    let mut i = 0;
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut stack: Vec<JsonState> = vec![JsonState::Root];

    while i < n {
        let st = *stack.last().unwrap();
        // whitespace mixin (all states except inside a string)
        if !matches!(st, JsonState::JString) {
            if ch[i].is_whitespace() {
                let s = i;
                while i < n && ch[i].is_whitespace() {
                    i += 1;
                }
                out.push(("w", ch[s..i].iter().collect()));
                continue;
            }
        }
        match st {
            JsonState::JString => {
                // [^\\"]+  -> s2 ; \\. -> se ; " -> s2 pop
                if ch[i] == '"' {
                    out.push(("s2", "\"".into()));
                    i += 1;
                    stack.pop();
                } else if ch[i] == '\\' && i + 1 < n {
                    out.push(("se", ch[i..i + 2].iter().collect()));
                    i += 2;
                } else {
                    let s = i;
                    while i < n && ch[i] != '"' && ch[i] != '\\' {
                        i += 1;
                    }
                    out.push(("s2", ch[s..i].iter().collect()));
                }
            }
            JsonState::Root | JsonState::Object | JsonState::Array | JsonState::Value => {
                let c = ch[i];
                // object/root/array structural chars
                if c == '{' {
                    out.push(("p", "{".into()));
                    i += 1;
                    stack.push(JsonState::Object);
                    continue;
                }
                if c == '[' {
                    out.push(("p", "[".into()));
                    i += 1;
                    stack.push(JsonState::Array);
                    continue;
                }
                if c == '}' {
                    out.push(("p", "}".into()));
                    i += 1;
                    if matches!(st, JsonState::Object) {
                        stack.pop();
                    }
                    continue;
                }
                if c == ']' {
                    out.push(("p", "]".into()));
                    i += 1;
                    if matches!(st, JsonState::Array) {
                        stack.pop();
                    }
                    continue;
                }
                if c == ',' {
                    out.push(("p", ",".into()));
                    i += 1;
                    continue;
                }
                // `name` mixin: a quoted key followed by optional ws + `:` — only
                // in root/object/value where a key can appear.
                if c == '"' {
                    if let Some((keylen, wslen, colon)) = json_try_name(&ch, i) {
                        out.push(("nl", ch[i..i + keylen].iter().collect()));
                        if wslen > 0 {
                            out.push(("w", ch[i + keylen..i + keylen + wslen].iter().collect()));
                        }
                        if colon {
                            out.push(("p", ":".into()));
                        }
                        i += keylen + wslen + if colon { 1 } else { 0 };
                        continue;
                    }
                    // otherwise a string VALUE: emit opening quote as s2, push string
                    out.push(("s2", "\"".into()));
                    i += 1;
                    stack.push(JsonState::JString);
                    continue;
                }
                // constants mixin: true/false/null, float, integer
                if let Some(len) = json_match_word(&ch, i, "true")
                    .or_else(|| json_match_word(&ch, i, "false"))
                    .or_else(|| json_match_word(&ch, i, "null"))
                {
                    out.push(("kc", ch[i..i + len].iter().collect()));
                    i += len;
                    continue;
                }
                if let Some((len, is_float)) = json_number(&ch, i) {
                    out.push((if is_float { "mf" } else { "mi" }, ch[i..i + len].iter().collect()));
                    i += len;
                    continue;
                }
                // Unrecognized char: emit as bare text (Text -> no span).
                out.push(("", c.to_string()));
                i += 1;
            }
        }
    }
    out
}

/// At a `"`, is this an object key `"…"` (ws) `:` ? Returns (keylen incl quotes,
/// wslen, has_colon). rouge json `:name` rule: `("(?:\\.|[^"\\\n])*?")(\s*)(:)`.
fn json_try_name(ch: &[char], i: usize) -> Option<(usize, usize, bool)> {
    // scan the quoted string
    let n = ch.len();
    let mut j = i + 1;
    while j < n {
        match ch[j] {
            '\\' if j + 1 < n => j += 2,
            '"' => break,
            '\n' => return None,
            _ => j += 1,
        }
    }
    if j >= n || ch[j] != '"' {
        return None;
    }
    let keylen = j + 1 - i;
    // optional whitespace then a colon
    let mut k = j + 1;
    let ws_start = k;
    while k < n && ch[k].is_whitespace() {
        k += 1;
    }
    if k < n && ch[k] == ':' {
        Some((keylen, k - ws_start, true))
    } else {
        None
    }
}

fn json_match_word(ch: &[char], i: usize, word: &str) -> Option<usize> {
    let w: Vec<char> = word.chars().collect();
    if i + w.len() <= ch.len() && ch[i..i + w.len()] == w[..] {
        Some(w.len())
    } else {
        None
    }
}

/// rouge json number: float `-?(0|[1-9]\d*)\.\d+(e[+-]?\d+)?` else integer
/// `-?(0|[1-9]\d*)(e[+-]?\d+)?`. Returns (len, is_float).
fn json_number(ch: &[char], i: usize) -> Option<(usize, bool)> {
    let n = ch.len();
    let mut j = i;
    if j < n && ch[j] == '-' {
        j += 1;
    }
    let int_start = j;
    if j < n && ch[j] == '0' {
        j += 1;
    } else if j < n && ('1'..='9').contains(&ch[j]) {
        while j < n && ch[j].is_ascii_digit() {
            j += 1;
        }
    } else {
        return None;
    }
    if j == int_start {
        return None;
    }
    let mut is_float = false;
    if j < n && ch[j] == '.' && j + 1 < n && ch[j + 1].is_ascii_digit() {
        is_float = true;
        j += 1;
        while j < n && ch[j].is_ascii_digit() {
            j += 1;
        }
    }
    // optional exponent
    if j < n && (ch[j] == 'e' || ch[j] == 'E') {
        let mut k = j + 1;
        if k < n && (ch[k] == '+' || ch[k] == '-') {
            k += 1;
        }
        if k < n && ch[k].is_ascii_digit() {
            while k < n && ch[k].is_ascii_digit() {
                k += 1;
            }
            j = k;
            // exponent implies float only if the float form matched; rouge's
            // integer rule also allows an exponent, keeping it `mi`.
        }
    }
    Some((j - i, is_float))
}

// ------------------------------------------------------------ JavaScript lexer
//
// rouge/lexers/javascript.rb — the subset the corpus needs (JSON-shaped example
// payloads rendered with the js lexer). Shortnames: keyword `k`, declaration
// `kd`, reserved `kr`, constant `kc`, builtin `nb`, other-name `nx`, function
// `nf`, string delim `dl`, string body `s2`/`s1`, escape `se`, numbers
// `mf`/`mh`/`mo`/`mb`/`mi`, operator `o`, punctuation `p`, comments `c1`/`cm`,
// object-key `na`. JS whitespace is plain Text (NO span), unlike JSON.

const JS_KEYWORDS: &[&str] = &[
    "async", "await", "break", "case", "catch", "continue", "debugger", "default", "delete", "do",
    "else", "export", "finally", "from", "for", "if", "import", "in", "instanceof", "new", "of",
    "return", "super", "switch", "this", "throw", "try", "typeof", "void", "while", "yield",
];
const JS_DECLARATIONS: &[&str] = &[
    "var", "let", "const", "with", "function", "class", "extends", "constructor", "get", "set",
    "static",
];
const JS_RESERVED: &[&str] =
    &["enum", "implements", "interface", "package", "private", "protected", "public"];
const JS_CONSTANTS: &[&str] = &["true", "false", "null", "NaN", "Infinity", "undefined"];
const JS_BUILTINS: &[&str] = &[
    "Array", "Boolean", "Date", "Error", "Function", "Math", "netscape", "Number", "Object",
    "Packages", "RegExp", "String", "sun", "decodeURI", "decodeURIComponent", "encodeURI",
    "encodeURIComponent", "eval", "isFinite", "isNaN", "parseFloat", "parseInt", "document",
    "window", "navigator", "self", "global", "Promise", "Set", "Map", "WeakSet", "WeakMap",
    "Symbol", "Proxy", "Reflect", "Int8Array", "Uint8Array", "Uint8ClampedArray", "Int16Array",
    "Uint16Array", "Uint16ClampedArray", "Int32Array", "Uint32Array", "Uint32ClampedArray",
    "Float32Array", "Float64Array", "DataView", "ArrayBuffer",
];

fn is_js_id_start(c: char) -> bool {
    c.is_alphabetic() || c == '$' || c == '_'
}
fn is_js_id_cont(c: char) -> bool {
    c.is_alphanumeric() || c == '$' || c == '_'
}

fn tokenize_js(src: &str) -> Vec<(&'static str, String)> {
    let ch: Vec<char> = src.chars().collect();
    let n = ch.len();
    let mut i = 0;
    let mut out: Vec<(&'static str, String)> = Vec::new();

    while i < n {
        let c = ch[i];
        // whitespace -> Text (no span)
        if c.is_whitespace() {
            let s = i;
            while i < n && ch[i].is_whitespace() {
                i += 1;
            }
            out.push(("", ch[s..i].iter().collect()));
            continue;
        }
        // line comment //…
        if c == '/' && ch.get(i + 1) == Some(&'/') {
            let s = i;
            while i < n && ch[i] != '\n' {
                i += 1;
            }
            out.push(("c1", ch[s..i].iter().collect()));
            continue;
        }
        // block comment /* … */
        if c == '/' && ch.get(i + 1) == Some(&'*') {
            let s = i;
            i += 2;
            while i < n && !(ch[i] == '*' && ch.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(n);
            out.push(("cm", ch[s..i].iter().collect()));
            continue;
        }
        // double / single quoted string -> dl "…" with s2/s1 body, se escapes.
        // rouge js `:dq`/`:sq` escape rule is `\\[\\nrt"]?` / `\\[\\nrt']?` — the
        // escaped char is OPTIONAL from a fixed set, so `\'` inside a "…" string
        // tokenizes as `se`=`\` then the `'` continues as body (NOT `\'` as one
        // escape). This matters for `"Saint Luke \'s Hospital"`.
        if c == '"' || c == '\'' {
            let (body_short, quote) = if c == '"' { ("s2", '"') } else { ("s1", '\'') };
            out.push(("dl", quote.to_string()));
            i += 1;
            while i < n && ch[i] != quote {
                if ch[i] == '\\' {
                    // The escaped char is consumed only if it is `\`, n, r, t, or
                    // the string's own quote char.
                    let esc_next = ch.get(i + 1).copied();
                    let takes_next = matches!(esc_next, Some('\\') | Some('n') | Some('r') | Some('t'))
                        || esc_next == Some(quote);
                    if takes_next {
                        out.push(("se", ch[i..i + 2].iter().collect()));
                        i += 2;
                    } else {
                        out.push(("se", "\\".into()));
                        i += 1;
                    }
                } else {
                    let s = i;
                    while i < n && ch[i] != quote && ch[i] != '\\' {
                        i += 1;
                    }
                    out.push((body_short, ch[s..i].iter().collect()));
                }
            }
            if i < n {
                out.push(("dl", quote.to_string()));
                i += 1;
            }
            continue;
        }
        // numbers
        if c.is_ascii_digit() {
            if let Some((len, short)) = js_number(&ch, i) {
                out.push((short, ch[i..i + len].iter().collect()));
                i += len;
                continue;
            }
        }
        // identifier / keyword — with object-key lookahead (`id` ws `:` -> na)
        if is_js_id_start(c) || c == '#' {
            let s = i;
            if ch[i] == '#' {
                i += 1;
            }
            while i < n && is_js_id_cont(ch[i]) {
                i += 1;
            }
            let word: String = ch[s..i].iter().collect();
            // object-key: identifier followed by optional ws then `:` (not `::`)
            let mut k = i;
            while k < n && (ch[k] == ' ' || ch[k] == '\t') {
                k += 1;
            }
            if k < n && ch[k] == ':' && ch.get(k + 1) != Some(&':') {
                // Name.Attribute (object key). Emit key, ws, then let the `:`
                // be handled by the punctuation rule on the next iteration.
                out.push(("na", word));
                continue;
            }
            let short = js_word_class(&word);
            out.push((short, word));
            continue;
        }
        // operators (multi-char first)
        if let Some(len) = js_operator(&ch, i) {
            out.push(("o", ch[i..i + len].iter().collect()));
            i += len;
            continue;
        }
        // punctuation
        if "{}[]().,;:?".contains(c) {
            out.push(("p", c.to_string()));
            i += 1;
            continue;
        }
        // anything else: bare text
        out.push(("", c.to_string()));
        i += 1;
    }
    out
}

fn js_word_class(w: &str) -> &'static str {
    if JS_KEYWORDS.contains(&w) {
        "k"
    } else if JS_DECLARATIONS.contains(&w) {
        "kd"
    } else if JS_RESERVED.contains(&w) {
        "kr"
    } else if JS_CONSTANTS.contains(&w) {
        "kc"
    } else if JS_BUILTINS.contains(&w) {
        "nb"
    } else {
        "nx"
    }
}

/// rouge js numbers: float `\d+\.\d+([eE]\d+)?[fd]?`, hex `0x…`, oct `0o…`,
/// bin `0b…`, else integer. Returns (len, shortname).
fn js_number(ch: &[char], i: usize) -> Option<(usize, &'static str)> {
    let n = ch.len();
    // hex/oct/bin prefixes
    if ch[i] == '0' && i + 1 < n {
        let p = ch[i + 1];
        let (radix_ok, short): (fn(char) -> bool, &str) = match p {
            'x' | 'X' => (|c| c.is_ascii_hexdigit(), "mh"),
            'o' | 'O' => (|c| ('0'..='7').contains(&c), "mo"),
            'b' | 'B' => (|c| c == '0' || c == '1', "mb"),
            _ => (|_| false, ""),
        };
        if !short.is_empty() {
            let mut j = i + 2;
            while j < n && (radix_ok(ch[j]) || ch[j] == '_') {
                j += 1;
            }
            if j > i + 2 {
                return Some((j - i, short));
            }
        }
    }
    // float: digits . digits
    let mut j = i;
    while j < n && ch[j].is_ascii_digit() {
        j += 1;
    }
    if j < n && ch[j] == '.' && j + 1 < n && ch[j + 1].is_ascii_digit() {
        j += 1;
        while j < n && ch[j].is_ascii_digit() {
            j += 1;
        }
        if j < n && (ch[j] == 'e' || ch[j] == 'E') {
            let mut k = j + 1;
            while k < n && ch[k].is_ascii_digit() {
                k += 1;
            }
            if k > j + 1 {
                j = k;
            }
        }
        if j < n && (ch[j] == 'f' || ch[j] == 'd') {
            j += 1;
        }
        return Some((j - i, "mf"));
    }
    // integer
    let mut j = i;
    while j < n && ch[j].is_ascii_digit() {
        j += 1;
    }
    if j > i {
        Some((j - i, "mi"))
    } else {
        None
    }
}

/// rouge js operator run. Multi-char ops first, then single-char. Returns len.
fn js_operator(ch: &[char], i: usize) -> Option<usize> {
    let n = ch.len();
    let s: String = ch[i..(i + 4).min(n)].iter().collect();
    for op in ["===", "!==", ">>>", "&&", "||", "<<", ">>", "++", "--", "??"] {
        if s.starts_with(op) {
            return Some(op.chars().count());
        }
    }
    // op with optional trailing `=` : - < > + * % & | ^ / ! =
    let c = ch[i];
    if "-<>+*%&|^/!=~".contains(c) {
        if ch.get(i + 1) == Some(&'=') {
            return Some(2);
        }
        return Some(1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_basic() {
        // Coalesced string value span; nl keys; kc/mi constants; w whitespace.
        let h = highlight("json", "{\n  \"a\": \"x\",\n  \"n\": 5,\n  \"b\": true\n}").unwrap();
        assert!(h.contains("<span class=\"nl\">\"a\"</span>"), "{h}");
        assert!(h.contains("<span class=\"s2\">\"x\"</span>"), "{h}");
        assert!(h.contains("<span class=\"mi\">5</span>"), "{h}");
        assert!(h.contains("<span class=\"kc\">true</span>"), "{h}");
        assert!(h.contains("<span class=\"p\">{</span>"), "{h}");
    }

    #[test]
    fn js_basic() {
        // JS whitespace is plain text (no w span); quotes are dl, body s2; keys na.
        let h = highlight("js", "{\n  \"resourceType\": \"Observation\"\n}").unwrap();
        assert!(h.contains("<span class=\"p\">{</span>\n"), "{h}");
        assert!(h.contains("<span class=\"dl\">\"</span>"), "{h}");
        assert!(h.contains("<span class=\"s2\">resourceType</span>"), "{h}");
    }
}

// ---------------------------------------------------------------------------
// http (rouge/lexers/http.rb, request/response line + headers subset)
//
// Rules reproduced (Rouge 4.7.0):
//   response line: ^(HTTP)(/)(\d(?:\.\d)?)( +)(\d{3})( +)([^\r\n]+)
//     -> Keyword k, Operator o, Num m, Text, Num m, Text, Name::Exception ne
//   request line:  ^(GET|POST|PUT|DELETE|HEAD|OPTIONS|PATCH|TRACE|CONNECT)
//                  ( +)([^ ]+)( +)(HTTP)(/)(\d(?:\.\d)?)
//     -> Name::Function nf, Text, Name::Namespace nn, Text, Keyword k,
//        Operator o, Num m
//   header line:   ^([^\s:]+)( *)(:)( *)([^\r\n]+)
//     -> Name::Attribute na, Text, Punctuation p, Text, Str s
// Body delegation (content-type sub-lexing) is NOT modeled: a block with a
// body (or any unmodeled line) returns None so the caller's tokenless path
// keeps the deferral loud.

fn tokenize_http(code: &str) -> Option<Vec<(&'static str, String)>> {
    let mut toks: Vec<(&'static str, String)> = Vec::new();
    let mut lines = code.split_inclusive('\n').peekable();
    let mut first = true;
    while let Some(line) = lines.next() {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        if body.is_empty() {
            // Blank line = the header/body separator; a body follows -> not
            // modeled unless it is only trailing whitespace.
            if lines.peek().is_some() {
                return None;
            }
            toks.push(("", nl.to_string()));
            continue;
        }
        if first {
            first = false;
            if let Some(t) = http_status_line(body) {
                toks.extend(t);
                toks.push(("", nl.to_string()));
                continue;
            }
            if let Some(t) = http_request_line(body) {
                toks.extend(t);
                toks.push(("", nl.to_string()));
                continue;
            }
            return None;
        }
        // header line
        let colon = body.find(':')?;
        let (name, rest) = body.split_at(colon);
        if name.is_empty() || name.contains(char::is_whitespace) {
            return None;
        }
        let rest = &rest[1..]; // drop ':'
        let val_start = rest.len() - rest.trim_start().len();
        toks.push(("na", name.to_string()));
        toks.push(("p", ":".to_string()));
        toks.push(("", rest[..val_start].to_string()));
        toks.push(("s", rest[val_start..].to_string()));
        toks.push(("", nl.to_string()));
    }
    Some(toks)
}

fn http_status_line(line: &str) -> Option<Vec<(&'static str, String)>> {
    let rest = line.strip_prefix("HTTP/")?;
    let (ver, rest) = rest.split_at(rest.find(' ')?);
    if !ver.chars().all(|c| c.is_ascii_digit() || c == '.') || ver.is_empty() {
        return None;
    }
    let sp1_len = rest.len() - rest.trim_start().len();
    let (sp1, rest) = rest.split_at(sp1_len);
    let code_end = rest.find(' ').unwrap_or(rest.len());
    let (code, rest) = rest.split_at(code_end);
    if code.len() != 3 || !code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let sp2_len = rest.len() - rest.trim_start().len();
    let (sp2, reason) = rest.split_at(sp2_len);
    let mut t = vec![
        ("k", "HTTP".to_string()),
        ("o", "/".to_string()),
        ("m", ver.to_string()),
        ("", sp1.to_string()),
        ("m", code.to_string()),
    ];
    if !reason.is_empty() {
        t.push(("", sp2.to_string()));
        t.push(("ne", reason.to_string()));
    }
    Some(t)
}

fn http_request_line(line: &str) -> Option<Vec<(&'static str, String)>> {
    const METHODS: [&str; 9] = [
        "GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH", "TRACE", "CONNECT",
    ];
    let method = METHODS.iter().find(|m| {
        line.starts_with(**m) && line[m.len()..].starts_with(' ')
    })?;
    let rest = &line[method.len()..];
    let sp1_len = rest.len() - rest.trim_start().len();
    let (sp1, rest) = rest.split_at(sp1_len);
    let path_end = rest.find(' ')?;
    let (path, rest) = rest.split_at(path_end);
    let sp2_len = rest.len() - rest.trim_start().len();
    let (sp2, rest) = rest.split_at(sp2_len);
    let ver = rest.strip_prefix("HTTP/")?;
    if !ver.chars().all(|c| c.is_ascii_digit() || c == '.') || ver.is_empty() {
        return None;
    }
    Some(vec![
        ("nf", method.to_string()),
        ("", sp1.to_string()),
        ("nn", path.to_string()),
        ("", sp2.to_string()),
        ("k", "HTTP".to_string()),
        ("o", "/".to_string()),
        ("m", ver.to_string()),
    ])
}
