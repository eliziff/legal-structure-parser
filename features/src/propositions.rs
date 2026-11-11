//! What a footnote's quotations are checked against: the proposition its call stands for in the body text (the ALR
//! Quote Verifier's "quote context"), the passage since the note before it or the sentence its call ends or stands in.
//! Offsets count UTF-16 units, as the host's text does.

use legal_structure::{javascript_whitespace, JS_WHITESPACE_CLASS};
use legal_structure_model::text::normalize_blank_whitespace;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

/// A unit of a brief: its body text or a note's, with the calls of notes it holds (note id, UTF-16 offset).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PropositionUnit {
    pub id: String,
    pub kind: String,
    pub ordinal: f64,
    pub text: String,
    #[serde(default)]
    pub footnote_refs: Vec<(f64, f64)>,
}

/// How much of the text a note's proposition is.
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum QuoteContext {
    Passage,
    Sentence,
}

#[derive(Serialize)]
pub struct PropositionPart {
    /// The unit's place among those given, and the part of its text the proposition holds.
    pub unit: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposition {
    pub footnote_id: u64,
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub parts: Vec<PropositionPart>,
}

/// The body text's propositions, where each body unit starts in it (its units in order, each followed by a line
/// break; none for a note's unit), and every call of a note in it by where it stands.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FootnotePropositions {
    pub propositions: Vec<Proposition>,
    pub body_starts: Vec<Option<usize>>,
    pub anchors: Vec<(u64, usize)>,
}

const CLOSING_DOUBLE: [u16; 3] = ['"' as u16, '\u{201d}' as u16, '\u{bb}' as u16];
const CLOSERS: [u16; 7] = [')' as u16, '"' as u16, '\'' as u16, ']' as u16, '}' as u16, '\u{201d}' as u16, '\u{2019}' as u16];

fn unit_is(text: &[u16], at: isize, wanted: &[char]) -> bool {
    at >= 0 && text.get(at as usize).is_some_and(|&unit| wanted.iter().any(|&c| c as u32 == unit as u32))
}
fn space(text: &[u16], at: isize) -> bool {
    at >= 0 && text.get(at as usize).and_then(|&unit| char::from_u32(unit as u32)).is_some_and(javascript_whitespace)
}
fn digit(text: &[u16], at: isize) -> bool {
    at >= 0 && text.get(at as usize).is_some_and(|unit| (b'0' as u16..=b'9' as u16).contains(unit))
}
fn stop(text: &[u16], at: isize) -> bool {
    unit_is(text, at, &['.', '!', '?'])
}

/// Whether the period ending `value` follows an abbreviation or a single letter standing apart (the ALR verifier's
/// `(?<![\p{L}\p{N}_])(?:Dr|Mr|…|[A-Za-z])\.?$`, case aside).
fn after_abbreviation(value: &str) -> bool {
    static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?i:Dr|Mr|Mrs|Ms|Jr|Sr|Hon|Prof|Rev|St|No|Nos|pp|para|paras|vol|vols|art|arts|pt|ch|cl|sch|sec|ss|ed|eds|e\.g|i\.e|etc|cf|viz|seq|vs|al|Ltd|Inc|Co|Corp|Bros|Assn|Dept|Univ|Intl|Natl|Ct|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sept?|Oct|Nov|Dec|[A-Za-z])$").unwrap());
    static WORD_CHAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}_]$").unwrap());
    let rest = value.strip_suffix('.').unwrap_or(value);
    rest.char_indices().any(|(at, _)| WORD.is_match(&rest[at..])
        && !rest[..at].chars().next_back().is_some_and(|before| WORD_CHAR.is_match(before.encode_utf8(&mut [0; 4]))))
}

fn ends_sentence(text: &[u16], at: isize) -> bool {
    if unit_is(text, at, &['!', '?']) {
        return true;
    }
    unit_is(text, at, &['.'])
        && !(digit(text, at - 1) && digit(text, at + 1))
        && !unit_is(text, at - 1, &['.'])
        && !unit_is(text, at + 1, &['.'])
        && !after_abbreviation(&String::from_utf16_lossy(&text[(at - 12).max(0) as usize..at as usize + 1]))
}

/// The sentence at `position`: a line break ends one too, and a closing quotation mark after its end belongs to it.
fn sentence_bounds(text: &[u16], position: isize) -> (usize, usize) {
    const WINDOW: isize = 1200;
    let length = text.len() as isize;
    let at = position.clamp(0, length);
    let (low, high) = ((at - WINDOW).max(0), (at + WINDOW).min(length));
    let breaks = |index: isize| unit_is(text, index, &['\n']) || ends_sentence(text, index);
    let left = (low..at).rev().find(|&index| breaks(index)).unwrap_or(low - 1);
    let mut start = left + 1;
    if start > 0 && start < length && stop(text, start - 1) && CLOSING_DOUBLE.contains(&text[start as usize]) {
        start += 1;
    }
    while start < high && space(text, start) {
        start += 1;
    }
    let mut end = (at..high).find(|&index| breaks(index)).map_or(high, |index| index + 1);
    while end > 0 && end < length && stop(text, end - 1) && CLOSING_DOUBLE.contains(&text[end as usize]) {
        end += 1;
    }
    (start as usize, end as usize)
}

/// `text.lastIndexOf(needle, from)` and `text.indexOf(needle, from)` for one UTF-16 unit.
fn last_index_of(text: &[u16], needle: u16, from: isize) -> Option<usize> {
    let from = from.min(text.len() as isize - 1).max(0);
    (0..=from).rev().find(|&at| text.get(at as usize) == Some(&needle)).map(|at| at as usize)
}
fn index_of(text: &[u16], needle: u16, from: usize) -> Option<usize> {
    text.get(from..)?.iter().position(|&unit| unit == needle).map(|at| from + at)
}

