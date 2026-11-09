//! Derive a structure's levels and extents from its recorded parents and membership.
use crate::{DocumentStructure, NodeKind, PrimaryRole, ScalarRange, StructureNode};
use std::collections::{HashMap, VecDeque};

fn heading(node: &crate::StructureNode) -> bool {
    matches!(node.kind, NodeKind::Heading | NodeKind::Section) || node.role == Some(PrimaryRole::Heading)
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
        if !node.rendered_membership.is_empty() || node.rendered_ranges.is_empty() {
            return Ok(node.rendered_membership.clone());
        }
        if heading(node) {
            return Err("Rendered sections require their own logical membership".to_owned());
        }
        Ok(node.rendered_ranges.clone())
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
    }
    Ok(())
}
