//! Parser: tokens -> AST. Implements the T1+T2 grammar with Liquid-4.0.4
//! semantics for expressions, filters and conditions.

use crate::ast::*;
use crate::lexer::{tokenize, Token};
use crate::value::Value;

pub struct ParseError(pub String);

pub fn parse(src: &str) -> Result<Template, ParseError> {
    let tokens = tokenize(src);
    let mut p = Parser {
        tokens,
        pos: 0,
    };
    let body = p.parse_block(&[])?;
    Ok(body)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    /// Parse nodes until one of `stop` tag-names is seen (which is NOT
    /// consumed) or EOF. Returns the block body.
    fn parse_block(&mut self, stop: &[&str]) -> Result<Template, ParseError> {
        let mut out = Vec::new();
        while self.pos < self.tokens.len() {
            match self.tokens[self.pos].clone() {
                Token::Raw(s) => {
                    if !s.is_empty() {
                        out.push(Node::Raw(s));
                    }
                    self.pos += 1;
                }
                Token::RawBlock { body, .. } => {
                    out.push(Node::Raw2(body));
                    self.pos += 1;
                }
                Token::Output { inner, .. } => {
                    let expr = parse_expr(&inner)?;
                    out.push(Node::Output(expr));
                    self.pos += 1;
                }
                Token::Tag { inner, .. } => {
                    let name = first_word(&inner);
                    if stop.contains(&name.as_str()) {
                        return Ok(out);
                    }
                    self.pos += 1;
                    let node = self.parse_tag(&name, &inner)?;
                    if let Some(n) = node {
                        out.push(n);
                    }
                }
            }
        }
        Ok(out)
    }

    fn parse_tag(&mut self, name: &str, inner: &str) -> Result<Option<Node>, ParseError> {
        let rest = inner[name.len()..].trim();
        match name {
            "assign" => {
                let (var, expr_src) = split_once_char(rest, '=')
                    .ok_or_else(|| ParseError("assign missing =".into()))?;
                let expr = parse_expr(expr_src.trim())?;
                Ok(Some(Node::Assign {
                    name: var.trim().to_string(),
                    expr,
                }))
            }
            "capture" => {
                let body = self.parse_block(&["endcapture"])?;
                self.expect_end("endcapture")?;
                Ok(Some(Node::Capture {
                    name: rest.trim().to_string(),
                    body,
                }))
            }
            "increment" => Ok(Some(Node::Increment(rest.trim().to_string()))),
            "decrement" => Ok(Some(Node::Decrement(rest.trim().to_string()))),
            "comment" => {
                self.skip_until(&["endcomment"]);
                self.expect_end("endcomment")?;
                Ok(Some(Node::Comment))
            }
            // `raw` is handled entirely in the lexer as a single verbatim
            // RawBlock token (so exact spacing like `{{access_token}}` is
            // preserved). A bare `raw` reaching here means an unterminated raw;
            // emit nothing.
            "raw" | "endraw" => Ok(None),
            "if" => self.parse_if(rest, false).map(Some),
            "unless" => self.parse_if(rest, true).map(Some),
            "case" => self.parse_case(rest).map(Some),
            "for" => self.parse_for(rest).map(Some),
            "break" => Ok(Some(Node::Break)),
            "continue" => Ok(Some(Node::Continue)),
            "include" | "include_relative" => self.parse_include(rest).map(Some),
            // Recognized-but-passthrough tags (registry-documented). They emit
            // nothing by default; the host may register handlers later.
            "lang-fragment" | "fragment" | "sql" | "sqlToData" => Ok(Some(Node::UnknownTag {
                name: name.to_string(),
                markup: rest.to_string(),
            })),
            other => Ok(Some(Node::UnknownTag {
                name: other.to_string(),
                markup: rest.to_string(),
            })),
        }
    }

    fn parse_if(&mut self, cond_src: &str, negate: bool) -> Result<Node, ParseError> {
        let mut branches: Vec<(Condition, Template)> = Vec::new();
        let mut else_body: Option<Template> = None;

        let mut cond = parse_condition(cond_src)?;
        if negate {
            cond = negate_condition(cond);
        }
        let stops = ["elsif", "else", "endif", "endunless"];
        let body = self.parse_block(&stops)?;
        branches.push((cond, body));

        loop {
            let Some(Token::Tag { inner, .. }) = self.tokens.get(self.pos).cloned() else {
                break;
            };
            let w = first_word(&inner);
            match w.as_str() {
                "elsif" => {
                    self.pos += 1;
                    let c = parse_condition(inner["elsif".len()..].trim())?;
                    let b = self.parse_block(&stops)?;
                    branches.push((c, b));
                }
                "else" => {
                    self.pos += 1;
                    let b = self.parse_block(&["endif", "endunless"])?;
                    else_body = Some(b);
                }
                "endif" | "endunless" => {
                    self.pos += 1;
                    break;
                }
                _ => break,
            }
        }
        Ok(Node::If {
            branches,
            else_body,
        })
    }

    fn parse_case(&mut self, subject_src: &str) -> Result<Node, ParseError> {
        let subject = parse_expr(subject_src.trim())?;
        let mut whens: Vec<(Vec<Term>, Template)> = Vec::new();
        let mut else_body = None;
        // Skip any raw/text between `{% case %}` and the first `{% when %}`
        // (Liquid ignores it).
        let stops = ["when", "else", "endcase"];
        // Discard leading non-when content by parsing (and dropping) a block.
        let _ = self.parse_block(&stops)?;
        loop {
            let Some(Token::Tag { inner, .. }) = self.tokens.get(self.pos).cloned() else {
                break;
            };
            match first_word(&inner).as_str() {
                "when" => {
                    self.pos += 1;
                    // `when a, b` or `when a or b` -> candidate terms
                    let cand_src = inner["when".len()..].trim();
                    let mut cands = Vec::new();
                    for part in split_when_values(cand_src) {
                        cands.push(parse_term(part.trim())?);
                    }
                    let body = self.parse_block(&stops)?;
                    whens.push((cands, body));
                }
                "else" => {
                    self.pos += 1;
                    else_body = Some(self.parse_block(&["endcase"])?);
                }
                "endcase" => {
                    self.pos += 1;
                    break;
                }
                _ => break,
            }
        }
        Ok(Node::Case {
            subject,
            whens,
            else_body,
        })
    }

    fn parse_for(&mut self, src: &str) -> Result<Node, ParseError> {
        // syntax: VAR in ITERABLE [reversed] [offset:N] [limit:N]
        let (var, after) = split_once_word(src, "in")
            .ok_or_else(|| ParseError("for missing 'in'".into()))?;
        let var = var.trim().to_string();
        // split iterable expr from trailing attributes (reversed/offset/limit)
        let mut reversed = false;
        let mut offset = None;
        let mut limit = None;
        // Tokenize the remaining by whitespace but keep the iterable (which may
        // contain a range or a filtered pipeline). Attributes appear at the
        // END. We scan tokens for `reversed`, `offset:`, `limit:`.
        let (iter_src, attrs) = split_for_attrs(after.trim());
        for (k, v) in attrs {
            match k.as_str() {
                "reversed" => reversed = true,
                "offset" => offset = Some(parse_expr(&v)?),
                "limit" => limit = Some(parse_expr(&v)?),
                _ => {}
            }
        }
        let iterable = parse_expr(iter_src.trim())?;
        let body = self.parse_block(&["else", "endfor"])?;
        let mut else_body = None;
        if let Some(Token::Tag { inner, .. }) = self.tokens.get(self.pos).cloned() {
            if first_word(&inner) == "else" {
                self.pos += 1;
                else_body = Some(self.parse_block(&["endfor"])?);
            }
        }
        self.expect_end("endfor")?;
        Ok(Node::For {
            var,
            iterable,
            reversed,
            offset,
            limit,
            body,
            else_body,
        })
    }

    fn parse_include(&mut self, src: &str) -> Result<Node, ParseError> {
        // `NAME [key=value key2=value2 ...]` where NAME is a bare filename
        // (possibly containing `{{ }}` for dynamic includes) up to the first
        // space that precedes a `key=`.
        let src = src.trim();
        // Detect dynamic include: contains `{{`
        let (name_part, params_part) = split_include_name(src);
        let name = if name_part.contains("{{") {
            IncludeName::Dynamic(name_part.to_string())
        } else {
            IncludeName::Literal(name_part.to_string())
        };
        let params = parse_include_params(params_part)?;
        Ok(Node::Include { name, params })
    }

    fn expect_end(&mut self, _tag: &str) -> Result<(), ParseError> {
        // parse_block already leaves us positioned on the end tag (unconsumed);
        // callers that used parse_block+expect_end consume it here.
        if let Some(Token::Tag { inner, .. }) = self.tokens.get(self.pos).cloned() {
            let _ = inner;
            self.pos += 1;
        }
        Ok(())
    }

    fn skip_until(&mut self, stop: &[&str]) {
        while self.pos < self.tokens.len() {
            if let Token::Tag { inner, .. } = &self.tokens[self.pos] {
                if stop.contains(&first_word(inner).as_str()) {
                    return;
                }
            }
            self.pos += 1;
        }
    }

}

