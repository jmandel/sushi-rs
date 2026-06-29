//! Recursive-descent FSH parser + FSHImporter port. Consumes the byte-exact
//! token stream from `lex_document`, builds the `fsh_model` import AST with
//! 1-based inclusive source spans, resolves aliases, applies indentation/soft
//! -index path context, and expands parameterized RuleSets at parse time.
//!
//! Behavior mirrors `sushi-ts/src/import/FSHImporter.ts` (diagnostics omitted —
//! the AST parity gate only compares structure/spans). Span math follows
//! `extractStartStop` (spec 01 §4.5).

use std::collections::HashMap;

use fsh_model::*;

use crate::lex::lex_document;
use crate::token::{Channel, Token, TokenKind as K};

// ------------------------------------------------------------------ helpers

fn is_entity_keyword(k: K) -> bool {
    matches!(
        k,
        K::KW_ALIAS
            | K::KW_PROFILE
            | K::KW_EXTENSION
            | K::KW_INVARIANT
            | K::KW_INSTANCE
            | K::KW_VALUESET
            | K::KW_CODESYSTEM
            | K::KW_RULESET
            | K::KW_MAPPING
            | K::KW_LOGICAL
            | K::KW_RESOURCE
    )
}

fn is_flag(k: K) -> bool {
    matches!(
        k,
        K::KW_MS | K::KW_SU | K::KW_MOD | K::KW_TU | K::KW_NORMATIVE | K::KW_DRAFT
    )
}

/// A path/name token alternative: SEQUENCE | NUMBER | DATETIME | TIME | mostAlphaKeywords.
fn is_path_token(k: K) -> bool {
    matches!(
        k,
        K::SEQUENCE
            | K::NUMBER
            | K::DATETIME
            | K::TIME
            | K::KW_MS
            | K::KW_SU
            | K::KW_TU
            | K::KW_NORMATIVE
            | K::KW_DRAFT
            | K::KW_FROM
            | K::KW_CONTAINS
            | K::KW_NAMED
            | K::KW_AND
            | K::KW_ONLY
            | K::KW_OR
            | K::KW_OBEYS
            | K::KW_TRUE
            | K::KW_FALSE
            | K::KW_INCLUDE
            | K::KW_EXCLUDE
            | K::KW_CODES
            | K::KW_WHERE
            | K::KW_VSREFERENCE
            | K::KW_SYSTEM
            | K::KW_CONTENTREFERENCE
    )
}

fn utf16_len(s: &str) -> i64 {
    s.encode_utf16().count() as i64
}

fn star_col(text: &str) -> u32 {
    let after = match text.rfind('\n') {
        Some(i) => &text[i + 1..],
        None => text,
    };
    (utf16_len(after) - 1) as u32
}

fn loc(start: &Token, stop: &Token) -> Location {
    let (start_line, start_column) = if start.kind == K::STAR {
        (start.line + 1, star_col(&start.text))
    } else {
        (start.line, start.col + 1)
    };
    Location {
        start_line,
        start_column,
        end_line: stop.line,
        end_column: (stop.stop - stop.start + stop.col as i64 + 1) as u32,
    }
}

fn loc_tok(t: &Token) -> Location {
    Location {
        start_line: t.line,
        start_column: t.col + 1,
        end_line: t.line,
        end_column: (t.stop - t.start + t.col as i64 + 1) as u32,
    }
}

/// Extract a substring of `s` by inclusive UTF-16 offsets [start, stop].
fn utf16_slice(s: &str, start: i64, stop: i64) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    if start < 0 || stop < start || stop as usize >= units.len() {
        return String::new();
    }
    String::from_utf16_lossy(&units[start as usize..=stop as usize])
}

/// split a path on '.' that are not within square brackets (splitOnPathPeriods).
fn split_on_path_periods(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in path.chars() {
        match c {
            '[' => {
                depth += 1;
                cur.push(c);
            }
            ']' => {
                depth -= 1;
                cur.push(c);
            }
            '.' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn split_path(path: &str) -> Vec<String> {
    if path == "." {
        vec![".".to_string()]
    } else {
        split_on_path_periods(path)
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect()
    }
}

// ----- string unescaping -----

fn unescape_unicode(seg: &str) -> String {
    // replace \uXXXX
    let mut out = String::new();
    let chars: Vec<char> = seg.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1] == 'u' && i + 5 < chars.len() {
            let hex: String = chars[i + 2..i + 6].iter().collect();
            if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                    if let Some(ch) = char::from_u32(cp) {
                        out.push(ch);
                        i += 6;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn unescape_quoted_string(s: &str) -> String {
    // strip surrounding quotes
    let inner = if s.len() >= 2 {
        &s[1..s.len() - 1]
    } else {
        ""
    };
    // split on \\ , unescape each, rejoin with literal backslash
    let parts: Vec<&str> = inner.split("\\\\").collect();
    let replaced: Vec<String> = parts
        .iter()
        .map(|seg| {
            let s = unescape_unicode(seg);
            s.replace("\\\"", "\"")
                .replace("\\n", "\n")
                .replace("\\r", "\r")
                .replace("\\t", "\t")
        })
        .collect();
    replaced.join("\\")
}

fn extract_string(text: &str) -> String {
    unescape_quoted_string(text)
}

fn extract_multiline_string(text: &str) -> String {
    // strip leading/trailing """
    let mlstr = if text.len() >= 6 {
        &text[3..text.len() - 3]
    } else {
        ""
    };
    let mut lines: Vec<String> = mlstr
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .map(|l| {
            let s = unescape_unicode(l);
            s.replace("\\n", "\n").replace("\\r", "\r").replace("\\t", "\t")
        })
        .collect();
    // drop first line if only whitespace
    if !lines.is_empty() && lines[0].trim().is_empty() {
        lines.remove(0);
    }
    if !lines.is_empty() && lines[lines.len() - 1].trim().is_empty() {
        lines.pop();
    }
    // blank out whitespace-only interior lines
    for l in lines.iter_mut() {
        if l.trim().is_empty() {
            l.clear();
        }
    }
    // min leading spaces over non-empty lines
    let min_spaces = lines
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().take_while(|c| *c == ' ').count())
        .min();
    if let Some(min) = min_spaces {
        for l in lines.iter_mut() {
            if l.chars().count() >= min {
                *l = l.chars().skip(min).collect();
            }
        }
    }
    lines.join("\n")
}

fn parse_code_lexeme(text: &str) -> (String, Option<String>) {
    // returns (code, system?)
    // find split point: /(^|[^\\])(\\\\)*#/  -- first unescaped #
    let chars: Vec<char> = text.chars().collect();
    let mut split_idx: Option<usize> = None;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' {
            // count preceding backslashes
            let mut bs = 0;
            let mut j = i;
            while j > 0 && chars[j - 1] == '\\' {
                bs += 1;
                j -= 1;
            }
            if bs % 2 == 0 {
                split_idx = Some(i);
                break;
            }
        }
        i += 1;
    }
    let (mut system, mut code) = match split_idx {
        None => {
            let code = if text.starts_with('#') {
                text[1..].to_string()
            } else {
                String::new()
            };
            (String::new(), code)
        }
        Some(idx) => {
            let sys: String = chars[..idx].iter().collect();
            let cd: String = chars[idx + 1..].iter().collect();
            (sys, cd)
        }
    };
    system = system.replace("\\\\", "\\").replace("\\#", "#");
    if code.starts_with('"') && code.ends_with('"') && code.len() >= 2 {
        code = code[1..code.len() - 1]
            .replace("\\\\", "\\")
            .replace("\\\"", "\"");
    }
    let sys = if system.is_empty() { None } else { Some(system) };
    (code, sys)
}

fn parse_or_split(reference: &str) -> Vec<String> {
    // text inside Reference( ... ) split on \s+or\s+
    let open = reference.find('(').map(|i| i + 1).unwrap_or(0);
    let close = reference.rfind(')').unwrap_or(reference.len());
    let inner = if close >= open {
        &reference[open..close]
    } else {
        ""
    };
    split_ws_or(inner)
}

fn split_ws_or(inner: &str) -> Vec<String> {
    // split on whitespace-surrounded "or"
    let mut out = Vec::new();
    let tokens: Vec<&str> = inner.split_whitespace().collect();
    let mut cur: Vec<&str> = Vec::new();
    for t in tokens {
        if t == "or" {
            out.push(cur.join(" "));
            cur.clear();
        } else {
            cur.push(t);
        }
    }
    out.push(cur.join(" "));
    out.into_iter().map(|s| s.trim().to_string()).collect()
}

/// extractNumberValue: integer -> bigint (decimal string), else float.
fn extract_number_value(num: &str) -> Value {
    // /([-+]?\d+)(\.\d+)?([eE][-+]?\d+)?/
    let bytes: Vec<char> = num.chars().collect();
    let mut i = 0;
    let mut whole = String::new();
    if i < bytes.len() && (bytes[i] == '-' || bytes[i] == '+') {
        whole.push(bytes[i]);
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        whole.push(bytes[i]);
        i += 1;
    }
    let mut decimal = String::new();
    if i < bytes.len() && bytes[i] == '.' {
        decimal.push('.');
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            decimal.push(bytes[i]);
            i += 1;
        }
    }
    let mut exp = String::new();
    if i < bytes.len() && (bytes[i] == 'e' || bytes[i] == 'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == '-' || bytes[i] == '+') {
            exp.push(bytes[i]);
            i += 1;
        }
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            exp.push(bytes[i]);
            i += 1;
        }
    }
    let exp_val: i64 = if exp.is_empty() {
        0
    } else {
        exp.parse().unwrap_or(0)
    };
    let float_fallback = || Value::Float(num.parse::<f64>().unwrap_or(0.0));

    if !decimal.is_empty() {
        // /\.(\d*[1-9])0*/
        let dec_digits = &decimal[1..];
        // trimmed = leading up to last nonzero
        let trimmed: String = match dec_digits.rfind(|c: char| c != '0') {
            Some(idx) => dec_digits[..=idx].to_string(),
            None => String::new(),
        };
        if !trimmed.is_empty() {
            let tl = trimmed.len() as i64;
            if tl <= exp_val {
                // BigInt(whole+trimmed) * 10^(exp - tl)
                return bigint_mul_pow10(&format!("{}{}", whole, trimmed), exp_val - tl);
            } else {
                return float_fallback();
            }
        }
    }
    // no decimal (or all zeros)
    // wholeZeroes from /[-+]?\d*[1-9](0*)/
    let trailing_zeros = trailing_zero_count(&whole);
    let remaining = trailing_zeros + exp_val;
    if remaining >= 0 {
        if exp_val < 0 {
            return bigint_div_pow10(&whole, -exp_val);
        } else {
            return bigint_mul_pow10(&whole, exp_val);
        }
    }
    float_fallback()
}

fn trailing_zero_count(whole: &str) -> i64 {
    // count trailing zeros after the last nonzero digit; if no nonzero -> 0
    let digits: String = whole.chars().filter(|c| c.is_ascii_digit()).collect();
    match digits.rfind(|c: char| c != '0') {
        Some(idx) => (digits.len() - idx - 1) as i64,
        None => 0,
    }
}

fn bigint_mul_pow10(whole: &str, pow: i64) -> Value {
    let (sign, digits) = split_sign(whole);
    let mut d = digits.to_string();
    for _ in 0..pow {
        d.push('0');
    }
    Value::BigInt(normalize_int(sign, &d))
}

fn bigint_div_pow10(whole: &str, pow: i64) -> Value {
    let (sign, digits) = split_sign(whole);
    let p = pow as usize;
    let d = if digits.len() > p {
        digits[..digits.len() - p].to_string()
    } else {
        "0".to_string()
    };
    Value::BigInt(normalize_int(sign, &d))
}

