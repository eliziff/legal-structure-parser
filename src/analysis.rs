//! The citation grammar's services on laid-out text: the citations a line holds, the spans they protect, and its cues.

use legal_citations::cues;
use legal_structure_model::CitationSpan;

pub use cues::{is_citation_shaped_tail, layout_has_citation_cue as has_citation_cue, layout_has_citation_signal as has_citation_signal};

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
