//! FSH lexer (hand-written port of `FSHLexer.g4`).
//!
//! Goal: BYTE-EXACT parity with the ANTLR-generated lexer (see
//! `harness/lex-oracle.cjs`). The strategy mirrors ANTLR's maximal-munch with
//! first-rule-wins tie-breaking and a `pushMode`/`popMode` mode stack.
//!
//! Geometry follows ANTLR exactly:
//! - `line`: 1-based line of the token's first char (incremented only on `\n`).
//! - `col`: 0-based UTF-16 code-unit column of the first char.
//! - `start`/`stop`: 0-based inclusive UTF-16 offsets into the (newline-appended)
//!   input. EOF has `stop == start - 1` and text `"<EOF>"`.

use crate::token::TokenKind::*;
use crate::token::{Channel, Token, TokenKind};

// ---------------------------------------------------------------------------
// Modes & actions
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Default,
    RulesetOrInsert,
    ParamRuleset,
    ListContexts,
    ListCodes,
}

#[derive(Clone, Copy)]
enum Action {
    Nothing,
    Push(Mode),
    Pop,
    PopPop,
}

#[derive(Clone, Copy)]
struct Cand {
    len: usize, // length in CHARS
    kind: TokenKind,
    skip: bool,
    action: Action,
}

// ---------------------------------------------------------------------------
// Character-class helpers (fragments WS / NONWS / RSNONWS / NONWS_STR)
// ---------------------------------------------------------------------------

#[inline]
fn is_ws(c: char) -> bool {
    // fragment WS: [ \t\r\n\f ]
    matches!(c, ' ' | '\t' | '\r' | '\n' | '\u{000C}' | '\u{00A0}')
}
#[inline]
fn is_nonws(c: char) -> bool {
    !is_ws(c)
}
#[inline]
fn is_rsnonws(c: char) -> bool {
    // fragment RSNONWS: ~[ \t\r\n\f (]
    is_nonws(c) && c != '('
}
#[inline]
fn is_nonws_str(c: char) -> bool {
    // fragment NONWS_STR: ~[ \t\r\n\f \\"]
    is_nonws(c) && c != '\\' && c != '"'
}

#[inline]
fn dig(s: &[char], k: usize) -> bool {
    k < s.len() && s[k].is_ascii_digit()
}
#[inline]
fn chr(s: &[char], k: usize, c: char) -> bool {
    k < s.len() && s[k] == c
}

/// If `lit` occurs at `s[i..]`, return its length in chars.
fn starts_with(s: &[char], i: usize, lit: &str) -> Option<usize> {
    let mut j = i;
    for ch in lit.chars() {
        if j >= s.len() || s[j] != ch {
            return None;
        }
        j += 1;
    }
    Some(j - i)
}

fn ws_run(s: &[char], mut j: usize) -> usize {
    while j < s.len() && is_ws(s[j]) {
        j += 1;
    }
    j
}

// ---------------------------------------------------------------------------
// Token-rule matchers (return match length in chars)
// ---------------------------------------------------------------------------

// KW: 'word' WS* ':'
fn kw_colon(s: &[char], i: usize, lit: &str) -> Option<usize> {
    let l = starts_with(s, i, lit)?;
    let j = ws_run(s, i + l);
    if chr(s, j, ':') {
        Some(j + 1 - i)
    } else {
        None
    }
}

// KW: '(' WS* 'word' WS* ')'
fn kw_paren(s: &[char], i: usize, word: &str) -> Option<usize> {
    if !chr(s, i, '(') {
        return None;
    }
    let j = ws_run(s, i + 1);
    let l = starts_with(s, j, word)?;
    let j = ws_run(s, j + l);
    if chr(s, j, ')') {
        Some(j + 1 - i)
    } else {
        None
    }
}

// STAR: ([\r\n] | LINE_COMMENT) WS* '*' [  ]
fn m_star(s: &[char], i: usize) -> Option<usize> {
    let mut j;
    if chr(s, i, '\r') || chr(s, i, '\n') {
        j = i + 1;
    } else if let Some(l) = m_line_comment(s, i) {
        j = i + l;
    } else {
        return None;
    }
    j = ws_run(s, j);
    if !chr(s, j, '*') {
        return None;
    }
    j += 1;
    if j < s.len() && (s[j] == ' ' || s[j] == '\u{00A0}') {
        Some(j + 1 - i)
    } else {
        None
    }
}

