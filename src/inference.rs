use super::*;
use std::{
    collections::{hash_map::Entry, BTreeMap},
    sync::Arc,
};

macro_rules! cached_regex {
    ($name:ident, $pattern:expr) => {{
        static $name: OnceLock<Regex> = OnceLock::new();
        $name.get_or_init(|| Regex::new($pattern).unwrap())
    }};
}

mod paragraphs;
use paragraphs::*;
mod headings;
pub(crate) use headings::*;
mod runs;
pub(crate) use runs::*;
mod pages;
use pages::*;
mod case;
use case::*;
mod sections;
pub(crate) use sections::*;
mod instrument;
pub(crate) use instrument::*;
mod journal;
use journal::*;
#[cfg(all(feature = "native-markup", feature = "courtlistener"))]
mod formatted_headings;
#[cfg(all(feature = "native-markup", feature = "courtlistener"))]
pub(crate) use formatted_headings::*;

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(usize)]
enum MarkerStyle {
    Bracket,
    Dot,
    Bare,
}

#[derive(Clone)]
struct Marker {
    number: u32,
    start: usize,
    content_start: usize,
    style: MarkerStyle,
    score: f64,
    formal: bool,
    sentence: bool,
}

#[derive(Clone, Copy)]
struct Line<'a> {
    byte_start: usize,
    byte_end: usize,
    scalar_start: usize,
    text: &'a str,
}

fn lines<'a>(text: &'a ScalarText<'a>) -> impl Iterator<Item = Line<'a>> + 'a {
    text.lines().iter().map(move |line| Line {
        byte_start: line[0],
        byte_end: line[1],
        scalar_start: line[2],
        text: &text.value[line[0]..line[1]],
    })
}

fn javascript_lines<'a>(text: &'a ScalarText<'a>) -> Vec<Line<'a>> {
    // This is JavaScript regexp line segmentation, not coordinate conversion:
    // CRLF is one break while its two source scalars remain counted.
    let mut result = Vec::new();
    let mut chars = text.value.char_indices().peekable();
    let (mut byte_start, mut scalar_start, mut scalar) = (0, 0, 0);
    while let Some((byte, character)) = chars.next() {
        if matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
            result.push(Line {
                byte_start,
                byte_end: byte,
                scalar_start,
                text: &text.value[byte_start..byte],
            });
            scalar += 1;
            let mut next_byte = byte + character.len_utf8();
            if character == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
                let (next, _) = chars.next().unwrap();
                next_byte = next + 1;
                scalar += 1;
            }
            byte_start = next_byte;
            scalar_start = scalar;
        } else {
            scalar += 1;
        }
    }
    result.push(Line {
        byte_start,
        byte_end: text.value.len(),
        scalar_start,
        text: &text.value[byte_start..],
    });
    result
}

fn leading_ascii_space(value: &str) -> usize {
    value
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()
}

fn decimal_prefix(value: &str, maximum: usize) -> Option<(&str, usize)> {
    let length = value.bytes().take_while(u8::is_ascii_digit).count();
    (length > 0 && length <= maximum).then(|| (&value[..length], length))
}

pub(super) fn inferred_blocks(evidence: &DocumentInput, text: &ScalarText<'_>) -> Vec<Block> {
    match evidence.profile {
        DetectionProfile::Legislation => {
            let mut blocks = detect_legislation(
                text,
                evidence.allow_hyphenated_sections,
                &evidence.native_claims,
            );
            // A marginal note or heading printed before the next provision belongs to that
            // provision, not to the end of the one before it.
            for block in &mut blocks {
                if block.kind == NodeKind::Section {
                    let start = block.content_start.unwrap_or(block.range.start);
                    block.range.end =
                        provision_end_before_marginal_notes(text, start, block.range.end);
                }
            }
            blocks
        }
        DetectionProfile::Instrument => detect_instrument(text),
        DetectionProfile::Journal => detect_journal(text),
        _ => {
            let layout = case_source_layout(evidence, text);
            let mut exclusions = evidence
                .exclusions
                .iter()
                .filter(|value| value.applies_to.iter().any(|name| name == "paragraph"))
                .map(|value| value.range)
                .collect::<Vec<_>>();
            exclusions.extend(layout.exclusions.iter().chain(&layout.gutter).copied());
            exclusions.sort_unstable_by_key(|range| (range.start, range.end));
            let mut blocks = detect_case(
                text,
                evidence.profile,
                evidence.report_start_page,
                evidence.require_report_start,
                &exclusions,
                evidence.provider == "courtlistener",
            );
            blocks.extend(detect_case_headings(evidence, text, &layout));
            let prose = detect_case_prose(evidence, text, &layout, &blocks);
            blocks.extend(layout.blocks);
            blocks.extend(prose);
            if evidence.provider == "courtlistener" {
                let pages = form_feed_pages(evidence, text, &blocks);
                blocks.extend(pages);
            }
            blocks
        }
    }
}
