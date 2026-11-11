//! A record's notes beyond the rendition the engine read (each note of it final as the engine paired it:
//! `native_markup::courtlistener`): other texts' notes, notes set flat in the text, and lists of notes its paragraphs hold.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use legal_structure_model::ScalarRange;
use legal_structure_model::document::{DocumentStructure, NodeKind, StructureNode};
use legal_structure_model::text::{javascript_whitespace, utf16_len, ScalarText};
use regex::Regex;
use crate::trim_javascript_whitespace as trim_js;
use super::{read_note, reads, word_starts, Kinds, Pending, ScalarCoordinates};
use crate::native_markup::titles::{self, notes_title};
use crate::native_markup::{answer_by_label, called, printed_calls};
pub(super) use crate::native_markup::{opens_on_reporter, plain_label};

/// Notes another text of the case prints and this reading lacks: another rendition's, or the head matter's, where a
/// judge's or counsel's note sits ahead of the opinion. One is the same note if a note of ours shares most of its words,
/// whatever a scanner did to the rest; else it is new, and its words join the end of the reading text. Either way a note of
/// ours with no call takes the other's, placed by the words before it. `coordinates` index the other's text; the words a
/// new note brings index ours again.
pub(super) fn supply_notes(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending, other: &DocumentStructure, coordinates: &ScalarText) {
    const SHINGLE: usize = 4;
    if !other.nodes.iter().any(|node| node.kind == NodeKind::Footnote) {
        return;
    }
    let words_of = |text: &str| word_starts(text).into_iter().map(|word| word.0).collect::<HashSet<_>>();
    let held = {
        let ours = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
        kinds.notes.iter().map(|note| {
            note.ranges.iter().filter_map(|range| ours.slice_utf16(range.start..range.end)).map(words_of).fold(HashSet::new(), |mut all, part| { all.extend(part); all })
        }).collect::<Vec<_>>()
    };
    let ours = word_starts(&structure.text);
    let mut ending = HashMap::<String, Option<usize>>::new();
    for (at, window) in ours.windows(SHINGLE).enumerate() {
        ending.entry(super::shingle(window)).and_modify(|place| *place = None).or_insert(Some(at + SHINGLE - 1));
    }
    let placed = |call: ScalarRange| {
        let before = (0..4).find_map(|short| coordinates.slice_utf16((call.start.saturating_sub(120) + short).min(call.start)..call.start))?;
        let theirs = word_starts(before);
        let window = theirs.get(theirs.len().checked_sub(SHINGLE)?..)?;
        let at = ending.get(&window.iter().map(|word| word.0.as_str()).collect::<Vec<_>>().join(" ")).copied().flatten()?;
        let unit = ours[at].1 + utf16_len(&ours[at].0);
        Some(ScalarRange { start: unit, end: unit })
    };
    let mut added = HashSet::new();
    let length = structure.text.len();
    for note in other.nodes.iter().filter(|node| node.kind == NodeKind::Footnote) {
        let label = note.label.clone().unwrap_or_default();
        let body = coordinates.slice_utf16(note.range.start..note.range.end).map(trim_js).unwrap_or("");
        let words = words_of(body);
        if words.is_empty() || notes_title(body) || !added.insert(body.to_owned()) {
            continue;
        }
        let calls = note.calls.iter().filter_map(|call| placed(*call)).collect::<Vec<_>>();
        if let Some(same) = held.iter().position(|ours| words.iter().filter(|word| ours.contains(*word)).count() * 2 > words.len()) {
            if let Some(witness) = pending.witness.as_mut() {
                witness.note_witness[usize::from(kinds.notes[same].calls.is_empty())] += 1;
            }
            if kinds.notes[same].calls.is_empty() {
                if calls.is_empty() && !note.calls.is_empty() {
                    kinds.notes[same].properties.insert("called_outside".to_owned(), String::new());
                }
                kinds.notes[same].calls = calls;
            }
            continue;
        }
        if let Some(witness) = pending.witness.as_mut() {
            witness.note_witness[2] += 1;
        }
        structure.text.push('\n');
        let start = utf16_len(&structure.text);
        structure.text.push_str(body);
        let range = ScalarRange { start, end: start + utf16_len(body) };
        let called_outside = calls.is_empty() && !note.calls.is_empty();
        kinds.notes.push({ let mut note = read_note(label, start, range.end, vec![range], calls); if called_outside { note.properties.insert("called_outside".to_owned(), String::new()); } note });
    }
    if structure.text.len() != length {
        kinds.coordinates = ScalarCoordinates::new(&structure.text);
    }
}

