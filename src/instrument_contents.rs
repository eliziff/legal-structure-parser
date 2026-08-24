use crate::{
    instrument::{
        InstrumentContentsEntry, InstrumentContentsOutline, InstrumentContentsReading,
        InstrumentContentsRefusal,
    },
    javascript_whitespace,
    text::{normalize_javascript_whitespace, ScalarText},
};
use regex::Regex;
use std::{collections::HashMap, sync::OnceLock};

// Contents entries advertise provision labels and printed pages; they are not
// provision spans and never enter the detected node inventory.
const CONTENTS_MAX_ENTRY_GAP_UTF16: usize = 400;
// Measured entry gaps were 28-176 UTF-16 units across the accepted corpus;
// 200, 400, and 800 produced identical outlines on all 124 agreement texts.
const CONTENTS_WINDOW_UTF16: usize = 80_000;
const CONTENTS_MAX_ANCHORS: usize = 4;
const MIN_CONTENTS_ENTRIES: usize = 5;
// Accepted contents regions cite pages on 84-100% of their entries.
const MIN_CONTENTS_PAGE_SHARE: f64 = 0.6;
// A short pageless exhibits tail is valid; a continuing body walk is not.
const MAX_PAGELESS_RUN: usize = 3;

#[derive(Clone, Debug)]
enum InstrumentContentsHeadKind {
    Container { word: String, value: String },
    Schedule { word: String, value: String },
    Section { number: String },
}

#[derive(Clone, Debug)]
struct InstrumentContentsHead {
    start_byte: usize,
    end_byte: usize,
    start_utf16: usize,
    end_utf16: usize,
    kind: InstrumentContentsHeadKind,
}

fn instrument_contents_anchors(text: &str) -> Vec<usize> {
    static TABLE: OnceLock<Regex> = OnceLock::new();
    static BARE: OnceLock<Regex> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        Regex::new(r"(?i:TABLE[ \t]+OF[ \t]+CONTENTS)")
            .expect("valid instrument contents anchor grammar")
    });
    let bare = BARE.get_or_init(|| {
        Regex::new(r"(?i)^(?:CONTENTS|INDEX)$")
            .expect("valid bare instrument contents anchor grammar")
    });
    let mut anchors = Vec::with_capacity(CONTENTS_MAX_ANCHORS);
    let mut tables = table
        .find_iter(text)
        .filter_map(|found| {
            let before = text[..found.start()].chars().next_back();
            let after = text[found.end()..].chars().next();
            (before.is_none_or(|character| matches!(character, '\r' | '\n' | '\t' | ' '))
                && after.is_none_or(|character| matches!(character, '\r' | '\n' | '\t' | ' ')))
            .then_some((found.start(), found.end()))
        })
        .peekable();
    let mut start = 0;
    loop {
        let end = text[start..]
            .find(['\r', '\n'])
            .map_or(text.len(), |length| start + length);
        let line = &text[start..end];
        while tables
            .peek()
            .is_some_and(|(found_start, _)| *found_start < end)
        {
            anchors.push(tables.next().unwrap().1);
            if anchors.len() == CONTENTS_MAX_ANCHORS {
                break;
            }
        }
        if anchors.len() == CONTENTS_MAX_ANCHORS {
            break;
        }
        let core = line.trim_matches([' ', '\t']);
        if bare.is_match(core) {
            anchors.push(end);
            if anchors.len() == CONTENTS_MAX_ANCHORS {
                break;
            }
        }
        if end == text.len() {
            break;
        }
        start = end + text[end..].chars().next().unwrap().len_utf8();
    }
    anchors
}

