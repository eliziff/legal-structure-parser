use crate::{
    AuthoritativeTableCell, CandidateEvidenceV2, DocumentStructure, EngineError, NotePairClaimV2,
    StructureCandidateRun, StructureDiagnostic, StructureNode,
};
use crate::layout::{Page, PairingOutput, PdfSourceSpan};
use std::collections::BTreeMap;
use std::ops::Range;

/// A citation the citation grammar found, by byte range and the text it spans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CitationSpan {
    pub range: Range<usize>,
    /// The citation itself, excluding introductory signals and surrounding style.
    pub core_range: Range<usize>,
    pub text: String,
}

/// A marker interpreted by the shared numbering grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberingMarker {
    pub family: String,
    pub ordinal: u32,
    pub prefix: Vec<u32>,
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

    /// Resolve source candidates, then invoke the producer's single outline assembler. The
    /// producer refreshes the revision once it completes the structure.
    #[allow(clippy::too_many_arguments)]
    fn resolve_structure_graph_with_assembler(
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
        assembler: &mut dyn FnMut(&mut [StructureNode]),
    ) -> Result<DocumentStructure, EngineError>;

    fn numbering_marker(
        &self,
        marker: &str,
        previous: Option<&NumberingMarker>,
    ) -> Option<NumberingMarker>;

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
    /// Whether a short laid-out line reads as a heading's title, by the citation grammar.
    fn heading_text_plausible(&self, text: &str) -> bool;
    /// [`Self::heading_text_plausible`], or a title set apart in bold naming a case by its parties.
    fn styled_heading_text_plausible(&self, text: &str) -> bool;
    fn has_citation_cue(&self, text: &str) -> bool;
    fn has_citation_signal(&self, text: &str) -> bool;
    fn is_citation_continuation(&self, text: &str) -> bool;
    fn is_citation_shaped_tail(&self, text: &str) -> bool;
    fn is_counter_noun(&self, word: &str) -> bool;
    fn crossref_short_form(&self, text: &str, byte_start: usize) -> String;
    fn reporter_abbreviation_regex(&self, abbreviation: &str) -> String;

    /// A laid-out document's note labels paired with their references, its lines' citations
    /// (by line id, in scalar offsets) protected from being read as either.
    fn pair_footnotes(
        &self,
        pages: &[Page],
        protected_citation_spans: &BTreeMap<String, Vec<PdfSourceSpan>>,
        include_audit: bool,
    ) -> PairingOutput;
    /// Restores note labels whose leading digits recognition dropped, from the note chain.
    fn repair_label_dropouts(&self, pages: &mut [Page]);
}
