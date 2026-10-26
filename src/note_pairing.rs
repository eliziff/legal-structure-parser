use crate::layout_cues;
use legal_citations::cues;
use legal_citations::cues::is_counter_noun as counter_noun;
use legal_structure_model::layout::{
    self, line_font_size, Anchor, DetachedReference, Diagnostic, Footnote, Line, NotePairClaim,
    NotePairKind, Page, PairingOutput, PdfPairingAudit, PdfSourceSpan, SourceAnchor,
};
use legal_structure_model::{
    enumerator_interpretations, normalize_decimal_digit, normalize_note_symbol,
    parse_heading_ladder, select_numeric_sequence, HeadingLadderStatus, NotePairSupportV2,
    NumericSequenceCandidate, NumericSequencePolicy, ScalarText,
};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Deref;
use std::sync::OnceLock;

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
    note_sequence_restart: bool,
    detached_references: Cow<'a, [DetachedReference]>,
}

impl PairLine<'_> {
    fn height(&self) -> f64 {
        self.bbox[3] - self.bbox[1]
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

fn label_token(text: &str, provider: bool) -> Option<LabelToken> {
    let values = chars(text);
    let scalar = ScalarText::new(text);
    let mut cursor = 0;
    while cursor < values.len()
        && cursor < 3
        && (values[cursor].is_whitespace() || "\"'‘’“”.,:;–—-".contains(values[cursor]))
    {
        cursor += 1;
    }
    if provider && text.chars().skip(cursor).take(9).eq("Footnote ".chars()) {
        cursor += 9;
    }
    let pre_start = 0;
    let start = cursor;
    let mut form = "plain";
    let mut token_start = cursor;
    let token_end: usize;
    let mut value = None;
    let mut symbol = String::new();
    // A bracketed label encloses its number as a parenthesized one does.
    let close = match values.get(cursor) {
        Some('(') => Some(')'),
        Some('[') if provider => Some(']'),
        _ => None,
    };
    if let Some(close) = close {
        cursor += 1;
        while values
            .get(cursor)
            .is_some_and(|character| character.is_whitespace())
        {
            cursor += 1;
        }
        token_start = cursor;
        while cursor < values.len() && cursor - token_start < 3 && values[cursor].is_ascii_digit() {
            cursor += 1;
        }
        token_end = cursor;
        while values
            .get(cursor)
            .is_some_and(|character| character.is_whitespace())
        {
            cursor += 1;
        }
        if token_end == token_start || values.get(cursor) != Some(&close) {
            return None;
        }
        cursor += 1;
        value = numeric_value(scalar.slice(token_start..token_end)?);
        form = "paren";
    } else if values
        .get(cursor)
        .is_some_and(|character| is_superscript(*character))
    {
        while cursor < values.len() && cursor - token_start < 3 && is_superscript(values[cursor]) {
            cursor += 1;
        }
        token_end = cursor;
        value = numeric_value(scalar.slice(token_start..token_end)?);
        form = "sup";
    } else if values.get(cursor).is_some_and(char::is_ascii_digit) {
        while cursor < values.len() && cursor - token_start < 3 && values[cursor].is_ascii_digit() {
            cursor += 1;
        }
        token_end = cursor;
        if values.get(cursor).is_some_and(char::is_ascii_digit) {
            return None;
        }
        value = numeric_value(scalar.slice(token_start..token_end)?);
    } else if values
        .get(cursor)
        .is_some_and(|character| is_symbol(*character))
    {
        while cursor < values.len()
            && cursor - token_start < MAX_SYMBOL_RUN
            && is_symbol(values[cursor])
        {
            cursor += 1;
        }
        token_end = cursor;
        symbol = normalize_symbol(scalar.slice(token_start..token_end)?);
        form = "symbol";
    } else {
        return None;
    }
    let end = if form == "paren" { cursor } else { token_end };
    let post_start = cursor;
    while cursor < values.len() && cursor - post_start < 2 && ".)],".contains(values[cursor]) {
        cursor += 1;
    }
    let observed = scalar.slice(if form == "paren" { start } else { token_start }..end)?;
    let embedded_endnote = format!("endnote {observed}");
    let remainder = scalar.slice(cursor..scalar.len())?;
    let match_end = if form == "plain"
        && remainder
            .get(..embedded_endnote.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&embedded_endnote))
    {
        cursor + embedded_endnote.chars().count()
    } else {
        cursor
    };
    Some(LabelToken {
        pre_start,
        start: if form == "paren" { start } else { token_start },
        end,
        match_end,
        observed: observed.to_owned(),
        value,
        symbol,
        form,
        post: scalar.slice(post_start..cursor)?.to_owned(),
    })
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

fn core_line_label(text: &str) -> Option<String> {
    static LINE_START: OnceLock<Regex> = OnceLock::new();
    static PURE: OnceLock<Regex> = OnceLock::new();
    let line_start =
        LINE_START.get_or_init(|| Regex::new(r"^\s*(\d{1,4}|[*†‡§¶#])(?:\s|[.)\],:;-])").unwrap());
    let pure = PURE.get_or_init(|| Regex::new(r"^(?:\d{1,4}|[*†‡§¶#])$").unwrap());
    let raw = line_start
        .captures(text)
        .and_then(|capture| capture.get(1))
        .map(|value| value.as_str())
        .or_else(|| {
            let stripped = text.trim();
            pure.is_match(stripped).then_some(stripped)
        })?;
    Some(
        normalize_symbol(raw)
            .parse::<u32>()
            .map_or_else(|_| normalize_symbol(raw), |number| number.to_string()),
    )
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
                note_sequence_restart: false,
                detached_references,
            }
        })
        .collect();
    let mut page_local_one_pages = HashSet::new();
    for line in &mut lines {
        let page_local_one = line.region_type == "footnote"
            && line.note_region_mode == "footnote"
            && core_line_label(&line.text).as_deref() == Some("1");
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

/// Preserve the old extractor's detached-marker evidence when the faster PDF
/// backend emits the same raised run inline. A visibly separated line-final
/// digit is the exact case PyMuPDF exposed as its own row; contiguous native
/// superscripts remain on the monotone pairing path.
fn isolated_inline_references(line: &Line) -> Vec<DetachedReference> {
    let text_len = line.text.chars().count();
    line.spans
        .iter()
        .filter_map(|marker| {
            let selected = marker.text.trim();
            if !marker.superscript
                || marker.end != text_len
                || marker.start >= marker.end
                || selected.is_empty()
                || selected.len() > 4
                || !selected.chars().all(|character| character.is_ascii_digit())
            {
                return None;
            }
            let host = line
                .spans
                .iter()
                .filter(|span| !span.superscript && span.end == marker.start)
                .max_by_key(|span| span.end)?;
            let gap = marker.bbox[0] - host.bbox[2];
            (gap >= host.size.max(1.0) * 0.25).then(|| DetachedReference {
                note_id: selected.to_owned(),
                selected_text: selected.to_owned(),
                start_offset: marker.start,
                end_offset: marker.end,
                source_line_id: line.id.clone(),
            })
        })
        .collect()
}

fn ref_zone_score(line: &PairLine<'_>) -> Option<(f64, bool)> {
    match line.zone {
        Zone::Body => Some((0.6, false)),
        Zone::Title => Some((0.1, false)),
        Zone::Visual => Some((-0.4, false)),
        Zone::Note => Some((-0.4, true)),
        Zone::Header | Zone::Number if line.prose_like => Some((-0.5, true)),
        _ => None,
    }
}

fn ref_left(character: char) -> bool {
    character.is_alphabetic() || REF_PUNCTUATION.contains(character) || QUOTES.contains(character)
}

fn ref_right(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\u{00a0}')
        || REF_RIGHT_PUNCTUATION.contains(character)
        || QUOTES.contains(character)
        || DASHES.contains(character)
}

fn chars_at_line_end(values: &[char], end: usize) -> bool {
    let tail = values.get(end..).unwrap_or_default();
    let start = tail
        .iter()
        .position(|character| !character.is_whitespace())
        .unwrap_or(tail.len());
    let end = tail
        .iter()
        .rposition(|character| !character.is_whitespace())
        .map_or(start, |index| index + 1);
    tail[start..end]
        .iter()
        .all(|character| REF_PUNCTUATION.contains(*character) || QUOTES.contains(*character))
}

fn at_line_end(text: &str, end: usize) -> bool {
    let text = ScalarText::new(text);
    text.slice(end.min(text.len())..text.len())
        .is_some_and(|tail| {
            tail.trim()
                .chars()
                .all(|character| REF_PUNCTUATION.contains(character) || QUOTES.contains(character))
        })
}

fn preceding_word(values: &[char], start: usize) -> String {
    let end = start.min(values.len());
    let start = values[..end]
        .iter()
        .rposition(|character| !character.is_ascii_alphabetic())
        .map_or(0, |index| index + 1);
    values[start..end].iter().collect()
}

fn spaced_preceding_word(values: &[char], start: usize) -> String {
    let mut end = start.min(values.len());
    while end > 0 && matches!(values[end - 1], ' ' | '\t') {
        end -= 1;
    }
    preceding_word(values, end)
}

fn abbreviation_pinpoint(prefix: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:^|[\s(\[])(?:pp?|paras?|ss?|secs?|arts?|vols?|nos?|c|ch|cc|pts?|eds?|figs?|tabs?|apps?|cls?)\.$",
        )
        .unwrap()
    })
    .is_match(prefix)
}

fn date_day_site(values: &[char], start: usize, end: usize) -> bool {
    static MONTH_BEFORE: OnceLock<Regex> = OnceLock::new();
    static MONTH_AFTER: OnceLock<Regex> = OnceLock::new();
    static COMMA_YEAR: OnceLock<Regex> = OnceLock::new();
    static DAY_LIST: OnceLock<Regex> = OnceLock::new();
    let month = MONTH;
    let prefix = chars_slice(values, 0, start);
    let tail = chars_slice(values, end, values.len());
    MONTH_BEFORE
        .get_or_init(|| Regex::new(&format!(r"(?i)\b{month}\.?[ \t]+$")).unwrap())
        .is_match(&prefix)
        || MONTH_AFTER
            .get_or_init(|| Regex::new(&format!(r"(?i)^[ \t]+{month}\b[.,]?")).unwrap())
            .is_match(&tail)
        || COMMA_YEAR
            .get_or_init(|| Regex::new(r"^,\s*(?:1[6-9]|20)\d\d\b").unwrap())
            .is_match(&tail)
        || DAY_LIST
            .get_or_init(|| {
                Regex::new(&format!(
                    r"(?i)^\s*\d{{1,2}}(?:\s*,\s*\d{{1,2}})+\s+{month}\b"
                ))
                .unwrap()
            })
            .is_match(&chars_slice(values, start, values.len()))
}

fn ref_site_penalty(
    line: &PairLine<'_>,
    values: &[char],
    start: usize,
    end: usize,
    form: &str,
) -> Option<f64> {
    if form == "sup" {
        return Some(0.0);
    }
    // A number in a web address ("ubclawreview/vol50/iss2/6", a repository's stamp) is part of it.
    let word_start = values[..start]
        .iter()
        .rposition(|value| value.is_whitespace())
        .map_or(0, |at| at + 1);
    let word_end = values[end..]
        .iter()
        .position(|value| value.is_whitespace())
        .map_or(values.len(), |at| end + at);
    let word = chars_slice(values, word_start, word_end);
    if word.contains('/') || word.contains("www.") {
        return None;
    }
    let left = start
        .checked_sub(1)
        .and_then(|index| values.get(index))
        .copied();
    let right = values.get(end).copied();
    let mut penalty = 0.0;
    if let Some(right) = right {
        if !ref_right(right) {
            let tail = chars_slice(values, end, (end + 2).min(values.len())).to_lowercase();
            if ["th", "st", "nd", "rd", "am", "pm"].contains(&tail.as_str())
                && values
                    .get(end + 2)
                    .is_none_or(|value| !value.is_alphabetic())
            {
                return None;
            }
            if right.is_uppercase() {
                penalty += 0.4;
            } else if right.is_lowercase()
                && values
                    .get(end + 1)
                    .is_none_or(|value| value.is_whitespace())
            {
                penalty += 0.7;
            } else if right.is_lowercase() {
                penalty += 1.0;
            } else {
                return None;
            }
        }
        if DASHES.contains(right) && values.get(end + 1).is_some_and(char::is_ascii_digit) {
            return None;
        }
        if right == '.' && values.get(end + 1).is_some_and(char::is_ascii_digit) {
            let left_char = left.unwrap_or_default();
            let following_end = values[end + 1..]
                .iter()
                .position(|value| !value.is_ascii_digit())
                .map_or(values.len(), |offset| end + 1 + offset);
            if !(left_char.is_alphabetic()
                && values.get(end + 1..following_end) == values.get(start..end))
            {
                return None;
            }
        }
        // Hours and minutes ("2:47"), or chapter and verse.
        if right == ':' && values.get(end + 1).is_some_and(char::is_ascii_digit) {
            return None;
        }
        if right == ','
            && values
                .get(end + 1..end + 4)
                .is_some_and(|tail| tail.iter().all(char::is_ascii_digit))
            && values
                .get(end + 4)
                .is_none_or(|value| !value.is_ascii_digit())
        {
            return None;
        }
    }
    if left.is_some_and(|value| DASHES.contains(value))
        && start >= 2
        && values[start - 2].is_ascii_digit()
    {
        return None;
    }
    if left == Some('.') && start >= 2 && values[start - 2] == '.' {
        let mut run_start = start - 2;
        while run_start > 0 && values[run_start - 1] == '.' {
            run_start -= 1;
        }
        if run_start == 0 || !values[run_start - 1].is_alphabetic() {
            return None;
        }
        penalty += 0.4;
    }
    if left == Some('.') && start >= 2 && values[start - 2].is_ascii_digit() {
        // A number running on into lower-case words ("rule 4.4 and") is a decimal or
        // section number; a reference after a sentence's final number starts a sentence.
        if values[end..]
            .iter()
            .find(|value| !value.is_whitespace())
            .is_some_and(|value| value.is_lowercase())
        {
            return None;
        }
        penalty += 0.8;
    }
    if values[start.saturating_sub(3)..start].contains(&'$') {
        return None;
    }
    if overlaps(&line.outline_spans, start, end) {
        return None;
    }
    if line.protected(start, end) {
        let tail_glued = left.is_some_and(|value| ".,".contains(value))
            && start >= 2
            && values[start - 2].is_ascii_digit()
            && line
                .citation_spans
                .iter()
                .any(|span| end == span.end && start > span.start);
        if !tail_glued {
            return None;
        }
        penalty += 1.2;
    }
    let word = preceding_word(values, start);
    if counter_noun(&word) {
        if line.zone == Zone::Note {
            return None;
        }
        penalty += 1.1;
    }
    let prefix = chars_slice(values, 0, start);
    if abbreviation_pinpoint(&prefix) {
        return None;
    }
    // A page number or page count ("page 4", "Page 1 of 3").
    static PAGE_COUNTER: OnceLock<Regex> = OnceLock::new();
    if PAGE_COUNTER
        .get_or_init(|| {
            Regex::new(r"(?i)(?:^|[^\p{L}])(?:pages?|p\.|pp\.)\s*(?:\d+\s*(?:of|/|de)\s*)?$")
                .unwrap()
        })
        .is_match(&prefix)
    {
        return None;
    }
    if matches!(form, "spaced_eol" | "spaced_mid")
        && counter_noun(&spaced_preceding_word(values, start))
    {
        return None;
    }
    // A spaced number counts a measure mid-line or at a line's start, and is a date's
    // day wherever it sits.
    if matches!(form, "spaced_mid" | "line_start") {
        static MEASURE: OnceLock<Regex> = OnceLock::new();
        if MEASURE
            .get_or_init(|| Regex::new(r"(?i)^[ \t]+(?:years?|days?|months?|weeks?|hours?|minutes?|seconds?|per|percent|p\.c\b|cents?|dollars?|pounds?|shillings?|pence|feet|foot|acres?|miles?|inches?|yards?|tons?|o.?clock)\b").unwrap())
            .is_match(&chars_slice(values, end, values.len()))
        {
            return None;
        }
    }
    if matches!(form, "spaced_mid" | "spaced_eol" | "line_start")
        && date_day_site(values, start, end)
    {
        return None;
    }
    Some(penalty)
}