// ------------------------------------------------------------------ helpers

fn first_word(s: &str) -> String {
    s.trim()
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_string()
}

fn negate_condition(c: Condition) -> Condition {
    // `unless X` == `if not X`. Liquid has no `not`; we invert at eval by
    // wrapping in a comparison-to-false. Simplest: represent as Truthy inverse
    // via a synthetic Comparison. We implement by swapping in the renderer:
    // easiest is to wrap in Or/And identity and mark — but cleanest is a
    // dedicated approach: compare (cond truthy) == false. We encode unless by
    // building `Comparison{ Truthy? }`. To keep the AST simple we introduce a
    // wrapper Condition via Or(false-eq). Instead: invert known simple forms.
    match c {
        // `contains` has no inverse comparison operator, so wrap the whole
        // comparison in NotTruthy rather than mis-inverting it.
        Condition::Comparison { op: CompareOp::Contains, .. } => {
            Condition::NotTruthy(Box::new(c))
        }
        Condition::Comparison { left, op, right } => Condition::Comparison {
            left,
            op: invert_op(op),
            right,
        },
        other => {
            // Wrap: `unless (A and B)` -> not(A) or not(B) is complex; corpus
            // `unless` is always a single truthy/comparison (forloop.last,
            // single_example). Fall back to comparing truthiness to false.
            Condition::NotTruthy(Box::new(other))
        }
    }
}