fn instrument_contents_heads(
    region: &str,
    text: &ScalarText<'_>,
    region_start_byte: usize,
    region_start_utf16: usize,
) -> Vec<InstrumentContentsHead> {
    // Heads, not line breaks, delimit entries because source formats variously
    // pack entries, preserve spacing, or break them mid-entry. Schedule-like
    // heads require a line boundary or two spaces because their vocabulary
    // also appears inside entry titles.
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let head = HEAD.get_or_init(|| {
        Regex::new(
            r"(?:(?P<container>ARTICLE|Article|PART|Part|DIVISION|Division)[ \t]+(?P<container_value>[IVXLCDM]{1,7}|[0-9]{1,3})[.:]?|(?P<schedule>SCHEDULE|Schedule|EXHIBIT|Exhibit|ANNEX|Annex|APPENDIX|Appendix)[ \t]+(?P<schedule_value>[A-Z0-9][A-Za-z0-9_.-]{0,12}?)[.:]?|(?P<section_word>Section|SECTION)[ \t]+(?P<section>[0-9]{1,3}(?:\.[0-9]{1,3})*[A-Za-z]?)[.)]?|(?P<decimal>[0-9]{1,3}\.[0-9]{1,3}(?:\.[0-9]{1,3})*)[.)]?|(?P<integer>[0-9]{1,3})[.)])(?P<trail>[ \t\r\n]|$)",
        )
        .expect("valid instrument contents head grammar")
    });
    let mut heads = Vec::<InstrumentContentsHead>::new();
    let mut search = 0;
    while search <= region.len() {
        let Some(found) = head.captures_at(region, search) else {
            break;
        };
        let whole = found.get(0).expect("contents head match");
        let trail = found.name("trail").expect("contents head trail");
        let body = found
            .name("container")
            .or_else(|| found.name("schedule"))
            .or_else(|| found.name("section_word"))
            .or_else(|| found.name("decimal"))
            .or_else(|| found.name("integer"))
            .expect("contents head body");
        let before = region[..body.start()].chars().next_back();
        let valid_lead = body.start() == 0 || before.is_some_and(javascript_whitespace);
        let schedule_lead = || {
            let mut before = region[..body.start()].chars().rev();
            match before.next() {
                Some('\r' | '\n') => true,
                Some(' ' | '\t') => before
                    .next()
                    .is_some_and(|value| matches!(value, ' ' | '\t')),
                _ => false,
            }
        };
        let kind = if !valid_lead {
            None
        } else if let (Some(word), Some(value)) =
            (found.name("container"), found.name("container_value"))
        {
            Some(InstrumentContentsHeadKind::Container {
                word: word.as_str().to_owned(),
                value: value.as_str().to_owned(),
            })
        } else if let (Some(word), Some(value)) =
            (found.name("schedule"), found.name("schedule_value"))
        {
            schedule_lead().then(|| InstrumentContentsHeadKind::Schedule {
                word: word.as_str().to_owned(),
                value: value.as_str().to_owned(),
            })
        } else {
            found
                .name("section")
                .or_else(|| found.name("decimal"))
                .or_else(|| found.name("integer"))
                .map(|number| InstrumentContentsHeadKind::Section {
                    number: number.as_str().to_owned(),
                })
        };
        if let Some(kind) = kind {
            let start_byte = body.start();
            let start_utf16 = text
                .utf16_at_byte(region_start_byte + start_byte)
                .expect("regex start is a UTF-8 boundary")
                - region_start_utf16;
            if heads
                .last()
                .map_or(start_utf16 > CONTENTS_MAX_ENTRY_GAP_UTF16, |prior| {
                    start_utf16 - prior.end_utf16 > CONTENTS_MAX_ENTRY_GAP_UTF16
                })
            {
                break;
            }
            heads.push(InstrumentContentsHead {
                start_byte,
                end_byte: trail.start(),
                start_utf16,
                end_utf16: text
                    .utf16_at_byte(region_start_byte + trail.start())
                    .expect("regex end is a UTF-8 boundary")
                    - region_start_utf16,
                kind,
            });
            search = trail.start();
        } else {
            search = body.start() + body.as_str().chars().next().unwrap().len_utf8();
        }
        if search >= region.len() {
            break;
        }
        if search <= whole.start() {
            search = whole.end();
        }
    }
    heads
}

fn instrument_contents_unit_end(value: &str) -> Option<usize> {
    // Printed page footers occur between blank lines inside contents pages;
    // absorbing one as an entry page creates a false page decrease.
    let mut search = 0;
    while let Some(relative) = value[search..].find('\n') {
        let newline = search + relative;
        let mut start = newline;
        for (byte, character) in value[..newline].char_indices().rev() {
            if character == '\n' || !javascript_whitespace(character) {
                break;
            }
            start = byte;
        }

        let after_newline = newline + 1;
        let mut closes_blank_line = false;
        for character in value[after_newline..].chars() {
            if character == '\n' {
                closes_blank_line = true;
                break;
            }
            if !javascript_whitespace(character) {
                break;
            }
        }
        if closes_blank_line {
            return Some(start);
        }
        search = after_newline;
    }
    None
}

fn instrument_roman_value(value: &str) -> Option<u32> {
    let values = |character| match character {
        'I' => Some(1),
        'V' => Some(5),
        'X' => Some(10),
        'L' => Some(50),
        'C' => Some(100),
        'D' => Some(500),
        'M' => Some(1000),
        _ => None,
    };
    let mut characters = value.chars().peekable();
    let mut total = 0i32;
    while let Some(character) = characters.next() {
        let current = values(character)?;
        let next = characters.peek().copied().and_then(values).unwrap_or(0);
        total += if current < next { -current } else { current };
    }
    u32::try_from(total).ok()
}

