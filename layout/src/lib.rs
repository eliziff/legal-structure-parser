//! Laid-out pages of lines, as a PDF's extraction or a provider's flattened text gives them, the
//! note pairs read from them, and the structure engine's services on them. It depends on nothing
//! of the document model, so a parser that reads only lines recompiles for neither.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScalarRange {
    pub start: usize,
    pub end: usize,
}

impl ScalarRange {
    /// Sort and merge touching or overlapping members without filling gaps.
    /// All ranges must use the same coordinate plane and offset unit.
    pub fn normalize_union(ranges: &mut Vec<Self>) {
        ranges.sort_by_key(|range| (range.start, range.end));
        let mut result: Vec<Self> = Vec::with_capacity(ranges.len());
        for range in ranges.drain(..) {
            if let Some(last) = result.last_mut().filter(|last| range.start <= last.end) {
                last.end = last.end.max(range.end);
            } else {
                result.push(range);
            }
        }
        *ranges = result;
    }

    #[inline]
    pub fn valid(self, length: usize) -> bool {
        self.start <= self.end && self.end <= length
    }
}

/// Evidence from an occurrence-level assignment within one numbering restart.
/// Counts concern distinct labels, not repeated references to the same note.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct NotePairSupportV2 {
    pub sequence_id: usize,
    pub sequence_labels: usize,
    pub sequence_observed_pairs: usize,
    pub sequence_note_labels: usize,
    pub observed_pair: bool,
    pub typographic_reference: bool,
    pub note_region: bool,
}

impl NotePairSupportV2 {
    pub fn corroborated_sequence(&self) -> bool {
        self.sequence_observed_pairs >= 3
            && 3 * self.sequence_observed_pairs >= self.sequence_labels
    }

    pub fn supports_label_only(&self) -> bool {
        self.note_region
            && (self.sequence_observed_pairs >= 2
                && self.sequence_labels <= 3 * self.sequence_observed_pairs
                || self.sequence_note_labels >= 3
                    && 4 * self.sequence_note_labels >= 3 * self.sequence_labels)
    }

