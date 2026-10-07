#[cfg(feature = "structure-inference")]
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
#[cfg(feature = "structure-inference")]
use std::sync::OnceLock;

#[cfg(feature = "structure-inference")]
mod analysis;
#[cfg(feature = "citator")]
mod citator;
mod definitions;
use legal_structure_model::document;
mod docx_lint;
mod docx_numbering;
use legal_structure_model::fingerprint;
#[cfg(feature = "footnote-pairing")]
mod footnote_pairing;
mod instrument;
#[cfg(feature = "structure-inference")]
mod instrument_contents;
#[cfg(feature = "structure-inference")]
mod instrument_references;
#[cfg(feature = "journal")]
mod journal;
use legal_structure_model::locator;
#[cfg(feature = "native-markup")]
mod native_markup;
use legal_structure_model::numeric_sequence;
mod reading_order;
#[cfg(feature = "provider-text")]
mod provider_text;
#[cfg(feature = "quote-verification")]
mod quote_verification;
mod tables;
use legal_structure_model::text;
#[cfg(feature = "structure-inference")]
pub use analysis::{EngineAnalysis, STRUCTURE_ANALYSIS};
#[cfg(feature = "citator")]
pub use citator::*;
pub use definitions::*;
pub(crate) use document::node_depths;
pub use document::{
    CitedAuthority, Derivation, DiagnosticSeverity, DocumentStructure, NodeKind, Note, NoteKindV2,
    NoteReference, StructureDiagnostic, StructureNode,
};
#[cfg(feature = "document-query")]
pub use legal_document_query::*;
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
pub use native_markup::{analyze_native_markup, legisquebec_statute_text, NativeMarkupInput};
pub use numeric_sequence::*;
pub use reading_order::{document_reading_order, ReadingOrderUnit};
#[cfg(feature = "provider-text")]
pub use provider_text::{
    provider_text_document_structure, ProviderSectionMap, ProviderTextInput, ProviderTextSourceKind,
};
#[cfg(feature = "quote-verification")]
pub use quote_verification::*;
pub use legal_structure_model::AuthoritativeTableCell;
pub(crate) use tables::AuthoritativeTables;
pub(crate) use text::javascript_whitespace;
pub use text::{
    last_scalars, normalize_decimal_digit, normalize_javascript_whitespace, normalize_note_symbol,
    trim_javascript_whitespace, utf16_len, utf16_prefix_ceil, ScalarText, JS_WHITESPACE_CLASS,
};

pub use legal_structure_model::{
    CandidateEvidenceV2, CandidateGrammar, CandidateObservationV2, DetectionProfile, EngineError,
    NoteBodyV2, NotePairClaimV2, Origin, ResolutionProofV2, ResolutionRuleV2, ScalarRange, Scope,
    ScopeKind, StructureAnalysis, StructureCandidateRun, StructureMarkerCandidate, TextAnchorV2,
    DOCUMENT_STRUCTURE_SCHEMA,
};
pub(crate) use legal_structure_model::EvidenceKind;
pub const ENGINE_SOURCE_SHA256: &str = env!("LEGAL_STRUCTURE_ENGINE_SHA256");
const ENGINE_ORIGIN: &str = "legalpdf.structure-engine";

#[cfg(feature = "structure-inference")]
use legal_citations::cues::canadian_report_start;

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

#[derive(Clone, Copy, Eq, PartialEq)]
enum CoverageState {
    Absent,
    Augment,
    Complete,
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

impl DocumentInput {
    fn new(
        document_id: String,
        provider: &str,
        profile: DetectionProfile,
        text: String,
        origin_id: &str,
    ) -> Self {
        let text_sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
        Self::with_sha256(document_id, provider, profile, text, text_sha256, origin_id)
    }

    /// `new` for a text whose digest the caller already holds.
    fn with_sha256(
        document_id: String,
        provider: &str,
        profile: DetectionProfile,
        text: String,
        text_sha256: String,
        origin_id: &str,
    ) -> Self {
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
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
mod outline;
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
mod printed_statute;
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
pub use outline::{document_outline, OutlineEntry};
#[cfg(all(feature = "structure-inference", feature = "document-query"))]
pub use printed_statute::{PrintedLine, PrintedStatute, ProvisionPlacement};
#[cfg(feature = "provider-text")]
mod statute_outline;
#[cfg(feature = "provider-text")]
pub use statute_outline::{statute_outline, StatuteOutlineEntry};
#[cfg(feature = "provider-text")]
mod case_outline;
#[cfg(feature = "provider-text")]
pub use case_outline::{case_outline, CaseOutlineEntry};

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