fn instrument_contents_region(
    text: &ScalarText<'_>,
    from_byte: usize,
    from_utf16: usize,
) -> Option<InstrumentContentsOutline> {
    // Original-text offsets: the window floors a split surrogate and the
    // final-entry lookahead ceils one; exact regex boundaries never round.
    let requested_utf16 = CONTENTS_WINDOW_UTF16.min(text.utf16_len() - from_utf16);
    let region_end = text
        .byte_at_utf16_floor(from_utf16 + requested_utf16)
        .expect("bounded UTF-16 window");
    let region_utf16 = text
        .utf16_at_byte(region_end)
        .expect("contents window ends at a UTF-8 boundary")
        - from_utf16;
    let region = &text.value[from_byte..region_end];
    let heads = instrument_contents_heads(region, text, from_byte, from_utf16);
    if heads.is_empty() || heads[0].start_utf16 > CONTENTS_MAX_ENTRY_GAP_UTF16 {
        return None;
    }

    let mut entries = Vec::new();
    let mut by_label: HashMap<String, usize> = HashMap::new();
    let mut container: Option<String> = None;
    let mut previous_page = 0;
    let mut pageless = 0;
    let mut pageless_from = 0;
    let mut last_head: Option<usize> = None;
    for (index, head) in heads.iter().enumerate() {
        if index > 0 && head.start_utf16 - heads[index - 1].end_utf16 > CONTENTS_MAX_ENTRY_GAP_UTF16
        {
            break;
        }
        let until_byte = if heads
            .get(index + 1)
            .is_some_and(|next| next.start_utf16 - head.end_utf16 <= CONTENTS_MAX_ENTRY_GAP_UTF16)
        {
            heads[index + 1].start_byte
        } else {
            text.byte_at_utf16_ceil(from_utf16 + (head.end_utf16 + 200).min(region_utf16))
                .expect("bounded UTF-16 contents cut")
                - from_byte
        };
        let raw = &region[head.end_byte..until_byte];
        let raw = instrument_contents_unit_end(raw).map_or(raw, |cut| &raw[..cut]);
        let unit = normalize_javascript_whitespace(raw);
        let page_match_start = unit.rfind(' ').map_or(0, |space| space);
        let page_token = if page_match_start == 0 {
            unit.as_str()
        } else {
            &unit[page_match_start + 1..]
        };
        let page = (!page_token.is_empty()
            && page_token.len() <= 4
            && page_token.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| page_token.parse::<u32>().ok())
        .flatten();
        if page.is_some_and(|page| page < previous_page) {
            break;
        }

        let (label, display, depth, parent_label, is_container) = match &head.kind {
            InstrumentContentsHeadKind::Container { word, value } => {
                let number = value
                    .parse::<u32>()
                    .ok()
                    .or_else(|| instrument_roman_value(&value.to_ascii_uppercase()));
                let Some(number) = number else { continue };
                let lower = word.to_ascii_lowercase();
                let prefix = match lower.as_str() {
                    "article" => "art",
                    "part" => "part",
                    _ => "div",
                };
                (
                    format!("{prefix}{number}"),
                    format!("{} {value}", word.to_ascii_uppercase()),
                    0,
                    None,
                    true,
                )
            }
            InstrumentContentsHeadKind::Schedule { word, value } => {
                let prefix = match word.to_ascii_lowercase().as_str() {
                    "schedule" => "sched",
                    "exhibit" => "exh",
                    "annex" => "annex",
                    _ => "app",
                };
                (
                    format!("{prefix}{}", value.to_ascii_lowercase()),
                    format!("{} {value}", word.to_ascii_uppercase()),
                    0,
                    None,
                    true,
                )
            }
            InstrumentContentsHeadKind::Section { number } => {
                let numbered_parent = number.rfind('.').and_then(|dot| {
                    let parent = format!("sec{}", &number[..dot]);
                    by_label
                        .get(&parent)
                        .copied()
                        .map(|depth| (parent, depth + 1))
                });
                let (parent, depth) = numbered_parent
                    .map(|(parent, depth)| (Some(parent), depth))
                    .unwrap_or_else(|| (container.clone(), usize::from(container.is_some())));
                (
                    format!("sec{number}"),
                    format!("Section {number}"),
                    depth,
                    parent,
                    false,
                )
            }
        };
        if by_label.contains_key(&label) {
            break;
        }
        if page.is_none() {
            if pageless == 0 {
                pageless_from = entries.len();
            }
            pageless += 1;
            if pageless > MAX_PAGELESS_RUN {
                entries.truncate(pageless_from);
                break;
            }
        } else {
            pageless = 0;
            previous_page = page.unwrap();
        }
        if is_container {
            container = Some(label.clone());
        }
        let heading_source = page.map_or(unit.as_str(), |_| &unit[..page_match_start]);
        let heading = heading_source
            .trim_end_matches(|character: char| {
                character == '.' || character == '\u{2026}' || javascript_whitespace(character)
            })
            .trim_start_matches(|character: char| {
                javascript_whitespace(character)
                    || matches!(character, '\u{2013}' | '\u{2014}' | '-' | ':' | '.')
            })
            .trim_matches(javascript_whitespace)
            .to_owned();
        let entry = InstrumentContentsEntry {
            label: label.clone(),
            display,
            heading,
            depth,
            parent_label,
            page,
            contents_line_start: from_utf16 + head.start_utf16,
        };
        by_label.insert(label, depth);
        entries.push(entry);
        last_head = Some(index);
    }
    if entries.is_empty() {
        return None;
    }
    let last_head = last_head?;
    Some(InstrumentContentsOutline {
        pages_cited: entries.iter().filter(|entry| entry.page.is_some()).count(),
        region_start: from_utf16 + heads[0].start_utf16,
        region_end: from_utf16 + heads[last_head].end_utf16,
        entries,
    })
}

