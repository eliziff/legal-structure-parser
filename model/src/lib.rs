//! The document-structure model: the types a structure result is made of, the text
//! coordinates they are measured in, and the seam through which parsers that build on
//! the model reach the structure engine without being compiled against it.
//!
//! This crate changes when the model changes, not when detection does, so the crates
//! that only read or build structures are not recompiled for every detection edit.

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

/// The SHA-256 of a value's JSON, in hex, written straight into the digest: the key every cache of the stack uses.
pub fn serialization_sha256(value: &impl Serialize) -> String {
    json_sha256(value).expect("a value of the model serializes")
}

fn json_sha256(value: &impl Serialize) -> serde_json::Result<String> {
    use sha2::{Digest, Sha256};
    struct Hashing(Sha256);
    impl std::io::Write for Hashing {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.update(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut digest = Hashing(Sha256::new());
    serde_json::to_writer(&mut digest, value)?;
    Ok(digest.0.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

mod analysis;
mod assembly;
mod authority;
pub mod definitions;
pub mod document;
pub mod docx;
pub mod instrument;
pub mod opinion;
pub mod propositions;
/// Laid-out pages and lines and note pairs, which the document model builds on.
pub use legal_layout as layout;
pub use legal_layout::{ScalarRange};
pub mod text;

pub use analysis::{install_structure_analysis, structure_analysis, NotePairing, StructureAnalysis, NOTE_PAIRING_ORIGIN};
pub use assembly::{assemble_in_place, write_levels};
pub use authority::{AuthoritySource, AuthorityTextUnit, SourceMapping};
pub use legal_layout::{ablated, CitationSpan, LayoutAnalysis, NumberingMarker, PARTY_ROLES};
pub use definitions::{DefinedTerm, DefinitionOccurrence};
pub use document::{
    provision_mark, CitedAuthority, Derivation, DocumentStructure, MarkupAncestor, NodeReading,
    ContinuationKind, SourceContinuation, SourceJoin, ManuscriptEdits, MarkupElement, NodeKind, PrimaryRole, SourceFragment, StructureNode,
};
pub use docx::{
    DocxAttachmentReference, DocxAttachmentReferenceStatus, DocxCrossReference,
    DocxCrossReferenceStatus, DocxNumberAnchor, DocxNumberingDuplicate, DocxNumberingGap,
    DocxNumberingResult, DocxStructureFacts,
};
pub use instrument::{
    InstrumentContentsEntry, InstrumentContentsOutline, InstrumentContentsReading,
    InstrumentContentsRefusal, InstrumentCrossReferenceCounts, InstrumentCrossReferenceEdge,
    InstrumentCrossReferenceGraph, InstrumentCrossReferenceReason, InstrumentCrossReferenceStatus,
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


#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Complete,
    Excerpt,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub kind: ScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
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
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub id: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub enum CandidateGrammar {
    Numeric,
    Hierarchy,
    Enumerator,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct StructureMarkerCandidate {
    pub id: String,
    pub range: ScalarRange,
    pub marker_range: ScalarRange,
    pub label: String,
    pub grammar_value: String,
    /// A provision's kind as its grammar read its marker ("section", "article", "part", ...); none for a clause read by
    /// its number alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub locator_kind: Option<String>,
    pub parent_candidate_id: Option<String>,
    pub level: usize,
    pub content_start: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct StructureCandidateRun {
    pub id: String,
    pub grammar: CandidateGrammar,
    pub range: ScalarRange,
    pub rooted: bool,
    pub consecutive: bool,
    pub markers: Vec<StructureMarkerCandidate>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct CandidateEvidenceV2 {
    pub candidate_id: String,
    pub page_indexes: Vec<usize>,
    pub line_ids: Vec<String>,
    pub observations: Vec<CandidateObservationV2>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CandidateObservationV2 {
    BodyProseFlow,
    SectionHeading,
    ListItemLayout,
    /// An explicit source list-item role covers the marker's complete source lines.
    SourceListItem,
    /// A lead-in and clause syntax connect aligned sibling markers into a list.
    ListItemSyntax,
    CrossReference,
    Furniture,
    TableOrForm,
    ContentsRow,
    IndexRow,
    TranscriptLineNumber,
    /// The marker sits in a passage the document quotes: its numbering is the quoted text's.
    Quotation,
    /// The witnesses of the marker's line, composed over its page and document, read it as a list's
    /// item.
    ComposedListItem,
    /// The witnesses of the marker's line, composed over its page and document, read it as a
    /// numbered paragraph.
    ComposedParagraph,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ResolutionRuleV2 {
    RootedNumericProse,
    HierarchySection,
    ListItemLayout,
    ListItemSyntax,
    PairedNote,
    DirectExclusion,
    ConflictingRoles,
    InsufficientEvidence,
    /// A numbered paragraph off the document's count (`paragraph_spine`): a list's or a quotation's.
    OffCount,
}

/// The unit a numbering marker opens: one of the text's numbered paragraphs, a section or clause,
/// or a list's item.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum UnitRole {
    Paragraph,
    Section,
    Item,
}

/// The locator a provision's marker makes it, at its depth (`level`) on its ladder: the kind its grammar read
/// (`locator_kind`), else a clause at the top and a subclause under it.
pub fn provision_locator(kind: Option<&str>, level: usize) -> &str {
    kind.unwrap_or(if level == 0 { "clause" } else { "subclause" })
}

impl UnitRole {
    /// The kind a numbering's family fixes, read on a marker's label, as the structure gold's
    /// numbering rule does: numbers, decimals ("1.01") and bare capitals ("A." recitals) are the
    /// document's own numbered paragraphs; letters, roman numerals, bullets and a bracketed capital
    /// are a list's items.
    pub fn of_family(label: &str) -> Self {
        let token = label.trim().trim_start_matches('*');
        let bare = token.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | '.' | ':' | '-'));
        let bracketed = token.starts_with(['(', '[']);
        let digits = bare.starts_with(|c: char| c.is_ascii_digit()) && bare.chars().all(|c| c.is_ascii_digit() || c == '.');
        let capital = !bracketed && bare.chars().count() == 1 && bare.chars().all(|c| c.is_ascii_uppercase());
        if digits || capital {
            Self::Paragraph
        } else {
            Self::Item
        }
    }
}

/// A numbering candidate's role: the unit it opens, or none, and the rule that decided it; and
/// whether its number counts in its text (the next of its count after the last of that count read
/// before it, or that last one).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct CandidateRoleV2 {
    pub candidate_id: String,
    pub role: Option<UnitRole>,
    pub rule: ResolutionRuleV2,
    #[serde(default)]
    pub counted: bool,
}

/// An authoritative table cell a document's text was rendered from, in UTF-16 offsets.
#[derive(Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AuthoritativeTableCell {
    pub table: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub table_name: Option<String>,
    pub row: usize,
    pub column: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub row_span: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub column_span: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub display_value: Option<String>,
    pub ranges: Vec<ScalarRange>,
    pub start: usize,
    pub end: usize,
}