// STRING: '"' (~[\\"] | '\\u' | '\\r' | '\\n' | '\\t' | '\\"' | '\\\\')* '"'
fn m_string(s: &[char], i: usize) -> Option<usize> {
    if !chr(s, i, '"') {
        return None;
    }
    let mut j = i + 1;
    while j < s.len() {
        let c = s[j];
        if c == '"' {
            return Some(j + 1 - i);
        }
        if c == '\\' {
            if j + 1 >= s.len() {
                return None;
            }
            match s[j + 1] {
                'u' | 'r' | 'n' | 't' | '"' | '\\' => j += 2,
                _ => return None,
            }
        } else {
            j += 1;
        }
    }
    None
}

// MULTILINE_STRING: '"""' .*? '"""'
fn m_multiline_string(s: &[char], i: usize) -> Option<usize> {
    starts_with(s, i, "\"\"\"")?;
    let mut k = i + 3;
    while k + 2 < s.len() + 1 {
        if chr(s, k, '"') && chr(s, k + 1, '"') && chr(s, k + 2, '"') {
            return Some(k + 3 - i);
        }
        k += 1;
    }
    None
}

// NUMBER: [+\-]? [0-9]+ ('.' [0-9]+)? ([eE] [+\-]? [0-9]+)?
fn m_number(s: &[char], i: usize) -> Option<usize> {
    let mut j = i;
    if chr(s, j, '+') || chr(s, j, '-') {
        j += 1;
    }
    let ds = j;
    while dig(s, j) {
        j += 1;
    }
    if j == ds {
        return None;
    }
    if chr(s, j, '.') && dig(s, j + 1) {
        j += 1;
        while dig(s, j) {
            j += 1;
        }
    }
    if chr(s, j, 'e') || chr(s, j, 'E') {
        let mut k = j + 1;
        if chr(s, k, '+') || chr(s, k, '-') {
            k += 1;
        }
        if dig(s, k) {
            k += 1;
            while dig(s, k) {
                k += 1;
            }
            j = k;
        }
    }
    Some(j - i)
}

// UNIT: '\'' (~[\\'])* '\''
fn m_unit(s: &[char], i: usize) -> Option<usize> {
    if !chr(s, i, '\'') {
        return None;
    }
    let mut j = i + 1;
    while j < s.len() {
        let c = s[j];
        if c == '\'' {
            return Some(j + 1 - i);
        }
        if c == '\\' {
            return None;
        }
        j += 1;
    }
    None
}

// CONCEPT_STRING word: (NONWS_STR | '\\"' | '\\\\')+  — returns new index or None
fn cs_word(s: &[char], mut j: usize) -> Option<usize> {
    let start = j;
    loop {
        if chr(s, j, '\\') && j + 1 < s.len() && (s[j + 1] == '"' || s[j + 1] == '\\') {
            j += 2;
        } else if j < s.len() && is_nonws_str(s[j]) {
            j += 1;
        } else {
            break;
        }
    }
    if j > start {
        Some(j)
    } else {
        None
    }
}

// CONCEPT_STRING: '"' word (WS word)* '"'
fn m_concept_string(s: &[char], i: usize) -> Option<usize> {
    if !chr(s, i, '"') {
        return None;
    }
    let mut j = cs_word(s, i + 1)?;
    loop {
        if j < s.len() && is_ws(s[j]) {
            if let Some(nj) = cs_word(s, j + 1) {
                j = nj;
                continue;
            }
        }
        break;
    }
    if chr(s, j, '"') {
        Some(j + 1 - i)
    } else {
        None
    }
}

// TIME: [0-9][0-9](':'[0-9][0-9](':'[0-9][0-9]('.'[0-9]+)?)?)?('Z' | ('+'|'-')[0-9][0-9]':'[0-9][0-9])?
fn m_time(s: &[char], i: usize) -> Option<usize> {
    if !(dig(s, i) && dig(s, i + 1)) {
        return None;
    }
    let mut j = i + 2;
    if chr(s, j, ':') && dig(s, j + 1) && dig(s, j + 2) {
        j += 3;
        if chr(s, j, ':') && dig(s, j + 1) && dig(s, j + 2) {
            j += 3;
            if chr(s, j, '.') && dig(s, j + 1) {
                j += 1;
                while dig(s, j) {
                    j += 1;
                }
            }
        }
    }
    if chr(s, j, 'Z') {
        j += 1;
    } else if (chr(s, j, '+') || chr(s, j, '-'))
        && dig(s, j + 1)
        && dig(s, j + 2)
        && chr(s, j + 3, ':')
        && dig(s, j + 4)
        && dig(s, j + 5)
    {
        j += 6;
    }
    Some(j - i)
}

