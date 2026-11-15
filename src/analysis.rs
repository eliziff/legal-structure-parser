//! The citation grammar's services on laid-out text: the citations a line holds, each text read once for them
//! (`CitedText`), the spans they protect, and its cues.

use legal_citations::cues;
use legal_structure_model::text::ScalarText;
use legal_structure_model::CitationSpan;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub use cues::{is_citation_shaped_tail, layout_has_citation_cue as has_citation_cue, layout_has_citation_signal as has_citation_signal};

/// The spans of a laid-out line a citation protects, in UTF-16 offsets.
pub fn protected_citation_spans(text: &str) -> Vec<(usize, usize)> {
    let document = legal_citations::text::ScalarText::new(text);
    cues::layout_protected_spans(text)
        .into_iter()
        .map(|span| (document.utf16_of_byte(span.start), document.utf16_of_byte(span.end)))
        .collect()
}

/// Whether `text` may hold a citation: two runs of figures (a year and a number, a volume and a page, an instrument's
/// year and number, a chapter's statute and number), or a year alone (a statute by its title, "City of Toronto Act,
/// 2006"). A text with neither ("Issue 2", "Section 7", a numbered paragraph's own figure) is no citation's and is not
/// read by the finder, which is most of a decision's reading.
pub fn may_cite(text: impl AsRef<[u8]>) -> bool {
    let mut runs = text.as_ref().split(|byte| !byte.is_ascii_digit()).filter(|run| !run.is_empty());
    match (runs.next(), runs.next()) {
        (Some(_), Some(_)) => true,
        (Some(run), None) => run.len() == 4,
        _ => false,
    }
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

/// A text read for its structure, with the citations of every text its readings ask about (`citation_spans`), each
/// read once for the document and its parts however many readings ask.
pub struct CitedText<'a> {
    text: ScalarText<'a>,
    read: Arc<Mutex<HashMap<Box<str>, Vec<CitationSpan>>>>,
}

impl<'a> CitedText<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text: ScalarText::new(text), read: Arc::default() }
    }

    /// `value`, a part of the text, as a text of its own sharing what was read.
    pub fn part<'b>(&'b self, value: &'b str) -> CitedText<'b> {
        CitedText { text: ScalarText::new(value), read: Arc::clone(&self.read) }
    }

    /// `citation_spans(part)`, read the first time `part` is asked about.
    pub fn spans(&self, part: &str) -> Vec<CitationSpan> {
        let mut read = self.read.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(spans) = read.get(part) {
            return spans.clone();
        }
        read.entry(part.into()).or_insert_with(|| citation_spans(part)).clone()
    }
}

impl<'a> std::ops::Deref for CitedText<'a> {
    type Target = ScalarText<'a>;

    fn deref(&self) -> &ScalarText<'a> {
        &self.text
    }
}
