//! A parser structure as composition blocks and annotations over its source lines, for the
//! structure gold to score it.
use crate::{
    CompositionAnnotation, CompositionBlock, CompositionLine, CompositionLink, CompositionLinks, DocumentStructure,
    NodeKind, PrimaryRole, ScalarRange, ScalarText, SourceSpan,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Deserialize)]
pub struct StructureCompositionBlocksRequest {
    pub structure: DocumentStructure,
    pub lines: Vec<CompositionLine>,
}

#[derive(Serialize)]
pub struct PreparedCompositionBlocks {
    pub groups: Vec<CompositionBlock>,
    pub annotations: Vec<CompositionAnnotation>,
    pub continuations: CompositionLinks,
}

type Line<'a> = (&'a CompositionLine, ScalarRange, usize);

/// A UTF-16 range as spans of the source lines it crosses.
fn line_spans(text: &ScalarText<'_>, lines: &[Line<'_>], ranges: &[ScalarRange]) -> Result<Vec<SourceSpan>, String> {
    let mut result = Vec::new();
    for range in ranges {
        for (line, extent, scalar_start) in lines {
            let (start, end) = (range.start.max(extent.start), range.end.min(extent.end));
            if start < end {
                result.push(SourceSpan {
                    line_id: line.id.clone(),
                    start: text.scalar_at_utf16(start).ok_or("Annotation splits a source character")? - scalar_start,
                    end: text.scalar_at_utf16(end).ok_or("Annotation splits a source character")? - scalar_start,
                });
            }
        }
    }
    Ok(result)
}

