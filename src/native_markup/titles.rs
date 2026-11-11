//! What a CourtListener rendition's body is and what it prints as a title, read once here for the engine's reading and for
//! the opinion's: the body's blocks, the one pass deciding which of them (or which part of one) is a heading, the words
//! that make a block or a line a title, and the paragraph marks blocks open on.

use std::sync::OnceLock;
use legal_structure_model::document::{Derivation, DocumentStructure, MarkupElement, NodeKind, PrimaryRole, StructureNode};
use legal_structure_model::text::{javascript_whitespace, utf16_len, ScalarText};
use legal_structure_model::ScalarRange;
use regex::Regex;
use crate::inference::{enumerator_end, heading_enumerator, heading_title_case};
use crate::text::ScalarCoordinates;
use crate::trim_javascript_whitespace as trim_js;

/// The markup element a node was read from, and where it stands among the elements.
pub(crate) fn element_of<'a>(structure: &'a DocumentStructure, node: &StructureNode) -> Option<(usize, &'a MarkupElement)> {
    let at = super::element_at(&structure.markup_elements, node.markup_start?)?;
    Some((at, &structure.markup_elements[at]))
}

/// A block of the body: its node, its range, its words without the page marks in it ("*223III." is "III.") and whether
/// it is a line of a table of contents.
pub(crate) struct BodyBlock {
    pub(crate) index: usize,
    pub(crate) range: ScalarRange,
    pub(crate) printed: String,
    pub(crate) contents: bool,
}

fn inside(ranges: &[ScalarRange], range: ScalarRange) -> bool {
    ranges.iter().any(|other| other.start <= range.start && range.end <= other.end)
}

/// The page marks the engine's pages print.
fn page_marks(structure: &DocumentStructure) -> Vec<ScalarRange> {
    let mut marks = structure.nodes.iter().filter(|node| node.kind == NodeKind::Page).filter_map(|page| page.marker_range).collect::<Vec<_>>();
    marks.sort_by_key(|mark| mark.start);
    marks
}

/// A range's words without the page marks in it.
fn unmarked(coordinates: &ScalarText, marks: &[ScalarRange], range: ScalarRange) -> String {
    let mut words = String::new();
    let mut at = range.start;
    let first = marks.partition_point(|mark| mark.start < range.start);
    for mark in marks[first..].iter().take_while(|mark| mark.end <= range.end) {
        words.push_str(coordinates.slice_utf16(at..mark.start).unwrap_or(""));
        at = mark.end.max(at);
    }
    words.push_str(coordinates.slice_utf16(at..range.end).unwrap_or(""));
    words
}

/// What holds head matter: a headnote, a line of the front matter by its role, an author's line.
fn held_ranges(structure: &DocumentStructure) -> Vec<ScalarRange> {
    structure.nodes.iter().filter(|node| node.markup_tag.as_deref() == Some("author") || node.role.is_some_and(|role| matches!(role,
        PrimaryRole::Headnote | PrimaryRole::Caption | PrimaryRole::Judges | PrimaryRole::Counsel | PrimaryRole::Date
        | PrimaryRole::DocketNumber | PrimaryRole::Court | PrimaryRole::Title)))
        .map(|node| structure.query_range(node)).collect()
}

/// The body's blocks in the text's order: each paragraph, prose block or quotation of the text's own role that no note
/// mostly holds, no head matter (`held_ranges`) and no heading holds, with words past its page marks.
pub(crate) fn body_blocks(structure: &DocumentStructure, text_index: &ScalarCoordinates) -> Vec<BodyBlock> {
    let coordinates = ScalarText::with_coordinates(structure.query_text(), text_index);
    let nodes = &structure.nodes;
    let notes = nodes.iter().filter(|node| node.kind == NodeKind::Footnote)
        .flat_map(|note| if note.ranges.is_empty() { vec![note.range] } else { note.ranges.clone() }).collect::<Vec<_>>();
    let held = held_ranges(structure);
    let headings = nodes.iter().filter(|node| node.kind == NodeKind::Heading)
        .map(|node| node.heading_range.unwrap_or(structure.query_range(node))).collect::<Vec<_>>();
    let marks = page_marks(structure);
    let mostly_in = |ranges: &[ScalarRange], range: ScalarRange| {
        ranges.iter().map(|other| other.end.min(range.end).saturating_sub(other.start.max(range.start))).sum::<usize>() * 2 >= (range.end - range.start).max(1)
    };
    let mut blocks = nodes.iter().enumerate().filter_map(|(index, node)| {
        if !matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose | NodeKind::Quote) || node.role.is_some_and(|role| role != PrimaryRole::Text) {
            return None;
        }
        let range = structure.query_range(node);
        if mostly_in(&notes, range) || inside(&held, range) || inside(&headings, range) {
            return None;
        }
        let printed = unmarked(&coordinates, &marks, range);
        let contents = contents_line(trim_js(&printed));
        (!printed.trim().is_empty()).then_some(BodyBlock { index, range, printed, contents })
    }).collect::<Vec<_>>();
    blocks.sort_by_key(|block| block.range.start);
    blocks
}