// DATETIME: [0-9]{4} ('-'[0-9][0-9] ('-'[0-9][0-9] ('T' TIME)?)?)?
fn m_datetime(s: &[char], i: usize) -> Option<usize> {
    if !(dig(s, i) && dig(s, i + 1) && dig(s, i + 2) && dig(s, i + 3)) {
        return None;
    }
    let mut j = i + 4;
    if chr(s, j, '-') && dig(s, j + 1) && dig(s, j + 2) {
        j += 3;
        if chr(s, j, '-') && dig(s, j + 1) && dig(s, j + 2) {
            j += 3;
            if chr(s, j, 'T') {
                if let Some(tl) = m_time(s, j + 1) {
                    j = j + 1 + tl;
                }
            }
        }
    }
    Some(j - i)
}

// CARD: ([0-9]+)? '..' ([0-9]+ | '*')?
fn m_card(s: &[char], i: usize) -> Option<usize> {
    let mut j = i;
    while dig(s, j) {
        j += 1;
    }
    if !(chr(s, j, '.') && chr(s, j + 1, '.')) {
        return None;
    }
    j += 2;
    if dig(s, j) {
        while dig(s, j) {
            j += 1;
        }
    } else if chr(s, j, '*') {
        j += 1;
    }
    Some(j - i)
}

// CARET_SEQUENCE: '^' NONWS+
fn m_caret_sequence(s: &[char], i: usize) -> Option<usize> {
    if !chr(s, i, '^') {
        return None;
    }
    let mut j = i + 1;
    while j < s.len() && is_nonws(s[j]) {
        j += 1;
    }
    if j > i + 1 {
        Some(j - i)
    } else {
        None
    }
}

// REGEX: '/' ('\\/' | ~[*/\r\n]) ('\\/' | ~[/\r\n])* '/'
fn m_regex(s: &[char], i: usize) -> Option<usize> {
    if !chr(s, i, '/') {
        return None;
    }
    let mut j = i + 1;
    // first element
    if chr(s, j, '\\') && chr(s, j + 1, '/') {
        j += 2;
    } else if j < s.len() && !matches!(s[j], '*' | '/' | '\r' | '\n') {
        j += 1;
    } else {
        return None;
    }
    // remaining elements
    loop {
        if chr(s, j, '\\') && chr(s, j + 1, '/') {
            j += 2;
        } else if j < s.len() && !matches!(s[j], '/' | '\r' | '\n') {
            j += 1;
        } else {
            break;
        }
    }
    if chr(s, j, '/') {
        Some(j + 1 - i)
    } else {
        None
    }
}

// BLOCK_COMMENT: '/*' .*? '*/'
fn m_block_comment(s: &[char], i: usize) -> Option<usize> {
    starts_with(s, i, "/*")?;
    let mut k = i + 2;
    while k < s.len() {
        if chr(s, k, '*') && chr(s, k + 1, '/') {
            return Some(k + 2 - i);
        }
        k += 1;
    }
    None
}

// LINE_COMMENT: '//' ~[\r\n]* [\r\n]
fn m_line_comment(s: &[char], i: usize) -> Option<usize> {
    starts_with(s, i, "//")?;
    let mut j = i + 2;
    while j < s.len() && s[j] != '\r' && s[j] != '\n' {
        j += 1;
    }
    if j < s.len() && (s[j] == '\r' || s[j] == '\n') {
        Some(j + 1 - i)
    } else {
        None
    }
}

// SEQUENCE: NONWS+
fn m_sequence(s: &[char], i: usize) -> Option<usize> {
    let mut j = i;
    while j < s.len() && is_nonws(s[j]) {
        j += 1;
    }
    if j > i {
        Some(j - i)
    } else {
        None
    }
}

