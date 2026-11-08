use crate::{
    AuthoritativeTableCell, CandidateEvidenceV2, CandidateRoleV2, DocumentStructure, EngineError,
    StructureCandidateRun, };
use legal_layout::LayoutAnalysis;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The pairer's audit of a document's markers, where asked for: each marker's reading and the pairing's summary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PdfPairingAudit {
    pub markers: Vec<Value>,
    pub pairing_summary: Value,
}

/// The origin of the notes the pairer writes.
pub const NOTE_PAIRING_ORIGIN: &str = "legal-structure.note-pairing";

/// A laid-out document's notes as the pairer writes them: each a Footnote (or Endnote) node placed in the reader's text,
/// its body its ranges, its label its marker range, its calls; the diagnostics of what it could not pair or place; and,
/// where asked for, its audit of the markers.
pub struct NotePairing {
    pub notes: Vec<crate::StructureNode>,
    pub diagnostics: Vec<legal_layout::Diagnostic>,
    pub audit: Option<PdfPairingAudit>,
}

/// The structure engine's document reading, as parsers that build documents on the model call it,
/// beside its services on laid-out text. A parser takes this as a dependency on the model alone, so
/// editing the engine recompiles the engine and the program that supplies it, not every parser.
pub trait StructureAnalysis: LayoutAnalysis {
    /// A laid-out document's notes paired with their references, its lines' citations (by line id, in scalar offsets)
    /// protected from being read as either, each note placed in the reader's text by `place` (a line's id and a range
    /// of its characters).
    fn pair_footnotes(
        &self,
        pages: &[legal_layout::Page],
        protected_citation_spans: &std::collections::BTreeMap<String, Vec<legal_layout::ScalarRange>>,
        include_audit: bool,
        place: &dyn Fn(&str, usize, usize) -> Option<crate::ScalarRange>,
    ) -> NotePairing;
    /// The numbering runs in `text`: its counts of numbers, letters and bullets, and the provision
    /// ladder the instrument grammar reads. With `refinements`, the ladder also reads a section's
    /// number its subsections repeat ("33(2) The …"), and leaves out enactment history notes,
    /// regnal heads, contents columns and years standing alone.
    fn detect_structure_candidate_runs(&self, text: &str, refinements: bool) -> Vec<StructureCandidateRun>;

    /// Each candidate of `runs` resolved, in text order, to the unit it opens, if any: its evidence
    /// says whether it numbers anything, its run's numbering family what.
    fn resolve_candidates(
        &self,
        runs: &[StructureCandidateRun],
        evidence: &[CandidateEvidenceV2],
    ) -> Result<Vec<CandidateRoleV2>, EngineError>;

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

    /// Each paragraph number `document` labels, once, with the indexes of the nodes printing it (a parallel
    /// report's columns), the first in the text first: the paragraph every consumer's pinpoint addresses.
    fn paragraph_printings(&self, document: &DocumentStructure) -> Vec<(usize, Vec<usize>)>;

    /// A judge's signature or a release line.
    fn signature_line(&self, line: &str) -> bool;

    /// The line naming who gives a judge's reasons.
    fn author_line(&self, line: &str) -> bool;

    /// A line opening what stands after a judge's reasons.
    fn end_matter(&self, line: &str) -> bool;

    /// A date alone on its line, as a decision is dated under its reasons.
    fn date_line(&self, line: &str) -> bool;

    /// A report's line recording what became of the proceeding.
    fn disposition_line(&self, line: &str) -> bool;

    /// A judgment's numbered paragraphs as its text reads them, the reading of a provider's text of a decision (a
    /// block to a line): each printing of each number, with the spans of `text` its paragraph holds — its words, its
    /// quotations and lists — up to the next paragraph, the headings between them no paragraph's.
    fn judgment_paragraphs(&self, text: &str) -> Vec<(usize, Vec<crate::ScalarRange>)>;

    /// A statute's history note under a provision.
    fn history_note(&self, line: &str) -> bool;

    /// Whether a line reads as a statute's marginal note.
    fn marginal_note_shaped(&self, line: &str) -> bool;

    /// A numeric printed paragraph, page or transcript line locator (`kind`) read in its canonical spelling grammar.
    fn parse_ordinal(&self, kind: &str, raw: &str) -> Option<usize>;

    /// A numbered section locator's canonical spelling, whitespace already out ("" where it is none).
    fn numbered_section_locator(&self, compact: &str) -> String;
}

static STRUCTURE_ANALYSIS: std::sync::OnceLock<&'static dyn StructureAnalysis> = std::sync::OnceLock::new();

/// Supplies the structure engine's document reading to the parsers built on the model. The first
/// one supplied stays.
pub fn install_structure_analysis(analysis: &'static dyn StructureAnalysis) {
    let _ = STRUCTURE_ANALYSIS.set(analysis);
}

/// The supplied structure engine.
pub fn structure_analysis() -> &'static dyn StructureAnalysis {
    *STRUCTURE_ANALYSIS
        .get()
        .expect("the program must supply the structure engine with install_structure_analysis")
}