/// Read a document's own table of contents as a page-addressed outline. The
/// outline never claims provision spans; ambiguous inputs receive a typed refusal.
#[cfg(test)]
fn instrument_contents_outline(text: &str) -> InstrumentContentsReading {
    instrument_contents_outline_indexed(&ScalarText::new(text))
}

fn instrument_contents_outline_from_anchors(
    text: &ScalarText<'_>,
    anchors: Vec<usize>,
) -> InstrumentContentsReading {
    let mut refusal = InstrumentContentsRefusal::NoContentsEntries;
    for from_byte in anchors {
        let from_utf16 = text
            .utf16_at_byte(from_byte)
            .expect("contents anchor end is a UTF-8 boundary");
        let Some(outline) = instrument_contents_region(text, from_byte, from_utf16) else {
            continue;
        };
        if outline.entries.len() < MIN_CONTENTS_ENTRIES {
            refusal = InstrumentContentsRefusal::TooFewContentsEntries;
            continue;
        }
        if outline.pages_cited as f64 / (outline.entries.len() as f64) < MIN_CONTENTS_PAGE_SHARE {
            refusal = InstrumentContentsRefusal::ContentsWithoutPageNumbers;
            continue;
        }
        return InstrumentContentsReading {
            outline: Some(outline),
            refusal: None,
        };
    }
    InstrumentContentsReading {
        outline: None,
        refusal: Some(refusal),
    }
}