fn digit_runs(values: &[char]) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < values.len() {
        if !values[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < values.len() && values[index].is_ascii_digit() {
            index += 1;
        }
        if index - start <= 7 {
            result.push((start, index));
        }
    }
    result
}

fn ascii_number(values: &[char], start: usize, end: usize) -> u32 {
    values[start..end]
        .iter()
        .fold(0, |number, digit| number * 10 + digit.to_digit(10).unwrap())
}

fn superscript_runs(values: &[char]) -> Vec<(usize, usize, String)> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < values.len() {
        if !is_superscript(values[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < values.len() && is_superscript(values[index]) && index - start < 4 {
            index += 1;
        }
        result.push((start, index, values[start..index].iter().collect()));
    }
    result
}

fn symbol_runs(values: &[char]) -> Vec<(usize, usize, String)> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < values.len() {
        if !is_symbol(values[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < values.len() && is_symbol(values[index]) && index - start < MAX_SYMBOL_RUN {
            index += 1;
        }
        result.push((start, index, values[start..index].iter().collect()));
    }
    result
}

fn extract_refs(lines: &[PairLine<'_>]) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    let mut previous_body = None;
    let mut previous_page = None;
    let mut previous_by_line = Vec::with_capacity(lines.len());
    static PAREN_REF: OnceLock<Regex> = OnceLock::new();
    static SECTION_BEFORE_PAREN: OnceLock<Regex> = OnceLock::new();
    let paren_ref = PAREN_REF.get_or_init(|| Regex::new(r"\(\s*(\d{1,3})\s*\)").unwrap());
    static PROVIDER_CALL: OnceLock<Regex> = OnceLock::new();
    let provider_call =
        PROVIDER_CALL.get_or_init(|| Regex::new(r"\[(\d{1,3})\]|Footnote (\d{1,3})\b").unwrap());
    let section_before = SECTION_BEFORE_PAREN
        .get_or_init(|| Regex::new(r"(?i)\b(?:ss?|sub-?ss?|arts?|paras?|cls?)\.?\s*$").unwrap());
    // A subsection number after the section number opening its line: "2. (1) The ...".
    static SECTION_NUMBER: OnceLock<Regex> = OnceLock::new();
    let section_number =
        SECTION_NUMBER.get_or_init(|| Regex::new(r"^\s*\d{1,4}[A-Za-z]?\.\s*$").unwrap());
    for (line_index, line) in lines.iter().enumerate() {
        if previous_page != Some(line.page) {
            previous_body = None;
            previous_page = Some(line.page);
        }
        previous_by_line.push(previous_body);
        if line.zone == Zone::Body && !line.text.trim().is_empty() {
            previous_body = Some(line_index);
        }
    }
    for (line_index, line) in lines.iter().enumerate() {
        let zone = ref_zone_score(line);
        let sup_zone_score = zone.map(|(score, _)| score).or_else(|| {
            (matches!(line.zone, Zone::Header | Zone::Number)
                && line.text.trim().chars().count() >= 6)
                .then_some(0.1)
        });
        let Some(sup_zone_score) = sup_zone_score else {
            continue;
        };
        let label_end = if line.zone == Zone::Note {
            label_token(&line.text, provider_text(line)).map_or(0, |token| token.match_end)
        } else {
            0
        };
        let line_chars = chars(&line.text);
        // A provider's flattened text runs a note's call into the word before it: "counsel[1] for",
        // "appeal.[1] The", "job.Footnote 1 The".
        // Its calls take no other form: no superscript survives the flattening, and its digits are text.
        if provider_text(line) {
            let Some((zone_score, _)) = zone else {
                continue;
            };
            for captures in provider_call.captures_iter(&line.text) {
                let whole = captures.get(0).expect("provider call");
                let digits = captures.get(1).or_else(|| captures.get(2)).expect("call digits");
                let open = line.text[..whole.start()].chars().count();
                let start = line.text[..digits.start()].chars().count();
                let end = line.text[..digits.end()].chars().count();
                // A bracketed call may follow a number: a citation's page runs into it ("2011 SKCA 136[1]");
                // a year in brackets has four digits, which no call has.
                let left = line_chars.get(open.wrapping_sub(1)).copied();
                if open == 0
                    || start < label_end
                    || !left.is_some_and(|left| ref_left(left) || left.is_ascii_digit() && whole.as_str().starts_with('['))
                    || line.protected(start, end)
                {
                    continue;
                }
                let Some(value) = numeric_value(digits.as_str()) else {
                    continue;
                };
                candidates.push(Candidate {
                    line: line_index,
                    start: open,
                    end: line.text[..whole.end()].chars().count(),
                    observed: digits.as_str().to_owned(),
                    value: Some(value),
                    symbol: String::new(),
                    form: "paren",
                    score: zone_score + 0.4,
                    reason: "provider_call",
                    repaired: false,
                    repair_kind: "",
                    requires_visual_cue: false,
                    flags: CandidateFlags {
                        paren_ref: true,
                        ..CandidateFlags::default()
                    },
                });
            }
            continue;
        }
        let trimmed_len = line_chars
            .iter()
            .rposition(|character| !character.is_whitespace())
            .map_or(0, |index| index + 1);
        let digit_runs = digit_runs(&line_chars);
        let mut seen = HashSet::new();
        for (start, end, observed) in superscript_runs(&line_chars) {
            if start < label_end || line.protected(start, end) {
                continue;
            }
            let Some(value) = numeric_value(&observed) else {
                continue;
            };
            seen.insert((start, end));
            candidates.push(Candidate {
                line: line_index,
                start,
                end,
                observed,
                value: Some(value),
                symbol: String::new(),
                form: "sup",
                score: 1.6 + sup_zone_score,
                reason: "superscript_marker",
                repaired: false,
                repair_kind: "",
                requires_visual_cue: line.zone == Zone::Visual,
                flags: CandidateFlags::default(),
            });
        }
        for &(start, end) in &line.native_superscript_spans {
            // A raised native call can follow a citation whose lexical span runs over it.
            if seen.contains(&(start, end))
                || start < label_end
                || overlaps(&line.outline_spans, start, end)
            {
                continue;
            }
            let observed = chars_slice(&line_chars, start, end);
            let value = numeric_value(&observed);
            let symbol = value.map_or_else(
                || {
                    if (1..=MAX_SYMBOL_RUN).contains(&observed.chars().count())
                        && observed.chars().all(is_symbol)
                    {
                        normalize_symbol(&observed)
                    } else {
                        String::new()
                    }
                },
                |_| String::new(),
            );
            if value.is_none() && symbol.is_empty() {
                continue;
            };
            seen.insert((start, end));
            candidates.push(Candidate {
                line: line_index,
                start,
                end,
                observed,
                value,
                symbol,
                form: if value.is_some() { "sup" } else { "symbol" },
                score: 1.6 + sup_zone_score,
                reason: "native_superscript_span",
                repaired: false,
                repair_kind: "",
                requires_visual_cue: line.zone == Zone::Visual,
                flags: CandidateFlags::default(),
            });
        }
        let Some((zone_score, _)) = zone else {
            continue;
        };
        let stripped = line.text.trim();
        if stripped.chars().all(|character| character.is_ascii_digit())
            && (1..=3).contains(&stripped.chars().count())
            && line.zone == Zone::Body
            // A folio's place at the page's top; a line without a box has none.
            && !(line.bbox[3] > 0.0
                && line.page_height > 0.0
                && (line.bbox[1] + line.bbox[3]) / 2.0 / line.page_height <= 0.05)
        {
            if let Some(value) = numeric_value(stripped) {
                let start = line
                    .text
                    .chars()
                    .position(|character| character.is_ascii_digit())
                    .unwrap_or(0);
                candidates.push(Candidate {
                    line: line_index,
                    start,
                    end: start + stripped.chars().count(),
                    observed: stripped.to_owned(),
                    value: Some(value),
                    symbol: String::new(),
                    form: "standalone",
                    score: 0.5 + zone_score,
                    reason: "standalone_marker_line",
                    repaired: false,
                    repair_kind: "",
                    requires_visual_cue: false,
                    flags: CandidateFlags::default(),
                });
                continue;
            }
        }
        for &(start, end) in digit_runs
            .iter()
            .filter(|(start, end)| (1..=3).contains(&(end - start)))
        {
            // A note is never numbered with a leading zero ("2023.11.05", "F28.02A").
            if seen.contains(&(start, end))
                || start < label_end
                || (end - start > 1 && line_chars[start] == '0')
            {
                continue;
            }
            let value = ascii_number(&line_chars, start, end);
            if !(1..=MAX_VALUE).contains(&value) {
                continue;
            }
            let left = start
                .checked_sub(1)
                .and_then(|index| line_chars.get(index))
                .copied();
            let at_eol = chars_at_line_end(&line_chars, end);
            let form = if start == 0 {
                let previous = previous_by_line[line_index].map(|index| &lines[index]);
                let follow_ok = line_chars
                    .get(end)
                    .is_none_or(|value| value.is_whitespace());
                if line.zone == Zone::Note
                    || !follow_ok
                    || previous.is_none_or(|value| {
                        value
                            .text
                            .trim_end()
                            .ends_with(|value| TERMINAL_PUNCTUATION.contains(value))
                    })
                {
                    continue;
                }
                "line_start"
            } else if left.is_some_and(ref_left) {
                "tight"
            } else if left.is_some_and(char::is_whitespace) && at_eol && line.zone != Zone::Note {
                "spaced_eol"
            } else if left.is_some_and(char::is_whitespace) && line.zone == Zone::Body {
                "spaced_mid"
            } else if left.is_some_and(|value| value.is_lowercase()) {
                let word = preceding_word(&line_chars, start);
                if word.len() < 3
                    || counter_noun(&word)
                    || line_chars.get(end).is_some_and(|value| !ref_right(*value))
                {
                    continue;
                }
                "letter_glued"
            } else {
                continue;
            };
            let Some(penalty) = ref_site_penalty(line, &line_chars, start, end, form) else {
                continue;
            };
            let mut score = zone_score - penalty
                + match form {
                    "tight" => 0.9,
                    "line_start" => -0.3,
                    "spaced_eol" => 0.1,
                    "spaced_mid" | "letter_glued" => -0.6,
                    _ => 0.0,
                };
            if end == trimmed_len {
                score += 0.15;
            }
            if matches!(line.zone, Zone::Title | Zone::Visual) && form != "tight" {
                continue;
            }
            candidates.push(Candidate {
                line: line_index,
                start,
                end,
                observed: chars_slice(&line_chars, start, end),
                value: Some(value),
                symbol: String::new(),
                form,
                score,
                reason: match form {
                    "tight" => "attached_digit_marker",
                    "line_start" => "line_start_marker",
                    "spaced_eol" => "spaced_end_of_line_marker",
                    "spaced_mid" => "spaced_mid_line_marker",
                    _ => "word_glued_marker",
                },
                repaired: false,
                repair_kind: "",
                requires_visual_cue: line.zone == Zone::Visual,
                flags: CandidateFlags::default(),
            });
        }
        for &(start, end) in digit_runs
            .iter()
            .filter(|(start, end)| (5..=7).contains(&(end - start)))
        {
            if start < label_end || matches!(line.zone, Zone::Title | Zone::Visual) {
                continue;
            }
            let year = ascii_number(&line_chars, start, start + 4);
            let tail = ascii_number(&line_chars, start + 4, end);
            let left = start
                .checked_sub(1)
                .and_then(|index| line_chars.get(index))
                .copied();
            if !(1600..=2069).contains(&year)
                || tail == 0
                || left.is_some_and(|value| !value.is_whitespace() && !ref_left(value))
                || line_chars[start.saturating_sub(3)..start].contains(&'$')
            {
                continue;
            }
            let sub_start = start + 4;
            let Some(penalty) = ref_site_penalty(line, &line_chars, sub_start, end, "tight") else {
                continue;
            };
            let mut score = zone_score - penalty;
            if end == trimmed_len {
                score += 0.15;
            }
            candidates.push(Candidate {
                line: line_index,
                start: sub_start,
                end,
                observed: chars_slice(&line_chars, sub_start, end),
                value: Some(tail),
                symbol: String::new(),
                form: "year_glued",
                score,
                reason: "year_glued_marker",
                repaired: false,
                repair_kind: "",
                requires_visual_cue: false,
                flags: CandidateFlags::default(),
            });
        }
        for captures in paren_ref.captures_iter(&line.text) {
            let whole = captures.get(0).expect("paren ref");
            let digits = captures.get(1).expect("paren digits");
            let open = line.text[..whole.start()].chars().count();
            let start = line.text[..digits.start()].chars().count();
            let end = line.text[..digits.end()].chars().count();
            if start < label_end
                || (open > 0 && line_chars[open - 1].is_alphanumeric())
                || section_before.is_match(&char_slice(&line.text, 0, open))
                || section_number.is_match(&char_slice(&line.text, 0, open))
                || line.protected(start, end)
            {
                continue;
            }
            let Some(value) = numeric_value(digits.as_str()) else {
                continue;
            };
            let Some(penalty) = ref_site_penalty(line, &line_chars, start, end, "tight") else {
                continue;
            };
            candidates.push(Candidate {
                line: line_index,
                start,
                end,
                observed: digits.as_str().to_owned(),
                value: Some(value),
                symbol: String::new(),
                form: "paren",
                score: zone_score - penalty + 0.4,
                reason: "paren_marker",
                repaired: false,
                repair_kind: "",
                requires_visual_cue: line.zone == Zone::Visual,
                flags: CandidateFlags {
                    paren_ref: true,
                    ..CandidateFlags::default()
                },
            });
        }
        if line.zone != Zone::Note {
            for (start, end, observed) in symbol_runs(&line_chars) {
                if start == 0
                    || seen.contains(&(start, end))
                    || start < label_end
                    || line.protected(start, end)
                {
                    continue;
                }
                let left = line_chars[start - 1];
                let mut spaced = left.is_whitespace() && line.zone == Zone::Title;
                if left.is_whitespace() && line.zone == Zone::Body && !spaced {
                    let prefix = chars_slice(&line_chars, 0, start);
                    let tail = chars_slice(&line_chars, end, line_chars.len());
                    let run_head = prefix
                        .trim_end()
                        .chars()
                        .last()
                        .is_none_or(|character| !is_symbol(character));
                    let starts_with_run =
                        line.text.trim_start().chars().next().is_some_and(is_symbol);
                    let more_symbols = tail.trim_start().chars().next().is_some_and(is_symbol);
                    let prose = line_chars
                        .iter()
                        .filter(|character| character.is_alphabetic())
                        .count()
                        >= 12;
                    spaced = run_head && !starts_with_run && more_symbols && prose;
                }
                let glued_byline = left.is_alphabetic() && line.zone == Zone::Title;
                if !ref_left(left) && !left.is_ascii_digit() && !spaced && !glued_byline {
                    continue;
                }
                if line_chars.get(end).is_some_and(|right| !ref_right(*right)) {
                    continue;
                }
                candidates.push(Candidate {
                    line: line_index,
                    start,
                    end,
                    observed: observed.clone(),
                    value: None,
                    symbol: normalize_symbol(&observed),
                    form: "symbol",
                    score: 0.9 + zone_score - if spaced { 0.3 } else { 0.0 },
                    reason: "attached_symbol_marker",
                    repaired: false,
                    repair_kind: "",
                    requires_visual_cue: line.zone == Zone::Visual,
                    flags: CandidateFlags::default(),
                });
            }
        }
    }
    candidates
}

fn heading_shaped(body: &str) -> bool {
    let mut letters = 0;
    body.trim().chars().take(40).all(|character| {
        if character.is_alphabetic() {
            letters += 1;
            character.is_uppercase()
        } else {
            true
        }
    }) && letters >= 4
}

fn paren_style(lines: &[PairLine<'_>]) -> bool {
    let mut paren = 0;
    let mut plain = 0;
    for line in lines.iter().filter(|line| line.zone == Zone::Note) {
        let Some(token) = label_token(&line.text, provider_text(line)) else {
            continue;
        };
        if token.form == "paren" || (token.form == "plain" && token.post.contains(')')) {
            paren += 1;
        } else if matches!(token.form, "plain" | "sup") {
            plain += 1;
        }
    }
    paren >= 3 && paren > 2 * plain
}

fn spaced_symbol_separator(text: &str) -> bool {
    let mut parts = 0;
    text.split_whitespace().all(|part| {
        parts += 1;
        part.chars().count() == 1 && part.chars().all(is_symbol)
    }) && parts >= 3
}

fn day_list_date(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*\d{1,2}(?:\s*,\s*\d{1,2})+\s+(?:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\b",
        )
        .unwrap()
    })
    .is_match(text)
}

fn month_year_start(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"(?i)^\s*{MONTH}\.?,?\s+\d{{4}}\b")).unwrap())
        .is_match(text)
}

