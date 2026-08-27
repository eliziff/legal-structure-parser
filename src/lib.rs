#[cfg(feature = "structure-inference")]
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
#[cfg(feature = "structure-inference")]
use std::sync::OnceLock;

#[cfg(feature = "a2aj")]
mod a2aj;
#[cfg(feature = "citator")]
mod citator;
mod definitions;
mod document;
#[cfg(feature = "document-query")]
mod document_block;
#[cfg(feature = "document-query")]
mod document_query;
mod docx_lint;
mod docx_numbering;
mod fingerprint;
#[cfg(feature = "footnote-pairing")]
mod footnote_pairing;
mod instrument;
#[cfg(feature = "structure-inference")]
mod instrument_contents;
#[cfg(feature = "structure-inference")]
mod instrument_references;
#[cfg(feature = "journal")]
mod journal;
mod locator;
#[cfg(feature = "native-markup")]
mod native_markup;
mod numeric_sequence;
#[cfg(feature = "quote-verification")]
mod quote_verification;
mod tables;
mod text;
#[cfg(feature = "a2aj")]
pub use a2aj::{a2aj_document_structure, A2ajInput, A2ajSectionMap, A2ajSourceKind};
#[cfg(feature = "citator")]
pub use citator::*;
pub use definitions::*;
pub(crate) use document::{node_depths, public_structure_label};
pub use document::{
    CitedAuthority, Derivation, DiagnosticSeverity, DocumentStructure, NodeKind, Note, NoteKindV2,
    NoteReference, StructureDiagnostic, StructureNode,
};
#[cfg(feature = "document-query")]
pub use document_block::{DocumentBlock, DocumentKind, DocumentOrigin};
#[cfg(feature = "document-query")]
pub use document_query::*;
pub use docx_lint::*;
pub use docx_numbering::*;
pub use fingerprint::*;
#[cfg(feature = "footnote-pairing")]
pub use footnote_pairing::{pair_numbered_footnotes, NumberedFootnotePairing, PairedFootnote};
pub use instrument::*;
#[cfg(feature = "journal")]
pub use journal::{journal_document_structure, journal_text_document_structure, JournalPageLabel};
pub use locator::{normalize_compact_numbered_section_locator, normalize_section_locator};
#[cfg(feature = "native-markup")]
pub use native_markup::{analyze_native_markup, NativeMarkupInput};
pub use numeric_sequence::*;
#[cfg(feature = "quote-verification")]
pub use quote_verification::*;
pub use tables::AuthoritativeTableCell;
pub(crate) use tables::AuthoritativeTables;
pub(crate) use text::javascript_whitespace;
pub use text::{
    last_scalars, normalize_decimal_digit, normalize_javascript_whitespace, normalize_note_symbol,
    trim_javascript_whitespace, utf16_len, utf16_prefix_ceil, ScalarText, JS_WHITESPACE_CLASS,
};

pub const DOCUMENT_STRUCTURE_SCHEMA: &str = "legalpdf.document-structure.v1";
pub const ENGINE_SOURCE_SHA256: &str = env!("LEGAL_STRUCTURE_ENGINE_SHA256");
const ENGINE_ORIGIN: &str = "legalpdf.structure-engine";

#[cfg(feature = "structure-inference")]
fn canadian_report_start(value: &str) -> Option<u32> {
    static REPORT: OnceLock<Regex> = OnceLock::new();
    REPORT
        .get_or_init(|| Regex::new(r"(?iu)\b(?:S\.?C\.?R\.?|R\.?C\.?S\.?)\s+(\d{1,4})\b").unwrap())
        .captures(value)
        .and_then(|capture| capture[1].parse().ok())
}

fn whole_document_coverage(
    end: usize,
    state: impl Fn(EvidenceKind) -> CoverageState,
) -> Vec<Coverage> {
    [
        EvidenceKind::Paragraph,
        EvidenceKind::Prose,
        EvidenceKind::Page,
        EvidenceKind::Section,
        EvidenceKind::Heading,
        EvidenceKind::Footnote,
        EvidenceKind::Endnote,
    ]
    .into_iter()
    .map(|kind| Coverage {
        kind,
        range: ScalarRange { start: 0, end },
        state: state(kind),
    })
    .collect()
}

#[derive(Debug)]
pub struct EngineError {
    pub code: &'static str,
    pub message: String,
}

