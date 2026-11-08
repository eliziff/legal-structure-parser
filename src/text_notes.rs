//! Note pairing's plain-text mode: a provider's flattened text read as laid-out lines without
//! geometry, and paired by the same pairer as a PDF's lines. What a PDF's layout tells the pairer,
//! the text's own reading tells it here: the lines from where a note list may open stand in a note
//! zone, and the lines a paragraph count numbers are paragraph labels the pairer may overturn.

use crate::note_pairing::pair_footnotes;
use crate::{NodeKind, ScalarRange, ScalarText, StructureNode};
use legal_citations::cues;
use legal_structure_model::layout::{Line, Page};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The notes of a text whose note list may open at `note_start` (a scalar offset at a line's start),
/// each a block from its label to its last body line, its note carrying the calls the pairer paired
/// it with. `paragraph_labels` are the starts of the lines a paragraph count numbers.
pub(crate) fn text_note_blocks(
    text: &ScalarText<'_>,
    note_start: usize,
    paragraph_labels: &HashSet<usize>,
) -> Vec<StructureNode> {
    let index = text.lines();
    let mut lines = Vec::with_capacity(index.len());
    let mut protected = BTreeMap::new();
    let mut starts = HashMap::with_capacity(index.len());
    for (order, line) in index.iter().enumerate() {
        let value = &text.value[line[0]..line[1]];
        let id = format!("l{order}");
        starts.insert(id.clone(), (line[2], value));
        // The citations a line holds are no note label and no call, as on a PDF's lines. Only a line
        // with a label or a call ("[1]", "Footnote 1") needs them.
        if marked(value) {
            let characters = ScalarText::new(value);
            let spans = cues::layout_protected_spans(value)
                .into_iter()
                .map(|span| ScalarRange {
                    start: characters.scalar(span.start),
                    end: characters.scalar(span.end),
                })
                .collect::<Vec<_>>();
            if !spans.is_empty() {
                protected.insert(id.clone(), spans);
            }
        }
        lines.push(Line {
            id,
            page_index: 0,
            page_number: 1,
            source_index: order,
            reading_order: order,
            block_index: order,
            text: value.to_owned(),
            suppress_footnote_label: line[2] < note_start,
            paragraph_label_candidate: paragraph_labels.contains(&line[2]),
            note_region_mode: if line[2] >= note_start {
                "endnote".to_owned()
            } else {
                String::new()
            },
            region_type: if line[2] >= note_start {
                "endnote"
            } else {
                "paragraph"
            }
            .to_owned(),
            source: "provider_text".to_owned(),
            ..Default::default()
        });
    }
    let page = Page {
        id: "page-1".to_owned(),
        index: 0,
        number: 1,
        lines,
        source: "provider_text".to_owned(),
        text_quality: 1.0,
        ..Default::default()
    };
    let at = |id: &str, offset: usize| starts.get(id).map(|(start, _)| start + offset);
    let place = |id: &str, start: usize, end: usize| Some(ScalarRange { start: at(id, start)?, end: at(id, end)? });
    let pairing = pair_footnotes(std::slice::from_ref(&page), &protected, &|_| 0, &place);
    // Each paired note: its label, the start of its label's line, its number and its calls.
    let mut notes = pairing
        .notes
        .iter()
        .filter_map(|note| {
            let range = note.marker_range?;
            let line_start = starts.get(note.line_ids.first()?.as_str())?.0;
            (range.start >= note_start).then(|| (range, line_start, note.label.as_deref().unwrap_or("").to_owned(), note.calls.clone()))
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
            let start = line[2] + value[..lead].chars().count();
            notes.push((ScalarRange { start, end: start + length }, line[2], number.to_string(), Vec::new()));
        }
    }
    notes.sort_by_key(|(range, ..)| range.start);
    let mut blocks = Vec::with_capacity(notes.len());
    for (position, (label_range, _, label, references)) in notes.iter().enumerate() {
        let next = notes
            .get(position + 1)
            .map_or(text.len(), |(_, line_start, ..)| *line_start);
        let (from, to) = (text.byte(label_range.end), text.byte(next));
        let body = &text.value[from..to];
        let body_range = ScalarRange {
            start: text.scalar(from + body.len() - body.trim_start().len()),
            end: text.scalar(from + body.trim_end().len()),
        };
        if body_range.start >= body_range.end {
            continue;
        }
        let mut block = crate::labelled_node(
            NodeKind::Endnote,
            format!("{label}"),
            body_range.start,
            body_range.end,
        );
        block.marker_range = Some(*label_range);
        block.calls = references.clone();
        blocks.push(block);
    }
    blocks
}

/// Whether a line holds a note label or call as a provider prints them: "[1]" or "Footnote 1".
fn marked(value: &str) -> bool {
    value.contains("Footnote ")
        || value.match_indices('[').any(|(at, _)| {
            let digits = value[at + 1..].bytes().take(4).take_while(u8::is_ascii_digit).count();
            (1..=3).contains(&digits) && value.as_bytes().get(at + 1 + digits) == Some(&b']')
        })
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

pub struct PairedFootnote {
    pub label: String,
    pub restart_sequence: usize,
    pub note_page_index: usize,
    pub ref_page_index: Option<usize>,
    pub body: String,
    pub proposition: Option<String>,
}

pub struct NumberedFootnotePairing {
    pub notes: Vec<PairedFootnote>,
    pub pages: usize,
    /// Each page's printed label, by page index; none for matter before the first marker.
    pub page_labels: Vec<Option<String>>,
}

/// The sentence of `text` that the reference `own` marks, with the line's reference markers (`anchors`, character
/// ranges) left out: the sentence holding the marker, or the one it follows. `before` and `after` are the lines the
/// text runs on from and into, with their own markers.
#[cfg(all(feature = "citator", feature = "document-query"))]
fn sentence_at_reference(text: &str, anchors: &[(usize, usize)], own: usize, before: Option<(&str, &[(usize, usize)])>,
    after: Option<(&str, &[(usize, usize)])>) -> Option<String> {
    let unmarked = |text: &str, anchors: &[(usize, usize)]| text.chars().enumerate()
        .filter(|(index, _)| !anchors.iter().any(|&(start, end)| start <= *index && *index < end))
        .map(|(_, character)| character).collect::<String>();
    let mut clean = before.map(|(text, anchors)| format!("{} ", unmarked(text, anchors).trim_end())).unwrap_or_default();
    let mut at = None;
    for (index, character) in text.chars().enumerate() {
        if index == own { at = Some(clean.encode_utf16().count()); }
        if !anchors.iter().any(|&(start, end)| start <= index && index < end) { clean.push(character); }
    }
    let at = at.unwrap_or_else(|| clean.encode_utf16().count());
    if let Some((text, anchors)) = after { clean = format!("{} {}", clean.trim_end(), unmarked(text, anchors)); }
    let units: Vec<u16> = clean.encode_utf16().collect();
    let [start, end] = crate::text_units(&clean, true).into_iter().take_while(|[start, _]| *start < at.max(1)).last()?;
    let sentence = String::from_utf16_lossy(&units[start..end]).trim().to_owned();
    (!sentence.is_empty()).then_some(sentence)
}

#[cfg(not(all(feature = "citator", feature = "document-query")))]
fn sentence_at_reference(_: &str, _: &[(usize, usize)], _: usize, _: Option<(&str, &[(usize, usize)])>, _: Option<(&str, &[(usize, usize)])>)
    -> Option<String> { None }

/// A journal article's flattened text paired by note pairing's plain-text mode: its pages are the
/// export's, each opened by its "[page LABEL]" marker in page-map order; each page's notes are the
/// run of numbered paragraphs it ends with ("3 See e.g. …"), called from its body by numbers run
/// into the words before them.
pub fn pair_numbered_footnotes(text: &str, page_labels: &[String]) -> NumberedFootnotePairing {
    let mut pages = Vec::with_capacity(page_labels.len());
    let mut page_starts = Vec::with_capacity(page_labels.len());
    let mut labels = Vec::with_capacity(page_labels.len());
    let (mut from, mut current) = (0, None);
    for label in page_labels {
        let marker = format!("[page {label}]");
        let Some(at) = text[from..].find(&marker).map(|at| from + at) else { continue };
        if from > 0 || !text[..at].trim().is_empty() {
            pages.push(&text[from..at]);
            page_starts.push(from);
            labels.push(current.clone());
        }
        from = at + marker.len();
        current = Some(label.clone());
    }
    pages.push(&text[from..]);
    page_starts.push(from);
    labels.push(current);
    let coordinates = ScalarText::new(text);
    // Each line's start in the text (scalars), its length and its page.
    let mut placed = HashMap::<String, (usize, usize, usize)>::new();
    let mut built = Vec::with_capacity(pages.len());
    let mut protected = BTreeMap::new();
    let mut order = 0;
    // Each body line's neighbours in reading order, past blank lines, note lists and page breaks.
    let mut previous_body = HashMap::<String, String>::new();
    let mut next_body = HashMap::<String, String>::new();
    let mut last_body: Option<String> = None;
    for (page_index, page) in pages.iter().enumerate() {
        let lines = page.split('\n').collect::<Vec<_>>();
        // The page's notes open at the first of the numbered paragraphs it ends with.
        let mut zone = lines.len();
        let mut opens_paragraph = true;
        let mut starts = Vec::new();
        for (position, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                opens_paragraph = true;
                continue;
            }
            if opens_paragraph {
                starts.push((position, note_label(line)));
            }
            opens_paragraph = false;
        }
        for (position, numbered) in starts.iter().rev() {
            if !numbered {
                break;
            }
            zone = *position;
        }
        let mut page_lines = Vec::with_capacity(lines.len());
        let mut byte = page_starts[page_index];
        for (position, line) in lines.iter().enumerate() {
            let id = format!("p{page_index}l{position}");
            placed.insert(id.clone(), (coordinates.scalar(byte), line.chars().count(), page_index));
            byte += line.len() + 1;
            if position < zone && !line.trim().is_empty() {
                if let Some(previous) = last_body.replace(id.clone()) {
                    previous_body.insert(id.clone(), previous.clone());
                    next_body.insert(previous, id.clone());
                }
            }
            if line.bytes().any(|byte| byte.is_ascii_digit()) {
                let characters = ScalarText::new(line);
                let spans = cues::layout_protected_spans(line)
                    .into_iter()
                    .map(|span| ScalarRange { start: characters.scalar(span.start), end: characters.scalar(span.end) })
                    .collect::<Vec<_>>();
                if !spans.is_empty() {
                    protected.insert(id.clone(), spans);
                }
            }
            let note = position >= zone;
            page_lines.push(Line {
                id,
                page_index,
                page_number: page_index as u32 + 1,
                source_index: order,
                reading_order: order,
                block_index: order,
                text: (*line).to_owned(),
                suppress_footnote_label: !note,
                note_region_mode: if note { "footnote".to_owned() } else { String::new() },
                region_type: if note { "footnote" } else { "paragraph" }.to_owned(),
                source: "text".to_owned(),
                ..Default::default()
            });
            order += 1;
        }
        built.push(Page {
            id: format!("page-{}", page_index + 1),
            index: page_index,
            number: page_index as u32 + 1,
            lines: page_lines,
            source: "text".to_owned(),
            text_quality: 1.0,
            ..Default::default()
        });
    }
    let place = |id: &str, start: usize, end: usize| placed.get(id).map(|&(at, ..)| ScalarRange { start: at + start, end: at + end });
    let pairing = pair_footnotes(&built, &protected, &|_| 0, &place);
    let line_text: HashMap<&str, &str> = built.iter()
        .flat_map(|page| page.lines.iter().map(|line| (line.id.as_str(), line.text.as_str()))).collect();
    // The line a call stands on, and every note's calls on each line, in the line's characters.
    let line_of = |offset: usize| placed.iter().find(|(_, &(at, length, _))| at <= offset && offset < at + length.max(1)).map(|(id, &(at, _, page))| (id.as_str(), at, page));
    let mut marks = HashMap::<&str, Vec<(usize, usize)>>::new();
    for call in pairing.notes.iter().flat_map(|note| &note.calls) {
        if let Some((id, at, _)) = line_of(call.start) {
            marks.entry(id).or_default().push((call.start - at, call.end - at));
        }
    }
    // A note's proposition: the sentence its first call marks. A line opening in the middle of a sentence (with a
    // lowercase word) continues the body line before it, across a blank line, a note list or a page break.
    let continues = |id: &str| line_text.get(id).and_then(|text| text.trim_start().chars().next()).is_some_and(char::is_lowercase);
    let unmarked: Vec<(usize, usize)> = Vec::new();
    let line = |id: &String| Some((*line_text.get(id.as_str())?, marks.get(id.as_str()).unwrap_or(&unmarked).as_slice()));
    let proposition = |call: &ScalarRange| {
        let (id, at, _) = line_of(call.start)?;
        sentence_at_reference(line_text.get(id)?, marks.get(id)?, call.start - at,
            previous_body.get(id).filter(|_| continues(id)).and_then(line),
            next_body.get(id).filter(|next| continues(next)).and_then(line))
    };
    let notes = pairing
        .notes
        .iter()
        .map(|note| {
            let body = note.ranges.iter().filter_map(|range| coordinates.slice(range.start..range.end)).collect::<Vec<_>>().join(" ");
            let body = body.trim();
            PairedFootnote {
                label: note.label.as_deref().unwrap_or("").to_owned(),
                restart_sequence: note.series.parse().unwrap_or(0),
                note_page_index: note.page_indexes.first().copied().unwrap_or(0),
                ref_page_index: note.calls.first().and_then(|call| line_of(call.start)).map(|(.., page)| page),
                body: if body.is_empty() { note.label.as_deref().unwrap_or("").to_owned() } else { body.to_owned() },
                proposition: note.calls.first().and_then(proposition),
            }
        })
        .collect::<Vec<_>>();
    NumberedFootnotePairing {
        pages: built.len(),
        page_labels: labels,
        notes,
    }
}

/// Whether a paragraph opens with a journal note's number: "3 See e.g. …", "12. Ibid."
fn note_label(line: &str) -> bool {
    let line = line.trim_start();
    let digits = line.bytes().take(4).take_while(u8::is_ascii_digit).count();
    (1..=3).contains(&digits)
        && line[digits..].starts_with([' ', '\t', '.'])
        && line[digits..].trim_start_matches(['.', ' ', '\t']).chars().next().is_some_and(|character| !character.is_ascii_digit())
}
