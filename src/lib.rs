#[cfg(feature = "structure-inference")]
use regex::Regex;
use std::collections::{HashMap, HashSet};
#[cfg(feature = "structure-inference")]
use std::sync::OnceLock;

#[cfg(feature = "structure-inference")]
mod analysis;
#[cfg(feature = "structure-inference")]
pub mod hierarchy;
#[cfg(feature = "structure-inference")]
mod numbered_paragraphs;
#[cfg(feature = "structure-inference")]
pub use numbered_paragraphs::paragraph_printings;
#[cfg(feature = "structure-inference")]
mod layout_cues;
#[cfg(feature = "structure-inference")]
mod transcript;
#[cfg(feature = "structure-inference")]
mod numbering_repair;
#[cfg(feature = "structure-inference")]
pub mod note_pairing;
#[cfg(feature = "structure-inference")]
mod text_notes;
#[cfg(feature = "citator")]
mod citator;
#[cfg(all(feature = "citator", feature = "document-query"))]
mod text_units;
#[cfg(all(feature = "citator", feature = "canlii"))]
mod canlii;
mod filing;
pub use filing::{filing_cover, FiledBy, FilingCover, PartyGroup};
use legal_structure_model::document;
mod docx_lint;
mod docx_numbering;
mod instrument;
#[cfg(feature = "structure-inference")]
mod instrument_contents;
#[cfg(feature = "structure-inference")]
mod instrument_references;
#[cfg(feature = "journal")]
mod journal;
use legal_locator as locator;
#[cfg(feature = "native-markup")]
mod native_markup;
/// A CourtListener opinion record read whole: its pages by reporter, paragraphs, notes, headnotes and front matter over
/// the engine's reading of each rendition, as the structure the product serves (`opinion_structure`).
#[cfg(feature = "opinion")]
pub mod opinion;
pub mod judgment;
mod numeric_sequence;
#[cfg(feature = "structure-inference")]
mod provider_text;
mod tables;
#[cfg(feature = "structure-inference")]
pub use analysis::{EngineAnalysis, STRUCTURE_ANALYSIS};
#[cfg(feature = "citator")]
pub use citator::*;
#[cfg(all(feature = "citator", feature = "document-query"))]
pub use text_units::text_units;
#[cfg(all(feature = "citator", feature = "canlii"))]
pub use canlii::{canlii_case_url, canlii_court_route, canlii_law_url};
pub use document::{
    provision_mark, CitedAuthority, Derivation, DocumentStructure, MarkupAncestor,
    ContinuationKind, SourceContinuation, SourceJoin, ManuscriptEdits, MarkupElement, NodeKind, PrimaryRole, SourceFragment, StructureNode,
};
pub use docx_lint::*;
pub use docx_numbering::*;
#[cfg(feature = "structure-inference")]
pub use text_notes::{pair_numbered_footnotes, NumberedFootnotes};
pub use instrument::*;
#[cfg(feature = "journal")]
pub use journal::{journal_document_structure, journal_text_document_structure, JournalPageLabel};
#[cfg(feature = "document-query")]
pub use legal_document_query::*;
use legal_structure_model::text;
pub use legal_structure_model::AuthoritativeTableCell;
/// The laid-out pages and lines note pairing reads, for programs that hand it lines of their own.
pub use legal_structure_model::layout;
pub use locator::{collapse_labels, expand_labels, normalize_compact_numbered_section_locator, normalize_section_locator, parse_ordinal};
pub mod statute;
#[cfg(feature = "native-markup")]
pub use native_markup::{analyze_native_markup, NativeMarkupInput};
pub use numeric_sequence::*;
#[cfg(feature = "structure-inference")]
pub use provider_text::{
    provider_text_document_structure, provider_text_outline_reading, windows_1252_punctuation, ProviderPage, ProviderSectionMap, ProviderTextInput,
    ProviderTextSourceKind,
};
pub(crate) use tables::AuthoritativeTables;
pub use text::javascript_whitespace;
pub use text::{
    equal_fold, last_scalars, normalize_decimal_digit, normalize_javascript_whitespace, normalize_note_symbol,
    trim_javascript_whitespace, utf16_len, utf16_prefix_ceil, ScalarText, JS_WHITESPACE_CLASS,
};

pub use legal_structure_model::{
    CandidateEvidenceV2, CandidateGrammar, CandidateObservationV2, CandidateRoleV2, DetectionProfile, EngineError,
    Origin, ResolutionRuleV2, ScalarRange, Scope,
    LayoutAnalysis, ScopeKind, StructureAnalysis, StructureCandidateRun, StructureMarkerCandidate,
    UnitRole, DOCUMENT_STRUCTURE_SCHEMA,
};
pub const ENGINE_SOURCE_SHA256: &str = env!("LEGAL_STRUCTURE_ENGINE_SHA256");
const ENGINE_ORIGIN: &str = "legalpdf.structure-engine";

#[cfg(feature = "structure-inference")]
use legal_citations::cues::canadian_report_start;

struct Exclusion {
    range: ScalarRange,
    applies_to: Vec<NodeKind>,
}

/// What a reader hands `derive`: the structure it read, its own nodes, each naming its parent (`parent_id`)
/// by its id or label until `derive` resolves it; the profile inference reads it by; and what it tells inference.
pub(crate) struct Evidence {
    structure: DocumentStructure,
    profile: DetectionProfile,
    report_start_page: Option<u32>,
    require_report_start: bool,
    allow_hyphenated_sections: bool,
    exclusions: Vec<Exclusion>,
}

impl Evidence {
    fn new(structure: DocumentStructure, profile: DetectionProfile) -> Self {
        Self {
            structure,
            profile,
            report_start_page: None,
            require_report_start: false,
            allow_hyphenated_sections: false,
            exclusions: Vec::new(),
        }
    }

    fn clip_inference(&self, kind: NodeKind, range: ScalarRange) -> Option<ScalarRange> {
        let mut end = range.end;
        for value in self
            .exclusions
            .iter()
            .filter(|value| value.applies_to.contains(&kind))
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

}

/// A node inference reads, by its kind, label and span; `derive` numbers it.
fn labelled_node(kind: NodeKind, label: String, start: usize, end: usize) -> StructureNode {
    let mut node = StructureNode::new(
        String::new(),
        kind,
        ScalarRange { start, end },
        ENGINE_ORIGIN,
        Derivation::Heuristic,
        None,
    );
    node.label = Some(label);
    node
}

#[cfg(feature = "structure-inference")]
mod inference;

#[cfg(feature = "structure-inference")]
mod candidates;
mod derive;
#[cfg(feature = "structure-inference")]
mod statute_outline;
#[cfg(feature = "structure-inference")]
pub use statute_outline::statute_provision_spans;
#[cfg(feature = "structure-inference")]
mod case_outline;
#[cfg(feature = "structure-inference")]
pub use case_outline::{judgment_division, judgment_end, reasons_go_on, LIST_NUMBERS};
pub use judgment::{date_line, figure_caption};

#[cfg(feature = "structure-inference")]
pub use candidates::detect_structure_candidate_runs;
