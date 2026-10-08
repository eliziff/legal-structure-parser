use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[cfg(feature = "structure-inference")]
pub fn detect_structure_candidate_runs(value: &str) -> Vec<StructureCandidateRun> {
    let text = ScalarText::new(value);
    let mut runs = inference::raw_numeric_runs(&text);
    let mut raw_enumerators = inference::raw_enumerator_runs(&text);
    raw_enumerators.extend(inference::raw_bullet_runs(&text));
    let points = inference::detect_instrument_grammar(&text, false);
    let parent_indexes = points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            point.parent_label.as_ref().and_then(|parent| {
                points[..index].iter().rposition(|candidate| {
                    candidate.label == *parent
                        && candidate.range.start <= point.range.start
                        && point.range.start < candidate.range.end
                })
            })
        })
        .collect::<Vec<_>>();
    // A section read from nothing but its number, on the line of a number the text counts from
    // 1, is a numbered paragraph; its sub-items are then read as lists of their own.
    let counted = runs
        .iter()
        .filter(|run| run.rooted && run.consecutive)
        .flat_map(|run| run.markers.iter().map(|marker| marker.marker_range.start))
        .collect::<HashSet<_>>();
    let paragraphs = (0..points.len())
        .filter(|index| {
            parent_indexes[*index].is_none()
                && points[*index].direct
                && counted.contains(&points[*index].range.start)
        })
        .collect::<HashSet<_>>();
    let mut grouped = BTreeMap::<usize, (Vec<StructureMarkerCandidate>, bool)>::new();
    for (index, point) in points.into_iter().enumerate() {
        let grammar_value = point.label;
        let mut root = index;
        let mut level = 0;
        while let Some(parent) = parent_indexes[root] {
            root = parent;
            level += 1;
        }
        if paragraphs.contains(&root) {
            continue;
        }
        let content_start = point.content_start;
        let entry = grouped.entry(root).or_insert_with(|| (Vec::new(), true));
        entry.1 &= !matches!(
            point.diagnostic,
            Some(
                "instrument_ladder_forward_jump"
                    | "instrument_ladder_midcounter_open"
                    | "instrument_ladder_violation"
            )
        );
        let label = text
            .slice(point.range.start..content_start)
            .expect("grammar point range is bounded")
            .trim()
            .to_owned();
        entry.0.push(StructureMarkerCandidate {
            id: format!("grammar-point-{index:06}"),
            range: point.range,
            marker_range: ScalarRange {
                start: point.range.start,
                end: content_start,
            },
            label,
            grammar_value,
            parent_candidate_id: parent_indexes[index]
                .map(|parent| format!("grammar-point-{parent:06}")),
            level,
            content_start,
        });
    }
    let mut hierarchy = grouped
        .into_values()
        .filter(|(markers, _)| !markers.is_empty())
        .map(|(markers, consecutive)| StructureCandidateRun {
            id: String::new(),
            grammar: CandidateGrammar::Hierarchy,
            range: ScalarRange {
                start: markers[0].range.start,
                end: markers.iter().map(|marker| marker.range.end).max().unwrap(),
            },
            rooted: markers[0].parent_candidate_id.is_none(),
            consecutive,
            markers,
        })
        .collect::<Vec<_>>();
    let captured = hierarchy
        .iter()
        .flat_map(|run| run.markers.iter().map(|marker| marker.range.start))
        .collect::<HashSet<_>>();
    raw_enumerators.retain(|run| {
        !run.markers
            .iter()
            .any(|marker| captured.contains(&marker.range.start))
    });
    hierarchy.append(&mut raw_enumerators);
    hierarchy.sort_by_key(|run| (run.range.start, run.range.end));
    for (run_index, run) in hierarchy.iter_mut().enumerate() {
        let prefix = match run.grammar {
            CandidateGrammar::Hierarchy => "hierarchy",
            CandidateGrammar::Enumerator => "enumerator",
            CandidateGrammar::Numeric => "numeric",
        };
        run.id = format!("{prefix}-{:06}", run_index + 1);
        let candidate_ids = run
            .markers
            .iter()
            .enumerate()
            .map(|(marker_index, marker)| {
                (
                    marker.id.clone(),
                    format!("{}-{:04}", run.id, marker_index + 1),
                )
            })
            .collect::<HashMap<_, _>>();
        for (marker_index, marker) in run.markers.iter_mut().enumerate() {
            marker.id = format!("{}-{:04}", run.id, marker_index + 1);
            marker.parent_candidate_id = marker
                .parent_candidate_id
                .as_ref()
                .and_then(|parent| candidate_ids.get(parent))
                .cloned();
        }
    }
    runs.extend(hierarchy);
    runs.sort_by_key(|run| {
        let grammar = match run.grammar {
            CandidateGrammar::Numeric => 0,
            CandidateGrammar::Hierarchy => 1,
            CandidateGrammar::Enumerator => 2,
        };
        (run.range.start, grammar, run.range.end)
    });
    runs
}

