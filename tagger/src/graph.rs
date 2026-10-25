//! The tagger's units written into the parser's structure graph. The graph keeps what the tagger
//! does not decide (pages, sections and document constituents, tables, notes and their pairs) and
//! its paragraph-level nodes (paragraphs, prose, headings, list items, quotes) give way to the
//! decoded units: one node per unit, parents by the hierarchy rule, markers and heading levels as
//! the gold has them. Consumers read the same node fields they read from tier 0.
use crate::state::Gold;
use legal_structure_model::{
    ContinuationKind, Derivation, DocumentStructure, NodeKind, Note, NoteKindV2, NoteReference, Origin, PrimaryRole,
    ScalarRange, SourceContinuation, StructureNode,
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

/// The node a unit of a region becomes; tables stay the parser's and notes are written apart
/// (`notes`). The region itself is kept on the node (`grammar: region:<name>`) so the replay
/// reads it back exactly.
fn kind_of(region: &str) -> Option<(NodeKind, PrimaryRole)> {
    Some(match region {
        "text" => (NodeKind::Paragraph, PrimaryRole::Text),
        "list_item" => (NodeKind::ListItem, PrimaryRole::Text),
        "heading" => (NodeKind::Heading, PrimaryRole::Heading),
        "quote" => (NodeKind::Quote, PrimaryRole::Text),
        "contents" => (NodeKind::Prose, PrimaryRole::Text),
        "caption" => (NodeKind::Paragraph, PrimaryRole::Caption),
        "headnote" => (NodeKind::Paragraph, PrimaryRole::Headnote),
        "marginal_note" => (NodeKind::Prose, PrimaryRole::MarginalNote),
        "furniture" => (NodeKind::Prose, PrimaryRole::Furniture),
        "page_label" => (NodeKind::Prose, PrimaryRole::PageLabel),
        "line_label" => (NodeKind::Prose, PrimaryRole::LineLabel),
        _ => return None,
    })
}

/// The region a tagger node carries, from `grammar: region:<name>`.
pub fn region_of(node: &StructureNode) -> Option<&str> {
    node.grammar.as_deref().and_then(|g| g.strip_prefix("region:"))
}

/// Whether the parser's node gives way to the tagger's units: everything but pages and sections.
/// The parser's footnotes give way to the tagger's notes (`notes`).
fn replaced(node: &StructureNode) -> bool {
    matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose | NodeKind::Heading | NodeKind::ListItem | NodeKind::List
        | NodeKind::Quote | NodeKind::Table | NodeKind::Row | NodeKind::Cell | NodeKind::Footnote | NodeKind::Endnote)
}