/// Where no note answers the calls the engine left, the numbered paragraphs past the last call that run through their labels
/// are the notes, set as text. Then the notes this reading holds from every text answer the calls by the engine's rules: by
/// label (`native_markup::answer_by_label`), and an uncalled numbered note by a call printed as words (`printed_calls`).
pub(super) fn answer_calls(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    static OPENING: OnceLock<Regex> = OnceLock::new();
    let (calls, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut kinds.spans).into_iter().partition(|span| reads(span, "unmatched-call"));
    kinds.spans = rest;
    let labels = kinds.notes.iter().map(|note| plain_label(note.label.as_deref().unwrap_or("")).to_owned()).collect::<HashSet<_>>();
    let unanswered = calls.iter().filter(|call| !labels.contains(called(call))).collect::<Vec<_>>();
    let mut wanted = unanswered.iter().filter_map(|call| called(call).parse::<u32>().ok()).collect::<Vec<_>>();
    wanted.sort_unstable();
    wanted.dedup();
    // A note list's own markers open their paragraphs: the last call is the last in the text's run.
    let opens = |call: &&&StructureNode| kinds.paragraphs.binary_search_by_key(&call.range.start, |paragraph| paragraph.range.start).is_ok();
    let mut last_call = unanswered.iter().filter(|call| !opens(call)).map(|call| call.range.end).max().unwrap_or(0);
    // A list under a notes title needs no call at all.
    static TITLE: OnceLock<Regex> = OnceLock::new();
    let title = TITLE.get_or_init(|| Regex::new(r"(?mi)^[ \t]*(?:(?:foot|end)?notes?|escolios?)[ \t]*:?[ \t]*$").unwrap()).find_iter(&structure.text).last();
    let titled = title.is_some() && kinds.notes.is_empty();
    if let Some(title) = title.filter(|_| kinds.notes.is_empty()) {
        last_call = utf16_len(&structure.text[..title.end()]);
        if wanted.is_empty() {
            wanted = (1..1000).collect();
        }
    }
    if !wanted.is_empty() {
        let opening = OPENING.get_or_init(|| Regex::new(r"^[\[(]{0,2}(\d{1,3})[\])\.]{0,2}(?:\s|\p{Lu})").unwrap());
        let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
        let mut found = Vec::new();
        let printed_of = |paragraph: &StructureNode| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map(trim_js);
        // A list of kinds.notes stands last, and a list in the opinion may print its numbers first: "(3) Was the conduct ...". Read from
        // the tail back, each note is the last paragraph that prints its number.
        let mut chosen = HashMap::new();
        let mut ceiling = u32::MAX;
        for paragraph in kinds.paragraphs.iter().rev().take_while(|paragraph| paragraph.range.start >= last_call) {
            let number = printed_of(paragraph).and_then(|printed| opening.captures(printed)).and_then(|capture| capture[1].parse::<u32>().ok());
            if let Some(number) = number.filter(|number| *number < ceiling && wanted.contains(number)) {
                chosen.insert(paragraph.range.start, number);
                ceiling = number;
            }
        }
        // The kinds.paragraphs under a note belong to it only where another note closes them, or a kinds.notes title heads the list: past
        // the last note of no list is the opinion, a number having opened a paragraph of it. A paragraph's own mark ends any note.
        let mut under = None;
        let mut closed = false;
        for paragraph in kinds.paragraphs.iter().filter(|paragraph| paragraph.range.start >= last_call) {
            let Some(printed) = printed_of(paragraph) else { continue };
            if let Some(number) = chosen.get(&paragraph.range.start) {
                if let (Some(last), Some(end)) = (found.last_mut(), under.take()) {
                    let last: &mut (u32, usize, usize) = last;
                    last.2 = end;
                }
                found.push((*number, paragraph.range.start, paragraph.range.end));
                closed = false;
            } else if titles::says_paragraph(printed) {
                under = None;
                closed = true;
            } else if !found.is_empty() && !closed {
                under = Some(paragraph.range.end);
            }
        }
        if let (true, Some(last), Some(end)) = (titled, found.last_mut(), under) {
            last.2 = end;
        }
        if found.len() * 2 >= wanted.len().min(found.len() + 1) {
            for (number, start, end) in found {
                kinds.notes.push(read_note(number, start, end, vec![ScalarRange { start, end }], Vec::new()));
            }
            kinds.notes.sort_by_key(|note| note.range.start);
        }
    }
    let calls = answer_by_label(&mut kinds.notes, calls);
    kinds.spans.extend(calls);
    let opens = kinds.paragraphs.iter().map(|paragraph| paragraph.range.start).collect::<HashSet<_>>();
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    printed_calls(&structure.text, &coordinates, &opens, &mut kinds.notes);
}

