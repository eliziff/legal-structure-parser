//! A CourtListener rendition's pages, read once: the marks its markup prints (the engine's star pages), the pages a
//! numbering's first mark implies before it, the leaves and breaks its elements name, each page's printed mark, whether a
//! note holds it, and the run its numbering keeps.

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;
use legal_structure_model::document::{Derivation, DocumentStructure, NodeKind, StructureNode};
use legal_structure_model::text::{javascript_whitespace, utf16_len, ScalarText};
use legal_structure_model::ScalarRange;
use regex::Regex;
use crate::text::ScalarCoordinates;
use crate::trim_javascript_whitespace as trim_js;

/// A page the engine reads off the text rather than a mark: its label, numbering, start, end (0 where the next page ends
/// it), printed mark and how it was found (`grammar`: `opening`, `pgmap`, `counted`, `break`, or `markup` for a mark).
fn read_page(label: impl std::fmt::Display, series: impl Into<String>, start: usize, end: usize, marker: Option<ScalarRange>, source: &str) -> StructureNode {
    let label = label.to_string();
    let series = series.into();
    let derivation = if source == "markup" { Derivation::Native } else { Derivation::Heuristic };
    let mut page = StructureNode::new(format!("page-{series}-{label}-{start}"), NodeKind::Page, ScalarRange { start, end }, crate::ENGINE_ORIGIN, derivation, None);
    page.label = Some(label);
    page.series = series;
    page.marker_range = marker;
    page.grammar = Some(source.to_owned());
    page
}

/// Every page of the rendition, final: each star page's numbering named (a reporter's where its mark names none, a run of its
/// own within it, "Supp. 12", apart), the pages its marks imply and its elements name added, each page's printed mark found
/// where its node holds none, `in_note` where a note holds it, and a numbering's strays and moved blocks' pages marked.
pub(super) fn courtlistener_pages(structure: &mut DocumentStructure, text_index: &ScalarCoordinates) {
    let (mut pages, nodes): (Vec<_>, Vec<_>) = std::mem::take(&mut structure.nodes).into_iter().partition(|node| node.kind == NodeKind::Page);
    structure.nodes = nodes;
    let coordinates = ScalarText::with_coordinates(structure.query_text(), text_index);
    let notes = structure.nodes.iter().filter(|node| node.kind == NodeKind::Footnote)
        .flat_map(|note| if note.ranges.is_empty() { vec![note.range] } else { note.ranges.clone() }).collect::<Vec<_>>();
    for page in &mut pages {
        let series = series_of(page);
        page.series = page.label.as_deref().and_then(label_run).map_or_else(|| series.clone(), |run| format!("{series}/{run}"));
        // A page the engine read off the text's layout keeps how it was found (`inference`'s "furniture", "break").
        page.grammar.get_or_insert_with(|| "markup".to_owned());
    }
    opening_pages(structure, &coordinates, &notes, &mut pages);
    element_pages(structure, &coordinates, &notes, &mut pages);
    pages.sort_by_key(|page| page.range.start);
    for page in &mut pages {
        if notes.iter().any(|note| note.start <= page.range.start && page.range.start < note.end) {
            page.properties.insert("in_note".to_owned(), String::new());
        } else {
            page.properties.remove("in_note");
        }
        page.marker_range = page.marker_range.or_else(|| printed_mark(&coordinates, page.range.start, page.range.end, page.label.as_deref().unwrap_or("")));
    }
    never_down(structure, &mut pages);
    structure.nodes.extend(pages);
}

/// The page mark printed at a page's start, in whatever words it carries its label: "*645", "*Page 169", "*West Page 12",
/// "*Supp. 12", "*[12]", "*B5", "**709"; or "Page 382" alone in its block.
pub(crate) fn printed_mark(coordinates: &ScalarText, at: usize, end: usize, label: &str) -> Option<ScalarRange> {
    static ALONE: OnceLock<Regex> = OnceLock::new();
    let head = (0..4).find_map(|short| coordinates.slice_utf16(at..(at + 48 - short).min(end)))?;
    let body = head.trim_start_matches(javascript_whitespace);
    let start = at + utf16_len(&head[..head.len() - body.len()]);
    // "[*5]" as "*5", its bracket closing with its label.
    let framed = body.starts_with("[*");
    let inner = if framed { &body[1..] } else { body };
    let stars = inner.len() - inner.trim_start_matches('*').len();
    if stars == 0 {
        let found = ALONE.get_or_init(|| Regex::new(r"^Page\s+\d{1,6}\s*$").unwrap()).find(body)?;
        return Some(ScalarRange { start, end: start + utf16_len(found.as_str().trim_end()) });
    }
    let rest = &inner[stars..];
    let place = if label.is_empty() { 0 } else { rest.find(label)? };
    if place > 24 || !rest[..place].chars().all(|c| c.is_alphabetic() || matches!(c, ' ' | '\t' | '\n' | '\r' | '.' | '[' | '(')) {
        return None;
    }
    let past = place + label.len();
    let closed = past + rest[past..].len() - rest[past..].trim_start_matches([']', ')', '.']).len();
    Some(ScalarRange { start, end: start + usize::from(framed) + utf16_len(&inner[..stars + closed]) })
}

