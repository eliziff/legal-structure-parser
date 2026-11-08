//! Derive a structure's levels and extents from its recorded parents and membership.
use crate::document::public_structure_label;
use crate::{DocumentStructure, NodeKind, PrimaryRole, ScalarRange, StructureNode};
use std::collections::{HashMap, HashSet, VecDeque};

fn envelope(ranges: &[ScalarRange]) -> Option<ScalarRange> {
    Some(ScalarRange {
        start: ranges.iter().map(|range| range.start).min()?,
        end: ranges.iter().map(|range| range.end).max()?,
    })
}

fn heading(node: &crate::StructureNode) -> bool {
    matches!(node.kind, NodeKind::Heading | NodeKind::Section) || node.role == Some(PrimaryRole::Heading)
}

/// The nodes a statute's readers address by label: its paragraphs and prose, pages, provisions, notes and tables.
fn addressed(node: &StructureNode) -> bool {
    matches!(node.kind, NodeKind::Paragraph | NodeKind::Prose | NodeKind::Page | NodeKind::Section | NodeKind::Footnote | NodeKind::Table
        | NodeKind::Row | NodeKind::Cell)
}

/// A statute's outline as its readers address it, written once: each addressed node's label as printed, the
/// occurrence that told a repeated label apart left off (`public_structure_label`), and its parent the outermost one
/// its label's chain reaches, a repeated label read as its last printing (in the text's order, an enclosing provision
/// before what it holds).
pub fn legislation_outline(nodes: &mut [StructureNode]) {
    let index = nodes.iter().enumerate().map(|(i, node)| (node.id.clone(), i)).collect::<HashMap<_, _>>();
    let parent_of = |nodes: &[StructureNode], i: usize| nodes[i].parent_id.as_deref().and_then(|id| index.get(id)).copied();
    let public = nodes.iter().map(|node| node.label.as_deref().map(|label| public_structure_label(label).into_owned())).collect::<Vec<_>>();
    let mut order = (0..nodes.len()).filter(|&i| addressed(&nodes[i])).collect::<Vec<_>>();
    order.sort_by(|&a, &b| nodes[a].range.start.cmp(&nodes[b].range.start).then(nodes[b].range.end.cmp(&nodes[a].range.end)).then(public[a].cmp(&public[b])));
    let owners = order.iter().filter_map(|&i| Some((public[i].clone()?, i))).collect::<HashMap<_, _>>();
    let parents = order.iter().map(|&i| {
        let owner = public[i].as_ref().and_then(|label| owners.get(label)).copied().unwrap_or(i);
        let mut seen = HashSet::new();
        let parent = std::iter::successors(parent_of(nodes, owner), |&parent| {
            let label = public[parent].clone()?;
            seen.insert(label.clone()).then(|| owners.get(&label).and_then(|&owner| parent_of(nodes, owner))).flatten()
        })
        .last();
        (i, parent.map(|parent| nodes[parent].id.clone()))
    }).collect::<Vec<_>>();
    for (i, parent) in parents {
        nodes[i].parent_id = parent;
    }
    for (node, label) in nodes.iter_mut().zip(public) {
        if addressed(node) {
            node.label = label;
        }
    }
}

/// Each node's one-based level under its parents (`parent_id`), and the nodes from the roots down.
fn levels(nodes: &[StructureNode]) -> Result<(Vec<usize>, Vec<usize>), String> {
    let positions = nodes.iter().enumerate().map(|(i, node)| (node.id.as_str(), i)).collect::<HashMap<_, _>>();
    let mut children = vec![Vec::new(); nodes.len()];
    let mut roots = VecDeque::new();
    for (i, node) in nodes.iter().enumerate() {
        match &node.parent_id {
            Some(parent) => children[*positions.get(parent.as_str()).ok_or("Structural parent is absent")?].push(i),
            None => roots.push_back((i, 1)),
        }
    }
    let mut order = Vec::with_capacity(nodes.len());
    let mut levels = vec![0; nodes.len()];
    while let Some((i, level)) = roots.pop_front() {
        levels[i] = level;
        order.push(i);
        roots.extend(children[i].iter().map(|child| (*child, level + 1)));
    }
    if order.len() != nodes.len() {
        return Err("Structural parents contain a cycle".into());
    }
    Ok((levels, order))
}

/// Each node's one-based level under its parents, written on it.
pub fn write_levels(nodes: &mut [StructureNode]) -> Result<(), String> {
    let (levels, _) = levels(nodes)?;
    for (node, level) in nodes.iter_mut().zip(levels) {
        node.level = Some(level);
    }
    Ok(())
}

/// Each node's one-based level under its parents, its extent (its own membership with its
/// descendants'), and its readable membership in the rendered text. The producer, which completes
/// the structure, refreshes its revision; the tagger's retag, which replaces the graph's
/// paragraph-level nodes after the producer assembled it, runs it again so every node it adds
/// carries its level, extent and rendered membership like the nodes it replaced.
pub fn assemble_in_place(structure: &mut DocumentStructure) -> Result<(), String> {
    let positions = structure.nodes.iter().enumerate()
        .map(|(i, node)| (node.id.clone(), i)).collect::<HashMap<_, _>>();
    let (levels, order) = levels(&structure.nodes)?;
    let mut spans = structure.nodes.iter()
        .map(|node| if node.ranges.is_empty() { vec![node.range] } else { node.ranges.clone() })
        .collect::<Vec<_>>();
    let same_text = structure.rendered_text.as_deref().is_none_or(|value| value == structure.text);
    let own_rendered = structure.nodes.iter().enumerate().map(|(i, node)| {
        if same_text {
            return Ok(spans[i].clone());
        }
        if !node.rendered_membership.is_empty() {
            return Ok(node.rendered_membership.clone());
        }
        if node.rendered_range == Some(ScalarRange { start: 0, end: 0 }) && node.rendered_ranges.is_empty() {
            return Ok(Vec::new());
        }
        if heading(node) {
            return Err("Rendered sections require their own logical membership".to_owned());
        }
        if !node.rendered_ranges.is_empty() {
            return Ok(node.rendered_ranges.clone());
        }
        node.rendered_range.map(|range| vec![range])
            .ok_or_else(|| "Missing source-to-manuscript membership".to_owned())
    }).collect::<Result<Vec<_>, String>>()?;
    let mut rendered_extents = own_rendered.clone();
    for i in order.into_iter().rev() {
        ScalarRange::normalize_union(&mut spans[i]);
        ScalarRange::normalize_union(&mut rendered_extents[i]);
        if let Some(parent) = &structure.nodes[i].parent_id {
            let parts = spans[i].clone();
            spans[positions[parent]].extend(parts);
            let parts = rendered_extents[i].clone();
            rendered_extents[positions[parent]].extend(parts);
        }
    }
    for (i, node) in structure.nodes.iter_mut().enumerate() {
        node.level = Some(levels[i]);
        node.extent = std::mem::take(&mut spans[i]);
        node.rendered_ranges = if heading(node) { rendered_extents[i].clone() } else { own_rendered[i].clone() };
        node.rendered_membership = own_rendered[i].clone();
        node.rendered_range = Some(envelope(&node.rendered_ranges).unwrap_or(ScalarRange { start: 0, end: 0 }));
    }
    Ok(())
}
