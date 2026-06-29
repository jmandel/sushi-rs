//! FSH lexer + parser -> typed AST with source spans. Phase 2.
//!
//! The lexer reproduces `sushi-ts/antlr/src/main/antlr/FSHLexer.g4` exactly
//! (mode stack, STAR-folds-newline, longest-match) so its token stream is
//! byte-identical to the ANTLR oracle. Verified by `tests/lex_parity.rs`.

pub mod token;
pub mod lex;

pub use lex::lex_document;
pub use token::{Channel, Token, TokenKind};

/// Import FSH files into the AST and serialize to the oracle's JSON shape
/// (array of FSHDocument, `__type` tags, Map->{"__map"}, bigint->{"__bigint"},
/// id getter->`_id`). The contract gated by `tests/ast_parity.rs`.
///
/// IMPLEMENTATION PENDING — port FSH.g4 + the FSHImporter visitor + a dumper.
pub fn import_to_json(_files: &[(&str, &str)]) -> serde_json::Value {
    todo!("port FSH.g4 parser + FSHImporter -> fsh_model AST -> oracle-shape JSON dumper")
}