fn invert_op(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::Eq => CompareOp::Ne,
        CompareOp::Ne => CompareOp::Eq,
        CompareOp::Lt => CompareOp::Ge,
        CompareOp::Gt => CompareOp::Le,
        CompareOp::Le => CompareOp::Gt,
        CompareOp::Ge => CompareOp::Lt,
        // contains has no clean inverse operator; wrap instead
        CompareOp::Contains => CompareOp::Contains,
    }
}

// split "a = b" on first top-level '='
fn split_once_char(s: &str, ch: char) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    for (i, c) in s.char_indices() {
        match in_str {
            Some(q) => {
                if c == q {
                    in_str = None;
                }
            }
            None => match c {
                '\'' | '"' => in_str = Some(c),
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                _ if c == ch && depth == 0 => {
                    return Some((&s[..i], &s[i + c.len_utf8()..]))
                }
                _ => {}
            },
        }
    }
    None
}

fn split_once_word<'a>(s: &'a str, word: &str) -> Option<(&'a str, &'a str)> {
    // find ` word ` at top level (not inside string)
    let bytes = s.as_bytes();
    let w = word.as_bytes();
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i + w.len() <= bytes.len() {
        let c = bytes[i];
        if let Some(q) = in_str {
            if c == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if c == b'\'' || c == b'"' {
            in_str = Some(c);
            i += 1;
            continue;
        }
        let before_ok = i == 0 || bytes[i - 1].is_ascii_whitespace();
        let after_idx = i + w.len();
        let after_ok = after_idx >= bytes.len() || bytes[after_idx].is_ascii_whitespace();
        if before_ok && after_ok && &bytes[i..after_idx] == w {
            return Some((&s[..i], &s[after_idx..]));
        }
        i += 1;
    }
    None
}

/// Split `for` trailing attributes (reversed / offset:N / limit:N) from the
/// iterable expression. Attributes always come after the iterable and contain
/// `:` (offset/limit) or are the bare word `reversed`.
fn split_for_attrs(s: &str) -> (String, Vec<(String, String)>) {
    // We scan from the RIGHT collecting attribute tokens.
    let mut attrs = Vec::new();
    let mut rest = s.trim().to_string();
    loop {
        let trimmed = rest.trim_end();
        // match trailing `reversed`
        if let Some(stripped) = trimmed.strip_suffix("reversed") {
            let boundary_ok = stripped
                .chars()
                .last()
                .map_or(true, |c| c.is_whitespace());
            if boundary_ok {
                attrs.push(("reversed".to_string(), String::new()));
                rest = stripped.to_string();
                continue;
            }
        }
        // match trailing `offset:EXPR` / `limit:EXPR`
        if let Some((k, v)) = trailing_kv(trimmed, "offset").or_else(|| trailing_kv(trimmed, "limit"))
        {
            attrs.push((k.clone(), v.1));
            rest = trimmed[..v.0].to_string();
            continue;
        }
        break;
    }
    attrs.reverse();
    (rest.trim().to_string(), attrs)
}

/// If `s` ends with `KEY:VALUE` (value = last whitespace-delimited chunk),
/// return (key, (start_index_of_key, value)).
fn trailing_kv(s: &str, key: &str) -> Option<(String, (usize, String))> {
    // find last occurrence of `key:`
    let pat = format!("{key}:");
    let idx = s.rfind(&pat)?;
    // ensure key start is at a word boundary
    if idx > 0 && !s.as_bytes()[idx - 1].is_ascii_whitespace() {
        return None;
    }
    let value = s[idx + pat.len()..].trim().to_string();
    // value must be a single token (no spaces) to be a real attribute
    if value.is_empty() || value.contains(char::is_whitespace) {
        return None;
    }
    Some((key.to_string(), (idx, value)))
}

/// Split `{% when %}` candidate values: Liquid allows both `,` and `or` as
/// separators (`{% when 'a', 'b' %}` / `{% when 'a' or 'b' %}`).
fn split_when_values(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for by_comma in split_commas(s) {
        // further split each on top-level ` or `
        let mut rest = by_comma;
        loop {
            if let Some((l, _, r)) = split_first_or(&rest) {
                out.push(l.trim().to_string());
                rest = r.to_string();
            } else {
                out.push(rest.trim().to_string());
                break;
            }
        }
    }
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

fn split_first_or(s: &str) -> Option<(String, &'static str, String)> {
    if let Some((l, r)) = split_once_word(s, "or") {
        return Some((l.to_string(), "or", r.to_string()));
    }
    None
}

fn split_include_name(s: &str) -> (&str, &str) {
    // name = up to first whitespace that is followed (eventually) by `key=`.
    // Simplest robust rule matching corpus: name is the first whitespace-
    // delimited token, UNLESS it's a dynamic `{{...}}...` which may contain
    // spaces inside `{{ }}`. Handle dynamic by scanning past balanced braces.
    let bytes = s.as_bytes();
    let mut i = 0;
    // skip leading ws
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // skip to matching }}
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'}' && bytes[i + 1] == b'}') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if bytes[i].is_ascii_whitespace() {
            break;
        }
        i += 1;
    }
    (&s[start..i], s[i..].trim_start())
}

