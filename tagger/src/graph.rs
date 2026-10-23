//! The tagger's units written into the parser's structure graph. The graph keeps what the tagger
//! does not decide (pages, sections and document constituents, tables, notes and their pairs) and
//! its paragraph-level nodes (paragraphs, prose, headings, list items, quotes) give way to the
//! decoded units: one node per unit, parents by the hierarchy rule, markers and heading levels as
//! the gold has them. Consumers read the same node fields they read from tier 0.
use crate::state::Gold;
use legal_structure_model::{
    ContinuationKind, Derivation, DocumentStructure, NodeKind, Origin, PrimaryRole, ScalarRange, SourceContinuation,
    StructureNode,
};
use std::collections::{BTreeSet, HashMap};

pub const ORIGIN: &str = "legal-structure-tagger.v1";

/// A source line as the graph places it: its UTF-16 start in the document text, its page, and the
/// UTF-16 offset of each of its characters (so a span counted in characters maps onto the text).
struct Placed {
    start: usize,
    page: usize,
    offsets: Vec<usize>,
}

fn placed(structure: &DocumentStructure) -> Result<HashMap<String, Placed>, String> {
    let units: Vec<u16> = structure.text.encode_utf16().collect();
    let place = |id: &str, start: usize, end: usize, page: usize, out: &mut HashMap<String, Placed>| -> Result<(), String> {
        let text = String::from_utf16(units.get(start..end).ok_or("Invalid source fragment")?).map_err(|e| e.to_string())?;
        let mut offsets = Vec::with_capacity(text.chars().count() + 1);
        let mut at = start;
        for c in text.chars() {
            offsets.push(at);
            at += c.len_utf16();
        }
        offsets.push(at);
        out.insert(id.to_owned(), Placed { start, page, offsets });
        Ok(())
    };
    let mut out = HashMap::new();
    if !structure.source_fragments.is_empty() {
        for fragment in &structure.source_fragments {
            place(&fragment.id, fragment.range.start, fragment.range.end, fragment.page_index, &mut out)?;
        }
        return Ok(out);
    }
    for page in structure.nodes.iter().filter(|n| n.kind == NodeKind::Page) {
        let content = String::from_utf16(units.get(page.range.start..page.range.end).ok_or("Invalid page range")?).map_err(|e| e.to_string())?;
        let mut start = page.range.start;
        for (id, text) in page.line_ids.iter().zip(content.split('\n')) {
            let stop = start + text.encode_utf16().count();
            place(id, start, stop, page.page_indexes.first().copied().unwrap_or(0), &mut out)?;
            start = stop + 1;
        }
    }
    Ok(out)
}

fn kind_of(region: &str) -> Option<(NodeKind, PrimaryRole)> {
    Some(match region {
        "text" => (NodeKind::Paragraph, PrimaryRole::Text),
        "list_item" => (NodeKind::ListItem, PrimaryRole::Text),
        "heading" => (NodeKind::Heading, PrimaryRole::Heading),
        "quote" => (NodeKind::Quote, PrimaryRole::Text),
        "contents" => (NodeKind::Paragraph, PrimaryRole::Text),
        "caption" => (NodeKind::Paragraph, PrimaryRole::Caption),
        "headnote" => (NodeKind::Paragraph, PrimaryRole::Headnote),
        "marginal_note" => (NodeKind::Paragraph, PrimaryRole::MarginalNote),
        // Notes, tables, furniture and page labels stay the parser's.
        _ => return None,
    })
}

/// Whether the parser's node gives way to the tagger's units.
fn replaced(node: &StructureNode) -> bool {
    matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose | NodeKind::Heading | NodeKind::ListItem | NodeKind::List | NodeKind::Quote)
}

