use legal_citations::cues;
use legal_citations::cues::is_counter_noun as counter_noun;
use legal_structure_model::layout::{
    self, line_font_size, Anchor, DetachedReference, Diagnostic, Footnote, Line, NotePairClaim,
    NotePairKind, Page, PairingOutput, PdfPairingAudit, PdfSourceSpan, SourceAnchor,
};
use legal_structure_model::{
    normalize_decimal_digit, normalize_note_symbol, select_numeric_sequence, NotePairSupportV2,
    NumericSequenceCandidate, NumericSequencePolicy, ScalarText,
};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Deref;
use std::sync::OnceLock;

mod labels;
use labels::*;
mod refs;
use refs::*;
mod backbone;
use backbone::*;
mod assign;
use assign::*;
mod repairs;
use repairs::*;
mod build;
use build::*;
mod markers;
use markers::*;
mod materialize;
use materialize::*;

const MAX_VALUE: u32 = 999;
const ENDNOTE_MIN_LABELS: usize = 8;
const SYMBOL_START_PAGE_LIMIT: u32 = 2;
const MAX_SYMBOL_RUN: usize = 8;
const SYMBOLS: &str = "*∗\u{f02a}†‡§¶#";
const SUPERSCRIPTS: &str = "⁰¹²³⁴⁵⁶⁷⁸⁹";
const QUOTES: &str = "\"'‘’“”«»";
const DASHES: &str = "–—-";
const TERMINAL_PUNCTUATION: &str = ".!?:;”\"'’";
const REF_PUNCTUATION: &str = ".,;:!?)]}";
const REF_RIGHT_PUNCTUATION: &str = ".,;:!?)]}…/¬\u{00ad}·";
// English and French month names, full or abbreviated.
const MONTH: &str = r"(?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?|janv(?:ier)?|f[ée]vr(?:ier)?|mars|avr(?:il)?|mai|juin|juil(?:let)?|ao[ûu]t|octobre|novembre|d[ée]c(?:embre)?|septembre)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Zone {
    Note,
    Body,
    Title,
    Header,
    Number,
    Visual,
}

#[derive(Debug)]
struct PairLine<'a> {
    line: &'a Line,
    idx: usize,
    page: u32,
    page_index: usize,
    order: usize,
    page_width: f64,
    page_height: f64,
    zone: Zone,
    citation_spans: &'a [PdfSourceSpan],
    outline_spans: Vec<(usize, usize)>,
    note_column_fit: bool,
    small_font: bool,
    prose_like: bool,
    region_witness_demoted: bool,
    native_superscript_spans: Vec<(usize, usize)>,
    edge_folio: bool,
    /// The line is the page's furniture, which holds no note's reference.
    furniture: bool,
    note_sequence_restart: bool,
    detached_references: Cow<'a, [DetachedReference]>,
}

impl PairLine<'_> {
    fn height(&self) -> f64 {
        self.bbox[3] - self.bbox[1]
    }

    /// Whether the line stands in its page's note band: a laid-out line (one with a box) its parser
    /// set as a footnote and gave a note mode. The band is final, so a label on it opens a note
    /// whether or not a reference to it was read, and the page's body holds that note's reference.
    fn in_note_band(&self) -> bool {
        self.region_type == "footnote" && !self.note_region_mode.is_empty() && self.height() > 0.0
    }

    fn protected(&self, start: usize, end: usize) -> bool {
        self.citation_spans
            .iter()
            .any(|span| start < span.end && end > span.start)
            || overlaps(&self.outline_spans, start, end)
    }
}

impl Deref for PairLine<'_> {
    type Target = Line;

    fn deref(&self) -> &Self::Target {
        self.line
    }
}

#[derive(Debug, Clone, Default)]
struct CandidateFlags {
    ref_supported: bool,
    paren_ref: bool,
    volume_split: bool,
}

#[derive(Debug, Clone)]
struct Candidate {
    line: usize,
    start: usize,
    end: usize,
    observed: String,
    value: Option<u32>,
    symbol: String,
    form: &'static str,
    score: f64,
    reason: &'static str,
    repaired: bool,
    repair_kind: &'static str,
    requires_visual_cue: bool,
    flags: CandidateFlags,
}