fn parse_include_params(s: &str) -> Result<Vec<(String, Expr)>, ParseError> {
    let mut params = Vec::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // read key
        let ks = i;
        while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = s[ks..i].to_string();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            // valueless param — skip
            continue;
        }
        i += 1; // skip =
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // read value: quoted string or bare token (variable)
        let vs = i;
        if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            let q = bytes[i];
            i += 1;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            i += 1; // closing quote
        } else {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b',' {
                i += 1;
            }
        }
        let val_src = s[vs..i].trim();
        if !key.is_empty() {
            params.push((key, parse_expr(val_src)?));
        }
    }
    Ok(params)
}

// ------------------------------------------------------- expression parsing

/// Parse a full `{{ ... }}` / assign RHS expression: base term + filters.
pub fn parse_expr(src: &str) -> Result<Expr, ParseError> {
    let parts = split_pipes(src);
    let base = parse_term(parts[0].trim())?;
    let mut filters = Vec::new();
    for f in &parts[1..] {
        filters.push(parse_filter(f.trim())?);
    }
    Ok(Expr { base, filters })
}

fn parse_filter(src: &str) -> Result<FilterCall, ParseError> {
    // `name` or `name: arg1, arg2` or `name: k:v` (date)
    let (name, argsrc) = match split_once_char(src, ':') {
        Some((n, a)) => (n.trim().to_string(), a.trim().to_string()),
        None => (src.trim().to_string(), String::new()),
    };
    let mut args = Vec::new();
    let mut named = Vec::new();
    if !argsrc.is_empty() {
        for a in split_commas(&argsrc) {
            let a = a.trim();
            // named arg `k: v`? (only used by date-ish; keep support)
            if let Some((k, v)) = split_once_char(a, ':') {
                // heuristic: named only if key is a bare identifier
                if k.trim().chars().all(|c| c.is_alphanumeric() || c == '_') && !k.trim().is_empty()
                {
                    named.push((k.trim().to_string(), parse_term(v.trim())?));
                    continue;
                }
            }
            args.push(parse_term(a)?);
        }
    }
    Ok(FilterCall { name, args, named })
}

