//! A statute's reading past its provisions, read from a provider's text of it as nodes: its headings (its parts,
//! divisions and schedules) in their hierarchy, each provision's marginal note, and the title over it all.

use crate::case_outline::{citation_title, front_line};
use crate::statute::{markdown_line, marginal_note_shaped};
use crate::{
    provision_mark, utf16_len, Derivation, DocumentStructure, EngineError, NodeKind, PrimaryRole, ProviderTextInput,
    ProviderTextSourceKind, ScalarRange, ScalarText, StructureNode,
};
use std::collections::{HashMap, HashSet};

/// A line of the text: its span, where its words open, and its content.
struct TextLine<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

/// A heading line's words after its markdown marks: the kind its first word names ("part", "division", "schedule"),
/// and where that word and its number end ("PART XXIII", "Part 2", "Division A").
fn heading(words: &str) -> (Option<&'static str>, usize) {
    let mut tokens = words.split_whitespace().map(|token| (token.as_ptr() as usize - words.as_ptr() as usize, token));
    let first = tokens.next().unwrap_or_default();
    let kind = match first.1.trim_matches('*').to_lowercase().as_str() {
        "part" | "partie" => Some("part"),
        "division" => Some("division"),
        "schedule" | "annexe" | "appendix" => Some("schedule"),
        _ => None,
    };
    if kind.is_none() {
        return (None, 0);
    }
    // Its number: digits, a Roman numeral or a letter.
    let number = tokens.next().filter(|(_, word)| {
        let word = word.trim_matches('*').trim_end_matches(['.', ':']);
        word.chars().any(|c| c.is_ascii_digit())
            || !word.is_empty() && word.chars().all(|c| "IVXLCDM".contains(c))
            || word.chars().count() == 1 && word.chars().all(|c| c.is_ascii_uppercase())
    });
    let (at, printed) = number.unwrap_or(first);
    (kind, at + printed.trim_end_matches(['.', ':']).len())
}

/// A statute's headings, marginal notes and title as nodes, written on the structure its provisions were read into: a
/// markdown heading ("## PART XXIII – Sentencing") under the heading with fewer marks before it, open until the next
/// heading or provision; the document's own title ("# Criminal Code"), and a line before its first heading or
/// provision naming it by its citation, as its title. A marginal note stands on its line just above its provision,
/// blank lines aside: a section's in the shape of one, a subsection's set in bold alone; a section's first subsection,
/// opening on the section's line, has the section's note above it, not one of its own.
pub(crate) fn add_statute_nodes(structure: &mut DocumentStructure) {
    let text = structure.text.as_str();
    let mut lines = Vec::new();
    let mut unit = 0;
    for raw in text.split_inclusive('\n') {
        let content = raw.trim_end_matches(['\n', '\r']);
        lines.push(TextLine { start: unit, end: unit + utf16_len(content), text: content });
        unit += utf16_len(raw);
    }
    let length = unit;
    let mut provisions = structure.nodes.iter()
        .filter(|node| node.kind == NodeKind::Section)
        .map(|node| node.range.start)
        .collect::<Vec<_>>();
    provisions.sort_unstable();
    let first_provision = provisions.first().copied().unwrap_or(length);
    let mut added = Vec::<StructureNode>::new();
    let mut open = Vec::<(usize, String, usize)>::new();
    let mut opened = false;
    for line in &lines {
        let trimmed = line.text.trim_start();
        let lead = utf16_len(&line.text[..line.text.len() - trimmed.len()]);
        let marks = trimmed.chars().take_while(|c| *c == '#').count();
        let words_at = |byte: usize| line.start + lead + utf16_len(&trimmed[..byte]);
        let markdown = marks > 0 && trimmed[marks..].starts_with(' ');
        if markdown && provisions.get(provisions.partition_point(|&start| start < line.start))
            .is_some_and(|&start| start < line.end.max(line.start + 1)) {
            continue;
        }
        if markdown && marks == 1 || !markdown && !opened && line.end <= first_provision && citation_title(markdown_line(line.text).0) {
            let id = format!("statute-span-{:06}", added.len() + 1);
            added.push(front_line(id, PrimaryRole::Title, line.start + lead, line.end));
            continue;
        }
        if !markdown {
            continue;
        }
        opened = true;
        let words = trimmed[marks..].trim();
        let words_start = marks + (trimmed[marks..].len() - trimmed[marks..].trim_start().len());
        let (kind, marker_end) = heading(words);
        while open.last().is_some_and(|(depth, ..)| *depth >= marks) {
            open.pop();
        }
        let (parent, level) = open.last().map_or((None, 1), |(_, id, level)| (Some(id.clone()), level + 1));
        let id = format!("statute-heading-{:06}", added.len() + 1);
        let mut node = StructureNode::new(id.clone(), NodeKind::Heading, ScalarRange { start: line.start, end: length },
            crate::ENGINE_ORIGIN, Derivation::Heuristic, parent);
        node.level = Some(level);
        node.heading_range = Some(ScalarRange { start: words_at(words_start), end: line.end });
        node.marker_range = (marker_end > 0).then(|| ScalarRange { start: words_at(words_start), end: words_at(words_start + marker_end) });
        node.locator_kind = kind.map(str::to_owned);
        node.label = Some(markdown_line(words).0.to_owned());
        added.push(node);
        open.push((marks, id, level));
    }
    // A heading heads what follows it until the next heading or provision.
    let mut starts = added.iter().filter(|node| node.kind == NodeKind::Heading).map(|node| node.range.start)
        .chain(provisions.iter().copied()).collect::<Vec<_>>();
    starts.sort_unstable();
    for node in added.iter_mut().filter(|node| node.kind == NodeKind::Heading) {
        let start = node.range.start;
        node.range.end = starts.get(starts.partition_point(|&next| next <= start)).copied().unwrap_or(length);
    }
    // Each provision's marginal note.
    let indexed = ScalarText::new(text);
    let printed = |node: &StructureNode| node.marker_range
        .and_then(|range| indexed.slice_utf16(range.start..range.end));
    let mut noted = HashSet::new();
    let ids = structure.nodes.iter().enumerate().map(|(at, node)| (node.id.clone(), at)).collect::<HashMap<_, _>>();
    let mut notes = Vec::new();
    for (at, node) in structure.nodes.iter().enumerate() {
        if node.kind != NodeKind::Section || provision_mark(node.locator_kind.as_deref()) != "sec" {
            continue;
        }
        let top = node.parent_id.as_ref().and_then(|parent| ids.get(parent)).is_none_or(|&parent| structure.nodes[parent].kind != NodeKind::Section);
        let subsection = !top && printed(node).is_some_and(|marker| marker.trim_start().strip_prefix('(').is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit())));
        if !top && !subsection {
            continue;
        }
        let start = node.range.start;
        let Some(above) = lines.partition_point(|line| line.end <= start && line.start < start).checked_sub(1) else { continue };
        let Some(line) = lines[..=above].iter().rev().find(|line| !line.text.trim().is_empty()) else { continue };
        let (_, emphasized) = markdown_line(line.text);
        if line.text.trim_start().starts_with('#') || !marginal_note_shaped(line.text) || !top && !emphasized || !noted.insert(line.start) {
            continue;
        }
        let lead = utf16_len(&line.text[..line.text.len() - line.text.trim_start().len()]);
        notes.push((at, ScalarRange { start: line.start + lead, end: line.end }));
    }
    for (at, note) in notes {
        structure.nodes[at].heading_range = Some(note);
    }
    structure.nodes.extend(added);
}