impl Candidate {
    fn pos(&self, lines: &[PairLine<'_>]) -> (usize, usize) {
        (lines[self.line].idx, self.start)
    }

    fn note_id(&self) -> String {
        self.value
            .map_or_else(|| self.symbol.clone(), |value| value.to_string())
    }

    fn zone_is_noteish(&self, lines: &[PairLine<'_>]) -> bool {
        let line = &lines[self.line];
        (line.zone == Zone::Note && !line.region_witness_demoted) || line.note_column_fit
    }
}

#[derive(Debug, Clone)]
struct Pair {
    label: Candidate,
    refs: Vec<Candidate>,
    primary_ref: Option<(usize, usize, usize)>,
    previous_value: Option<u32>,
    next_value: Option<u32>,
    restart_sequence: usize,
    sequence_labels: usize,
    endnote: bool,
    pair_id: String,
    provenance: String,
}

#[derive(Debug, Clone)]
struct LabelToken {
    pre_start: usize,
    start: usize,
    end: usize,
    match_end: usize,
    observed: String,
    value: Option<u32>,
    symbol: String,
    form: &'static str,
    post: String,
}

fn chars(value: &str) -> Vec<char> {
    value.chars().collect()
}

fn char_slice(value: &str, start: usize, end: usize) -> &str {
    if value.is_ascii() {
        return &value[start.min(value.len())..end.min(value.len())];
    }
    let text = ScalarText::new(value);
    text.slice(start.min(text.len())..end.min(text.len()))
        .expect("valid scalar range")
}

fn chars_slice(values: &[char], start: usize, end: usize) -> String {
    values
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn is_symbol(character: char) -> bool {
    SYMBOLS.contains(character)
}

fn is_superscript(character: char) -> bool {
    SUPERSCRIPTS.contains(character)
}

fn normalize_symbol(value: &str) -> String {
    value.chars().map(normalize_note_symbol).collect()
}

fn numeric_value(value: &str) -> Option<u32> {
    let mut number = 0_u32;
    let mut found = false;
    for character in value.chars() {
        number = number
            .checked_mul(10)?
            .checked_add(normalize_decimal_digit(character)?.to_digit(10)?)?;
        found = true;
    }
    (found && (1..=MAX_VALUE).contains(&number)).then_some(number)
}

/// Whether a line is a provider's flattened text, whose notes read as their HTML printed them:
/// a label "[1]" or "Footnote 1", a call run into the word before it ("counsel[1]", "job.Footnote 1").
fn provider_text(line: &Line) -> bool {
    line.source == "provider_text"
}

fn overlaps(spans: &[(usize, usize)], start: usize, end: usize) -> bool {
    spans
        .iter()
        .any(|(span_start, span_end)| start < *span_end && end > *span_start)
}

fn classify_zone(line: &Line) -> Zone {
    // core._pair_markers presents region_type twice (region/coarse) and a
    // derived line_type to the canonical pairer. Reconstruct that adapter
    // contract exactly rather than interpreting note_region_mode here.
    let footnote = line.region_type == "footnote";
    let region = line.region_type.to_lowercase();
    if footnote
        || ["footnote", "endnote", "reference_content", "note"]
            .iter()
            .any(|token| region.contains(token))
    {
        Zone::Note
    } else if ["page_number", "number", "folio"]
        .iter()
        .any(|token| region.contains(token))
    {
        Zone::Number
    } else if ["header", "running"]
        .iter()
        .any(|token| region.contains(token))
    {
        Zone::Header
    } else if [
        "image",
        "figure",
        "chart",
        "graphic",
        "formula",
        "separator",
        "table",
        "photo",
    ]
    .iter()
    .any(|token| region.contains(token))
    {
        Zone::Visual
    } else if [
        "title",
        "heading",
        "byline",
        "abstract",
        "toc",
        "table_of_contents",
    ]
    .iter()
    .any(|token| region.contains(token))
    {
        Zone::Title
    } else {
        // The canonical adapter supplies "paragraph" as the fallback line
        // type, so every remaining source region is body text.
        Zone::Body
    }
}

fn is_edge_folio(line: &Line) -> bool {
    matches!(
        line.region_type.as_str(),
        "header" | "footer" | "page_number" | "folio"
    ) && {
        let text = line
            .text
            .trim()
            .trim_matches(['-', '\u{2013}', '\u{2014}'])
            .trim();
        (1..=4).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit())
    }
}

/// Whether a line is the page's furniture that would otherwise read as body: a running foot or a
/// folio at the foot, a transcript's line label.
fn is_furniture(line: &Line) -> bool {
    matches!(line.region_type.as_str(), "footer" | "line_label")
}

fn prose_like(text: &str) -> bool {
    static LOWER_WORD: OnceLock<Regex> = OnceLock::new();
    let lower_words = LOWER_WORD
        .get_or_init(|| Regex::new(r"[a-z]{2,}").unwrap())
        .find_iter(text)
        .take(2)
        .count();
    (lower_words >= 2 && text.chars().any(|character| ".,;".contains(character)))
        || (text
            .chars()
            .filter(|character| character.is_alphabetic())
            .count()
            >= 8
            && text
                .as_bytes()
                .windows(2)
                .any(|pair| pair[0].is_ascii_lowercase() && pair[1].is_ascii_digit()))
}

fn median(values: Vec<f64>) -> Option<f64> {
    let values = values.into_iter().filter(|value| value.is_finite()).collect::<Vec<_>>();
    (!values.is_empty()).then(|| layout::upper_median(values))
}

fn build_lines<'a>(
    pages: &'a [Page],
    protected_citation_spans: &'a BTreeMap<String, Vec<PdfSourceSpan>>,
) -> Vec<PairLine<'a>> {
    let mut source: Vec<(&Page, &Line)> = pages
        .iter()
        .flat_map(|page| page.lines.iter().map(move |line| (page, line)))
        .collect();
    source.sort_by(|(left_page, left_line), (right_page, right_line)| {
        left_page
            .index
            .cmp(&right_page.index)
            .then_with(|| left_line.reading_order.cmp(&right_line.reading_order))
            .then_with(|| left_line.id.cmp(&right_line.id))
    });
    let mut lines: Vec<PairLine<'_>> = source
        .into_iter()
        .enumerate()
        .filter(|(_, (_, line))| !line.exclude_from_body)
        .enumerate()
        .map(|(idx, (source_order, (page, line)))| {
            let isolated_references = isolated_inline_references(line);
            let detached_references = if isolated_references.is_empty() {
                Cow::Borrowed(line.detached_references.as_slice())
            } else {
                Cow::Owned(
                    line.detached_references
                        .iter()
                        .cloned()
                        .chain(isolated_references)
                        .collect(),
                )
            };
            let text_len = line.text.chars().count();
            PairLine {
                line,
                idx,
                page: page.number,
                page_index: page.index,
                order: source_order + 1,
                page_width: page.width,
                page_height: page.height,
                zone: classify_zone(line),
                citation_spans: protected_citation_spans
                    .get(&line.id)
                    .map_or(&[], Vec::as_slice),
                outline_spans: Vec::new(),
                note_column_fit: false,
                small_font: false,
                prose_like: prose_like(&line.text),
                region_witness_demoted: false,
                native_superscript_spans: line
                    .spans
                    .iter()
                    .filter(|span| {
                        span.superscript && span.start < span.end && span.end <= text_len
                    })
                    .map(|span| (span.start, span.end))
                    .collect(),
                edge_folio: is_edge_folio(line),
                furniture: is_furniture(line),
                note_sequence_restart: false,
                detached_references,
            }
        })
        .collect();
    let mut page_local_one_pages = HashSet::new();
    for line in &mut lines {
        let page_local_one = line.region_type == "footnote"
            && line.note_region_mode == "footnote"
            && legal_structure_model::text::label_prefix(&line.text).is_some_and(|prefix| prefix.label == "1");
        if page_local_one {
            line.note_sequence_restart =
                !page_local_one_pages.is_empty() && !page_local_one_pages.contains(&line.page);
            page_local_one_pages.insert(line.page);
        }
    }
    let body_median = median(
        lines
            .iter()
            .filter(|line| line.zone == Zone::Body && line.height() > 0.0)
            .map(PairLine::height)
            .collect(),
    );
    let note_column = median(
        lines
            .iter()
            .filter(|line| line.zone == Zone::Note && label_token(&line.text, provider_text(line)).is_some())
            .map(|line| line.bbox[0])
            .collect(),
    );
    for line in &mut lines {
        if body_median.is_some_and(|height| line.height() > 0.0 && line.height() <= 0.92 * height) {
            line.small_font = true;
        }
        if note_column.is_some_and(|column| {
            line.page_width > 0.0 && (line.bbox[0] - column).abs() <= 0.025 * line.page_width
        }) {
            line.note_column_fit = true;
        }
    }
    lines
}

pub fn pair_footnotes(
    pages: &[Page],
    protected_citation_spans: &BTreeMap<String, Vec<PdfSourceSpan>>,
    include_audit: bool,
) -> PairingOutput {
    let (pairs, lines, summary) = build_pairs(pages, protected_citation_spans, include_audit);
    materialize(pairs, &lines, summary)
}