/// The tagger's notes: one Footnote node and one note record per decoded note unit, its label the
/// marker the decoder read on its first line, its body the unit's text, its references the pairs the
/// decoder made from the parser's reference reading. The parser's footnote nodes and note records
/// go: the model's notes read higher than the parser's on the main development set.
fn notes(gold: &Gold, lines: &HashMap<String, Placed>, regions: &[&str]) -> (Vec<StructureNode>, Vec<Note>) {
    let range_of = |span: &legal_structure::SourceSpan| -> Option<ScalarRange> {
        let line = lines.get(&span.line_id)?;
        let last = line.offsets.len() - 1;
        let (start, end) = (line.offsets[span.start.min(last)], line.offsets[span.end.min(last)]);
        (start < end).then_some(ScalarRange { start, end })
    };
    let mut references_of: HashMap<usize, Vec<legal_structure::SourceSpan>> = HashMap::new();
    for pair in &gold.notes {
        if let (Some(note), Some(r)) = (pair.note, pair.reference) {
            if let Some(span) = gold.typed[pair.line].references.get(r).and_then(|reference| reference.spans.first()) {
                references_of.entry(note).or_default().push(span.clone());
            }
        }
    }
    let mut nodes = Vec::new();
    let mut records = Vec::new();
    for (u, unit) in gold.units.iter().enumerate() {
        if regions[u] != "note" {
            continue;
        }
        let head = &gold.typed[unit.lines[0]];
        let mut ranges: Vec<ScalarRange> = Vec::new();
        let mut pages = BTreeSet::new();
        let mut line_ids: Vec<String> = Vec::new();
        for &t in &unit.lines {
            for span in &gold.typed[t].spans {
                let Some(range) = range_of(span) else { continue };
                match ranges.last_mut() {
                    Some(previous) if range.start <= previous.end + 1 && range.start >= previous.start => previous.end = previous.end.max(range.end),
                    _ => ranges.push(range),
                }
                if let Some(line) = lines.get(&span.line_id) {
                    pages.insert(line.page);
                }
                if line_ids.last() != Some(&span.line_id) {
                    line_ids.push(span.line_id.clone());
                }
            }
        }
        let Some(&first) = ranges.first() else { continue };
        let hull = ScalarRange { start: first.start, end: ranges.last().unwrap().end };
        let id = format!("tag-note-{:06}", u + 1);
        let label = head.markers.first().and_then(|m| m.first()).and_then(|m| range_of(m)).filter(|r| r.start >= hull.start && r.end <= hull.end);
        let mut node = StructureNode::new(id.clone(), NodeKind::Footnote, hull, ORIGIN, Derivation::Model, None);
        node.grammar = Some("region:note".to_owned());
        node.ranges = ranges.clone();
        node.page_indexes = pages.into_iter().collect();
        node.line_ids = line_ids;
        if let Some(label_range) = label {
            node.marker_range = Some(label_range);
            node.label = Some(head.text.split_whitespace().next().unwrap_or_default().to_owned());
        }
        let references: Vec<NoteReference> = references_of.get(&u).into_iter().flatten().filter_map(|span| {
            let range = range_of(span)?;
            let line = lines.get(&span.line_id)?;
            Some(NoteReference { range, anchor: None, page_indexes: vec![line.page], line_ids: vec![span.line_id.clone()] })
        }).collect();
        let body_ranges: Vec<ScalarRange> = match label {
            Some(l) => ranges.iter().filter_map(|r| {
                let start = if r.start < l.end && l.start < r.end { l.end.max(r.start) } else { r.start };
                (start < r.end).then_some(ScalarRange { start, end: r.end })
            }).collect(),
            None => ranges.clone(),
        };
        records.push(Note {
            id: format!("note-{id}"),
            node_id: Some(id),
            kind: NoteKindV2::Footnote,
            label_range: label.unwrap_or(ScalarRange { start: hull.start, end: hull.start }),
            body_range: hull,
            label_ranges: label.into_iter().collect(),
            body_ranges,
            primary_reference: references.first().map(|r| r.range),
            references,
        });
        nodes.push(node);
    }
    (nodes, records)
}