/// Notes set flat in the text, each under its own "Footnote 1:" or "[Note 1]" (the calls that name them, "[FN1]", are the
/// engine's `printed_calls`). A source's one note round all of them gives way to them.
pub(super) fn flat_notes(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"(?m)^[ \t]*(?:(?:Footnote|FN) ?(\d{1,3}|\*+)(?: ?[:.]|[ \t]+\p{Lu}|[ \t]*$)|\[(?:Note|FN) ?(\d{1,3})\])").unwrap());
    let opened = label.captures_iter(&structure.text).filter_map(|capture| {
        let whole = capture.get(0)?;
        Some((whole.start(), whole.end(), capture.get(1).or_else(|| capture.get(2))?.as_str().to_owned()))
    }).collect::<Vec<_>>();
    if opened.is_empty() {
        return;
    }
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let starts = opened.iter().map(|open| coordinates.utf16_of_byte(open.1)).collect::<Vec<_>>();
    // A note set flat ends with its own paragraph, ahead of the next note or not: the opinion goes on below it. A plain text's
    // lines wrap, so its paragraphs end at a blank line; a paragraph's own mark ends one anywhere. Under a "Footnotes" line
    // the notes are a list of their own, as a slip opinion sets them: each runs to the next.
    static PAST: OnceLock<Regex> = OnceLock::new();
    static BROKEN: OnceLock<Regex> = OnceLock::new();
    static LIST: OnceLock<Regex> = OnceLock::new();
    let past = PAST.get_or_init(|| Regex::new(r"\n[ \t]*\n|\n[ \t]*[\[{(]?¶").unwrap());
    let broken = BROKEN.get_or_init(|| Regex::new(r"\n").unwrap());
    let listed = LIST.get_or_init(|| Regex::new(r"(?m)^[ \t]*Footnotes[ \t]*$").unwrap()).find(&structure.text).map(|head| head.end());
    let closes = if kinds.opinion.rendition == "plain_text.txt" { past } else { broken };
    let stops = opened.iter().enumerate().map(|(index, open)| {
        let limit = opened.get(index + 1).map_or(structure.text.len(), |next| next.0);
        let body = &structure.text[open.1..limit];
        if listed.is_some_and(|head| head <= open.0) {
            return coordinates.utf16_of_byte(open.1 + body.trim_end_matches(javascript_whitespace).len());
        }
        let lead = body.len() - body.trim_start().len();
        coordinates.utf16_of_byte(closes.find(&body[lead..]).map_or(limit, |close| open.1 + lead + close.start()))
    }).collect::<Vec<_>>();
    let found = opened.iter().enumerate().map(|(index, (_, _, number))| (number.clone(), starts[index], stops[index])).collect::<Vec<_>>();
    for (number, start, stop) in found {
        if kinds.notes.iter().any(|note| note.label.as_deref().unwrap_or("") == number && note.range.start <= start && start < note.range.end) {
            continue;
        }
        kinds.notes.retain(|note| !(note.range.start <= start && start < note.range.end));
        kinds.notes.push(read_note(number, start, stop, vec![ScalarRange { start, end: stop }], Vec::new()));
    }
    kinds.notes.sort_by_key(|note| note.range.start);
}
