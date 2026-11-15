use std::collections::{HashMap, HashSet};
use legal_structure_model::document::{Derivation, DocumentStructure, NodeKind, StructureNode};
#[cfg(feature = "structure-inference")]
use crate::analysis::CitedText;
#[cfg(feature = "structure-inference")]
use crate::inference;
use crate::Evidence;

/// The structure from a reader's evidence: its own nodes, with what inference finds (or the blocks it was given in
/// place of inference), each node's parent resolved from the place (or id) it names: a node's label or alias, a
/// provision's under its kind's mark (`StructureNode::places`). The reader takes
/// its revision once it has set all it adds.
pub(crate) fn derive(mut evidence: Evidence, precomputed: Option<Vec<StructureNode>>) -> DocumentStructure {
    #[cfg(feature = "structure-inference")]
    let inferred = precomputed.unwrap_or_else(|| {
        inference::inferred_blocks(&evidence, &CitedText::new(&evidence.structure.text))
    });
    #[cfg(not(feature = "structure-inference"))]
    let inferred = precomputed.unwrap_or_default();
    let mut nodes = std::mem::take(&mut evidence.structure.nodes);
    let native_labels = (!inferred.is_empty())
        .then(|| {
            nodes
                .iter()
                .flat_map(|node| node.places().map(|place| (node.kind, place.to_ascii_lowercase())))
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let mut counters = HashMap::<NodeKind, usize>::new();
    // An inferred block may name its parent by the id inference gave it; that id is renumbered here.
    let mut renumbered = HashMap::<String, String>::new();
    for mut block in inferred {
        let Some(range) = evidence.clip_inference(block.kind, block.range) else {
            continue;
        };
        block.range = range;
        if block.content_start.is_some_and(|at| {
            block.kind != NodeKind::Section || at < block.range.start || at > block.range.end
        }) || (!native_labels.is_empty()
            && block.label.is_some()
            && block.places().next().is_some_and(|place| {
                native_labels.contains(&(block.kind, place.to_ascii_lowercase()))
            }))
        {
            // (What it holds stands under the node that takes its place.)
            let place = block.places().next();
            if let Some(place) = place.filter(|_| !block.id.is_empty()) {
                renumbered.insert(std::mem::take(&mut block.id), place);
            }
            continue;
        }
        let ordinal = counters.entry(block.kind).or_default();
        *ordinal += 1;
        let source = match block.source {
            Derivation::Native => "native",
            Derivation::Heuristic => "heuristic",
        };
        let id = format!("{source}-{}-{:06}", block.kind.name(), ordinal);
        let provisional = std::mem::replace(&mut block.id, id);
        if !provisional.is_empty() {
            renumbered.insert(provisional, block.id.clone());
        }
        // An instrument's ladder fault is the candidate runs' to read (`detect_structure_candidate_runs`), no node's.
        block.properties.remove("diagnostic");
        nodes.push(block);
    }
    if nodes.iter().any(|node| node.parent_id.is_some()) {
        let identities = nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.id.clone(), position))
            .collect::<HashMap<_, _>>();
        let mut labels = HashMap::<String, Vec<usize>>::with_capacity(nodes.len());
        for (position, node) in nodes.iter().enumerate() {
            for place in node.places() {
                let candidates = labels.entry(place.to_ascii_lowercase()).or_default();
                if candidates.last() != Some(&position) {
                    candidates.push(position);
                }
            }
        }
        for position in 0..nodes.len() {
            let parent = nodes[position].parent_id.take()
                .map(|parent| renumbered.get(&parent).cloned().unwrap_or(parent));
            nodes[position].parent_id = parent
                .and_then(|label| {
                    if let Some(&candidate) = identities.get(label.as_str()) {
                        return (candidate != position).then_some(candidate);
                    }
                    let candidates = labels
                        .get(&label)
                        .or_else(|| labels.get(&label.to_ascii_lowercase()))?;
                    if candidates.len() == 1 {
                        return candidates
                            .first()
                            .copied()
                            .filter(|&candidate| candidate != position);
                    }
                    let child = nodes[position].range;
                    let containing = candidates.iter().copied().filter(|&candidate| {
                        let range = nodes[candidate].range;
                        candidate != position
                            && range.start <= child.start
                            && range.end >= child.end
                    });
                    // Repeated legal labels belong to their containing source
                    // scope, not whichever same-labelled node was inserted last.
                    let span = |candidate: usize| {
                        nodes[candidate].range.end - nodes[candidate].range.start
                    };
                    let first = containing
                        .clone()
                        .min_by_key(|&candidate| span(candidate))?;
                    (containing
                        .filter(|&candidate| span(candidate) == span(first))
                        .count()
                        == 1)
                        .then_some(first)
                })
                .map(|parent_position| nodes[parent_position].id.clone());
        }
    }
    // Every node's level is written once its parents stand.
    legal_structure_model::write_levels(&mut nodes).ok();
    let mut structure = evidence.structure;
    structure.profile = Some(evidence.profile);
    structure.nodes = nodes;
    structure
}
