//! The structure graph resolved from a producer's nodes, numbering candidates and note pairs. The
//! steps run in order over one `Resolution`, which holds the nodes and what binds them:
//!
//! 1. `validate`: every node, candidate and run names a valid range and identity.
//! 2. The candidates resolved to their roles (`resolve_structure_candidates`).
//! 3. `note_nodes`: each paired note a node of its own, the candidates its label holds bound to it.
//! 4. `note_rooted_lists`: a hierarchy hung from a note's label is a list, never provisions.
//! 5. `split_at_starts`: a PDF source unit parted where a resolved candidate opens inside it.
//! 6. `candidate_nodes`: each resolved candidate bound to the source unit it opens, or a node made.
//! 7. `settle_roles`: an alias whose node took another kind gives up its role; a section yielding
//!    to its line's paragraph stands for it.
//! 8. `list_nodes`: a run's list items gathered into lists, parted at numbered paragraphs.
//! 9. `lists_into_paragraphs`: a list set after a numbered paragraph's words is that paragraph's.
//! 10. `assign_parents`: headings under their sections, then every made or bound node under its
//!     declared parent or the smallest node that holds it.
//! 11. `finish`: broken paragraphs joined, the producer's outline assembled, each run's diagnostic.

use super::*;

/// The nodes being resolved and what binds them.
struct Resolution<'a> {
    nodes: Vec<StructureNode>,
    node_ids: HashSet<String>,
    /// Each node by its kind and where its marker (or range) starts.
    identities: HashMap<(NodeKind, usize), usize>,
    /// The nodes the resolution made, by index.
    generated: HashSet<usize>,
    /// Each candidate's node.
    candidate_nodes: HashMap<&'a str, usize>,
    /// A node's declared parent candidate, until parents are assigned.
    pending_parents: HashMap<usize, Option<&'a str>>,
    /// How many nodes of each kind there are, numbering the ones made.
    counters: HashMap<NodeKind, usize>,
}

impl<'a> Resolution<'a> {
    /// A made node's id: the next of its kind, which no input node may already hold.
    fn generated_id(&mut self, name: &str, kind: NodeKind) -> Result<String, EngineError> {
        let ordinal = self.counters.entry(kind).or_default();
        *ordinal += 1;
        let id = format!("heuristic-{}-{:06}", name, *ordinal);
        if !self.node_ids.insert(id.clone()) {
            return Err(EngineError::invalid(format!("generated node id '{}' collides with input", id)));
        }
        Ok(id)
    }
}

fn validate(nodes: &[StructureNode], runs: &[StructureCandidateRun], scalar_len: usize) -> Result<HashSet<String>, EngineError> {
    let mut node_ids = HashSet::new();
    for node in nodes {
        if node.id.is_empty() || !node_ids.insert(node.id.clone()) {
            return Err(EngineError::invalid(format!("node id '{}' is empty or duplicated", node.id)));
        }
        if !node.range.valid(scalar_len)
            || node.marker_range.is_some_and(|range| !range.valid(scalar_len))
            || node.line_ids.iter().any(String::is_empty)
        {
            return Err(EngineError::invalid(format!("node '{}' has invalid source identity", node.id)));
        }
    }
    for node in nodes {
        if node.parent_id.as_ref().is_some_and(|parent| parent == &node.id || !node_ids.contains(parent)) {
            return Err(EngineError::invalid(format!("node '{}' has an invalid parent", node.id)));
        }
    }
    let mut all_candidate_ids = HashSet::new();
    for run in runs {
        if run.id.is_empty() || !run.range.valid(scalar_len) {
            return Err(EngineError::invalid(format!("candidate run '{}' is invalid", run.id)));
        }
        let local_ids = run.markers.iter().map(|candidate| candidate.id.as_str()).collect::<HashSet<_>>();
        for candidate in &run.markers {
            let faults: Vec<&str> = [
                (!all_candidate_ids.insert(candidate.id.as_str()), "duplicate id"),
                (!candidate.range.valid(scalar_len), "range outside the text"),
                (!candidate.marker_range.valid(scalar_len), "marker range outside the text"),
                (candidate.marker_range.start < candidate.range.start || candidate.marker_range.end > candidate.range.end, "marker outside its range"),
                (candidate.content_start < candidate.marker_range.end || candidate.content_start > candidate.range.end, "content start outside marker..range end"),
                (candidate.range.start < run.range.start || candidate.range.end > run.range.end, "range outside its run"),
                (candidate.parent_candidate_id.as_deref() == Some(candidate.id.as_str()), "parent is itself"),
                (candidate.parent_candidate_id.as_deref().is_some_and(|parent| !local_ids.contains(parent)), "parent outside its run"),
            ].iter().filter(|(failed, _)| *failed).map(|(_, name)| *name).collect();
            if !faults.is_empty() {
                return Err(EngineError::invalid(format!(
                    "candidate '{}' ({}) has invalid ranges or parent identity: {} (range {}..{}, marker {}..{}, content {}, run {}..{}, parent {:?})",
                    candidate.id, candidate.label, faults.join(", "), candidate.range.start, candidate.range.end,
                    candidate.marker_range.start, candidate.marker_range.end, candidate.content_start,
                    run.range.start, run.range.end, candidate.parent_candidate_id
                )));
            }
        }
    }
    Ok(node_ids)
}

