//! This engine's detection and the citation grammar, supplied to parsers that are built
//! on the model crate alone and so are not recompiled when the engine changes.

use crate::{
    AuthoritativeTableCell, CandidateEvidenceV2, DocumentStructure, EngineError, NotePairClaimV2,
    StructureCandidateRun, StructureDiagnostic, StructureNode,
};
use legal_citations::cues;
use legal_structure_model::{CitationSpan, StructureAnalysis};

pub struct EngineAnalysis;

/// The structure engine, for a program to hand to the parsers it links.
pub static STRUCTURE_ANALYSIS: EngineAnalysis = EngineAnalysis;

impl StructureAnalysis for EngineAnalysis {
    fn source_sha256(&self) -> &'static str {
        crate::ENGINE_SOURCE_SHA256
    }

    fn detect_structure_candidate_runs(&self, text: &str) -> Vec<StructureCandidateRun> {
        crate::detect_structure_candidate_runs(text)
    }

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
    ) -> Result<DocumentStructure, EngineError> {
        crate::resolve_structure_graph(
            document_id,
            provider,
            text,
            source_sha256,
            nodes,
            runs,
            evidence,
            note_pairs,
            diagnostics,
        )
    }

    fn analyze_instrument(
        &self,
        text: String,
        document_id: String,
        table_cells: &[AuthoritativeTableCell],
        reconstruct_lineation: bool,
    ) -> Result<DocumentStructure, EngineError> {
        crate::analyze_instrument(text, document_id, table_cells, reconstruct_lineation)
    }

    fn analyze_docx(
        &self,
        document_id: String,
        paragraphs: Vec<String>,
        table_cells: &[AuthoritativeTableCell],
    ) -> Result<DocumentStructure, EngineError> {
        crate::analyze_docx(document_id, paragraphs, table_cells)
    }

    fn citations(&self, text: &str) -> Vec<CitationSpan> {
        legal_citations::extract(text, &legal_citations::Options::default())
            .into_iter()
            .map(|citation| CitationSpan {
                range: citation.full_span.start..citation.full_span.end,
                text: citation.full_span.text,
            })
            .collect()
    }

    fn protected_citation_spans(&self, text: &str) -> Vec<(usize, usize)> {
        let document = legal_citations::text::ScalarText::new(text);
        cues::layout_protected_spans(text)
            .into_iter()
            .map(|span| {
                (
                    document.scalar_at_byte(span.start).expect("citation start"),
                    document.scalar_at_byte(span.end).expect("citation end"),
                )
            })
            .collect()
    }

    fn heading_text_plausible(&self, text: &str) -> bool {
        cues::layout_heading_text_plausible(text)
    }

    fn has_citation_cue(&self, text: &str) -> bool {
        cues::layout_has_citation_cue(text)
    }

    fn has_citation_signal(&self, text: &str) -> bool {
        cues::layout_has_citation_signal(text)
    }

    fn is_citation_continuation(&self, text: &str) -> bool {
        cues::layout_is_citation_continuation(text)
    }

    fn is_citation_shaped_tail(&self, text: &str) -> bool {
        cues::is_citation_shaped_tail(text)
    }

    fn is_counter_noun(&self, word: &str) -> bool {
        cues::is_counter_noun(word)
    }

    fn crossref_short_form(&self, text: &str, byte_start: usize) -> String {
        cues::crossref_short_form(text, byte_start)
    }

    fn reporter_abbreviation_regex(&self, abbreviation: &str) -> String {
        cues::reporter_abbreviation_regex(abbreviation)
    }
}