/// The indexes of the nodes of `kinds`, in the order they open.
#[cfg(feature = "structure-inference")]
fn nodes_by_start(nodes: &[StructureNode], kinds: &[NodeKind]) -> Vec<usize> {
    let mut found = (0..nodes.len())
        .filter(|index| kinds.contains(&nodes[*index].kind))
        .collect::<Vec<_>>();
    found.sort_by_key(|index| nodes[*index].range.start);
    found
}

/// The shortest of `containers` (node indexes in the order they open) holding `range` that
/// `accept` takes, the first in node order among equals: the node a scan of every node finds.
/// A node opening further back is no shorter than the distance to `range`'s end, so the scan
/// back from `range` stops once that distance passes the shortest found.
#[cfg(feature = "structure-inference")]
fn smallest_container(
    nodes: &[StructureNode],
    containers: &[usize],
    range: ScalarRange,
    accept: impl Fn(usize) -> bool,
) -> Option<usize> {
    let opened = containers.partition_point(|index| nodes[*index].range.start <= range.start);
    let mut best: Option<(usize, usize)> = None;
    for &index in containers[..opened].iter().rev() {
        let candidate = nodes[index].range;
        if best.is_some_and(|(length, _)| range.end.saturating_sub(candidate.start) > length) {
            break;
        }
        if range.end <= candidate.end && accept(index) {
            let key = (candidate.end - candidate.start, index);
            if best.is_none_or(|best| key < best) {
                best = Some(key);
            }
        }
    }
    best.map(|(_, index)| index)
}