/// The decoded units written into `structure`.
pub fn retag(structure: &mut DocumentStructure, gold: &Gold) -> Result<(), String> {
    let lines = placed(structure)?;
    let regions: Vec<&str> = gold.units.iter().map(|u| gold.typed[u.lines[0]].region.as_str()).collect();
    let node_id: Vec<Option<String>> = gold.units.iter().enumerate()
        .map(|(u, _)| kind_of(regions[u]).map(|_| format!("tag-{:06}", u + 1))).collect();
    let mut nodes: Vec<StructureNode> = Vec::new();
    let mut continuations: Vec<SourceContinuation> = Vec::new();
    // A span counted in characters of a line, as a UTF-16 range in the text.
    let range_of = |span: &legal_structure::SourceSpan| -> Option<ScalarRange> {
        let line = lines.get(&span.line_id)?;
        let last = line.offsets.len() - 1;
        let (start, end) = (line.offsets[span.start.min(last)], line.offsets[span.end.min(last)]);
        (start < end).then_some(ScalarRange { start, end })
    };
    // Tables: a run of table units (the gold types a table one row at a time, cells joined) is one
    // Table node with Row children and their Cell children, as the composition reads a grid.
    let mut table_at: Vec<Option<usize>> = vec![None; gold.units.len()];
    let mut tables = 0;
    let mut u = 0;
    while u < gold.units.len() {
        if regions[u] != "table" {
            u += 1;
            continue;
        }
        let first = u;
        while u < gold.units.len() && regions[u] == "table" {
            table_at[u] = Some(tables);
            u += 1;
        }
        tables += 1;
        let table_id = format!("tag-table-{tables:06}");
        let mut table_ranges: Vec<ScalarRange> = Vec::new();
        let mut table_pages = BTreeSet::new();
        let mut table_lines: Vec<String> = Vec::new();
        let mut rows: Vec<StructureNode> = Vec::new();
        let mut cells: Vec<StructureNode> = Vec::new();
        for (r, unit_index) in (first..u).enumerate() {
            let row_id = format!("tag-row-{tables:06}-{:04}", r + 1);
            let head = &gold.typed[gold.units[unit_index].lines[0]];
            let spans: Vec<&legal_structure::SourceSpan> = gold.units[unit_index].lines.iter().flat_map(|&t| gold.typed[t].spans.iter()).collect();
            let row_ranges: Vec<ScalarRange> = spans.iter().filter_map(|s| range_of(s)).collect();
            let Some(&row_first) = row_ranges.first() else { continue };
            let hull = ScalarRange { start: row_first.start, end: row_ranges.iter().map(|x| x.end).max().unwrap() };
            table_ranges.extend(row_ranges.iter().copied());
            let mut row = StructureNode::new(row_id.clone(), NodeKind::Row, hull, ORIGIN, Derivation::Model, Some(table_id.clone()));
            row.role = Some(PrimaryRole::Text);
            row.grammar = Some("region:table".to_owned());
            row.ranges = row_ranges.clone();
            row.row = Some(r + 1);
            for s in &spans {
                if let Some(line) = lines.get(&s.line_id) {
                    table_pages.insert(line.page);
                    if !table_lines.contains(&s.line_id) {
                        table_lines.push(s.line_id.clone());
                    }
                    row.page_indexes.push(line.page);
                    if !row.line_ids.contains(&s.line_id) {
                        row.line_ids.push(s.line_id.clone());
                    }
                }
            }
            row.page_indexes.sort_unstable();
            row.page_indexes.dedup();
            let grid: Vec<Vec<legal_structure::SourceSpan>> = match &head.cells {
                Some(grid) if !grid.is_empty() => grid.clone(),
                _ => vec![spans.iter().map(|s| (*s).clone()).collect()],
            };
            for (c, cell_spans) in grid.iter().enumerate() {
                let ranges: Vec<ScalarRange> = cell_spans.iter().filter_map(|s| range_of(s)).collect();
                let Some(&cell_first) = ranges.first() else { continue };
                let cell_hull = ScalarRange { start: cell_first.start, end: ranges.iter().map(|x| x.end).max().unwrap() };
                let mut cell = StructureNode::new(format!("{row_id}-c{:03}", c + 1), NodeKind::Cell, cell_hull, ORIGIN, Derivation::Model, Some(row_id.clone()));
                cell.ranges = ranges;
                cell.row = Some(r + 1);
                cell.column = Some(c + 1);
                cell.page_indexes = row.page_indexes.clone();
                cells.push(cell);
            }
            rows.push(row);
        }
        if let (Some(&start), Some(end)) = (table_ranges.first(), table_ranges.iter().map(|x| x.end).max()) {
            // The table sits where its first row does in the hierarchy (under the nearest heading), so
            // its rows' parent resolves there through it.
            let table_parent = gold.units[first].parent.and_then(|p| node_id.get(p).cloned().flatten());
            let mut table = StructureNode::new(table_id, NodeKind::Table, ScalarRange { start: start.start, end }, ORIGIN, Derivation::Model, table_parent);
            table.ranges = table_ranges;
            table.page_indexes = table_pages.into_iter().collect();
            table.line_ids = table_lines;
            table.table_number = Some(tables);
            nodes.push(table);
            nodes.extend(rows);
            nodes.extend(cells);
        }
    }
    // Every tagged unit's text ranges, sorted: a unit whose consecutive ranges another unit's range
    // separates reads as parts, as the gold's projection reads it.
    let mut occupied: Vec<(usize, usize)> = Vec::new();
    for (u, unit) in gold.units.iter().enumerate() {
        if kind_of(regions[u]).is_none() {
            continue;
        }
        for &t in &unit.lines {
            for span in &gold.typed[t].spans {
                if let Some(range) = range_of(span) {
                    occupied.push((range.start, u));
                }
            }
        }
    }
    occupied.sort_unstable();
    let other_between = |u: usize, from: usize, to: usize| -> bool {
        let start = occupied.partition_point(|&(s, _)| s <= from);
        occupied[start..].iter().take_while(|&&(s, _)| s < to).any(|&(_, v)| v != u)
    };
    for (u, unit) in gold.units.iter().enumerate() {
        let Some((kind, role)) = kind_of(regions[u]) else { continue };
        let Some(id) = node_id[u].clone() else { continue };
        // The unit's spans as UTF-16 ranges in reading order, merged across the newline between lines.
        let mut ranges: Vec<ScalarRange> = Vec::new();
        let mut range_page: Vec<usize> = Vec::new();
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
                match ranges.last_mut() {
                    Some(previous) if start <= previous.end + 1 && start >= previous.start => {
                        previous.end = previous.end.max(end);
                    }
                    _ => {
                        ranges.push(ScalarRange { start, end });
                        range_page.push(line.page);
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
        node.grammar = Some(format!("region:{}", regions[u]));
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
        // Parts are one unit joined by a continuation: on different pages, where another tagged
        // unit's text comes between (a second column, a note, a heading the unit resumes after), or
        // where the unit reads backwards through the extraction. A link's endpoint cuts the block, so
        // each part reads by its own first line, as the scorer's projection of the same units does.
        // Parts on one page with only untagged lines between (a margin line number) stay one block.
        // A link's endpoints are the two parts' starts, as the parser's own links are: the composition
        // cuts each line where an endpoint falls, so an endpoint inside a part would cut the part.
        let kind_of_link = match kind { NodeKind::Heading => ContinuationKind::Heading, NodeKind::ListItem => ContinuationKind::List, _ => ContinuationKind::Paragraph };
        let mut part_start = first.start;
        for (k, pair) in ranges.windows(2).enumerate() {
            let apart = range_page[k] != range_page[k + 1]
                || pair[1].start < pair[0].start
                || other_between(u, pair[0].end, pair[1].start);
            if apart {
                continuations.push(SourceContinuation { kind: kind_of_link, from: part_start, to: pair[1].start, separator: Some(" ".to_owned()) });
                part_start = pair[1].start;
            }
        }
        nodes.push(node);
    }
    // Text no unit covers (the model read it as no region: a whole line, or a piece inside a line at
    // a column seam) is furniture: outside the body, as the decoded units leave it, rather than an
    // unowned block the composition would make of it. Span by span, so a covered line's leftover
    // piece is covered too.
    let mut covered: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for unit in &gold.units {
        for &t in &unit.lines {
            for span in &gold.typed[t].spans {
                covered.entry(span.line_id.as_str()).or_default().push((span.start, span.end));
            }
        }
    }
    let text_units: Vec<u16> = structure.text.encode_utf16().collect();
    let mut extra = 0;
    let mut ordered: Vec<(&String, &Placed)> = lines.iter().collect();
    ordered.sort_by_key(|(_, line)| line.start);
    for (id, line) in ordered {
        let chars = line.offsets.len() - 1;
        if chars == 0 {
            continue;
        }
        let mut spans = covered.get(id.as_str()).cloned().unwrap_or_default();
        spans.sort_unstable();
        let mut gaps: Vec<(usize, usize)> = Vec::new();
        let mut at = 0;
        for (start, end) in spans {
            let start = start.min(chars);
            if start > at {
                gaps.push((at, start));
            }
            at = at.max(end.min(chars));
        }
        if at < chars {
            gaps.push((at, chars));
        }
        for (a, b) in gaps {
            let range = ScalarRange { start: line.offsets[a], end: line.offsets[b] };
            let blank = String::from_utf16_lossy(&text_units[range.start.min(text_units.len())..range.end.min(text_units.len())]).trim().is_empty();
            if range.start >= range.end || blank {
                continue;
            }
            extra += 1;
            let mut node = StructureNode::new(format!("tag-none-{extra:06}"), NodeKind::Prose, range, ORIGIN, Derivation::Model, None);
            node.role = Some(PrimaryRole::Furniture);
            node.grammar = Some("region:furniture".to_owned());
            node.ranges = vec![range];
            node.page_indexes = vec![line.page];
            node.line_ids = vec![id.clone()];
            nodes.push(node);
        }
    }
    // The tagger's notes replace the parser's.
    let (note_nodes, note_records) = notes(gold, &lines, &regions);
    nodes.extend(note_nodes);
    structure.notes = note_records;
    // The parser's paragraph-level nodes go; what stays loses any parent that went.
    let kept_ids: BTreeSet<String> = structure.nodes.iter().filter(|n| !replaced(n)).map(|n| n.id.clone()).collect();
    structure.nodes.retain(|n| !replaced(n));
    for node in &mut structure.nodes {
        if node.parent_id.as_ref().is_some_and(|p| !kept_ids.contains(p)) {
            node.parent_id = None;
        }
    }
    structure.nodes.extend(nodes);
    // The parser's note continuations named its notes; the tagger's notes carry none.
    let mut links: Vec<SourceContinuation> = Vec::new();
    links.extend(continuations);
    structure.continuations = Some(links);
    if !structure.origins.iter().any(|o| o.id == ORIGIN) {
        structure.origins.push(Origin { id: ORIGIN.to_owned() });
    }
    Ok(())
}
