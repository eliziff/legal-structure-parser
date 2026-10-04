use crate::{
    AuthoritativeTableCell, CandidateEvidenceV2, DocumentStructure, EngineError, NotePairClaimV2,
    StructureCandidateRun, StructureDiagnostic, StructureNode,
};
use std::ops::Range;

/// A citation the citation grammar found, by byte range and the text it spans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CitationSpan {
    pub range: Range<usize>,
    pub text: String,
}

/// The structure engine's detection and the citation grammar, as parsers that build on the
/// model call them. A parser takes this as a dependency on the model alone, so editing the
/// engine recompiles the engine and the program that supplies it, not every parser.
pub trait StructureAnalysis: Sync {
    /// The digest of the engine's source, which keys caches of its results.
    fn source_sha256(&self) -> &'static str;

    fn detect_structure_candidate_runs(&self, text: &str) -> Vec<StructureCandidateRun>;

    #[allow(clippy::too_many_arguments)]
    fn resolve_structure_graph(
        &self,
        document_id: String,
        provider: String,
        text: &str,
        source_sha256: Option<String>,
        nodes: Vec<StructureNode>,
        runs: &[StructureCandidateRun],
        evidence: &[CandidateEvidenceV2],
        note_pairs: &[NotePairClaimV2],
        diagnostics: Vec<StructureDiagnostic>,
    ) -> Result<DocumentStructure, EngineError>;

    fn analyze_instrument(
        &self,
        text: String,
        document_id: String,
        table_cells: &[AuthoritativeTableCell],
        reconstruct_lineation: bool,
    ) -> Result<DocumentStructure, EngineError>;

    fn analyze_docx(
        &self,
        document_id: String,
        paragraphs: Vec<String>,
        table_cells: &[AuthoritativeTableCell],
    ) -> Result<DocumentStructure, EngineError>;

    /// Every citation in `text`, each with its style of cause, pinpoints and parentheticals.
    fn citations(&self, text: &str) -> Vec<CitationSpan>;
    /// The spans of a laid-out line a citation protects, in scalar offsets.
    fn protected_citation_spans(&self, text: &str) -> Vec<(usize, usize)>;
    fn heading_text_plausible(&self, text: &str) -> bool;
    fn has_citation_cue(&self, text: &str) -> bool;
    fn has_citation_signal(&self, text: &str) -> bool;
    fn is_citation_continuation(&self, text: &str) -> bool;
    fn is_citation_shaped_tail(&self, text: &str) -> bool;
    fn is_counter_noun(&self, word: &str) -> bool;
    fn crossref_short_form(&self, text: &str, byte_start: usize) -> String;
    fn reporter_abbreviation_regex(&self, abbreviation: &str) -> String;
}