pub fn structure_composition_blocks(request: StructureCompositionBlocksRequest) -> Result<PreparedCompositionBlocks, String> {
    let structure = &request.structure;
    if structure.offset_unit != "utf16" {
        return Err("Native source coordinates must be UTF-16".into());
    }
    let text = ScalarText::new(&structure.text);
    let mut lines = Vec::new();
    for line in &request.lines {
        let range: ScalarRange = serde_json::from_value(line.evidence.get("range").cloned().ok_or("Source range missing")?)
            .map_err(|error| error.to_string())?;
        if text.slice_utf16(range.start..range.end) != Some(line.text.as_str()) {
            return Err("Source line text differs from its declared range".into());
        }
        lines.push((line, range, text.scalar_at_utf16(range.start).ok_or("Invalid source boundary")?));
    }
    lines.sort_by_key(|(_, range, _)| (range.start, range.end));
    let nodes: HashMap<_, _> = structure.nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let mut endpoints = structure.continuations.as_deref().unwrap_or_default().iter()
        .flat_map(|link| [link.from, link.to]).collect::<Vec<_>>();
    endpoints.sort_unstable();
    endpoints.dedup();
    // Each unit's own membership (a heading's heading text), the most specific owning a span. A
    // table row is a unit too (the gold types a table one row at a time, cells joined): its lines
    // are one block, under any paragraph or item read inside it.
    // A tagger's graph owns its text through the tagger's units; the parser's section containers
    // (statute provisions, kept for lookups) then stay out of block ownership.
    let tagged = structure.origins.iter().any(|origin| origin.id == "legal-structure-tagger.v1");
    let mut candidates = Vec::new();
    for node in &structure.nodes {
        if tagged && node.kind == NodeKind::Section {
            continue;
        }
        let heading = node.kind == NodeKind::Heading || node.role == Some(PrimaryRole::Heading);
        if node.role.is_none() && !heading && !matches!(node.kind, NodeKind::Prose | NodeKind::Paragraph | NodeKind::Section
            | NodeKind::ListItem | NodeKind::Footnote | NodeKind::Endnote | NodeKind::Row) {
            continue;
        }
        let membership = if node.ranges.is_empty() { vec![node.range] } else { node.ranges.clone() };
        let ranges = if heading { node.heading_range.map_or(membership, |range| vec![range]) } else { membership };
        let size: usize = ranges.iter().map(|range| range.end.saturating_sub(range.start)).sum();
        let priority = if node.role.is_some() { 0 } else if node.kind == NodeKind::Heading { 1 }
            else if matches!(node.kind, NodeKind::Paragraph | NodeKind::ListItem) { 2 } else { 3 };
        candidates.push((node, ranges, size, priority));
    }
    let rank = |index: usize| (candidates[index].2, candidates[index].3, index);
    let mut owner_endpoints = HashMap::<usize, Vec<usize>>::new();
    for &offset in &endpoints {
        if let Some(index) = (0..candidates.len()).filter(|&index| candidates[index].1.iter()
            .any(|range| range.start <= offset && offset < range.end)).min_by_key(|&index| rank(index)) {
            owner_endpoints.entry(index).or_default().push(offset);
        }
    }
    let mut intervals = candidates.iter().enumerate()
        .flat_map(|(index, (_, ranges, _, _))| ranges.iter().map(move |range| (index, range.start, range.end)))
        .collect::<Vec<_>>();
    intervals.sort_by_key(|(_, start, end)| (*start, *end));
    // Each line cut where units and continuations start and end; its pieces go to the block of
    // their owning unit on that page, and unowned text stays a block of its line.
    let mut parts: Vec<(Option<usize>, Vec<SourceSpan>)> = Vec::new();
    let mut buckets = BTreeMap::new();
    let (mut cursor, mut active) = (0, Vec::new());
    for (line, line_range, scalar_start) in &lines {
        active.retain(|&(_, _, end)| end > line_range.start);
        while cursor < intervals.len() && intervals[cursor].1 < line_range.end {
            if intervals[cursor].2 > line_range.start { active.push(intervals[cursor]); }
            cursor += 1;
        }
        let here = active.iter().map(|&(index, start, end)| (index, start.max(line_range.start), end.min(line_range.end))).collect::<Vec<_>>();
        let mut cuts = vec![line_range.start, line_range.end];
        cuts.extend(endpoints.iter().copied().filter(|at| line_range.start < *at && *at < line_range.end));
        cuts.extend(here.iter().flat_map(|&(_, start, end)| [start, end]));
        cuts.sort_unstable();
        cuts.dedup();
        for edges in cuts.windows(2) {
            let (start, end) = (edges[0], edges[1]);
            if start == end { continue; }
            let owner = here.iter().filter(|(_, a, b)| *a <= start && end <= *b).map(|(index, _, _)| *index).min_by_key(|&index| rank(index));
            let span = SourceSpan {
                line_id: line.id.clone(),
                start: text.scalar_at_utf16(start).ok_or("Block splits a source character")? - scalar_start,
                end: text.scalar_at_utf16(end).ok_or("Block splits a source character")? - scalar_start,
            };
            let key = (owner, line.page,
                owner.and_then(|index| owner_endpoints.get(&index)).map_or(0, |points| points.partition_point(|at| *at <= start)),
                if owner.is_none() { line.id.as_str() } else { "" });
            let position = *buckets.entry(key).or_insert_with(|| { parts.push((owner, Vec::new())); parts.len() - 1 });
            let spans = &mut parts[position].1;
            match spans.last_mut().filter(|last| last.line_id == span.line_id && last.end == span.start) {
                Some(last) => last.end = span.end,
                None => spans.push(span),
            }
        }
    }
    let anchors: HashMap<_, _> = parts.iter().rev().filter_map(|(owner, spans)| owner.map(|index|
        (candidates[index].0.id.as_str(), crate::composition::anchor(&spans[0])))).collect();
    let mut groups = Vec::new();
    for (owner, spans) in parts {
        let node = owner.map(|index| candidates[index].0);
        let role = node.and_then(|node| node.role).unwrap_or(match node.map(|node| node.kind) {
            Some(NodeKind::Heading) => PrimaryRole::Heading,
            _ => PrimaryRole::Text,
        });
        let mut attributes = Map::new();
        attributes.insert("parent".into(), Value::Null);
        if let Some(node) = node {
            attributes.insert("native_id".into(), json!(node.id));
            // The nearest ancestor that is a block of its own.
            let (mut parent, mut visited) = (node.parent_id.as_deref(), HashSet::new());
            while let Some(id) = parent.filter(|id| visited.insert(*id)) {
                if let Some(anchor) = anchors.get(id) {
                    attributes.insert("parent".into(), json!(anchor));
                    break;
                }
                parent = nodes.get(id).and_then(|node| node.parent_id.as_deref());
            }
            let locator = node.locator_kind.as_deref().or(match node.kind {
                NodeKind::Paragraph => Some("paragraph"),
                NodeKind::ListItem => Some("list_item"),
                NodeKind::Section => Some("section"),
                _ => None,
            });
            if let Some(locator) = locator { attributes.insert("locator_kind".into(), json!(locator)); }
            if let Some(label) = &node.label { attributes.insert("label".into(), json!(label)); }
            if let Some(marker) = node.marker_range.filter(|_| !matches!(node.kind, NodeKind::Footnote | NodeKind::Endnote)) {
                let markers = line_spans(&text, &lines, &[marker])?.into_iter().filter_map(|mut found| {
                    let span = spans.iter().find(|span| span.line_id == found.line_id)?;
                    found.start = found.start.max(span.start);
                    found.end = found.end.min(span.end);
                    (found.start < found.end).then_some(found)
                }).collect::<Vec<_>>();
                if !markers.is_empty() { attributes.insert("marker".into(), json!(markers)); }
            }
        }
        let mut seen = HashSet::new();
        groups.push(CompositionBlock {
            id: crate::composition::anchor(&spans[0]),
            kind: serde_json::to_value(role).unwrap().as_str().unwrap().to_owned(),
            line_ids: spans.iter().filter(|span| seen.insert(span.line_id.clone())).map(|span| span.line_id.clone()).collect(),
            spans,
            attributes,
            document: None,
        });
    }
    let source_anchor = |offset: usize| -> Result<String, String> {
        let (line, _, scalar_start) = lines.iter().find(|(_, range, _)| range.start <= offset && offset < range.end)
            .ok_or("Continuation has no source fragment")?;
        Ok(format!("{}@{}", line.id, text.scalar_at_utf16(offset).ok_or("Continuation starts inside a source character")? - scalar_start))
    };
    let link = |next: String, separator: Option<String>, kind: &str| CompositionLink {
        next, separator, attributes: BTreeMap::from([("kind".into(), json!(kind))]),
    };
    let mut continuations = CompositionLinks::new();
    for found in structure.continuations.as_deref().unwrap_or_default() {
        let kind = serde_json::to_value(found.kind).unwrap().as_str().unwrap().to_owned();
        continuations.entry(kind.clone()).or_default()
            .insert(source_anchor(found.from)?, link(source_anchor(found.to)?, found.separator.clone(), &kind));
    }
    // A unit printed on several pages is one object even where its text seam is not linked.
    let mut native_parts = HashMap::<&str, Vec<&CompositionBlock>>::new();
    for group in &groups {
        if let Some(id) = group.attributes.get("native_id").and_then(Value::as_str) {
            native_parts.entry(id).or_default().push(group);
        }
    }
    let page = |group: &CompositionBlock| lines.iter().find(|(line, _, _)| line.id == group.spans[0].line_id).unwrap().0.page;
    for (id, parts) in native_parts {
        let node = nodes[id];
        let kind = if node.kind == NodeKind::Heading || node.role == Some(PrimaryRole::Heading) { "heading" }
            else if matches!(node.kind, NodeKind::Footnote | NodeKind::Endnote) { "note" }
            else if node.kind == NodeKind::ListItem { "list" } else { "paragraph" };
        for pair in parts.windows(2).filter(|pair| page(pair[0]) != page(pair[1])) {
            let layer = continuations.entry(kind.to_owned()).or_default();
            if !layer.contains_key(&pair[0].id) && !layer.values().any(|found| found.next == pair[1].id) {
                layer.insert(pair[0].id.clone(), link(pair[1].id.clone(), None, kind));
            }
        }
    }
    let annotations = seed_annotations(structure, &text, &lines)?;
    Ok(PreparedCompositionBlocks { groups, annotations, continuations })
}