// CODE: SEQUENCE? '#' (SEQUENCE | CONCEPT_STRING)
// Returns the maximal match length (maximal munch over the optional prefix).
fn m_code(s: &[char], i: usize) -> Option<usize> {
    let n = s.len();
    let mut best: Option<usize> = None;
    let mut p = i;
    loop {
        if p < n && s[p] == '#' {
            // tail option 1: SEQUENCE (NONWS+)
            let mut q = p + 1;
            while q < n && is_nonws(s[q]) {
                q += 1;
            }
            if q > p + 1 {
                let len = q - i;
                if best.is_none_or(|b| len > b) {
                    best = Some(len);
                }
            }
            // tail option 2: CONCEPT_STRING
            if let Some(cl) = m_concept_string(s, p + 1) {
                let len = p + 1 + cl - i;
                if best.is_none_or(|b| len > b) {
                    best = Some(len);
                }
            }
        }
        // extend optional NONWS prefix
        if p < n && is_nonws(s[p]) {
            p += 1;
        } else {
            break;
        }
    }
    best
}

/// Is `s[a..b]` exactly a valid CODE?
fn code_exact(s: &[char], a: usize, b: usize) -> bool {
    if b <= a {
        return false;
    }
    for h in a..b {
        if s[h] != '#' {
            continue;
        }
        if !(a..h).all(|k| is_nonws(s[k])) {
            continue;
        }
        // SEQUENCE tail
        if b > h + 1 && (h + 1..b).all(|k| is_nonws(s[k])) {
            return true;
        }
        // CONCEPT_STRING tail
        if m_concept_string(s, h + 1) == Some(b - (h + 1)) {
            return true;
        }
    }
    false
}

#[inline]
fn seq_exact(s: &[char], a: usize, b: usize) -> bool {
    b > a && (a..b).all(|k| is_nonws(s[k]))
}

// REFERENCE / CODEABLE_REFERENCE / CANONICAL:
//   kw WS* '(' WS* SEQUENCE WS* (WS 'or' WS+ SEQUENCE WS*)* ')'
// (CANONICAL's '|' version is absorbed by NONWS+ so the same matcher applies.)
// Content grammar: `SEQUENCE (WS+ 'or' WS+ SEQUENCE)*` (WS = any whitespace incl
// newlines; SEQUENCE = NONWS+, so `)`/`|` may appear inside a word). Greedy: the
// token ends at the RIGHTMOST `)` whose preceding content is valid (an odd number
// of whitespace-separated words where every odd-index word is exactly `or`).
//
// Single forward pass, O(content) with early termination — the previous version
// re-tokenized content_start..k for every `)` up to EOF (O(n²), unbounded), which
// dominated CPU on Reference/Canonical-heavy IGs (~26% on mCODE).
fn m_ref(s: &[char], i: usize, kw: &str) -> Option<usize> {
    let l = starts_with(s, i, kw)?;
    let j = ws_run(s, i + l);
    if !chr(s, j, '(') {
        return None;
    }
    let content_start = j + 1;
    let mut best = None;

    // Incremental state: words completed before the cursor, whether all completed
    // odd-index words are `or`, and the start of the in-progress word (if any).
    let mut completed = 0usize;
    let mut odd_ok = true;
    let mut word_start: Option<usize> = None;
    let is_or = |st: usize, en: usize| en - st == 2 && s[st] == 'o' && s[st + 1] == 'r';

    let mut k = content_start;
    while k < s.len() {
        let c = s[k];
        if c == ')' {
            // Candidate terminator: content is content_start..k (excludes this `)`).
            let (count, last_ok) = match word_start {
                Some(ws_) => (completed + 1, completed % 2 == 0 || is_or(ws_, k)),
                None => (completed, true),
            };
            if count % 2 == 1 && odd_ok && last_ok {
                best = Some(k + 1 - i);
            }
            // `)` is also a NONWS word char — keep extending the current word.
            if word_start.is_none() {
                word_start = Some(k);
            }
        } else if is_ws(c) {
            if let Some(ws_) = word_start {
                if completed % 2 == 1 && !is_or(ws_, k) {
                    odd_ok = false; // odd-index word != "or" → permanently invalid
                }
                completed += 1;
                word_start = None;
                if !odd_ok {
                    break;
                }
            }
        } else if word_start.is_none() {
            word_start = Some(k);
        }
        k += 1;
    }
    best
}

// ---------------------------------------------------------------------------
// List-mode item matchers
// ---------------------------------------------------------------------------

/// `prefix WS* term` where the prefix predicate decides validity of `s[i..m]`.
/// Returns the longest total length (incl. terminator).
fn item_then_term(
    s: &[char],
    i: usize,
    term: char,
    exact: impl Fn(&[char], usize, usize) -> bool,
) -> Option<usize> {
    let mut best = None;
    let mut t = i;
    while t < s.len() {
        if s[t] == term {
            let mut m = t;
            while m > i && is_ws(s[m - 1]) {
                m -= 1;
            }
            if m > i && exact(s, i, m) {
                best = Some(t + 1 - i);
            }
        }
        t += 1;
    }
    best
}