/// A call after a sentence's end stands for that sentence; anywhere else, for the one it stands in.
fn marker_sentence(text: &[u16], marker: usize, after: usize) -> (usize, usize) {
    let mut at = (marker as isize - 1).max(0).min(text.len() as isize - 1);
    let mut position = marker as isize;
    while at > 0 && space(text, at) {
        at -= 1;
    }
    let closer = |index: isize| index >= 0 && text.get(index as usize).is_some_and(|unit| CLOSERS.contains(unit));
    if stop(text, at) {
        position = at;
    } else if closer(at) {
        let mut before = at;
        while before > 0 && closer(before) {
            before -= 1;
        }
        while before > 0 && space(text, before) {
            before -= 1;
        }
        if stop(text, before) {
            position = before;
        }
    }
    let (mut start, end) = sentence_bounds(text, position);
    // A sentence that starts inside a quotation opened since the last proposition starts where the quotation's does.
    for (open, close) in [('\u{201c}' as u16, '\u{201d}' as u16), ('"' as u16, '"' as u16)] {
        let Some(opened) = last_index_of(text, open, start as isize - 1).filter(|&opened| opened >= after) else {
            continue;
        };
        if index_of(text, close, opened + 1).is_some_and(|closed| closed < start) {
            continue;
        }
        if index_of(text, close, start).is_some_and(|closing| closing < start + 200) {
            start = sentence_bounds(text, (opened as isize - 1).max(0)).0;
            break;
        }
    }
    (start, end)
}

fn safe_integer(value: f64) -> bool {
    value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
}

/// Each note's proposition in the body text: the passage since the call before its one call (the first after the
/// article's Introduction heading), or with `context` sentence, the sentence its call ends or stands in. A note called
/// more than once has none.
pub fn footnote_propositions(units: &[PropositionUnit], context: QuoteContext) -> FootnotePropositions {
    static INTRODUCTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(
        r"(?i)^{ws}*(?:[IVX0-9]{{1,4}}|[A-Z])?[.)]?{ws}*introduction{ws}*$", ws = JS_WHITESPACE_CLASS)).unwrap());
    let mut body = (0..units.len()).filter(|&index| units[index].kind == "body").collect::<Vec<_>>();
    body.sort_by(|&a, &b| units[a].ordinal.total_cmp(&units[b].ordinal).then_with(|| units[a].id.cmp(&units[b].id)));
    let mut text = Vec::<u16>::new();
    let mut body_starts = vec![None; units.len()];
    let mut parts = Vec::<(usize, usize, usize)>::new();
    let mut anchors = Vec::<(u64, usize)>::new();
    let mut introduction = 0;
    for &index in &body {
        let unit = &units[index];
        let own = unit.text.encode_utf16().collect::<Vec<_>>();
        body_starts[index] = Some(text.len());
        parts.push((index, text.len(), own.len()));
        // The first note's passage starts after the article's Introduction heading, past its title and authors.
        if anchors.is_empty() && introduction == 0 && INTRODUCTION.is_match(&unit.text) {
            introduction = text.len() + own.len() + 1;
        }
        for &(id, offset) in &unit.footnote_refs {
            if safe_integer(id) && id > 0.0 && safe_integer(offset) && offset >= 0.0 && offset <= own.len() as f64 {
                anchors.push((id as u64, text.len() + offset as usize));
            }
        }
        text.extend(own);
        text.push('\n' as u16);
    }
    anchors.sort_by_key(|&(id, position)| (position, id));
    let mut counts = HashMap::<u64, usize>::new();
    for &(id, _) in &anchors {
        *counts.entry(id).or_default() += 1;
    }
    let mut propositions = Vec::<Proposition>::new();
    let (mut previous, mut first, mut after) = (0, 0, 0);
    for &(id, position) in &anchors {
        let passage = if previous > 0 { previous } else { introduction.min(position) };
        previous = position;
        while first < parts.len() && parts[first].1 + parts[first].2 <= passage {
            first += 1;
        }
        if counts[&id] != 1 || position <= passage {
            continue;
        }
        let (sentence, end) = match context {
            QuoteContext::Sentence => marker_sentence(&text, position, after),
            QuoteContext::Passage => (passage, position),
        };
        // The first note's sentence, too, starts after the Introduction heading.
        let start = if context == QuoteContext::Sentence && passage > 0 && passage == introduction {
            sentence.max(passage)
        } else {
            sentence
        };
        if context == QuoteContext::Sentence {
            after = end;
        }
        if start >= end {
            continue;
        }
        // A sentence may start before the note before it.
        let mut index = first;
        while index > 0 && index < parts.len() && parts[index].1 > start {
            index -= 1;
        }
        let mut covered = Vec::new();
        while index < parts.len() && parts[index].1 < end {
            let (unit, offset, length) = parts[index];
            let (from, to) = (start.saturating_sub(offset), length.min(end.saturating_sub(offset)));
            if from < to {
                covered.push(PropositionPart { unit, start: from, end: to });
            }
            index += 1;
        }
        // The note's one proposition: a later call of the same id never reaches here.
        propositions.push(Proposition { footnote_id: id,
            text: normalize_blank_whitespace(&String::from_utf16_lossy(&text[start..end])), start, end, parts: covered });
    }
    FootnotePropositions { propositions, body_starts, anchors }
}
