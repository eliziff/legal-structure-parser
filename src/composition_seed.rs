//! Project existing source facts into editable blocks without reducing starts to line IDs.
use crate::{
    CompositionBlock, CompositionLine, NodeKind, PrimaryRole, ScalarRange,
    ScalarText, SourceSpan,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructureCompositionBlocksRequest {
    pub structure: Value,
    pub lines: Vec<CompositionLine>,
}

#[derive(Serialize)]
pub struct PreparedCompositionBlocks {
    pub groups: Vec<CompositionBlock>,
    pub annotations: Vec<crate::CompositionAnnotation>,
    /// Baseline nodes not superseded by a represented composition object.
    pub retained_node_ids: Vec<String>,
    /// Editable source membership and exact parent edges for carried native facts.
    pub removable_nodes: Vec<crate::CompositionAnnotation>,
    pub manuscript_edits: crate::ManuscriptEdits,
    pub continuations: BTreeMap<String, BTreeMap<String, crate::CompositionLink>>,
    pub joins: BTreeMap<String, crate::CompositionLink>,
    pub resumes: BTreeMap<String, Value>,
}

/// Membership that the block representation can address; unsupported facts remain witnesses.
pub(crate) fn reading_stream_source_membership(
    scope: &crate::StructureScopeAnnotation, text: &ScalarText<'_>, physical: &[ScalarRange],
) -> Result<(Vec<ScalarRange>, Option<&'static str>), String> {
    let declared = if scope.owned_ranges.is_empty() { &scope.ranges } else { &scope.owned_ranges };
    let mut ranges = Vec::new();
    let mut reason = None;
    for range in declared {
        if range.start > range.end || text.slice_utf16(range.start..range.end).is_none() {
            return Err("Reading stream has invalid source membership".into());
        }
        if range.start == range.end {
            reason = Some("Reading stream point membership has no block ownership representation");
        } else { ranges.push(*range); }
    }
    ScalarRange::normalize_union(&mut ranges);
    let mut represented = Vec::new();
    for range in &ranges {
        let mut covered = physical.iter().filter_map(|extent| {
            let part = ScalarRange { start: range.start.max(extent.start), end: range.end.min(extent.end) };
            (part.start < part.end).then_some(part)
        }).collect::<Vec<_>>();
        ScalarRange::normalize_union(&mut covered);
        let mut cursor = range.start;
        for part in &covered {
            if !text.slice_utf16(cursor..part.start).is_some_and(|gap| gap.trim().is_empty()) {
                return Ok((Vec::new(), Some("Reading stream membership is outside physical source lines")));
            }
            cursor = part.end;
        }
        if !text.slice_utf16(cursor..range.end).is_some_and(|gap| gap.trim().is_empty()) {
            return Ok((Vec::new(), Some("Reading stream membership is outside physical source lines")));
        }
        represented.extend(covered);
    }
    if represented.is_empty() {
        return Ok((Vec::new(), Some(reason.unwrap_or("Reading stream has no addressable source member"))));
    }
    ScalarRange::normalize_union(&mut represented);
    Ok((represented, reason))
}

pub fn structure_composition_blocks(
    request: StructureCompositionBlocksRequest,
) -> Result<PreparedCompositionBlocks, String> {
    let structure = crate::composition_annotation_seed::typed_structure(&request.structure)?;
    let structure = &structure;
    if structure.offset_unit != "utf16" {
        return Err("Native source coordinates must be UTF-16".into());
    }
    let text = ScalarText::new(&structure.text);
    let fragments: HashMap<_, _> = structure
        .source_fragments
        .iter()
        .map(|fragment| (fragment.id.as_str(), fragment))
        .collect();
    if !fragments.is_empty() && fragments.len() != request.lines.len() {
        return Err("Source identity coverage differs from native fragments".into());
    }
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    for line in &request.lines {
        if !seen.insert(&line.id) {
            return Err("Repeated source line identity".into());
        }
        let range: ScalarRange = serde_json::from_value(
            line.evidence
                .get("range")
                .cloned()
                .ok_or("Source range missing")?,
        )
        .map_err(|error| error.to_string())?;
        if text.slice_utf16(range.start..range.end) != Some(line.text.as_str()) {
            return Err("Source line text differs from its declared range".into());
        }
        if !fragments.is_empty()
            && !fragments.get(line.id.as_str()).is_some_and(|fragment| {
                fragment.range == range && fragment.page_index + 1 == line.page
            })
        {
            return Err("Source identity or page differs from its native fragment".into());
        }
        lines.push((
            line,
            range,
            text.scalar_at_utf16(range.start)
                .ok_or("Invalid source boundary")?,
        ));
    }
    crate::composition::validate_manuscript_edits(&structure.manuscript_edits,
        &request.lines.iter().map(|line|(line.id.as_str(),line.text.chars().count())).collect())?;
    let nodes: HashMap<_, _> = structure
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    // Stream membership is an independent source fact; it need not have split a role node.
    let mut streams = Vec::new();
    let mut stream_ids = HashSet::new();
    for scope in structure.scope_annotations.iter().filter(|scope| scope.kind == crate::StructureScopeKind::ReadingStream) {
        if scope.id.is_empty() || !stream_ids.insert(scope.id.as_str())
            || scope.ownership.reading_stream_id.as_deref() != Some(scope.id.as_str()) {
            return Err("Reading stream has an invalid source identity".into());
        }
        let physical = lines.iter().map(|(_, range, _)| *range).collect::<Vec<_>>();
        let (ranges, _) = reading_stream_source_membership(scope, &text, &physical)?;
        if ranges.is_empty() { continue; }
        streams.push((scope, ranges));
    }
    let mut endpoints = structure.continuations.as_deref().unwrap_or_default().iter()
        .flat_map(|link| [link.from, link.to]).collect::<Vec<_>>();
    endpoints.extend(streams.iter().flat_map(|(_, ranges)| ranges.iter().flat_map(|range| [range.start, range.end])));
    endpoints.sort_unstable(); endpoints.dedup();
    let mut candidates = Vec::new();
    for node in &structure.nodes {
        if node.role.is_none()
            && !matches!(
                node.kind,
                NodeKind::Prose
                    | NodeKind::Paragraph
                    | NodeKind::Section
                    | NodeKind::ListItem
                    | NodeKind::Heading
                    | NodeKind::Footnote
                    | NodeKind::Endnote
            )
        {
            continue;
        }
        let ranges = if node.kind == NodeKind::Heading || node.role == Some(PrimaryRole::Heading) {
            node.heading_range
                .map(|range| vec![range])
                .unwrap_or_else(|| {
                    if node.ranges.is_empty() {
                        vec![node.range]
                    } else {
                        node.ranges.clone()
                    }
                })
        } else if node.ranges.is_empty() {
            vec![node.range]
        } else {
            node.ranges.clone()
        };
        let size: usize = ranges
            .iter()
            .map(|range| range.end.saturating_sub(range.start))
            .sum();
        let priority = if node.role.is_some() {
            0
        } else if node.kind == NodeKind::Heading {
            1
        } else if matches!(node.kind, NodeKind::Paragraph | NodeKind::ListItem) {
            2
        } else {
            3
        };
        candidates.push((node, ranges, size, priority));
    }
    let mut owner_endpoints = HashMap::<usize, Vec<usize>>::new();
    for &offset in &endpoints {
        if let Some((index, _)) = candidates.iter().enumerate()
            .filter(|(_, (_, ranges, _, _))| ranges.iter().any(|range| range.start <= offset && offset < range.end))
            .min_by_key(|(index, (_, _, size, priority))| (*size, *priority, *index)) {
            owner_endpoints.entry(index).or_default().push(offset);
        }
    }
    // Containers may overlap leaf blocks. Select their most specific membership,
    // retaining every distinct boundary and keeping all original nodes as witnesses.
    let mut parts: Vec<(Option<usize>, usize, Vec<SourceSpan>, Option<String>)> = Vec::new();
    let mut buckets = BTreeMap::new();
    let mut source_intervals = candidates
        .iter()
        .enumerate()
        .flat_map(|(index, (_, ranges, _, _))| {
            ranges
                .iter()
                .map(move |range| (index, range.start, range.end))
        })
        .collect::<Vec<_>>();
    source_intervals.sort_by_key(|(_, start, end)| (*start, *end));
    lines.sort_by_key(|(_, range, _)| (range.start, range.end));
    let mut cursor = 0;
    let mut active = Vec::new();
    for (line, line_range, scalar_start) in &lines {
        active.retain(|&(_, _, end)| end > line_range.start);
        while cursor < source_intervals.len() && source_intervals[cursor].1 < line_range.end {
            if source_intervals[cursor].2 > line_range.start {
                active.push(source_intervals[cursor]);
            }
            cursor += 1;
        }
        let intervals = active
            .iter()
            .map(|&(index, start, end)| {
                (index, start.max(line_range.start), end.min(line_range.end))
            })
            .collect::<Vec<_>>();
        let mut cuts = vec![line_range.start, line_range.end];
        cuts.extend(endpoints.iter().copied().filter(|at| line_range.start < *at && *at < line_range.end));
        for &(_, start, end) in &intervals {
            cuts.extend([start, end]);
        }
        cuts.sort_unstable();
        cuts.dedup();
        for edges in cuts.windows(2) {
            let (start, end) = (edges[0], edges[1]);
            if start == end {
                continue;
            }
            let owner = intervals
                .iter()
                .filter(|(_, a, b)| *a <= start && end <= *b)
                .min_by_key(|(index, _, _)| (candidates[*index].2, candidates[*index].3, *index))
                .map(|(index, _, _)| *index);
            let matches = streams.iter().filter(|(_, ranges)| ranges.iter().any(|range| range.start <= start && end <= range.end)).collect::<Vec<_>>();
            if matches.len() > 1 { return Err("Source membership belongs to conflicting reading streams".into()); }
            let inherited = owner.and_then(|index| candidates[index].0.ownership.as_ref())
                .and_then(|ownership| ownership.reading_stream_id.as_deref());
            let explicit = matches.first().map(|(scope, _)| scope.id.as_str());
            if inherited.is_some_and(|id| explicit.is_some_and(|explicit| explicit != id)
                || streams.iter().any(|(scope, _)| scope.id == id) && explicit != Some(id)) {
                return Err("Node reading stream conflicts with its declared source membership".into());
            }
            let stream = explicit.or(inherited);
            let span = SourceSpan {
                line_id: line.id.clone(),
                start: text
                    .scalar_at_utf16(start)
                    .ok_or("Block splits a source character")?
                    - scalar_start,
                end: text
                    .scalar_at_utf16(end)
                    .ok_or("Block splits a source character")?
                    - scalar_start,
            };
            // Unowned text remains its own physical block, never a document-wide bucket.
            let key = (
                owner,
                line.page,
                stream,
                owner.and_then(|index| owner_endpoints.get(&index)).map_or(0, |points| points.partition_point(|at| *at <= start)),
                if owner.is_none() {
                    line.id.as_str()
                } else {
                    ""
                },
            );
            let position = *buckets.entry(key).or_insert_with(|| {
                parts.push((owner, line.page, Vec::new(), stream.map(str::to_owned)));
                parts.len() - 1
            });
            let spans = &mut parts[position].2;
            if let Some(last) = spans
                .last_mut()
                .filter(|last| last.line_id == span.line_id && last.end == span.start)
            {
                last.end = span.end;
            } else {
                spans.push(span);
            }
        }
    }
    let anchors: HashMap<_, _> = parts
        .iter()
        .filter_map(|(owner, _, spans, _)| {
            owner.map(|index| {
                (
                    candidates[index].0.id.as_str(),
                    format!("{}@{}", spans[0].line_id, spans[0].start),
                )
            })
        })
        .rev()
        .collect();
    let mut result = Vec::new();
    for (owner, _, spans, stream) in parts {
        let node = owner.map(|index| candidates[index].0);
        let role =
            node.and_then(|node| node.role)
                .unwrap_or_else(|| match node.map(|node| &node.kind) {
                    Some(NodeKind::Heading) => PrimaryRole::Heading,
                    _ => PrimaryRole::Text,
                });
        let mut attributes = Map::new();
        attributes.insert("parent".into(), Value::Null);
        if let Some(node) = node {
            attributes.insert("native_id".into(), json!(node.id));
            let mut parent = node.parent_id.as_deref();
            let mut visited = HashSet::new();
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
            if let Some(locator) = locator {
                attributes.insert("locator_kind".into(), json!(locator));
            }
            if let Some(label) = &node.label {
                attributes.insert("label".into(), json!(label));
            }
            if !matches!(node.kind, NodeKind::Footnote | NodeKind::Endnote) {
                if let Some(marker) = node.marker_range {
                    let mut markers = Vec::new();
                    for span in &spans {
                        let (_, range, scalar_start) = lines
                            .iter()
                            .find(|(line, _, _)| line.id == span.line_id)
                            .unwrap();
                        let left = marker.start.max(range.start);
                        let right = marker.end.min(range.end);
                        if left >= right {
                            continue;
                        }
                        let start = text
                            .scalar_at_utf16(left)
                            .ok_or("Invalid marker start")?
                            .saturating_sub(*scalar_start)
                            .max(span.start);
                        let end = text
                            .scalar_at_utf16(right)
                            .ok_or("Invalid marker end")?
                            .saturating_sub(*scalar_start)
                            .min(span.end);
                        if start < end {
                            markers.push(SourceSpan {
                                line_id: span.line_id.clone(),
                                start,
                                end,
                            });
                        }
                    }
                    if !markers.is_empty() {
                        attributes.insert("marker".into(), json!(markers));
                    }
                }
            }
        }
        attributes.insert("reading_stream_id".into(), json!(stream));
        let mut ownership = node.and_then(|node| node.ownership.clone());
        if let Some(id) = &stream {
            let document = streams.iter().find(|(scope, _)| scope.id == *id)
                .map(|(scope, _)| scope.ownership.document_id.as_str());
            if let Some(document) = document {
                let owner = ownership.get_or_insert_with(|| crate::StructureOwnership {
                    document_id: document.to_owned(), quotation_id: None, reading_stream_id: None });
                if owner.document_id != document {
                    return Err("Node reading stream belongs to another document".into());
                }
                owner.reading_stream_id = Some(id.clone());
            }
        }
        let mut ids = HashSet::new();
        result.push(CompositionBlock {
            id: format!("{}@{}", spans[0].line_id, spans[0].start),
            kind: serde_json::to_value(role)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned(),
            line_ids: spans
                .iter()
                .filter_map(|span| {
                    ids.insert(span.line_id.clone())
                        .then_some(span.line_id.clone())
                })
                .collect(),
            spans,
            attributes,
            ownership,
        });
    }
    let source_anchor = |offset: usize| -> Result<String, String> {
        let (line, _, scalar_start) = lines
            .iter()
            .find(|(_, range, _)| range.start <= offset && offset < range.end)
            .ok_or("Scope start has no source fragment")?;
        Ok(format!(
            "{}@{}",
            line.id,
            text.scalar_at_utf16(offset)
                .ok_or("Scope starts inside source character")?
                - scalar_start
        ))
    };
    let mut continuations: BTreeMap<String, BTreeMap<String, crate::CompositionLink>> = BTreeMap::new();
    for link in structure.continuations.as_deref().unwrap_or_default() {
        let kind=serde_json::to_value(link.kind).unwrap().as_str().unwrap().to_owned();
        continuations.entry(kind.clone()).or_default().insert(source_anchor(link.from)?, crate::CompositionLink {
            next:source_anchor(link.to)?, separator:link.separator.clone(),
            attributes:BTreeMap::from([("kind".into(),json!(kind))]),
        });
    }
    // Native membership establishes one object across pages even when its
    // literal text seam is not yet established. Keep that obligation in the
    // continuation itself; only complete assembly requires a separator.
    let mut native_parts = HashMap::<&str, Vec<&CompositionBlock>>::new();
    for group in &result {
        if let Some(id)=group.attributes.get("native_id").and_then(Value::as_str) {
            native_parts.entry(id).or_default().push(group);
        }
    }
    for (id, parts) in native_parts {
        let Some(node)=structure.nodes.iter().find(|node|node.id==id) else {continue;};
        let kind=if node.kind==NodeKind::Heading || node.role==Some(PrimaryRole::Heading) {"heading"}
            else if matches!(node.kind,NodeKind::Footnote|NodeKind::Endnote) {"note"}
            else if node.kind==NodeKind::ListItem {"list"} else {"paragraph"};
        for pair in parts.windows(2) {
            let page=|group:&CompositionBlock| lines.iter().find(|(line,_,_)|line.id==group.spans[0].line_id).unwrap().0.page;
            if page(pair[0])==page(pair[1]) || pair[0].attributes.get("reading_stream_id") != pair[1].attributes.get("reading_stream_id")
                || pair[0].ownership.as_ref().map(|owner| &owner.document_id) != pair[1].ownership.as_ref().map(|owner| &owner.document_id) {continue;}
            let layer=continuations.entry(kind.to_owned()).or_default();
            if !layer.contains_key(&pair[0].id) && !layer.values().any(|link|link.next==pair[1].id) {
                layer.insert(pair[0].id.clone(),crate::CompositionLink {next:pair[1].id.clone(),separator:None,
                    attributes:BTreeMap::from([("kind".into(),json!(kind))])});
            }
        }
    }
    let mut joins=BTreeMap::new();
    for link in &structure.joins {
        joins.insert(source_anchor(link.from)?,crate::CompositionLink { next:source_anchor(link.to)?,
            separator:Some(link.separator.clone()),attributes:BTreeMap::new() });
    }
    let documents = structure
        .scope_annotations
        .iter()
        .filter(|scope| scope.kind == crate::StructureScopeKind::Document)
        .collect::<Vec<_>>();
    let mut openings = HashMap::new();
    let mut events = Vec::new();
    for scope in documents {
        if let Some(start) = scope.ranges.iter().map(|range| range.start).min() {
            openings.insert(scope.id.as_str(), start);
            for range in &scope.owned_ranges {
                if range.start < range.end {
                    events.push((range.start, range.end, scope.id.as_str()));
                }
            }
        }
    }
    events.sort_by_key(|&(start, end, _)| (start, end));
    let mut resumes = BTreeMap::new();
    let mut previous_document = None;
    for (start, _, document) in events {
        if previous_document == Some(document) {
            continue;
        }
        if openings[document] < start {
            resumes.insert(
                source_anchor(start)?,
                json!({"document":document}),
            );
        }
        previous_document = Some(document);
    }
    crate::composition::canonicalize_reading_streams(&mut result);
    crate::composition::validate_manuscript_boundaries(&structure.manuscript_edits, result.iter().flat_map(|group|&group.spans))?;
    let annotations = crate::composition_annotation_seed::seed_annotations(structure, &request.structure, &text, &lines)?;
    let mut represented: HashSet<String> = result.iter().filter_map(|group|
        group.attributes.get("native_id").and_then(Value::as_str).map(str::to_owned)).collect();
    for annotation in &annotations {
        represented.insert(annotation.id.clone());
        if annotation.kind == "table" {
            let rows: HashSet<_> = structure.nodes.iter().filter(|node| node.kind == NodeKind::Row
                && node.parent_id.as_deref() == Some(annotation.id.as_str())).map(|node|node.id.as_str()).collect();
            represented.extend(rows.iter().map(|id|(*id).to_owned()));
            represented.extend(structure.nodes.iter().filter(|node| node.kind == NodeKind::Cell
                && node.parent_id.as_deref().is_some_and(|id|rows.contains(id))).map(|node|node.id.clone()));
        }
        if annotation.kind == "note" {
            represented.extend(structure.notes.iter().filter(|note|note.id == annotation.id)
                .filter_map(|note|note.node_id.clone()));
        }
    }
    let retained_node_ids: Vec<String> = request.structure.get("nodes").and_then(Value::as_array).into_iter().flatten()
        .filter_map(|node|node["id"].as_str()).filter(|id|!represented.contains(*id)).map(str::to_owned).collect();
    let removable_nodes = crate::composition_annotation_seed::seed_removable_nodes(
        structure, &request.structure, &retained_node_ids, &text, &lines)?;
    Ok(PreparedCompositionBlocks {
        groups: result,
        annotations,
        retained_node_ids,
        removable_nodes,
        manuscript_edits: structure.manuscript_edits.clone(),
        continuations,
        joins,
        resumes,
    })
}
