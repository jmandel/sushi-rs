//! FSH token kinds and tokens. The kind set mirrors `FSHLexer.g4`'s symbolic
//! token names EXACTLY (89 kinds + EOF) so the Rust token stream is directly
//! comparable to the ANTLR oracle (`harness/lex-oracle.cjs`).

/// Channel a token belongs to. WHITESPACE/CONTEXT_WHITESPACE/CODE_LIST_WHITESPACE
/// go to HIDDEN; everything else to Default. Skipped tokens (LINE_COMMENT,
/// BLOCK_COMMENT) are dropped entirely, matching ANTLR `-> skip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Default,
    Hidden,
}

macro_rules! token_kinds {
    ($($name:ident),+ $(,)?) => {
        /// Token kind. Variant names match `FSHLexer` symbolic names verbatim.
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum TokenKind {
            $($name,)+
            EOF,
        }

        impl TokenKind {
            /// The symbolic name as emitted by the oracle (e.g. "KW_PROFILE").
            pub fn name(self) -> &'static str {
                match self {
                    $(TokenKind::$name => stringify!($name),)+
                    TokenKind::EOF => "EOF",
                }
            }

            /// Parse a symbolic name back to a kind (for golden comparison).
            pub fn from_name(s: &str) -> Option<TokenKind> {
                match s {
                    $(stringify!($name) => Some(TokenKind::$name),)+
                    "EOF" => Some(TokenKind::EOF),
                    _ => None,
                }
            }
        }
    };
}

token_kinds! {
    KW_ALIAS, KW_PROFILE, KW_EXTENSION, KW_INSTANCE, KW_INSTANCEOF, KW_INVARIANT,
    KW_VALUESET, KW_CODESYSTEM, KW_RULESET, KW_MAPPING, KW_LOGICAL, KW_RESOURCE,
    KW_PARENT, KW_ID, KW_TITLE, KW_DESCRIPTION, KW_EXPRESSION, KW_XPATH,
    KW_SEVERITY, KW_USAGE, KW_SOURCE, KW_TARGET, KW_CONTEXT, KW_CHARACTERISTICS,
    KW_MOD, KW_MS, KW_SU, KW_TU, KW_NORMATIVE, KW_DRAFT, KW_FROM, KW_EXAMPLE,
    KW_PREFERRED, KW_EXTENSIBLE, KW_REQUIRED, KW_CONTAINS, KW_NAMED, KW_AND,
    KW_ONLY, KW_OR, KW_OBEYS, KW_TRUE, KW_FALSE, KW_INCLUDE, KW_EXCLUDE, KW_CODES,
    KW_WHERE, KW_VSREFERENCE, KW_SYSTEM, KW_EXACTLY, KW_INSERT, KW_CONTENTREFERENCE,
    EQUAL, STAR, COLON, COMMA, ARROW,
    STRING, MULTILINE_STRING, NUMBER, UNIT, CODE, CONCEPT_STRING, DATETIME, TIME,
    CARD, REFERENCE, CODEABLE_REFERENCE, CANONICAL, CARET_SEQUENCE, REGEX,
    BLOCK_COMMENT, SEQUENCE, WHITESPACE, LINE_COMMENT,
    PARAM_RULESET_REFERENCE, RULESET_REFERENCE,
    BRACKETED_PARAM, LAST_BRACKETED_PARAM, PLAIN_PARAM, LAST_PLAIN_PARAM,
    QUOTED_CONTEXT, LAST_QUOTED_CONTEXT, UNQUOTED_CONTEXT, LAST_UNQUOTED_CONTEXT,
    CONTEXT_WHITESPACE, CODE_ITEM, LAST_CODE_ITEM, CODE_LIST_WHITESPACE,
}

/// A lexed token. Geometry mirrors ANTLR exactly:
/// - `line`: 1-based line of the token's first char
/// - `col`: 0-based UTF-16 column of the token's first char
/// - `start`/`stop`: 0-based, inclusive UTF-16 code-unit offsets
///   (`stop = start - 1` for empty/EOF tokens)
/// - `text`: the matched text (for STAR this includes the folded `\n…* `)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub channel: Channel,
    pub text: String,
    pub line: u32,
    pub col: u32,
    pub start: i64,
    pub stop: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_roundtrip() {
        for n in [
            "KW_PROFILE",
            "STAR",
            "SEQUENCE",
            "CODE",
            "EOF",
            "WHITESPACE",
        ] {
            assert_eq!(TokenKind::from_name(n).unwrap().name(), n);
        }
        assert!(TokenKind::from_name("NOPE").is_none());
    }
}