fn volume_cite_start(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let Some(found) = RE
        .get_or_init(|| {
        Regex::new(
            r"^\d{1,3}\s+[A-Z][A-Za-z]{0,5}\.(?:(?:\s+(?:&\s*)?|&\s*)[A-Z][A-Za-z]{0,5}\.?|[A-Z]\.)*\s*(?:\(\d{1,4}[a-z]{0,2}\)\s*)?(?:(?:c|ch|ss?)\.\s*)?\d{1,4}",
        )
        .unwrap()
    })
        .find(text)
    else {
        return false;
    };
    text[found.end()..]
        .chars()
        .next()
        .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_')
}

fn bracket_year_body(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\[(?:1[5-9]|20)\d\d\]").unwrap())
        .is_match(text)
}

/// Whether words stand on the line's own row, on either side: a paragraph's or a form
/// item's text set beside its number, or a form's line around a blank's number. A note's
/// label alone on its line has its text below.
fn words_on_row(lines: &[PairLine<'_>], index: usize) -> bool {
    let line = &lines[index];
    let height = line.height();
    let same_page = |other: &&PairLine<'_>| other.page == line.page;
    let start = lines[..index].iter().rev().take_while(same_page).count();
    let end = lines[index..].iter().take_while(same_page).count();
    height > 0.0
        && lines[index - start..index + end].iter().any(|other| {
            let overlap = line.bbox[3].min(other.bbox[3]) - line.bbox[1].max(other.bbox[1]);
            other.idx != line.idx
                && overlap > 0.5 * height.min(other.height())
                && other.text.chars().any(char::is_alphabetic)
        })
}

fn extract_labels(lines: &[PairLine<'_>], refs: &[Candidate], first_page: u32) -> Vec<Candidate> {
    let mut ref_pages: HashMap<u32, HashSet<u32>> = HashMap::new();
    for candidate in refs
        .iter()
        .filter(|candidate| !candidate.repaired && !candidate.flags.paren_ref)
    {
        if let Some(value) = candidate.value {
            ref_pages
                .entry(value)
                .or_default()
                .insert(lines[candidate.line].page);
        }
    }
    let paren_style = paren_style(lines);
    let mut candidates = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        if line.suppress_footnote_label || line.edge_folio {
            continue;
        }
        let Some(token) = label_token(&line.text, provider_text(line)) else {
            continue;
        };
        let paired_note_region = (line.zone == Zone::Note && !line.region_witness_demoted
            || line.note_column_fit)
            && token.value.is_some_and(|value| {
                ref_pages.get(&value).is_some_and(|pages| {
                    pages.iter().any(|page| {
                        line.page.abs_diff(*page) <= 1 || line.note_region_mode == "endnote"
                    })
                })
            });
        // A note such as "1 S.C. 1997, c. 12" can also parse as a reporter
        // citation beginning with volume 1. Independent reference and note-region
        // evidence keep both readings alive for the sequence assignment.
        if line.protected(token.start, token.end) && !paired_note_region {
            continue;
        }
        let scalar = ScalarText::new(&line.text);
        let body = scalar
            .slice(token.match_end..scalar.len())
            .expect("token offsets must be valid");
        let follow = body.chars().next();
        if token.form == "plain"
            && token.value.is_some()
            && day_list_date(
                scalar
                    .slice(token.start..scalar.len())
                    .expect("token offsets must be valid"),
            )
        {
            continue;
        }
        if token.post.is_empty()
            && follow.is_some_and(|value| {
                !value.is_whitespace()
                    && !value.is_uppercase()
                    && !QUOTES.contains(value)
                    && !(value == '[' && bracket_year_body(&body))
            })
        {
            continue;
        }
        if token.symbol.is_empty() && token.value.is_none() {
            continue;
        }
        // A decimal ("17.93"), a zero-padded number ("05") or a date ("13 May 2026")
        // opens the line, not a note's label.
        if token.value.is_some()
            && ((token.observed.len() > 1 && token.observed.starts_with('0'))
                || (token.post.starts_with('.')
                    && follow.is_some_and(|value| value.is_ascii_digit()))
                || (token.form == "plain" && month_year_start(&body)))
        {
            continue;
        }
        if token.form == "plain" && follow.is_some_and(|value| value.is_lowercase()) {
            continue;
        }
        if token.form == "symbol" {
            if spaced_symbol_separator(&line.text) {
                continue;
            }
            let bare = body.trim().is_empty();
            if !(bare && line.page - first_page < SYMBOL_START_PAGE_LIMIT)
                && body.trim().chars().count() < 4
            {
                continue;
            }
        }
        let body_stripped = body.trim();
        if token.value.is_some() && line.zone != Zone::Note && heading_shaped(body_stripped) {
            continue;
        }
        // A number and its period alone on a body line ("4.") is a numbered paragraph's
        // number when the layout set its words beside it on the same row: never a note's
        // label. So is a bare number at the body's size ("22"), as Singapore's courts print
        // theirs. A note's label alone on its line, its text below, stays one.
        if token.value.is_some()
            && line.zone == Zone::Body
            && body_stripped.is_empty()
            && (token.post.trim() == "." || (token.post.is_empty() && !line.small_font))
            && words_on_row(lines, line_index)
        {
            continue;
        }
        // So is a number and its period opening a body line in the body's own type, its words
        // running on beside it ("1. SERVICE: Once you have filed"): a note's label sits in the
        // notes' region or in their smaller type. A line that carries no type, as recognition
        // reads it, says neither.
        if token.value.is_some()
            && line.zone == Zone::Body
            && !line.spans.is_empty()
            && !line.small_font
            && token.post.trim() == "."
            && !body_stripped.is_empty()
        {
            continue;
        }
        // So is a bare or bracketed number opening a body line in the body's type with prose
        // beside it ("4  Where a compromise is proposed", "(8)  Any claim by"): a statute's
        // section or subsection, or a judgment's paragraph. A note set in the body's type opens
        // as a citation ("9 [1991] 2 S.C.R. 577", "6 Ibid., p. 8"), not prose citing one later.
        if token.value.is_some()
            && matches!(token.form, "plain" | "paren")
            && line.zone == Zone::Body
            && !line.spans.is_empty()
            && !line.small_font
            && !body_stripped.is_empty()
            && !cues::layout_has_citation_cue(body_stripped.split_whitespace().next().unwrap_or(""))
        {
            continue;
        }
        if token.value.is_some()
            && token.post.contains(',')
            && line.zone != Zone::Note
            && body_stripped
                .chars()
                .next()
                .is_some_and(|character| character.is_lowercase())
        {
            continue;
        }
        let mut score = match line.zone {
            Zone::Note => 3.0,
            Zone::Body => 0.2,
            Zone::Title => -2.0,
            Zone::Header | Zone::Number => -1.2,
            Zone::Visual => -1.5,
        };
        if line.zone == Zone::Note && line.region_witness_demoted {
            score = 0.6;
        }
        if matches!(line.zone, Zone::Header | Zone::Number)
            && !line.prose_like
            && body_stripped.is_empty()
        {
            continue;
        }
        if token.form == "sup" {
            score += 1.0;
        } else if token.form == "paren" || (token.form == "plain" && token.post.contains(')')) {
            score += if paren_style { 0.4 } else { -1.8 };
        } else if !token.post.is_empty() || follow.is_some_and(char::is_whitespace) {
            score += 0.8;
        }
        if !body_stripped.is_empty() {
            if cues::layout_has_citation_cue(body_stripped) {
                score += 0.7;
            } else if body_stripped.chars().count() >= 8 {
                score += 0.4;
            }
            if cues::layout_is_citation_continuation(body_stripped) {
                score -= 1.2;
            }
        } else if !(token.form == "symbol" && line.page - first_page < SYMBOL_START_PAGE_LIMIT) {
            score -= 0.9;
        }
        if line.note_column_fit {
            score += 0.7;
        }
        if line.small_font {
            score += 0.4;
        }
        let junk_prefix = scalar
            .slice(token.pre_start..token.start)
            .expect("token offsets must be valid")
            .trim()
            .to_owned();
        if !junk_prefix.is_empty() {
            score -= 0.6;
        }
        let ref_supported = token.value.is_some_and(|value| {
            ref_pages
                .get(&value)
                .is_some_and(|pages| pages.iter().any(|page| line.page.abs_diff(*page) <= 1))
        });
        if ref_supported {
            score += 0.9;
        }
        // Paragraph layout is a competing witness, not a veto before pairing.
        if line.paragraph_label_candidate {
            score -= 0.9;
        }
        let candidate = Candidate {
            line: line_index,
            start: token.start,
            end: token.end,
            observed: token.observed.clone(),
            value: token.value,
            symbol: token.symbol,
            form: token.form,
            score,
            reason: "line_start_label",
            repaired: !junk_prefix.is_empty(),
            repair_kind: if junk_prefix.is_empty() {
                ""
            } else {
                "junk_prefix_stripped"
            },
            requires_visual_cue: false,
            flags: CandidateFlags {
                ref_supported,
                ..CandidateFlags::default()
            },
        };
        candidates.push(candidate.clone());
        if token.form == "plain" && token.post.is_empty() && token.observed.chars().count() >= 2 {
            let observed = ScalarText::new(&token.observed);
            for cut in 1..observed.len() {
                let head = observed
                    .slice(0..cut)
                    .expect("candidate offsets must be valid");
                let rest = format!(
                    "{}{}",
                    observed
                        .slice(cut..observed.len())
                        .expect("candidate offsets must be valid"),
                    body
                );
                let Some(head_value) = numeric_value(&head) else {
                    continue;
                };
                if !volume_cite_start(&rest) {
                    continue;
                }
                let mut split = candidate.clone();
                split.end = split.start + cut;
                split.observed = head.to_owned();
                split.value = Some(head_value);
                split.score -= 1.1;
                split.reason = "glued_volume_split";
                split.flags.volume_split = true;
                candidates.push(split);
                break;
            }
        }
    }
    let mut by_value: HashMap<u32, Vec<usize>> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if !candidate.flags.volume_split {
            if let Some(value) = candidate.value {
                by_value.entry(value).or_default().push(index);
            }
        }
    }
    for indexes in by_value.values().filter(|indexes| indexes.len() >= 2) {
        for &index in indexes {
            let candidate = &candidates[index];
            let line = ScalarText::new(&lines[candidate.line].text);
            let continuation = line
                .slice(candidate.start..line.len())
                .expect("candidate offsets must be valid");
            if volume_cite_start(&continuation)
                && indexes.iter().any(|other| {
                    *other != index
                        && lines[candidates[*other].line]
                            .idx
                            .abs_diff(lines[candidate.line].idx)
                            <= 3
                })
            {
                candidates[index].score -= 1.2;
            }
        }
    }
    candidates
}

fn select_backbone(
    candidates: &[Candidate],
    lines: &[PairLine<'_>],
    include_audit: bool,
) -> (Vec<Candidate>, Option<Value>) {
    let numeric = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            Some(NumericSequenceCandidate {
                index,
                value: candidate.value?,
                position: candidate.pos(lines),
                page: lines[candidate.line].page,
                score: candidate.score,
                start_supported: candidate.flags.ref_supported,
            })
        })
        .collect::<Vec<_>>();
    let candidate_count = numeric.len();
    if candidate_count == 0 {
        return (
            Vec::new(),
            include_audit.then(|| json!({"candidate_count": 0})),
        );
    }
    let selected = select_numeric_sequence(numeric, NumericSequencePolicy::FootnoteBackbone);
    let chain = selected
        .indices
        .into_iter()
        .map(|index| candidates[index].clone())
        .collect::<Vec<_>>();
    let diagnostic = include_audit.then(|| {
        json!({
            "candidate_count": candidate_count,
            "selected_count": chain.len(),
            "chain_score": (selected.score * 1_000.0).round_ties_even() / 1_000.0,
            "first_value": chain.first().and_then(|candidate| candidate.value),
            "last_value": chain.last().and_then(|candidate| candidate.value),
        })
    });
    (chain, diagnostic)
}

fn trim_unsupported_tail(mut segment: Vec<Candidate>) -> Vec<Candidate> {
    while segment.len() >= 2 {
        let tail = &segment[segment.len() - 1];
        let prior = &segment[segment.len() - 2];
        let gap = tail
            .value
            .unwrap_or(0)
            .saturating_sub(prior.value.unwrap_or(0));
        if gap > 20 && tail.score < 3.2 && !tail.flags.ref_supported {
            segment.pop();
        } else {
            break;
        }
    }
    segment
}

fn select_segments(
    candidates: &[Candidate],
    lines: &[PairLine<'_>],
    include_audit: bool,
) -> (Vec<Vec<Candidate>>, Option<Value>) {
    let mut remaining: Vec<Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.value.is_some())
        .cloned()
        .collect();
    let mut segments: Vec<Vec<Candidate>> = Vec::new();
    let mut diagnostics = include_audit.then(Vec::new);
    for _ in 0..6 {
        let (chain, diagnostic) = select_backbone(&remaining, lines, include_audit);
        if chain.is_empty() {
            break;
        }
        if !segments.is_empty() {
            let average =
                chain.iter().map(|candidate| candidate.score).sum::<f64>() / chain.len() as f64;
            let first = chain[0].value.unwrap_or(0);
            let span = (
                lines[chain[0].line].idx,
                lines[chain.last().unwrap().line].idx,
            );
            let overlaps = segments.iter().any(|segment| {
                let other = (
                    lines[segment[0].line].idx,
                    lines[segment.last().unwrap().line].idx,
                );
                !(span.1 < other.0 || other.1 < span.0)
            });
            let strong_short = first == 1
                && average >= 3.2
                && (chain.len() >= 2
                    || (lines[chain[0].line].note_sequence_restart
                        && chain[0].flags.ref_supported));
            if (chain.len() < 4 && !strong_short) || average < 2.0 || first > 3 || overlaps {
                break;
            }
        }
        let used: HashSet<usize> = chain.iter().map(|candidate| candidate.line).collect();
        remaining.retain(|candidate| !used.contains(&candidate.line));
        if let (Some(diagnostics), Some(diagnostic)) = (&mut diagnostics, diagnostic) {
            diagnostics.push(diagnostic);
        }
        segments.push(chain);
    }
    segments.sort_by_key(|segment| lines[segment[0].line].idx);
    let count = segments.len();
    (
        segments,
        diagnostics.map(|diagnostics| json!({"segments": diagnostics, "segment_count": count})),
    )
}

fn confusable_variants(value: &str) -> HashSet<u32> {
    if value.is_empty() || value.chars().count() > 3 {
        return HashSet::new();
    }
    let mut result = String::new();
    for character in value.chars() {
        let mapped = normalize_decimal_digit(character).or(match character {
            'l' | 'I' | 'i' | '|' | '!' | 'í' | 't' => Some('1'),
            'o' | 'O' | '°' | 'º' | 'ð' | 'D' | 'Q' => Some('0'),
            's' | 'S' => Some('5'),
            'B' => Some('8'),
            'G' | 'b' => Some('6'),
            'Z' | 'z' => Some('2'),
            'g' | 'q' => Some('9'),
            'A' => Some('4'),
            'T' | '?' | 'n' => Some('7'),
            'J' => Some('3'),
            _ => None,
        });
        let Some(mapped) = mapped else {
            return HashSet::new();
        };
        result.push(mapped);
    }
    result
        .parse::<u32>()
        .ok()
        .filter(|value| (1..=MAX_VALUE).contains(value))
        .into_iter()
        .collect()
}