#[cfg(feature = "structure-inference")]
pub(crate) fn resolve_structure_candidates<'a>(
    runs: &'a [StructureCandidateRun],
    evidence: &'a [CandidateEvidenceV2],
) -> Result<Vec<ResolvedCandidate<'a>>, EngineError> {
    let mut provision_starts = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Hierarchy && run.rooted && run.consecutive)
        .flat_map(|run| {
            run.markers
                .iter()
                .map(|candidate| candidate.marker_range.start)
        })
        .collect::<HashSet<_>>();
    let mut candidate_ids = HashSet::new();
    for run in runs {
        for candidate in &run.markers {
            if candidate.id.is_empty() || !candidate_ids.insert(candidate.id.as_str()) {
                return Err(EngineError::invalid(format!(
                    "candidate id '{}' is empty or duplicated",
                    candidate.id
                )));
            }
        }
    }
    let mut evidence_by_candidate = HashMap::new();
    for item in evidence {
        if !candidate_ids.contains(item.candidate_id.as_str()) {
            return Err(EngineError::invalid(format!(
                "evidence names unknown candidate '{}'",
                item.candidate_id
            )));
        }
        if item.page_indexes.is_empty()
            || item.line_ids.is_empty()
            || item.line_ids.iter().any(String::is_empty)
        {
            return Err(EngineError::invalid(format!(
                "candidate '{}' has incomplete page or line identity",
                item.candidate_id
            )));
        }
        if evidence_by_candidate
            .insert(item.candidate_id.as_str(), item)
            .is_some()
        {
            return Err(EngineError::invalid(format!(
                "candidate '{}' has duplicate evidence",
                item.candidate_id
            )));
        }
    }
    // A quotation's numbers are the quoted text's: they open none of this text's sections or
    // paragraphs, and decide nothing about them.
    let quoted = |candidate: &StructureMarkerCandidate| {
        evidence_by_candidate
            .get(candidate.id.as_str())
            .is_some_and(|item| {
                item.observations
                    .contains(&CandidateObservationV2::Quotation)
            })
    };
    let quoted_starts = runs
        .iter()
        .flat_map(|run| &run.markers)
        .filter(|candidate| quoted(candidate))
        .map(|candidate| candidate.marker_range.start)
        .collect::<HashSet<_>>();
    provision_starts.retain(|start| !quoted_starts.contains(start));
    // A count from 1 a quarter or more of whose numbers open prose that no section reads is the
    // text's numbered paragraphs: the sections read on its other numbers yield to it, and
    // their sub-items are read as lists.
    let mut yielded = HashSet::new();
    for run in runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Numeric && run.rooted && run.consecutive)
    {
        let (read, unread) = run.markers.iter().partition::<Vec<_>, _>(|candidate| {
            provision_starts.contains(&candidate.marker_range.start)
        });
        let prose = unread
            .iter()
            .filter(|candidate| {
                !quoted(candidate)
                    && evidence_by_candidate
                        .get(candidate.id.as_str())
                        .is_some_and(|item| {
                            item.observations
                                .contains(&CandidateObservationV2::BodyProseFlow)
                        })
            })
            .count();
        if !read.is_empty() && 4 * prose >= run.markers.len() {
            yielded.extend(read.iter().map(|candidate| candidate.marker_range.start));
        }
    }
    provision_starts.retain(|start| !yielded.contains(start));
    // A count most of whose numbers open sections is the sections'; the numbers among them
    // that open none are no paragraphs.
    let sectioned = |run: &StructureCandidateRun| {
        4 * run
            .markers
            .iter()
            .filter(|candidate| provision_starts.contains(&candidate.marker_range.start))
            .count()
            > 3 * run.markers.len()
    };
    let mut resolved = Vec::new();
    for run in runs {
        for candidate in &run.markers {
            let item = evidence_by_candidate.get(candidate.id.as_str()).copied();
            let mut seen_observations = HashSet::new();
            let observations = item.map_or_else(Vec::new, |item| {
                item.observations
                    .iter()
                    .copied()
                    .filter(|observation| seen_observations.insert(*observation))
                    .collect()
            });
            let excluded = observations.iter().any(|observation| {
                matches!(
                    observation,
                    CandidateObservationV2::CrossReference
                        | CandidateObservationV2::Furniture
                        | CandidateObservationV2::TableOrForm
                        | CandidateObservationV2::ContentsRow
                        | CandidateObservationV2::IndexRow
                        | CandidateObservationV2::TranscriptLineNumber
                )
            });
            let quotation = observations.contains(&CandidateObservationV2::Quotation);
            let (role, rule) = if excluded {
                (None, ResolutionRuleV2::DirectExclusion)
            } else if run.grammar == CandidateGrammar::Numeric
                && !quotation
                && run.rooted
                && run.consecutive
                && !provision_starts.contains(&candidate.marker_range.start)
                && !sectioned(run)
                && observations.contains(&CandidateObservationV2::BodyProseFlow)
            {
                (
                    Some(ResolvedRole::NumberedParagraph),
                    ResolutionRuleV2::RootedNumericProse,
                )
            } else if run.grammar == CandidateGrammar::Hierarchy
                && !quotation
                && run.rooted
                && run.consecutive
                && !yielded.contains(&run.markers[0].marker_range.start)
                && (observations.contains(&CandidateObservationV2::BodyProseFlow)
                    || observations.contains(&CandidateObservationV2::SectionHeading))
            {
                (
                    Some(ResolvedRole::Section),
                    ResolutionRuleV2::HierarchySection,
                )
            } else if matches!(
                run.grammar,
                CandidateGrammar::Hierarchy | CandidateGrammar::Enumerator
            ) && observations.contains(&CandidateObservationV2::ListItemLayout)
            {
                (
                    Some(ResolvedRole::ListItem),
                    ResolutionRuleV2::ListItemLayout,
                )
            } else {
                (None, ResolutionRuleV2::InsufficientEvidence)
            };
            resolved.push(ResolvedCandidate {
                candidate,
                role,
                proof: ResolutionProofV2 { rule, observations },
                page_indexes: item.map_or(&[], |item| item.page_indexes.as_slice()),
                line_ids: item.map_or(&[], |item| item.line_ids.as_slice()),
            });
        }
    }
    resolved.sort_by_key(|value| (value.candidate.range.start, value.candidate.range.end));
    Ok(resolved)
}