/// Parse a base term: literal, range, or variable path.
fn parse_term(src: &str) -> Result<Term, ParseError> {
    let mut s = src.trim();
    if s.is_empty() {
        return Ok(Term::Literal(Value::Nil));
    }
    // Jekyll quirk: `{{ expr }}` used INSIDE a tag (e.g.
    // `{% assign x = {{site.data.fhir.path}} | append: ... %}`) is interpolated
    // to the variable's value. Strip the braces and treat the inner as the
    // term. (Verified via oracle.)
    if s.starts_with("{{") && s.ends_with("}}") && s.len() >= 4 {
        s = s[2..s.len() - 2].trim();
    }
    // range `(a..b)`
    if s.starts_with('(') && s.ends_with(')') {
        let inner = &s[1..s.len() - 1];
        if let Some((a, b)) = inner.split_once("..") {
            return Ok(Term::Range(
                Box::new(parse_term(a.trim())?),
                Box::new(parse_term(b.trim())?),
            ));
        }
    }
    // string literal
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        return Ok(Term::Literal(Value::str(unescape(&s[1..s.len() - 1]))));
    }
    // number
    if let Ok(i) = s.parse::<i64>() {
        return Ok(Term::Literal(Value::Int(i)));
    }
    if let Ok(f) = s.parse::<f64>() {
        // ensure it's really numeric (not "1.2.3")
        if s.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e') {
            return Ok(Term::Literal(Value::Float(f)));
        }
    }
    // keywords
    match s {
        "true" => return Ok(Term::Literal(Value::Bool(true))),
        "false" => return Ok(Term::Literal(Value::Bool(false))),
        "nil" | "null" => return Ok(Term::Literal(Value::Nil)),
        "empty" => return Ok(Term::Var(VarPath { root: "empty".into(), segments: vec![] })),
        "blank" => return Ok(Term::Var(VarPath { root: "blank".into(), segments: vec![] })),
        _ => {}
    }
    // variable path
    Ok(Term::Var(parse_var_path(s)?))
}

fn parse_var_path(s: &str) -> Result<VarPath, ParseError> {
    let bytes = s.as_bytes();
    let mut i = 0;
    // root ident
    let rs = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-') {
        i += 1;
    }
    let root = s[rs..i].to_string();
    let mut segments = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                i += 1;
                // Tolerate `foo.[expr]` (a `.` directly before a bracket, as in
                // US Core's `site.data.[include.file]`): skip the empty field
                // and let the next iteration handle the `[`.
                if i < bytes.len() && bytes[i] == b'[' {
                    continue;
                }
                let fs = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-')
                {
                    i += 1;
                }
                segments.push(Segment::Field(s[fs..i].to_string()));
            }
            b'[' => {
                i += 1;
                let es = i;
                let mut depth = 1;
                while i < bytes.len() && depth > 0 {
                    match bytes[i] {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                let inner = &s[es..i];
                i += 1; // skip ]
                // The bracket may contain a full filtered expression
                // (e.g. `item["title" | trim]`). Parse it as an Expr.
                let expr = parse_expr(inner.trim())?;
                // Fast path: a bare string literal with no filters is a static
                // field access.
                if expr.filters.is_empty() {
                    if let Term::Literal(Value::Str(name)) = &expr.base {
                        segments.push(Segment::Field(name.to_string()));
                        continue;
                    }
                }
                segments.push(Segment::Index(expr));
            }
            _ => break,
        }
    }
    Ok(VarPath { root, segments })
}

// ------------------------------------------------------- condition parsing