// RULESET_OR_INSERT mode -----------------------------------------------------

// PARAM_RULESET_REFERENCE: WS* RSNONWS+ WS* '('
fn m_param_ruleset_reference(s: &[char], i: usize) -> Option<usize> {
    let j = ws_run(s, i);
    let start = j;
    let mut j = j;
    while j < s.len() && is_rsnonws(s[j]) {
        j += 1;
    }
    if j == start {
        return None;
    }
    let j = ws_run(s, j);
    if chr(s, j, '(') {
        Some(j + 1 - i)
    } else {
        None
    }
}

// RULESET_REFERENCE: WS* RSNONWS+
fn m_ruleset_reference(s: &[char], i: usize) -> Option<usize> {
    let start = ws_run(s, i);
    let mut j = start;
    while j < s.len() && is_rsnonws(s[j]) {
        j += 1;
    }
    if j == start {
        None
    } else {
        Some(j - i)
    }
}

// PARAM_RULESET_OR_INSERT mode ----------------------------------------------

// BRACKETED_PARAM / LAST_BRACKETED_PARAM:
//   WS* '[[' ( ~[\]] | (']'~[\]]) | (']]' WS* ~[,) \t\r\n\f ]) )+ ']]' WS* term
fn m_bracketed(s: &[char], i: usize, term: char) -> Option<usize> {
    let n = s.len();
    let j0 = ws_run(s, i);
    if !(chr(s, j0, '[') && chr(s, j0 + 1, '[')) {
        return None;
    }
    let content_start = j0 + 2;
    let mut j = content_start;
    loop {
        if j >= n {
            return None;
        }
        let c = s[j];
        if c != ']' {
            j += 1;
            continue;
        }
        // c == ']'
        if chr(s, j + 1, ']') {
            // possible closing ']]' or embedded (alt3)
            let k = ws_run(s, j + 2);
            let is_alt3 = k < n && !is_ws(s[k]) && s[k] != ',' && s[k] != ')';
            if is_alt3 {
                j = k + 1; // consume ']]' WS* and the char
                continue;
            } else {
                break; // closing ']]' begins at j
            }
        } else if j + 1 < n {
            // ']' ~[\]]
            j += 2;
            continue;
        } else {
            return None;
        }
    }
    if j == content_start {
        return None; // need at least one content element
    }
    if !(chr(s, j, ']') && chr(s, j + 1, ']')) {
        return None;
    }
    j += 2;
    let j = ws_run(s, j);
    if chr(s, j, term) {
        Some(j + 1 - i)
    } else {
        None
    }
}