#[cfg(feature = "structure-inference")]
pub fn resolve_structure_graph(
    document_id: String,
    provider: String,
    text: &str,
    source_sha256: Option<String>,
    mut nodes: Vec<StructureNode>,
    runs: &[StructureCandidateRun],
    evidence: &[CandidateEvidenceV2],
    note_pairs: &[NotePairClaimV2],
    mut diagnostics: Vec<StructureDiagnostic>,
) -> Result<DocumentStructure, EngineError> {
    let scalar_text = ScalarText::new(text);
    let scalar_len = scalar_text.len();
    let mut node_ids = HashSet::new();
    for node in &nodes {
        if node.id.is_empty() || !node_ids.insert(node.id.clone()) {
            return Err(EngineError::invalid(format!(
                "node id '{}' is empty or duplicated",
                node.id
            )));
        }
        if !node.range.valid(scalar_len)
            || node
                .marker_range
                .is_some_and(|range| !range.valid(scalar_len))
            || node.line_ids.iter().any(String::is_empty)
        {
            return Err(EngineError::invalid(format!(
                "node '{}' has invalid source identity",
                node.id
            )));
        }
    }
    for node in &nodes {
        if node
            .parent_id
            .as_ref()
            .is_some_and(|parent| parent == &node.id || !node_ids.contains(parent))
        {
            return Err(EngineError::invalid(format!(
                "node '{}' has an invalid parent",
                node.id
            )));
        }
    }
    let mut all_candidate_ids = HashSet::new();
    for run in runs {
        if run.id.is_empty() || !run.range.valid(scalar_len) {
            return Err(EngineError::invalid(format!(
                "candidate run '{}' is invalid",
                run.id
            )));
        }
        let local_ids = run
            .markers
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<HashSet<_>>();
        for candidate in &run.markers {
            if !all_candidate_ids.insert(candidate.id.as_str())
                || !candidate.range.valid(scalar_len)
                || !candidate.marker_range.valid(scalar_len)
                || candidate.marker_range.start < candidate.range.start
                || candidate.marker_range.end > candidate.range.end
                || candidate.content_start < candidate.marker_range.end
                || candidate.content_start > candidate.range.end
                || candidate.range.start < run.range.start
                || candidate.range.end > run.range.end
                || candidate.parent_candidate_id.as_deref() == Some(candidate.id.as_str())
                || candidate
                    .parent_candidate_id
                    .as_deref()
                    .is_some_and(|parent| !local_ids.contains(parent))
            {
                return Err(EngineError::invalid(format!(
                    "candidate '{}' has invalid ranges or parent identity",
                    candidate.id
                )));
            }
        }
    }
    let resolved_candidates = resolve_structure_candidates(runs, evidence)?;

    let mut identities = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            (
                (
                    node.kind,
                    node.marker_range
                        .map_or(node.range.start, |range| range.start),
                ),
                index,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut generated_node_ids = HashSet::new();
    let mut paired_candidate_nodes = HashMap::<&str, usize>::new();
    let mut pending_parents = HashMap::<usize, Option<&str>>::new();
    let mut counters = HashMap::<NodeKind, usize>::new();
    for node in &nodes {
        *counters.entry(node.kind).or_default() += 1;
    }
    // A note whose label is a number of a count of paragraphs, most of whose numbers are no
    // note's, is that paragraph's number read as a note.
    let paragraph_numbers = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Numeric && run.rooted && run.consecutive)
        .filter_map(|run| {
            let labels = run
                .markers
                .iter()
                .filter(|marker| {
                    note_pairs.iter().any(|pair| {
                        marker.marker_range.start <= pair.label.range.start
                            && pair.label.range.end <= marker.marker_range.end
                    })
                })
                .map(|marker| marker.marker_range)
                .collect::<Vec<_>>();
            (4 * labels.len() <= run.markers.len()).then_some(labels)
        })
        .flatten()
        .collect::<Vec<_>>();
    let note_pairs = note_pairs
        .iter()
        .filter(|pair| {
            !paragraph_numbers.iter().any(|range| {
                range.start <= pair.label.range.start && pair.label.range.end <= range.end
            })
        })
        .collect::<Vec<_>>();
    let mut notes = Vec::with_capacity(note_pairs.len());
    let mut pair_ids = HashSet::new();
    for pair in note_pairs {
        if pair.pair_id.is_empty()
            || !pair_ids.insert(pair.pair_id.as_str())
            || !pair.label.range.valid(scalar_len)
            || pair.label.range.start == pair.label.range.end
            || pair.label.line_id.is_empty()
            || !pair.body.range.valid(scalar_len)
            || pair.body.range.start == pair.body.range.end
            || pair.body.page_indexes.is_empty()
            || pair.body.line_ids.is_empty()
            || pair.body.line_ids.iter().any(String::is_empty)
            || pair.references.is_empty()
            || pair.references.iter().any(|reference| {
                !reference.range.valid(scalar_len)
                    || reference.range.start == reference.range.end
                    || reference.line_id.is_empty()
            })
        {
            return Err(EngineError::invalid(format!(
                "note pair '{}' has incomplete source identity",
                pair.pair_id
            )));
        }
        let kind = match pair.kind {
            NoteKindV2::Footnote => NodeKind::Footnote,
            NoteKindV2::Endnote => NodeKind::Endnote,
        };
        let note_node_index = if let Some(&index) = identities.get(&(kind, pair.label.range.start))
        {
            index
        } else {
            let ordinal = counters.entry(kind).or_default();
            *ordinal += 1;
            let id = format!("heuristic-{}-{:06}", kind.name(), *ordinal);
            if !node_ids.insert(id.clone()) {
                return Err(EngineError::invalid(format!(
                    "generated node id '{}' collides with input",
                    id
                )));
            }
            let mut page_indexes = Vec::with_capacity(pair.body.page_indexes.len() + 1);
            page_indexes.push(pair.label.page_index);
            page_indexes.extend(pair.body.page_indexes.iter().copied());
            page_indexes.sort_unstable();
            page_indexes.dedup();
            let mut seen_lines = HashSet::new();
            let line_ids = std::iter::once(pair.label.line_id.as_str())
                .chain(pair.body.line_ids.iter().map(String::as_str))
                .filter(|line_id| seen_lines.insert(*line_id))
                .map(str::to_owned)
                .collect();
            let index = nodes.len();
            let mut node = StructureNode::new(
                id,
                kind,
                pair.body.range,
                ENGINE_ORIGIN,
                Derivation::Heuristic,
                None,
            );
            node.label = Some(
                scalar_text
                    .slice(pair.label.range.start..pair.label.range.end)
                    .expect("validated note label range")
                    .trim()
                    .to_owned(),
            );
            node.anchor = Some(pair.pair_id.clone());
            node.content_start = Some(pair.body.range.start);
            node.marker_range = Some(pair.label.range);
            node.page_indexes = page_indexes;
            node.line_ids = line_ids;
            node.grammar = Some("note_pair".to_owned());
            node.proof = Some(ResolutionProofV2 {
                rule: ResolutionRuleV2::PairedNote,
                observations: Vec::new(),
            });
            nodes.push(node);
            identities.insert((kind, pair.label.range.start), index);
            generated_node_ids.insert(index);
            index
        };
        for candidate in runs.iter().flat_map(|run| &run.markers) {
            if candidate.marker_range.start <= pair.label.range.start
                && pair.label.range.end <= candidate.marker_range.end
            {
                if paired_candidate_nodes
                    .insert(candidate.id.as_str(), note_node_index)
                    .is_some()
                {
                    return Err(EngineError::invalid(format!(
                        "candidate '{}' matches more than one note pair",
                        candidate.id
                    )));
                }
            }
        }
        let mut seen = HashSet::new();
        let references = pair
            .references
            .iter()
            .filter(|reference| seen.insert(reference.range))
            .map(|reference| NoteReference {
                range: reference.range,
                page_indexes: vec![reference.page_index],
                line_ids: vec![reference.line_id.clone()],
            })
            .collect::<Vec<_>>();
        notes.push(Note {
            id: pair.pair_id.clone(),
            node_id: nodes[note_node_index].id.clone(),
            kind: pair.kind,
            label_range: pair.label.range,
            body_range: pair.body.range,
            primary_reference: references.first().map(|reference| reference.range),
            references,
        });
    }

    let mut candidate_node_ids = paired_candidate_nodes;
    let mut resolved_candidates = resolved_candidates;
    for resolved in &mut resolved_candidates {
        if candidate_node_ids.contains_key(resolved.candidate.id.as_str()) {
            continue;
        }
        let Some(role) = resolved.role else { continue };
        let kind = role.node_kind();
        if let Some(&index) = identities.get(&(kind, resolved.candidate.marker_range.start)) {
            candidate_node_ids.insert(resolved.candidate.id.as_str(), index);
            pending_parents
                .entry(index)
                .or_insert(resolved.candidate.parent_candidate_id.as_deref());
            continue;
        }
        let ordinal = counters.entry(kind).or_default();
        *ordinal += 1;
        let id = format!("heuristic-{}-{:06}", kind.name(), *ordinal);
        if !node_ids.insert(id.clone()) {
            return Err(EngineError::invalid(format!(
                "generated node id '{}' collides with input",
                id
            )));
        }
        let index = nodes.len();
        identities.insert((kind, resolved.candidate.marker_range.start), index);
        candidate_node_ids.insert(resolved.candidate.id.as_str(), index);
        pending_parents.insert(index, resolved.candidate.parent_candidate_id.as_deref());
        generated_node_ids.insert(index);
        let label = match role {
            ResolvedRole::NumberedParagraph => {
                format!("par{}", resolved.candidate.grammar_value)
            }
            ResolvedRole::Section => resolved.candidate.grammar_value.clone(),
            ResolvedRole::ListItem => resolved.candidate.label.clone(),
        };
        let aliases =
            (resolved.candidate.label != label).then(|| vec![resolved.candidate.label.clone()]);
        let locator_kind = (role == ResolvedRole::Section).then(|| {
            let label = resolved.candidate.grammar_value.as_str();
            if label.starts_with("sec") {
                "section"
            } else if label.starts_with("art") {
                "article"
            } else if label.starts_with("part") {
                "part"
            } else if label.starts_with("sched") {
                "schedule"
            } else if label.starts_with("exh") {
                "exhibit"
            } else if label.starts_with("ann") {
                "annex"
            } else if label.starts_with("app") {
                "appendix"
            } else if resolved.candidate.level == 0 {
                "clause"
            } else {
                "subclause"
            }
            .to_owned()
        });
        let mut node = StructureNode::new(
            id,
            kind,
            resolved.candidate.range,
            ENGINE_ORIGIN,
            Derivation::Heuristic,
            None,
        );
        node.label = Some(label);
        node.locator_kind = locator_kind;
        node.aliases = aliases;
        node.content_start = Some(resolved.candidate.content_start);
        node.marker_range = Some(resolved.candidate.marker_range);
        node.page_indexes = resolved.page_indexes.to_vec();
        node.line_ids = resolved.line_ids.to_vec();
        node.grammar = Some(
            match role {
                ResolvedRole::NumberedParagraph => "numeric",
                ResolvedRole::Section => "hierarchy",
                ResolvedRole::ListItem => "enumerator",
            }
            .to_owned(),
        );
        node.proof = Some(ResolutionProofV2 {
            rule: resolved.proof.rule,
            observations: std::mem::take(&mut resolved.proof.observations),
        });
        nodes.push(node);
    }

    // A section that yielded to the numbered paragraph on its line stands for that paragraph,
    // so its sub-items are the paragraph's.
    let hierarchy = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Hierarchy)
        .flat_map(|run| run.markers.iter().map(|candidate| candidate.id.as_str()))
        .collect::<HashSet<_>>();
    for resolved in &resolved_candidates {
        let id = resolved.candidate.id.as_str();
        if resolved.role.is_none() && hierarchy.contains(id) && !candidate_node_ids.contains_key(id)
        {
            if let Some(&index) =
                identities.get(&(NodeKind::Paragraph, resolved.candidate.marker_range.start))
            {
                candidate_node_ids.insert(id, index);
            }
        }
    }
    let resolved_by_candidate = resolved_candidates
        .iter()
        .map(|resolved| (resolved.candidate.id.as_str(), resolved))
        .collect::<HashMap<_, _>>();
    // A list lies within one of the text's numbered paragraphs: a run's items on either side of
    // one are two lists.
    let mut paragraph_starts = resolved_candidates
        .iter()
        .filter(|resolved| resolved.role == Some(ResolvedRole::NumberedParagraph))
        .map(|resolved| resolved.candidate.range.start)
        .collect::<Vec<_>>();
    paragraph_starts.sort_unstable();
    let mut lists = Vec::<Vec<&ResolvedCandidate>>::new();
    for run in runs {
        let mut prior: Option<usize> = None;
        for candidate in &run.markers {
            let Some(resolved) = resolved_by_candidate
                .get(candidate.id.as_str())
                .copied()
                .filter(|resolved| resolved.role == Some(ResolvedRole::ListItem))
            else {
                continue;
            };
            let start = candidate.range.start;
            let parted = prior.is_none_or(|prior| {
                paragraph_starts
                    .get(paragraph_starts.partition_point(|at| *at <= prior))
                    .is_some_and(|at| *at <= start)
            });
            if parted {
                lists.push(Vec::new());
            }
            lists.last_mut().expect("a list was opened").push(resolved);
            prior = Some(start);
        }
    }
    for items in lists {
        if items.len() < 2 {
            continue;
        }
        let ordinal = counters.entry(NodeKind::List).or_default();
        *ordinal += 1;
        let id = format!("heuristic-list-{:06}", *ordinal);
        if !node_ids.insert(id.clone()) {
            return Err(EngineError::invalid(format!(
                "generated node id '{}' collides with input",
                id
            )));
        }
        let mut page_indexes = items
            .iter()
            .flat_map(|item| item.page_indexes.iter().copied())
            .collect::<Vec<_>>();
        page_indexes.sort_unstable();
        page_indexes.dedup();
        let mut seen_lines = HashSet::new();
        let line_ids = items
            .iter()
            .flat_map(|item| item.line_ids.iter().map(String::as_str))
            .filter(|line_id| seen_lines.insert(*line_id))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let index = nodes.len();
        let mut node = StructureNode::new(
            id.clone(),
            NodeKind::List,
            ScalarRange {
                start: items
                    .iter()
                    .map(|item| item.candidate.range.start)
                    .min()
                    .unwrap(),
                end: items
                    .iter()
                    .map(|item| item.candidate.range.end)
                    .max()
                    .unwrap(),
            },
            ENGINE_ORIGIN,
            Derivation::Heuristic,
            None,
        );
        node.page_indexes = page_indexes;
        node.line_ids = line_ids;
        node.grammar = Some("enumerator_hierarchy".to_owned());
        node.proof = Some(ResolutionProofV2 {
            rule: ResolutionRuleV2::ListItemLayout,
            observations: vec![CandidateObservationV2::ListItemLayout],
        });
        nodes.push(node);
        generated_node_ids.insert(index);
        for item in items
            .iter()
            .filter(|item| item.candidate.parent_candidate_id.is_none())
        {
            if let Some(&index) = candidate_node_ids.get(item.candidate.id.as_str()) {
                nodes[index].parent_id = Some(id.clone());
            }
        }
    }

    // Kinds and ranges stay as they are while parents are assigned: the nodes that may hold
    // another are ordered by where they open once.
    let sections = nodes_by_start(&nodes, &[NodeKind::Section]);
    for index in 0..nodes.len() {
        if nodes[index].kind != NodeKind::Heading || nodes[index].parent_id.is_some() {
            continue;
        }
        nodes[index].parent_id = smallest_container(&nodes, &sections, nodes[index].range, |_| true)
            .map(|section| nodes[section].id.clone());
    }
    let enclosures = nodes_by_start(
        &nodes,
        &[
            NodeKind::Page,
            NodeKind::Section,
            NodeKind::Paragraph,
            NodeKind::List,
        ],
    );

    for index in 0..nodes.len() {
        if (!generated_node_ids.contains(&index) && !pending_parents.contains_key(&index))
            || nodes[index].parent_id.is_some()
            || nodes[index].kind == NodeKind::Page
        {
            continue;
        }
        let candidate_parent = pending_parents
            .get(&index)
            .and_then(|candidate| *candidate)
            .and_then(|candidate| candidate_node_ids.get(candidate).copied())
            .filter(|parent| *parent != index)
            .map(|parent| nodes[parent].id.clone());
        let has_declared_parent = pending_parents.get(&index).is_some_and(Option::is_some);
        let enclosing = (candidate_parent.is_none()
            && !has_declared_parent
            && generated_node_ids.contains(&index))
        .then(|| {
            let range = nodes[index].range;
            // A list holds its items, never a paragraph its extent happens to run over.
            let holds = |candidate: usize| {
                candidate != index
                    && (nodes[candidate].range.start, nodes[candidate].range.end)
                        != (range.start, range.end)
                    && !(nodes[index].kind == NodeKind::Paragraph
                        && nodes[candidate].kind == NodeKind::List)
            };
            // The page a node sits on yields to a section the source itself supplies around it.
            smallest_container(&nodes, &enclosures, range, holds)
                .and_then(|best| {
                    if nodes[best].kind != NodeKind::Page {
                        return Some(best);
                    }
                    smallest_container(&nodes, &enclosures, range, |candidate| {
                        nodes[candidate].kind != NodeKind::Page
                            && !generated_node_ids.contains(&candidate)
                            && holds(candidate)
                    })
                    .or(Some(best))
                })
                .map(|candidate| nodes[candidate].id.clone())
        })
        .flatten();
        nodes[index].parent_id = candidate_parent.or(enclosing);
    }

    diagnostics.extend(runs.iter().map(|run| {
        let node_ids = run
            .markers
            .iter()
            .filter_map(|candidate| {
                candidate_node_ids
                    .get(candidate.id.as_str())
                    .map(|index| nodes[*index].id.clone())
            })
            .collect::<Vec<_>>();
        let resolved_count = node_ids.len();
        StructureDiagnostic {
            code: if resolved_count == 0 {
                "structure_run_abstained"
            } else if resolved_count == run.markers.len() {
                "structure_run_resolved"
            } else {
                "structure_run_partially_resolved"
            }
            .to_owned(),
            severity: DiagnosticSeverity::Info,
            ranges: vec![run.range],
            node_ids,
        }
    }));
    let mut origins = nodes
        .iter()
        .map(|node| node.origin_id.as_ref())
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|id| Origin { id: id.to_owned() })
        .collect::<Vec<_>>();
    origins.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(DocumentStructure::from_scalar_parts(
        document_id,
        provider,
        text.to_owned(),
        format!("{:x}", Sha256::digest(text.as_bytes())),
        source_sha256,
        Scope::complete(),
        origins,
        nodes,
        notes,
        diagnostics,
    ))
}
