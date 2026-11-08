//! The services the reading stages call on the structure engine: apart from the page types, so a change to them
//! does not make every extraction stale (legal-pdf-parser/build.rs hashes lib.rs for extraction).

use super::*;

/// The structure engine's services on laid-out text, as parsers that read pages and lines call
/// them: the citation grammar's cues, the numbering grammar, and note pairing. A parser takes this
/// as a dependency on this crate alone, so editing the engine or its document model recompiles
/// neither it nor anything that reads only lines.
pub trait LayoutAnalysis: Sync {
    /// The digest of the engine's source, which keys caches of its results.
    fn source_sha256(&self) -> &'static str;

    fn numbering_marker(&self, marker: &str, previous: Option<&NumberingMarker>) -> Option<NumberingMarker>;

    /// Every citation in `text`, each with its style of cause, pinpoints and parentheticals.
    fn citations(&self, text: &str) -> Vec<CitationSpan>;
    /// The spans of a laid-out line a citation protects, in scalar offsets.
    fn protected_citation_spans(&self, text: &str) -> Vec<(usize, usize)>;
    /// Whether a short laid-out line reads as a heading's title, by the citation grammar.
    fn heading_text_plausible(&self, text: &str) -> bool;
    fn has_citation_cue(&self, text: &str) -> bool;
    fn has_citation_signal(&self, text: &str) -> bool;
    fn is_citation_shaped_tail(&self, text: &str) -> bool;
    fn crossref_short_form(&self, text: &str, byte_start: usize) -> String;

    /// A laid-out document's note labels paired with their references, its lines' citations
    /// (by line id, in scalar offsets) protected from being read as either.
    fn pair_footnotes(
        &self,
        pages: &[Page],
        protected_citation_spans: &BTreeMap<String, Vec<PdfSourceSpan>>,
        include_audit: bool,
    ) -> PairingOutput;
    /// Restores the labels recognition misread from the runs they stand in: note labels whose
    /// leading digits it dropped, from the note chain, and paragraph and item numbers.
    fn repair_label_dropouts(&self, pages: &mut [Page]);

    /// A body line's bracketed paragraph number ("[12] ..."); a bracketed year ("[2008] 2 S.C.R. 3") numbers no
    /// paragraph.
    fn printed_paragraph_number(&self, text: &str) -> Option<usize>;
    /// A quotation's editorial line, wholly in square brackets or an ellipsis alone ("[Emphasis added]").
    fn editorial_line(&self, text: &str) -> bool;

    /// Every speaker label opening a transcript's line, as byte ranges of `text`: the first, and a name's after a "Q."
    /// or "A." ("Q.   MR. PHILLIPS:").
    fn speaker_labels(&self, text: &str) -> Vec<(usize, usize)>;
    /// The speaker label opening a transcript's line, if any.
    fn speaker_label(&self, text: &str) -> Option<(usize, usize)> {
        self.speaker_labels(text).into_iter().next()
    }
}
