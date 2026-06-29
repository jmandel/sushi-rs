//! FSH lexer (port of FSHLexer.g4). IMPLEMENTATION PENDING — see tests/lex_parity.rs.

use crate::token::Token;

/// Lex an FSH document. Mirrors the importer: appends a trailing `\n` if the
/// content does not already end in one (so a trailing line comment tokenizes),
/// then produces the full token stream including HIDDEN-channel whitespace and a
/// final EOF token. Skipped tokens (LINE_COMMENT, BLOCK_COMMENT) are dropped.
pub fn lex_document(_input: &str) -> Vec<Token> {
    todo!("port FSHLexer.g4 — make tests/lex_parity.rs green against the oracle goldens")
}