/// The one reading of a rendition's headings past those its markup declares. A heading the markup sets that is a case's
/// name is the caption's line. A body block is a title by its words where it stands alone (set off in its type, or after
/// a finished sentence: "I.", "II. BACKGROUND"); the title a paragraph's mark carries ("¶ 4 Facts"), a title in strong or
/// underlined type at a paragraph's head (`run_in`), and a section's title run into a paragraph at its start or end
/// (`fused_titles`) are headings of their own and the block keeps the rest. A line no node holds (a div's or a br's) is a
/// heading where it is a title by its words, past the head's lines. No title is read in front matter, a headnote, a note,
/// a contents line or a numbered block.
pub(super) fn shape_headings(structure: &mut DocumentStructure, text_index: &ScalarCoordinates) {
    let blocks = body_blocks(structure, text_index);
    let coordinates = ScalarText::with_coordinates(structure.query_text(), text_index);
    let slice = |range: ScalarRange| coordinates.slice_utf16(range.start..range.end).map(trim_js).unwrap_or("");
    let marks = page_marks(structure);
    let bare = |range: ScalarRange| unmarked(&coordinates, &marks, range);
    let held = held_ranges(structure);
    let nodes = &structure.nodes;
    let (captions, declared): (Vec<_>, Vec<_>) = nodes.iter().enumerate().filter(|(_, node)| node.kind == NodeKind::Heading)
        .map(|(index, node)| (index, node.heading_range.unwrap_or(structure.query_range(node))))
        .partition(|(_, range)| !inside(&held, *range) && caption_heading(slice(*range)));
    let mut headings = declared.into_iter().map(|(_, range)| range).collect::<Vec<_>>();
    // Blocks wholly titles; blocks that keep part of their range; the titles taken out of blocks, by the block.
    let (mut whole, mut kept, mut titles) = (Vec::new(), Vec::new(), Vec::<(Option<usize>, ScalarRange)>::new());
    let mut last: Option<(ScalarRange, bool)> = None;
    let mut first_text = usize::MAX;
    for block in blocks.iter().filter(|block| !block.contents) {
        let (index, read, printed) = (block.index, block.range, &block.printed);
        let node = &nodes[index];
        let quote = node.kind == NodeKind::Quote || node.markup_tag.as_deref().is_some_and(|tag| matches!(tag, "blockquote" | "excerpt"));
        let mut range = read;
        if !headings.iter().any(|heading| heading.start < range.end && range.start < heading.end) && node.marker_range.is_none() {
            let taken = titles.len();
            let after_a_sentence = last.is_none_or(|(last, quote)| !quote && crate::inference::ends_sentence(slice(last), true));
            let opened = trim_js(printed);
            let whole_title = enumerator_end(opened).is_some_and(|end| heading_title_case(&opened[end..].split_whitespace().collect::<Vec<_>>())) && sentences(opened).len() == 1;
            let alone = whole_title || after_a_sentence && (!crate::inference::ends_sentence(opened, true) || (opened.ends_with('.') && printed.split_whitespace().count() <= 3));
            if heading_line(printed, alone || emphasized_whole(structure, node)) {
                // A mark on a title keeps its place; it goes to the text under the title once paragraphs are numbered.
                let title = says_paragraph(opened).then(|| past_marker(opened)).filter(|title| !title.is_empty())
                    .and_then(|title| Some(range.start + utf16_len(&slice(range)[..slice(range).find(title)?])));
                let Some(start) = title else {
                    whole.push(index);
                    headings.push(range);
                    continue;
                };
                titles.push((Some(index), ScalarRange { start, end: range.end }));
                range.end = start;
            } else if let Some(end) = run_in(structure, node, range, &bare) {
                titles.push((Some(index), ScalarRange { start: range.start, end }));
                range.start = end;
            } else {
                let (lead, tail) = fused_titles(coordinates.slice_utf16(range.start..range.end).unwrap_or(""));
                if let Some(lead) = lead {
                    titles.push((Some(index), ScalarRange { start: range.start, end: range.start + lead }));
                    range.start += lead;
                }
                if let Some(tail) = tail {
                    titles.push((Some(index), ScalarRange { start: range.start.max(range.end - tail), end: range.end }));
                    range.end -= tail;
                }
            }
            headings.extend(titles[taken..].iter().map(|(_, title)| *title));
            if range != read {
                kept.push((index, range));
            }
        }
        first_text = first_text.min(range.start);
        last = Some((range, quote));
    }
    // Text no node holds: each line of it that is a title by its words, but a bare number and, ahead of the text, a line
    // of the head.
    let mut taken = nodes.iter().filter(|node| !matches!(node.kind, NodeKind::Section | NodeKind::Span | NodeKind::Page)).flat_map(|node| match node.kind {
        NodeKind::Footnote | NodeKind::Endnote if !node.ranges.is_empty() => node.ranges.clone(),
        NodeKind::Heading => vec![node.heading_range.unwrap_or(structure.query_range(node))],
        _ => vec![structure.query_range(node)],
    }).chain(marks.iter().copied()).chain(headings.iter().copied()).collect::<Vec<_>>();
    taken.sort_by_key(|range| range.start);
    let end = utf16_len(structure.query_text());
    let mut cursor = 0;
    for range in taken.into_iter().chain(std::iter::once(ScalarRange { start: end, end })) {
        let Some(raw) = coordinates.slice_utf16(cursor..range.start).filter(|_| range.start > cursor) else {
            cursor = cursor.max(range.end);
            continue;
        };
        let mut at = cursor;
        for line in raw.split_inclusive('\n') {
            let words = trim_js(line);
            let start = at + utf16_len(&line[..line.len() - line.trim_start_matches(javascript_whitespace).len()]);
            at += utf16_len(line);
            if words.is_empty() || bare_label(words) || start < first_text && front_role(words).is_some() {
                continue;
            }
            let enumerated = words.split_whitespace().next().is_some_and(heading_enumerator);
            if heading_line(words, true) && (enumerated || !words.chars().any(char::is_lowercase) || section_keyword(words)) {
                titles.push((None, ScalarRange { start, end: start + utf16_len(words) }));
            }
        }
        cursor = cursor.max(range.end);
    }
    let titles = titles.into_iter().map(|(index, range)| {
        let parent = index.and_then(|index| structure.nodes[index].parent_id.clone());
        let mut heading = StructureNode::new(format!("shape-heading-{}-{}", range.start, range.end), NodeKind::Heading, range, crate::ENGINE_ORIGIN, Derivation::Heuristic, parent);
        heading.role = Some(PrimaryRole::Heading);
        heading.heading_range = Some(range);
        heading.label = coordinates.slice_utf16(range.start..range.end).map(|label| trim_js(label).to_owned());
        heading
    }).collect::<Vec<_>>();
    for (index, range) in captions {
        let node = &mut structure.nodes[index];
        node.kind = NodeKind::Prose;
        node.role = Some(PrimaryRole::Caption);
        node.heading_range = None;
        node.range = range;
    }
    for index in whole {
        let range = structure.query_range(&structure.nodes[index]);
        let node = &mut structure.nodes[index];
        node.kind = NodeKind::Heading;
        node.role = Some(PrimaryRole::Heading);
        node.heading_range = Some(range);
    }
    for (index, range) in kept {
        let node = &mut structure.nodes[index];
        node.range = range;
        node.ranges = node.ranges.iter().filter_map(|part| {
            let (start, end) = (part.start.max(range.start), part.end.min(range.end));
            (start < end).then_some(ScalarRange { start, end })
        }).collect();
    }
    structure.nodes.extend(titles);
}