impl<'a> Resolution<'a> {
    /// Each paired note a node (the producer's at its label, else one made), its membership the
    /// label and body parts, the candidates its label holds bound to it. A sidenote, table note or
    /// author's note is a note with no node.
    fn note_nodes(&mut self, note_pairs: &[NotePairClaimV2], runs: &'a [StructureCandidateRun], scalar_text: &ScalarText<'_>) -> Result<Vec<Note>, EngineError> {
        let scalar_len = scalar_text.len();
        let note_pairs = note_pairs.iter().filter(|pair| pair.support.supports_note()).collect::<Vec<_>>();
        // The candidates by where their markers start, each with the furthest marker end so far, so a
        // note's label finds the markers holding it without reading every candidate.
        let markers = runs.iter().flat_map(|run| &run.markers).collect::<Vec<_>>();
        let mut by_marker_start = (0..markers.len()).collect::<Vec<_>>();
        by_marker_start.sort_by_key(|&slot| markers[slot].marker_range.start);
        let marker_reach = by_marker_start
            .iter()
            .scan(0, |end, &slot| {
                *end = (*end).max(markers[slot].marker_range.end);
                Some(*end)
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
                || pair.body.ranges.iter().any(|range| {
                    !range.valid(scalar_len) || range.start == range.end || range.start < pair.body.range.start || range.end > pair.body.range.end
                })
                || (!pair.body.ranges.is_empty()
                    && (pair.body.ranges.iter().map(|range| range.start).min() != Some(pair.body.range.start)
                        || pair.body.ranges.iter().map(|range| range.end).max() != Some(pair.body.range.end)))
                || pair.body.page_indexes.is_empty()
                || pair.body.line_ids.is_empty()
                || pair.body.line_ids.iter().any(String::is_empty)
                || pair.references.iter().any(|reference| {
                    !reference.range.valid(scalar_len) || reference.range.start == reference.range.end || reference.line_id.is_empty()
                })
            {
                return Err(EngineError::invalid(format!("note pair '{}' has incomplete source identity", pair.pair_id)));
            }
            let references = || {
                let mut seen = HashSet::new();
                pair.references
                    .iter()
                    .filter(|reference| seen.insert(reference.range))
                    .map(|reference| NoteReference {
                        range: reference.range,
                        anchor: None,
                        page_indexes: vec![reference.page_index],
                        line_ids: vec![reference.line_id.clone()],
                    })
                    .collect::<Vec<_>>()
            };
            let body_ranges = || if pair.body.ranges.is_empty() { vec![pair.body.range] } else { pair.body.ranges.clone() };
            let kind = match pair.kind {
                NoteKindV2::Footnote => NodeKind::Footnote,
                NoteKindV2::Endnote => NodeKind::Endnote,
                NoteKindV2::Sidenote | NoteKindV2::TableNote | NoteKindV2::AuthorNote => {
                    let references = references();
                    notes.push(Note { id: pair.pair_id.clone(), node_id: None, kind: pair.kind,
                        label_range: pair.label.range, body_range: pair.body.range, label_ranges: vec![pair.label.range],
                        body_ranges: body_ranges(), primary_reference: references.first().map(|reference| reference.range), references });
                    continue;
                }
            };
            let note_node_index = if let Some(&index) = self.identities.get(&(kind, pair.label.range.start)) {
                index
            } else {
                let id = self.generated_id(kind.name(), kind)?;
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
                let index = self.nodes.len();
                let mut node = StructureNode::new(id, kind, pair.body.range, ENGINE_ORIGIN, Derivation::Heuristic, None);
                node.label = Some(
                    scalar_text.slice(pair.label.range.start..pair.label.range.end).expect("validated note label range").trim().to_owned(),
                );
                node.anchor = Some(pair.pair_id.clone());
                node.content_start = Some(pair.body.range.start);
                node.marker_range = Some(pair.label.range);
                node.page_indexes = page_indexes;
                node.line_ids = line_ids;
                node.grammar = Some("note_pair".to_owned());
                node.proof = Some(ResolutionProofV2 { rule: ResolutionRuleV2::PairedNote, observations: Vec::new() });
                self.nodes.push(node);
                self.identities.insert((kind, pair.label.range.start), index);
                self.generated.insert(index);
                index
            };
            // Keep the paired label and body parts, never the intervening text in their envelope.
            let mut membership = body_ranges();
            membership.push(pair.label.range);
            membership.sort_by_key(|range| (range.start, range.end));
            let node = &mut self.nodes[note_node_index];
            node.ranges.clear();
            for range in membership {
                if let Some(last) = node.ranges.last_mut().filter(|last| range.start <= last.end) {
                    last.end = last.end.max(range.end);
                } else {
                    node.ranges.push(range);
                }
            }
            node.range = ScalarRange {
                start: node.ranges.first().expect("validated note membership").start,
                end: node.ranges.last().expect("validated note membership").end,
            };
            let upto = by_marker_start.partition_point(|&slot| markers[slot].marker_range.start <= pair.label.range.start);
            let from = marker_reach[..upto].partition_point(|end| *end < pair.label.range.end);
            let mut holding = by_marker_start[from..upto]
                .iter()
                .copied()
                .filter(|&slot| pair.label.range.end <= markers[slot].marker_range.end)
                .collect::<Vec<_>>();
            holding.sort_unstable();
            for candidate in holding.into_iter().map(|slot| markers[slot]) {
                if self.candidate_nodes.insert(candidate.id.as_str(), note_node_index).is_some() {
                    return Err(EngineError::invalid(format!("candidate '{}' matches more than one note pair", candidate.id)));
                }
            }
            let references = references();
            notes.push(Note {
                id: pair.pair_id.clone(),
                node_id: Some(self.nodes[note_node_index].id.clone()),
                kind: pair.kind,
                label_range: pair.label.range,
                body_range: pair.body.range,
                label_ranges: vec![pair.label.range],
                body_ranges: body_ranges(),
                primary_reference: references.first().map(|reference| reference.range),
                references,
            });
        }
        Ok(notes)
    }
}

/// A note's label opens no provision: the items a hierarchy hangs from one are lists, not its
/// subsections. Returns the roots of those hierarchies.
fn note_rooted_lists<'a>(runs: &'a [StructureCandidateRun], notes: &[Note], resolved_candidates: &mut [ResolvedCandidate<'a>]) -> HashSet<&'a str> {
    let note_rooted = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Hierarchy)
        .filter(|run| {
            run.markers.first().is_some_and(|root| {
                root.parent_candidate_id.is_none()
                    && notes.iter().any(|note| root.marker_range.start <= note.label_range.start && note.label_range.end <= root.marker_range.end)
            })
        })
        .flat_map(|run| run.markers.iter().map(|candidate| candidate.id.as_str()))
        .collect::<HashSet<_>>();
    let note_roots = runs
        .iter()
        .filter(|run| note_rooted.contains(run.markers[0].id.as_str()))
        .map(|run| run.markers[0].id.as_str())
        .collect::<HashSet<_>>();
    for resolved in resolved_candidates.iter_mut() {
        if resolved.role == Some(ResolvedRole::Section) && note_rooted.contains(resolved.candidate.id.as_str()) {
            let observations = &resolved.proof.observations;
            let rule = if observations.contains(&CandidateObservationV2::ListItemSyntax) {
                Some(ResolutionRuleV2::ListItemSyntax)
            } else if observations.contains(&CandidateObservationV2::ListItemLayout) {
                Some(ResolutionRuleV2::ListItemLayout)
            } else {
                None
            };
            resolved.role = rule.map(|_| ResolvedRole::ListItem);
            resolved.proof.rule = rule.unwrap_or(ResolutionRuleV2::InsufficientEvidence);
        }
    }
    note_roots
}

impl<'a> Resolution<'a> {
    /// Native PDF paragraph grouping is source layout, not a second structural boundary truth.
    /// Expose grammar starts within those groups before attaching locators. Learned exact spans
    /// already own their boundaries and are never split by this source-layout adjustment.
    fn split_at_starts(&mut self, resolved_candidates: &[ResolvedCandidate<'a>], scalar_text: &ScalarText<'_>) -> Result<(), EngineError> {
        let mut starts = resolved_candidates
            .iter()
            .filter(|candidate| candidate.role.is_some())
            .map(|candidate| candidate.candidate.marker_range.start)
            .collect::<Vec<_>>();
        starts.sort_unstable();
        starts.dedup();
        let mut fragments = Vec::new();
        for node in &mut self.nodes {
            if !matches!(node.kind, NodeKind::Prose | NodeKind::Heading)
                || !node.origin_id.starts_with("legalpdf.")
                || node.source == Derivation::Model
            {
                continue;
            }
            let within = starts.partition_point(|start| *start <= node.range.start)..starts.partition_point(|start| *start < node.range.end);
            let mut cuts = starts[within]
                .iter()
                .copied()
                .filter(|start| node.ranges.is_empty() || node.ranges.iter().any(|part| part.start <= *start && *start < part.end))
                .collect::<Vec<_>>();
            if cuts.is_empty() {
                continue;
            }
            cuts.insert(0, node.range.start);
            cuts.push(node.range.end);
            let original = node.clone();
            for (position, bounds) in cuts.windows(2).enumerate() {
                let mut fragment = original.clone();
                fragment.range = ScalarRange { start: bounds[0], end: bounds[1] };
                let membership = if original.ranges.is_empty() { std::slice::from_ref(&original.range) } else { &original.ranges };
                fragment.ranges = membership
                    .iter()
                    .filter_map(|part| {
                        let start = part.start.max(bounds[0]);
                        let end = part.end.min(bounds[1]);
                        (start < end).then_some(ScalarRange { start, end })
                    })
                    .collect();
                let (Some(first), Some(last)) = (fragment.ranges.first(), fragment.ranges.last()) else {
                    continue;
                };
                fragment.range = ScalarRange { start: first.start, end: last.end };
                if fragment.kind == NodeKind::Heading {
                    fragment.heading_range = (fragment.ranges.len() == 1).then_some(fragment.range);
                    fragment.label = Some(
                        fragment.ranges.iter().filter_map(|part| scalar_text.slice(part.start..part.end)).collect::<Vec<_>>().join(" "),
                    );
                }
                if position == 0 {
                    *node = fragment;
                } else {
                    fragment.id = format!("{}@{}", original.id, bounds[0]);
                    if !self.node_ids.insert(fragment.id.clone()) {
                        return Err(EngineError::invalid("Split source unit ID collides"));
                    }
                    fragments.push(fragment);
                }
            }
        }
        self.nodes.extend(fragments);
        Ok(())
    }

    /// Each resolved candidate bound to its node: the producer's node of its kind at its marker, a
    /// PDF source unit it opens (which owns its role and exact membership: numbering adds locator
    /// facts to it rather than a second unit at the same start), or a node made for it.
    fn candidate_nodes(&mut self, resolved_candidates: &mut [ResolvedCandidate<'a>]) -> Result<(), EngineError> {
        // Source units by where they open, so each candidate finds its own in one lookup.
        let mut units_at = HashMap::<usize, Vec<usize>>::new();
        for (index, node) in self.nodes.iter().enumerate() {
            units_at.entry(node.range.start).or_default().push(index);
        }
        let has_locator = |node: &StructureNode| {
            node.marker_range.is_some()
                || node.label.is_some()
                || node.locator_kind.is_some()
                || node.aliases.as_ref().is_some_and(|aliases| !aliases.is_empty())
        };
        let is_pdf_unit = |node: &StructureNode| {
            node.origin_id.starts_with("legalpdf.")
                && (matches!(node.kind, NodeKind::Prose | NodeKind::Heading | NodeKind::Paragraph | NodeKind::ListItem)
                    || node.kind == NodeKind::Section && (has_locator(node) || node.role.is_some()))
        };
        for resolved in resolved_candidates.iter_mut() {
            if self.candidate_nodes.contains_key(resolved.candidate.id.as_str()) {
                continue;
            }
            let Some(role) = resolved.role else { continue };
            let kind = role.node_kind();
            let nodes = &self.nodes;
            let source_unit = units_at.get(&resolved.candidate.marker_range.start).into_iter().flatten().copied().find(|&index| {
                let node = &nodes[index];
                is_pdf_unit(node)
                    && node.range.start == resolved.candidate.marker_range.start
                    && resolved.candidate.marker_range.end <= node.range.end
                    && (node.ranges.is_empty()
                        || node.ranges.iter().any(|part| {
                            part.start <= resolved.candidate.marker_range.start && resolved.candidate.marker_range.end <= part.end
                        }))
            });
            let claimed_source = source_unit.filter(|&index| {
                let node = &nodes[index];
                node.marker_range.is_some()
                    || matches!(node.kind, NodeKind::Paragraph | NodeKind::Section | NodeKind::ListItem) && (node.kind != kind || has_locator(node))
            });
            if let Some(index) = self.identities.get(&(kind, resolved.candidate.marker_range.start)).copied().filter(|&index| {
                let node = &nodes[index];
                node.kind == kind && (!node.origin_id.starts_with("legalpdf.") || is_pdf_unit(node))
            }) {
                if source_unit != Some(index) || claimed_source == Some(index) {
                    self.candidate_nodes.insert(resolved.candidate.id.as_str(), index);
                    self.pending_parents.entry(index).or_insert(resolved.candidate.parent_candidate_id.as_deref());
                    continue;
                }
            }
            if let Some(index) = claimed_source {
                self.candidate_nodes.insert(resolved.candidate.id.as_str(), index);
                if self.nodes[index].kind == kind {
                    self.identities.insert((kind, resolved.candidate.marker_range.start), index);
                    self.pending_parents.entry(index).or_insert(resolved.candidate.parent_candidate_id.as_deref());
                }
                continue;
            }
            let index = source_unit.unwrap_or(self.nodes.len());
            let id = if let Some(index) = source_unit {
                self.nodes[index].id.clone()
            } else {
                let id = self.generated_id(kind.name(), kind)?;
                self.generated.insert(index);
                id
            };
            if let Some(source) = source_unit {
                let old = (self.nodes[source].kind, self.nodes[source].range.start);
                if self.identities.get(&old) == Some(&source) {
                    self.identities.remove(&old);
                }
            }
            self.identities.insert((kind, resolved.candidate.marker_range.start), index);
            self.candidate_nodes.insert(resolved.candidate.id.as_str(), index);
            self.pending_parents.insert(index, resolved.candidate.parent_candidate_id.as_deref());
            let label = match role {
                ResolvedRole::NumberedParagraph => format!("par{}", resolved.candidate.grammar_value),
                ResolvedRole::Section => resolved.candidate.grammar_value.clone(),
                ResolvedRole::ListItem => resolved.candidate.label.clone(),
            };
            let aliases = (resolved.candidate.label != label).then(|| vec![resolved.candidate.label.clone()]);
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
            let mut node = if let Some(index) = source_unit {
                let mut node = self.nodes[index].clone();
                if node.role.is_none() {
                    node.role = Some(if node.kind == NodeKind::Heading { PrimaryRole::Heading } else { PrimaryRole::Text });
                }
                // A heading's range is its one membership part; a heading over several lines has
                // no single range, as composition derives it.
                if node.role == Some(PrimaryRole::Heading) && node.heading_range.is_none() && node.ranges.len() <= 1 {
                    node.heading_range = Some(node.range);
                }
                node.kind = kind;
                node
            } else {
                StructureNode::new(id, kind, resolved.candidate.range, ENGINE_ORIGIN, Derivation::Heuristic, None)
            };
            node.label = Some(label);
            node.locator_kind = locator_kind;
            node.aliases = aliases;
            node.content_start = Some(resolved.candidate.content_start.min(node.range.end));
            node.marker_range = Some(resolved.candidate.marker_range);
            if source_unit.is_none() {
                node.page_indexes = resolved.page_indexes.to_vec();
                node.line_ids = resolved.line_ids.to_vec();
            }
            // The provision reader still uses the source scope to select the primary text.
            if !matches!(node.grammar.as_deref(), Some("contents" | "translation")) {
                node.grammar = Some(
                    match role {
                        ResolvedRole::NumberedParagraph => "numeric",
                        ResolvedRole::Section => "hierarchy",
                        ResolvedRole::ListItem => "enumerator",
                    }
                    .to_owned(),
                );
            }
            node.proof = Some(ResolutionProofV2 { rule: resolved.proof.rule, observations: std::mem::take(&mut resolved.proof.observations) });
            if source_unit.is_some() {
                self.nodes[index] = node;
            } else {
                units_at.entry(node.range.start).or_default().push(self.nodes.len());
                self.nodes.push(node);
            }
        }
        Ok(())
    }

    /// Aliases keep their source binding, but only the canonical node's kind can form lists or
    /// paragraph boundaries; a paired note label supplies neither role. A section that yielded to
    /// the numbered paragraph on its line stands for that paragraph, so its sub-items are the
    /// paragraph's.
    fn settle_roles(&mut self, runs: &'a [StructureCandidateRun], resolved_candidates: &mut [ResolvedCandidate<'a>]) {
        for resolved in resolved_candidates.iter_mut() {
            let Some(role) = resolved.role else { continue };
            let Some(&index) = self.candidate_nodes.get(resolved.candidate.id.as_str()) else {
                continue;
            };
            if self.nodes[index].kind != role.node_kind() {
                resolved.role = None;
                resolved.proof.rule = if matches!(self.nodes[index].kind, NodeKind::Footnote | NodeKind::Endnote) {
                    ResolutionRuleV2::PairedNote
                } else {
                    ResolutionRuleV2::ConflictingRoles
                };
            }
        }
        let hierarchy = runs
            .iter()
            .filter(|run| run.grammar == CandidateGrammar::Hierarchy)
            .flat_map(|run| run.markers.iter().map(|candidate| candidate.id.as_str()))
            .collect::<HashSet<_>>();
        for resolved in resolved_candidates.iter() {
            let id = resolved.candidate.id.as_str();
            if resolved.role.is_none() && hierarchy.contains(id) && !self.candidate_nodes.contains_key(id) {
                if let Some(&index) = self.identities.get(&(NodeKind::Paragraph, resolved.candidate.marker_range.start)) {
                    self.candidate_nodes.insert(id, index);
                }
            }
        }
    }

    /// A run's list items gathered into lists: a list lies within one of the text's numbered
    /// paragraphs, so a run's items on either side of one are two lists. A list of one item is none.
    fn list_nodes(&mut self, runs: &'a [StructureCandidateRun], resolved_candidates: &[ResolvedCandidate<'a>], note_roots: &HashSet<&str>) -> Result<(), EngineError> {
        let resolved_by_candidate =
            resolved_candidates.iter().map(|resolved| (resolved.candidate.id.as_str(), resolved)).collect::<HashMap<_, _>>();
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
                    paragraph_starts.get(paragraph_starts.partition_point(|at| *at <= prior)).is_some_and(|at| *at <= start)
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
            let id = self.generated_id("list", NodeKind::List)?;
            let mut page_indexes = items.iter().flat_map(|item| item.page_indexes.iter().copied()).collect::<Vec<_>>();
            page_indexes.sort_unstable();
            page_indexes.dedup();
            let mut seen_lines = HashSet::new();
            let line_ids = items
                .iter()
                .flat_map(|item| item.line_ids.iter().map(String::as_str))
                .filter(|line_id| seen_lines.insert(*line_id))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let index = self.nodes.len();
            let mut node = StructureNode::new(
                id.clone(),
                NodeKind::List,
                ScalarRange {
                    start: items.iter().map(|item| item.candidate.range.start).min().unwrap(),
                    end: items.iter().map(|item| item.candidate.range.end).max().unwrap(),
                },
                ENGINE_ORIGIN,
                Derivation::Heuristic,
                None,
            );
            node.page_indexes = page_indexes;
            node.line_ids = line_ids;
            let mut membership = items
                .iter()
                .filter_map(|item| self.candidate_nodes.get(item.candidate.id.as_str()))
                .flat_map(|&item| own_membership(&self.nodes[item]).iter().copied())
                .collect::<Vec<_>>();
            membership.sort_by_key(|part| (part.start, part.end));
            for part in membership {
                if let Some(last) = node.ranges.last_mut().filter(|last| part.start <= last.end) {
                    last.end = last.end.max(part.end);
                } else {
                    node.ranges.push(part);
                }
            }
            if let (Some(first), Some(last)) = (node.ranges.first(), node.ranges.last()) {
                node.range = ScalarRange { start: first.start, end: last.end };
            }
            node.grammar = Some("enumerator_hierarchy".to_owned());
            node.proof = Some(ResolutionProofV2 {
                rule: ResolutionRuleV2::ListItemLayout,
                observations: vec![CandidateObservationV2::ListItemLayout],
            });
            self.nodes.push(node);
            self.generated.insert(index);
            for item in items.iter().filter(|item| item.candidate.parent_candidate_id.as_deref().is_none_or(|parent| note_roots.contains(parent))) {
                if let Some(&index) = self.candidate_nodes.get(item.candidate.id.as_str()) {
                    self.nodes[index].parent_id = Some(id.clone());
                }
            }
        }
        Ok(())
    }