/// Notes, tables and the source's own document as annotations over the source lines.
fn seed_annotations(structure: &DocumentStructure, text: &ScalarText<'_>, lines: &[Line<'_>]) -> Result<Vec<CompositionAnnotation>, String> {
    let annotation = |id: &str, kind: &str, spans: Vec<SourceSpan>, attributes: Value| {
        let mut seen = HashSet::new();
        CompositionAnnotation {
            id: id.to_owned(), kind: kind.to_owned(),
            line_ids: spans.iter().filter(|span| seen.insert(span.line_id.clone())).map(|span| span.line_id.clone()).collect(),
            spans, attributes: attributes.as_object().unwrap().clone(),
        }
    };
    let mut annotations = Vec::new();
    for note in &structure.notes {
        let mut ranges = note.label_ranges.iter().chain(&note.body_ranges).copied().collect();
        ScalarRange::normalize_union(&mut ranges);
        let spans = line_spans(text, lines, &ranges)?;
        if spans.is_empty() { continue; }
        let references = note.references.iter().map(|reference| reference.range).collect::<Vec<_>>();
        annotations.push(annotation(&note.id, "note", spans, json!({"kind": note.kind,
            "label": line_spans(text, lines, &note.label_ranges)?, "references": line_spans(text, lines, &references)?})));
    }
    for table in structure.nodes.iter().filter(|node| node.kind == NodeKind::Table) {
        let rows: Vec<_> = structure.nodes.iter().filter(|node| node.kind == NodeKind::Row && node.parent_id.as_deref() == Some(table.id.as_str())).collect();
        let row_ids: HashSet<_> = rows.iter().map(|node| node.id.as_str()).collect();
        let cells: Vec<_> = structure.nodes.iter().filter(|node| node.kind == NodeKind::Cell
            && node.parent_id.as_deref().is_some_and(|id| row_ids.contains(id))).collect();
        // A grid only where every row and cell has its one-based place.
        if rows.is_empty() || rows.iter().any(|row| !row.row.is_some_and(|n| n > 0))
            || cells.iter().any(|cell| !cell.row.is_some_and(|n| n > 0) || !cell.column.is_some_and(|n| n > 0)) {
            continue;
        }
        let last = |start: Option<usize>, extent: Option<usize>| start.unwrap() + extent.unwrap_or(1).max(1) - 1;
        let height = rows.iter().filter_map(|row| row.row).chain(cells.iter().map(|cell| last(cell.row, cell.row_span))).max().unwrap_or(0);
        let width = cells.iter().map(|cell| last(cell.column, cell.column_span)).max().unwrap_or(0);
        if width == 0 { continue; }
        let mut grid = vec![vec![Some(Vec::<SourceSpan>::new()); width]; height];
        let mut merges = Vec::new();
        for cell in &cells {
            let (r, c) = (cell.row.unwrap() - 1, cell.column.unwrap() - 1);
            let (h, w) = (cell.row_span.unwrap_or(1).max(1), cell.column_span.unwrap_or(1).max(1));
            for row in &mut grid[r..r + h] {
                for slot in &mut row[c..c + w] { *slot = None; }
            }
            grid[r][c] = Some(line_spans(text, lines, if cell.ranges.is_empty() { std::slice::from_ref(&cell.range) } else { &cell.ranges })?);
            if h > 1 || w > 1 { merges.push([r, c, h, w]); }
        }
        let headers = match &table.header_rows {
            Some(headers) => headers.iter().map(|row| row.saturating_sub(1)).collect::<Vec<_>>(),
            None => rows.iter().filter(|row| {
                let own: Vec<_> = cells.iter().filter(|cell| cell.parent_id.as_deref() == Some(row.id.as_str())).collect();
                !own.is_empty() && own.iter().all(|cell| cell.markup_tag.as_deref() == Some("th"))
            }).map(|row| row.row.unwrap() - 1).collect(),
        };
        let spans = grid.iter().flatten().flatten().flatten().cloned().collect::<Vec<_>>();
        if spans.is_empty() { continue; }
        annotations.push(annotation(&table.id, "table", spans, json!({"rows": grid, "header_rows": headers, "merges": merges})));
    }
    if let Some((line, _, _)) = lines.iter().find(|(line, _, _)| !line.text.is_empty()) {
        annotations.push(annotation(&structure.document_id, "document",
            vec![SourceSpan { line_id: line.id.clone(), start: 0, end: line.text.chars().count() }],
            json!({"kind": structure.doc_type, "parent": null, "relationship": null, "title": [], "facets": {}})));
    }
    Ok(annotations)
}