/// Parse a boolean condition with `and`/`or` (Liquid: right-associative, no
/// precedence between and/or — strictly right-to-left).
pub fn parse_condition(src: &str) -> Result<Condition, ParseError> {
    let s = src.trim();
    // find the FIRST top-level `and`/`or` (Liquid evaluates right-assoc: the
    // parse is right-recursive, so we split on the first connective).
    if let Some((left, connective, right)) = split_first_connective(s) {
        let l = parse_single_condition(left.trim())?;
        let r = parse_condition(right.trim())?;
        return Ok(match connective {
            "and" => Condition::And(Box::new(l), Box::new(r)),
            _ => Condition::Or(Box::new(l), Box::new(r)),
        });
    }
    parse_single_condition(s)
}

fn split_first_connective(s: &str) -> Option<(&str, &str, &str)> {
    let bytes = s.as_bytes();
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = in_str {
            if c == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' {
            in_str = Some(c);
            i += 1;
            continue;
        }
        for (w, wl) in [("and", 3usize), ("or", 2usize)] {
            if i + wl <= bytes.len()
                && &s[i..i + wl] == w
                && (i == 0 || bytes[i - 1].is_ascii_whitespace())
                && (i + wl >= bytes.len() || bytes[i + wl].is_ascii_whitespace())
            {
                return Some((&s[..i], w, &s[i + wl..]));
            }
        }
        i += 1;
    }
    None
}

fn parse_single_condition(s: &str) -> Result<Condition, ParseError> {
    // comparison operators
    for (op_str, op) in [
        ("==", CompareOp::Eq),
        ("!=", CompareOp::Ne),
        ("<>", CompareOp::Ne),
        (">=", CompareOp::Ge),
        ("<=", CompareOp::Le),
        (">", CompareOp::Gt),
        ("<", CompareOp::Lt),
    ] {
        if let Some((l, r)) = split_top_level(s, op_str) {
            return Ok(Condition::Comparison {
                left: parse_expr(l.trim())?,
                op,
                right: parse_expr(r.trim())?,
            });
        }
    }
    // `contains` operator (word)
    if let Some((l, r)) = split_top_level_word(s, "contains") {
        return Ok(Condition::Comparison {
            left: parse_expr(l.trim())?,
            op: CompareOp::Contains,
            right: parse_expr(r.trim())?,
        });
    }
    // bare truthiness (may include filters, e.g. `x | size`)
    Ok(Condition::Truthy(parse_expr(s.trim())?))
}

fn split_top_level<'a>(s: &'a str, op: &str) -> Option<(&'a str, &'a str)> {
    let bytes = s.as_bytes();
    let ob = op.as_bytes();
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i + ob.len() <= bytes.len() {
        let c = bytes[i];
        if let Some(q) = in_str {
            if c == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' {
            in_str = Some(c);
            i += 1;
            continue;
        }
        if &bytes[i..i + ob.len()] == ob {
            // avoid matching `<` inside `<=`/`<>`, `>` inside `>=`
            return Some((&s[..i], &s[i + ob.len()..]));
        }
        i += 1;
    }
    None
}

fn split_top_level_word<'a>(s: &'a str, word: &str) -> Option<(&'a str, &'a str)> {
    split_once_word(s, word)
}

// ------------------------------------------------------- pipe/comma splitting

/// Split on top-level `|` (filter pipes), respecting quotes and brackets.
fn split_pipes(s: &str) -> Vec<String> {
    split_top(s, '|')
}

/// Split on top-level `,`.
fn split_commas(s: &str) -> Vec<String> {
    split_top(s, ',')
}

fn split_top(s: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str: Option<char> = None;
    let mut depth = 0i32;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match in_str {
            Some(q) => {
                cur.push(c);
                if c == q {
                    in_str = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    in_str = Some(c);
                    cur.push(c);
                }
                '(' | '[' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' | ']' => {
                    depth -= 1;
                    cur.push(c);
                }
                _ if c == delim && depth == 0 => {
                    out.push(cur.clone());
                    cur.clear();
                }
                _ => cur.push(c),
            },
        }
    }
    out.push(cur);
    out
}

fn unescape(s: &str) -> String {
    // Liquid string literals are mostly raw; handle escaped quote+backslash.
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&n) = chars.peek() {
                match n {
                    '"' | '\'' | '\\' => {
                        out.push(n);
                        chars.next();
                        continue;
                    }
                    _ => {}
                }
            }
        }
        out.push(c);
    }
    out
}
