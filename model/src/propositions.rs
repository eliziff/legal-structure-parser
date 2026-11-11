//! What a note's call stands for in the body text: the sentence the call ends or stands in, and the passage since the
//! call before it. The one reading of a note's proposition, for a brief's units and a PDF's alike. Offsets count UTF-16
//! units, as the hosts' texts do.

use legal_text::{javascript_whitespace, normalize_blank_whitespace, JS_WHITESPACE_CLASS};
use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

/// A note's proposition: the sentence and the passage, each a range of the joined body text and its words.
pub struct NoteProposition {
    pub note: u64,
    pub sentence: (usize, usize),
    pub passage: (usize, usize),
    pub sentence_text: String,
    pub passage_text: String,
}

/// The body text the units make (each followed by a line break), where each unit starts in it, each call of a note by
/// where it stands, and each note's proposition. A note called more than once has none.
pub struct Propositions {
    pub text: Vec<u16>,
    pub unit_starts: Vec<usize>,
    pub calls: Vec<(u64, usize)>,
    pub notes: Vec<NoteProposition>,
}

const CLOSING_DOUBLE: [u16; 3] = ['"' as u16, '\u{201d}' as u16, '\u{bb}' as u16];
const CLOSERS: [u16; 7] = [')' as u16, '"' as u16, '\'' as u16, ']' as u16, '}' as u16, '\u{201d}' as u16, '\u{2019}' as u16];
const NOTE_MARK: &str = "\u{27e6}FN:";

fn at(text: &[u16], index: isize) -> Option<u16> {
    (index >= 0).then(|| text.get(index as usize).copied()).flatten()
}
fn is(text: &[u16], index: isize, wanted: &[char]) -> bool {
    at(text, index).is_some_and(|unit| wanted.iter().any(|&c| c as u32 == unit as u32))
}
fn space(text: &[u16], index: isize) -> bool {
    at(text, index).and_then(|unit| char::from_u32(unit as u32)).is_some_and(javascript_whitespace)
}
fn digit(text: &[u16], index: isize) -> bool {
    at(text, index).is_some_and(|unit| (b'0' as u16..=b'9' as u16).contains(&unit))
}
fn stop(text: &[u16], index: isize) -> bool {
    is(text, index, &['.', '!', '?'])
}

/// Whether the period ending `value` follows an abbreviation or a letter standing alone.
fn after_abbreviation(value: &str) -> bool {
    static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?i:Dr|Mr|Mrs|Ms|Jr|Sr|Hon|Prof|Rev|St|No|Nos|pp|para|paras|vol|vols|art|arts|pt|ch|cl|sch|sec|ss|ed|eds|e\.g|i\.e|etc|cf|viz|seq|vs|al|Ltd|Inc|Co|Corp|Bros|Assn|Dept|Univ|Intl|Natl|Ct|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sept?|Oct|Nov|Dec|[A-Za-z])$").unwrap());
    let rest = value.strip_suffix('.').unwrap_or(value);
    rest.char_indices().any(|(at, _)| WORD.is_match(&rest[at..])
        && !rest[..at].chars().next_back().is_some_and(|before| before.is_alphanumeric() || before == '_'))
}

/// Whether a sentence ends at `index`: a line break, or a stop that is no decimal point, ellipsis or abbreviation's,
/// with what closes it (quotation marks, brackets) followed by a space, the text's end or a note's mark.
fn ends_sentence(text: &[u16], index: isize) -> bool {
    if is(text, index, &['\n']) {
        return true;
    }
    if !stop(text, index) || (is(text, index, &['.'])
        && ((digit(text, index - 1) && digit(text, index + 1)) || is(text, index - 1, &['.']) || is(text, index + 1, &['.'])
            || after_abbreviation(&String::from_utf16_lossy(&text[(index - 12).max(0) as usize..index as usize + 1]))))
    {
        return false;
    }
    let mut next = index + 1;
    while at(text, next).is_some_and(|unit| CLOSERS.contains(&unit)) {
        next += 1;
    }
    at(text, next).is_none() || space(text, next) || text[next as usize..].starts_with(&NOTE_MARK.encode_utf16().collect::<Vec<_>>())
}

