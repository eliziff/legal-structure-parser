//! Laid-out pages of lines, as a PDF's extraction or a provider's flattened text gives them, the
//! note pairs read from them, and the structure engine's services on them. It depends on nothing
//! of the document model, so a parser that reads only lines recompiles for neither.

use serde::{Deserialize, Serialize};
use std::ops::Range;

mod analysis;
mod measures;
mod stats;
pub use analysis::*;
pub use measures::*;
pub use stats::*;

/// Temporary ablation: whether LEGALPDF_ABLATE (comma-separated pass names) leaves `pass` out.
pub fn ablated(pass: &str) -> bool {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| std::env::var("LEGALPDF_ABLATE").unwrap_or_default().split(',').map(str::to_owned).collect())
        .iter()
        .any(|name| name == pass)
}

/// A party's role as a filing, a cover or a judgment prints it, in English or French ("Applicant",
/// "Respondents", "Amicus Curiae", "Intimée"), as a regular expression's alternatives: the one
/// vocabulary every reader of party labels, covers and front matter builds its pattern from.
pub const PARTY_ROLES: &str = concat!(
    r"applicants?|respondents?|appellants?|appellees?|plaintiffs?|defendants?|petitioners?|claimants?|complainants?",
    r"|prosecutors?|accused|interven(?:er|or)s?|grievors?|employers?|amic(?:us|i)\s+curiae",
    r"|(?:moving|responding|notice|third|interested|intervening|affected)\s+(?:part(?:y|ies)|persons?)",
    r"|fonctionnaires?\s+s.estimant\s+lésée?s?|employeurs?|plaignante?s?|intimée?s?|demandeurs?|demanderesses?",
    r"|défendeurs?|défenderesses?|appelante?s?|requérante?s?",
);

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
    /// A rule is drawn through the word, as a blackline strikes deleted text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub struck: bool,
    /// The word's stroke weight in the page's 300 dpi render, as its line's `weight` is measured;
    /// 0 where not measured. A bold word in a line of plain words (a run-in title) shows here.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub weight: f64,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// The dark share of the line's box in the page's 300 dpi render, where the page was rendered
    /// (a scan whose text layer is kept); 0 where not measured. Weight evidence for a text layer
    /// whose fonts carry no flags.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ink: f64,
    /// The median horizontal run of dark pixels through the line's box in that render, in units of
    /// the box's height: the stems' thickness, which bold type sets wider; 0 where not measured.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub weight: f64,
    /// The height of the line's capitals and ascenders over its baseline in that render, in points:
    /// its type's size where a recognized page names none (a box's height swings with descenders
    /// and capitals); 0 where not measured.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cap_height: f64,
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

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

fn unknown_region() -> String {
    "unknown".to_owned()
}

fn native_source() -> String {
    "native".to_owned()
}

impl Line {
    /// Whether recognition read the line, our own or a scan's invisible layer: the only readings that lose or misread
    /// glyphs. Printed text is exact.
    pub fn recognized(&self) -> bool {
        self.source == "ocr" || self.spans.iter().any(|span| span.flags & INVISIBLE != 0)
    }
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

/// A fault a reading found that leaves the document's reading short of ready, by its code, and the page it is on: a
/// page no text could be read from (OCR_REQUIRED), a column order still uncertain, a note band read from weak geometry,
/// a note's label or reference left unpaired.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    #[serde(default)]
    pub page_index: Option<usize>,
}

impl Diagnostic {
    pub fn new(code: &str, page_index: Option<usize>) -> Self {
        Self { code: code.to_owned(), page_index }
    }
}

/// Capitals stand about seven tenths of the type size in common text faces (Times 0.66, Arial
/// 0.72), so a measured capital height reads as a type size.
pub const CAP_HEIGHT_SHARE: f64 = 0.7;

/// A line's type size: on a scanned page, from the capital height measured on its image (its box
/// grows with descenders and its text layer's sizes are guesses), to the half point, as near as a
/// pixel of a 300 dpi render measures it; otherwise its spans' median size.
pub fn line_font_size(line: &Line) -> f64 {
    const INLINE_SPANS: usize = 32;
    if line.cap_height > 0.0 {
        return (2.0 * line.cap_height / CAP_HEIGHT_SHARE).round() / 2.0;
    }
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
