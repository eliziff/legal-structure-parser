//! Derive a structure's levels and extents from its recorded parents and membership.
use crate::{DocumentStructure, NodeKind, PrimaryRole, ScalarRange};
use std::collections::{HashMap, VecDeque};

fn envelope(ranges: &[ScalarRange]) -> Option<ScalarRange> {
    Some(ScalarRange {
        start: ranges.iter().map(|range| range.start).min()?,
        end: ranges.iter().map(|range| range.end).max()?,
    })
}

fn heading(node: &crate::StructureNode) -> bool {
    matches!(node.kind, NodeKind::Heading | NodeKind::Section) || node.role == Some(PrimaryRole::Heading)
}

/// Each node's one-based level under its parents, its extent (its own membership with its
/// descendants'), and its readable membership in the rendered text.
/// The producer, which completes the structure, refreshes its revision.
pub(crate) fn assemble_structure(mut structure: DocumentStructure) -> Result<DocumentStructure, String> {
    let positions = structure.nodes.iter().enumerate()
        .map(|(i, node)| (node.id.clone(), i)).collect::<HashMap<_, _>>();
    let mut children = vec![Vec::new(); structure.nodes.len()];
    let mut roots = VecDeque::new();
    for (i, node) in structure.nodes.iter().enumerate() {
        match &node.parent_id {
            Some(parent) => children[*positions.get(parent).ok_or("Structural parent is absent")?].push(i),
            None => roots.push_back((i, 1)),
        }
    }
    let mut order = Vec::with_capacity(structure.nodes.len());
    let mut levels = vec![0; structure.nodes.len()];
    while let Some((i, level)) = roots.pop_front() {
        levels[i] = level;
        order.push(i);
        roots.extend(children[i].iter().map(|child| (*child, level + 1)));
    }
    if order.len() != structure.nodes.len() {
        return Err("Structural parents contain a cycle".into());
    }
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
    Ok(structure)
}
