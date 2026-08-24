use super::*;

fn native_kind(kind: EvidenceKind) -> NodeKind {
    match kind {
        EvidenceKind::Paragraph => NodeKind::Paragraph,
        EvidenceKind::Prose => NodeKind::Prose,
        EvidenceKind::Page => NodeKind::Page,
        EvidenceKind::Section => NodeKind::Section,
        EvidenceKind::Heading => NodeKind::Heading,
        EvidenceKind::Footnote => NodeKind::Footnote,
        EvidenceKind::Endnote => NodeKind::Endnote,
        EvidenceKind::List => NodeKind::List,
        EvidenceKind::Table => NodeKind::Table,
        EvidenceKind::Row => NodeKind::Row,
        EvidenceKind::Cell => NodeKind::Cell,
    }
}
fn infer_graph(mut evidence: DocumentInput, precomputed: Option<Vec<Block>>) -> DocumentStructure {
    let coordinates = ScalarText::new(&evidence.text);
    #[cfg(feature = "structure-inference")]
    let inferred = precomputed.unwrap_or_else(|| {
        if evidence.needs_inference() {
            inference::inferred_blocks(&evidence, &coordinates)
        } else {
            Vec::new()
        }
    });
    #[cfg(not(feature = "structure-inference"))]
    let inferred = precomputed.unwrap_or_default();
    let native_claims = std::mem::take(&mut evidence.native_claims);
    let native_labels = (!inferred.is_empty())
        .then(|| {
            native_claims
                .iter()
                .flat_map(|claim| {
                    claim
                        .label
                        .iter()
                        .chain(&claim.aliases)
                        .map(|label| (claim.kind, label.to_ascii_lowercase()))
                })
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    let mut native_parents = Vec::with_capacity(native_claims.len());
    let mut nodes = native_claims
        .into_iter()
        .map(|claim| {
            native_parents.push(claim.parent_label);
            let mut node = StructureNode::new(
                claim.id,
                native_kind(claim.kind),
                claim.range,
                claim.origin_id,
                Derivation::Native,
                None,
            );
            node.label = claim.label;
            node.aliases = (!claim.aliases.is_empty()).then_some(claim.aliases);
            node.anchor = claim.anchor;
            node
        })
        .collect::<Vec<_>>();
    let mut counters = HashMap::<NodeKind, usize>::new();
    let mut generated_parents = Vec::new();
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
            Derivation::Model => "model",
        };
        let id = format!("{source}-{}-{:06}", block.kind.name(), ordinal);
        if let Some(code) = block.diagnostic {
            diagnostics.push(StructureDiagnostic {
                code: code.to_owned(),
                severity: if code.ends_with("violation") {
                    DiagnosticSeverity::Warning
                } else {
                    DiagnosticSeverity::Info
                },
                ranges: vec![block.range],
                node_ids: vec![id.clone()],
            });
        }
        generated_parents.push(block.parent_label);
        let mut node = StructureNode::new(
            id,
            block.kind,
            block.range,
            block.origin_id,
            block.source,
            None,
        );
        node.label = block.label;
        node.aliases = (!block.aliases.is_empty()).then_some(block.aliases);
        node.content_start = block.content_start;
        nodes.push(node);
    }
    if native_parents
        .iter()
        .chain(&generated_parents)
        .any(Option::is_some)
    {
        let mut labels = HashMap::with_capacity(nodes.len());
        for (position, node) in nodes.iter().enumerate() {
            for label in node.label.iter().chain(node.aliases.iter().flatten()) {
                labels.insert(label.to_ascii_lowercase(), position);
            }
        }
        for (position, parent) in native_parents
            .iter()
            .map(Option::as_ref)
            .chain(generated_parents.iter().map(Option::as_ref))
            .enumerate()
        {
            nodes[position].parent_id = parent
                .and_then(|label| match labels.get(label) {
                    Some(&position) => Some(position),
                    None if label.bytes().any(|byte| byte.is_ascii_uppercase()) => {
                        labels.get(&label.to_ascii_lowercase()).copied()
                    }
                    None => None,
                })
                .map(|parent_position| nodes[parent_position].id.clone());
        }
    }
    DocumentStructure::project_scalar_parts(&coordinates, &mut nodes, &mut [], &mut diagnostics);
    drop(coordinates);
    let document_id = std::mem::take(&mut evidence.document_id);
    let provider = std::mem::take(&mut evidence.provider);
    let profile = evidence.profile;
    let text_sha256 = std::mem::take(&mut evidence.text_sha256);
    let url = evidence.url.take();
    let doc_type = evidence.doc_type.map(str::to_owned);
    let text = std::mem::take(&mut evidence.text);
    let source_sha256 = evidence.source_sha256.take();
    let scope = evidence.scope;
    let origins = evidence.origins;
    let mut structure = DocumentStructure::from_projected_parts(
        document_id,
        text,
        text_sha256,
        source_sha256,
        scope,
        origins,
        nodes,
        Vec::new(),
        diagnostics,
    );
    structure.provider = provider;
    structure.profile = Some(profile);
    structure.url = url;
    structure.doc_type = doc_type;
    structure
}

#[cfg(all(feature = "structure-inference", test))]
pub(crate) fn derive_structure_evidence(
    evidence: DocumentInput,
) -> Result<DocumentStructure, EngineError> {
    Ok(infer_graph(evidence, None))
}

#[cfg(any(feature = "journal", test))]
pub(crate) fn derive_native_structure_evidence(
    evidence: DocumentInput,
) -> Result<DocumentStructure, EngineError> {
    Ok(infer_graph(evidence, Some(Vec::new())))
}

#[cfg(feature = "structure-inference")]
pub(crate) fn derive_trusted(input: DocumentInput) -> Result<DocumentStructure, EngineError> {
    Ok(infer_graph(input, None))
}

#[cfg(feature = "structure-inference")]
pub(crate) fn derive_trusted_inferred(
    input: DocumentInput,
    inferred: Vec<Block>,
) -> Result<DocumentStructure, EngineError> {
    Ok(infer_graph(input, Some(inferred)))
}
