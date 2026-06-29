//! FSH lexer + parser -> typed AST with source spans. Phase 2.
//!
//! The lexer reproduces `sushi-ts/antlr/src/main/antlr/FSHLexer.g4` exactly
//! (mode stack, STAR-folds-newline, longest-match) so its token stream is
//! byte-identical to the ANTLR oracle. Verified by `tests/lex_parity.rs`.

pub mod token;
pub mod lex;

pub use lex::lex_document;
pub use token::{Channel, Token, TokenKind};