// PLAIN_PARAM / LAST_PLAIN_PARAM: WS* ('\)' | '\,' | '\\' | ~[),])* WS* term
fn m_plain(s: &[char], i: usize, term: char) -> Option<usize> {
    let n = s.len();
    let mut j = ws_run(s, i);
    loop {
        if chr(s, j, '\\') && j + 1 < n && matches!(s[j + 1], ')' | ',' | '\\') {
            j += 2;
        } else if j < n && s[j] != ')' && s[j] != ',' {
            j += 1;
        } else {
            break;
        }
    }
    let j = ws_run(s, j);
    if chr(s, j, term) {
        Some(j + 1 - i)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Keyword tables (grammar declaration order)
// ---------------------------------------------------------------------------

static COLON_KW: &[(&str, TokenKind, Action)] = &[
    ("Alias", KW_ALIAS, Action::Nothing),
    ("Profile", KW_PROFILE, Action::Nothing),
    ("Extension", KW_EXTENSION, Action::Nothing),
    ("Instance", KW_INSTANCE, Action::Nothing),
    ("InstanceOf", KW_INSTANCEOF, Action::Nothing),
    ("Invariant", KW_INVARIANT, Action::Nothing),
    ("ValueSet", KW_VALUESET, Action::Nothing),
    ("CodeSystem", KW_CODESYSTEM, Action::Nothing),
    ("RuleSet", KW_RULESET, Action::Push(Mode::RulesetOrInsert)),
    ("Mapping", KW_MAPPING, Action::Nothing),
    ("Logical", KW_LOGICAL, Action::Nothing),
    ("Resource", KW_RESOURCE, Action::Nothing),
    ("Parent", KW_PARENT, Action::Nothing),
    ("Id", KW_ID, Action::Nothing),
    ("Title", KW_TITLE, Action::Nothing),
    ("Description", KW_DESCRIPTION, Action::Nothing),
    ("Expression", KW_EXPRESSION, Action::Nothing),
    ("XPath", KW_XPATH, Action::Nothing),
    ("Severity", KW_SEVERITY, Action::Nothing),
    ("Usage", KW_USAGE, Action::Nothing),
    ("Source", KW_SOURCE, Action::Nothing),
    ("Target", KW_TARGET, Action::Nothing),
    ("Context", KW_CONTEXT, Action::Push(Mode::ListContexts)),
    ("Characteristics", KW_CHARACTERISTICS, Action::Push(Mode::ListCodes)),
];

#[derive(Clone, Copy)]
enum KwForm {
    Bare,
    Paren,
}

static SIMPLE_KW: &[(&str, TokenKind, KwForm, Action)] = &[
    ("?!", KW_MOD, KwForm::Bare, Action::Nothing),
    ("MS", KW_MS, KwForm::Bare, Action::Nothing),
    ("SU", KW_SU, KwForm::Bare, Action::Nothing),
    ("TU", KW_TU, KwForm::Bare, Action::Nothing),
    ("N", KW_NORMATIVE, KwForm::Bare, Action::Nothing),
    ("D", KW_DRAFT, KwForm::Bare, Action::Nothing),
    ("from", KW_FROM, KwForm::Bare, Action::Nothing),
    ("example", KW_EXAMPLE, KwForm::Paren, Action::Nothing),
    ("preferred", KW_PREFERRED, KwForm::Paren, Action::Nothing),
    ("extensible", KW_EXTENSIBLE, KwForm::Paren, Action::Nothing),
    ("required", KW_REQUIRED, KwForm::Paren, Action::Nothing),
    ("contains", KW_CONTAINS, KwForm::Bare, Action::Nothing),
    ("named", KW_NAMED, KwForm::Bare, Action::Nothing),
    ("and", KW_AND, KwForm::Bare, Action::Nothing),
    ("only", KW_ONLY, KwForm::Bare, Action::Nothing),
    ("or", KW_OR, KwForm::Bare, Action::Nothing),
    ("obeys", KW_OBEYS, KwForm::Bare, Action::Nothing),
    ("true", KW_TRUE, KwForm::Bare, Action::Nothing),
    ("false", KW_FALSE, KwForm::Bare, Action::Nothing),
    ("include", KW_INCLUDE, KwForm::Bare, Action::Nothing),
    ("exclude", KW_EXCLUDE, KwForm::Bare, Action::Nothing),
    ("codes", KW_CODES, KwForm::Bare, Action::Nothing),
    ("where", KW_WHERE, KwForm::Bare, Action::Nothing),
    ("valueset", KW_VSREFERENCE, KwForm::Bare, Action::Nothing),
    ("system", KW_SYSTEM, KwForm::Bare, Action::Nothing),
    ("exactly", KW_EXACTLY, KwForm::Paren, Action::Nothing),
    ("insert", KW_INSERT, KwForm::Bare, Action::Push(Mode::RulesetOrInsert)),
    ("contentReference", KW_CONTENTREFERENCE, KwForm::Bare, Action::Nothing),
];

// ---------------------------------------------------------------------------
// Per-mode candidate selection
// ---------------------------------------------------------------------------

#[inline]
fn consider(best: &mut Option<Cand>, len: Option<usize>, kind: TokenKind, skip: bool, action: Action) {
    if let Some(l) = len {
        if l == 0 {
            return;
        }
        let replace = match best {
            Some(b) => l > b.len, // strictly greater -> first-declared wins ties
            None => true,
        };
        if replace {
            *best = Some(Cand { len: l, kind, skip, action });
        }
    }
}

fn match_default(s: &[char], i: usize) -> Option<Cand> {
    let mut best: Option<Cand> = None;

    for &(lit, kind, action) in COLON_KW {
        consider(&mut best, kw_colon(s, i, lit), kind, false, action);
    }
    for &(lit, kind, form, action) in SIMPLE_KW {
        let len = match form {
            KwForm::Bare => starts_with(s, i, lit),
            KwForm::Paren => kw_paren(s, i, lit),
        };
        consider(&mut best, len, kind, false, action);
    }

    // SYMBOLS
    consider(&mut best, single(s, i, '='), EQUAL, false, Action::Nothing);
    consider(&mut best, m_star(s, i), STAR, false, Action::Nothing);
    consider(&mut best, single(s, i, ':'), COLON, false, Action::Nothing);
    consider(&mut best, single(s, i, ','), COMMA, false, Action::Nothing);
    consider(&mut best, starts_with(s, i, "->"), ARROW, false, Action::Nothing);

    // PATTERNS
    consider(&mut best, m_string(s, i), STRING, false, Action::Nothing);
    consider(&mut best, m_multiline_string(s, i), MULTILINE_STRING, false, Action::Nothing);
    consider(&mut best, m_number(s, i), NUMBER, false, Action::Nothing);
    consider(&mut best, m_unit(s, i), UNIT, false, Action::Nothing);
    consider(&mut best, m_code(s, i), CODE, false, Action::Nothing);
    consider(&mut best, m_concept_string(s, i), CONCEPT_STRING, false, Action::Nothing);
    consider(&mut best, m_datetime(s, i), DATETIME, false, Action::Nothing);
    consider(&mut best, m_time(s, i), TIME, false, Action::Nothing);
    consider(&mut best, m_card(s, i), CARD, false, Action::Nothing);
    consider(&mut best, m_ref(s, i, "Reference"), REFERENCE, false, Action::Nothing);
    consider(&mut best, m_ref(s, i, "CodeableReference"), CODEABLE_REFERENCE, false, Action::Nothing);
    consider(&mut best, m_ref(s, i, "Canonical"), CANONICAL, false, Action::Nothing);
    consider(&mut best, m_caret_sequence(s, i), CARET_SEQUENCE, false, Action::Nothing);
    consider(&mut best, m_regex(s, i), REGEX, false, Action::Nothing);
    consider(&mut best, m_block_comment(s, i), BLOCK_COMMENT, true, Action::Nothing);
    consider(&mut best, m_sequence(s, i), SEQUENCE, false, Action::Nothing);

    // IGNORED
    consider(&mut best, single_ws(s, i), WHITESPACE, false, Action::Nothing);
    consider(&mut best, m_line_comment(s, i), LINE_COMMENT, true, Action::Nothing);

    best
}

fn match_ruleset_or_insert(s: &[char], i: usize) -> Option<Cand> {
    let mut best: Option<Cand> = None;
    consider(
        &mut best,
        m_param_ruleset_reference(s, i),
        PARAM_RULESET_REFERENCE,
        false,
        Action::Push(Mode::ParamRuleset),
    );
    consider(&mut best, m_ruleset_reference(s, i), RULESET_REFERENCE, false, Action::Pop);
    best
}

fn match_param_ruleset(s: &[char], i: usize) -> Option<Cand> {
    let mut best: Option<Cand> = None;
    consider(&mut best, m_bracketed(s, i, ','), BRACKETED_PARAM, false, Action::Nothing);
    consider(&mut best, m_bracketed(s, i, ')'), LAST_BRACKETED_PARAM, false, Action::PopPop);
    consider(&mut best, m_plain(s, i, ','), PLAIN_PARAM, false, Action::Nothing);
    consider(&mut best, m_plain(s, i, ')'), LAST_PLAIN_PARAM, false, Action::PopPop);
    best
}

fn match_list_contexts(s: &[char], i: usize) -> Option<Cand> {
    let mut best: Option<Cand> = None;
    // QUOTED_CONTEXT: STRING WS* ','
    let quoted = m_string(s, i).and_then(|l| {
        let j = ws_run(s, i + l);
        if chr(s, j, ',') {
            Some(j + 1 - i)
        } else {
            None
        }
    });
    consider(&mut best, quoted, QUOTED_CONTEXT, false, Action::Nothing);
    consider(&mut best, m_string(s, i), LAST_QUOTED_CONTEXT, false, Action::Pop);
    // UNQUOTED_CONTEXT: (SEQUENCE | CODE) WS* ','
    consider(
        &mut best,
        item_then_term(s, i, ',', |s, a, b| seq_exact(s, a, b) || code_exact(s, a, b)),
        UNQUOTED_CONTEXT,
        false,
        Action::Nothing,
    );
    // LAST_UNQUOTED_CONTEXT: (SEQUENCE | CODE)
    let last_unquoted = max_opt(m_sequence(s, i), m_code(s, i));
    consider(&mut best, last_unquoted, LAST_UNQUOTED_CONTEXT, false, Action::Pop);
    // CONTEXT_WHITESPACE: WS
    consider(&mut best, single_ws(s, i), CONTEXT_WHITESPACE, false, Action::Nothing);
    best
}

fn match_list_codes(s: &[char], i: usize) -> Option<Cand> {
    let mut best: Option<Cand> = None;
    // CODE_ITEM: CODE WS* ','
    consider(
        &mut best,
        item_then_term(s, i, ',', code_exact),
        CODE_ITEM,
        false,
        Action::Nothing,
    );
    // LAST_CODE_ITEM: CODE
    consider(&mut best, m_code(s, i), LAST_CODE_ITEM, false, Action::Pop);
    // CODE_LIST_WHITESPACE: WS
    consider(&mut best, single_ws(s, i), CODE_LIST_WHITESPACE, false, Action::Nothing);
    best
}

#[inline]
fn single(s: &[char], i: usize, c: char) -> Option<usize> {
    if chr(s, i, c) {
        Some(1)
    } else {
        None
    }
}
#[inline]
fn single_ws(s: &[char], i: usize) -> Option<usize> {
    if i < s.len() && is_ws(s[i]) {
        Some(1)
    } else {
        None
    }
}
#[inline]
fn max_opt(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) => Some(x),
        (None, b) => b,
    }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    u16off: i64,
    line: u32,
    col: u32,
    mode: Mode,
    stack: Vec<Mode>,
    out: Vec<Token>,
}

