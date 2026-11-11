//! The citation grammar's services on laid-out text, and this engine as the layout crate's `LayoutAnalysis`, which
//! extraction reads through legal-pdf-core so that an engine edit leaves extraction's key alone.

use legal_citations::cues;
use legal_structure_model::layout::{Line, LineZone};
use legal_structure_model::{CitationSpan, LayoutAnalysis, NumberingMarker};

pub use cues::{is_citation_shaped_tail, layout_has_citation_cue as has_citation_cue, layout_has_citation_signal as has_citation_signal};

pub struct EngineAnalysis;

/// The structure engine, for a program to hand to legal-pdf-core's extraction.
pub static STRUCTURE_ANALYSIS: EngineAnalysis = EngineAnalysis;

impl LayoutAnalysis for EngineAnalysis {
    fn source_sha256(&self) -> &'static str {
        crate::ENGINE_SOURCE_SHA256
    }

    fn numbering_marker(&self, marker: &str, previous: Option<&NumberingMarker>) -> Option<NumberingMarker> {
        crate::candidates::numbering_marker(marker, previous)
    }

    fn citations(&self, text: &str) -> Vec<CitationSpan> {
        citation_spans(text)
    }

    fn protected_citation_spans(&self, text: &str) -> Vec<(usize, usize)> {
        protected_citation_spans(text)
    }

    fn heading_text_plausible(&self, text: &str) -> bool {
        crate::layout_cues::heading_text_plausible(text)
    }

    fn has_citation_cue(&self, text: &str) -> bool {
        has_citation_cue(text)
    }

    fn has_citation_signal(&self, text: &str) -> bool {
        has_citation_signal(text)
    }

    fn is_citation_shaped_tail(&self, text: &str) -> bool {
        is_citation_shaped_tail(text)
    }

    fn repair_label_dropouts(&self, lines: &mut [(&mut Line, LineZone)]) {
        crate::numbering_repair::repair_numbering(lines);
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

/// The spans of a laid-out line a citation protects, in UTF-16 offsets.
pub fn protected_citation_spans(text: &str) -> Vec<(usize, usize)> {
    let document = legal_citations::text::ScalarText::new(text);
    cues::layout_protected_spans(text)
        .into_iter()
        .map(|span| (document.utf16_of_byte(span.start), document.utf16_of_byte(span.end)))
        .collect()
}

/// Every citation in `text`, each with its style of cause, pinpoints and parentheticals: where the
/// citation grammar finds them, without the metadata, short-form resolution and name references
/// extraction adds, which no structure reading uses (measured: the structure gold, the footnote
/// benchmark and the A2AJ census unchanged; 3-5x faster on documents dense with citations).
pub fn citation_spans(text: &str) -> Vec<CitationSpan> {
    legal_citations::find::find_occurrences(text, &legal_citations::Options::default())
        .into_iter()
        .map(|citation| CitationSpan {
            range: citation.full_span.start..citation.full_span.end,
            core_range: citation.span.start..citation.span.end,
            text: citation.full_span.text,
        })
        .collect()
}
