use legal_citations::cues;
use legal_citations::cues::is_counter_noun as counter_noun;
use legal_structure_model::layout::{self, Diagnostic, LineZone, NoteLine, NoteMode, Span};
use legal_structure_model::{Derivation, NodeKind, NotePairing, ScalarRange, StructureNode, NOTE_PAIRING_ORIGIN};
use crate::{select_numeric_sequence, NumericSequenceCandidate, NumericSequencePolicy};
use legal_structure_model::{normalize_decimal_digit, normalize_note_symbol, ScalarText};
use legal_structure_model::text::{is_note_symbol as is_symbol, MAX_SYMBOL_LABEL_LEN as MAX_SYMBOL_RUN};
use regex::Regex;
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
mod materialize;
use materialize::*;

const MAX_VALUE: u32 = 999;
const ENDNOTE_MIN_LABELS: usize = 8;
const SYMBOL_START_PAGE_LIMIT: u32 = 2;
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

struct PairLine<'a> {
    line: &'a NoteLine<'a>,
    /// Its place among the document's lines, and among its part's.
    at: usize,
    idx: usize,
    page: u32,
    page_index: usize,
    order: usize,
    page_width: f64,
    page_height: f64,
    zone: Zone,
    bbox: [f64; 4],
    spans: &'a [Span],
    /// The pairer reads a line by its characters, as its layout does, and places what it finds in UTF-16.
    coordinates: ScalarText<'a>,
    /// The line's characters, as the pairer indexes them.
    characters: Vec<char>,
    /// The note label the line opens on, read once.
    label: Option<LabelToken>,
    outline_spans: Vec<(usize, usize)>,
    note_column_fit: bool,
    small_font: bool,
    prose_like: bool,
    region_witness_demoted: bool,
    native_superscript_spans: Vec<(usize, usize)>,
    /// The line is a provider's flattened text, whose notes read as their HTML printed them:
    /// a label "[1]" or "Footnote 1", a call run into the word before it ("counsel[1]", "job.Footnote 1").
    provider: bool,
    edge_folio: bool,
    /// The line is the page's furniture, which holds no note's reference.
    furniture: bool,
    note_sequence_restart: bool,
}

impl<'a> PairLine<'a> {
    fn height(&self) -> f64 {
        self.bbox[3] - self.bbox[1]
    }

    /// Whether the line stands in its page's note band: a laid-out line (one with a box) in a band
    /// of notes. The band is final, so a label on it opens a note whether or not a reference to it
    /// was read, and the page's body holds that note's reference.
    fn in_note_band(&self) -> bool {
        matches!(self.line.zone, LineZone::Note(Some(_))) && self.height() > 0.0
    }

    /// Whether the line is a note's, in a band or a region read as notes.
    fn note_line(&self) -> bool {
        matches!(self.line.zone, LineZone::Note(_))
    }

    fn note_mode(&self) -> Option<NoteMode> {
        match self.line.zone {
            LineZone::Note(mode) => mode,
            _ => None,
        }
    }

    /// Where a character of the line stands in UTF-16.
    fn unit(&self, at: usize) -> usize {
        self.coordinates.utf16(at.min(self.coordinates.len()))
    }

    /// The line's text from its character `start`.
    fn tail(&self, start: usize) -> &'a str {
        self.coordinates.slice(start.min(self.coordinates.len())..self.coordinates.len()).expect("valid scalar range")
    }

    fn font_size(&self) -> f64 {
        self.line.layout.map_or(0.0, |line| line.size)
    }

    fn protected(&self, start: usize, end: usize) -> bool {
        let (from, to) = (self.unit(start), self.unit(end));
        self.citations
            .iter()
            .any(|span| from < span.end && to > span.start)
            || overlaps(&self.outline_spans, start, end)
    }
}

impl<'a> Deref for PairLine<'a> {
    type Target = NoteLine<'a>;

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
    repaired: bool,
    repair_kind: &'static str,
    requires_visual_cue: bool,
    flags: CandidateFlags,
}

