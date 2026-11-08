use super::*;

/// The structure from a reader's evidence: its own nodes, with what inference finds (or the blocks it was given in
/// place of inference), each node's parent resolved from the label (or id) it names, ranges in UTF-16.
pub(crate) fn derive(mut evidence: DocumentInput, precomputed: Option<Vec<StructureNode>>) -> DocumentStructure {
    let coordinates = ScalarText::new(&evidence.text);
    #[cfg(feature = "structure-inference")]
    let inferred = precomputed.unwrap_or_else(|| {
        inference::inferred_blocks(&evidence, &coordinates)
    });
    #[cfg(not(feature = "structure-inference"))]
    let inferred = precomputed.unwrap_or_default();
    let mut nodes = std::mem::take(&mut evidence.nodes);
    let native_labels = (!inferred.is_empty())
        .then(|| {
            nodes
                .iter()
                .flat_map(|node| {
                    node.label
                        .iter()
                        .chain(node.aliases.iter().flatten())
                        .map(|label| (node.kind.evidence(), label.to_ascii_lowercase()))
                })
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let mut counters = HashMap::<NodeKind, usize>::new();
    let mut diagnostics = Vec::new();
    for mut block in inferred {
        let Some(range) = evidence.clip_inference(block.kind.evidence(), block.range) else {
            continue;
        };
        block.range = range;
        if block.content_start.is_some_and(|at| {
            block.kind != NodeKind::Section || at < block.range.start || at > block.range.end
        }) || (!native_labels.is_empty()
            && block.label.as_ref().is_some_and(|label| {
                native_labels.contains(&(block.kind.evidence(), label.to_ascii_lowercase()))
            }))
        {
            continue;
        }
        let ordinal = counters.entry(block.kind).or_default();
        *ordinal += 1;
        let source = match block.source {
            Derivation::Native => "native",
            Derivation::Heuristic => "heuristic",
        };
        block.id = format!("{source}-{}-{:06}", block.kind.name(), ordinal);
        if let Some(code) = block.properties.remove("diagnostic") {
            diagnostics.push(StructureDiagnostic {
                severity: if code.ends_with("violation") {
                    DiagnosticSeverity::Warning
                } else {
                    DiagnosticSeverity::Info
                },
                code,
                ranges: vec![block.range],
                node_ids: vec![block.id.clone()],
            });
        }
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
            for label in node.label.iter().chain(node.aliases.iter().flatten()) {
                let candidates = labels.entry(label.to_ascii_lowercase()).or_default();
                if candidates.last() != Some(&position) {
                    candidates.push(position);
                }
            }
        }
        for position in 0..nodes.len() {
            let parent = nodes[position].parent_id.take();
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
    DocumentStructure::project_scalar_parts(&coordinates, &mut nodes, &mut diagnostics);
    drop(coordinates);
    DocumentStructure::from_projected_parts(
        evidence.document_id,
        evidence.provider,
        evidence.url,
        evidence.doc_type.map(str::to_owned),
        Some(evidence.profile),
        evidence.text,
        evidence.text_sha256,
        evidence.source_sha256,
        evidence.scope,
        evidence.origins,
        nodes,
        diagnostics,
    )
}