fn split_sign(s: &str) -> (&str, &str) {
    if let Some(rest) = s.strip_prefix('-') {
        ("-", rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        ("", rest)
    } else {
        ("", s)
    }
}

fn normalize_int(sign: &str, digits: &str) -> String {
    let trimmed = digits.trim_start_matches('0');
    let core = if trimmed.is_empty() { "0" } else { trimmed };
    if core == "0" {
        "0".to_string()
    } else {
        format!("{}{}", sign, core)
    }
}

// ------------------------------------------------------------------ cursor

struct Cursor {
    toks: Vec<Token>,
    pos: usize,
}

impl Cursor {
    fn new(toks: Vec<Token>) -> Self {
        Cursor { toks, pos: 0 }
    }
    fn kind(&self) -> K {
        self.toks.get(self.pos).map(|t| t.kind).unwrap_or(K::EOF)
    }
    fn la(&self, n: usize) -> K {
        self.toks.get(self.pos + n).map(|t| t.kind).unwrap_or(K::EOF)
    }
    fn tok(&self) -> &Token {
        &self.toks[self.pos]
    }
    fn at(&self, i: usize) -> &Token {
        &self.toks[i]
    }
    fn advance(&mut self) -> usize {
        let i = self.pos;
        self.pos += 1;
        i
    }
}

// ------------------------------------------------------------------ importer

pub struct Importer {
    pub docs: Vec<FshDocument>,
    all_aliases: HashMap<String, String>,
    param_rule_sets: HashMap<String, ParamRuleSet>,
    path_context: Vec<Vec<String>>,
    current_file: String,
    current_doc: usize,
    top_level_parse: bool,
}

impl Importer {
    pub fn new() -> Self {
        Importer {
            docs: Vec::new(),
            all_aliases: HashMap::new(),
            param_rule_sets: HashMap::new(),
            path_context: Vec::new(),
            current_file: String::new(),
            current_doc: 0,
            top_level_parse: true,
        }
    }

    /// Two-pass import of (path, content) files.
    pub fn import(&mut self, files: &[(&str, &str)]) {
        // store appended content + tokens per file
        let mut per_file: Vec<(String, Vec<Token>, Vec<(usize, usize)>)> = Vec::new();

        for (path, content) in files {
            let appended = if content.ends_with('\n') {
                content.to_string()
            } else {
                format!("{}\n", content)
            };
            let toks: Vec<Token> = lex_document(content)
                .into_iter()
                .filter(|t| t.channel == Channel::Default && t.kind != K::EOF)
                .collect();
            let ranges = entity_ranges(&toks);
            let doc = FshDocument::new(path);
            self.docs.push(doc);
            self.current_doc = self.docs.len() - 1;
            self.current_file = path.to_string();

            // pass 1: aliases + param rule sets
            for &(s, e) in &ranges {
                let kw = toks[s].kind;
                if kw == K::KW_ALIAS {
                    self.collect_alias(&toks, s, e);
                } else if kw == K::KW_RULESET && toks.get(s + 1).map(|t| t.kind) == Some(K::PARAM_RULESET_REFERENCE)
                {
                    self.collect_param_ruleset(&appended, &toks, s, e);
                }
            }
            per_file.push((appended, toks, ranges));
        }

        // pass 2: visit entities
        for (idx, (_appended, toks, ranges)) in per_file.iter().enumerate() {
            self.current_doc = idx;
            self.current_file = self.docs[idx].file.clone();
            for &(s, e) in ranges {
                let kw = toks[s].kind;
                if kw == K::KW_ALIAS {
                    continue;
                }
                if kw == K::KW_RULESET
                    && toks.get(s + 1).map(|t| t.kind) == Some(K::PARAM_RULESET_REFERENCE)
                {
                    continue; // handled in pass 1
                }
                let slice = toks[s..e].to_vec();
                let mut cur = Cursor::new(slice);
                self.path_context = Vec::new();
                self.visit_entity(&mut cur);
            }
        }
    }

    fn doc(&mut self) -> &mut FshDocument {
        &mut self.docs[self.current_doc]
    }

    // ---------- pass 1 collectors ----------

    fn collect_alias(&mut self, toks: &[Token], s: usize, e: usize) {
        // alias: KW_ALIAS name EQUAL (SEQUENCE | CODE)
        // name token at s+1; value token after EQUAL
        let name = toks.get(s + 1).map(|t| t.text.clone()).unwrap_or_default();
        // find value: token after EQUAL
        let mut value = String::new();
        let mut i = s + 1;
        while i < e {
            if toks[i].kind == K::EQUAL {
                if let Some(v) = toks.get(i + 1) {
                    if v.kind == K::SEQUENCE || v.kind == K::CODE {
                        value = v.text.clone();
                    }
                }
                break;
            }
            i += 1;
        }
        if name.contains('|') {
            return;
        }
        let dup = self.all_aliases.get(&name).map(|v| v != &value).unwrap_or(false);
        if dup {
            return; // keep original
        }
        self.all_aliases.insert(name.clone(), value.clone());
        // doc.aliases (first wins per name within doc)
        let exists = self.doc().aliases.iter().any(|(k, _)| k == &name);
        if !exists {
            self.doc().aliases.push((name, value));
        }
    }

    fn collect_param_ruleset(&mut self, appended: &str, toks: &[Token], s: usize, e: usize) {
        // KW_RULESET PARAM_RULESET_REFERENCE parameter* lastParameter paramRuleSetContent
        // name + params from paramRuleSetRef
        let ref_tok = &toks[s + 1];
        let name = trim(slice_off_last(&ref_tok.text)); // "Param(" -> "Param"
        // params: parameter* lastParameter, each slice(0,-1).trim()
        let mut params = Vec::new();
        let mut star_idx = None;
        let mut i = s + 2;
        while i < e {
            match toks[i].kind {
                K::BRACKETED_PARAM | K::PLAIN_PARAM | K::LAST_BRACKETED_PARAM | K::LAST_PLAIN_PARAM => {
                    params.push(trim(slice_off_last(&toks[i].text)));
                }
                K::STAR => {
                    star_idx = Some(i);
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        let (start_tok, stop_tok) = (&toks[s], &toks[e - 1]);
        let location = loc(start_tok, stop_tok);
        // contents from STAR.start .. last.stop
        let contents = if let Some(si) = star_idx {
            utf16_slice(appended, toks[si].start, toks[e - 1].stop)
        } else {
            String::new()
        };
        if self.param_rule_sets.contains_key(&name) {
            return;
        }
        let prs = ParamRuleSet {
            source_info: SourceInfo::new(&self.current_file, location),
            name: name.clone(),
            parameters: params,
            contents,
        };
        self.param_rule_sets.insert(name, prs);
    }

    // ---------- entity dispatch ----------

    fn visit_entity(&mut self, cur: &mut Cursor) {
        match cur.kind() {
            K::KW_PROFILE => self.visit_structure(cur, StructureKind::Profile),
            K::KW_EXTENSION => self.visit_structure(cur, StructureKind::Extension),
            K::KW_RESOURCE => self.visit_structure(cur, StructureKind::Resource),
            K::KW_LOGICAL => self.visit_structure(cur, StructureKind::Logical),
            K::KW_INSTANCE => self.visit_instance(cur),
            K::KW_VALUESET => self.visit_valueset(cur),
            K::KW_CODESYSTEM => self.visit_codesystem(cur),
            K::KW_INVARIANT => self.visit_invariant(cur),
            K::KW_RULESET => self.visit_ruleset(cur),
            K::KW_MAPPING => self.visit_mapping(cur),
            _ => {}
        }
    }

    fn entity_loc(&self, cur: &Cursor) -> Location {
        loc(cur.at(0), cur.at(cur.toks.len() - 1))
    }

    fn dup_in_docs(&self, kind: &str, name: &str) -> bool {
        self.docs.iter().any(|d| match kind {
            "profiles" => d.profiles.iter().any(|(k, _)| k == name),
            "extensions" => d.extensions.iter().any(|(k, _)| k == name),
            "resources" => d.resources.iter().any(|(k, _)| k == name),
            "logicals" => d.logicals.iter().any(|(k, _)| k == name),
            "instances" => d.instances.iter().any(|(k, _)| k == name),
            "valueSets" => d.value_sets.iter().any(|(k, _)| k == name),
            "codeSystems" => d.code_systems.iter().any(|(k, _)| k == name),
            "invariants" => d.invariants.iter().any(|(k, _)| k == name),
            "ruleSets" => d.rule_sets.iter().any(|(k, _)| k == name),
            "mappings" => d.mappings.iter().any(|(k, _)| k == name),
            _ => false,
        })
    }

    // ---------- structures (profile/extension/logical/resource) ----------

    fn visit_structure(&mut self, cur: &mut Cursor, kind: StructureKind) {
        let location = self.entity_loc(cur);
        cur.advance(); // keyword
        let name = cur.tok().text.clone();
        cur.advance(); // name
        let map = match kind {
            StructureKind::Profile => "profiles",
            StructureKind::Extension => "extensions",
            StructureKind::Logical => "logicals",
            StructureKind::Resource => "resources",
        };
        if self.dup_in_docs(map, &name) {
            return;
        }
        let parent_default = match kind {
            StructureKind::Profile => None,
            StructureKind::Extension => Some("Extension".to_string()),
            StructureKind::Logical => Some("Base".to_string()),
            StructureKind::Resource => Some("DomainResource".to_string()),
        };
        let mut def = StructureDef {
            kind,
            source_info: SourceInfo::new(&self.current_file, location),
            name: name.clone(),
            id: name.clone(),
            parent: parent_default,
            title: None,
            description: None,
            rules: Vec::new(),
            contexts: Vec::new(),
            characteristics: Vec::new(),
        };

        // metadata
        let mut seen: Vec<&'static str> = Vec::new();
        loop {
            match cur.kind() {
                K::KW_PARENT => {
                    cur.advance();
                    let v = self.alias_aware(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"Parent") {
                        seen.push("Parent");
                        def.parent = Some(v);
                    }
                }
                K::KW_ID => {
                    cur.advance();
                    let v = cur.tok().text.clone();
                    cur.advance();
                    if !seen.contains(&"Id") {
                        seen.push("Id");
                        def.id = v;
                    }
                }
                K::KW_TITLE => {
                    cur.advance();
                    let v = extract_string(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"Title") {
                        seen.push("Title");
                        def.title = Some(v);
                    }
                }
                K::KW_DESCRIPTION => {
                    cur.advance();
                    let v = self.string_or_multiline(cur);
                    if !seen.contains(&"Description") {
                        seen.push("Description");
                        def.description = Some(v);
                    }
                }
                K::KW_CONTEXT if kind == StructureKind::Extension => {
                    let ctxs = self.parse_context(cur);
                    if def.contexts.is_empty() {
                        def.contexts = ctxs;
                    }
                }
                K::KW_CHARACTERISTICS if kind == StructureKind::Logical => {
                    let chars = self.parse_characteristics(cur);
                    if def.characteristics.is_empty() {
                        def.characteristics = chars;
                    }
                }
                _ => break,
            }
        }

        // rules
        while cur.kind() == K::STAR {
            let rules = match kind {
                StructureKind::Logical | StructureKind::Resource => self.parse_lr_rule(cur),
                _ => self.parse_sd_rule(cur),
            };
            def.rules.extend(rules);
        }

        match kind {
            StructureKind::Profile => self.doc().profiles.push((name, def)),
            StructureKind::Extension => self.doc().extensions.push((name, def)),
            StructureKind::Logical => self.doc().logicals.push((name, def)),
            StructureKind::Resource => self.doc().resources.push((name, def)),
        }
    }

    fn visit_instance(&mut self, cur: &mut Cursor) {
        let location = self.entity_loc(cur);
        cur.advance();
        let name = cur.tok().text.clone();
        cur.advance();
        if self.dup_in_docs("instances", &name) {
            return;
        }
        let mut inst = Instance {
            source_info: SourceInfo::new(&self.current_file, location),
            name: name.clone(),
            id: name.clone(),
            instance_of: None,
            title: None,
            description: None,
            usage: "Example".to_string(),
            rules: Vec::new(),
        };
        let mut seen: Vec<&'static str> = Vec::new();
        loop {
            match cur.kind() {
                K::KW_INSTANCEOF => {
                    cur.advance();
                    let v = self.alias_aware(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"InstanceOf") {
                        seen.push("InstanceOf");
                        inst.instance_of = Some(v);
                    }
                }
                K::KW_TITLE => {
                    cur.advance();
                    let v = extract_string(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"Title") {
                        seen.push("Title");
                        inst.title = Some(v);
                    }
                }
                K::KW_DESCRIPTION => {
                    cur.advance();
                    let v = self.string_or_multiline(cur);
                    if !seen.contains(&"Description") {
                        seen.push("Description");
                        inst.description = Some(v);
                    }
                }
                K::KW_USAGE => {
                    cur.advance();
                    let (code, _sys) = parse_code_lexeme(&cur.tok().text);
                    cur.advance();
                    let usage = upper_first(&code);
                    let usage = if matches!(usage.as_str(), "Example" | "Definition" | "Inline") {
                        usage
                    } else {
                        "Example".to_string()
                    };
                    if !seen.contains(&"Usage") {
                        seen.push("Usage");
                        inst.usage = usage;
                    }
                }
                _ => break,
            }
        }
        if inst.instance_of.is_none() {
            return; // RequiredMetadataError -> not added
        }
        while cur.kind() == K::STAR {
            if let Some(rule) = self.parse_instance_rule(cur) {
                inst.rules.push(rule);
            }
        }
        self.doc().instances.push((name, inst));
    }

    fn visit_valueset(&mut self, cur: &mut Cursor) {
        let location = self.entity_loc(cur);
        cur.advance();
        let name = cur.tok().text.clone();
        cur.advance();
        if self.dup_in_docs("valueSets", &name) {
            return;
        }
        let mut vs = FshValueSet {
            source_info: SourceInfo::new(&self.current_file, location),
            name: name.clone(),
            id: name.clone(),
            title: None,
            description: None,
            rules: Vec::new(),
        };
        let mut seen: Vec<&'static str> = Vec::new();
        loop {
            match cur.kind() {
                K::KW_ID => {
                    cur.advance();
                    let v = cur.tok().text.clone();
                    cur.advance();
                    if !seen.contains(&"Id") {
                        seen.push("Id");
                        vs.id = v;
                    }
                }
                K::KW_TITLE => {
                    cur.advance();
                    let v = extract_string(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"Title") {
                        seen.push("Title");
                        vs.title = Some(v);
                    }
                }
                K::KW_DESCRIPTION => {
                    cur.advance();
                    let v = self.string_or_multiline(cur);
                    if !seen.contains(&"Description") {
                        seen.push("Description");
                        vs.description = Some(v);
                    }
                }
                _ => break,
            }
        }
        while cur.kind() == K::STAR {
            if let Some(rule) = self.parse_vs_rule(cur) {
                self.merge_vs_rule(&mut vs.rules, rule);
            }
        }
        self.doc().value_sets.push((name, vs));
    }

    fn merge_vs_rule(&self, rules: &mut Vec<Rule>, rule: Rule) {
        if let Rule::VsConcept {
            inclusion,
            from,
            concepts,
            ..
        } = &rule
        {
            // try merge into existing concept component with same inclusion + from
            for existing in rules.iter_mut() {
                if let Rule::VsConcept {
                    inclusion: ei,
                    from: ef,
                    concepts: ec,
                    ..
                } = existing
                {
                    if ei == inclusion
                        && ef.system == from.system
                        && sorted_opt(&ef.value_sets) == sorted_opt(&from.value_sets)
                    {
                        ec.extend(concepts.clone());
                        return;
                    }
                }
            }
        }
        rules.push(rule);
    }

    fn visit_codesystem(&mut self, cur: &mut Cursor) {
        let location = self.entity_loc(cur);
        cur.advance();
        let name = cur.tok().text.clone();
        cur.advance();
        if self.dup_in_docs("codeSystems", &name) {
            return;
        }
        let mut cs = FshCodeSystem {
            source_info: SourceInfo::new(&self.current_file, location),
            name: name.clone(),
            id: name.clone(),
            title: None,
            description: None,
            rules: Vec::new(),
        };
        let mut seen: Vec<&'static str> = Vec::new();
        loop {
            match cur.kind() {
                K::KW_ID => {
                    cur.advance();
                    let v = cur.tok().text.clone();
                    cur.advance();
                    if !seen.contains(&"Id") {
                        seen.push("Id");
                        cs.id = v;
                    }
                }
                K::KW_TITLE => {
                    cur.advance();
                    let v = extract_string(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"Title") {
                        seen.push("Title");
                        cs.title = Some(v);
                    }
                }
                K::KW_DESCRIPTION => {
                    cur.advance();
                    let v = self.string_or_multiline(cur);
                    if !seen.contains(&"Description") {
                        seen.push("Description");
                        cs.description = Some(v);
                    }
                }
                _ => break,
            }
        }
        while cur.kind() == K::STAR {
            if let Some(rule) = self.parse_cs_rule(cur) {
                cs.rules.push(rule);
            }
        }
        self.doc().code_systems.push((name, cs));
    }

    fn visit_invariant(&mut self, cur: &mut Cursor) {
        let location = self.entity_loc(cur);
        cur.advance();
        let name = cur.tok().text.clone();
        cur.advance();
        if self.dup_in_docs("invariants", &name) {
            return;
        }
        let mut inv = Invariant {
            source_info: SourceInfo::new(&self.current_file, location),
            name: name.clone(),
            description: None,
            expression: None,
            xpath: None,
            severity: None,
            rules: Vec::new(),
        };
        let mut seen: Vec<&'static str> = Vec::new();
        loop {
            match cur.kind() {
                K::KW_DESCRIPTION => {
                    cur.advance();
                    let v = self.string_or_multiline(cur);
                    if !seen.contains(&"Description") {
                        seen.push("Description");
                        inv.description = Some(v);
                    }
                }
                K::KW_EXPRESSION => {
                    cur.advance();
                    let v = self.string_or_multiline(cur);
                    if !seen.contains(&"Expression") {
                        seen.push("Expression");
                        inv.expression = Some(v);
                    }
                }
                K::KW_XPATH => {
                    cur.advance();
                    let v = extract_string(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"XPath") {
                        seen.push("XPath");
                        inv.xpath = Some(v);
                    }
                }
                K::KW_SEVERITY => {
                    cur.advance();
                    let t = cur.tok().clone();
                    let (code, sys) = parse_code_lexeme(&t.text);
                    let system = sys.map(|s| self.alias_aware(&s));
                    cur.advance();
                    if !seen.contains(&"Severity") {
                        seen.push("Severity");
                        inv.severity = Some(FshCode {
                            source_info: SourceInfo::new(&self.current_file, loc_tok(&t)),
                            code,
                            system,
                            display: None,
                        });
                    }
                }
                _ => break,
            }
        }
        while cur.kind() == K::STAR {
            if let Some(rule) = self.parse_invariant_rule(cur) {
                inv.rules.push(rule);
            }
        }
        self.doc().invariants.push((name, inv));
    }

    fn visit_ruleset(&mut self, cur: &mut Cursor) {
        let location = self.entity_loc(cur);
        cur.advance();
        // RULESET_REFERENCE
        let name = trim(cur.tok().text.clone());
        cur.advance();
        if self.dup_in_docs("ruleSets", &name) {
            return;
        }
        let mut rs = RuleSet {
            source_info: SourceInfo::new(&self.current_file, location),
            name: name.clone(),
            rules: Vec::new(),
        };
        while cur.kind() == K::STAR {
            let rules = self.parse_ruleset_rule(cur);
            rs.rules.extend(rules);
        }
        self.doc().rule_sets.push((name, rs));
    }

    fn visit_mapping(&mut self, cur: &mut Cursor) {
        let location = self.entity_loc(cur);
        cur.advance();
        let name = cur.tok().text.clone();
        cur.advance();
        if self.dup_in_docs("mappings", &name) {
            return;
        }
        let mut m = Mapping {
            source_info: SourceInfo::new(&self.current_file, location),
            name: name.clone(),
            id: name.clone(),
            source: None,
            target: None,
            title: None,
            description: None,
            rules: Vec::new(),
        };
        let mut seen: Vec<&'static str> = Vec::new();
        loop {
            match cur.kind() {
                K::KW_ID => {
                    cur.advance();
                    let v = cur.tok().text.clone();
                    cur.advance();
                    if !seen.contains(&"Id") {
                        seen.push("Id");
                        m.id = v;
                    }
                }
                K::KW_SOURCE => {
                    cur.advance();
                    let v = self.alias_aware(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"Source") {
                        seen.push("Source");
                        m.source = Some(v);
                    }
                }
                K::KW_TARGET => {
                    cur.advance();
                    let v = extract_string(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"Target") {
                        seen.push("Target");
                        m.target = Some(v);
                    }
                }
                K::KW_DESCRIPTION => {
                    cur.advance();
                    let v = self.string_or_multiline(cur);
                    if !seen.contains(&"Description") {
                        seen.push("Description");
                        m.description = Some(v);
                    }
                }
                K::KW_TITLE => {
                    cur.advance();
                    let v = extract_string(&cur.tok().text);
                    cur.advance();
                    if !seen.contains(&"Title") {
                        seen.push("Title");
                        m.title = Some(v);
                    }
                }
                _ => break,
            }
        }
        while cur.kind() == K::STAR {
            if let Some(rule) = self.parse_mapping_entity_rule(cur) {
                m.rules.push(rule);
            }
        }
        self.doc().mappings.push((name, m));
    }

    // ---------- metadata helpers ----------

    fn string_or_multiline(&mut self, cur: &mut Cursor) -> String {
        let t = cur.tok();
        let v = if t.kind == K::MULTILINE_STRING {
            extract_multiline_string(&t.text)
        } else {
            extract_string(&t.text)
        };
        cur.advance();
        v
    }

    fn parse_context(&mut self, cur: &mut Cursor) -> Vec<ExtensionContext> {
        // KW_CONTEXT contextItem* lastContextItem
        cur.advance(); // KW_CONTEXT
        let mut out = Vec::new();
        loop {
            match cur.kind() {
                K::QUOTED_CONTEXT => {
                    let t = cur.tok().clone();
                    cur.advance();
                    let raw = slice_off_last(&t.text);
                    out.push(ExtensionContext {
                        value: unescape_quoted_string(raw.trim()),
                        is_quoted: true,
                        source_info: SourceInfo::new(&self.current_file, loc_tok(&t)),
                    });
                }
                K::UNQUOTED_CONTEXT => {
                    let t = cur.tok().clone();
                    cur.advance();
                    out.push(ExtensionContext {
                        value: slice_off_last(&t.text).trim().to_string(),
                        is_quoted: false,
                        source_info: SourceInfo::new(&self.current_file, loc_tok(&t)),
                    });
                }
                K::LAST_QUOTED_CONTEXT => {
                    let t = cur.tok().clone();
                    cur.advance();
                    out.push(ExtensionContext {
                        value: unescape_quoted_string(&t.text),
                        is_quoted: true,
                        source_info: SourceInfo::new(&self.current_file, loc_tok(&t)),
                    });
                    break;
                }
                K::LAST_UNQUOTED_CONTEXT => {
                    let t = cur.tok().clone();
                    cur.advance();
                    out.push(ExtensionContext {
                        value: t.text.clone(),
                        is_quoted: false,
                        source_info: SourceInfo::new(&self.current_file, loc_tok(&t)),
                    });
                    break;
                }
                _ => break,
            }
        }
        out
    }

    fn parse_characteristics(&mut self, cur: &mut Cursor) -> Vec<String> {
        cur.advance(); // KW_CHARACTERISTICS
        let mut out = Vec::new();
        loop {
            match cur.kind() {
                K::CODE_ITEM => {
                    let t = slice_off_last(&cur.tok().text).trim().to_string();
                    out.push(t.trim_start_matches('#').to_string());
                    cur.advance();
                }
                K::LAST_CODE_ITEM => {
                    let t = cur.tok().text.trim().to_string();
                    out.push(t.trim_start_matches('#').to_string());
                    cur.advance();
                    break;
                }
                _ => break,
            }
        }
        out
    }

    // ---------- alias resolution ----------

    fn alias_aware(&self, value: &str) -> String {
        let mut it = value.splitn(2, '|');
        let without = it.next().unwrap_or("");
        let version = it.next();
        if let Some(resolved) = self.all_aliases.get(without) {
            match version {
                Some(v) => format!("{}|{}", resolved, v),
                None => resolved.clone(),
            }
        } else {
            value.to_string()
        }
    }

    // ---------- rule dispatchers ----------

    fn parse_sd_rule(&mut self, cur: &mut Cursor) -> Vec<Rule> {
        self.parse_rule_generic(cur, RuleHost::Sd)
    }
    fn parse_lr_rule(&mut self, cur: &mut Cursor) -> Vec<Rule> {
        self.parse_rule_generic(cur, RuleHost::Lr)
    }
    fn parse_ruleset_rule(&mut self, cur: &mut Cursor) -> Vec<Rule> {
        self.parse_rule_generic(cur, RuleHost::RuleSet)
    }

    fn parse_instance_rule(&mut self, cur: &mut Cursor) -> Option<Rule> {
        // fixedValueRule | insertRule | pathRule(isInstanceRule=true)
        let start = cur.pos;
        let star = cur.tok().clone();
        cur.advance();
        // insert (no path)?
        if cur.kind() == K::KW_INSERT {
            return self.finish_insert(cur, start, &star, "", false);
        }
        // caret? not in instance grammar -> but fixedValue handles path? = ...
        // path
        let (local_path, _had_path) = self.read_path(cur);
        match cur.kind() {
            K::EQUAL => Some(self.finish_assignment(cur, start, &star, &local_path)),
            K::KW_INSERT => self.finish_insert(cur, start, &star, &local_path, false),
            _ => {
                // pathRule
                Some(self.finish_path_rule(cur, start, &star, &local_path, true))
            }
        }
    }

    fn parse_invariant_rule(&mut self, cur: &mut Cursor) -> Option<Rule> {
        let start = cur.pos;
        let star = cur.tok().clone();
        cur.advance();
        if cur.kind() == K::KW_INSERT {
            return self.finish_insert(cur, start, &star, "", false);
        }
        let (local_path, _had) = self.read_path(cur);
        match cur.kind() {
            K::EQUAL => Some(self.finish_assignment(cur, start, &star, &local_path)),
            K::KW_INSERT => self.finish_insert(cur, start, &star, &local_path, false),
            _ => {
                // pathRule (side effect)
                self.finish_path_rule(cur, start, &star, &local_path, false);
                None
            }
        }
    }

    fn parse_mapping_entity_rule(&mut self, cur: &mut Cursor) -> Option<Rule> {
        let start = cur.pos;
        let star = cur.tok().clone();
        cur.advance();
        if cur.kind() == K::ARROW {
            return Some(self.finish_mapping_rule(cur, start, &star, ""));
        }
        if cur.kind() == K::KW_INSERT {
            return self.finish_insert(cur, start, &star, "", false);
        }
        let (local_path, _had) = self.read_path(cur);
        match cur.kind() {
            K::ARROW => Some(self.finish_mapping_rule(cur, start, &star, &local_path)),
            K::KW_INSERT => self.finish_insert(cur, start, &star, &local_path, false),
            _ => {
                self.finish_path_rule(cur, start, &star, &local_path, false);
                None
            }
        }
    }

    fn parse_vs_rule(&mut self, cur: &mut Cursor) -> Option<Rule> {
        let start = cur.pos;
        let star = cur.tok().clone();
        cur.advance();
        match cur.kind() {
            K::KW_INCLUDE | K::KW_EXCLUDE | K::KW_CODES => {
                Some(self.finish_vs_component(cur, start, &star))
            }
            K::CODE => {
                // could be vsConceptComponent, codeCaretValueRule, codeInsertRule
                // peek after CODE list
                let mut p = cur.pos;
                while cur.toks.get(p).map(|t| t.kind) == Some(K::CODE) {
                    p += 1;
                }
                let after = cur.toks.get(p).map(|t| t.kind).unwrap_or(K::EOF);
                if after == K::CARET_SEQUENCE {
                    Some(self.finish_code_caret(cur, start, &star, true))
                } else if after == K::KW_INSERT {
                    self.finish_code_insert(cur, start, &star, true)
                } else {
                    Some(self.finish_vs_component(cur, start, &star))
                }
            }
            K::CARET_SEQUENCE => {
                // caretValueRule with no path; on VS path forced to ''
                let mut rule = self.finish_caret(cur, start, &star, "");
                if let Rule::CaretValue { path, .. } = &mut rule {
                    *path = String::new();
                }
                Some(rule)
            }
            K::KW_INSERT => self.finish_insert(cur, start, &star, "", true),
            _ => {
                // path-led: caret with path (error on VS) or insert
                let (local_path, _had) = self.read_path(cur);
                match cur.kind() {
                    K::CARET_SEQUENCE => {
                        // caret with path before ^ -> skipped on VS
                        let _rule = self.finish_caret(cur, start, &star, &local_path);
                        None
                    }
                    K::KW_INSERT => self.finish_insert(cur, start, &star, &local_path, true),
                    _ => None,
                }
            }
        }
    }

    fn parse_cs_rule(&mut self, cur: &mut Cursor) -> Option<Rule> {
        let start = cur.pos;
        let star = cur.tok().clone();
        cur.advance();
        match cur.kind() {
            K::CODE => {
                let mut p = cur.pos;
                while cur.toks.get(p).map(|t| t.kind) == Some(K::CODE) {
                    p += 1;
                }
                let after = cur.toks.get(p).map(|t| t.kind).unwrap_or(K::EOF);
                if after == K::CARET_SEQUENCE {
                    Some(self.finish_code_caret(cur, start, &star, false))
                } else if after == K::KW_INSERT {
                    self.finish_code_insert(cur, start, &star, false)
                } else {
                    Some(self.finish_concept(cur, start, &star, false))
                }
            }
            K::CARET_SEQUENCE => Some(self.finish_code_caret(cur, start, &star, false)),
            K::KW_INSERT => self.finish_code_insert(cur, start, &star, false),
            _ => None,
        }
    }

    /// Generic STAR-rule for sdRule / lrRule / ruleSetRule.
    fn parse_rule_generic(&mut self, cur: &mut Cursor, host: RuleHost) -> Vec<Rule> {
        let start = cur.pos;
        let star = cur.tok().clone();
        cur.advance();

        match cur.kind() {
            K::CARET_SEQUENCE => {
                vec![self.finish_caret(cur, start, &star, "")]
            }
            K::KW_OBEYS => self.finish_obeys(cur, start, &star, ""),
            K::KW_INSERT => self
                .finish_insert(cur, start, &star, "", false)
                .into_iter()
                .collect(),
            K::ARROW if host == RuleHost::RuleSet => {
                vec![self.finish_mapping_rule(cur, start, &star, "")]
            }
            K::CODE if host == RuleHost::RuleSet => {
                // concept | codeCaret | codeInsert | vsComponent
                let mut p = cur.pos;
                while cur.toks.get(p).map(|t| t.kind) == Some(K::CODE) {
                    p += 1;
                }
                let after = cur.toks.get(p).map(|t| t.kind).unwrap_or(K::EOF);
                if after == K::CARET_SEQUENCE {
                    vec![self.finish_code_caret(cur, start, &star, false)]
                } else if after == K::KW_INSERT {
                    self.finish_code_insert(cur, start, &star, false)
                        .into_iter()
                        .collect()
                } else {
                    vec![self.finish_concept(cur, start, &star, true)]
                }
            }
            K::KW_INCLUDE | K::KW_EXCLUDE | K::KW_CODES if host == RuleHost::RuleSet => {
                vec![self.finish_vs_component(cur, start, &star)]
            }
            _ => {
                let (local_path, _had) = self.read_path(cur);
                match cur.kind() {
                    K::CARD => self.finish_card_or_add(cur, start, &star, &local_path, host),
                    K::KW_FROM => vec![self.finish_binding(cur, start, &star, &local_path)],
                    K::EQUAL => vec![self.finish_assignment(cur, start, &star, &local_path)],
                    K::KW_CONTAINS => self.finish_contains(cur, start, &star, &local_path),
                    K::KW_ONLY => vec![self.finish_only(cur, start, &star, &local_path)],
                    K::KW_OBEYS => self.finish_obeys(cur, start, &star, &local_path),
                    K::ARROW if host == RuleHost::RuleSet => {
                        vec![self.finish_mapping_rule(cur, start, &star, &local_path)]
                    }
                    K::KW_INSERT => self
                        .finish_insert(cur, start, &star, &local_path, false)
                        .into_iter()
                        .collect(),
                    K::CARET_SEQUENCE => vec![self.finish_caret(cur, start, &star, &local_path)],
                    k if is_flag(k) || k == K::KW_AND => {
                        self.finish_flag(cur, start, &star, &local_path)
                    }
                    _ => {
                        // pathRule (side effect only)
                        self.finish_path_rule(cur, start, &star, &local_path, false);
                        vec![]
                    }
                }
            }
        }
    }

    /// Read an optional path token; returns (text, had_path).
    fn read_path(&mut self, cur: &mut Cursor) -> (String, bool) {
        if is_path_token(cur.kind()) {
            let t = cur.tok().text.clone();
            cur.advance();
            (t, true)
        } else {
            (String::new(), false)
        }
    }

    fn stop_tok(&self, cur: &Cursor, start: usize) -> Token {
        // stop = last consumed token (pos-1), but not before start
        let idx = if cur.pos > start { cur.pos - 1 } else { start };
        cur.at(idx).clone()
    }

    // ---------- rule finishers ----------

    fn finish_card_or_add(
        &mut self,
        cur: &mut Cursor,
        start: usize,
        star: &Token,
        local_path: &str,
        host: RuleHost,
    ) -> Vec<Rule> {
        let card_text = cur.tok().text.clone();
        cur.advance();
        // flags
        let flag_toks = self.consume_flags(cur);
        // is there an addElement tail? (targetType / STRING / contentReference)
        let has_tail = matches!(host, RuleHost::Lr)
            && !matches!(cur.kind(), K::STAR | K::EOF)
            && cur.pos < cur.toks.len();
        if has_tail {
            return vec![self.finish_add_element(cur, start, star, local_path, &card_text, &flag_toks)];
        }
        // cardRule (+ optional flagRule)
        let mut rules = Vec::new();
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path = self.path_with_context(local_path, &location, false, false);
        let (min, max) = parse_card(&card_text);
        rules.push(Rule::Card {
            source_info: SourceInfo::new(&self.current_file, location.clone()),
            path: path.clone(),
            min,
            max,
        });
        if !flag_toks.is_empty() {
            rules.push(Rule::Flag {
                source_info: SourceInfo::new(&self.current_file, location),
                path,
                flags: flags_from(&flag_toks),
            });
        }
        rules
    }

    fn finish_add_element(
        &mut self,
        cur: &mut Cursor,
        start: usize,
        star: &Token,
        local_path: &str,
        card_text: &str,
        flag_toks: &[K],
    ) -> Rule {
        // addElementRule or addCRElementRule. Parse types or contentReference, then strings.
        let mut content_reference = None;
        let mut types = Vec::new();
        if cur.kind() == K::KW_CONTENTREFERENCE {
            cur.advance();
            // (SEQUENCE | CODE)
            if matches!(cur.kind(), K::SEQUENCE | K::CODE) {
                content_reference = Some(self.alias_aware(&cur.tok().text));
                cur.advance();
            }
        } else {
            types = self.parse_target_types(cur);
        }
        // strings: short, definition?
        let mut short = None;
        let mut definition = None;
        let mut strings = Vec::new();
        while matches!(cur.kind(), K::STRING | K::MULTILINE_STRING) {
            let t = cur.tok().clone();
            cur.advance();
            strings.push(t);
        }
        if let Some(s0) = strings.first() {
            short = Some(extract_string_any(s0));
            if strings.len() > 1 {
                definition = Some(extract_string_any(&strings[1]));
            } else {
                definition = short.clone();
            }
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path = self.path_with_context(local_path, &location, false, false);
        let (min, max) = parse_card(card_text);
        Rule::AddElement {
            source_info: SourceInfo::new(&self.current_file, location),
            path,
            min,
            max,
            flags: flags_from(flag_toks),
            types,
            content_reference,
            short,
            definition,
        }
    }

    fn finish_binding(&mut self, cur: &mut Cursor, start: usize, star: &Token, local_path: &str) -> Rule {
        cur.advance(); // KW_FROM
        let name = cur.tok().text.clone();
        cur.advance();
        let mut strength = "required".to_string();
        if matches!(
            cur.kind(),
            K::KW_EXAMPLE | K::KW_PREFERRED | K::KW_EXTENSIBLE | K::KW_REQUIRED
        ) {
            strength = match cur.kind() {
                K::KW_EXAMPLE => "example",
                K::KW_PREFERRED => "preferred",
                K::KW_EXTENSIBLE => "extensible",
                _ => "required",
            }
            .to_string();
            cur.advance();
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path = self.path_with_context(local_path, &location, false, false);
        Rule::Binding {
            source_info: SourceInfo::new(&self.current_file, location),
            path,
            value_set: self.alias_aware(&name),
            strength,
        }
    }

    fn finish_assignment(&mut self, cur: &mut Cursor, start: usize, star: &Token, local_path: &str) -> Rule {
        cur.advance(); // EQUAL
        let vr = self.parse_value(cur);
        let exactly = if cur.kind() == K::KW_EXACTLY {
            cur.advance();
            true
        } else {
            false
        };
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path = self.path_with_context(local_path, &location, false, false);
        let is_instance = vr.is_name && !self.is_alias(vr.name_text.as_deref());
        Rule::Assignment {
            source_info: SourceInfo::new(&self.current_file, location),
            path,
            value: vr.value,
            raw_value: vr.raw_value,
            exactly,
            is_instance,
        }
    }

    fn finish_only(&mut self, cur: &mut Cursor, start: usize, star: &Token, local_path: &str) -> Rule {
        cur.advance(); // KW_ONLY
        let types = self.parse_target_types(cur);
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path = self.path_with_context(local_path, &location, false, false);
        Rule::Only {
            source_info: SourceInfo::new(&self.current_file, location),
            path,
            types,
        }
    }

    fn parse_target_types(&mut self, cur: &mut Cursor) -> Vec<OnlyRuleType> {
        let mut out = Vec::new();
        loop {
            match cur.kind() {
                K::REFERENCE => {
                    let txt = cur.tok().text.clone();
                    cur.advance();
                    for r in parse_or_split(&txt) {
                        out.push(OnlyRuleType {
                            type_: self.alias_aware(&r),
                            is_reference: true,
                            ..Default::default()
                        });
                    }
                }
                K::CODEABLE_REFERENCE => {
                    let txt = cur.tok().text.clone();
                    cur.advance();
                    for r in parse_or_split(&txt) {
                        out.push(OnlyRuleType {
                            type_: self.alias_aware(&r),
                            is_codeable_reference: true,
                            ..Default::default()
                        });
                    }
                }
                K::CANONICAL => {
                    let txt = cur.tok().text.clone();
                    cur.advance();
                    for c in canonical_choices(&txt) {
                        let (item, version) = split_canonical(&c);
                        let type_ = match version {
                            Some(v) => format!("{}|{}", self.alias_aware(&item), v),
                            None => self.alias_aware(&item),
                        };
                        out.push(OnlyRuleType {
                            type_,
                            is_canonical: true,
                            ..Default::default()
                        });
                    }
                }
                k if is_path_token(k) => {
                    let t = cur.tok().text.clone();
                    cur.advance();
                    out.push(OnlyRuleType {
                        type_: self.alias_aware(&t),
                        ..Default::default()
                    });
                }
                _ => break,
            }
            // (KW_OR targetType)*
            if cur.kind() == K::KW_OR {
                cur.advance();
            } else {
                break;
            }
        }
        out
    }

    fn finish_contains(&mut self, cur: &mut Cursor, start: usize, star: &Token, local_path: &str) -> Vec<Rule> {
        cur.advance(); // KW_CONTAINS
        // items: name (KW_NAMED name)? CARD flag*  (KW_AND ...)*
        struct Item {
            name: String,
            type_: Option<String>,
            card: String,
            flags: Vec<K>,
        }
        let mut items = Vec::new();
        loop {
            let n1 = cur.tok().text.clone();
            cur.advance();
            let (name, type_) = if cur.kind() == K::KW_NAMED {
                cur.advance();
                let n2 = cur.tok().text.clone();
                cur.advance();
                (n2, Some(self.alias_aware(&n1)))
            } else {
                (n1, None)
            };
            let card = if cur.kind() == K::CARD {
                let c = cur.tok().text.clone();
                cur.advance();
                c
            } else {
                String::new()
            };
            let flags = self.consume_flags(cur);
            items.push(Item {
                name,
                type_,
                card,
                flags,
            });
            if cur.kind() == K::KW_AND {
                cur.advance();
            } else {
                break;
            }
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path = self.path_with_context(local_path, &location, false, false);
        let mut rules = Vec::new();
        rules.push(Rule::Contains {
            source_info: SourceInfo::new(&self.current_file, location.clone()),
            path: path.clone(),
            items: items
                .iter()
                .map(|i| ContainsRuleItem {
                    name: i.name.clone(),
                    type_: i.type_.clone(),
                })
                .collect(),
        });
        for i in &items {
            let item_path = format!("{}[{}]", path, i.name);
            let (min, max) = parse_card(&i.card);
            rules.push(Rule::Card {
                source_info: SourceInfo::new(&self.current_file, location.clone()),
                path: item_path.clone(),
                min,
                max,
            });
            if !i.flags.is_empty() {
                rules.push(Rule::Flag {
                    source_info: SourceInfo::new(&self.current_file, location.clone()),
                    path: item_path,
                    flags: flags_from(&i.flags),
                });
            }
        }
        rules
    }

    fn finish_obeys(&mut self, cur: &mut Cursor, start: usize, star: &Token, local_path: &str) -> Vec<Rule> {
        cur.advance(); // KW_OBEYS
        let mut names = Vec::new();
        loop {
            names.push(cur.tok().text.clone());
            cur.advance();
            if cur.kind() == K::KW_AND {
                cur.advance();
            } else {
                break;
            }
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path = self.path_with_context(local_path, &location, false, false);
        names
            .into_iter()
            .map(|inv| Rule::Obeys {
                source_info: SourceInfo::new(&self.current_file, location.clone()),
                path: path.clone(),
                invariant: inv,
            })
            .collect()
    }

    fn finish_flag(&mut self, cur: &mut Cursor, start: usize, star: &Token, local_path: &str) -> Vec<Rule> {
        // flagRule: path (KW_AND path)* flag+
        let mut paths = vec![local_path.to_string()];
        while cur.kind() == K::KW_AND {
            cur.advance();
            let (p, _had) = self.read_path(cur);
            paths.push(p);
        }
        let flag_toks = self.consume_flags(cur);
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        paths
            .into_iter()
            .map(|p| {
                let path = self.path_with_context(&p, &location, false, false);
                Rule::Flag {
                    source_info: SourceInfo::new(&self.current_file, location.clone()),
                    path,
                    flags: flags_from(&flag_toks),
                }
            })
            .collect()
    }

    fn finish_caret(&mut self, cur: &mut Cursor, start: usize, star: &Token, local_path: &str) -> Rule {
        // caretPath EQUAL value
        let caret_path = cur.tok().text.clone();
        cur.advance(); // CARET_SEQUENCE
        let caret_path = caret_path.strip_prefix('^').unwrap_or(&caret_path).to_string();
        let mut value = None;
        let mut raw_value = None;
        let mut is_instance = false;
        if cur.kind() == K::EQUAL {
            cur.advance();
            let vr = self.parse_value(cur);
            value = vr.value;
            raw_value = vr.raw_value;
            is_instance = vr.is_name && !self.is_alias(vr.name_text.as_deref());
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let split = split_path(local_path);
        let path_array = self.prepend_path_context(split, &location, false, false, false);
        Rule::CaretValue {
            source_info: SourceInfo::new(&self.current_file, location),
            path: path_array.join("."),
            caret_path: Some(caret_path),
            value,
            raw_value,
            is_instance,
            is_code_caret_rule: false,
            path_array,
        }
    }

    fn finish_code_caret(&mut self, cur: &mut Cursor, start: usize, star: &Token, keep_system: bool) -> Rule {
        // CODE* caretPath EQUAL value
        let mut local_code_path = Vec::new();
        while cur.kind() == K::CODE {
            let (code, sys) = parse_code_lexeme(&cur.tok().text);
            cur.advance();
            if keep_system {
                local_code_path.push(format!("{}#{}", sys.unwrap_or_default(), code));
            } else {
                local_code_path.push(format!("#{}", code));
            }
        }
        let mut caret_path = None;
        if cur.kind() == K::CARET_SEQUENCE {
            let cp = cur.tok().text.clone();
            cur.advance();
            caret_path = Some(cp.strip_prefix('^').unwrap_or(&cp).to_string());
        }
        let mut value = None;
        let mut raw_value = None;
        let mut is_instance = false;
        if cur.kind() == K::EQUAL {
            cur.advance();
            let vr = self.parse_value(cur);
            value = vr.value;
            raw_value = vr.raw_value;
            is_instance = vr.is_name && !self.is_alias(vr.name_text.as_deref());
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path_array = self.prepend_path_context(local_code_path, &location, false, false, false);
        Rule::CaretValue {
            source_info: SourceInfo::new(&self.current_file, location),
            path: String::new(),
            caret_path,
            value,
            raw_value,
            is_instance,
            is_code_caret_rule: false,
            path_array,
        }
    }

    fn finish_concept(&mut self, cur: &mut Cursor, start: usize, star: &Token, in_ruleset: bool) -> Rule {
        // CODE+ STRING? (STRING | MULTILINE_STRING)?
        let mut codes = Vec::new();
        while cur.kind() == K::CODE {
            codes.push(parse_code_lexeme(&cur.tok().text));
            cur.advance();
        }
        let mut strings = Vec::new();
        while matches!(cur.kind(), K::STRING | K::MULTILINE_STRING) {
            strings.push(cur.tok().clone());
            cur.advance();
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let code_paths: Vec<String> = codes.iter().map(|(c, _)| format!("#{}", c)).collect();
        let full_code_path = self.prepend_path_context(code_paths, &location, false, false, false);
        let (code_part_code, code_part_system) = codes.last().cloned().unwrap_or_default();
        let hierarchy: Vec<String> = full_code_path
            .iter()
            .take(full_code_path.len().saturating_sub(1))
            .map(|c| c.strip_prefix('#').unwrap_or(c).to_string())
            .collect();
        let display = strings.first().map(extract_string_any);
        let definition = if strings.len() > 1 {
            Some(extract_string_any(&strings[1]))
        } else {
            None
        };
        // system handling
        let mut system = None;
        let any_system = codes.iter().any(|(_, s)| s.is_some());
        if any_system {
            if in_ruleset
                && code_part_system.is_some()
                && definition.is_none()
                && hierarchy.is_empty()
            {
                system = code_part_system;
            }
        }
        Rule::Concept {
            source_info: SourceInfo::new(&self.current_file, location),
            path: String::new(),
            code: code_part_code,
            display,
            definition,
            system,
            hierarchy,
        }
    }

    fn finish_mapping_rule(&mut self, cur: &mut Cursor, start: usize, star: &Token, local_path: &str) -> Rule {
        cur.advance(); // ARROW
        let map = if cur.kind() == K::STRING {
            let s = extract_string(&cur.tok().text);
            cur.advance();
            s
        } else {
            String::new()
        };
        let mut comment = None;
        if matches!(cur.kind(), K::STRING | K::MULTILINE_STRING) {
            comment = Some(if cur.kind() == K::MULTILINE_STRING {
                extract_multiline_string(&cur.tok().text)
            } else {
                extract_string(&cur.tok().text)
            });
            cur.advance();
        }
        let mut language = None;
        if cur.kind() == K::CODE {
            let t = cur.tok().clone();
            let (code, sys) = parse_code_lexeme(&t.text);
            let system = sys.map(|s| self.alias_aware(&s));
            cur.advance();
            language = Some(FshCode {
                source_info: SourceInfo::new(&self.current_file, loc_tok(&t)),
                code,
                system,
                display: None,
            });
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path = self.path_with_context(local_path, &location, false, false);
        Rule::Mapping {
            source_info: SourceInfo::new(&self.current_file, location),
            path,
            map,
            comment,
            language,
        }
    }

    fn finish_path_rule(
        &mut self,
        cur: &mut Cursor,
        start: usize,
        star: &Token,
        local_path: &str,
        is_instance_rule: bool,
    ) -> Rule {
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path = self.path_with_context(local_path, &location, true, is_instance_rule);
        Rule::Path {
            source_info: SourceInfo::new(&self.current_file, location),
            path,
        }
    }

    fn finish_insert(
        &mut self,
        cur: &mut Cursor,
        start: usize,
        star: &Token,
        local_path: &str,
        with_path_array: bool,
    ) -> Option<Rule> {
        cur.advance(); // KW_INSERT
        let (name, params) = self.read_insert_ref(cur);
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let local = if local_path.is_empty() {
            vec![]
        } else {
            vec![local_path.to_string()]
        };
        let full_path_array = self.prepend_path_context(local, &location, false, false, false);
        let path = full_path_array.join(".");
        let path_array = if with_path_array {
            full_path_array
        } else {
            vec![]
        };
        let rule_set = name;
        // A parameterized insert whose RuleSet can't be found/expanded (or whose
        // param count mismatches) is dropped entirely by SUSHI (returns undefined).
        let (params_vec, keep) = match params {
            Some(p) => {
                let keep = self.apply_ruleset_params(&rule_set, &p, &location);
                (p, keep)
            }
            None => (vec![], true),
        };
        if !keep {
            return None;
        }
        Some(Rule::Insert {
            source_info: SourceInfo::new(&self.current_file, location),
            path,
            path_array,
            params: params_vec,
            rule_set,
        })
    }

    fn finish_code_insert(
        &mut self,
        cur: &mut Cursor,
        start: usize,
        star: &Token,
        keep_system: bool,
    ) -> Option<Rule> {
        // CODE* KW_INSERT ref
        let mut local_code_path = Vec::new();
        while cur.kind() == K::CODE {
            let (code, sys) = parse_code_lexeme(&cur.tok().text);
            cur.advance();
            if keep_system {
                local_code_path.push(format!("{}#{}", sys.unwrap_or_default(), code));
            } else {
                local_code_path.push(format!("#{}", code));
            }
        }
        cur.advance(); // KW_INSERT
        let (name, params) = self.read_insert_ref(cur);
        let stop = self.stop_tok(cur, start);
        let location = loc(star, &stop);
        let path_array = self.prepend_path_context(local_code_path, &location, false, false, false);
        let (params_vec, keep) = match params {
            Some(p) => {
                let keep = self.apply_ruleset_params(&name, &p, &location);
                (p, keep)
            }
            None => (vec![], true),
        };
        if !keep {
            return None;
        }
        Some(Rule::Insert {
            source_info: SourceInfo::new(&self.current_file, location),
            path: String::new(),
            path_array,
            params: params_vec,
            rule_set: name,
        })
    }

    /// Reads (RULESET_REFERENCE | paramRuleSetRef) after KW_INSERT.
    /// Returns (name, Some(params) if parameterized else None).
    fn read_insert_ref(&mut self, cur: &mut Cursor) -> (String, Option<Vec<String>>) {
        if cur.kind() == K::RULESET_REFERENCE {
            let name = trim(cur.tok().text.clone());
            cur.advance();
            (name, None)
        } else if cur.kind() == K::PARAM_RULESET_REFERENCE {
            let name = trim(slice_off_last(&cur.tok().text));
            cur.advance();
            let mut params = Vec::new();
            loop {
                match cur.kind() {
                    K::BRACKETED_PARAM | K::PLAIN_PARAM => {
                        params.push(parse_insert_param(&cur.tok().text, cur.kind()));
                        cur.advance();
                    }
                    K::LAST_BRACKETED_PARAM | K::LAST_PLAIN_PARAM => {
                        params.push(parse_insert_param(&cur.tok().text, cur.kind()));
                        cur.advance();
                        break;
                    }
                    _ => break,
                }
            }
            (name, Some(params))
        } else {
            (String::new(), None)
        }
    }

    fn consume_flags(&mut self, cur: &mut Cursor) -> Vec<K> {
        let mut out = Vec::new();
        while is_flag(cur.kind()) {
            out.push(cur.kind());
            cur.advance();
        }
        out
    }

    // ---------- value parsing ----------

    fn parse_value(&mut self, cur: &mut Cursor) -> ValueResult {
        let mut vr = ValueResult::default();
        match cur.kind() {
            K::STRING => {
                vr.value = Some(Value::Str(extract_string(&cur.tok().text)));
                cur.advance();
            }
            K::MULTILINE_STRING => {
                vr.value = Some(Value::Str(extract_multiline_string(&cur.tok().text)));
                cur.advance();
            }
            K::DATETIME | K::TIME => {
                vr.value = Some(Value::Str(cur.tok().text.clone()));
                cur.advance();
            }
            K::REFERENCE => {
                vr.value = Some(self.parse_reference(cur));
            }
            K::CANONICAL => {
                vr.value = Some(self.parse_canonical_value(cur));
            }
            K::CODE => {
                vr.value = Some(self.parse_code(cur));
            }
            K::UNIT => {
                vr.value = Some(self.parse_quantity(cur));
            }
            K::NUMBER => {
                vr.value = Some(self.parse_number_value(cur, &mut vr.raw_value));
            }
            K::KW_TRUE | K::KW_FALSE => {
                let is_true = cur.kind() == K::KW_TRUE;
                vr.raw_value = Some(cur.tok().text.clone());
                vr.value = Some(Value::Bool(is_true));
                cur.advance();
            }
            k if is_path_token(k) => {
                let name = cur.tok().text.clone();
                cur.advance();
                vr.is_name = true;
                vr.name_text = Some(name.clone());
                vr.value = Some(Value::Str(self.alias_aware(&name)));
            }
            _ => {}
        }
        vr
    }

    fn parse_code(&mut self, cur: &mut Cursor) -> Value {
        let start = cur.pos;
        let t = cur.tok().clone();
        let (code, sys) = parse_code_lexeme(&t.text);
        let system = sys.map(|s| self.alias_aware(&s));
        cur.advance();
        let mut display = None;
        if cur.kind() == K::STRING {
            display = Some(extract_string(&cur.tok().text));
            cur.advance();
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(&t, &stop);
        Value::Code(FshCode {
            source_info: SourceInfo::new(&self.current_file, location),
            code,
            system,
            display,
        })
    }

    fn parse_reference(&mut self, cur: &mut Cursor) -> Value {
        let start = cur.pos;
        let t = cur.tok().clone();
        let refs = parse_or_split(&t.text);
        let reference = self.alias_aware(refs.first().map(|s| s.as_str()).unwrap_or(""));
        cur.advance();
        let mut display = None;
        if cur.kind() == K::STRING {
            display = Some(extract_string(&cur.tok().text));
            cur.advance();
        }
        let stop = self.stop_tok(cur, start);
        let location = loc(&t, &stop);
        Value::Reference(FshReference {
            source_info: SourceInfo::new(&self.current_file, location),
            reference,
            display,
        })
    }

    fn parse_canonical_value(&mut self, cur: &mut Cursor) -> Value {
        let t = cur.tok().clone();
        cur.advance();
        let choices = canonical_choices(&t.text);
        let c = choices.into_iter().next().unwrap_or_default();
        let (item, version) = split_canonical(&c);
        Value::Canonical(FshCanonical {
            source_info: SourceInfo::new(&self.current_file, loc_tok(&t)),
            entity_name: item,
            version,
        })
    }

    fn parse_quantity(&mut self, cur: &mut Cursor) -> Value {
        let start = cur.pos;
        let start_tok = cur.tok().clone();
        let mut value = None;
        if cur.kind() == K::NUMBER {
            value = cur.tok().text.parse::<f64>().ok();
            cur.advance();
        }
        let mut unit_code;
        let mut unit_system = Some("http://unitsofmeasure.org".to_string());
        let unit_tok;
        if cur.kind() == K::UNIT {
            let t = cur.tok().clone();
            let raw = t.text.clone();
            // strip surrounding quotes ('...')
            unit_code = raw[1..raw.len().saturating_sub(1)].to_string();
            unit_tok = Some(t);
            cur.advance();
        } else if cur.kind() == K::CODE {
            let t = cur.tok().clone();
            let (code, sys) = parse_code_lexeme(&t.text);
            unit_code = code;
            unit_system = sys.map(|s| self.alias_aware(&s));
            unit_tok = None;
            cur.advance();
        } else {
            unit_code = String::new();
            unit_tok = None;
        }
        let mut display = None;
        if cur.kind() == K::STRING {
            display = Some(extract_string(&cur.tok().text));
            cur.advance();
        }
        let stop = self.stop_tok(cur, start);
        let q_location = loc(&start_tok, &stop);
        let unit_location = match &unit_tok {
            Some(u) => loc_tok(u),
            None => q_location.clone(),
        };
        let unit = FshCode {
            source_info: SourceInfo::new(&self.current_file, unit_location),
            code: std::mem::take(&mut unit_code),
            system: unit_system,
            display,
        };
        Value::Quantity(FshQuantity {
            source_info: SourceInfo::new(&self.current_file, q_location),
            value,
            unit: Some(unit),
        })
    }

    fn parse_number_value(&mut self, cur: &mut Cursor, raw: &mut Option<String>) -> Value {
        // could be number, quantity, or ratio
        let num_text = cur.tok().text.clone();
        let nxt = cur.la(1);
        if matches!(nxt, K::UNIT | K::CODE) {
            // quantity (maybe ratio)
            let q = self.parse_quantity(cur);
            return self.maybe_ratio(cur, q);
        }
        if nxt == K::COLON {
            // ratio: NUMBER : ratioPart
            let start = cur.pos;
            let start_tok = cur.tok().clone();
            cur.advance(); // NUMBER
            let num = FshQuantity {
                source_info: SourceInfo::new(&self.current_file, loc_tok(&start_tok)),
                value: num_text.parse::<f64>().ok(),
                unit: None,
            };
            return self.finish_ratio(cur, start, &start_tok, num);
        }
        // plain number
        *raw = Some(num_text.clone());
        cur.advance();
        extract_number_value(&num_text)
    }

    fn maybe_ratio(&mut self, cur: &mut Cursor, first: Value) -> Value {
        if cur.kind() == K::COLON {
            // need to wrap first as ratioPart quantity
            if let Value::Quantity(q) = first {
                cur.advance(); // COLON
                let denom = self.parse_ratio_part(cur);
                return Value::Ratio(Box::new(FshRatio {
                    source_info: q.source_info.clone(),
                    numerator: q,
                    denominator: denom,
                }));
            }
        }
        first
    }

    fn finish_ratio(&mut self, cur: &mut Cursor, _start: usize, start_tok: &Token, num: FshQuantity) -> Value {
        cur.advance(); // COLON
        let denom = self.parse_ratio_part(cur);
        Value::Ratio(Box::new(FshRatio {
            source_info: SourceInfo::new(&self.current_file, loc_tok(start_tok)),
            numerator: num,
            denominator: denom,
        }))
    }

    fn parse_ratio_part(&mut self, cur: &mut Cursor) -> FshQuantity {
        if cur.kind() == K::NUMBER && !matches!(cur.la(1), K::UNIT | K::CODE) {
            let t = cur.tok().clone();
            let v = t.text.parse::<f64>().ok();
            cur.advance();
            FshQuantity {
                source_info: SourceInfo::new(&self.current_file, loc_tok(&t)),
                value: v,
                unit: None,
            }
        } else if let Value::Quantity(q) = self.parse_quantity(cur) {
            q
        } else {
            FshQuantity {
                source_info: SourceInfo::default(),
                value: None,
                unit: None,
            }
        }
    }

    fn is_alias(&self, name: Option<&str>) -> bool {
        match name {
            Some(n) => self.all_aliases.contains_key(n),
            None => false,
        }
    }

    // ---------- vsComponent ----------

    fn finish_vs_component(&mut self, cur: &mut Cursor, start: usize, star: &Token) -> Rule {
        let mut inclusion = true;
        if cur.kind() == K::KW_INCLUDE {
            cur.advance();
        } else if cur.kind() == K::KW_EXCLUDE {
            inclusion = false;
            cur.advance();
        }
        if cur.kind() == K::KW_CODES {
            // filter component
            cur.advance();
            let from = self.parse_vs_component_from(cur);
            let mut filters = Vec::new();
            if cur.kind() == K::KW_WHERE {
                cur.advance();
                if from.system.is_some() {
                    loop {
                        if let Some(f) = self.parse_vs_filter_def(cur) {
                            filters.push(f);
                        }
                        if cur.kind() == K::KW_AND {
                            cur.advance();
                        } else {
                            break;
                        }
                    }
                }
            }
            let stop = self.stop_tok(cur, start);
            let location = loc(star, &stop);
            // reset context
            self.prepend_path_context(vec![], &location, false, false, true);
            Rule::VsFilter {
                source_info: SourceInfo::new(&self.current_file, location),
                path: String::new(),
                inclusion,
                from,
                filters,
            }
        } else {
            // concept component: code vsComponentFrom?
            let code = match self.parse_code(cur) {
                Value::Code(c) => c,
                _ => FshCode {
                    source_info: SourceInfo::default(),
                    code: String::new(),
                    system: None,
                    display: None,
                },
            };
            let mut from = ValueSetComponentFrom {
                system: None,
                value_sets: None,
            };
            if cur.kind() == K::KW_FROM {
                from = self.parse_vs_component_from(cur);
            }
            let mut concepts = Vec::new();
            let mut single = code;
            if single.system.is_some() && from.system.is_some() {
                // error (multiple systems) -> push nothing per importer? importer logs error, no push
                // but importer pushes nothing in that branch -> concepts stays empty
            } else if single.system.is_some() {
                from.system = single.system.clone();
                concepts.push(single.clone());
            } else if from.system.is_some() {
                single.system = from.system.clone();
                concepts.push(single.clone());
            }
            let stop = self.stop_tok(cur, start);
            let location = loc(star, &stop);
            // context: single concept -> set, else reset
            if concepts.len() == 1 {
                let c = &concepts[0];
                let ctx_path = format!("{}#{}", c.system.clone().unwrap_or_default(), c.code);
                self.prepend_path_context(vec![ctx_path], &location, false, false, true);
            } else {
                self.prepend_path_context(vec![], &location, false, false, true);
            }
            Rule::VsConcept {
                source_info: SourceInfo::new(&self.current_file, location),
                path: String::new(),
                inclusion,
                from,
                concepts,
            }
        }
    }

    fn parse_vs_component_from(&mut self, cur: &mut Cursor) -> ValueSetComponentFrom {
        let mut from = ValueSetComponentFrom {
            system: None,
            value_sets: None,
        };
        if cur.kind() != K::KW_FROM {
            return from;
        }
        cur.advance(); // KW_FROM
        // (vsFromSystem (KW_AND vsFromValueset)? | vsFromValueset (KW_AND vsFromSystem)?)
        loop {
            match cur.kind() {
                K::KW_SYSTEM => {
                    cur.advance();
                    from.system = Some(self.alias_aware(&cur.tok().text));
                    cur.advance();
                }
                K::KW_VSREFERENCE => {
                    cur.advance();
                    let mut vss = Vec::new();
                    loop {
                        vss.push(self.alias_aware(&cur.tok().text));
                        cur.advance();
                        if cur.kind() == K::KW_AND && cur.la(1) != K::KW_SYSTEM {
                            cur.advance();
                        } else {
                            break;
                        }
                    }
                    if !vss.is_empty() {
                        from.value_sets = Some(vss);
                    }
                }
                _ => break,
            }
            if cur.kind() == K::KW_AND && matches!(cur.la(1), K::KW_SYSTEM | K::KW_VSREFERENCE) {
                cur.advance();
            } else {
                break;
            }
        }
        from
    }

    fn parse_vs_filter_def(&mut self, cur: &mut Cursor) -> Option<ValueSetFilter> {
        let property = cur.tok().text.clone();
        cur.advance();
        // operator: EQUAL | SEQUENCE
        let operator_raw = cur.tok().text.clone();
        cur.advance();
        let operator = operator_raw.to_lowercase().replace("descendant", "descendent");
        // value?
        let value = match cur.kind() {
            K::CODE => {
                if let Value::Code(c) = self.parse_code(cur) {
                    FilterValue::Code(c)
                } else {
                    return None;
                }
            }
            K::REGEX => {
                let t = cur.tok().text.clone();
                cur.advance();
                FilterValue::Regex(t[1..t.len().saturating_sub(1)].to_string())
            }
            K::STRING => {
                let s = extract_string(&cur.tok().text);
                cur.advance();
                FilterValue::Str(s)
            }
            K::KW_TRUE => {
                cur.advance();
                FilterValue::Bool(true)
            }
            K::KW_FALSE => {
                cur.advance();
                FilterValue::Bool(false)
            }
            _ => FilterValue::Bool(true), // exists -> true
        };
        Some(ValueSetFilter {
            property,
            operator,
            value,
        })
    }

    // ---------- path context (soft index) ----------

    fn path_with_context(
        &mut self,
        path: &str,
        location: &Location,
        is_path_rule: bool,
        is_instance_rule: bool,
    ) -> String {
        let split = split_path(path);
        self.prepend_path_context(split, location, is_path_rule, is_instance_rule, false)
            .join(".")
    }

    fn prepend_path_context(
        &mut self,
        path: Vec<String>,
        location: &Location,
        is_path_rule: bool,
        is_instance_rule: bool,
        suppress_error: bool,
    ) -> Vec<String> {
        let result = self.prepend_path_context_inner(path, location, suppress_error);
        // finally: mutate [+] -> [=]
        if !is_path_rule || is_instance_rule {
            for ctx in self.path_context.iter_mut() {
                for seg in ctx.iter_mut() {
                    *seg = seg.replace("[+]", "[=]");
                }
            }
        }
        result
    }

    fn prepend_path_context_inner(
        &mut self,
        path: Vec<String>,
        location: &Location,
        suppress_error: bool,
    ) -> Vec<String> {
        let current_indent = location.start_column as i64 - 1;
        let context_index = current_indent / 2;

        if !self.is_valid_context(current_indent) {
            return path;
        }

        if context_index == 0 {
            self.path_context = vec![path.clone()];
            return path;
        }

        if path.len() == 1 && path[0] == "." {
            return path;
        }

        let idx = (context_index - 1) as usize;
        let current_context = match self.path_context.get(idx) {
            Some(c) => c.clone(),
            None => return path,
        };
        if current_context.is_empty() && !suppress_error {
            return path;
        }

        // trim out-of-scope
        if (context_index as usize) < self.path_context.len() {
            self.path_context.truncate(context_index as usize);
        }
        let mut full = current_context;
        full.extend(path);
        self.path_context.push(full.clone());
        full
    }

    fn is_valid_context(&self, current_indent: i64) -> bool {
        if current_indent > 0 && self.path_context.is_empty() {
            return false;
        }
        if current_indent % 2 != 0 || current_indent < 0 {
            return false;
        }
        let context_index = current_indent / 2;
        if context_index > self.path_context.len() as i64 {
            return false;
        }
        true
    }

    // ---------- parameterized ruleset expansion ----------

    /// Returns true if the insert rule should be kept; false if SUSHI would drop
    /// it (unknown RuleSet, param-count mismatch, or failed expansion).
    fn apply_ruleset_params(&mut self, name: &str, params: &[String], _location: &Location) -> bool {
        let prs = match self.param_rule_sets.get(name) {
            Some(p) => p.clone(),
            None => return false,
        };
        if prs.parameters.len() != params.len() {
            return false;
        }
        let mut id_parts = vec![name.to_string()];
        id_parts.extend(params.iter().cloned());
        let identifier = serde_json_array(&id_parts);
        if self.docs[self.current_doc]
            .applied_rule_sets
            .iter()
            .any(|(k, _)| k == &identifier)
        {
            return true;
        }
        let applied_fsh = apply_ruleset_substitutions(&prs, params);
        if let Some(mut applied) = self.parse_generated_ruleset(&applied_fsh, &prs.name) {
            // rebase source info onto original ruleset
            applied.source_info.file = prs.source_info.file.clone();
            applied.source_info.location = prs.source_info.location.clone();
            let base_start = prs
                .source_info
                .location
                .as_ref()
                .map(|l| l.start_line)
                .unwrap_or(1);
            for rule in applied.rules.iter_mut() {
                rebase_rule(rule, prs.source_info.file.clone(), base_start);
            }
            self.docs[self.current_doc]
                .applied_rule_sets
                .push((identifier, applied));
            true
        } else {
            false
        }
    }

    fn parse_generated_ruleset(&mut self, input: &str, name: &str) -> Option<RuleSet> {
        let appended = if input.ends_with('\n') {
            input.to_string()
        } else {
            format!("{}\n", input)
        };
        let toks: Vec<Token> = lex_document(input)
            .into_iter()
            .filter(|t| t.channel == Channel::Default && t.kind != K::EOF)
            .collect();
        let ranges = entity_ranges(&toks);

        // save state
        let saved_doc = self.current_doc;
        let saved_ctx = std::mem::take(&mut self.path_context);
        let prev_top = self.top_level_parse;
        if self.top_level_parse {
            self.top_level_parse = false;
        }

        // temp doc
        let temp = FshDocument::new(&self.current_file);
        self.docs.push(temp);
        let temp_idx = self.docs.len() - 1;
        self.current_doc = temp_idx;

        for &(s, e) in &ranges {
            let kw = toks[s].kind;
            if kw == K::KW_RULESET && toks.get(s + 1).map(|t| t.kind) == Some(K::PARAM_RULESET_REFERENCE)
            {
                self.collect_param_ruleset(&appended, &toks, s, e);
                continue;
            }
            if kw == K::KW_ALIAS {
                continue;
            }
            let slice = toks[s..e].to_vec();
            let mut cur = Cursor::new(slice);
            self.path_context = Vec::new();
            self.visit_entity(&mut cur);
        }

        // retrieve result
        let temp_doc = self.docs.remove(temp_idx);
        let result = temp_doc
            .rule_sets
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v);

        // restore
        self.current_doc = saved_doc;
        self.path_context = saved_ctx;
        self.top_level_parse = prev_top;

        result
    }
}

fn rebase_rule(rule: &mut Rule, file: Option<String>, base_start: u32) {
    let si = rule_source_info_mut(rule);
    si.file = file;
    if let Some(loc) = si.location.as_mut() {
        loc.start_line += base_start - 1;
        loc.end_line += base_start - 1;
    }
}

fn rule_source_info_mut(rule: &mut Rule) -> &mut SourceInfo {
    match rule {
        Rule::Card { source_info, .. }
        | Rule::Flag { source_info, .. }
        | Rule::Binding { source_info, .. }
        | Rule::Assignment { source_info, .. }
        | Rule::Only { source_info, .. }
        | Rule::Contains { source_info, .. }
        | Rule::CaretValue { source_info, .. }
        | Rule::Obeys { source_info, .. }
        | Rule::Insert { source_info, .. }
        | Rule::Path { source_info, .. }
        | Rule::Concept { source_info, .. }
        | Rule::Mapping { source_info, .. }
        | Rule::AddElement { source_info, .. }
        | Rule::VsConcept { source_info, .. }
        | Rule::VsFilter { source_info, .. } => source_info,
    }
}

#[derive(PartialEq, Clone, Copy)]
enum RuleHost {
    Sd,
    Lr,
    RuleSet,
}

#[derive(Default)]
struct ValueResult {
    value: Option<Value>,
    is_name: bool,
    name_text: Option<String>,
    raw_value: Option<String>,
}

// ------------------------------------------------------------------ free fns

fn entity_ranges(toks: &[Token]) -> Vec<(usize, usize)> {
    let mut starts = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if is_entity_keyword(t.kind) {
            starts.push(i);
        }
    }
    let mut ranges = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let e = if k + 1 < starts.len() {
            starts[k + 1]
        } else {
            toks.len()
        };
        ranges.push((s, e));
    }
    ranges
}

fn trim(s: impl AsRef<str>) -> String {
    s.as_ref().trim().to_string()
}

fn slice_off_last(s: &str) -> &str {
    // slice(0, -1) on a JS string == remove last UTF-16 unit; for our tokens
    // (comma / paren / colon last char) it's always a 1-byte ASCII char.
    let mut end = s.len();
    if end > 0 {
        // find last char boundary
        end -= 1;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
    }
    &s[..end]
}

fn upper_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn parse_card(card: &str) -> (Option<i64>, String) {
    let parts: Vec<&str> = card.splitn(2, "..").collect();
    let min = parts.first().and_then(|p| p.parse::<i64>().ok());
    let max = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
    (min, max)
}

fn flags_from(toks: &[K]) -> Flags {
    let mut f = Flags::default();
    for k in toks {
        match k {
            K::KW_MS => f.must_support = true,
            K::KW_SU => f.summary = true,
            K::KW_MOD => f.modifier = true,
            K::KW_TU => f.trial_use = true,
            K::KW_NORMATIVE => f.normative = true,
            K::KW_DRAFT => f.draft = true,
            _ => {}
        }
    }
    f
}

fn extract_string_any(t: &Token) -> String {
    if t.kind == K::MULTILINE_STRING {
        extract_multiline_string(&t.text)
    } else {
        extract_string(&t.text)
    }
}

fn canonical_choices(text: &str) -> Vec<String> {
    // text = Canonical( ... ); split inner on \s+or\s+
    let open = text.find('(').map(|i| i + 1).unwrap_or(0);
    let close = text.rfind(')').unwrap_or(text.len());
    let inner = if close >= open { &text[open..close] } else { "" };
    split_ws_or(inner)
}

fn split_canonical(c: &str) -> (String, Option<String>) {
    // split on \s*\|\s*(.+)
    if let Some(idx) = c.find('|') {
        let item = c[..idx].trim().to_string();
        let version = c[idx + 1..].trim().to_string();
        if version.is_empty() {
            (item, None)
        } else {
            (item, Some(version))
        }
    } else {
        (c.trim().to_string(), None)
    }
}

fn parse_insert_param(text: &str, kind: K) -> String {
    match kind {
        K::BRACKETED_PARAM | K::LAST_BRACKETED_PARAM => {
            // slice(0,-1).trim().slice(2,-2).replace(/(\]\]\s*)\\([,\)])|(\\\\)/g, '$1$2$3')
            let s = slice_off_last(text).trim().to_string();
            let inner = if s.len() >= 4 { &s[2..s.len() - 2] } else { "" };
            unescape_bracketed(inner)
        }
        _ => {
            // PLAIN: slice(0,-1).trim().replace(/\\([,\)])/g, '$1')
            let s = slice_off_last(text).trim().to_string();
            unescape_plain(&s)
        }
    }
}

fn unescape_plain(s: &str) -> String {
    // replace \, -> ,  and \) -> )
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && (chars[i + 1] == ',' || chars[i + 1] == ')') {
            out.push(chars[i + 1]);
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn unescape_bracketed(s: &str) -> String {
    // /(\]\]\s*)\\([,\)])|(\\\\)/g => '$1$2$3'
    // i.e. "]]  \," -> "]]  ," ; "\\" -> "\"
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        // try match ]] \s* \ [,)]
        if chars[i] == ']' && i + 1 < chars.len() && chars[i + 1] == ']' {
            let mut j = i + 2;
            let mut ws = String::new();
            while j < chars.len() && chars[j].is_whitespace() {
                ws.push(chars[j]);
                j += 1;
            }
            if j + 1 < chars.len() && chars[j] == '\\' && (chars[j + 1] == ',' || chars[j + 1] == ')')
            {
                out.push(']');
                out.push(']');
                out.push_str(&ws);
                out.push(chars[j + 1]);
                i = j + 2;
                continue;
            }
        }
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1] == '\\' {
            out.push('\\');
            i += 2;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn serde_json_array(parts: &[String]) -> String {
    serde_json::to_string(parts).unwrap_or_default()
}

fn sorted_opt(v: &Option<Vec<String>>) -> Option<Vec<String>> {
    v.as_ref().map(|x| {
        let mut c = x.clone();
        c.sort();
        c
    })
}

// ------------------------------------------------------------------ MiniFSH

fn apply_ruleset_substitutions(prs: &ParamRuleSet, values: &[String]) -> String {
    let rules = mini_split_rules(&prs.contents);
    let mut out_lines = Vec::new();
    for (indent, parts) in rules {
        let rule_text = parts.join(" ");
        let is_insert = parts.first().map(|s| s == "insert").unwrap_or(false) && parts.len() > 1;
        let path_insert = parts.get(1).map(|s| s == "insert").unwrap_or(false);
        let indent_str = " ".repeat(indent);
        if is_insert || path_insert {
            out_lines.push(format!(
                "{}{}",
                indent_str,
                bracket_aware_substitution(&rule_text, &prs.parameters, values)
            ));
        } else {
            out_lines.push(format!(
                "{}{}",
                indent_str,
                regular_substitution(&rule_text, &prs.parameters, values)
            ));
        }
    }
    format!("RuleSet: {}\n{}", prs.name, out_lines.join("\n"))
}

/// Split MiniFSH contents into (indent, parts). A rule starts at a STAR
/// (newline + ws* + '*' + space). Parts are SEQUENCE / STRING / MULTILINE_STRING.
fn mini_split_rules(contents: &str) -> Vec<(usize, Vec<String>)> {
    let chars: Vec<char> = contents.chars().collect();
    let mut rules: Vec<(usize, Vec<String>)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        // find STAR: [\r\n] WS* '*' [  ]
        if chars[i] == '\n' || chars[i] == '\r' {
            let mut j = i + 1;
            while j < chars.len() && matches!(chars[j], ' ' | '\t' | '\r' | '\n' | '\u{0c}' | '\u{a0}') && chars[j] != '*' {
                // whitespace before star; but newline included? WS includes \r\n
                if chars[j] == '*' {
                    break;
                }
                j += 1;
            }
            // Actually find the '*' preceded only by WS on this segment
            // recompute: from i+1, skip WS, expect '*'
            let mut k = i + 1;
            let mut indent_spaces = 0usize;
            while k < chars.len() && matches!(chars[k], ' ' | '\t' | '\u{0c}' | '\u{a0}') {
                indent_spaces += 1;
                k += 1;
            }
            if k < chars.len() && chars[k] == '*' && k + 1 < chars.len() && (chars[k + 1] == ' ' || chars[k + 1] == '\u{a0}') {
                // start of a rule
                let body_start = k + 2;
                let (parts, next) = mini_tokenize(&chars, body_start);
                rules.push((indent_spaces, parts));
                i = next;
                continue;
            }
            let _ = j;
        }
        i += 1;
    }
    rules
}

/// Tokenize rule parts until next STAR boundary or end. Returns (parts, next_index).
fn mini_tokenize(chars: &[char], start: usize) -> (Vec<String>, usize) {
    let mut parts = Vec::new();
    let mut i = start;
    loop {
        // skip whitespace
        while i < chars.len() && matches!(chars[i], ' ' | '\t' | '\u{0c}' | '\u{a0}') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        // STAR boundary detection: newline then ws* '*' ' '
        if chars[i] == '\n' || chars[i] == '\r' {
            // peek ahead for a star rule
            let mut k = i + 1;
            while k < chars.len() && matches!(chars[k], ' ' | '\t' | '\u{0c}' | '\u{a0}') {
                k += 1;
            }
            if k < chars.len() && chars[k] == '*' && k + 1 < chars.len() && (chars[k + 1] == ' ' || chars[k + 1] == '\u{a0}') {
                // next rule
                return (parts, i);
            }
            // otherwise just whitespace, skip
            i += 1;
            continue;
        }
        // a part
        if chars[i] == '"' {
            // multiline?
            if i + 2 < chars.len() && chars[i + 1] == '"' && chars[i + 2] == '"' {
                let mut j = i + 3;
                while j + 2 < chars.len() && !(chars[j] == '"' && chars[j + 1] == '"' && chars[j + 2] == '"') {
                    j += 1;
                }
                let end = (j + 3).min(chars.len());
                parts.push(chars[i..end].iter().collect());
                i = end;
            } else {
                // string with escapes
                let mut j = i + 1;
                while j < chars.len() {
                    if chars[j] == '\\' && j + 1 < chars.len() {
                        j += 2;
                        continue;
                    }
                    if chars[j] == '"' {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                parts.push(chars[i..j].iter().collect());
                i = j;
            }
        } else {
            // SEQUENCE: NONWS+
            let mut j = i;
            while j < chars.len() && !matches!(chars[j], ' ' | '\t' | '\r' | '\n' | '\u{0c}' | '\u{a0}') {
                j += 1;
            }
            parts.push(chars[i..j].iter().collect());
            i = j;
        }
    }
    (parts, i)
}

fn regular_substitution(rule_text: &str, params: &[String], values: &[String]) -> String {
    format!("* {}", substitute_plain(rule_text, params, values))
}

fn bracket_aware_substitution(rule_text: &str, params: &[String], values: &[String]) -> String {
    // Simplified bracket-aware substitution. Handles [[{param}]] and {param}.
    // bracket zones: (?:,|\()\s*\[\[.+?\]\]\s*(?=,|\))
    let bracket_zones = find_bracket_zones(rule_text);
    let chars: Vec<char> = rule_text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        // try [[{param}]]
        if i + 1 < chars.len() && chars[i] == '[' && chars[i + 1] == '[' {
            if let Some((pname, consumed)) = match_brace_param(&chars, i + 2, params) {
                // require closing ]]
                let after = i + 2 + consumed;
                if after + 1 < chars.len() && chars[after] == ']' && chars[after + 1] == ']' {
                    let idx = params.iter().position(|p| p == &pname).unwrap();
                    let v = values[idx]
                        .replace("]],", "]]\\,")
                        .replace("]])", "]]\\)");
                    out.push_str(&format!("[[{}]]", v));
                    i = after + 2;
                    continue;
                }
            }
        }
        // try {param}
        if chars[i] == '{' {
            if let Some((pname, consumed)) = match_brace_param(&chars, i + 1, params) {
                let end = i + 1 + consumed;
                if end < chars.len() && chars[end] == '}' {
                    let idx = params.iter().position(|p| p == &pname).unwrap();
                    let offset = i;
                    let in_zone = bracket_zones.iter().any(|(s, e)| *s < offset && offset < *e);
                    let v = if in_zone {
                        values[idx].replace("]],", "]]\\,").replace("]])", "]]\\)")
                    } else {
                        values[idx].clone()
                    };
                    out.push_str(&v);
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    format!("* {}", out)
}

fn find_bracket_zones(text: &str) -> Vec<(usize, usize)> {
    // (?:,|\()\s*\[\[.+?\]\]\s*(?=,|\))  -- approximate by scanning
    let chars: Vec<char> = text.chars().collect();
    let mut zones = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ',' || chars[i] == '(' {
            let start = i;
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j + 1 < chars.len() && chars[j] == '[' && chars[j + 1] == '[' {
                // find ]]
                let mut k = j + 2;
                while k + 1 < chars.len() && !(chars[k] == ']' && chars[k + 1] == ']') {
                    k += 1;
                }
                if k + 1 < chars.len() {
                    let mut m = k + 2;
                    while m < chars.len() && chars[m].is_whitespace() {
                        m += 1;
                    }
                    if m < chars.len() && (chars[m] == ',' || chars[m] == ')') {
                        zones.push((start, m));
                    }
                }
            }
        }
        i += 1;
    }
    zones
}

fn match_brace_param(chars: &[char], start: usize, params: &[String]) -> Option<(String, usize)> {
    // matches \s* (param) \s*  ; returns (param, consumed_chars_until_before_})
    let mut i = start;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    let name_start = i;
    while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '}' {
        i += 1;
    }
    let name: String = chars[name_start..i].iter().collect();
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if params.iter().any(|p| p == &name) {
        Some((name, i - start))
    } else {
        None
    }
}

fn substitute_plain(text: &str, params: &[String], values: &[String]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            if let Some((pname, consumed)) = match_brace_param(&chars, i + 1, params) {
                let end = i + 1 + consumed;
                if end < chars.len() && chars[end] == '}' {
                    let idx = params.iter().position(|p| p == &pname).unwrap();
                    out.push_str(&values[idx]);
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}
