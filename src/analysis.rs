//! This engine's detection and the citation grammar, supplied to parsers that are built
//! on the model crate alone and so are not recompiled when the engine changes.

use crate::{
    AuthoritativeTableCell, CandidateEvidenceV2, CandidateRoleV2, DocumentStructure, EngineError,
    StructureCandidateRun, };
use legal_citations::cues;
use legal_structure_model::layout::{Page, PairingOutput, PdfSourceSpan};
use legal_structure_model::{CitationSpan, LayoutAnalysis, StructureAnalysis};
use std::collections::BTreeMap;

pub struct EngineAnalysis;

/// The structure engine, for a program to hand to the parsers it links.
pub static STRUCTURE_ANALYSIS: EngineAnalysis = EngineAnalysis;

impl LayoutAnalysis for EngineAnalysis {
    fn source_sha256(&self) -> &'static str {
        crate::ENGINE_SOURCE_SHA256
    }

    fn numbering_marker(
        &self,
        marker: &str,
        previous: Option<&legal_structure_model::NumberingMarker>,
    ) -> Option<legal_structure_model::NumberingMarker> {
        crate::candidates::numbering_marker(marker, previous)
    }

    fn citations(&self, text: &str) -> Vec<CitationSpan> {
        citation_spans(text)
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
        crate::layout_cues::heading_text_plausible(text)
    }

    fn has_citation_cue(&self, text: &str) -> bool {
        cues::layout_has_citation_cue(text)
    }

    fn has_citation_signal(&self, text: &str) -> bool {
        cues::layout_has_citation_signal(text)
    }

    fn is_citation_shaped_tail(&self, text: &str) -> bool {
        cues::is_citation_shaped_tail(text)
    }

    fn crossref_short_form(&self, text: &str, byte_start: usize) -> String {
        cues::crossref_short_form(text, byte_start)
    }

    fn pair_footnotes(
        &self,
        pages: &[Page],
        protected_citation_spans: &BTreeMap<String, Vec<PdfSourceSpan>>,
        include_audit: bool,
    ) -> PairingOutput {
        crate::note_pairing::pair_footnotes(pages, protected_citation_spans, include_audit)
    }

    fn repair_label_dropouts(&self, pages: &mut [Page]) {
        crate::numbering_repair::repair_label_dropouts(pages);
        crate::numbering_repair::repair_paragraph_numbers(pages);
    }

    fn printed_paragraph_number(&self, text: &str) -> Option<usize> {
        crate::numbered_paragraphs::paragraph_number(text)
    }

    fn editorial_line(&self, text: &str) -> bool {
        crate::numbered_paragraphs::editorial_line(text)
    }

    fn speaker_labels(&self, text: &str) -> Vec<(usize, usize)> {
        crate::transcript::speaker_labels(text)
    }
}

impl StructureAnalysis for EngineAnalysis {
    fn signature_line(&self, line: &str) -> bool {
        crate::judgment::signature_line(line)
    }

    fn author_line(&self, line: &str) -> bool {
        crate::judgment::author_line(line)
    }

    fn end_matter(&self, line: &str) -> bool {
        crate::judgment::end_matter(line)
    }

    fn date_line(&self, line: &str) -> bool {
        crate::judgment::date_line(line)
    }

    fn disposition_line(&self, line: &str) -> bool {
        crate::judgment::disposition_line(line)
    }

    fn history_note(&self, line: &str) -> bool {
        crate::statute::history_note(line)
    }

    fn marginal_note_shaped(&self, line: &str) -> bool {
        crate::statute::marginal_note_shaped(line)
    }

    fn parse_ordinal(&self, kind: &str, raw: &str) -> Option<usize> {
        crate::parse_ordinal(kind, raw)
    }

    fn numbered_section_locator(&self, compact: &str) -> String {
        crate::normalize_compact_numbered_section_locator(compact)
    }

    fn detect_structure_candidate_runs(&self, text: &str, refinements: bool) -> Vec<StructureCandidateRun> {
        crate::detect_structure_candidate_runs(text, refinements)
    }

    fn resolve_candidates(
        &self,
        runs: &[StructureCandidateRun],
        evidence: &[CandidateEvidenceV2],
    ) -> Result<Vec<CandidateRoleV2>, EngineError> {
        crate::candidates::resolve_candidates(runs, evidence)
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

    fn paragraph_printings(&self, document: &DocumentStructure) -> Vec<(usize, Vec<usize>)> {
        crate::numbered_paragraphs::paragraph_printings(document)
    }

    fn judgment_paragraphs(&self, text: &str) -> Vec<(usize, Vec<legal_structure_model::ScalarRange>)> {
        crate::case_outline::judgment_paragraphs(text)
    }
}

/// Every citation in `text`, each with its style of cause, pinpoints and parentheticals: where the
/// citation grammar finds them, without the metadata, short-form resolution and name references
/// extraction adds, which no structure reading uses (measured: the structure gold, the footnote
/// benchmark and the A2AJ census unchanged; 3-5x faster on documents dense with citations).
pub(crate) fn citation_spans(text: &str) -> Vec<CitationSpan> {
    legal_citations::find::find_occurrences(text, &legal_citations::Options::default())
        .into_iter()
        .map(|citation| CitationSpan {
            range: citation.full_span.start..citation.full_span.end,
            core_range: citation.span.start..citation.span.end,
            text: citation.full_span.text,
        })
        .collect()
}