impl Lexer {
    fn advance(&mut self) {
        let c = self.chars[self.pos];
        let w = c.len_utf16() as i64;
        if c == '\n' {
            self.line += 1;
            self.col = 0;
        } else {
            self.col += c.len_utf16() as u32;
        }
        self.u16off += w;
        self.pos += 1;
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::Nothing => {}
            Action::Push(m) => {
                self.stack.push(self.mode);
                self.mode = m;
            }
            Action::Pop => {
                self.mode = self.stack.pop().unwrap_or(Mode::Default);
            }
            Action::PopPop => {
                self.mode = self.stack.pop().unwrap_or(Mode::Default);
                self.mode = self.stack.pop().unwrap_or(Mode::Default);
            }
        }
    }

    fn run(mut self) -> Vec<Token> {
        while self.pos < self.chars.len() {
            let cand = match self.mode {
                Mode::Default => match_default(&self.chars, self.pos),
                Mode::RulesetOrInsert => match_ruleset_or_insert(&self.chars, self.pos),
                Mode::ParamRuleset => match_param_ruleset(&self.chars, self.pos),
                Mode::ListContexts => match_list_contexts(&self.chars, self.pos),
                Mode::ListCodes => match_list_codes(&self.chars, self.pos),
            };
            match cand {
                Some(c) => {
                    let start_pos = self.pos;
                    let line = self.line;
                    let col = self.col;
                    let start = self.u16off;
                    let text: String = self.chars[start_pos..start_pos + c.len].iter().collect();
                    for _ in 0..c.len {
                        self.advance();
                    }
                    let stop = self.u16off - 1;
                    self.apply_action(c.action);
                    if !c.skip {
                        let channel = match c.kind {
                            WHITESPACE | CONTEXT_WHITESPACE | CODE_LIST_WHITESPACE => Channel::Hidden,
                            _ => Channel::Default,
                        };
                        self.out.push(Token {
                            kind: c.kind,
                            channel,
                            text,
                            line,
                            col,
                            start,
                            stop,
                        });
                    }
                }
                None => {
                    // ANTLR-style error recovery: consume one char and retry.
                    self.advance();
                }
            }
        }
        self.out.push(Token {
            kind: EOF,
            channel: Channel::Default,
            text: "<EOF>".to_string(),
            line: self.line,
            col: self.col,
            start: self.u16off,
            stop: self.u16off - 1,
        });
        self.out
    }
}

/// Lex an FSH document. Appends a trailing `\n` if the content does not already
/// end in one (mirrors the importer), then produces the full token stream
/// (including HIDDEN-channel whitespace and a final EOF). Skipped tokens
/// (LINE_COMMENT, BLOCK_COMMENT) are dropped.
pub fn lex_document(input: &str) -> Vec<Token> {
    let mut text = input.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let lexer = Lexer {
        chars: text.chars().collect(),
        pos: 0,
        u16off: 0,
        line: 1,
        col: 0,
        mode: Mode::Default,
        stack: Vec::new(),
        out: Vec::new(),
    };
    lexer.run()
}
