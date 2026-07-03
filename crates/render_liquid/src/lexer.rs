//! Tokenizer: split a template into raw text, `{{ output }}` and `{% tag %}`
//! tokens, honoring Liquid whitespace control (`{{-`, `-}}`, `{%-`, `-%}`).
//!
//! Whitespace control semantics (liquid-4.0.4/lib/liquid/block_body.rb
//! `WhitespaceControl` + `Liquid::Template` trim): a `-` immediately inside a
//! delimiter strips ALL whitespace on the adjacent side of the *neighboring raw
//! text*. `{%- ... -%}` strips whitespace before and after; `{{- ... -}}`
//! likewise. The strip removes `[ \t\r\n]+` (Ruby `\s` in the trim regex,
//! which includes form-feed/vertical-tab but IG content only uses space/tab/nl).

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    /// Literal text between markup.
    Raw(String),
    /// `{{ expr }}` — the inner source (trimmed of the surrounding braces).
    Output {
        inner: String,
        trim_left: bool,
        trim_right: bool,
    },
    /// `{% tag args %}` — inner source.
    Tag {
        inner: String,
        trim_left: bool,
        trim_right: bool,
    },
}

pub fn tokenize(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0usize;
    let mut raw_start = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && (bytes[i + 1] == b'{' || bytes[i + 1] == b'%')
        {
            // flush preceding raw
            if raw_start < i {
                tokens.push(Token::Raw(src[raw_start..i].to_string()));
            }
            let is_output = bytes[i + 1] == b'{';
            let (close_a, close_b) = if is_output { (b'}', b'}') } else { (b'%', b'}') };
            let open_end = i + 2;
            let trim_left = open_end < bytes.len() && bytes[open_end] == b'-';
            let content_start = if trim_left { open_end + 1 } else { open_end };

            // find closing delimiter
            let mut j = content_start;
            let mut found = None;
            while j + 1 < bytes.len() {
                if bytes[j] == close_a && bytes[j + 1] == close_b {
                    found = Some(j);
                    break;
                }
                // trimming close `-%}` / `-}}`
                if bytes[j] == b'-'
                    && j + 2 < bytes.len()
                    && bytes[j + 1] == close_a
                    && bytes[j + 2] == close_b
                {
                    found = Some(j);
                    break;
                }
                j += 1;
            }
            let Some(close_pos) = found else {
                // unterminated: treat rest as raw
                raw_start = i;
                i = bytes.len();
                break;
            };
            let trim_right = bytes[close_pos] == b'-';
            let content_end = close_pos;
            let inner = src[content_start..content_end].trim().to_string();
            let after = if trim_right { close_pos + 3 } else { close_pos + 2 };

            if is_output {
                tokens.push(Token::Output {
                    inner,
                    trim_left,
                    trim_right,
                });
            } else {
                tokens.push(Token::Tag {
                    inner,
                    trim_left,
                    trim_right,
                });
            }
            i = after;
            raw_start = after;
        } else {
            i += 1;
        }
    }
    if raw_start < bytes.len() {
        tokens.push(Token::Raw(src[raw_start..].to_string()));
    }

    apply_whitespace_control(tokens)
}

/// Apply `-` trim flags: strip trailing whitespace of the raw token to the LEFT
/// of a `trim_left` marker, and leading whitespace of the raw token to the
/// RIGHT of a `trim_right` marker.
fn apply_whitespace_control(mut tokens: Vec<Token>) -> Vec<Token> {
    let n = tokens.len();
    // Collect trim directives first (immutable borrow), then mutate raws.
    let mut trim_prev_right = vec![false; n]; // token k wants raw k-1's right trimmed
    let mut trim_next_left = vec![false; n]; // token k wants raw k+1's left trimmed
    for (k, t) in tokens.iter().enumerate() {
        match t {
            Token::Output {
                trim_left,
                trim_right,
                ..
            }
            | Token::Tag {
                trim_left,
                trim_right,
                ..
            } => {
                trim_prev_right[k] = *trim_left;
                trim_next_left[k] = *trim_right;
            }
            Token::Raw(_) => {}
        }
    }
    for k in 0..n {
        if trim_prev_right[k] && k > 0 {
            if let Token::Raw(s) = &mut tokens[k - 1] {
                let trimmed = s.trim_end_matches([' ', '\t', '\r', '\n']);
                s.truncate(trimmed.len());
            }
        }
        if trim_next_left[k] && k + 1 < n {
            if let Token::Raw(s) = &mut tokens[k + 1] {
                let start = s.len() - s.trim_start_matches([' ', '\t', '\r', '\n']).len();
                s.drain(..start);
            }
        }
    }
    tokens
}
