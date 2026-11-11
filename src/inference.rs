use super::*;
use std::{collections::BTreeMap, sync::Arc};

macro_rules! cached_regex {
    ($name:ident, $pattern:expr) => {{
        static $name: OnceLock<Regex> = OnceLock::new();
        $name.get_or_init(|| Regex::new($pattern).unwrap())
    }};
}

mod paragraphs;
use paragraphs::*;
pub(crate) use paragraphs::run_in_heading;
mod tail;
use tail::*;
pub(crate) use tail::closing_line;
#[cfg(feature = "opinion")]
pub(crate) use tail::ends_sentence;
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
    /// Under a line ending on a colon: what opens here is what that line introduces.
    introduced: bool,
    /// Its words open in lower case: the sentence above goes on under it, if the count prints it again.
    going_on: bool,
}

impl Marker {
    /// A marker on a line of its own: no heading run in before it, nothing introducing it.
    fn new(number: u32, start: usize, content_start: usize, style: MarkerStyle, score: f64) -> Self {
        Self { number, start, content_start, style, score, formal: false, sentence: false, introduced: false, going_on: false }
    }
}

/// A passage of running prose the reading found.
fn prose_block(range: ScalarRange) -> StructureNode {
    StructureNode::new(String::new(), NodeKind::Prose, range, ENGINE_ORIGIN, Derivation::Heuristic, None)
}

/// A line of the text: its bytes, where it starts in UTF-16, and its text.
#[derive(Clone, Copy)]
struct Line<'a> {
    byte_start: usize,
    byte_end: usize,
    start: usize,
    text: &'a str,
}

fn lines<'a>(text: &'a ScalarText<'a>) -> impl Iterator<Item = Line<'a>> + 'a {
    text.lines().iter().map(move |line| Line {
        byte_start: line[0],
        byte_end: line[1],
        start: line[2],
        text: &text.value[line[0]..line[1]],
    })
}

fn javascript_lines<'a>(text: &'a ScalarText<'a>) -> Vec<Line<'a>> {
    // This is JavaScript regexp line segmentation: CRLF is one break while its two units remain counted.
    let mut result = Vec::new();
    let mut chars = text.value.char_indices().peekable();
    let (mut byte_start, mut start, mut unit) = (0, 0, 0);
    while let Some((byte, character)) = chars.next() {
        if matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
            result.push(Line {
                byte_start,
                byte_end: byte,
                start,
                text: &text.value[byte_start..byte],
            });
            unit += 1;
            let mut next_byte = byte + character.len_utf8();
            if character == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
                let (next, _) = chars.next().unwrap();
                next_byte = next + 1;
                unit += 1;
            }
            byte_start = next_byte;
            start = unit;
        } else {
            unit += character.len_utf16();
        }
    }
    result.push(Line {
        byte_start,
        byte_end: text.value.len(),
        start,
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

pub(super) fn inferred_blocks(evidence: &Evidence, text: &ScalarText<'_>) -> Vec<StructureNode> {
    let mut blocks = profile_blocks(evidence, text);
    if !blocks.iter().chain(&evidence.structure.nodes).any(|node| node.kind == NodeKind::Page) {
        blocks.extend(marked_pages(text));
    }
    blocks
}

/// What the case reader reads in a provider's text of a judgment (its numbered paragraphs, notes and printed pages): the
/// blocks `profile_blocks` reads for such a text, which carries no layout, exclusions or report start to read with.
pub(crate) fn case_paragraphs(text: &ScalarText<'_>) -> Vec<StructureNode> {
    detect_case(text, DetectionProfile::CaseRootedComplete, None, false, &[], false, &[])
}

fn profile_blocks(evidence: &Evidence, text: &ScalarText<'_>) -> Vec<StructureNode> {
    match evidence.profile {
        DetectionProfile::Legislation => {
            let mut blocks = detect_legislation(
                text,
                evidence.allow_hyphenated_sections,
                &evidence.structure.nodes,
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
                .filter(|value| value.applies_to.contains(&NodeKind::Paragraph))
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
                evidence.structure.provider == "courtlistener",
                &layout.markers,
            );
            blocks.extend(detect_case_headings(evidence, text, &layout));
            let prose = detect_case_prose(evidence, text, &layout, &blocks);
            blocks.extend(layout.blocks);
            blocks.extend(prose);
            if evidence.structure.provider == "courtlistener" {
                let pages = form_feed_pages(evidence, text, &blocks);
                blocks.extend(pages);
            }
            blocks
        }
    }
}
