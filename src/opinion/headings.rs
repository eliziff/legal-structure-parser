//! What is a heading: titles by their words, set apart, run into a paragraph, or printed with a page.

use std::sync::OnceLock;
use legal_structure_model::document::DocumentStructure;
use legal_structure_model::text::{utf16_len, ScalarText};
use regex::Regex;
use super::pages::page_number;
use super::{read_heading, read_paragraph, Kinds};
use crate::native_markup::titles::sentences;

/// Sections numbered in the run of the paragraphs, title and text in one block with no markup: "II. The plea of estoppel is
/// not well founded. The fact is ...". Roman numerals running on from block to block say what they are; each title is its
/// block's first sentence.
pub(super) fn enumerated_titles(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    const TITLE_WORDS: usize = 16;
    static OPENING: OnceLock<Regex> = OnceLock::new();
    let opening = OPENING.get_or_init(|| Regex::new(r"^\s*([IVXLC]{1,6})\.\s+\p{Lu}").unwrap());
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let mut run = Vec::new();
    let mut next = None;
    for (index, paragraph) in kinds.paragraphs.iter().enumerate() {
        let Some(raw) = coordinates.slice_utf16(paragraph.range.start..paragraph.range.end) else { continue };
        let Some(number) = opening.captures(raw).and_then(|capture| page_number(&capture[1])) else { continue };
        if next.is_none_or(|next| next == number) {
            next = Some(number + 1);
            if let Some(&(_, end)) = sentences(raw).first().filter(|sentence| sentence.1 < raw.trim_end().len() && raw[..sentence.1].split_whitespace().count() <= TITLE_WORDS) {
                run.push((index, utf16_len(&raw[..end])));
            }
        }
    }
    if run.len() < 2 {
        return;
    }
    for (index, length) in run {
        let paragraph = &mut kinds.paragraphs[index];
        kinds.headings.push(read_heading(paragraph.range.start, paragraph.range.start + length, "run-in"));
        paragraph.range.start += length;
    }
}

/// Bracketed lines of one form with no text between them are a list's items: a title has a section under it.
pub(super) fn list_items(kinds: &mut Kinds) {
    // An outline numbers with a stop, "I." over "A."; a list with a bracket, "(a)", "d)", "6)".
    static ITEM: OnceLock<Regex> = OnceLock::new();
    let item = ITEM.get_or_init(|| Regex::new(r"^(\(?)([a-z]{1,4}|\d{1,2})\)").unwrap());
    let family = |text: &str| item.captures(text).map(|capture| (!capture[1].is_empty(), capture[2].starts_with(|c: char| c.is_ascii_digit())));
    let mut listed = vec![false; kinds.headings.len()];
    for at in 1..kinds.headings.len() {
        let (before, heading) = (&kinds.headings[at - 1], &kinds.headings[at]);
        let after = kinds.paragraphs.partition_point(|paragraph| paragraph.range.start <= before.range.start);
        let adjacent = kinds.paragraphs.get(after).is_none_or(|paragraph| paragraph.range.start >= heading.range.start);
        if adjacent && family(&before.display_value.as_deref().unwrap_or("")).is_some() && family(&before.display_value.as_deref().unwrap_or("")) == family(&heading.display_value.as_deref().unwrap_or("")) {
            listed[at - 1] = true;
            listed[at] = true;
        }
    }
    let mut index = 0;
    let mut items = Vec::new();
    kinds.headings.retain(|heading| {
        index += 1;
        if listed[index - 1] {
            items.push(read_paragraph(heading.range.start, heading.range.end, None));
        }
        !listed[index - 1]
    });
    if !items.is_empty() {
        kinds.paragraphs.append(&mut items);
        kinds.paragraphs.sort_by_key(|paragraph| paragraph.range.start);
    }
}