fn gap_line_score(line: &PairLine<'_>, body: &str) -> f64 {
    let mut score = if line.zone == Zone::Note && !line.region_witness_demoted {
        3.0
    } else {
        0.6
    };
    if line.note_column_fit {
        score += 0.7;
    }
    if cues::layout_has_citation_cue(body) {
        score += 0.7;
    }
    score
}

fn gap_repair_tokens(line: &PairLine<'_>) -> Vec<(String, usize, usize)> {
    let mut tokens = Vec::new();
    if let Some(token) = label_token(&line.text, provider_text(line)) {
        if token.value.is_some() && matches!(token.form, "plain" | "sup") {
            tokens.push((token.observed, token.start, token.end));
        }
    }
    let values = chars(&line.text);
    let lead = values
        .iter()
        .take_while(|value| value.is_whitespace())
        .count();
    let mut end = lead;
    while end < values.len()
        && end - lead < 3
        && (values[end].is_ascii_digit() || "lIi|!íoO°ºDQsSBGbZzgqATnJt?".contains(values[end]))
    {
        end += 1;
    }
    if end > lead {
        let observed: String = values[lead..end].iter().collect();
        let has_digit = observed.chars().any(|value| value.is_ascii_digit());
        let follow_ok = values
            .get(end)
            .is_none_or(|value| value.is_whitespace() || *value == '.');
        if has_digit
            && follow_ok
            && !tokens
                .iter()
                .any(|(_, start, stop)| *start == lead && *stop == end)
        {
            tokens.push((observed, lead, end));
        }
    }
    tokens
}

fn gap_repair_claim(
    line_index: usize,
    line: &PairLine<'_>,
    unclaimed: &HashSet<u32>,
    last_value: u32,
) -> Option<Candidate> {
    for (observed, start, end) in gap_repair_tokens(line) {
        let exact = numeric_value(&observed)
            .filter(|value| unclaimed.contains(value) && *value > last_value);
        let mut variants: Vec<u32> = confusable_variants(&observed)
            .into_iter()
            .filter(|value| unclaimed.contains(value) && *value > last_value)
            .collect();
        variants.sort_unstable();
        let (value, repair_kind) = if let Some(value) = exact {
            (value, "weak_form_promoted")
        } else {
            (variants.first().copied()?, "confusable_value_repair")
        };
        let body = char_slice(&line.text, end, line.text.chars().count())
            .trim_start_matches(|character: char| " .)]".contains(character))
            .to_owned();
        let score = gap_line_score(line, &body);
        if body.chars().count() < 4 || score < 1.2 {
            continue;
        }
        return Some(Candidate {
            line: line_index,
            start,
            end,
            observed,
            value: Some(value),
            symbol: String::new(),
            form: "plain",
            score,
            reason: "sequence_gap_glyph_repair",
            repaired: true,
            repair_kind,
            requires_visual_cue: false,
            flags: CandidateFlags::default(),
        });
    }
    None
}