impl Candidate {
    /// A note's label recovered by a repair (`repair_kind`) rather than read: plain, its score set by the repair.
    fn repaired(line: usize, start: usize, end: usize, observed: String, value: u32, repair_kind: &'static str) -> Self {
        Candidate { line, start, end, observed, value: Some(value), symbol: String::new(), form: "plain", score: 0.0, repaired: true,
            repair_kind, requires_visual_cue: false, flags: CandidateFlags::default() }
    }

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
    restart_sequence: usize,
    sequence_labels: usize,
    endnote: bool,
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

fn chars_slice(values: &[char], start: usize, end: usize) -> String {
    values
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

/// A superscript digit, as the text's digit grammar reads one (normalize_decimal_digit).
fn is_superscript(character: char) -> bool {
    !character.is_ascii_digit() && normalize_decimal_digit(character).is_some()
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

fn overlaps(spans: &[(usize, usize)], start: usize, end: usize) -> bool {
    spans
        .iter()
        .any(|(span_start, span_end)| start < *span_end && end > *span_start)
}

/// The pairer's reading of a line's zone: a note's, the body's, a heading's, a running head's, a
/// number's, a figure's or table's. A marginal note reads as a note's; a running foot as the body's.
fn zone_of(zone: LineZone) -> Zone {
    match zone {
        LineZone::Note(_) | LineZone::MarginalNote => Zone::Note,
        LineZone::Body | LineZone::Footer | LineZone::LineLabel => Zone::Body,
        LineZone::Heading => Zone::Title,
        LineZone::Header => Zone::Header,
        LineZone::Folio => Zone::Number,
        LineZone::Figure | LineZone::Table => Zone::Visual,
    }
}

/// Whether a line is a folio at the page's head or foot, or standing on its own: one to four digits.
fn is_edge_folio(line: &NoteLine<'_>) -> bool {
    matches!(line.zone, LineZone::Header | LineZone::Footer | LineZone::Folio) && {
        let text = line
            .text
            .trim()
            .trim_matches(['-', '\u{2013}', '\u{2014}'])
            .trim();
        (1..=4).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit())
    }
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

/// The lines of one part, in reading order, each read once for what the pairer asks of it; a
/// transcript's line labels are left out.
fn build_lines<'a>(source: &'a [NoteLine<'a>], provider: bool, part: usize) -> Vec<PairLine<'a>> {
    let mut lines: Vec<PairLine<'_>> = source
        .iter()
        .enumerate()
        .filter(|(_, line)| line.part == part)
        .enumerate()
        .filter(|(_, (_, line))| line.zone != LineZone::LineLabel)
        .enumerate()
        .map(|(idx, (source_order, (at, line)))| {
            let coordinates = ScalarText::new(line.text);
            let text_len = coordinates.len();
            let spans = line.layout.map_or(&[][..], |layout| layout.spans.as_slice());
            PairLine {
                line,
                at,
                idx,
                page: line.page.number,
                page_index: line.page.index,
                order: source_order + 1,
                page_width: line.page.width,
                page_height: line.page.height,
                zone: zone_of(line.zone),
                bbox: line.layout.map_or([0.0; 4], |layout| layout.bbox),
                spans,
                coordinates,
                characters: line.text.chars().collect(),
                label: label_token(line.text, provider),
                outline_spans: Vec::new(),
                note_column_fit: false,
                small_font: false,
                prose_like: prose_like(line.text),
                region_witness_demoted: false,
                native_superscript_spans: spans
                    .iter()
                    .filter(|span| {
                        span.superscript && span.start < span.end && span.end <= text_len
                    })
                    .map(|span| (span.start, span.end))
                    .collect(),
                provider,
                edge_folio: is_edge_folio(line),
                furniture: line.zone == LineZone::Footer,
                note_sequence_restart: false,
            }
        })
        .collect();
    let mut page_local_one_pages = HashSet::new();
    for line in &mut lines {
        let page_local_one = line.note_mode() == Some(NoteMode::Footnote)
            && legal_structure_model::text::label_prefix(line.text).is_some_and(|prefix| prefix.label == "1");
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
            .filter(|line| line.zone == Zone::Note && line.label.is_some())
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

/// A document's notes paired with their calls, each a Footnote (or Endnote) node placed in the reader's text by `place`
/// (a line's place among `lines` and a range of its text in UTF-16 code units), with what the pairing found wanting.
/// `lines` stand in reading order; `provider` says they are a provider's flattened text. The lines of each part (a
/// constituent document or language) are paired on their own, the parts in their order: the notes' ids count on across
/// them, and so do their numberings' restarts. The notes come in the order their labels stand in the reader's text.
pub fn pair_footnotes(
    lines: &[NoteLine<'_>],
    provider: bool,
    place: &dyn Fn(usize, usize, usize) -> Option<ScalarRange>,
) -> NotePairing {
    let parts: BTreeSet<usize> = lines.iter().map(|line| line.part).collect();
    let mut pairing = NotePairing { notes: Vec::new(), diagnostics: Vec::new() };
    let mut restarts = 0;
    for part in parts {
        let paired = build_lines(lines, provider, part);
        let pairs = build_pairs(&paired);
        restarts = materialize(pairs, &paired, place, restarts, &mut pairing);
    }
    pairing.notes.sort_by_key(|note| note.marker_range.map_or((usize::MAX, 0), |label| (label.start, label.end)));
    pairing
}