impl EngineError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_evidence",
            message: message.into(),
        }
    }

    fn source(message: impl Display) -> Self {
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
    fn valid(self, length: usize) -> bool {
        self.start <= self.end && self.end <= length
    }
}

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum EvidenceKind {
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum CoverageState {
    Absent,
    Augment,
    Complete,
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
    pub(crate) fn complete() -> Self {
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

struct NativeClaim {
    id: String,
    kind: EvidenceKind,
    label: Option<String>,
    aliases: Vec<String>,
    range: ScalarRange,
    origin_id: &'static str,
    parent_label: Option<String>,
    anchor: Option<String>,
}

struct Coverage {
    kind: EvidenceKind,
    range: ScalarRange,
    state: CoverageState,
}

struct Exclusion {
    range: ScalarRange,
    applies_to: Vec<String>,
}

pub(crate) struct DocumentInput {
    document_id: String,
    provider: String,
    url: Option<String>,
    doc_type: Option<&'static str>,
    profile: DetectionProfile,
    report_start_page: Option<u32>,
    require_report_start: bool,
    allow_hyphenated_sections: bool,
    text: String,
    text_sha256: String,
    source_sha256: Option<String>,
    scope: Scope,
    origins: Vec<Origin>,
    native_claims: Vec<NativeClaim>,
    coverage: Vec<Coverage>,
    exclusions: Vec<Exclusion>,
}

impl EvidenceKind {
    fn name(self) -> &'static str {
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

impl DocumentInput {
    fn new(
        document_id: String,
        provider: &str,
        profile: DetectionProfile,
        text: String,
        origin_id: &str,
    ) -> Self {
        let text_sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
        Self {
            document_id,
            provider: provider.to_owned(),
            url: None,
            doc_type: None,
            profile,
            report_start_page: None,
            require_report_start: false,
            allow_hyphenated_sections: false,
            text,
            text_sha256,
            source_sha256: None,
            scope: Scope::complete(),
            origins: vec![Origin {
                id: origin_id.to_owned(),
            }],
            native_claims: Vec::new(),
            coverage: Vec::new(),
            exclusions: Vec::new(),
        }
    }

    fn clip_inference(&self, kind: EvidenceKind, range: ScalarRange) -> Option<ScalarRange> {
        let mut end = range.end;
        for value in self
            .coverage
            .iter()
            .filter(|value| value.kind == kind && value.state == CoverageState::Complete)
        {
            if value.range.start <= range.start && range.start < value.range.end {
                return None;
            }
            if value.range.start > range.start {
                end = end.min(value.range.start);
            }
        }
        for value in self
            .exclusions
            .iter()
            .filter(|value| value.applies_to.iter().any(|name| name == kind.name()))
        {
            if value.range.start <= range.start && range.start < value.range.end {
                return None;
            }
            if value.range.start > range.start {
                end = end.min(value.range.start);
            }
        }
        (end > range.start).then_some(ScalarRange {
            start: range.start,
            end,
        })
    }

    fn needs_inference(&self) -> bool {
        self.coverage
            .iter()
            .any(|value| value.state != CoverageState::Complete)
    }
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CandidateGrammar {
    Numeric,
    Hierarchy,
    Enumerator,
}

#[cfg(feature = "structure-inference")]
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

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructureCandidateRun {
    pub id: String,
    pub grammar: CandidateGrammar,
    pub range: ScalarRange,
    pub rooted: bool,
    pub consecutive: bool,
    pub markers: Vec<StructureMarkerCandidate>,
}

#[cfg(feature = "structure-inference")]
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
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextAnchorV2 {
    pub range: ScalarRange,
    pub page_index: usize,
    pub line_id: String,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoteBodyV2 {
    pub range: ScalarRange,
    pub page_indexes: Vec<usize>,
    pub line_ids: Vec<String>,
}

#[cfg(feature = "structure-inference")]
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

#[cfg(feature = "structure-inference")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedRole {
    NumberedParagraph,
    Section,
    ListItem,
}

#[cfg(feature = "structure-inference")]
impl ResolvedRole {
    pub(crate) fn node_kind(self) -> NodeKind {
        match self {
            Self::NumberedParagraph => NodeKind::Paragraph,
            Self::Section => NodeKind::Section,
            Self::ListItem => NodeKind::ListItem,
        }
    }
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedCandidate<'a> {
    pub(crate) candidate: &'a StructureMarkerCandidate,
    pub(crate) role: Option<ResolvedRole>,
    pub(crate) proof: ResolutionProofV2,
    pub(crate) page_indexes: &'a [usize],
    pub(crate) line_ids: &'a [String],
}

#[derive(Clone)]
struct Block {
    kind: NodeKind,
    range: ScalarRange,
    label: Option<String>,
    aliases: Vec<String>,
    parent_label: Option<String>,
    content_start: Option<usize>,
    diagnostic: Option<&'static str>,
    source: Derivation,
    origin_id: &'static str,
}

impl Block {
    fn labelled(kind: NodeKind, label: String, start: usize, end: usize) -> Self {
        Self {
            kind,
            range: ScalarRange { start, end },
            label: Some(label),
            aliases: Vec::new(),
            parent_label: None,
            content_start: None,
            diagnostic: None,
            source: Derivation::Heuristic,
            origin_id: ENGINE_ORIGIN,
        }
    }
}

#[cfg(feature = "structure-inference")]
mod inference;

#[cfg(feature = "structure-inference")]
mod candidates;
mod derive;

#[cfg(all(feature = "structure-inference", test))]
pub(crate) use candidates::resolve_structure_candidates;
#[cfg(feature = "structure-inference")]
pub use candidates::{detect_structure_candidate_runs, resolve_structure_graph};
#[cfg(any(feature = "journal", test))]
pub(crate) use derive::derive_native_structure_evidence;
#[cfg(all(feature = "structure-inference", test))]
pub(crate) use derive::derive_structure_evidence;
#[cfg(test)]
mod tests;