    /// Plain numbers alone are not positive evidence for a note.
    pub fn supports_note(&self) -> bool {
        (self.observed_pair && (self.typographic_reference || self.note_region))
            || self.supports_label_only()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub id: String,
    pub text: String,
    pub bbox: [f64; 4],
    pub start: usize,
    pub end: usize,
    /// A rule is drawn under the word: painted in the PDF, or dark on a recognized page's image.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underlined: bool,
}

/// Span flag: the text is drawn invisibly, as a scan's recognition layer is.
pub const INVISIBLE: u32 = 32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcid: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure_role: Option<String>,
    pub id: String,
    pub text: String,
    pub bbox: [f64; 4],
    #[serde(default)]
    pub font: String,
    #[serde(default)]
    pub size: f64,
    /// PyMuPDF order: superscript 1, italic 2, serif 4, monospaced 8, bold 16; and
    /// [`INVISIBLE`] for text drawn invisibly (render mode 3), a recognition layer behind a scan.
    #[serde(default)]
    pub flags: u32,
    #[serde(default)]
    pub superscript: bool,
    #[serde(default)]
    pub start: usize,
    #[serde(default)]
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetachedReference {
    pub note_id: String,
    pub selected_text: String,
    pub start_offset: usize,
    pub end_offset: usize,
    pub source_line_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrMarkerEvidence {
    /// A physical association only; this does not classify a paragraph or note.
    pub body_line_id: Option<String>,
    pub sequence_line_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrEvidence {
    pub confidence: f64,
    pub recovered: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baseline: Vec<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boundary: Vec<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<OcrMarkerEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub id: String,
    pub page_index: usize,
    pub page_number: u32,
    pub source_index: usize,
    pub reading_order: usize,
    pub block_index: usize,
    pub text: String,
    pub bbox: [f64; 4],
    #[serde(default)]
    pub spans: Vec<Span>,
    #[serde(default)]
    pub words: Vec<Word>,
    #[serde(default)]
    pub detached_references: Vec<DetachedReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr: Option<OcrEvidence>,
    #[serde(default)]
    pub exclude_from_body: bool,
    #[serde(default)]
    pub suppress_footnote_label: bool,
    /// A competing paragraph-layout witness; pairing may overturn it.
    #[serde(default)]
    pub paragraph_label_candidate: bool,
    #[serde(default)]
    pub note_region_mode: String,
    #[serde(default)]
    pub region_id: String,
    #[serde(default = "unknown_region")]
    pub region_type: String,
    #[serde(default = "native_source")]
    pub source: String,
}

fn unknown_region() -> String {
    "unknown".to_owned()
}

fn native_source() -> String {
    "native".to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub id: String,
    pub page_index: usize,
    #[serde(rename = "type")]
    pub kind: String,
    pub line_ids: Vec<String>,
    pub bbox: [f64; 4],
    pub reading_order: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: String,
    pub index: usize,
    pub number: u32,
    pub width: f64,
    pub height: f64,
    pub lines: Vec<Line>,
    pub regions: Vec<Region>,
    #[serde(default = "native_source")]
    pub source: String,
    #[serde(default = "one")]
    pub text_quality: f64,
    #[serde(default)]
    pub printed_label: Option<String>,
    #[serde(default)]
    pub printed_label_source: Option<String>,
    #[serde(default)]
    pub printed_label_line_id: Option<String>,
}

const fn one() -> f64 {
    1.0
}

/// An unchanged UTF-16 run in produced text, anchored to an original physical line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceTextSpan {
    pub start: usize,
    pub end: usize,
    pub line_id: String,
    pub source_start: usize,
    pub source_end: usize,
}

impl SourceTextSpan {
    /// Retain an exact input slice at a new output position. Insertions have no run.
    pub fn retain(&self, start: usize, end: usize, output_start: usize) -> Option<Self> {
        let left = self.start.max(start);
        let right = self.end.min(end);
        (left < right).then(|| Self {
            start: output_start + left - start,
            end: output_start + right - start,
            line_id: self.line_id.clone(),
            source_start: self.source_start + left - self.start,
            source_end: self.source_start + right - self.start,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FootnoteCrossref {
    pub source_pair_id: String,
    pub kind: String,
    pub number: u32,
    pub shortform: String,
    pub start: usize,
    pub end: usize,
    pub resolved: bool,
    pub target_pair_id: String,
    pub target_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Footnote {
    #[serde(default)]
    pub source_spans: Vec<SourceTextSpan>,
    pub pair_id: String,
    pub label: String,
    pub occurrence: usize,
    pub restart_sequence: usize,
    pub reference_page: Option<u32>,
    pub body_pages: Vec<u32>,
    pub reference_line_id: Option<String>,
    pub body_line_ids: Vec<String>,
    pub body: String,
    pub sentence_proposition: String,
    pub passage_since_prior_note: String,
    pub confidence: f64,
    pub provenance: String,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub crossrefs: Vec<FootnoteCrossref>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
    #[serde(default)]
    pub page_index: Option<usize>,
    #[serde(default)]
    pub line_ids: Vec<String>,
    #[serde(default)]
    pub details: Map<String, Value>,
}

impl Diagnostic {
    pub fn warning(code: &str, message: impl Into<String>, page_index: Option<usize>) -> Self {
        Self {
            code: code.to_owned(),
            severity: "warning".to_owned(),
            message: message.into(),
            page_index,
            line_ids: Vec::new(),
            details: Map::new(),
        }
    }

    pub fn info(code: &str, message: impl Into<String>, page_index: Option<usize>) -> Self {
        Self {
            code: code.to_owned(),
            severity: "info".to_owned(),
            message: message.into(),
            page_index,
            line_ids: Vec::new(),
            details: Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfSourceSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PdfPairingAudit {
    pub markers: Vec<Value>,
    pub pairing_summary: Value,
}

#[derive(Debug, Clone)]
pub struct Anchor {
    pub pair_id: String,
    pub label: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotePairKind {
    Footnote,
    Endnote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAnchor {
    pub line_id: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotePairClaim {
    pub pair_id: String,
    pub support: NotePairSupportV2,
    pub label: String,
    pub kind: NotePairKind,
    pub label_anchor: SourceAnchor,
    pub reference_anchors: Vec<SourceAnchor>,
    pub body_line_ids: Vec<String>,
}

pub struct PairingOutput {
    pub footnotes: Vec<Footnote>,
    pub diagnostics: Vec<Diagnostic>,
    pub anchors: HashMap<String, Vec<Anchor>>,
    pub pair_claims: Vec<NotePairClaim>,
    pub pairing_audit: Option<PdfPairingAudit>,
}

pub fn line_font_size(line: &Line) -> f64 {
    const INLINE_SPANS: usize = 32;
    let exclude_superscripts = line
        .spans
        .iter()
        .any(|span| span.size > 0.0 && !span.superscript);
    let eligible = || {
        line.spans
            .iter()
            .filter(|span| span.size > 0.0 && (!exclude_superscripts || !span.superscript))
    };
    let count = eligible().count();
    let mut inline = [(0.0, 0_usize); INLINE_SPANS];
    let mut heap;
    let weighted = if count <= INLINE_SPANS {
        for (slot, span) in inline.iter_mut().zip(eligible()) {
            *slot = (span.size, span.text.chars().count().clamp(1, 100));
        }
        &mut inline[..count]
    } else {
        heap = eligible()
            .map(|span| (span.size, span.text.chars().count().clamp(1, 100)))
            .collect::<Vec<_>>();
        &mut heap
    };
    weighted.sort_by(|left, right| left.0.total_cmp(&right.0));
    let target = weighted.iter().map(|(_, count)| count).sum::<usize>() / 2;
    let mut seen = 0;
    weighted
        .iter()
        .find_map(|(size, count)| {
            seen += *count;
            (seen > target).then_some(*size)
        })
        .unwrap_or(0.0)
}

pub fn union_bbox(boxes: impl IntoIterator<Item = [f64; 4]>) -> [f64; 4] {
    let mut values = boxes.into_iter();
    let Some(first) = values.next() else {
        return [0.0; 4];
    };
    values.fold(first, |mut result, value| {
        result[0] = result[0].min(value[0]);
        result[1] = result[1].min(value[1]);
        result[2] = result[2].max(value[2]);
        result[3] = result[3].max(value[3]);
        result
    })
}

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
