//! Shared helpers: HTML escaping and the kramdown GFM auto-id algorithm.

use std::collections::HashMap;

/// Escape text for HTML **text/PCDATA** context, matching kramdown's
/// `entity_output: as_char`: only `&`, `<`, `>` are escaped. Quotes are left
/// literal (the FHIR template's smart_quotes config keeps ASCII quotes).
pub fn escape_html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape text for an HTML **attribute value** (double-quoted). kramdown
/// escapes `&`, `<`, `>` and `"` in attribute values.
pub fn escape_html_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Is `c` a Unicode "word" character per Ruby's `\p{Word}` (used by kramdown's
/// GFM header-id regex `/[^\p{Word}\- \t]/`)?
///
/// Ruby's `\p{Word}` = Unicode categories Letter (L*), Mark (M*), Decimal
/// Number (Nd), Connector Punctuation (Pc, which includes `_`), plus (in
/// Ruby/Onigmo) also `\p{Join_Control}`. For our corpus the meaningful members
/// are: ASCII letters/digits, `_`, and Unicode letters (é, ü, …) and combining
/// marks. We approximate `\p{Word}` as: alphanumeric (Unicode) OR mark OR `_`.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || is_mark(c)
}

/// Approximate Unicode Mark (Mn/Mc/Me) membership. `char::is_alphanumeric`
/// already covers letters/digits; combining marks are the remaining
/// `\p{Word}` members that appear in decomposed accented text.
fn is_mark(c: char) -> bool {
    // Combining diacritical marks and common combining ranges.
    matches!(c as u32,
        0x0300..=0x036F | // Combining Diacritical Marks
        0x1AB0..=0x1AFF | // Combining Diacritical Marks Extended
        0x1DC0..=0x1DFF | // Combining Diacritical Marks Supplement
        0x20D0..=0x20FF | // Combining Diacritical Marks for Symbols
        0xFE20..=0xFE2F   // Combining Half Marks
    )
}

/// kramdown-parser-gfm's `generate_gfm_header_id` (v1.1.0,
/// lib/kramdown/parser/gfm.rb:132-144), the algorithm Jekyll uses for heading
/// ids. Verbatim behavior:
///
/// ```ruby
/// NON_WORD_RE = /[^\p{Word}\- \t]/
/// def generate_gfm_header_id(text)
///   result = text.downcase
///   result.gsub!(NON_WORD_RE, '')
///   result.tr!(" \t", '-')
///   @id_counter[result] += 1
///   counter_result = @id_counter[result]
///   result << "-#{counter_result}" if counter_result > 0
///   @options[:auto_id_prefix] + result
/// end
/// ```
///
/// Notes:
/// * `text` is the header's *raw text*: concatenated plain text of text spans,
///   codespans, entities and smart-quote/typographic chars — NOT the rendered
///   HTML. Emphasis/link markup contributes only its text content.
/// * downcase → remove every char that is not `\p{Word}`, `-`, space or tab →
///   spaces/tabs to `-` (1:1, never collapsed). Underscores and Unicode
///   letters survive; ASCII punctuation and hyphens-in-source both... wait:
///   hyphens ARE `\-` in the class so they SURVIVE.
/// * Uniqueness: a per-document counter; the first occurrence of an id gets no
///   suffix, the 2nd gets `-1`, the 3rd `-2`, … (counter starts at -1, is
///   pre-incremented; suffix appended only when the resulting counter > 0).
pub struct IdGen {
    counter: HashMap<String, i64>,
}

impl IdGen {
    pub fn new() -> Self {
        IdGen {
            counter: HashMap::new(),
        }
    }

    pub fn generate(&mut self, raw_text: &str) -> String {
        // downcase
        let lowered = raw_text.to_lowercase();
        // gsub NON_WORD_RE, '' — keep \p{Word}, '-', ' ', '\t'
        let mut result = String::with_capacity(lowered.len());
        for c in lowered.chars() {
            if is_word_char(c) || c == '-' || c == ' ' || c == '\t' {
                result.push(c);
            }
        }
        // tr ' \t' -> '-'
        let result: String = result
            .chars()
            .map(|c| if c == ' ' || c == '\t' { '-' } else { c })
            .collect();
        // uniqueness: counter starts implicitly at -1 (Hash.new(-1) semantics
        // is NOT used; Ruby uses @id_counter with default 0 via `+= 1`).
        // In gfm.rb @id_counter = Hash.new(-1), so first += 1 => 0 (no suffix),
        // second => 1 (suffix -1). Replicate that.
        let entry = self.counter.entry(result.clone()).or_insert(-1);
        *entry += 1;
        let n = *entry;
        if n > 0 {
            format!("{result}-{n}")
        } else {
            result
        }
    }
}

impl Default for IdGen {
    fn default() -> Self {
        Self::new()
    }
}