pub(super) fn instrument_contents_outline_indexed(
    text: &ScalarText<'_>,
) -> InstrumentContentsReading {
    let anchors = instrument_contents_anchors(text.value);
    if anchors.is_empty() {
        InstrumentContentsReading {
            outline: None,
            refusal: Some(InstrumentContentsRefusal::NoContentsMarker),
        }
    } else {
        instrument_contents_outline_from_anchors(text, anchors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contents_preserve_packed_entries_and_nesting() {
        let text = "TABLE OF CONTENTS Page ARTICLE I DEFINITIONS 2 Section 1.01 Defined Terms 2 Section 1.02 Interpretation 4 ARTICLE II THE MERGER 5 Section 2.01 The Merger 5 Section 2.02 Closing 6";
        let reading = instrument_contents_outline(text);
        let outline = reading.outline.as_ref().expect("accepted outline");
        assert_eq!(
            outline
                .entries
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            ["art1", "sec1.01", "sec1.02", "art2", "sec2.01", "sec2.02"]
        );
        assert_eq!(outline.entries[0].heading, "DEFINITIONS");
        assert_eq!(outline.entries[1].parent_label.as_deref(), Some("art1"));
        assert_eq!(outline.entries[1].depth, 1);
        assert_eq!(outline.entries[4].parent_label.as_deref(), Some("art2"));
        assert_eq!(reading.refusal, None);
    }

    #[test]
    fn contents_match_all_refusals() {
        let cases = [
            (
                "The contents are discussed in prose.",
                InstrumentContentsRefusal::NoContentsMarker,
            ),
            (
                "CONTENTS\nnot an outline",
                InstrumentContentsRefusal::NoContentsEntries,
            ),
            (
                "CONTENTS\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 4",
                InstrumentContentsRefusal::TooFewContentsEntries,
            ),
            (
                "CONTENTS\nSection 1. First 1\nSection 2. Second\nSection 3. Third 2\nSection 4. Fourth\nSection 5. Fifth",
                InstrumentContentsRefusal::ContentsWithoutPageNumbers,
            ),
        ];
        for (text, expected) in cases {
            let reading = instrument_contents_outline(text);
            assert_eq!(reading.outline, None, "{text}");
            assert_eq!(reading.refusal, Some(expected), "{text}");
        }
        assert_eq!(
            serde_json::to_string(&instrument_contents_outline("plain text")).unwrap(),
            r#"{"outline":null,"refusal":"no_contents_marker"}"#
        );
    }

    #[test]
    fn contents_cut_page_footers_at_blank_lines() {
        let text = "TABLE OF CONTENTS\nARTICLE I DEFINITIONS 60\nSection 1.01 Defined Terms 61\n\n2\n\nSection 1.02 Interpretation 62\nSection 1.03 Currency 63\nSection 1.04 Notices 64\nSection 1.05 Time 65";
        let reading = instrument_contents_outline(text);
        let outline = reading.outline.as_ref().expect("accepted outline");
        assert_eq!(
            outline
                .entries
                .iter()
                .map(|entry| entry.page)
                .collect::<Vec<_>>(),
            [Some(60), Some(61), Some(62), Some(63), Some(64), Some(65)]
        );
    }

    #[test]
    fn contents_resume_inside_a_guarded_schedule() {
        let text = "TABLE OF CONTENTS\nSection 0. First 1\nSection 1. Company Schedule 2. Closing 2\nSection 3. Third 3\nSection 4. Fourth 4\nSection 5. Fifth 5";
        let reading = instrument_contents_outline(text);
        let outline = reading.outline.as_ref().expect("accepted outline");
        assert_eq!(
            outline
                .entries
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            ["sec0", "sec1", "sec2", "sec3", "sec4", "sec5"]
        );
        assert_eq!(outline.entries[1].heading, "Company Schedule");
        assert_eq!(outline.entries[1].page, None);
        assert_eq!(outline.entries[2].heading, "Closing");
        assert_eq!(outline.entries[2].page, Some(2));
    }

    #[test]
    fn contents_report_javascript_utf16_offsets() {
        let text = "\u{1f9ab}\nINDEX\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 4\nSection 5. Fifth 5";
        let reading = instrument_contents_outline(text);
        let outline = reading.outline.as_ref().expect("accepted outline");
        assert_eq!(outline.region_start, 9);
        assert_eq!(outline.entries[0].contents_line_start, 9);
    }

    #[test]
    fn contents_keep_astral_combining_text_and_crlf_offsets() {
        let text = "\u{1f9ab}\r\nINDEX\r\nSection 1. First \u{1f9ab} e\u{301} 1\rSection 2. Second 2\nSection 3. Third 3\r\nSection 4. Fourth 4\nSection 5. Fifth 5";
        let outline = instrument_contents_outline(text).outline.unwrap();
        assert_eq!(outline.region_start, 11);
        assert_eq!(outline.entries[0].contents_line_start, 11);
        assert_eq!(outline.entries[0].heading, "First \u{1f9ab} e\u{301}");
        assert_eq!(outline.entries.last().unwrap().label, "sec5");
    }

    #[test]
    fn contents_stop_on_decrease_duplicate_and_pageless_run() {
        let decrease = "CONTENTS\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 2\nSection 5. Fifth 5";
        assert_eq!(
            instrument_contents_outline(decrease).refusal,
            Some(InstrumentContentsRefusal::TooFewContentsEntries)
        );

        let duplicate = "CONTENTS\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 4\nSection 5. Fifth 5\nSection 3. Body 6\nSection 6. Sixth 6";
        let outline = instrument_contents_outline(duplicate).outline.unwrap();
        assert_eq!(outline.entries.len(), 5);
        assert_eq!(outline.entries.last().unwrap().label, "sec5");

        let pageless = "CONTENTS\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 4\nSection 5. Fifth 5\nSection 6. Sixth\nSection 7. Seventh\nSection 8. Eighth\nSection 9. Ninth";
        let outline = instrument_contents_outline(pageless).outline.unwrap();
        assert_eq!(outline.entries.len(), 5);
        assert_eq!(outline.entries.last().unwrap().label, "sec5");
    }
}
