//! Note pairing's plain-text mode: a text's lines read without geometry, and paired by the same
//! pairer as a PDF's lines. What a PDF's layout tells the pairer, the text's own reading tells it
//! here: the lines from where a note list may open stand in a note zone, the body's opening numbers
//! are no note's labels, and the lines a paragraph count numbers are paragraph labels the pairer may
//! overturn.

use crate::note_pairing::{label_token, pair_footnotes};
use crate::{utf16_len, NodeKind, ScalarRange, ScalarText, StructureNode};
use legal_structure_model::NotePairing;
use legal_citations::cues;
use legal_structure_model::layout::{LineZone, NoteLine, NoteMode, NotePage};
use std::collections::HashSet;

/// A line of a plain text as the text mode reads it: its text and where it starts in the reader's
/// text (in UTF-16), its page, the note band it stands in (none for the body) and whether a
/// paragraph count numbers it.
pub(crate) struct TextLine<'a> {
    pub text: &'a str,
    pub start: usize,
    pub page: usize,
    pub note: Option<NoteMode>,
    pub paragraph_label: bool,
}

/// The notes of a plain text paired with their calls, placed in the reader's text: its lines in
/// order, a provider's flattened text (`provider`) reading its notes as their HTML printed them
/// ("[1]", "Footnote 1"). A citation a line holds is neither label nor call.
pub(crate) fn pair_text_notes(lines: &[TextLine<'_>], provider: bool) -> NotePairing {
    let citations: Vec<Vec<ScalarRange>> = lines
        .iter()
        .map(|line| {
            let value = line.text;
            // (A provider's line is read for citations only where it may hold a note's label or call.)
            if !value.bytes().any(|byte| byte.is_ascii_digit())
                || provider && !value.contains('[') && !value.contains("Footnote ") && label_token(value, true).is_none() { return Vec::new(); }
            let characters = ScalarText::new(value);
            cues::layout_protected_spans(value)
                .into_iter()
                .map(|span| ScalarRange { start: characters.utf16_of_byte(span.start), end: characters.utf16_of_byte(span.end) })
                .collect()
        })
        .collect();
    let note_lines: Vec<NoteLine<'_>> = lines
        .iter()
        .zip(&citations)
        .map(|(line, citations)| NoteLine {
            id: "",
            text: line.text,
            layout: None,
            page: NotePage { index: line.page, number: line.page as u32 + 1, width: 0.0, height: 0.0 },
            zone: line.note.map_or(LineZone::Body, |mode| LineZone::Note(Some(mode))),
            block: None,
            label_suppressed: line.note.is_none(),
            paragraph_label: line.paragraph_label,
            part: 0,
            citations,
        })
        .collect();
    let place = |at: usize, start: usize, end: usize| Some(ScalarRange { start: lines[at].start + start, end: lines[at].start + end });
    let mut pairing = pair_footnotes(&note_lines, provider, &place);
    for note in &mut pairing.notes {
        note.line_ids.clear();
    }
    pairing
}

/// The notes of a text whose note list may open at `note_start` (an offset at a line's start),
/// each a block from its label to its last body line, its note carrying the calls the pairer paired
/// it with. `paragraph_labels` are the starts of the lines a paragraph count numbers.
pub(crate) fn text_note_blocks(
    text: &ScalarText<'_>,
    note_start: usize,
    paragraph_labels: &HashSet<usize>,
) -> Vec<StructureNode> {
    let index = text.lines();
    let lines = index
        .iter()
        .map(|line| TextLine {
            text: &text.value[line[0]..line[1]],
            start: line[2],
            page: 0,
            note: (line[2] >= note_start).then_some(NoteMode::Endnote),
            paragraph_label: paragraph_labels.contains(&line[2]),
        })
        .collect::<Vec<_>>();
    let pairing = pair_text_notes(&lines, true);
    let line_start = |offset: usize| index[index.partition_point(|line| line[2] <= offset) - 1][2];
    // Each paired note: its label, the start of its label's line, its number and its calls.
    let mut notes = pairing
        .notes
        .into_iter()
        .filter_map(|note| {
            let range = note.marker_range?;
            (range.start >= note_start).then(|| (range, line_start(range.start), note.label.unwrap_or_default(), note.calls))
        })
        .collect::<Vec<_>>();
    // The list counts from 1 whether or not its calls survived the flattening: a label the pairer
    // left unpaired is still its note, without a call.
    let paired = notes.iter().map(|(_, line_start, ..)| *line_start).collect::<HashSet<_>>();
    let mut expected = 1;
    for line in index.iter().filter(|line| line[2] >= note_start) {
        let value = &text.value[line[0]..line[1]];
        let lead = value.len() - value.trim_start().len();
        let Some((number, length)) = list_label(&value[lead..]) else { continue };
        if number != expected {
            break;
        }
        expected += 1;
        if !paired.contains(&line[2]) {
            let start = line[2] + utf16_len(&value[..lead]);
            notes.push((ScalarRange { start, end: start + length }, line[2], number.to_string(), Vec::new()));
        }
    }
    notes.sort_by_key(|(range, ..)| range.start);
    let mut blocks = Vec::with_capacity(notes.len());
    for (position, (label_range, _, label, references)) in notes.iter().enumerate() {
        let next = notes
            .get(position + 1)
            .map_or(text.utf16_len(), |(_, line_start, ..)| *line_start);
        let (from, to) = (text.byte_of_utf16(label_range.end), text.byte_of_utf16(next));
        let body = &text.value[from..to];
        let body_range = ScalarRange {
            start: text.utf16_of_byte(from + body.len() - body.trim_start().len()),
            end: text.utf16_of_byte(from + body.trim_end().len()),
        };
        if body_range.start >= body_range.end {
            continue;
        }
        let mut block = crate::labelled_node(NodeKind::Endnote, label.clone(), body_range.start, body_range.end);
        block.marker_range = Some(*label_range);
        block.calls = references.clone();
        blocks.push(block);
    }
    blocks
}

/// A note list's label at a line's start, as a provider prints it, and its length: "[1] …", or a
/// line "Footnote 1".
fn list_label(value: &str) -> Option<(u32, usize)> {
    if let Some(rest) = value.strip_prefix('[') {
        let digits = rest.bytes().take(4).take_while(u8::is_ascii_digit).count();
        return ((1..=3).contains(&digits) && rest.as_bytes().get(digits) == Some(&b']'))
            .then(|| (rest[..digits].parse().unwrap(), digits + 2));
    }
    let number = value.trim_end().strip_prefix("Footnote ")?;
    (!number.is_empty() && number.len() <= 3 && number.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| (number.parse().unwrap(), value.trim_end().len()))
}

/// A journal article's notes and pages, as `pair_numbered_footnotes` reads them.
pub struct NumberedFootnotes {
    /// Each note a Footnote node: its label, body ranges, calls, the numbering's restart it counts in (`series`), its
    /// page (`page_indexes`, among the text's pages, matter before the first marker one where it holds any), and the
    /// sentence its first call marks (property "sentence").
    pub notes: Vec<StructureNode>,
    /// Each page a marker opens, a Page node labelled as the marker prints it, from the marker's next line to the next
    /// marker.
    pub pages: Vec<StructureNode>,
}

impl NumberedFootnotes {
    /// The notes' bodies in `text` one after another, a blank line between, and each note's place there as the
    /// citation engine reads notes: its number and its numbering's restart. A note without a body reads as its label.
    pub fn note_text(&self, text: &str) -> (String, Vec<legal_citations::NoteRange>) {
        let coordinates = ScalarText::new(text);
        let mut notes = String::new();
        let ranges = self.notes.iter().map(|note| {
            if !notes.is_empty() {
                notes.push_str("\n\n");
            }
            let start = notes.len();
            let label = note.label.as_deref().unwrap_or("");
            let body = note.ranges.iter().filter_map(|range| coordinates.slice_utf16(range.start..range.end)).collect::<Vec<_>>().join(" ");
            notes.push_str(if body.trim().is_empty() { label } else { body.trim() });
            legal_citations::NoteRange { number: label.parse().unwrap_or(0), start, end: notes.len(), sequence: note.series.parse().unwrap_or(0), anchor: None }
        }).collect();
        (notes, ranges)
    }
}

/// The sentence of `text` that the reference `own` marks, with the line's reference markers (`anchors`, UTF-16
/// ranges) left out: the sentence holding the marker, or the one it follows. `before` and `after` are the lines the
/// text runs on from and into, with their own markers.
#[cfg(all(feature = "citator", feature = "document-query"))]
fn sentence_at_reference(text: &str, anchors: &[(usize, usize)], own: usize, before: Option<(&str, &[(usize, usize)])>,
    after: Option<(&str, &[(usize, usize)])>) -> Option<String> {
    // Each character of a line, and where it stands in UTF-16.
    let placed = |text: &str| text.chars().scan(0, |unit, character| {
        let at = *unit;
        *unit += character.len_utf16();
        Some((at, character))
    }).collect::<Vec<_>>();
    let marked = |anchors: &[(usize, usize)], at: usize| anchors.iter().any(|&(start, end)| start <= at && at < end);
    let unmarked = |text: &str, anchors: &[(usize, usize)]| placed(text).into_iter()
        .filter(|(at, _)| !marked(anchors, *at))
        .map(|(_, character)| character).collect::<String>();
    let mut clean = before.map(|(text, anchors)| format!("{} ", unmarked(text, anchors).trim_end())).unwrap_or_default();
    let mut at = None;
    for (unit, character) in placed(text) {
        if unit == own { at = Some(utf16_len(&clean)); }
        if !marked(anchors, unit) { clean.push(character); }
    }
    let at = at.unwrap_or_else(|| utf16_len(&clean));
    if let Some((text, anchors)) = after { clean = format!("{} {}", clean.trim_end(), unmarked(text, anchors)); }
    let coordinates = ScalarText::new(&clean);
    let [start, end] = crate::text_units(&clean, true).into_iter().take_while(|[start, _]| *start < at.max(1)).last()?;
    let sentence = coordinates.slice_utf16(start..end)?.trim().to_owned();
    (!sentence.is_empty()).then_some(sentence)
}

#[cfg(not(all(feature = "citator", feature = "document-query")))]
fn sentence_at_reference(_: &str, _: &[(usize, usize)], _: usize, _: Option<(&str, &[(usize, usize)])>, _: Option<(&str, &[(usize, usize)])>)
    -> Option<String> { None }

/// A journal article's flattened text paired by note pairing's plain-text mode: its pages are those its "[page LABEL]"
/// lines open; each page's notes are the run of numbered paragraphs it ends with ("3 See e.g. …"), called from its body
/// by numbers run into the words before them.
pub fn pair_numbered_footnotes(text: &str) -> NumberedFootnotes {
    let coordinates = ScalarText::new(text);
    let index = coordinates.lines();
    let value = |at: usize| &text[index[at][0]..index[at][1]];
    // Each page's label (none for matter before the first marker), its start and its lines, by their places in `index`.
    let mut pages: Vec<(Option<&str>, usize, Vec<usize>)> = vec![(None, 0, Vec::new())];
    for at in 0..index.len() {
        match crate::journal::page_marker(value(at)) {
            Some(label) => pages.push((Some(label), index.get(at + 1).map_or(coordinates.utf16_len(), |next| next[2]), Vec::new())),
            None => pages.last_mut().expect("a page").2.push(at),
        }
    }
    if pages[0].2.iter().all(|&at| value(at).trim().is_empty()) {
        pages.remove(0);
    }
    let mut lines = Vec::with_capacity(index.len());
    // The body's lines in reading order, past blank lines, note lists and page breaks, by their places in `lines`.
    let mut body = Vec::new();
    for (page, (_, _, page_lines)) in pages.iter().enumerate() {
        // The page's notes open at the first of the numbered paragraphs it ends with.
        let mut zone = page_lines.len();
        let mut opens_paragraph = true;
        let mut starts = Vec::new();
        for (position, &at) in page_lines.iter().enumerate() {
            if value(at).trim().is_empty() {
                opens_paragraph = true;
                continue;
            }
            if opens_paragraph {
                starts.push((position, note_label(value(at))));
            }
            opens_paragraph = false;
        }
        for (position, numbered) in starts.iter().rev() {
            if !numbered {
                break;
            }
            zone = *position;
        }
        for (position, &at) in page_lines.iter().enumerate() {
            if position < zone && !value(at).trim().is_empty() {
                body.push(lines.len());
            }
            lines.push(TextLine {
                text: value(at),
                start: index[at][2],
                page,
                note: (position >= zone).then_some(NoteMode::Footnote),
                paragraph_label: false,
            });
        }
    }
    let mut notes = pair_text_notes(&lines, false).notes;
    let line_text = |at: usize| lines[at].text;
    // The line a call stands on, and every note's calls on each line, in the line's UTF-16.
    let line_of = |offset: usize| lines.partition_point(|line| line.start <= offset).checked_sub(1);
    let mut marks = vec![Vec::new(); lines.len()];
    for call in notes.iter().flat_map(|note| &note.calls) {
        if let Some(at) = line_of(call.start) {
            marks[at].push((call.start - lines[at].start, call.end - lines[at].start));
        }
    }
    // A note's proposition: the sentence its first call marks. A line opening in the middle of a sentence (with a
    // lowercase word) continues the body line before it, across a blank line, a note list or a page break.
    let continues = |at: usize| line_text(at).trim_start().chars().next().is_some_and(char::is_lowercase);
    let neighbour = |at: usize, step: isize| {
        let place = body.iter().position(|&line| line == at)?;
        body.get(place.checked_add_signed(step)?).copied()
    };
    let line = |at: usize| (line_text(at), marks[at].as_slice());
    for note in &mut notes {
        let Some(call) = note.calls.first() else { continue };
        let Some(at) = line_of(call.start) else { continue };
        let sentence = sentence_at_reference(line_text(at), &marks[at], call.start - lines[at].start,
            neighbour(at, -1).filter(|_| continues(at)).map(line),
            neighbour(at, 1).filter(|&next| continues(next)).map(line));
        if let Some(sentence) = sentence {
            note.properties.insert("sentence".to_owned(), sentence);
        }
    }
    let pages = pages
        .iter()
        .enumerate()
        .filter_map(|(page, &(label, start, _))| {
            let end = pages.get(page + 1).map_or(coordinates.utf16_len(), |next| next.1);
            Some(crate::labelled_node(NodeKind::Page, label?.to_owned(), start, end))
        })
        .collect();
    NumberedFootnotes { notes, pages }
}

/// Whether a paragraph opens with a journal note's number: "3 See e.g. …", "12. Ibid."
fn note_label(line: &str) -> bool {
    let line = line.trim_start();
    let digits = line.bytes().take(4).take_while(u8::is_ascii_digit).count();
    (1..=3).contains(&digits)
        && line[digits..].starts_with([' ', '\t', '.'])
        && line[digits..].trim_start_matches(['.', ' ', '\t']).chars().next().is_some_and(|character| !character.is_ascii_digit())
}