/// A title in strong or underlined type at a paragraph's head, or in italics where it reads as one: where it ends.
fn run_in(structure: &DocumentStructure, node: &StructureNode, range: ScalarRange, bare: &dyn Fn(ScalarRange) -> String) -> Option<usize> {
    if node.source != Derivation::Native {
        return None;
    }
    let (at, element) = element_of(structure, node)?;
    let child = structure.markup_elements[at + 1..].iter().take_while(|child| child.markup_start < element.markup_end)
        .find(|child| child.range.end > child.range.start && matches!(child.tag.as_str(), "strong" | "b" | "u" | "underline" | "center" | "em" | "i"))?;
    if child.range.start.saturating_sub(range.start) > 8 || child.range.end >= range.end {
        return None;
    }
    let title = bare(child.range);
    let title = trim_js(&title);
    let firm = !matches!(child.tag.as_str(), "em" | "i");
    let after = bare(ScalarRange { start: child.range.end, end: range.end });
    if trim_js(&after).starts_with(|c: char| c.is_lowercase() || matches!(c, ',' | ';')) {
        return None;
    }
    let lead = bare(ScalarRange { start: range.start, end: child.range.start });
    let enumerated = enumerator_end(trim_js(&lead)).is_some_and(|end| end == trim_js(&lead).len());
    let whole = format!("{} {title}", trim_js(&lead));
    (heading_line(&whole, true) && (firm || (title.ends_with(['.', ':']) && (enumerated || section_keyword(title))))).then_some(child.range.end)
}