    /// A numbered paragraph holds the lists set after its words before the next paragraph or
    /// heading. The list's text is not the paragraph's own; literal assembly derives the resulting
    /// coverage through this parent edge.
    fn lists_into_paragraphs(&mut self) {
        let nodes = &mut self.nodes;
        let mut paragraphs =
            (0..nodes.len()).filter(|index| nodes[*index].kind == NodeKind::Paragraph && self.generated.contains(index)).collect::<Vec<_>>();
        paragraphs.sort_by_key(|index| nodes[*index].range.start);
        let mut lists = (0..nodes.len()).filter(|index| nodes[*index].kind == NodeKind::List && self.generated.contains(index)).collect::<Vec<_>>();
        lists.sort_by_key(|index| nodes[*index].range.start);
        let heading_starts = nodes.iter().filter(|node| node.kind == NodeKind::Heading).map(|node| node.range.start).collect::<Vec<_>>();
        for list in lists {
            let range = nodes[list].range;
            let after = paragraphs.partition_point(|index| nodes[*index].range.start < range.start);
            let Some(&paragraph) = after.checked_sub(1).and_then(|slot| paragraphs.get(slot)) else {
                continue;
            };
            let held = nodes[paragraph].range;
            if held.end < range.end
                && paragraphs.get(after).is_none_or(|next| nodes[*next].range.start >= range.end)
                && !heading_starts.iter().any(|start| held.start < *start && *start < range.end)
                && nodes[list].parent_id.is_none()
            {
                nodes[list].parent_id = Some(nodes[paragraph].id.clone());
            }
        }
    }