/// A printed run of its own within a reporter: "Supp. 12", "CJP Supp. 12", "B5", "R5" do not count with its pages.
fn label_run(label: &str) -> Option<&str> {
    let run = label.split(|c: char| c.is_ascii_digit()).next()?.trim();
    (!run.is_empty() && run.len() < label.len() && run.chars().any(char::is_alphabetic)).then_some(run)
}

/// A page label's number: its digits past any prefix, or a Roman numeral reporters print for a thousand and more.
pub(crate) fn page_number(label: &str) -> Option<u32> {
    let digits = label.trim_start_matches(|c: char| !c.is_ascii_digit());
    if let Some(number) = digits.split(|c: char| !c.is_ascii_digit()).next().filter(|part| !part.is_empty()).and_then(|part| part.parse().ok()) {
        return Some(number);
    }
    legal_structure_model::layout::roman_value(label).filter(|_| label.bytes().all(|byte| byte.is_ascii_uppercase()))
}

/// A markup page's numbering, as the markup reader read it from the mark; a mark that names none is the reporter's.
fn series_of(node: &StructureNode) -> String {
    if node.series.is_empty() { "reporter".to_owned() } else { node.series.clone() }
}

/// The page printed before a page: 289 before 290, and an inserted page's own run, 595 before 595a and 595a before 595b.
fn previous_page(printed: &str) -> Option<String> {
    if super::short_number(printed) {
        return printed.parse::<u32>().ok().filter(|number| *number >= 2).map(|number| (number - 1).to_string());
    }
    let letter = printed.chars().next_back().filter(char::is_ascii_lowercase)?;
    let digits = &printed[..printed.len() - 1];
    if !super::short_number(digits) {
        return None;
    }
    Some(if letter == 'a' { digits.to_owned() } else { format!("{digits}{}", (letter as u8 - 1) as char) })
}

/// The text before a numbering's first mark in a writing sits on the page before it: "*289" opens page 289, and what precedes
/// it in its writing (an opinion part, a Westlaw opinion block, else the text) is page 288. A page a note holds is the note's;
/// another numbering's page may cover the same text, this numbering's may not.
fn opening_pages(structure: &DocumentStructure, coordinates: &ScalarText, notes: &[ScalarRange], pages: &mut Vec<StructureNode>) {
    let writings = structure.markup_elements.iter().filter(|element| super::declared_writing(element))
        .map(|element| element.range).collect::<Vec<_>>();
    let writing_of = |at: usize| writings.iter().filter(|writing| writing.start <= at && at < writing.end).map(|writing| writing.start).max().unwrap_or(0);
    let mut firsts = BTreeMap::<(&str, usize), usize>::new();
    for (index, page) in pages.iter().enumerate().filter(|(_, page)| !notes.iter().any(|note| note.start <= page.range.start && page.range.start < note.end)) {
        let first = firsts.entry((page.series.as_str(), writing_of(page.range.start))).or_insert(index);
        if page.range.start < pages[*first].range.start {
            *first = index;
        }
    }
    let mut opening = Vec::new();
    for ((series, scope), first) in firsts {
        let end = pages[first].range.start;
        let (Some(before), Some(gap)) = (previous_page(pages[first].label.as_deref().unwrap_or("")), coordinates.slice_utf16(scope..end)) else { continue };
        let words = gap.trim_start_matches(javascript_whitespace);
        if trim_js(words).is_empty() {
            continue;
        }
        let start = scope + utf16_len(&gap[..gap.len() - words.len()]);
        if !pages.iter().any(|page| page.series == series && page.range.start < end && start < page.range.end) {
            opening.push(read_page(before, series.to_owned(), start, end, None, "opening"));
        }
    }
    pages.extend(opening);
}

