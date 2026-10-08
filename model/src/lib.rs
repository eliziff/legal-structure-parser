//! The document-structure model: the types a structure result is made of, the text
//! coordinates they are measured in, and the seam through which parsers that build on
//! the model reach the structure engine without being compiled against it.
//!
//! This crate changes when the model changes, not when detection does, so the crates
//! that only read or build structures are not recompiled for every detection edit.

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

mod analysis;
pub mod definitions;
pub mod docx;
pub mod document;
pub mod fingerprint;
pub mod instrument;
pub mod locator;
pub mod numeric_sequence;
pub mod text;

pub use analysis::{CitationSpan, StructureAnalysis};
pub use definitions::{DefinedTerm, DefinitionOccurrence, DefinitionsResult};
pub use docx::{
    DocxAttachmentReference, DocxAttachmentReferenceStatus, DocxCrossReference,
    DocxCrossReferenceStatus, DocxNumberAnchor, DocxNumberingDuplicate, DocxNumberingGap,
    DocxNumberingResult, DocxStructureFacts,
};
pub use document::{
    CitedAuthority, Derivation, DiagnosticSeverity, DocumentStructure, NodeKind, Note, NoteKindV2,
    NoteReference, StructureDiagnostic, StructureNode,
};
pub use fingerprint::{document_fingerprint, DocumentFingerprint, DocumentFingerprintCounts};
pub use instrument::{
    InstrumentContentsEntry, InstrumentContentsOutline, InstrumentContentsReading,
    InstrumentContentsRefusal, InstrumentCrossReferenceCounts, InstrumentCrossReferenceEdge,
    InstrumentCrossReferenceGraph, InstrumentCrossReferenceReason, InstrumentCrossReferenceStatus,
};
pub use locator::{normalize_compact_numbered_section_locator, normalize_section_locator};
pub use numeric_sequence::{
    select_numeric_sequence, NumericSequenceCandidate, NumericSequencePolicy,
    NumericSequenceSelection,
};
pub use text::{
    javascript_whitespace, last_scalars, normalize_decimal_digit, normalize_javascript_whitespace,
    normalize_note_symbol, trim_javascript_whitespace, utf16_len, utf16_prefix_ceil, ScalarText,
    JS_WHITESPACE_CLASS,
};

pub const DOCUMENT_STRUCTURE_SCHEMA: &str = "legalpdf.document-structure.v1";

#[derive(Debug)]
pub struct EngineError {
    pub code: &'static str,
    pub message: String,
}

impl EngineError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_evidence",
            message: message.into(),
        }
    }

    pub fn source(message: impl Display) -> Self {
        Self {
            code: "invalid_source",
            message: message.to_string(),
        }
    }
}

impl Display for EngineError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EngineError {}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScalarRange {
    pub start: usize,
    pub end: usize,
}

impl ScalarRange {
    #[inline]
    pub fn valid(self, length: usize) -> bool {
        self.start <= self.end && self.end <= length
    }
}

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EvidenceKind {
    Paragraph,
    Prose,
    Page,
    Section,
    Heading,
    Footnote,
    Endnote,
    List,
    Table,
    Row,
    Cell,
}

impl EvidenceKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Prose => "prose",
            Self::Page => "page",
            Self::Section => "section",
            Self::Heading => "heading",
            Self::Footnote => "footnote",
            Self::Endnote => "endnote",
            Self::List => "list",
            Self::Table => "table",
            Self::Row => "row",
            Self::Cell => "cell",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Complete,
    Excerpt,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionProfile {
    CaseRootedComplete,
    CaseContiguousComplete,
    CaseLossy,
    Legislation,
    Instrument,
    Journal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub kind: ScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt_of: Option<String>,
}

impl Scope {
    pub fn complete() -> Self {
        Self {
            kind: ScopeKind::Complete,
            excerpt_of: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub id: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CandidateGrammar {
    Numeric,
    Hierarchy,
    Enumerator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructureMarkerCandidate {
    pub id: String,
    pub range: ScalarRange,
    pub marker_range: ScalarRange,
    pub label: String,
    pub grammar_value: String,
    pub parent_candidate_id: Option<String>,
    pub level: usize,
    pub content_start: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructureCandidateRun {
    pub id: String,
    pub grammar: CandidateGrammar,
    pub range: ScalarRange,
    pub rooted: bool,
    pub consecutive: bool,
    pub markers: Vec<StructureMarkerCandidate>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateEvidenceV2 {
    pub candidate_id: String,
    pub page_indexes: Vec<usize>,
    pub line_ids: Vec<String>,
    pub observations: Vec<CandidateObservationV2>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateObservationV2 {
    BodyProseFlow,
    SectionHeading,
    ListItemLayout,
    CrossReference,
    Furniture,
    TableOrForm,
    ContentsRow,
    IndexRow,
    TranscriptLineNumber,
    /// The marker sits in a passage the document quotes: its numbering is the quoted text's.
    Quotation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextAnchorV2 {
    pub range: ScalarRange,
    pub page_index: usize,
    pub line_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteBodyV2 {
    pub range: ScalarRange,
    pub page_indexes: Vec<usize>,
    pub line_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NotePairClaimV2 {
    pub pair_id: String,
    pub kind: NoteKindV2,
    pub label: TextAnchorV2,
    pub body: NoteBodyV2,
    pub references: Vec<TextAnchorV2>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionRuleV2 {
    RootedNumericProse,
    HierarchySection,
    ListItemLayout,
    PairedNote,
    DirectExclusion,
    ConflictingRoles,
    InsufficientEvidence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolutionProofV2 {
    pub rule: ResolutionRuleV2,
    pub observations: Vec<CandidateObservationV2>,
}

/// An authoritative table cell a document's text was rendered from, in UTF-16 offsets.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthoritativeTableCell {
    pub table: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    pub row: usize,
    pub column: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_span: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_span: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_value: Option<String>,
    pub start: usize,
    pub end: usize,
}