/// The decoded units written into `structure`.
pub fn retag(structure: &mut DocumentStructure, gold: &Gold) -> Result<(), String> {
    let lines = placed(structure)?;
    let regions: Vec<&str> = gold.units.iter().map(|u| gold.typed[u.lines[0]].region.as_str()).collect();
    let node_id: Vec<Option<String>> = gold.units.iter().enumerate()
        .map(|(u, _)| kind_of(regions[u]).map(|_| format!("tag-{:06}", u + 1))).collect();
    let mut nodes: Vec<StructureNode> = Vec::new();
    let mut continuations: Vec<SourceContinuation> = Vec::new();
    for (u, unit) in gold.units.iter().enumerate() {
        let Some((kind, role)) = kind_of(regions[u]) else { continue };
        let Some(id) = node_id[u].clone() else { continue };
        // The unit's spans as UTF-16 ranges in reading order, merged across the newline between lines.
        let mut ranges: Vec<ScalarRange> = Vec::new();
        // The UTF-16 start of each range's last character: a continuation leaves from inside a line.
        let mut last_char: Vec<usize> = Vec::new();
        let mut pages = BTreeSet::new();
        let mut line_ids: Vec<String> = Vec::new();
        for &t in &unit.lines {
            for span in &gold.typed[t].spans {
                let Some(line) = lines.get(&span.line_id) else { continue };
                let last = line.offsets.len() - 1;
                let (start, end) = (line.offsets[span.start.min(last)], line.offsets[span.end.min(last)]);
                if start >= end {
                    continue;
                }
                let final_char = line.offsets[(span.end.min(last)).saturating_sub(1).max(span.start.min(last))];
                match ranges.last_mut() {
                    Some(previous) if start <= previous.end + 1 && start >= previous.start => {
                        previous.end = previous.end.max(end);
                        *last_char.last_mut().unwrap() = final_char;
                    }
                    _ => {
                        ranges.push(ScalarRange { start, end });
                        last_char.push(final_char);
                    }
                }
                pages.insert(line.page);
                if line_ids.last() != Some(&span.line_id) {
                    line_ids.push(span.line_id.clone());
                }
            }
        }
        let Some(&first) = ranges.first() else { continue };
        let hull = ScalarRange { start: first.start, end: ranges.last().unwrap().end };
        let parent_id = unit.parent.and_then(|p| node_id.get(p).cloned().flatten());
        let mut node = StructureNode::new(id, kind, hull, ORIGIN, Derivation::Model, parent_id);
        node.role = Some(role);
        node.ranges = ranges.clone();
        node.page_indexes = pages.into_iter().collect();
        node.line_ids = line_ids;
        if kind == NodeKind::Heading {
            node.level = unit.level.or(Some(1));
            node.heading_range = Some(hull);
            node.label = Some(gold.typed[unit.lines[0]].text.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        let head = &gold.typed[unit.lines[0]];
        if let Some(marker) = head.markers.first().and_then(|m| m.first()) {
            if let Some(line) = lines.get(&marker.line_id) {
                let last = line.offsets.len() - 1;
                let range = ScalarRange { start: line.offsets[marker.start.min(last)], end: line.offsets[marker.end.min(last)] };
                if range.start < range.end {
                    let text: String = String::from_utf16_lossy(&structure.text.encode_utf16().collect::<Vec<_>>()[range.start..range.end]);
                    node.marker_range = Some(range);
                    node.label = Some(text.trim().to_owned());
                    node.content_start = Some(range.end + 1);
                    node.locator_kind = Some(match kind { NodeKind::ListItem => "list_item", _ => "paragraph" }.to_owned());
                }
            }
        } else if kind == NodeKind::ListItem {
            node.locator_kind = Some("list_item".to_owned());
        }
        // Parts on different pages are one unit: the seam reads on as the parser's continuations do.
        let kind_of_link = match kind { NodeKind::Heading => ContinuationKind::Heading, NodeKind::ListItem => ContinuationKind::List, _ => ContinuationKind::Paragraph };
        for (k, pair) in ranges.windows(2).enumerate() {
            continuations.push(SourceContinuation { kind: kind_of_link, from: last_char[k], to: pair[1].start, separator: Some(" ".to_owned()) });
        }
        nodes.push(node);
    }
    // The parser's paragraph-level nodes go; what stays loses any parent that went.
    let kept_ids: BTreeSet<String> = structure.nodes.iter().filter(|n| !replaced(n)).map(|n| n.id.clone()).collect();
    structure.nodes.retain(|n| !replaced(n));
    for node in &mut structure.nodes {
        if node.parent_id.as_ref().is_some_and(|p| !kept_ids.contains(p)) {
            node.parent_id = None;
        }
    }
    structure.nodes.extend(nodes);
    let mut links: Vec<SourceContinuation> = structure.continuations.take().unwrap_or_default().into_iter()
        .filter(|c| matches!(c.kind, ContinuationKind::Note | ContinuationKind::Table)).collect();
    links.extend(continuations);
    structure.continuations = Some(links);
    if !structure.origins.iter().any(|o| o.id == ORIGIN) {
        structure.origins.push(Origin { id: ORIGIN.to_owned() });
    }
    Ok(())
}