/// Pages no mark prints. Harvard's `pgmap` gives the volume's leaves a block stands on, "316", or for a block that turns a
/// page each leaf and how many words of the block fall on it, "316(74) 317(17)": `leaves_to_pages` makes them pages. Colorado's page breaks are a `div.pagebreak` and
/// its number, or an empty `span.ldml-pagenumber`, which says only that a page turns.
fn element_pages(structure: &DocumentStructure, coordinates: &ScalarText, notes: &[ScalarRange], pages: &mut Vec<StructureNode>) {
    let mut known = pages.iter().filter(|page| page.series.starts_with("reporter")).map(|page| page.label.as_deref().unwrap_or("").to_owned()).collect::<HashSet<_>>();
    let page_at = |label: String, series: &str, start: usize, marker: Option<ScalarRange>, source: &'static str| {
        read_page(label, series.to_owned(), start, 0, marker, source)
    };
    let (mut broken, mut blank) = (Vec::new(), Vec::new());
    for element in &structure.markup_elements {
        let class = element.attributes.get("class").map_or("", String::as_str);
        if let Some(map) = element.attributes.get("pgmap") {
            if notes.iter().any(|note| note.start <= element.range.start && element.range.start < note.end) {
                continue;
            }
            let body = coordinates.slice_utf16(element.range.start..element.range.end).unwrap_or("");
            let mut words = 0;
            for entry in map.split_whitespace() {
                let (label, count) = entry.split_once('(').map_or((entry, 0), |(label, count)| (label, count.trim_end_matches(')').parse().unwrap_or(0)));
                if label.bytes().all(|byte| byte.is_ascii_digit()) && !label.is_empty() && known.insert(label.to_owned()) {
                    pages.push(page_at(label.to_owned(), "reporter-1", element.range.start + word_place(body, words), None, "pgmap"));
                }
                words += count;
            }
        } else if class == "pagebreak" {
            let printed = coordinates.slice_utf16(element.range.start..element.range.end).map(trim_js).unwrap_or("");
            if let Ok(number) = printed.parse::<u32>() {
                broken.push((number, element.range));
            }
        } else if class == "ldml-pagenumber" {
            blank.push(element.range);
        }
    }
    if let Some(first) = broken.first().map(|mark| mark.0).filter(|first| *first > 1) {
        pages.push(page_at((first - 1).to_string(), "slip", 0, None, "counted"));
    }
    for (number, range) in broken {
        pages.push(page_at(number.to_string(), "slip", range.start, Some(range), "markup"));
    }
    if pages.is_empty() && !blank.is_empty() {
        pages.push(page_at("1".to_owned(), "sheet", 0, None, "break"));
        for (index, range) in blank.into_iter().enumerate() {
            pages.push(page_at((index + 2).to_string(), "sheet", range.start, None, "break"));
        }
    }
}

/// Rule one of a printed numbering: read in order, its pages never go down. In the opinion's own text a mark lower than one
/// before it is the source's error ("*1308 controversy ... *1307" inside the block quotation that follows): the heaviest run that
/// never falls keeps its marks and the rest are strays. A mark in a block the source moved ahead of the opinion ("*462" on counsel
/// printed after it) is that block's page and takes no part in the run; it numbers no text after its block.
fn never_down(structure: &DocumentStructure, pages: &mut [StructureNode]) {
    let moved = structure.markup_elements.iter().filter(|element| MOVED_BLOCKS.contains(&element.tag.as_str())).map(|element| element.range).collect::<Vec<_>>();
    let in_moved = |start: usize| moved.iter().any(|block| block.start <= start && start < block.end);
    let mut series = pages.iter().filter(|page| page.series.starts_with("reporter") && in_flow(page)).map(|page| page.series.clone()).collect::<Vec<_>>();
    series.sort();
    series.dedup();
    for name in series {
        let mut marks = pages.iter().enumerate().filter(|(_, page)| page.series == name && in_flow(page) && !in_moved(page.range.start))
            .filter_map(|(at, page)| Some((page.range.start, at, page_number(page.label.as_deref().unwrap_or(""))?))).collect::<Vec<_>>();
        marks.sort_unstable();
        // The longest run that never falls, by patience: each mark extends the longest run ending at or below its number.
        let (mut best, mut back) = (vec![1usize; marks.len()], vec![usize::MAX; marks.len()]);
        for at in 0..marks.len() {
            for before in 0..at {
                if marks[before].2 <= marks[at].2 && best[before] + 1 > best[at] {
                    best[at] = best[before] + 1;
                    back[at] = before;
                }
            }
        }
        let Some(mut at) = (0..marks.len()).max_by_key(|at| (best[*at], std::cmp::Reverse(*at))) else { continue };
        let mut kept = HashSet::new();
        loop {
            kept.insert(marks[at].1);
            if back[at] == usize::MAX { break; }
            at = back[at];
        }
        for (_, index, _) in &marks {
            if !kept.contains(index) {
                pages[*index].grammar = Some(("stray").to_owned());
            }
        }
        // A moved block's mark: its page holds the block and ends with it.
        for page in pages.iter_mut().filter(|page| page.series == name && !page.properties.contains_key("in_note") && in_moved(page.range.start)) {
            if let Some(block) = moved.iter().find(|block| block.start <= page.range.start && page.range.start < block.end) {
                page.range.end = block.end;
                page.grammar = Some(("moved").to_owned());
            }
        }
    }
}

/// Head matter Harvard sets ahead of the opinion, wherever the book printed it: counsel printed on the case's last page, dates,
/// the syllabus. A mark inside one numbers that block alone.
const MOVED_BLOCKS: [&str; 12] = ["attorneys", "otherdate", "decisiondate", "docketnumber", "parties", "judges", "syllabus", "summary", "headnotes", "court", "seealso", "citation"];

/// A page mark of the opinion's own run of pages: neither one a note carries on to its next page nor a stray mark.
pub fn in_flow(page: &StructureNode) -> bool {
    !page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("stray")
}

/// Where the `count`th word of a text starts.
fn word_place(text: &str, count: usize) -> usize {
    let mut seen = 0;
    let mut inside = false;
    let mut unit = 0;
    for c in text.chars() {
        if c.is_whitespace() {
            inside = false;
        } else if !inside {
            inside = true;
            seen += 1;
            if seen > count {
                return unit;
            }
        }
        unit += c.len_utf16();
    }
    unit
}