/// The sentence at `position`; a closing quotation mark after its stop belongs to it.
fn sentence_bounds(text: &[u16], position: isize) -> (usize, usize) {
    const WINDOW: isize = 1200;
    let length = text.len() as isize;
    let position = position.clamp(0, length);
    let (low, high) = ((position - WINDOW).max(0), (position + WINDOW).min(length));
    let mut start = (low..position).rev().find(|&index| ends_sentence(text, index)).unwrap_or(low - 1) + 1;
    if start > 0 && start < length && stop(text, start - 1) && CLOSING_DOUBLE.contains(&text[start as usize]) {
        start += 1;
    }
    while start < high && space(text, start) {
        start += 1;
    }
    let mut end = (position..high).find(|&index| ends_sentence(text, index)).map_or(high, |index| index + 1);
    while end > 0 && end < length && stop(text, end - 1) && CLOSING_DOUBLE.contains(&text[end as usize]) {
        end += 1;
    }
    (start as usize, end as usize)
}

/// The sentence a call ends (it follows a stop and what closes it) or stands in; a sentence that opens inside a
/// quotation opened since `after` starts where the quotation's sentence does.
fn call_sentence(text: &[u16], call: usize, after: usize) -> (usize, usize) {
    let mut index = (call as isize - 1).max(0).min(text.len() as isize - 1);
    while index > 0 && space(text, index) {
        index -= 1;
    }
    let mut position = call as isize;
    let closer = |index: isize| at(text, index).is_some_and(|unit| CLOSERS.contains(&unit));
    let mut before = index;
    while before > 0 && closer(before) {
        before -= 1;
    }
    while before > 0 && before != index && space(text, before) {
        before -= 1;
    }
    if stop(text, before) {
        position = before;
    }
    let (mut start, end) = sentence_bounds(text, position);
    for (open, close) in [('\u{201c}' as u16, '\u{201d}' as u16), ('"' as u16, '"' as u16)] {
        let opened = text[..start.min(text.len())].iter().rposition(|&unit| unit == open).filter(|&opened| opened >= after);
        let Some(opened) = opened else { continue };
        let closed = |from: usize| text.get(from..).and_then(|rest| rest.iter().position(|&unit| unit == close)).map(|at| from + at);
        if closed(opened + 1).is_some_and(|closed| closed < start) {
            continue;
        }
        if closed(start).is_some_and(|closing| closing < start + 200) {
            start = sentence_bounds(text, (opened as isize - 1).max(0)).0;
            break;
        }
    }
    (start, end)
}

/// Each note's proposition in the body units (`calls`: each a unit's place, the note it calls and its UTF-16 offset in
/// the unit). The first note's passage, and its sentence, start after an article's Introduction heading.
pub fn propositions(units: &[&str], calls: &[(usize, u64, usize)]) -> Propositions {
    static INTRODUCTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(
        r"(?i)^{ws}*(?:[IVX0-9]{{1,4}}|[A-Z])?[.)]?{ws}*introduction{ws}*$", ws = JS_WHITESPACE_CLASS)).unwrap());
    let (mut text, mut unit_starts, mut introduction) = (Vec::<u16>::new(), Vec::new(), 0);
    let mut calls_at = Vec::new();
    for (index, unit) in units.iter().enumerate() {
        let own = unit.encode_utf16().collect::<Vec<_>>();
        if introduction == 0 && !calls.iter().any(|call| call.0 < index) && INTRODUCTION.is_match(unit) {
            introduction = text.len() + own.len() + 1;
        }
        unit_starts.push(text.len());
        calls_at.extend(calls.iter().filter(|call| call.0 == index && call.2 <= own.len()).map(|&(_, note, offset)| (note, text.len() + offset)));
        text.extend(own);
        text.push('\n' as u16);
    }
    calls_at.sort_by_key(|&(note, position)| (position, note));
    let mut counts = HashMap::<u64, usize>::new();
    calls_at.iter().for_each(|&(note, _)| *counts.entry(note).or_default() += 1);
    let read = |(start, end): (usize, usize)| normalize_blank_whitespace(&String::from_utf16_lossy(&text[start..end]));
    let (mut notes, mut previous, mut after) = (Vec::new(), 0, 0);
    for &(note, position) in &calls_at {
        let passage_start = if previous > 0 { previous } else { introduction.min(position) };
        previous = position;
        if counts[&note] != 1 || position <= passage_start {
            continue;
        }
        let (mut start, end) = call_sentence(&text, position, after);
        after = end;
        if passage_start > 0 && passage_start == introduction {
            start = start.max(passage_start);
        }
        let (sentence, passage) = ((start, end.max(start)), (passage_start, position));
        notes.push(NoteProposition { note, sentence, passage, sentence_text: read(sentence), passage_text: read(passage) });
    }
    Propositions { text, unit_starts, calls: calls_at, notes }
}
