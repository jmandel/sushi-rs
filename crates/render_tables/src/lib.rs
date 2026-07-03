//! `render_tables` — C2: a byte-exact Rust port of fhir-core's
//! `HierarchicalTableGenerator` (org.hl7.fhir.utilities.xhtml), the xhtml table
//! builder that every StructureDefinition element table (C1) bottoms out in.
//!
//! Source of truth (READ-ONLY): fhir-core 6.9.10-SNAPSHOT (the renderer version
//! that produced the golden corpus), file
//! `.../utilities/xhtml/HierarchicalTableGenerator.java` (1510 LOC).
//!
//! Output is a `render_xhtml::XhtmlNode` tree; callers compose it with
//! `render_xhtml`'s HTML-non-pretty composer to match the publisher's
//! `new XhtmlComposer(XhtmlComposer.HTML)` fragment serialization.
//!
//! The one deliberate structural choice: attributes are emitted in Java
//! `HashMap` iteration order (see `hashorder`), because fhir-core's XhtmlNode
//! stores attributes in a HashMap and the composer iterates keySet(). The
//! `build::Elem` builder buffers attributes and flushes them in that order.

pub mod build;
pub mod generate;
pub mod hashorder;
pub mod model;

pub use generate::{
    generate, init_grid_table, init_normal_table, path_url, phrase, Gen,
};
pub use model::{
    Cell, Counter, Piece, Row, TableGenerationMode, TableModel, TextAlignment, Title,
    BACKGROUND_ALT_COLOR, CONTINUE_REGULAR, CONTINUE_SLICE, CONTINUE_SLICER, NEW_REGULAR,
    NEW_SLICE, NEW_SLICER,
};
