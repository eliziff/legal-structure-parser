//! A record's notes beyond the rendition the engine read (each note of it final as the engine read and paired it:
//! `native_markup::courtlistener`): other texts' notes.

use std::collections::{HashMap, HashSet};
use legal_structure_model::ScalarRange;
use legal_structure_model::document::{DocumentStructure, NodeKind, StructureNode};
use legal_structure_model::text::{utf16_len, ScalarText};
use crate::trim_javascript_whitespace as trim_js;
use super::{read_note, reads, word_starts, Kinds, Pending, ScalarCoordinates};
use crate::native_markup::titles::notes_title;
use crate::native_markup::answer_calls;

/// Notes another text of the case prints and this reading lacks: another rendition's, or the head matter's, where a
/// judge's or counsel's note sits ahead of the opinion. One is the same note if a note of ours shares most of its words,
/// whatever a scanner did to the rest; else it is new, and its words join the end of the reading text. Either way a note of
/// ours with no call takes the other's, placed by the words before it. `coordinates` index the other's text; the words a
/// new note brings index ours again, and the new notes answer the calls the engine left by its rules (`answer_calls`).
pub(super) fn supply_notes(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending, other: &DocumentStructure, coordinates: &ScalarText) {
    const SHINGLE: usize = 4;
    // A note the other text's reading made of its numbered paragraphs (`notes_set_as_text`) is that text's guess for
    // calls it could not answer, no note it prints: a list in the opinion, "(1) The Chemical deposit ...", reads so
    // where the rendition lost its notes.
    let printed = |node: &&StructureNode| node.kind == NodeKind::Footnote && node.grammar.as_deref() != Some("paragraphs");
    if !other.nodes.iter().any(|node| printed(&node)) {
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
    for note in other.nodes.iter().filter(printed) {
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
    if structure.text.len() == length {
        return;
    }
    kinds.coordinates = ScalarCoordinates::new(&structure.text);
    let (calls, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut kinds.spans).into_iter().partition(|span| reads(span, "unmatched-call"));
    kinds.spans = rest;
    let opens = kinds.paragraphs.iter().map(|paragraph| paragraph.range.start).collect::<HashSet<_>>();
    let calls = answer_calls(&structure.text, &ScalarText::with_coordinates(&structure.text, &kinds.coordinates), &opens, &mut kinds.notes, calls);
    kinds.spans.extend(calls);
}