fn sequence_position_rekey(
    window: &[usize],
    residual: &[u32],
    lines: &[PairLine<'_>],
    used_lines: &mut HashSet<usize>,
) -> Vec<Candidate> {
    if residual.is_empty() || residual.len() > 8 {
        return Vec::new();
    }
    let residual_set: HashSet<u32> = residual.iter().copied().collect();
    let mut qualifying = Vec::<(usize, String, usize, usize)>::new();
    for &line_index in window {
        if used_lines.contains(&line_index) {
            continue;
        }
        let line = &lines[line_index];
        let Some(token) = label_token(&line.text, provider_text(line)) else {
            continue;
        };
        if !matches!(token.form, "plain" | "sup") {
            continue;
        }
        let Some(value) = token.value else {
            continue;
        };
        if residual_set.contains(&value) || value >= residual[0] {
            return Vec::new();
        }
        let body = char_slice(&line.text, token.end, line.text.chars().count())
            .trim_start_matches(|character: char| " .)]".contains(character))
            .to_owned();
        if body.chars().count() < 4 || cues::layout_is_citation_continuation(&body) {
            continue;
        }
        if gap_line_score(line, &body) < 1.2 {
            continue;
        }
        qualifying.push((line_index, token.observed, token.start, token.end));
    }
    if qualifying.len() != residual.len() {
        return Vec::new();
    }
    residual
        .iter()
        .copied()
        .zip(qualifying)
        .map(|(value, (line, observed, start, end))| {
            used_lines.insert(line);
            Candidate {
                line,
                start,
                end,
                observed,
                value: Some(value),
                symbol: String::new(),
                form: "plain",
                score: if lines[line].zone == Zone::Note && !lines[line].region_witness_demoted {
                    3.0
                } else {
                    0.6
                },
                reason: "sequence_position_rekey",
                repaired: true,
                repair_kind: "sequence_position_rekey",
                requires_visual_cue: false,
                flags: CandidateFlags::default(),
            }
        })
        .collect()
}

fn repair_gaps(
    chain: Vec<Candidate>,
    lines: &[PairLine<'_>],
    used_lines: &mut HashSet<usize>,
    include_audit: bool,
) -> (Vec<Candidate>, Option<Vec<Value>>) {
    if chain.is_empty() {
        return (chain, include_audit.then(Vec::new));
    }
    let mut windows = Vec::new();
    let mut holes = include_audit.then(Vec::new);
    if chain[0].value.is_some_and(|value| (2..=9).contains(&value)) {
        windows.push((0, lines[chain[0].line].idx, 1, chain[0].value.unwrap() - 1));
    } else if chain[0].value.is_some_and(|value| value > 9) {
        if let Some(holes) = &mut holes {
            holes.push(json!({
                "values_before_first": chain[0].value.unwrap() - 1,
                "reason": "chain_starts_above_one",
            }));
        }
    }
    for pair in chain.windows(2) {
        let left = pair[0].value.unwrap();
        let right = pair[1].value.unwrap();
        if right > left + 1 {
            windows.push((
                lines[pair[0].line].idx + 1,
                lines[pair[1].line].idx,
                left + 1,
                right - 1,
            ));
        }
    }
    let mut repairs = Vec::new();
    for (start, end, low, high) in windows {
        let window: Vec<usize> = lines
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
            .filter(|(line_index, line)| {
                !used_lines.contains(line_index)
                    && ((line.zone == Zone::Note && !line.region_witness_demoted)
                        || ((line.note_column_fit || line.zone == Zone::Body)
                            && cues::layout_has_citation_cue(&line.text)))
            })
            .map(|(line_index, _)| line_index)
            .collect();
        let mut unclaimed: HashSet<u32> = (low..=high).collect();
        let mut last_value = low.saturating_sub(1);
        let mut claims = Vec::<(usize, Candidate)>::new();
        for (position, &line_index) in window.iter().enumerate() {
            if unclaimed.is_empty() {
                break;
            }
            if let Some(candidate) =
                gap_repair_claim(line_index, &lines[line_index], &unclaimed, last_value)
            {
                let value = candidate.value.expect("gap value");
                claims.push((position, candidate.clone()));
                repairs.push(candidate);
                unclaimed.remove(&value);
                last_value = value;
                used_lines.insert(line_index);
            }
        }
        let mut previous_position = 0;
        let mut previous_value = low.saturating_sub(1);
        let mut intervals = Vec::<(Vec<usize>, u32, u32)>::new();
        for (position, candidate) in claims {
            let value = candidate.value.expect("gap value");
            if value > previous_value + 1 {
                intervals.push((
                    window[previous_position..position].to_vec(),
                    previous_value + 1,
                    value - 1,
                ));
            }
            previous_position = position + 1;
            previous_value = value;
        }
        if previous_value < high {
            intervals.push((
                window[previous_position..].to_vec(),
                previous_value + 1,
                high,
            ));
        }
        for (interval, interval_low, interval_high) in intervals {
            let residual: Vec<u32> = (interval_low..=interval_high)
                .filter(|value| unclaimed.contains(value))
                .collect();
            let rekeyed = sequence_position_rekey(&interval, &residual, lines, used_lines);
            if rekeyed.is_empty() {
                if let Some(holes) = &mut holes {
                    for value in residual {
                        holes.push(json!({
                            "value": value,
                            "reason": "no_confusable_glyph_in_window",
                        }));
                    }
                }
            } else {
                for candidate in &rekeyed {
                    if let Some(value) = candidate.value {
                        unclaimed.remove(&value);
                    }
                }
            }
            repairs.extend(rekeyed);
        }
    }
    let mut result = chain;
    result.extend(repairs);
    result.sort_by_key(|candidate| candidate.value);
    (result, holes)
}

fn note_zone_column_split(lines: &[PairLine<'_>], page: u32) -> Option<f64> {
    let candidates: Vec<&PairLine<'_>> = lines
        .iter()
        .filter(|line| {
            line.page == page
                && line.zone == Zone::Note
                && line.page_width > 0.0
                && line.page_height > 0.0
                && (line.bbox[2] - line.bbox[0]) / line.page_width <= 0.55
        })
        .collect();
    let boxed = lines
        .iter()
        .filter(|line| line.page == page && line.zone == Zone::Note)
        .count();
    if candidates.len() < 6 || boxed == 0 || 1.0 - candidates.len() as f64 / boxed as f64 > 0.40 {
        return None;
    }
    let mut centers: Vec<f64> = candidates
        .iter()
        .map(|line| (line.bbox[0] + line.bbox[2]) / 2.0 / line.page_width)
        .collect();
    centers.sort_by(f64::total_cmp);
    let (left, right) = centers
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .max_by(|left, right| (left.1 - left.0).total_cmp(&(right.1 - right.0)))?;
    let split = (left + right) / 2.0;
    if right - left < 0.12 || !(0.25..=0.75).contains(&split) {
        return None;
    }
    let left_lines: Vec<&&PairLine<'_>> = candidates
        .iter()
        .filter(|line| (line.bbox[0] + line.bbox[2]) / 2.0 / line.page_width < split)
        .collect();
    let right_lines: Vec<&&PairLine<'_>> = candidates
        .iter()
        .filter(|line| (line.bbox[0] + line.bbox[2]) / 2.0 / line.page_width >= split)
        .collect();
    if left_lines.len() < 3 || right_lines.len() < 3 {
        return None;
    }
    let left_y0 = left_lines
        .iter()
        .map(|line| line.bbox[1] / line.page_height)
        .min_by(f64::total_cmp)?;
    let left_y1 = left_lines
        .iter()
        .map(|line| line.bbox[3] / line.page_height)
        .max_by(f64::total_cmp)?;
    let right_y0 = right_lines
        .iter()
        .map(|line| line.bbox[1] / line.page_height)
        .min_by(f64::total_cmp)?;
    let right_y1 = right_lines
        .iter()
        .map(|line| line.bbox[3] / line.page_height)
        .max_by(f64::total_cmp)?;
    let span = left_y1.max(right_y1) - left_y0.min(right_y0);
    let overlap = left_y1.min(right_y1) - left_y0.max(right_y0);
    if span <= 0.0 || overlap.max(0.0) / span < 0.30 {
        return None;
    }
    let left_widths: Vec<f64> = left_lines
        .iter()
        .map(|line| (line.bbox[2] - line.bbox[0]) / line.page_width)
        .collect();
    let right_widths: Vec<f64> = right_lines
        .iter()
        .map(|line| (line.bbox[2] - line.bbox[0]) / line.page_width)
        .collect();
    let (left_width, right_width) = (layout::upper_median(left_widths), layout::upper_median(right_widths));
    let wide = left_width.max(right_width);
    if wide > 0.0 && left_width.min(right_width) / wide < 0.60 {
        None
    } else {
        Some(split)
    }
}

fn recover_out_of_order_labels(
    mut chain: Vec<Candidate>,
    label_candidates: &[Candidate],
    used_lines: &mut HashSet<usize>,
    lines: &[PairLine<'_>],
) -> Vec<Candidate> {
    if chain.len() < 2 {
        return chain;
    }
    let have: HashSet<u32> = chain
        .iter()
        .filter_map(|candidate| candidate.value)
        .collect();
    let Some(low) = have.iter().min().copied() else {
        return chain;
    };
    let high = have.iter().max().copied().unwrap_or(low);
    let min_page = chain
        .iter()
        .map(|candidate| lines[candidate.line].page)
        .min()
        .unwrap_or(0);
    let max_page = chain
        .iter()
        .map(|candidate| lines[candidate.line].page)
        .max()
        .unwrap_or(0);
    let mut by_value: HashMap<u32, Vec<Candidate>> = HashMap::new();
    for candidate in label_candidates {
        let Some(value) = candidate.value else {
            continue;
        };
        let line = &lines[candidate.line];
        let noteish = (line.zone == Zone::Note && !line.region_witness_demoted)
            || ((line.note_column_fit || line.zone == Zone::Body)
                && cues::layout_has_citation_cue(&line.text));
        if candidate.symbol.is_empty()
            && candidate.form == "plain"
            && !have.contains(&value)
            && low < value
            && value < high
            && !used_lines.contains(&candidate.line)
            && (min_page..=max_page).contains(&line.page)
            && noteish
            && candidate.score >= 1.2
        {
            by_value.entry(value).or_default().push(candidate.clone());
        }
    }
    let mut restored = Vec::new();
    for options in by_value.values_mut() {
        options.sort_by(|left, right| right.score.total_cmp(&left.score));
        if options.len() > 1 && options[0].score - options[1].score < 0.3 {
            continue;
        }
        let candidate = options[0].clone();
        let Some(split) = note_zone_column_split(lines, lines[candidate.line].page) else {
            continue;
        };
        let mut page_labels: Vec<&Candidate> = chain
            .iter()
            .filter(|entry| lines[entry.line].page == lines[candidate.line].page)
            .collect();
        page_labels.push(&candidate);
        page_labels.sort_by_key(|entry| entry.value);
        let mut prior: Option<(usize, f64)> = None;
        let consistent = page_labels.into_iter().all(|entry| {
            let line = &lines[entry.line];
            let rank = (
                usize::from((line.bbox[0] + line.bbox[2]) / 2.0 / line.page_width >= split),
                line.bbox[1],
            );
            let valid = prior
                .is_none_or(|value| rank.0 > value.0 || (rank.0 == value.0 && rank.1 > value.1));
            prior = Some(rank);
            valid
        });
        if consistent {
            let mut candidate = candidate;
            candidate.repaired = true;
            candidate.repair_kind = "out_of_order_label";
            used_lines.insert(candidate.line);
            restored.push(candidate);
        }
    }
    chain.extend(restored);
    chain.sort_by_key(|candidate| candidate.value);
    chain
}

fn endnote_mode(chain: &[Candidate], lines: &[PairLine<'_>]) -> bool {
    if chain
        .iter()
        .any(|candidate| lines[candidate.line].note_region_mode == "endnote")
    {
        return true;
    }
    if chain.len() < ENDNOTE_MIN_LABELS {
        return false;
    }
    let max_page = chain
        .iter()
        .map(|candidate| lines[candidate.line].page)
        .max()
        .unwrap_or(0);
    max_page > 1
        && chain
            .iter()
            .filter(|candidate| lines[candidate.line].page as f64 > 0.75 * max_page as f64)
            .count() as f64
            / chain.len() as f64
            >= 0.7
}

fn visual_label_cue(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:diagram|figure|chart|table|graph|map|illustration|image|photo)")
            .unwrap()
    })
    .is_match(text)
}

#[derive(Debug, Clone, Copy)]
struct RefTail {
    value: u32,
    reference: usize,
    parent: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct RefState {
    position: Option<(usize, usize)>,
    score: f64,
    tail: Option<usize>,
}

fn select_refs(
    backbone: &[Candidate],
    refs: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> (HashMap<u32, Candidate>, BTreeMap<String, usize>) {
    let mut drop_reasons = BTreeMap::<String, usize>::new();
    let labels: HashMap<u32, &Candidate> = backbone
        .iter()
        .filter_map(|candidate| candidate.value.map(|value| (value, candidate)))
        .collect();
    let label_spans: HashSet<(usize, usize)> = backbone
        .iter()
        .map(|candidate| (candidate.line, candidate.start))
        .collect();
    let mut candidates_by_value: HashMap<u32, Vec<usize>> = HashMap::new();
    for (index, candidate) in refs.iter().enumerate() {
        let Some(value) = candidate.value else {
            continue;
        };
        if label_spans.contains(&(candidate.line, candidate.start)) {
            *drop_reasons
                .entry("is_selected_label_span".to_owned())
                .or_default() += 1;
            continue;
        }
        let Some(label) = labels.get(&value) else {
            *drop_reasons
                .entry("no_selected_label".to_owned())
                .or_default() += 1;
            continue;
        };
        let table_superscript = candidate.form == "sup"
            && lines[candidate.line]
                .region_type
                .to_lowercase()
                .contains("table");
        if candidate.requires_visual_cue && !table_superscript {
            let label_body = char_slice(
                &lines[label.line].text,
                label.end,
                lines[label.line].text.chars().count(),
            );
            if !visual_label_cue(&label_body) {
                *drop_reasons
                    .entry("visual_zone_without_visual_label_cue".to_owned())
                    .or_default() += 1;
                continue;
            }
        }
        candidates_by_value.entry(value).or_default().push(index);
    }
    let mut values: Vec<u32> = labels.keys().copied().collect();
    values.sort_unstable();
    let max_pool_index = candidates_by_value
        .values()
        .flatten()
        .map(|index| lines[refs[*index].line].idx)
        .max()
        .unwrap_or(0)
        + 1;
    type MatchChoice = (f64, (usize, usize), usize);
    let mut by_value: HashMap<u32, Vec<MatchChoice>> = HashMap::new();
    for value in &values {
        let label = labels[value];
        let mut choices = Vec::new();
        for index in candidates_by_value.get(value).into_iter().flatten() {
            let candidate = &refs[*index];
            let page_delta =
                i64::from(lines[label.line].page) - i64::from(lines[candidate.line].page);
            if !endnote && labels.len() >= ENDNOTE_MIN_LABELS && page_delta.unsigned_abs() >= 2 {
                continue;
            }
            let proximity = if endnote {
                0.08 * lines[candidate.line].idx as f64 / max_pool_index as f64
            } else if page_delta == 0 {
                1.0
            } else if page_delta == 1 {
                0.35
            } else if page_delta == -1 && lines[candidate.line].order <= 10 {
                -0.2
            } else if page_delta < 0 {
                -2.0 - 0.45 * (-page_delta - 1) as f64
            } else {
                -0.45 * (page_delta - 1) as f64
            };
            choices.push((candidate.score + proximity, candidate.pos(lines), *index));
        }
        choices.sort_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        by_value.insert(*value, choices);
    }
    let mut tails = Vec::<RefTail>::new();
    let mut states = vec![RefState {
        position: None,
        score: 0.0,
        tail: None,
    }];
    for value in values {
        let mut next = states.clone();
        for state in &states {
            for (option_score, position, reference) in
                by_value.get(&value).into_iter().flatten().take(6)
            {
                if *option_score <= -1.0
                    || state.position.is_some_and(|previous| *position <= previous)
                {
                    continue;
                }
                let tail = tails.len();
                tails.push(RefTail {
                    value,
                    reference: *reference,
                    parent: state.tail,
                });
                next.push(RefState {
                    position: Some(*position),
                    score: state.score + option_score + 0.5,
                    tail: Some(tail),
                });
            }
        }
        next.sort_by(|left, right| {
            left.position
                .cmp(&right.position)
                .then_with(|| right.score.total_cmp(&left.score))
        });
        let mut best = f64::NEG_INFINITY;
        let mut pruned = Vec::new();
        for state in next {
            if state.score > best + 1e-9 {
                best = state.score;
                pruned.push(state);
            }
        }
        if pruned.len() > 400 {
            pruned.drain(..pruned.len() - 400);
        }
        states = pruned;
    }
    let mut result = HashMap::new();
    let mut final_state = &states[0];
    for state in &states[1..] {
        if state.score > final_state.score {
            final_state = state;
        }
    }
    let mut tail = final_state.tail;
    while let Some(index) = tail {
        let value = tails[index];
        result.insert(value.value, refs[value.reference].clone());
        tail = value.parent;
    }
    for value in labels.keys() {
        if !result.contains_key(value) {
            if let Some(count) = candidates_by_value.get(value).map(Vec::len) {
                *drop_reasons
                    .entry("window_conflict".to_owned())
                    .or_default() += count;
            }
        }
    }
    (result, drop_reasons)
}

fn truncated_value_match(observed: &str, expected: u32) -> bool {
    let Some(digits) = observed
        .chars()
        .map(normalize_decimal_digit)
        .collect::<Option<String>>()
    else {
        return false;
    };
    let expected = expected.to_string();
    digits != expected && (expected.starts_with(&digits) || expected.ends_with(&digits))
}

fn repair_missing_refs(
    chosen: &mut HashMap<u32, Candidate>,
    backbone: &[Candidate],
    refs: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> BTreeMap<String, usize> {
    let mut repair_counts = BTreeMap::<String, usize>::new();
    let labels: HashMap<u32, &Candidate> = backbone
        .iter()
        .filter_map(|candidate| candidate.value.map(|value| (value, candidate)))
        .collect();
    let mut values: Vec<u32> = labels.keys().copied().collect();
    values.sort_unstable();
    let mut taken: HashSet<(usize, usize, usize)> = chosen
        .values()
        .map(|candidate| (candidate.line, candidate.start, candidate.end))
        .collect();
    let label_spans: HashSet<(usize, usize)> = backbone
        .iter()
        .map(|candidate| (candidate.line, candidate.start))
        .collect();
    for (index, value) in values.iter().copied().enumerate() {
        if chosen.contains_key(&value) {
            continue;
        }
        let floor = values[..index]
            .iter()
            .rev()
            .find_map(|prior| chosen.get(prior).map(|candidate| candidate.pos(lines)));
        let ceiling = values[index + 1..]
            .iter()
            .find_map(|next| chosen.get(next).map(|candidate| candidate.pos(lines)));
        let in_window = |candidate: &Candidate| {
            let position = candidate.pos(lines);
            floor.is_none_or(|bound| position > bound)
                && ceiling.is_none_or(|bound| position < bound)
        };
        let label = labels[&value];
        let immediate_window = index > 0
            && index + 1 < values.len()
            && values[index - 1] == value - 1
            && values[index + 1] == value + 1
            && chosen.contains_key(&(value - 1))
            && chosen.contains_key(&(value + 1));
        let substitution_sites: Vec<&Candidate> = if immediate_window {
            refs.iter()
                .filter(|candidate| {
                    candidate.symbol.is_empty()
                        && candidate.value.is_some()
                        && !candidate.requires_visual_cue
                        && !taken.contains(&(candidate.line, candidate.start, candidate.end))
                        && !label_spans.contains(&(candidate.line, candidate.start))
                        && in_window(candidate)
                        && (endnote
                            || lines[label.line].page.abs_diff(lines[candidate.line].page) <= 1)
                        && at_line_end(&lines[candidate.line].text, candidate.end)
                })
                .collect()
        } else {
            Vec::new()
        };
        let sole_substitution = (substitution_sites.len() == 1).then(|| {
            let candidate = substitution_sites[0];
            (candidate.line, candidate.start, candidate.end)
        });
        let mut best: Option<(f64, Candidate, &'static str)> = None;
        for candidate in refs {
            if !candidate.symbol.is_empty()
                || candidate.value.is_none()
                || candidate.requires_visual_cue
                || taken.contains(&(candidate.line, candidate.start, candidate.end))
                || label_spans.contains(&(candidate.line, candidate.start))
                || !in_window(candidate)
                || (!endnote && lines[label.line].page.abs_diff(lines[candidate.line].page) > 1)
            {
                continue;
            }
            let left = candidate
                .start
                .checked_sub(1)
                .and_then(|position| lines[candidate.line].text.chars().nth(position));
            let repair_kind = if candidate.value == Some(value) {
                "window_rescued"
            } else if confusable_variants(&candidate.observed).contains(&value) {
                "confusable_value_repair"
            } else if truncated_value_match(&candidate.observed, value)
                && (chosen.contains_key(&value.saturating_sub(1))
                    || labels.contains_key(&value.saturating_sub(1))
                    || value == values[0])
                && (chosen.contains_key(&(value + 1))
                    || labels.contains_key(&(value + 1))
                    || value == *values.last().unwrap())
                && (at_line_end(&lines[candidate.line].text, candidate.end)
                    || left.is_some_and(|character| "\"'’”)]»".contains(character)))
                && !left.is_some_and(|character| character.is_uppercase())
            {
                "truncated_value_repair"
            } else if sole_substitution == Some((candidate.line, candidate.start, candidate.end)) {
                "sequence_window_substitution_repair"
            } else {
                continue;
            };
            let score = candidate.score - 0.8
                + if lines[candidate.line].page == lines[label.line].page {
                    1.0
                } else {
                    0.0
                };
            if score <= 0.0 || best.as_ref().is_some_and(|prior| prior.0 >= score) {
                continue;
            }
            let mut repaired = candidate.clone();
            if repair_kind != "window_rescued" {
                repaired.value = Some(value);
                repaired.repaired = true;
                repaired.repair_kind = repair_kind;
                repaired.score -= 0.8;
            }
            best = Some((score, repaired, repair_kind));
        }
        if let Some((_, candidate, repair_kind)) = best {
            taken.insert((candidate.line, candidate.start, candidate.end));
            chosen.insert(value, candidate);
            *repair_counts.entry(repair_kind.to_owned()).or_default() += 1;
        }
    }
    repair_counts
}

/// The stray glyphs a line's text could have made of a small raised number: one to three marks
/// glued to the word or punctuation before them and followed by a space or the line's end
/// ("importance.'", "Act,'=", "Trees\""). A lone apostrophe after a letter is elision or a
/// possessive.
fn glyph_sites(text: &[char]) -> Vec<(usize, usize)> {
    const GLYPHS: &str = "'’‘\"”“`°=«";
    let mut sites = Vec::new();
    let mut position = 0;
    while position < text.len() {
        if !GLYPHS.contains(text[position]) {
            position += 1;
            continue;
        }
        let start = position;
        while position < text.len() && position - start < 3 && GLYPHS.contains(text[position]) {
            position += 1;
        }
        let end = position;
        let glued = start
            .checked_sub(1)
            .map(|at| text[at])
            .is_some_and(|character| {
                ".,;:)".contains(character)
                    || (character.is_alphabetic()
                        && !(end - start == 1 && "'’‘".contains(text[start])))
            });
        if glued
            && text
                .get(end)
                .is_none_or(|character| character.is_whitespace())
        {
            sites.push((start, end));
        }
    }
    sites
}

/// Recognition reads a small raised number as a quotation mark, apostrophe or other
/// stray glyph ("importance.'", "Act,'=", "Trees\""). Between two paired references,
/// when the run of labels they skip finds exactly as many such glyphs, in order, glued
/// to the text before them, those glyphs are the run's references.
fn repair_glyph_refs(
    chosen: &mut HashMap<u32, Candidate>,
    backbone: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> usize {
    let label_lines: HashSet<usize> = backbone.iter().map(|candidate| candidate.line).collect();
    let mut values: Vec<u32> = backbone
        .iter()
        .filter_map(|candidate| candidate.value)
        .collect();
    values.sort_unstable();
    values.dedup();
    let mut repaired = 0;
    let mut index = 0;
    while index < values.len() {
        if chosen.contains_key(&values[index]) {
            index += 1;
            continue;
        }
        let first = index;
        while index < values.len() && !chosen.contains_key(&values[index]) {
            index += 1;
        }
        let missing = &values[first..index];
        let (Some(floor_value), Some(&ceiling_value)) = (
            first.checked_sub(1).map(|prior| values[prior]),
            values.get(index),
        ) else {
            continue;
        };
        if missing.len() > 6
            || missing
                .iter()
                .zip(missing.iter().skip(1))
                .any(|(left, right)| right - left != 1)
            || missing[0] != floor_value + 1
            || ceiling_value != missing[missing.len() - 1] + 1
        {
            continue;
        }
        let floor = chosen[&floor_value].pos(lines);
        let ceiling = chosen[&ceiling_value].pos(lines);
        let mut sites = Vec::new();
        for line_index in floor.0..=ceiling.0.min(lines.len().saturating_sub(1)) {
            let line = &lines[line_index];
            if line.zone != Zone::Body || label_lines.contains(&line_index) {
                continue;
            }
            for (start, end) in glyph_sites(&chars(&line.text)) {
                let site = (line_index, start);
                if site > floor && site < ceiling {
                    sites.push((line_index, start, end));
                }
            }
        }
        if sites.len() != missing.len() {
            continue;
        }
        let label_pages: HashMap<u32, u32> = backbone
            .iter()
            .filter_map(|candidate| Some((candidate.value?, lines[candidate.line].page)))
            .collect();
        if !endnote
            && missing
                .iter()
                .zip(&sites)
                .any(|(value, site)| label_pages[value].abs_diff(lines[site.0].page) > 1)
        {
            continue;
        }
        for (&value, &(line, start, end)) in missing.iter().zip(&sites) {
            chosen.insert(
                value,
                Candidate {
                    line,
                    start,
                    end,
                    observed: chars_slice(&chars(&lines[line].text), start, end),
                    value: Some(value),
                    symbol: String::new(),
                    form: "glyph",
                    score: 0.5,
                    reason: "glyph_sequence_marker",
                    repaired: true,
                    repair_kind: "glyph_sequence_repair",
                    requires_visual_cue: false,
                    flags: CandidateFlags::default(),
                },
            );
            repaired += 1;
        }
    }
    repaired
}

/// A footnote's reference stands on its label's page. On each page, the notes with no reference
/// fall between the references paired there (or the page's ends); where such a stretch of the
/// page's text holds exactly as many places a small raised number could have been misread (a
/// stray glyph, a lone letter glued after punctuation as in "Act.s", or a number no note claimed)
/// as it lacks references, those places are the references, in order.
fn repair_page_glyph_refs(
    chosen: &mut HashMap<u32, Candidate>,
    backbone: &[Candidate],
    pool: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> usize {
    if endnote {
        return 0;
    }
    let label_lines: HashSet<usize> = backbone.iter().map(|candidate| candidate.line).collect();
    let mut missing_by_page = BTreeMap::<u32, Vec<u32>>::new();
    for candidate in backbone {
        if let Some(value) = candidate.value.filter(|value| !chosen.contains_key(value)) {
            missing_by_page
                .entry(lines[candidate.line].page)
                .or_default()
                .push(value);
        }
    }
    let taken: HashSet<(usize, usize)> = chosen
        .values()
        .map(|candidate| (candidate.line, candidate.start))
        .collect();
    let mut repaired = 0;
    for (page, mut missing) in missing_by_page {
        missing.sort_unstable();
        missing.dedup();
        let anchors: Vec<(u32, (usize, usize))> = chosen
            .iter()
            .filter(|(_, candidate)| lines[candidate.line].page == page)
            .map(|(value, candidate)| (*value, candidate.pos(lines)))
            .collect();
        // The stretch of the page each missing value's reference must stand in.
        let window = |value: u32| {
            let floor = anchors
                .iter()
                .filter(|(other, _)| *other < value)
                .max_by_key(|(other, _)| *other)
                .map(|(_, position)| *position);
            let ceiling = anchors
                .iter()
                .filter(|(other, _)| *other > value)
                .min_by_key(|(other, _)| *other)
                .map(|(_, position)| *position);
            (floor, ceiling)
        };
        let mut groups = Vec::<((Option<(usize, usize)>, Option<(usize, usize)>), Vec<u32>)>::new();
        for value in missing {
            let bounds = window(value);
            match groups.last_mut() {
                Some((prior, values)) if *prior == bounds => values.push(value),
                _ => groups.push((bounds, vec![value])),
            }
        }
        for ((floor, ceiling), values) in groups {
            let inside = |position: (usize, usize)| {
                floor.is_none_or(|bound| position > bound)
                    && ceiling.is_none_or(|bound| position < bound)
            };
            let mut sites = Vec::<(usize, usize, usize)>::new();
            for (line_index, line) in lines.iter().enumerate() {
                if line.page != page || line.zone != Zone::Body || label_lines.contains(&line_index)
                {
                    continue;
                }
                let text = chars(&line.text);
                let mut found = glyph_sites(&text);
                // A lone letter glued after punctuation and standing before a space.
                for at in 1..text.len() {
                    if ".,;:)".contains(text[at - 1])
                        && text[at].is_alphabetic()
                        && text.get(at + 1).is_none_or(|next| next.is_whitespace())
                        && (at < 2 || !text[at - 2].is_whitespace() && text[at - 2].is_alphabetic())
                    {
                        found.push((at, at + 1));
                    }
                }
                found.extend(
                    pool.iter()
                        .filter(|candidate| {
                            candidate.line == line_index
                                && candidate.value.is_some()
                                && candidate.symbol.is_empty()
                                && !taken.contains(&(candidate.line, candidate.start))
                        })
                        .map(|candidate| (candidate.start, candidate.end)),
                );
                for (start, end) in found {
                    if inside((line.idx, start)) && !taken.contains(&(line_index, start)) {
                        sites.push((line_index, start, end));
                    }
                }
            }
            sites.sort_by_key(|(line, start, _)| (lines[*line].idx, *start));
            sites.dedup_by_key(|(line, start, _)| (*line, *start));
            if sites.len() != values.len() {
                continue;
            }
            for (value, (line, start, end)) in values.into_iter().zip(sites) {
                chosen.insert(
                    value,
                    Candidate {
                        line,
                        start,
                        end,
                        observed: chars_slice(&chars(&lines[line].text), start, end),
                        value: Some(value),
                        symbol: String::new(),
                        form: "glyph",
                        score: 0.5,
                        reason: "page_glyph_marker",
                        repaired: true,
                        repair_kind: "page_glyph_repair",
                        requires_visual_cue: false,
                        flags: CandidateFlags::default(),
                    },
                );
                repaired += 1;
            }
        }
    }
    repaired
}

fn rekey_same_value_ref_runs(
    chosen: &mut HashMap<u32, Candidate>,
    backbone: &[Candidate],
    refs: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> BTreeMap<String, usize> {
    let mut repair_counts = BTreeMap::<String, usize>::new();
    let labels: HashMap<u32, &Candidate> = backbone
        .iter()
        .filter_map(|candidate| candidate.value.map(|value| (value, candidate)))
        .collect();
    let mut values: Vec<u32> = labels.keys().copied().collect();
    values.sort_unstable();
    let mut taken: HashMap<(usize, usize, usize), u32> = chosen
        .iter()
        .map(|(value, candidate)| ((candidate.line, candidate.start, candidate.end), *value))
        .collect();
    for (w_index, w) in values.iter().copied().enumerate() {
        if !chosen.contains_key(&w) {
            continue;
        }
        let mut run = Vec::new();
        let mut cursor = w_index;
        while cursor > 0 {
            let candidate = values[cursor - 1];
            if candidate != w - run.len() as u32 - 1 || chosen.contains_key(&candidate) {
                break;
            }
            run.push(candidate);
            cursor -= 1;
        }
        if run.is_empty() {
            continue;
        }
        run.reverse();
        let floor = if cursor > 0 {
            chosen
                .get(&values[cursor - 1])
                .map(|candidate| candidate.pos(lines))
        } else {
            None
        };
        if floor.is_none() && run[0] != values[0] {
            continue;
        }
        let ceiling = values[w_index + 1..]
            .iter()
            .find_map(|value| chosen.get(value).map(|candidate| candidate.pos(lines)));
        let mut sites = Vec::new();
        let mut valid = true;
        for candidate in refs {
            if candidate.value != Some(w)
                || !candidate.symbol.is_empty()
                || candidate.requires_visual_cue
                || !matches!(candidate.form, "tight" | "sup")
                || candidate.score <= 0.0
                || floor.is_some_and(|bound| candidate.pos(lines) <= bound)
                || ceiling.is_some_and(|bound| candidate.pos(lines) >= bound)
            {
                continue;
            }
            if taken
                .get(&(candidate.line, candidate.start, candidate.end))
                .is_some_and(|owner| *owner != w)
            {
                valid = false;
                break;
            }
            sites.push(candidate.clone());
        }
        if !valid || sites.len() != run.len() + 1 {
            continue;
        }
        sites.sort_by_key(|candidate| candidate.pos(lines));
        let mut targets = run;
        targets.push(w);
        if !endnote
            && targets.iter().zip(&sites).any(|(value, candidate)| {
                lines[labels[value].line]
                    .page
                    .abs_diff(lines[candidate.line].page)
                    > 1
            })
        {
            continue;
        }
        for (value, mut candidate) in targets.into_iter().zip(sites) {
            let key = (candidate.line, candidate.start, candidate.end);
            if chosen
                .get(&value)
                .is_some_and(|selected| (selected.line, selected.start, selected.end) == key)
            {
                continue;
            }
            candidate.repaired = true;
            candidate.repair_kind = "sequence_position_rekey";
            candidate.value = Some(value);
            chosen.insert(value, candidate);
            taken.insert(key, value);
            *repair_counts
                .entry("sequence_position_rekey".to_owned())
                .or_default() += 1;
        }
    }
    repair_counts
}

fn repeated_refs(
    chosen: &HashMap<u32, Candidate>,
    refs: &[Candidate],
    lines: &[PairLine<'_>],
) -> HashMap<u32, Vec<Candidate>> {
    let primary: HashSet<(usize, usize, usize)> = chosen
        .values()
        .map(|candidate| (candidate.line, candidate.start, candidate.end))
        .collect();
    let mut result: HashMap<u32, Vec<Candidate>> = HashMap::new();
    for candidate in refs {
        let Some(value) = candidate.value else {
            continue;
        };
        let Some(selected) = chosen.get(&value) else {
            continue;
        };
        if candidate.form != "sup"
            || primary.contains(&(candidate.line, candidate.start, candidate.end))
            || candidate.pos(lines) <= selected.pos(lines)
        {
            continue;
        }
        result.entry(value).or_default().push(candidate.clone());
    }
    result
}

fn label_only_supported(backbone: &[Candidate], position: usize, lines: &[PairLine<'_>]) -> bool {
    let label = &backbone[position];
    label.zone_is_noteish(lines)
        && (label.score >= 1.5
            || [
                position.checked_sub(1),
                (position + 1 < backbone.len()).then_some(position + 1),
            ]
            .into_iter()
            .flatten()
            .any(|neighbor| {
                let neighbor = &backbone[neighbor];
                label.value.unwrap().abs_diff(neighbor.value.unwrap()) == 1
                    && lines[label.line].page.abs_diff(lines[neighbor.line].page) <= 1
            }))
}

fn build_pairs<'a>(
    pages: &'a [Page],
    protected_citation_spans: &'a BTreeMap<String, Vec<PdfSourceSpan>>,
    include_audit: bool,
) -> (Vec<Pair>, Vec<PairLine<'a>>, Option<Value>) {
    let lines = build_lines(pages, protected_citation_spans);
    // Source page numbers remain physical; introductory symbols belong to this document's start.
    let first_page = pages.iter().map(|page| page.number).min().unwrap_or(1);
    let refs = extract_refs(&lines);
    let labels = extract_labels(&lines, &refs, first_page);
    let (segments, backbone_summary) = select_segments(&labels, &lines, include_audit);
    let mut segments: Vec<Vec<Candidate>> = segments
        .into_iter()
        .map(trim_unsupported_tail)
        .filter(|segment| !segment.is_empty())
        .collect();
    let mut used: HashSet<usize> = segments
        .iter()
        .flatten()
        .map(|candidate| candidate.line)
        .collect();
    let mut sequence_holes = include_audit.then(Vec::<Value>::new);
    for segment in &mut segments {
        let (repaired, holes) =
            repair_gaps(std::mem::take(segment), &lines, &mut used, include_audit);
        if let (Some(sequence_holes), Some(holes)) = (&mut sequence_holes, holes) {
            sequence_holes.extend(holes);
        }
        *segment = recover_out_of_order_labels(repaired, &labels, &mut used, &lines);
    }
    segments.retain(|segment| !segment.is_empty());
    let endnote = segments
        .first()
        .is_some_and(|segment| endnote_mode(segment, &lines));

    let mut pools = Vec::<Vec<Candidate>>::new();
    let mut previous_boundary = None;
    for (segment_index, segment) in segments.iter().enumerate() {
        let last_page = segment
            .last()
            .map(|candidate| lines[candidate.line].page)
            .unwrap_or(0);
        let boundary = if segment_index + 1 == segments.len() {
            usize::MAX
        } else {
            lines
                .iter()
                .filter(|line| line.page <= last_page)
                .map(|line| line.idx)
                .max()
                .unwrap_or(0)
        };
        let label_pages: HashMap<u32, HashSet<u32>> =
            segment.iter().fold(HashMap::new(), |mut pages, candidate| {
                if let Some(value) = candidate.value {
                    pages
                        .entry(value)
                        .or_insert_with(HashSet::new)
                        .insert(lines[candidate.line].page);
                }
                pages
            });
        let pool = refs
            .iter()
            .filter(|candidate| {
                let position = lines[candidate.line].idx;
                previous_boundary.is_none_or(|prior| position > prior)
                    && position <= boundary
                    && (!candidate.flags.paren_ref
                        || candidate.value.is_some_and(|value| {
                            label_pages
                                .get(&value)
                                .is_some_and(|pages| pages.contains(&lines[candidate.line].page))
                        }))
            })
            .cloned()
            .collect();
        pools.push(pool);
        previous_boundary = Some(boundary);
    }

    let mut chosen_by_segment = Vec::<HashMap<u32, Candidate>>::new();
    let mut repeated_by_segment = Vec::<HashMap<u32, Vec<Candidate>>>::new();
    let mut suppressed = HashSet::<usize>::new();
    let mut suppressed_reasons = HashMap::<usize, &'static str>::new();
    let mut numbered_diagnostics = include_audit.then(Vec::<Value>::new);
    let mut ref_drop_reasons = BTreeMap::<String, usize>::new();
    let mut ref_repair_counts = BTreeMap::<String, usize>::new();
    for (segment_index, (segment, pool)) in segments.iter().zip(&pools).enumerate() {
        let (mut chosen, drops) = select_refs(segment, pool, &lines, endnote);
        for (reason, count) in drops {
            *ref_drop_reasons.entry(reason).or_default() += count;
        }
        // The labels' references read as themselves in a third of the run at least: notes, not
        // numbered paragraphs, rows or fields that happen to find a number or two.
        let clean = chosen
            .values()
            .filter(|candidate| !candidate.repaired)
            .count();
        let apparatus = clean >= 3 && 3 * clean >= segment.len();
        for (kind, count) in repair_missing_refs(&mut chosen, segment, pool, &lines, endnote) {
                *ref_repair_counts.entry(kind).or_default() += count;
            }
        for (kind, count) in rekey_same_value_ref_runs(&mut chosen, segment, pool, &lines, endnote) {
                *ref_repair_counts.entry(kind).or_default() += count;
            }
        let glyphs = repair_glyph_refs(&mut chosen, segment, &lines, endnote);
        if glyphs > 0 {
            *ref_repair_counts
                .entry("glyph_sequence_repair".to_owned())
                .or_default() += glyphs;
        }
        if apparatus {
            let glyphs = repair_page_glyph_refs(&mut chosen, segment, pool, &lines, endnote);
            if glyphs > 0 {
                *ref_repair_counts
                    .entry("page_glyph_repair".to_owned())
                    .or_default() += glyphs;
            }
        }

        let paren_count = segment
            .iter()
            .filter(|candidate| candidate.form == "paren")
            .count();
        if !segment.is_empty() && paren_count * 2 >= segment.len() {
            let label_pages: HashMap<u32, HashSet<u32>> =
                segment.iter().fold(HashMap::new(), |mut pages, candidate| {
                    if let Some(value) = candidate.value {
                        pages
                            .entry(value)
                            .or_insert_with(HashSet::new)
                            .insert(lines[candidate.line].page);
                    }
                    pages
                });
            let same_page = chosen.iter().any(|(value, candidate)| {
                label_pages
                    .get(value)
                    .is_some_and(|pages| pages.contains(&lines[candidate.line].page))
            });
            if !same_page {
                suppressed.insert(segment_index);
                suppressed_reasons.insert(segment_index, "paren_list_segment");
            }
        }

        if !suppressed.contains(&segment_index) && segment.len() >= 20 {
            let noteish = segment
                .iter()
                .filter(|candidate| {
                    lines[candidate.line].zone == Zone::Note
                        && !lines[candidate.line].region_witness_demoted
                })
                .count();
            let small = segment
                .iter()
                .filter(|candidate| lines[candidate.line].small_font)
                .count();
            let segment_pages: HashSet<u32> = segment
                .iter()
                .map(|candidate| lines[candidate.line].page)
                .collect();
            let article_pages: HashSet<u32> = lines.iter().map(|line| line.page).collect();
            let fired = chosen.len() <= (segment.len() / 50).max(1)
                && noteish * 5 < segment.len()
                && small * 5 < segment.len()
                && segment_pages.len() * 10 >= article_pages.len() * 6;
            if let Some(diagnostics) = &mut numbered_diagnostics {
                diagnostics.push(json!({
                    "segment_index": segment_index,
                    "labels": segment.len(),
                    "chosen_refs": chosen.len(),
                    "noteish_labels": noteish,
                    "small_font_labels": small,
                    "segment_pages": segment_pages.len(),
                    "article_pages": article_pages.len(),
                    "suppressed": fired,
                }));
            }
            if fired {
                suppressed.insert(segment_index);
                suppressed_reasons.insert(segment_index, "numbered_paragraph_segment");
            }
        }
        repeated_by_segment.push(repeated_refs(&chosen, pool, &lines));
        chosen_by_segment.push(chosen);
    }

    let mut ref_bearing_values = HashSet::<u32>::new();
    for (segment_index, (segment, chosen)) in segments.iter().zip(&chosen_by_segment).enumerate() {
        if !suppressed.contains(&segment_index)
            && !segment.is_empty()
            && chosen.len() * 2 >= segment.len()
        {
            ref_bearing_values.extend(segment.iter().filter_map(|candidate| candidate.value));
        }
    }
    let mut duplicate_diagnostics = include_audit.then(Vec::<Value>::new);
    for (segment_index, (segment, chosen)) in segments.iter().zip(&chosen_by_segment).enumerate() {
        if suppressed.contains(&segment_index) || segment.len() < 10 || !chosen.is_empty() {
            continue;
        }
        let values: HashSet<u32> = segment
            .iter()
            .filter_map(|candidate| candidate.value)
            .collect();
        let segment_pages: HashSet<u32> = segment
            .iter()
            .map(|candidate| lines[candidate.line].page)
            .collect();
        let page_lines = lines
            .iter()
            .filter(|line| segment_pages.contains(&line.page))
            .count();
        let fired = !values.is_empty()
            && segment_pages.len() <= 2
            && values.is_subset(&ref_bearing_values)
            && segment.len() * 5 >= page_lines * 2;
        if let Some(diagnostics) = &mut duplicate_diagnostics {
            diagnostics.push(json!({
                "segment_index": segment_index,
                "labels": segment.len(),
                "segment_pages": segment_pages.len(),
                "page_lines": page_lines,
                "values_covered": values.intersection(&ref_bearing_values).count(),
                "values_total": values.len(),
                "suppressed": fired,
            }));
        }
        if fired {
            suppressed.insert(segment_index);
            suppressed_reasons.insert(segment_index, "duplicate_valueset_zero_ref_segment");
        }
    }

    let mut body_restart_diagnostics = include_audit.then(Vec::<Value>::new);
    for (segment_index, (segment, chosen)) in segments.iter().zip(&chosen_by_segment).enumerate() {
        if segment_index == 0 || suppressed.contains(&segment_index) || segment.len() < 6 {
            continue;
        }
        let values: HashSet<u32> = segment
            .iter()
            .filter_map(|candidate| candidate.value)
            .collect();
        let noteish = segment
            .iter()
            .filter(|candidate| {
                lines[candidate.line].zone == Zone::Note || lines[candidate.line].note_column_fit
            })
            .count();
        let fired = !values.is_empty()
            && values.is_subset(&ref_bearing_values)
            && chosen.len() * 3 <= segment.len()
            && (segment.len() - noteish) * 5 >= segment.len() * 3;
        if let Some(diagnostics) = &mut body_restart_diagnostics {
            diagnostics.push(json!({
                "segment_index": segment_index,
                "labels": segment.len(),
                "chosen_refs": chosen.len(),
                "noteish_labels": noteish,
                "values_covered": values.intersection(&ref_bearing_values).count(),
                "values_total": values.len(),
                "suppressed": fired,
            }));
        }
        if fired {
            suppressed.insert(segment_index);
            suppressed_reasons.insert(segment_index, "body_zone_restart_segment");
        }
    }

    let mut pairs = Vec::new();
    let mut pair_number = 0;
    let mut paired_count = 0;
    let mut skipped = HashMap::<&'static str, usize>::new();
    for (segment_index, ((backbone, chosen), repeated)) in segments
        .iter()
        .zip(&chosen_by_segment)
        .zip(&repeated_by_segment)
        .enumerate()
    {
        if suppressed.contains(&segment_index) {
            *skipped
                .entry(
                    suppressed_reasons
                        .get(&segment_index)
                        .copied()
                        .unwrap_or("paren_list_segment"),
                )
                .or_default() += backbone.len();
            continue;
        }
        for (position, label) in backbone.iter().enumerate() {
            let value = label.value.expect("numeric backbone");
            let primary = chosen.get(&value).cloned();
            if primary.is_none() && !label_only_supported(backbone, position, &lines) {
                *skipped.entry("unsupported_label_only").or_default() += 1;
                continue;
            }
            pair_number += 1;
            let primary_ref = primary
                .as_ref()
                .map(|reference| (reference.line, reference.start, reference.end));
            let mut refs = primary.into_iter().collect::<Vec<_>>();
            if !refs.is_empty() {
                paired_count += 1;
                refs.extend(repeated.get(&value).into_iter().flatten().cloned());
            }
            pairs.push(Pair {
                label: label.clone(),
                refs,
                primary_ref,
                previous_value: position
                    .checked_sub(1)
                    .and_then(|index| backbone[index].value),
                next_value: backbone
                    .get(position + 1)
                    .and_then(|candidate| candidate.value),
                restart_sequence: segment_index + 1,
                // Count the whole run, including labels later omitted as unsupported.
                sequence_labels: backbone.len(),
                endnote,
                pair_id: format!("fnv2-pair-LEGALPDF-document-{pair_number:06}"),
                provenance: if label.repaired && label.repair_kind == "confusable_value_repair" {
                    "article_sequence_gap_glyph_repair"
                } else {
                    "article_sequence_line_start_label"
                }
                .to_owned(),
            });
        }
    }

    let mut used_symbols = HashSet::new();
    for label in labels
        .iter()
        .filter(|candidate| !candidate.symbol.is_empty())
    {
        if label.score < 0.4 {
            continue;
        }
        let mut options: Vec<&Candidate> = refs
            .iter()
            .filter(|reference| {
                reference.symbol == label.symbol
                    && !used_symbols.contains(&reference.pos(&lines))
                    && matches!(
                        i64::from(lines[label.line].page) - i64::from(lines[reference.line].page),
                        0 | 1
                    )
            })
            .collect();
        options.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.pos(&lines).cmp(&right.pos(&lines)))
        });
        let reference = options.first().copied().cloned();
        if reference.is_none()
            && !(lines[label.line].page - first_page < SYMBOL_START_PAGE_LIMIT
                && label.zone_is_noteish(&lines))
        {
            continue;
        }
        pair_number += 1;
        let primary_ref = reference
            .as_ref()
            .map(|candidate| (candidate.line, candidate.start, candidate.end));
        if let Some(reference) = &reference {
            paired_count += 1;
            used_symbols.insert(reference.pos(&lines));
        }
        pairs.push(Pair {
            label: label.clone(),
            sequence_labels: 1,
            refs: reference.into_iter().collect(),
            primary_ref,
            previous_value: None,
            next_value: None,
            restart_sequence: 1,
            endnote: false,
            pair_id: format!("fnv2-pair-LEGALPDF-document-{pair_number:06}"),
            provenance: "article_start_custom_symbol_label".to_owned(),
        });
    }

    pairs.sort_by_key(|pair| pair.label.pos(&lines));
    let summary = include_audit.then(|| {
        let marker_count = pairs.len() + pairs.iter().map(|pair| pair.refs.len()).sum::<usize>();
        let label_only = pairs.len().saturating_sub(paired_count);
        json!({
        "schema_version": "oajd.footnote_pairing_v2.v1",
        "engine": "tools.footnotes.footnote_pairing_v2",
        "line_count": lines.len(),
        "marker_count": marker_count,
        "safe_marker_count": marker_count,
        "role_counts": {
            "fn_label": pairs.len(),
            "fn_ref": pairs.iter().map(|pair| pair.refs.len()).sum::<usize>()
        },
        "pair_count": paired_count,
        "materialized_pair_count": paired_count,
        "materialized_label_only_count": label_only,
        "pair_status_counts": {"paired": paired_count, "label_only": label_only},
        "label_candidate_count": labels.len(),
        "ref_candidate_count": refs.len(),
        "article_footnote_pair_materialization": {
            "materialized_marker_count": marker_count,
            "materialized_pair_count": paired_count,
            "materialized_label_only_count": label_only,
            "endnote_mode": endnote,
            "segment_count": segments.len(),
            "skipped_marker_counts": skipped,
            "numbered_paragraph_guard": numbered_diagnostics.expect("audit enabled"),
            "duplicate_valueset_guard": duplicate_diagnostics.expect("audit enabled"),
            "body_zone_restart_guard": body_restart_diagnostics.expect("audit enabled"),
            "label_backbone": backbone_summary.expect("audit enabled"),
            "sequence_holes": sequence_holes.expect("audit enabled"),
            "monotone_ref_sequence": {"drop_reason_counts": ref_drop_reasons},
            "ref_repair_counts": ref_repair_counts,
        },
        "workflow_stage_summary": {
            "stages": [
                "extract_candidates",
                "label_backbone_dp",
                "gap_glyph_repair",
                "out_of_order_label_recovery",
                "monotone_ref_assignment",
                "ref_glyph_repair",
                "false_positive_suppression",
                "custom_symbol_pairs",
                "materialize"
            ]
        },
        })
    });
    (pairs, lines, summary)
}

fn candidate_key(candidate: &Candidate) -> (usize, usize, usize) {
    (candidate.line, candidate.start, candidate.end)
}

fn marker_confidence(value: f64) -> f64 {
    (value * 1_000.0).round_ties_even() / 1_000.0
}

fn normalized_observed(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter_map(|character| {
            normalize_decimal_digit(character).or_else(|| {
                (!character.is_whitespace()).then_some(normalize_note_symbol(character))
            })
        })
        .collect::<String>()
        .trim_end_matches(['.', ')', ':', ']'])
        .trim_start_matches(['(', '['])
        .to_owned()
}

fn canonical_marker_row(
    candidate: &Candidate,
    lines: &[PairLine<'_>],
    role: &str,
    marker_id: String,
    strategy: &str,
    confidence: f64,
) -> Map<String, Value> {
    let line = &lines[candidate.line];
    let note_id = candidate.note_id();
    let source_note_id = {
        let normalized = normalized_observed(&candidate.observed);
        if normalized.is_empty() {
            note_id.clone()
        } else {
            normalized
        }
    };
    let mut row = Map::new();
    row.insert(
        "schema_version".to_owned(),
        json!("oajd.footnote_pairing_v2_marker.v1"),
    );
    row.insert("marker_id".to_owned(), json!(marker_id));
    row.insert("role".to_owned(), json!(role));
    row.insert("safe_to_use".to_owned(), json!(true));
    row.insert("note_id".to_owned(), json!(note_id));
    row.insert("source_note_id".to_owned(), json!(source_note_id));
    row.insert("selected_text".to_owned(), json!(candidate.observed));
    row.insert("visible_marker_value".to_owned(), json!(candidate.observed));
    row.insert(
        "article_context_note_id_repaired".to_owned(),
        json!(candidate.repaired && candidate.repair_kind == "confusable_value_repair"),
    );
    row.insert(
        "repair_lane_status".to_owned(),
        json!(if candidate.repaired && !candidate.repair_kind.is_empty() {
            format!("repaired_{}", candidate.repair_kind)
        } else {
            "visible_glyph".to_owned()
        }),
    );
    row.insert("image_filename".to_owned(), json!(""));
    row.insert("dataset".to_owned(), json!("LEGALPDF"));
    row.insert("article_id".to_owned(), json!("document"));
    row.insert("pdf_page".to_owned(), json!(line.page));
    row.insert("line_id".to_owned(), json!(line.id));
    row.insert("region_id".to_owned(), json!(line.region_id));
    row.insert("reading_order_index".to_owned(), json!(line.order));
    row.insert("start_offset".to_owned(), json!(candidate.start));
    row.insert("end_offset".to_owned(), json!(candidate.end));
    row.insert("line_text".to_owned(), json!(line.text));
    row.insert("region_type".to_owned(), json!(line.region_type));
    row.insert(
        "line_type".to_owned(),
        json!(if line.region_type == "footnote" {
            "footnote"
        } else {
            "paragraph"
        }),
    );
    row.insert(
        "candidate_confidence".to_owned(),
        json!(marker_confidence(confidence)),
    );
    row.insert("candidate_reason".to_owned(), json!(candidate.reason));
    row.insert("pairing_strategy_family".to_owned(), json!(strategy));
    row.insert(
        "label_sequence_guard_status".to_owned(),
        json!(if role == "fn_label" {
            "passed"
        } else {
            "not_applicable"
        }),
    );
    row.insert("protected_span_guard_status".to_owned(), json!("passed"));
    row.insert(
        "materialization_source".to_owned(),
        json!("tools.footnotes.footnote_pairing_v2"),
    );
    row
}

#[allow(clippy::too_many_arguments)]
fn insert_shared_marker_fields(
    row: &mut Map<String, Value>,
    pair_id: &str,
    note_id: &str,
    label_marker_id: &str,
    ref_marker_ids: &[String],
    status: &str,
    same_page: bool,
    sequence_context: &Value,
) {
    row.insert("materialized_pair_id".to_owned(), json!(pair_id));
    row.insert("materialized_pair_status".to_owned(), json!(status));
    row.insert("materialized_note_id".to_owned(), json!(note_id));
    row.insert(
        "materialized_ref_count".to_owned(),
        json!(ref_marker_ids.len()),
    );
    row.insert("materialized_label_count".to_owned(), json!(1));
    row.insert(
        "materialized_pair_scope".to_owned(),
        json!("full_article_sequence_context"),
    );
    row.insert(
        "materialized_label_marker_id".to_owned(),
        json!(label_marker_id),
    );
    row.insert(
        "materialized_ref_marker_ids".to_owned(),
        json!(ref_marker_ids),
    );
    row.insert(
        "materialized_label_same_page_as_ref".to_owned(),
        json!(same_page),
    );
    row.insert(
        "article_sequence_context".to_owned(),
        sequence_context.clone(),
    );
}

fn pair_markers(pairs: &[(Pair, NotePairSupportV2)], lines: &[PairLine<'_>]) -> Vec<Value> {
    let mut ordered_pairs = pairs.iter().map(|(pair, _)| pair).collect::<Vec<_>>();
    ordered_pairs.sort_by(|left, right| left.pair_id.cmp(&right.pair_id));
    let mut rows = Vec::<Value>::new();
    let mut marker_sequence = 0;
    for pair in ordered_pairs {
        marker_sequence += 1;
        let label_marker_id = format!("fnv2-label-LEGALPDF-document-{marker_sequence:06}");
        let symbol_pair = !pair.label.symbol.is_empty();
        let label_strategy = if symbol_pair {
            "article_start_custom_symbol_label"
        } else if pair.label.repaired && pair.label.repair_kind == "confusable_value_repair" {
            "article_sequence_gap_glyph_repair"
        } else {
            "article_sequence_line_start_label"
        };
        let label_confidence = if symbol_pair {
            0.9
        } else {
            (0.7 + 0.05 * pair.label.score.max(0.0)).min(0.98)
        };
        let mut label_row = canonical_marker_row(
            &pair.label,
            lines,
            "fn_label",
            label_marker_id.clone(),
            label_strategy,
            label_confidence,
        );
        let mut canonical_refs: Vec<&Candidate> = pair
            .refs
            .iter()
            .filter(|candidate| candidate.form != "detached")
            .collect();
        canonical_refs.sort_by_key(|candidate| {
            (
                pair.primary_ref != Some(candidate_key(candidate)),
                candidate.pos(lines),
            )
        });
        let mut ref_rows = Vec::<Map<String, Value>>::new();
        for (ref_index, reference) in canonical_refs.iter().enumerate() {
            marker_sequence += 1;
            let marker_id = format!("fnv2-ref-LEGALPDF-document-{marker_sequence:06}");
            let primary = pair.primary_ref == Some(candidate_key(reference));
            let strategy = if symbol_pair {
                "article_context_custom_marker_ref_same_page_label"
            } else if !primary {
                "article_context_repeated_superscript_ref"
            } else if reference.requires_visual_cue {
                "article_context_visual_region_ref_same_page_label"
            } else if reference.repaired
                && matches!(
                    reference.repair_kind,
                    "confusable_value_repair" | "truncated_value_repair"
                )
            {
                "article_sequence_ref_value_repair"
            } else if lines[reference.line].page == lines[pair.label.line].page {
                "article_context_body_ref_same_page_label_sequence"
            } else {
                "article_context_body_ref_cross_page_label_sequence"
            };
            let confidence = if symbol_pair {
                0.9
            } else if !primary {
                0.85
            } else {
                (0.65 + 0.06 * reference.score.max(0.0)).min(0.98)
            };
            let mut row =
                canonical_marker_row(reference, lines, "fn_ref", marker_id, strategy, confidence);
            row.insert("valid_repeated_ref".to_owned(), json!(ref_index > 0));
            ref_rows.push(row);
        }
        let label_line = &lines[pair.label.line];
        let same_page = ref_rows.first().is_some_and(|row| {
            row.get("pdf_page").and_then(Value::as_u64) == Some(u64::from(label_line.page))
        });
        let sequence_context = if symbol_pair {
            json!({
                "value": pair.label.symbol,
                "selected_label_image_filename": "",
                "selected_label_pdf_page": label_line.page,
                "same_page_as_selected_label": same_page,
            })
        } else {
            json!({
                "value": pair.label.value,
                "previous_value": pair.previous_value,
                "next_value": pair.next_value,
                "endnote_mode": pair.endnote,
                "segment_index": pair.restart_sequence - 1,
                "selected_label_image_filename": "",
                "selected_label_pdf_page": label_line.page,
                "same_page_as_selected_label": same_page,
            })
        };
        let status = if ref_rows.is_empty() {
            "label_only"
        } else {
            "paired"
        };
        let ref_marker_ids: Vec<String> = ref_rows
            .iter()
            .filter_map(|row| {
                row.get("marker_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        insert_shared_marker_fields(
            &mut label_row,
            &pair.pair_id,
            &pair.label.note_id(),
            &label_marker_id,
            &ref_marker_ids,
            status,
            same_page,
            &sequence_context,
        );
        rows.push(Value::Object(label_row));
        for mut row in ref_rows {
            insert_shared_marker_fields(
                &mut row,
                &pair.pair_id,
                &pair.label.note_id(),
                &label_marker_id,
                &ref_marker_ids,
                status,
                same_page,
                &sequence_context,
            );
            rows.push(Value::Object(row));
        }
    }

    let detached_pairs: HashMap<(usize, usize, usize, String), &Pair> = pairs
        .iter()
        .flat_map(|(pair, _)| {
            pair.refs
                .iter()
                .filter(|candidate| candidate.form == "detached")
                .map(move |candidate| {
                    (
                        (
                            candidate.line,
                            candidate.start,
                            candidate.end,
                            candidate.note_id(),
                        ),
                        pair,
                    )
                })
        })
        .collect();
    let mut detached_count = 0;
    for (line_index, line) in lines.iter().enumerate() {
        for detached in line.detached_references.iter() {
            let raw = detached.note_id.as_str();
            let note_id =
                numeric_value(raw).map_or_else(|| normalize_symbol(raw), |n| n.to_string());
            let start = detached.start_offset;
            let end = detached.end_offset;
            let Some(pair) = detached_pairs.get(&(line_index, start, end, note_id.clone())) else {
                continue;
            };
            detached_count += 1;
            rows.push(json!({
                "schema_version": "oajd.footnote_pairing_v2_marker.v1",
                "marker_id": format!("legalpdf-detached-ref-{detached_count:06}"),
                "role": "fn_ref",
                "safe_to_use": true,
                "note_id": note_id,
                "selected_text": detached.selected_text,
                "line_id": line.id,
                "region_id": line.region_id,
                "region_type": line.region_type,
                "pdf_page": line.page,
                "reading_order_index": line.order,
                "start_offset": start,
                "end_offset": end,
                "confidence": 0.84,
                "pairing_strategy_family": "detached_pdf_superscript",
                "materialized_pair_id": pair.pair_id,
                "materialized_note_id": pair.label.note_id(),
                "materialized_pair_status": "paired",
                "restart_sequence": 1,
            }));
        }
    }

    let pair_indexes: HashMap<&str, usize> = pairs
        .iter()
        .enumerate()
        .map(|(index, (pair, _))| (pair.pair_id.as_str(), index))
        .collect();
    let mut grouped_rows = vec![(None, Vec::new()); pairs.len()];
    for (row_index, row) in rows.iter().enumerate() {
        let Some(pair_index) = row
            .get("materialized_pair_id")
            .and_then(Value::as_str)
            .and_then(|pair_id| pair_indexes.get(pair_id))
            .copied()
        else {
            continue;
        };
        match row.get("role").and_then(Value::as_str) {
            Some("fn_label") => {
                grouped_rows[pair_index].0.get_or_insert(row_index);
            }
            Some("fn_ref") => grouped_rows[pair_index].1.push(row_index),
            _ => {}
        }
    }
    for (label_index, ref_indexes) in grouped_rows {
        let Some(label_index) = label_index else {
            continue;
        };
        let label_page = rows[label_index]
            .get("pdf_page")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let status = if ref_indexes.is_empty() {
            "label_only"
        } else {
            "paired"
        };
        let ref_ids: Vec<String> = ref_indexes
            .iter()
            .filter_map(|&index| {
                rows[index]
                    .get("marker_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        let same_page = ref_indexes
            .iter()
            .any(|&index| rows[index].get("pdf_page").and_then(Value::as_u64) == Some(label_page));
        for index in std::iter::once(label_index).chain(ref_indexes) {
            if let Some(row) = rows[index].as_object_mut() {
                row.insert("materialized_pair_status".to_owned(), json!(status));
                row.insert("materialized_ref_count".to_owned(), json!(ref_ids.len()));
                row.insert("materialized_ref_marker_ids".to_owned(), json!(ref_ids));
                row.insert(
                    "materialized_label_same_page_as_ref".to_owned(),
                    json!(same_page),
                );
            }
        }
    }
    rows.sort_by_key(|row| {
        (
            row.get("reading_order_index")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            row.get("start_offset").and_then(Value::as_u64).unwrap_or(0),
        )
    });
    rows
}

fn refresh_summary(summary: &mut Value, markers: &[Value]) {
    let mut role_counts = BTreeMap::<String, usize>::new();
    let mut pair_statuses = BTreeMap::<String, usize>::new();
    let mut marker_statuses = BTreeMap::<String, usize>::new();
    let mut cross_page_ids = HashSet::<String>::new();
    for marker in markers {
        let role = marker
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        *role_counts.entry(role.clone()).or_default() += 1;
        let status = marker
            .get("materialized_pair_status")
            .and_then(Value::as_str)
            .unwrap_or("label_only")
            .to_owned();
        *marker_statuses.entry(status.clone()).or_default() += 1;
        if role == "fn_label" {
            *pair_statuses.entry(status.clone()).or_default() += 1;
            if status == "paired" {
                if let Some(pair_id) = marker.get("materialized_pair_id").and_then(Value::as_str) {
                    if marker
                        .get("materialized_label_same_page_as_ref")
                        .and_then(Value::as_bool)
                        == Some(false)
                    {
                        cross_page_ids.insert(pair_id.to_owned());
                    }
                }
            }
        }
    }
    let paired = pair_statuses.get("paired").copied().unwrap_or(0);
    let label_only = pair_statuses.get("label_only").copied().unwrap_or(0);
    let has_lines = summary
        .get("line_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0;
    summary["dataset"] = json!(if has_lines { "LEGALPDF" } else { "" });
    summary["article_id"] = json!(if has_lines { "document" } else { "" });
    summary["selected_image_count"] = json!(usize::from(has_lines));
    summary["marker_count"] = json!(markers.len());
    summary["safe_marker_count"] = json!(markers.len());
    summary["role_counts"] = json!(role_counts);
    summary["safe_role_counts"] = json!(role_counts);
    summary["pair_count"] = json!(paired);
    summary["pair_status_counts"] = json!(pair_statuses);
    summary["materialized_marker_count"] = json!(markers.len());
    summary["materialized_pair_count"] = json!(paired);
    summary["materialized_label_only_count"] = json!(label_only);
    summary["materialized_marker_status_counts"] = json!(marker_statuses);
    summary["materialized_pair_status_counts"] = json!(pair_statuses);
    summary["article_footnote_pair_materialization"]["materialized_marker_count"] =
        json!(markers.len());
    summary["article_footnote_pair_materialization"]["materialized_pair_count"] = json!(paired);
    summary["article_footnote_pair_materialization"]["materialized_label_only_count"] =
        json!(label_only);
    summary["article_footnote_pair_materialization"]["cross_page_pair_count"] =
        json!(cross_page_ids.len());
    summary["article_footnote_pair_materialization"]["synthesized_label_marker_count"] =
        json!(markers
            .iter()
            .filter(|marker| {
                marker.get("role").and_then(Value::as_str) == Some("fn_label")
                    && marker
                        .get("article_context_note_id_repaired")
                        .and_then(Value::as_bool)
                        == Some(true)
            })
            .count());
    summary["workflow_stage_summary"] = json!({
        "stages": [
            "extract_candidates",
            "label_backbone_dp",
            "gap_glyph_repair",
            "monotone_ref_assignment",
            "custom_symbol_pairs",
            "materialize"
        ]
    });
}

fn materialize(
    pairs: Vec<Pair>,
    lines: &[PairLine<'_>],
    pairing_summary: Option<Value>,
) -> PairingOutput {
    let mut occurrences: HashMap<String, usize> = HashMap::new();
    let continuation_pages: HashSet<u32> = lines
        .iter()
        .filter(|line| line.note_region_mode == "footnote_continuation")
        .map(|line| line.page)
        .collect();
    let mut footnotes = Vec::new();
    let mut diagnostics = Vec::new();
    let mut anchors: HashMap<String, Vec<Anchor>> = HashMap::new();
    let mut pair_claims = Vec::new();
    let observed_pair =
        |pair: &Pair| !pair.label.repaired && pair.refs.iter().any(|reference| !reference.repaired);
    // Count after detached-reference recovery, once per distinct numeric label.
    // The denominator remains the full pre-filter run saved by the selector.
    let mut observed_sequences = HashMap::<(usize, bool), usize>::new();
    let mut note_sequences = HashMap::<(usize, bool), usize>::new();
    for pair in &pairs {
        let line = &lines[pair.label.line];
        if pair.label.value.is_some() && line.zone == Zone::Note && !line.region_witness_demoted {
            *note_sequences
                .entry((pair.restart_sequence, pair.endnote))
                .or_default() += 1;
        }
        if pair.label.value.is_some() && observed_pair(pair) {
            *observed_sequences
                .entry((pair.restart_sequence, pair.endnote))
                .or_default() += 1;
        }
    }
    let pairs = pairs
        .into_iter()
        .filter_map(|pair| {
            let label_line = &lines[pair.label.line];
            let support = NotePairSupportV2 {
                sequence_id: pair.restart_sequence,
                sequence_labels: pair.sequence_labels,
                sequence_observed_pairs: if pair.label.value.is_some() {
                    observed_sequences
                        .get(&(pair.restart_sequence, pair.endnote))
                        .copied()
                        .unwrap_or(0)
                } else {
                    usize::from(observed_pair(&pair))
                },
                sequence_note_labels: if pair.label.value.is_some() {
                    note_sequences
                        .get(&(pair.restart_sequence, pair.endnote))
                        .copied()
                        .unwrap_or(0)
                } else {
                    usize::from(label_line.zone == Zone::Note && !label_line.region_witness_demoted)
                },
                observed_pair: observed_pair(&pair),
                typographic_reference: pair.refs.iter().any(|reference| {
                    !reference.repaired && matches!(reference.form, "sup" | "detached" | "symbol")
                }),
                note_region: label_line.zone == Zone::Note && !label_line.region_witness_demoted,
            };
            support.supports_note().then_some((pair, support))
        })
        .collect::<Vec<_>>();
    let pairing_audit = pairing_summary.map(|mut summary| {
        let markers = pair_markers(&pairs, lines);
        refresh_summary(&mut summary, &markers);
        PdfPairingAudit {
            markers,
            pairing_summary: summary,
        }
    });
    for (index, (pair, support)) in pairs.iter().enumerate() {
        let label_line = &lines[pair.label.line];
        let next = pairs.get(index + 1).map(|(pair, _)| pair);
        let next_page = next.map(|value| lines[value.label.line].page);
        let mut allowed_pages = BTreeSet::new();
        allowed_pages.insert(label_line.page);
        if pair.endnote {
            let last_page =
                next_page.unwrap_or_else(|| lines.last().map_or(label_line.page, |line| line.page));
            allowed_pages.extend(label_line.page..=last_page);
        } else {
            if next_page == Some(label_line.page + 1) {
                allowed_pages.insert(label_line.page + 1);
            }
            let mut continuation = label_line.page + 1;
            while continuation_pages.contains(&continuation) {
                allowed_pages.insert(continuation);
                continuation += 1;
            }
        }
        let stop = next
            .filter(|value| pair.endnote || allowed_pages.contains(&lines[value.label.line].page))
            .map_or(usize::MAX, |value| lines[value.label.line].idx);
        let mut body_lines: Vec<&PairLine<'_>> = lines[label_line.idx..stop.min(lines.len())]
            .iter()
            // The label's own line and the lines of its block are the note's, whatever region
            // the layout called them (a note set as body text or in the footer).
            .filter(|line| {
                allowed_pages.contains(&line.page)
                    && (line.region_type == "footnote"
                        || line.idx == label_line.idx
                        || (!label_line.region_id.is_empty()
                            && line.region_id == label_line.region_id))
            })
            .collect();
        if !pair.endnote && !label_line.region_id.is_empty() {
            let mut accepted = HashSet::from([label_line.region_id.as_str()]);
            for line in &body_lines {
                if line.page == label_line.page
                    && !line.region_id.is_empty()
                    && line.bbox[1] <= label_line.bbox[3]
                    && line.bbox[3] >= label_line.bbox[1]
                {
                    accepted.insert(line.region_id.as_str());
                }
            }
            let mut bounded = Vec::new();
            let mut prior: Option<&PairLine<'_>> = None;
            let mut blocked = false;
            for line in body_lines {
                if line.page != label_line.page {
                    bounded.push(line);
                    continue;
                }
                let mut include = accepted.contains(line.region_id.as_str());
                if !include && !blocked {
                    if let Some(previous) = prior {
                        let height = previous.height().max(1.0);
                        let gap = line.bbox[1] - previous.bbox[3];
                        let previous_size = line_font_size(previous.line);
                        let size = line_font_size(line.line);
                        include = gap <= (3.0_f64).max(height * 0.5)
                            && previous_size > 0.0
                            && (previous_size * 0.75..=previous_size * 1.25).contains(&size);
                    }
                }
                if include {
                    if !line.region_id.is_empty() {
                        accepted.insert(line.region_id.as_str());
                    }
                    prior = Some(line);
                    bounded.push(line);
                } else {
                    blocked = true;
                }
            }
            body_lines = bounded;
        }
        let mut parts = Vec::new();
        let mut source_spans = Vec::new();
        let mut output_position = 0;
        for line in &body_lines {
            let mut text = line.text.as_str();
            if line.id == label_line.id {
                let body_start = label_token(text, provider_text(line))
                    .filter(|token| {
                        token.start == pair.label.start && token.value == pair.label.value
                    })
                    .map_or(pair.label.end, |token| token.match_end);
                text = char_slice(text, body_start, text.chars().count()).trim_start_matches(
                    |character: char| character.is_whitespace() || ".)],:;-".contains(character),
                );
            }
            if text.is_empty() {
                continue;
            }
            if !parts.is_empty() {
                output_position += 1;
            }
            let source_start =
                legal_structure_model::utf16_len(&line.text[..line.text.len() - text.len()]);
            let length = legal_structure_model::utf16_len(text);
            source_spans.push(legal_structure_model::layout::SourceTextSpan {
                start: output_position,
                end: output_position + length,
                line_id: line.id.clone(),
                source_start,
                source_end: source_start + length,
            });
            output_position += length;
            parts.push(text);
        }
        let display = pair.label.note_id();
        let joined = parts.join(" ");
        let leading =
            legal_structure_model::utf16_len(&joined[..joined.len() - joined.trim_start().len()]);
        let body = joined.trim().to_owned();
        let stop = leading + legal_structure_model::utf16_len(&body);
        source_spans = source_spans
            .iter()
            .filter_map(|span| span.retain(leading, stop, 0))
            .collect();
        let body = if body.is_empty() {
            display.clone()
        } else {
            body
        };
        let occurrence = occurrences.entry(display.clone()).or_insert(0);
        *occurrence += 1;
        for reference in &pair.refs {
            let line = &lines[reference.line];
            anchors.entry(line.id.clone()).or_default().push(Anchor {
                pair_id: pair.pair_id.clone(),
                label: display.clone(),
                start: reference.start,
                end: reference.end,
            });
        }
        let primary = pair.refs.first();
        let mut warnings = Vec::new();
        if primary.is_none() {
            warnings.push("label_only".to_owned());
            let mut diagnostic = Diagnostic::warning(
                "FOOTNOTE_UNMATCHED_LABEL",
                format!("Footnote label '{display}' has no paired reference."),
                Some(label_line.page_index),
            );
            diagnostic.line_ids.push(label_line.id.clone());
            diagnostic
                .details
                .insert("pair_id".to_owned(), json!(pair.pair_id));
            diagnostic
                .details
                .insert("label".to_owned(), json!(display));
            diagnostics.push(diagnostic);
        }
        footnotes.push(Footnote {
            source_spans,
            pair_id: pair.pair_id.clone(),
            label: display,
            occurrence: *occurrence,
            restart_sequence: pair.restart_sequence,
            reference_page: primary.map(|reference| lines[reference.line].page),
            body_pages: body_lines
                .iter()
                .map(|line| line.page)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            reference_line_id: primary.map(|reference| lines[reference.line].id.clone()),
            body_line_ids: body_lines.iter().map(|line| line.id.clone()).collect(),
            body,
            sentence_proposition: String::new(),
            passage_since_prior_note: String::new(),
            // The Python materializer reads the marker row's `confidence`
            // field. Canonical pairer rows expose `candidate_confidence`
            // instead, so the established public value is the 0.75 default.
            confidence: 0.75,
            provenance: pair.provenance.clone(),
            warnings,
            crossrefs: Vec::new(),
        });
        pair_claims.push(NotePairClaim {
            pair_id: pair.pair_id.clone(),
            support: support.clone(),
            label: pair.label.note_id(),
            kind: if pair.endnote {
                NotePairKind::Endnote
            } else {
                NotePairKind::Footnote
            },
            label_anchor: SourceAnchor {
                line_id: label_line.id.clone(),
                start: pair.label.start,
                end: pair.label.end,
            },
            reference_anchors: pair
                .refs
                .iter()
                .map(|reference| SourceAnchor {
                    line_id: lines[reference.line].id.clone(),
                    start: reference.start,
                    end: reference.end,
                })
                .collect(),
            body_line_ids: body_lines.iter().map(|line| line.id.clone()).collect(),
        });
    }
    PairingOutput {
        footnotes,
        diagnostics,
        anchors,
        pair_claims,
        pairing_audit,
    }
}


pub fn pair_footnotes(
    pages: &[Page],
    protected_citation_spans: &BTreeMap<String, Vec<PdfSourceSpan>>,
    include_audit: bool,
) -> PairingOutput {
    let (pairs, lines, summary) = build_pairs(pages, protected_citation_spans, include_audit);
    materialize(pairs, &lines, summary)
}