    /// Headings under the smallest section that holds them; then every node the resolution made or
    /// bound under its declared parent's node, or else the smallest node that holds it (a page
    /// yielding to a section or paragraph around it). Kinds and ranges stay as they are meanwhile.
    fn assign_parents(&mut self) {
        let nodes = &mut self.nodes;
        let sections = nodes_by_start(nodes, &[NodeKind::Section]);
        for index in 0..nodes.len() {
            if nodes[index].kind != NodeKind::Heading || nodes[index].parent_id.is_some() {
                continue;
            }
            nodes[index].parent_id =
                smallest_container(nodes, &sections, own_membership(&nodes[index]), |_| true).map(|section| nodes[section].id.clone());
        }
        let enclosures = nodes_by_start(nodes, &[NodeKind::Page, NodeKind::Section, NodeKind::Paragraph, NodeKind::List]);
        for index in 0..nodes.len() {
            if (!self.generated.contains(&index) && !self.pending_parents.contains_key(&index))
                || nodes[index].parent_id.is_some()
                || nodes[index].kind == NodeKind::Page
            {
                continue;
            }
            let candidate_parent = self
                .pending_parents
                .get(&index)
                .and_then(|candidate| *candidate)
                .and_then(|candidate| self.candidate_nodes.get(candidate).copied())
                .filter(|parent| *parent != index)
                .map(|parent| nodes[parent].id.clone());
            // A declared parent that resolved to nothing leaves its item to whatever node holds it.
            let enclosing = (candidate_parent.is_none() && self.generated.contains(&index))
                .then(|| {
                    let range = nodes[index].range;
                    // A list holds its items, never a paragraph its extent happens to run over.
                    let holds = |candidate: usize| {
                        candidate != index
                            && (nodes[candidate].range.start, nodes[candidate].range.end) != (range.start, range.end)
                            && !(nodes[index].kind == NodeKind::Paragraph && nodes[candidate].kind == NodeKind::List)
                    };
                    // The page a node sits on yields to a section the source itself supplies around it.
                    smallest_container(nodes, &enclosures, own_membership(&nodes[index]), holds)
                        .and_then(|best| {
                            if nodes[best].kind != NodeKind::Page {
                                return Some(best);
                            }
                            // So does it to a paragraph read around it.
                            smallest_container(nodes, &enclosures, own_membership(&nodes[index]), |candidate| {
                                nodes[candidate].kind != NodeKind::Page
                                    && (!self.generated.contains(&candidate) || nodes[candidate].kind == NodeKind::Paragraph)
                                    && holds(candidate)
                            })
                            .or(Some(best))
                        })
                        .map(|candidate| nodes[candidate].id.clone())
                })
                .flatten();
            nodes[index].parent_id = candidate_parent.or(enclosing);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_structure_graph_with_assembler(
    document_id: String,
    provider: String,
    text: &str,
    source_sha256: Option<String>,
    nodes: Vec<StructureNode>,
    runs: &[StructureCandidateRun],
    evidence: &[CandidateEvidenceV2],
    note_pairs: &[NotePairClaimV2],
    mut diagnostics: Vec<StructureDiagnostic>,
    assembler: &mut dyn FnMut(&mut [StructureNode]),
) -> Result<DocumentStructure, EngineError> {
    TRACE_DOCUMENT.with(|d| *d.borrow_mut() = source_sha256.clone().unwrap_or_else(|| document_id.clone()));
    let scalar_text = ScalarText::new(text);
    let node_ids = validate(&nodes, runs, scalar_text.len())?;
    let mut resolved_candidates = resolve_structure_candidates(runs, evidence)?;
    let mut counters = HashMap::<NodeKind, usize>::new();
    for node in &nodes {
        *counters.entry(node.kind).or_default() += 1;
    }
    let identities = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| ((node.kind, node.marker_range.map_or(node.range.start, |range| range.start)), index))
        .collect::<HashMap<_, _>>();
    let mut resolution = Resolution {
        nodes,
        node_ids,
        identities,
        generated: HashSet::new(),
        candidate_nodes: HashMap::new(),
        pending_parents: HashMap::new(),
        counters,
    };
    let notes = resolution.note_nodes(note_pairs, runs, &scalar_text)?;
    let note_roots = note_rooted_lists(runs, &notes, &mut resolved_candidates);
    resolution.split_at_starts(&resolved_candidates, &scalar_text)?;
    resolution.candidate_nodes(&mut resolved_candidates)?;
    resolution.settle_roles(runs, &mut resolved_candidates);
    resolution.list_nodes(runs, &resolved_candidates, &note_roots)?;
    resolution.lists_into_paragraphs();
    resolution.assign_parents();

    let mut nodes = resolution.nodes;
    let candidate_ids = resolution
        .candidate_nodes
        .iter()
        .map(|(candidate, index)| (*candidate, nodes[*index].id.clone()))
        .collect::<HashMap<_, _>>();
    join_broken_paragraphs(&mut nodes, &scalar_text);
    assembler(&mut nodes);
    for diagnostic in &mut diagnostics {
        diagnostic.node_ids.sort();
        diagnostic.node_ids.dedup();
    }
    diagnostics.extend(runs.iter().map(|run| {
        let node_ids = run.markers.iter().filter_map(|candidate| candidate_ids.get(candidate.id.as_str()).cloned()).collect::<Vec<_>>();
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