/// A statute's text read into its provisions, headings and notes (`add_statute_nodes`). Text run together, a page to a
/// line (a PDF's page text), is read again with its lines recovered where provisions open, and the reading that finds
/// more sections is kept; the recovery only breaks lines, so offsets into the text hold.
pub(crate) fn statute_reading(text: &str) -> Result<DocumentStructure, EngineError> {
    let provider = crate::provider_text::provider_text_reading(ProviderTextInput::new("", ProviderTextSourceKind::Laws, text))?;
    let sections = |structure: &DocumentStructure| structure.nodes.iter()
        .filter(|node| node.kind == NodeKind::Section && provision_mark(node.locator_kind.as_deref()) == "sec"
            && node.label.as_deref().is_some_and(|label| !label.contains('(')))
        .count();
    let mut structure = if sections(&provider) * 200 < text.len().max(1) {
        let lineated = crate::instrument::instrument_reading(text)?;
        if sections(&lineated) > sections(&provider) { lineated } else { provider }
    } else {
        provider
    };
    add_statute_nodes(&mut structure);
    Ok(structure)
}

/// Where each provision `labels` cite is in a statute's text: a section or article ("16"), a part of one ("16(3)(a)"),
/// or a range ("49-51", "33 to 35") from its first provision through its last; each span (UTF-16) from its marginal
/// note through everything under it, in the order the text has them. A label the text holds no provision of has none.
pub fn statute_provision_spans(text: &str, labels: &[String]) -> Result<Vec<[usize; 2]>, EngineError> {
    static RANGE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\s*(?:-|\u{2013}|to)\s*").unwrap());
    let mut structure = statute_reading(text)?;
    legal_structure_model::assemble_in_place(&mut structure).map_err(EngineError::invalid)?;
    let span = |label: &str| {
        let compact = label.chars().filter(|character| !character.is_whitespace()).collect::<String>();
        let compact = compact.trim_end_matches('.');
        let node = structure.nodes.iter().find(|node| node.kind == NodeKind::Section
            && provision_mark(node.locator_kind.as_deref()) == "sec"
            && node.label.as_deref().is_some_and(|label| label.trim_end_matches('.') == compact))?;
        let start = node.heading_range.map_or(node.range.start, |note| note.start.min(node.range.start));
        Some([start, node.extent.iter().map(|range| range.end).max().unwrap_or(node.range.end)])
    };
    let mut spans = labels.iter().filter_map(|label| {
        if let Some(whole) = span(label) {
            return Some(whole);
        }
        let mut ends = RANGE.split(label);
        let (first, last) = (span(ends.next().unwrap_or("")), ends.next().and_then(|to| (!to.is_empty()).then(|| span(to)).flatten()));
        Some([first?[0], last?[1]])
    }).collect::<Vec<_>>();
    spans.sort_by_key(|span| span[0]);
    Ok(spans)
}
