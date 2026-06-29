//! FSH model: interned symbols plus typed FSH entities, rules, and source
//! metadata. The AST/entity types are filled in during Phase 2 (parser).

pub mod ast;
pub mod intern;

pub use ast::*;
pub use intern::{Interner, Symbol};

/// The kinds of top-level FSH entities SUSHI imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Profile,
    Extension,
    Logical,
    Resource,
    Instance,
    ValueSet,
    CodeSystem,
    Invariant,
    RuleSet,
    Mapping,
}
