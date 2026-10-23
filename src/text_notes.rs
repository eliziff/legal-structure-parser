//! Note pairing's plain-text mode: a provider's flattened text read as laid-out lines without
//! geometry, and paired by the same pairer as a PDF's lines. What a PDF's layout tells the pairer,
//! the text's own reading tells it here: the lines from where a note list may open stand in a note
//! zone, and the lines a paragraph count numbers are paragraph labels the pairer may overturn.

use crate::note_pairing::pair_footnotes;
use crate::{Block, NodeKind, Note, NoteKindV2, NoteReference, ScalarRange, ScalarText};
use legal_citations::cues;
use legal_structure_model::layout::{Line, Page, PdfSourceSpan};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The notes of a text whose note list may open at `note_start` (a scalar offset at a line's start),
/// each a block from its label to its last body line, its note carrying the calls the pairer paired
/// it with. `paragraph_labels` are the starts of the lines a paragraph count numbers.
pub(crate) fn text_note_blocks(
    text: &ScalarText<'_>,
    note_start: usize,
    paragraph_labels: &HashSet<usize>,
) -> Vec<Block> {
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
                .map(|span| PdfSourceSpan {
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
            bbox: [0.0; 4],
            spans: Vec::new(),
            words: Vec::new(),
            detached_references: Vec::new(),
            ocr: None,
            exclude_from_body: false,
            ink: 0.0,
            weight: 0.0,
            suppress_footnote_label: line[2] < note_start,
            paragraph_label_candidate: paragraph_labels.contains(&line[2]),
            note_region_mode: if line[2] >= note_start {
                "endnote".to_owned()
            } else {
                String::new()
            },
            region_id: String::new(),
            region_type: if line[2] >= note_start {
                "endnote"
            } else {
                "paragraph"
            }
            .to_owned(),
            source: "provider_text".to_owned(),
        });
    }
    let page = Page {
        id: "page-1".to_owned(),
        index: 0,
        number: 1,
        width: 0.0,
        height: 0.0,
        lines,
        regions: Vec::new(),
        source: "provider_text".to_owned(),
        text_quality: 1.0,
        printed_label: None,
        printed_label_source: None,
        printed_label_line_id: None,
    };
    let pairing = pair_footnotes(std::slice::from_ref(&page), &protected, false);
    let at = |id: &str, offset: usize| starts.get(id).map(|(start, _)| start + offset);
    // Each paired note: its label, the start of its label's line, its number and its calls.
    let mut notes = pairing
        .pair_claims
        .iter()
        .filter_map(|claim| {
            let label = &claim.label_anchor;
            let range = ScalarRange {
                start: at(&label.line_id, label.start)?,
                end: at(&label.line_id, label.end)?,
            };
            let references = claim
                .reference_anchors
                .iter()
                .filter_map(|anchor| {
                    Some(ScalarRange {
                        start: at(&anchor.line_id, anchor.start)?,
                        end: at(&anchor.line_id, anchor.end)?,
                    })
                })
                .collect::<Vec<_>>();
            (range.start >= note_start)
                .then(|| (range, starts[label.line_id.as_str()].0, claim.label.clone(), references))
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
        let mut block = Block::labelled(
            NodeKind::Endnote,
            format!("fn{label}"),
            body_range.start,
            body_range.end,
        );
        block.marker_range = Some(*label_range);
        block.note = Some(Note {
            id: String::new(),
            node_id: None,
            kind: NoteKindV2::Endnote,
            label_range: *label_range,
            body_range,
            label_ranges: vec![*label_range],
            body_ranges: vec![body_range],
            primary_reference: references.first().copied(),
            references: references
                .iter()
                .map(|&range| NoteReference {
                    range,
                    anchor: None,
                    page_indexes: Vec::new(),
                    line_ids: Vec::new(),
                })
                .collect(),
        });
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
    pub truncated: bool,
    pub proposition: Option<String>,
    pub passage: Option<String>,
}

pub struct NumberedFootnotePairing {
    pub notes: Vec<PairedFootnote>,
    pub symbol_labels_dropped: usize,
    pub labels_candidates: usize,
    pub labels_selected: usize,
    pub refs_assigned: usize,
    pub ambiguous_sites: usize,
    pub footnote_mode: bool,
    pub crossrefs: usize,
    pub crossrefs_unresolved: usize,
    pub pages: usize,
}

/// A journal article's flattened text paired by note pairing's plain-text mode: its pages are the
/// export's, each opened by its "[page LABEL]" marker in page-map order; each page's notes are the
/// run of numbered paragraphs it ends with ("3 See e.g. …"), called from its body by numbers run
/// into the words before them.
pub fn pair_numbered_footnotes(text: &str, page_labels: &[String]) -> NumberedFootnotePairing {
    let mut pages = Vec::with_capacity(page_labels.len());
    let mut from = 0;
    for label in page_labels {
        let marker = format!("[page {label}]");
        let Some(at) = text[from..].find(&marker).map(|at| from + at) else { continue };
        if from > 0 || !text[..at].trim().is_empty() {
            pages.push(&text[from..at]);
        }
        from = at + marker.len();
    }
    pages.push(&text[from..]);
    let mut built = Vec::with_capacity(pages.len());
    let mut protected = BTreeMap::new();
    let mut order = 0;
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
        for (position, line) in lines.iter().enumerate() {
            let id = format!("p{page_index}l{position}");
            if line.bytes().any(|byte| byte.is_ascii_digit()) {
                let characters = ScalarText::new(line);
                let spans = cues::layout_protected_spans(line)
                    .into_iter()
                    .map(|span| PdfSourceSpan { start: characters.scalar(span.start), end: characters.scalar(span.end) })
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
                bbox: [0.0; 4],
                spans: Vec::new(),
                words: Vec::new(),
                detached_references: Vec::new(),
                ocr: None,
                exclude_from_body: false,
                ink: 0.0,
                weight: 0.0,
                suppress_footnote_label: !note,
                paragraph_label_candidate: false,
                note_region_mode: if note { "footnote".to_owned() } else { String::new() },
                region_id: String::new(),
                region_type: if note { "footnote" } else { "paragraph" }.to_owned(),
                source: "text".to_owned(),
            });
            order += 1;
        }
        built.push(Page {
            id: format!("page-{}", page_index + 1),
            index: page_index,
            number: page_index as u32 + 1,
            width: 0.0,
            height: 0.0,
            lines: page_lines,
            regions: Vec::new(),
            source: "text".to_owned(),
            text_quality: 1.0,
            printed_label: None,
            printed_label_source: None,
            printed_label_line_id: None,
        });
    }
    let pairing = pair_footnotes(&built, &protected, true);
    let summary = pairing.pairing_audit.as_ref().map(|audit| &audit.pairing_summary);
    let count = |name: &str| summary.and_then(|summary| summary[name].as_u64()).unwrap_or(0) as usize;
    let notes = pairing
        .footnotes
        .iter()
        .map(|note| PairedFootnote {
            label: note.label.clone(),
            restart_sequence: note.restart_sequence,
            note_page_index: note.body_pages.first().map_or(0, |page| *page as usize - 1),
            ref_page_index: note.reference_page.map(|page| page as usize - 1),
            body: note.body.clone(),
            truncated: note.warnings.iter().any(|warning| warning.contains("truncat")),
            proposition: Some(note.sentence_proposition.clone()),
            passage: Some(note.passage_since_prior_note.clone()),
        })
        .collect::<Vec<_>>();
    NumberedFootnotePairing {
        symbol_labels_dropped: 0,
        labels_candidates: count("label_candidate_count"),
        labels_selected: notes.len(),
        refs_assigned: notes.iter().filter(|note| note.ref_page_index.is_some()).count(),
        ambiguous_sites: 0,
        footnote_mode: true,
        crossrefs: pairing.footnotes.iter().map(|note| note.crossrefs.len()).sum(),
        crossrefs_unresolved: pairing.footnotes.iter().flat_map(|note| &note.crossrefs).filter(|crossref| !crossref.resolved).count(),
        pages: built.len(),
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
