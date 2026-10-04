//! Queries over a derived legal document: its blocks, addresses, passages and text fragments.
//!
//! Built on the structure model alone, so a detection edit does not recompile it.

mod document_block;
mod document_query;

pub use document_block::{DocumentBlock, DocumentKind, DocumentOrigin};
pub use document_query::*;

use legal_structure_model::document::public_structure_label;
use legal_structure_model::{
    javascript_whitespace, locator, text, utf16_len, AuthoritativeTableCell, Derivation,
    DetectionProfile, DocumentStructure, InstrumentCrossReferenceGraph,
    InstrumentCrossReferenceStatus, NodeKind, ScalarRange, ScalarText, StructureNode,
};
#[cfg(test)]
use legal_structure_model::Scope;