/// Section titles run into a paragraph with no break: how far one runs from its start, and from its end.
fn fused_titles(raw: &str) -> (Option<usize>, Option<usize>) {
    static LEAD: OnceLock<Regex> = OnceLock::new();
    static TAIL: OnceLock<Regex> = OnceLock::new();
    static DASHED: OnceLock<Regex> = OnceLock::new();
    let dashed = DASHED.get_or_init(|| Regex::new(r"^\s*(\p{Lu}[^.—\n]{2,80}\.)—\s?\p{Lu}").unwrap()).captures(raw).and_then(|capture| capture.get(1)).map(|title| utf16_len(&raw[..title.end()]));
    let lead = LEAD.get_or_init(|| Regex::new(r"^\s*([A-Z][A-Z ,&]{5,60}?)\s+[A-Z][a-z .]").unwrap()).captures(raw)
        .filter(|capture| section_word(&capture[1])).and_then(|capture| capture.get(1)).map(|title| utf16_len(&raw[..title.end()])).or(dashed);
    let tail = TAIL.get_or_init(|| Regex::new(r#"[.;:!?”"]\s+([A-Z][A-Z ,&]{5,60})\s*$"#).unwrap()).captures(raw)
        .filter(|capture| section_word(&capture[1])).and_then(|capture| capture.get(1)).map(|title| utf16_len(&raw[title.start()..]));
    (lead, tail)
}

/// A section's usual titles.
const SECTION_TITLES: &str = r"(?:statement of )?(?:the )?(?:facts?|case)|background|factual (?:and procedural )?background|procedural (?:background|history)|(?:facts?|history) and procedure|facts and proceedings|discussion|analysis|conclusions?|introduction|opinion|order|memorandum(?: opinion| decision)?(?: and order)?|findings of fact(?: and conclusions of law)?|conclusions of law|standard of review|legal standards?|jurisdiction|argument|issues?(?: presented)?|questions? presented|holding|decision|disposition|summary|recommendation|appendix|judgment|decree|remedy|damages|law|applicable law|proceedings(?: below)?|prior proceedings|opinion of the court|memorandum of decision|reasons|order and judgment|syllabus(?: by the court)?|headnotes?|questions?|answers?|short answers?|brief answers?|errata|erratum|corrections?";

/// A section's usual title, past its enumerator and a "The": "II. BACKGROUND", "The Facts.", "Conclusion:".
pub(crate) fn section_word(body: &str) -> bool {
    static WORDS: OnceLock<Regex> = OnceLock::new();
    let body = &body[enumerator_end(body).unwrap_or(0)..];
    WORDS.get_or_init(|| Regex::new(&format!(r"(?i)^(?:the )?(?:{SECTION_TITLES})$")).unwrap()).is_match(body.trim_end_matches(['.', ':', ' ']))
}

/// The words of a section's title in a text that sets it no apart from a name or an address: a usual title's, or a topic's.
pub(crate) fn section_keyword(body: &str) -> bool {
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    KEYWORD.get_or_init(|| Regex::new(&format!(r"(?i)\b(?:{SECTION_TITLES}|history|standards?|review|claims?|motions?|arguments?|findings?|procedure|evidence|liability|fees|relief|statute|statutory|contract|negligence|immunity|appeal|applicable|legal)\b")).unwrap()).is_match(body)
}

/// A text without the page mark a page's turn fused to its front: "*69 DISCUSSION".
pub(crate) fn unfused(printed: &str) -> &str {
    static STAR: OnceLock<Regex> = OnceLock::new();
    STAR.get_or_init(|| Regex::new(r"^(?:\*+\s?\d{1,5}|\{\*+[^{}\n]{1,40}\})\s*").unwrap()).find(printed).filter(|found| found.end() < printed.len())
        .map_or(printed, |found| &printed[found.end()..])
}

/// Small capitals an extractor set apart: "O R D E R", "U N I T E D   S T A T E S".
pub(crate) fn letter_spaced(printed: &str) -> Option<String> {
    static SPACED: OnceLock<Regex> = OnceLock::new();
    static GAP: OnceLock<Regex> = OnceLock::new();
    SPACED.get_or_init(|| Regex::new(r"^(?:\p{Lu}[.,]?\s+){3,}\p{Lu}[.,:]?$").unwrap()).is_match(printed).then(|| {
        GAP.get_or_init(|| Regex::new(r"\s{2,}").unwrap()).split(printed).map(|word| word.split_whitespace().collect::<String>()).collect::<Vec<_>>().join(" ")
    })
}

/// A title without the page a text's table of contents or an extractor left on it: "FINDINGS OF FACT ...-— Page 1".
pub(crate) fn paged_title(printed: &str) -> &str {
    static PAGED: OnceLock<Regex> = OnceLock::new();
    PAGED.get_or_init(|| Regex::new(r"\s*[.\-—–]{2,}\s*Page \d+$").unwrap()).find(printed).map_or(printed, |found| &printed[..found.start()])
}

/// A paragraph or line that is a heading by its words: short, no citation, no sentence; in capitals, an enumerator
/// with a title, a section's usual name, or an enumerator alone where the source sets it apart.
pub(crate) fn heading_line(body: &str, set_apart: bool) -> bool {
    static CITATION: OnceLock<Regex> = OnceLock::new();
    static NOT_HEADING: OnceLock<Regex> = OnceLock::new();
    let body = paged_title(unfused(trim_js(body)));
    if body.matches('“').count() != body.matches('”').count() {
        return false;
    }
    if letter_spaced(body).is_some() {
        return true;
    }
    let length = body.chars().count();
    if length == 0 || length > 130 || body.chars().any(|c| c.is_control()) || contents_line(body) {
        return false;
    }
    if body.starts_with("e.g") || body.starts_with("i.e") || body.starts_with("Q.") {
        return false;
    }
    // "Ill" is III as a scanner reads it.
    if matches!(body.trim_end_matches('.'), "Ill" | "Il") {
        return true;
    }
    if body.chars().all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C')) && body.len() <= 6 {
        return true;
    }
    let rest = &body[enumerator_end(body).unwrap_or(0)..];
    let numbered = rest.len() != body.len();
    if rest.is_empty() {
        return set_apart || body.len() <= 6 && body.starts_with(['I', 'V', 'X']);
    }
    let citation = CITATION.get_or_init(|| Regex::new(r"\bv\.?(?:\s|$)|\bvs\.?(?:\s|$)|\b\d+\s+[A-Z][A-Za-z.\d ]{0,14}\s+\d+|U\.\s?S\.\s?C|\bF\.\s?(?:2d|3d|4th|Supp)|https?:|\bat \d|\(\d{4}\)$|¶").unwrap());
    let not_heading = NOT_HEADING.get_or_init(|| Regex::new(r"(?i),\s*(?:c\.\s*)?j\.?j?\.?$|\b(?:district|circuit|bankruptcy|chief|presiding|associate|senior|magistrate|administrative law) judge\b|\b(?:justice|judge)[.,:]?$|^per curiam|concur|dissent|^so ordered|^it is (?:so |hereby |further )?ordered|^(?:affirmed|reversed|remanded|vacated|dismissed|denied|granted)\b|^judgment (?:is |for |affirmed|reversed)|\b(?:affirmed|reversed|remanded|vacated|dismissed|denied|granted|sustained|overruled|modified|ordered|affirming|reversing)\.?$|\bjudge advocate\b|\bmilitary judge[.,]?$|\blaw (?:firm|offices?)\b|,\s*(?:llc|inc|pa|pc)\.?$|^(?:\p{Lu}\.){2,4}$|\$\s?\d|\(\d{3}\)\s?\d{3}|\b\d{5}(?:-\d{4})?$|^\d+\s.*\b(?:street|avenue|boulevard|suite|floor)\b|^enter:?$|^as used in\b|^(?:court|county|judge|trial court)\s?:|^opposed\b|^opinion by\b|^ibid\b|\besq\.?$|\b(?:pllc|llp|p\.?c\.?)$|^page \d|^division \S+$|^the people\b|^dated?\b|^signed|^filed|^entered|^/s/|^by the court|^before\b|^ordered\b|attorneys? for|^for (?:the )?(?:plaintiff|defendant|appell|petitioner|respondent)|\b(?:plaintiffs?|defendants?|appell(?:ant|ee)s?|petitioners?|respondents?)[,.]?$|^(?:january|february|march|april|may|june|july|august|september|october|november|december)\s+\d|^\W*$|^mr\.|^mrs\.|^ms\.|^q\.|^a\.\s|\bsee\b|_|\bfor (?:the )?(?:appell|plaintiff|defendant|petitioner|respondent)|\battorney general\b|^(?:the )?hon(?:orable|\.)|\bcourt of (?:appeals|claims|common pleas|errors|criminal appeals)\b|\b(?:district|bankruptcy|supreme|superior|circuit|appellate) court\b$|^in the\b.*\bcourt\b|\bno\.\s*\S*\d|\bjavascript\b").unwrap());
    // A title set apart may cite its statute; a case's name is a citation anywhere.
    if citation.is_match(rest) && (!set_apart || rest.contains(" v. ")) || not_heading.is_match(rest) {
        return false;
    }
    // A table's row is mostly figures; a sentence that wrapped opens as sentences do.
    let tokens = rest.split_whitespace().collect::<Vec<_>>();
    let figures = tokens.iter().filter(|token| token.chars().any(|c| c.is_ascii_digit()) && !token.chars().any(char::is_alphabetic)).count();
    let measured = tokens.iter().any(|token| token.contains('%') || token.contains('$') || (token.contains('.') && token.chars().filter(char::is_ascii_digit).count() >= 2 && !token.chars().any(char::is_alphabetic) && !token.ends_with('.')));
    static OPENS_A_SENTENCE: OnceLock<Regex> = OnceLock::new();
    let sentence = OPENS_A_SENTENCE.get_or_init(|| Regex::new(r"^(?:Moreover|However|Thus|Therefore|Furthermore|Further|Accordingly|Nevertheless|Hence|In addition|The following|It is|IT IS|NOW,)\b|\[\[Image").unwrap());
    if (tokens.len() >= 3 && figures * 2 >= tokens.len()) || (measured && figures >= 2) || sentence.is_match(rest) {
        return false;
    }
    if section_word(rest) && !rest.starts_with(char::is_lowercase) {
        return true;
    }
    if rest.ends_with([',', ';']) || rest.starts_with(char::is_lowercase) {
        return false;
    }
    // A numbered paragraph's first line, a wrapped line, a name.
    static NAME: OnceLock<Regex> = OnceLock::new();
    let last = rest.split_whitespace().next_back().unwrap_or("").to_lowercase();
    let number = body.split(|c: char| !c.is_ascii_digit()).next().and_then(|digits| digits.parse::<u32>().ok());
    if number.is_some_and(|number| number > 20)
        || matches!(last.as_str(), "to" | "the" | "of" | "and" | "or" | "a" | "an" | "in" | "for" | "with" | "that" | "by" | "on" | "at" | "from" | "as" | "is")
        || (!numbered && NAME.get_or_init(|| Regex::new(r"^\p{Lu}[\p{L}'’-]+(?:\s+\p{Lu}\.)+(?:\s+\p{Lu}[\p{L}'’-]+)+(?:,?\s+(?:JR|SR|II|III)\.?)?$").unwrap()).is_match(rest)) {
        return false;
    }
    let letters = rest.chars().filter(|c| c.is_alphabetic()).count();
    let upper = rest.chars().filter(|c| c.is_uppercase()).count();
    let words = rest.split_whitespace().collect::<Vec<_>>();
    let capitals = letters >= 4 && upper * 10 >= letters * 9;
    if capitals {
        return !rest.ends_with('.') || numbered && words.len() <= 4 || words.len() == 1 || rest.starts_with("ON ") || section_keyword(rest);
    }
    // An enumerator over a few words is a title, whatever its capitals: "K. Other discharge issues", "1. Injunction.". A
    // digit's list runs longer than a letter's outline, a stop or a colon says it is a sentence, and a witness answers.
    let said = words.iter().find(|word| !matches!(word.trim_matches(['"', '“', ':', '.']), "A" | "Q")).map(|word| word.trim_matches(['"', '“', '.', ',']));
    let reply = matches!(said, Some("Yes" | "No" | "I" | "That" | "It" | "We" | "He" | "She" | "They" | "Okay" | "Right" | "Correct" | "Sure" | "Yeah" | "Uh-huh" | "Certainly"));
    let digit = body.starts_with(|c: char| c.is_ascii_digit());
    let stopped = rest.ends_with('.');
    if reply && stopped {
        return false;
    }
    let room = if stopped { 3 } else if digit { 7 } else { 12 };
    if numbered && !reply && !body.starts_with('(') && rest.starts_with(char::is_uppercase) && !rest.ends_with(['?', '!', ':']) && !(stopped && rest.contains(',')) && !rest.contains(". ")
        && words.len() <= room && !(digit && !stopped && words.len() < 2) {
        return true;
    }
    let short_title = stopped && words.len() <= 3 && !rest.contains([':', '“', '"']) && !rest.contains(|c: char| c.is_ascii_digit());
    if rest.ends_with(['.', '?', '!']) && !(set_apart && (numbered || short_title)) {
        return false;
    }
    // Set apart with no stop, a block is a title in sentence case as well: "The claim that Bank breached its duty of good faith".
    // A title in capitals has a word of four letters at least: a lone "Id." or "Yes" is none.
    const PLAIN_TITLE: usize = 14;
    let worded = words.iter().any(|word| word.chars().filter(|c| c.is_alphabetic()).count() >= 4);
    let title = worded && heading_title_case(&words) || (set_apart && !rest.ends_with(['.', ':', '?', '!']) && (2..=PLAIN_TITLE).contains(&words.len()));
    (numbered && set_apart && !body.starts_with('(') && words.len() <= 10) || ((numbered || set_apart) && title && words.len() <= 16)
}

/// The title of a list of notes.
#[cfg(feature = "opinion")]
pub(crate) fn notes_title(body: &str) -> bool {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    TITLE.get_or_init(|| Regex::new(r"(?i)^(?:foot|end)?notes?:?$").unwrap()).is_match(body)
}

/// A case's name set as a heading: parties either side of a "v.".
pub(crate) fn caption_heading(body: &str) -> bool {
    static CAPTION: OnceLock<Regex> = OnceLock::new();
    CAPTION.get_or_init(|| Regex::new(r"\bv\.?\s|\bvs\.?\s|^In re\b|^IN RE\b").unwrap()).is_match(body)
}

/// A line of the head by what it says: a citation, an author's line, a court, a docket, a date, counsel, the parties.
pub(crate) fn front_role(body: &str) -> Option<&'static str> {
    static ROLES: OnceLock<[(Regex, &'static str); 11]> = OnceLock::new();
    ROLES.get_or_init(|| [
        (Regex::new(r"^\d{1,4}\s+[A-Z][A-Za-z.\d ]{0,20}\s+\d{1,5}(?:\s*\(\d{4}\))?\.?(?:\s*[;,]\s*\d.*)?$").unwrap(), "citation"),
        (Regex::new(r"(?i)^(?:per curiam\b.*|.{2,60},\s*(?:u\.\s?s\.\s+|united states\s+)?(?:(?:chief|senior|presiding|associate)\s+)?(?:(?:district|circuit|magistrate|bankruptcy)\s+)?(?:judge|justice)(?:\s*\((?:concurring|dissenting)[^)]*\))?[.:]?|.{2,60},\s*(?:[cp]\.\s*)?j\.[,:]?|(?:mr\.\s+(?:chief\s+)?|chief\s+)justice\s+\S.{0,40}[:.]|.{0,60}deliver(?:ing|ed) the opinion of the court\b.*|(?:specially\s+)?(?:concurring|dissenting)[.:,]?|opinion[,:])$").unwrap(), "author"),
        (Regex::new(r"(?i)^(?:appeals? from|on appeal from|on petition for (?:review|writ)|petition for review|trial court\b|lower tribunal\b)").unwrap(), "lower_court"),
        (Regex::new(r"(?i)^(?:re|subject)\s*:").unwrap(), "subject"),
        (Regex::new(r"(?i)^dear\s").unwrap(), "addressee"),
        (Regex::new(r"(?i)\b(?:court of appeals?|supreme court|district court|bankruptcy court|court of|tribunal|circuit)\b").unwrap(), "court"),
        (Regex::new(r"(?i)^(?:(?:case|docket|civil action|cause|appeal|criminal|civil|bankruptcy|adversary|adv\.?|index|c/a|cv|cr|opinion)\s*(?:nos?\.?|numbers?|action)|nos?\.\s*\S*\d)").unwrap(), "docket"),
        (Regex::new(r"(?i)^(?:before|panel|coram|present)\b|\b(?:circuit|district|chief|magistrate|senior) judges?\b").unwrap(), "judges"),
        (Regex::new(r"(?i)^(?:(?:filed|decided|argued|submitted|dated|entered|heard|delivered|rehearing|date of|opinion filed|date filed)\b.*\d{4}|(?:january|february|march|april|may|june|july|august|september|october|november|december)\s+\d{1,2},\s+\d{4}\.?$)").unwrap(), "date"),
        (Regex::new(r"(?i)\b(?:for (?:the )?(?:plaintiff|defendant|appell|petitioner|respondent)|attorneys? for|counsel for|on (?:the )?brief)").unwrap(), "counsel"),
        (Regex::new(r"(?i)\b(?:plaintiffs?|defendants?|appell(?:ant|ee)s?|petitioners?|respondents?)\b|^v\.?$|^vs\.?$").unwrap(), "caption"),
    ]).iter().find(|(pattern, _)| pattern.is_match(body)).map(|(_, role)| *role)
}

/// A number and nothing else, as a note's label stands when its note has been read: "[6]", "24".
pub(crate) fn bare_label(words: &str) -> bool {
    words.len() <= 8 && words.chars().any(|c| c.is_ascii_digit()) && words.chars().all(|c| c.is_ascii_digit() || matches!(c, '[' | ']' | '(' | ')' | '.'))
}

/// A line of a table of contents or authorities: its dot leaders and page.
pub(crate) fn contents_line(body: &str) -> bool {
    static LEADERS: OnceLock<Regex> = OnceLock::new();
    static TIGHT: OnceLock<Regex> = OnceLock::new();
    // A paragraph may end on a reporter's page, "99 S.Ct.2210": a contents line without its leaders is a short one.
    LEADERS.get_or_init(|| Regex::new(r"(?:\.{4,}|(?:\. ){4,}|(?:_ ){4,})\s*(?:passim|\d{1,4}(?:[,\s]+\d{1,4})*)\s*$").unwrap()).is_match(body)
        || (body.len() <= 200 && TIGHT.get_or_init(|| Regex::new(r"[\p{L})”]\.{1,3}\s?\d{3,4}\s*$").unwrap()).is_match(body))
}

/// A paragraph's printed number opening a line, in each form it takes: "¶ 12" ("[¶ 12.]", "¶¶ 12"), a scanner's "If 12" or
/// "T 12" for "¶ 12", the "12]" it leaves of "[¶ 12]", "{12}", "[12]" (or "[12J"), "12." and "12)". The number is the one
/// capture group that matched.
fn paragraph_mark() -> &'static Regex {
    static MARK: OnceLock<Regex> = OnceLock::new();
    MARK.get_or_init(|| Regex::new(r"^(?:[\[{(]?¶¶?\s*(\d{1,4})\.?[\]})]?|\[?I?[Tf] ?(\d{1,4})\]?\s|(\d{1,4})\]\s|\{\s*(\d{1,4})\s*\}?|\[ ?(\d{1,4})[\]jJ]|(\d{1,3})[.)](?:\s|$|\p{Lu}))").unwrap())
}

/// A numbered line's words past its number.
pub(crate) fn past_marker(body: &str) -> &str {
    paragraph_mark().find(body).map_or(body, |found| body[found.end()..].trim_start())
}

/// A paragraph's printed number and its family: 1 "¶ 12" (or the "12]" a scanner leaves of "[¶ 12]"), 2 "{12}", 3 "[12]", 4 "12.".
pub(crate) fn paragraph_marker(body: &str) -> Option<(u32, usize)> {
    let body = body.trim_start_matches('•');
    // A headnote's mark may stand first: "[5] [¶ 17] In McAdoo".
    let body = body.split_once(']').filter(|(mark, rest)| mark.starts_with('[') && mark[1..].bytes().all(|byte| byte.is_ascii_digit()) && rest.trim_start().trim_start_matches(['[', '{']).starts_with('¶')).map_or(body, |(_, rest)| rest.trim_start());
    let capture = paragraph_mark().captures(body)?;
    let group = (1..=6).find(|&group| capture.get(group).is_some())?;
    let bare = group == 2 || group == 1 && body.starts_with('¶');
    Some((capture[group].parse().ok()?, match group { _ if bare => 5, 1 | 3 => 1, 4 => 2, 5 => 3, _ => 4 }))
}

/// A mark that names itself a paragraph's: "¶ 12", "[¶ 12]", "{12}".
pub(crate) fn says_paragraph(body: &str) -> bool {
    paragraph_marker(body).is_some_and(|(_, family)| matches!(family, 1 | 2 | 5))
}

/// A text's sentences, as ranges of bytes. A stop ends one before a capital or the end, unless it closes an abbreviation
/// or a number: "I.R.S.", "Mr.", "Doc. 6", "$11,000.00".
pub(crate) fn sentences(raw: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut start = 0;
    for (at, stop) in raw.char_indices().filter(|(_, c)| matches!(c, '.' | '!' | '?' | ';')) {
        let after = raw[at + stop.len_utf8()..].trim_start_matches(['"', '”', '’', ')', ']']);
        let next = after.trim_start();
        let spaced = after.len() != next.len() || next.is_empty();
        let word = raw[..at].rsplit(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
        let short = stop == '.' && word.len() <= 3 && (word.starts_with(char::is_uppercase) || word.is_empty() || word.bytes().all(|byte| byte.is_ascii_digit()));
        if stop == ';' || spaced && !short && (next.is_empty() || next.starts_with(|c: char| c.is_uppercase() || c == '“' || c == '"')) {
            let end = at + stop.len_utf8();
            found.push((start, end));
            start = end;
        }
    }
    if raw[start..].chars().any(char::is_alphanumeric) {
        found.push((start, raw.len()));
    }
    found
}

/// A block its markup sets wholly in emphasis or centred.
pub(crate) fn emphasized_whole(structure: &DocumentStructure, node: &StructureNode) -> bool {
    let Some((at, element)) = element_of(structure, node) else { return false };
    if node.source != Derivation::Native { return false; }
    let whole = element.range.end - element.range.start;
    structure.markup_elements[at + 1..].iter().take_while(|child| child.markup_start < element.markup_end)
        .any(|child| matches!(child.tag.as_str(), "em" | "i" | "strong" | "b" | "u" | "center" | "underline")
            && (child.range.end - child.range.start) * 10 >= whole * 9)
        || element.attributes.get("align").is_some_and(|align| align.eq_ignore_ascii_case("center"))
}
