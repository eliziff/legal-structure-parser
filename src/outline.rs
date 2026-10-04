//! A document's own outline: its headings and its top-level sections, in order.

use crate::{DocumentStructure, NodeKind, PrintedStatute, StructureNode};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlineEntry {
    /// "heading" or "section".
    kind: &'static str,
    /// Headings nest by their own hierarchy; a section sits under the heading before it.
    level: usize,
    title: String,
    /// The node's start in the document's query text.
    start: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_index: Option<usize>,
}

/// A section label as a reader cites it: "sec33.1" is "s 33.1", "part2" "Part 2".
fn section_title(label: &str) -> String {
    for (prefix, name) in [("sec", "s "), ("part", "Part "), ("sched", "Schedule "),
        ("art", "Art "), ("ann", "Annex "), ("app", "Appendix ")] {
        if let Some(rest) = label.strip_prefix(prefix).filter(|rest| !rest.is_empty()) {
            return format!("{name}{rest}");
        }
    }
    label.to_owned()
}

fn outline_entries(structure: &DocumentStructure, sections: bool) -> Vec<OutlineEntry> {
    let nodes = structure.nodes.iter().map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let depth = |node: &StructureNode, kind: NodeKind| {
        let (mut level, mut parent) = (0, node.parent_id.as_deref());
        while let Some(found) = parent.and_then(|id| nodes.get(id)).filter(|_| level < 16) {
            if found.kind == kind { level += 1; }
            parent = found.parent_id.as_deref();
        }
        level
    };
    structure.nodes.iter().filter_map(|node| {
        let start = node.rendered_range.unwrap_or(node.range).start;
        let page_index = node.page_indexes.first().copied();
        let label = node.label.as_deref()?.split_whitespace().collect::<Vec<_>>().join(" ");
        match node.kind {
            NodeKind::Heading if !label.is_empty() && label.chars().count() <= 200 =>
                Some(OutlineEntry { kind: "heading", level: depth(node, NodeKind::Heading),
                    title: label, start, page_index }),
            NodeKind::Section if sections && depth(node, NodeKind::Section) == 0 => Some(OutlineEntry {
                kind: "section", level: 0, title: section_title(&label), start, page_index }),
            _ => None,
        }
    }).collect()
}

/// The document's own outline: its headings and its top-level sections, in order. A PDF's
/// numbered sections come from `printed`, the reading of its body as a statute, given only
/// for legislation, and only where its sections each appear once, in order; a judgment's
/// numbers are not sections.
pub fn document_outline(structure: &DocumentStructure, pdf: bool, printed: Option<&PrintedStatute>) -> Vec<OutlineEntry> {
    let mut entries = outline_entries(structure, !pdf);
    if let Some(instrument) = printed {
        let sections = outline_entries(instrument.structure(), true).into_iter()
            .filter(|entry| entry.kind == "section").collect::<Vec<_>>();
        let number = |title: &str| title.strip_prefix("s ")
            .and_then(|rest| rest.split('.').next()?.parse::<u32>().ok());
        let ordered = !sections.is_empty() && sections.windows(2).all(|pair|
            matches!((number(&pair[0].title), number(&pair[1].title)), (Some(a), Some(b)) if a < b
                || a == b && pair[0].title < pair[1].title));
        if ordered {
            // A section opens where the PDF paragraph it starts in does.
            entries.extend(sections.into_iter().filter_map(|entry| {
                let node = &structure.nodes[instrument.parts(entry.start, entry.start + 1).next()?.0];
                Some(OutlineEntry { start: node.rendered_range.unwrap_or(node.range).start,
                    page_index: node.page_indexes.first().copied(), ..entry })
            }));
        }
    }
    // A title printed atop every page is a running head, not a heading of the text.
    let mut counts = HashMap::<String, usize>::new();
    for entry in entries.iter().filter(|entry| entry.kind == "heading") {
        *counts.entry(entry.title.to_lowercase()).or_default() += 1;
    }
    entries.retain(|entry| entry.kind != "heading" || counts[&entry.title.to_lowercase()] < 3);
    entries.sort_by_key(|entry| entry.start);
    // The matter before a judgment's reasons (the style of cause, the court, the parties and
    // counsel, a date) is no heading of the text: the outline opens with the headings that run
    // straight into its first numbered paragraph, as its case outline reads it ("[1]"), with no
    // text between them. An enactment's title pages (its title block, assent, a consolidation's
    // cover) are the pages before its first section.
    let start = |node: &StructureNode| node.rendered_range.unwrap_or(node.range).start;
    // Entries start in the query text: the rendered text where the document has one.
    let text = structure.rendered_text.as_deref().unwrap_or(&structure.text);
    let paragraph = crate::case_outline(text).ok().and_then(|outline| outline.into_iter()
        .find(|entry| entry.kind == "paragraph")).map(|entry| {
            let (mut utf16, mut scalar) = (0, 0);
            for character in text.chars() {
                if utf16 >= entry.start { break; }
                utf16 += character.len_utf16();
                scalar += 1;
            }
            scalar
        });
    if paragraph.is_none() {
        // A judgment's section nodes are a misreading; an enactment's sections are its printed reading's.
        if let Some(page) = entries.iter().filter(|entry| entry.kind == "section").filter_map(|entry| entry.page_index).min() {
            entries.retain(|entry| entry.kind != "heading" || entry.page_index.is_none_or(|at| at >= page));
        }
    }
    if let Some(body) = paragraph {
        let mut opens = body;
        for entry in entries.iter().rev().filter(|entry| entry.kind == "heading" && entry.start < body) {
            // Text lying wholly between this heading and what follows it; the paragraph that
            // opens the body, whatever its parts, is not between.
            let text_between = structure.nodes.iter().any(|node| matches!(node.kind,
                NodeKind::Prose | NodeKind::List | NodeKind::Table | NodeKind::Paragraph)
                && start(node) > entry.start && node.rendered_range.unwrap_or(node.range).end <= opens);
            // A date alone ("March 31, 2021") dates the reasons; it heads nothing.
            if text_between || !legal_citations::cues::layout_heading_text_plausible(&entry.title) { break; }
            opens = entry.start;
        }
        entries.retain(|entry| entry.kind != "heading" || entry.start >= opens);
    }
    let mut heading = None;
    for entry in &mut entries {
        if entry.kind == "heading" { heading = Some(entry.level); }
        else { entry.level = heading.map_or(0, |level| level + 1); }
    }
    entries
}
