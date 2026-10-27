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
        let printed = text
            .slice(point.range.start..content_start)
            .expect("grammar point range is bounded");
        let label = printed.trim().to_owned();
        entry.0.push(StructureMarkerCandidate {
            id: format!("grammar-point-{index:06}"),
            range: point.range,
            marker_range: ScalarRange {
                start: point.range.start,
                end: point.range.start + printed.trim_end().chars().count(),
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
    // Hierarchy claims markers individually; the rest keep their complete count's evidence.
    for run in &mut raw_enumerators {
        run.markers
            .retain(|marker| !captured.contains(&marker.range.start));
    }
    raw_enumerators.retain(|run| !run.markers.is_empty());
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

#[cfg(feature = "structure-inference")]
fn own_membership(node: &StructureNode) -> &[ScalarRange] {
    if node.ranges.is_empty() {
        std::slice::from_ref(&node.range)
    } else {
        &node.ranges
    }
}

/// The shortest of `containers` (node indexes in the order they open) holding `range` that
/// `accept` takes, the first in node order among equals: the node a scan of every node finds.
/// A node opening further back is no shorter than the distance to `range`'s end, so the scan
/// back from `range` stops once that distance passes the shortest found.
#[cfg(feature = "structure-inference")]
fn smallest_container(
    nodes: &[StructureNode],
    containers: &[usize],
    membership: &[ScalarRange],
    accept: impl Fn(usize) -> bool,
) -> Option<usize> {
    let range = ScalarRange {
        start: membership.iter().map(|part| part.start).min()?,
        end: membership.iter().map(|part| part.end).max()?,
    };
    let opened = containers.partition_point(|index| nodes[*index].range.start <= range.start);
    let mut best: Option<(usize, usize)> = None;
    for &index in containers[..opened].iter().rev() {
        let candidate = nodes[index].range;
        if best.is_some_and(|(length, _)| range.end.saturating_sub(candidate.start) > length) {
            break;
        }
        let parts = own_membership(&nodes[index]);
        if range.end <= candidate.end
            && membership.iter().all(|part| {
                parts
                    .iter()
                    .any(|outer| outer.start <= part.start && part.end <= outer.end)
            })
            && accept(index)
        {
            let key = (candidate.end - candidate.start, index);
            if best.is_none_or(|best| key < best) {
                best = Some(key);
            }
        }
    }
    best.map(|(_, index)| index)
}

/// The kind a numbering's family fixes, as the structure gold's numbering rule does: numbers,
/// decimals ("1.01") and bare capitals ("A." recitals) are the document's own numbered
/// paragraphs; letters, roman numerals, bullets and a bracketed capital are a list's items.
fn family_kind(label: &str) -> ResolvedRole {
    let token = label.trim().trim_start_matches('*');
    let bare = token.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | '.' | ':' | '-'));
    let bracketed = token.starts_with(['(', '[']);
    let digits = bare.starts_with(|c: char| c.is_ascii_digit()) && bare.chars().all(|c| c.is_ascii_digit() || c == '.');
    let capital = !bracketed && bare.chars().count() == 1 && bare.chars().all(|c| c.is_ascii_uppercase());
    if digits || capital {
        ResolvedRole::NumberedParagraph
    } else {
        ResolvedRole::ListItem
    }
}

/// Whether a marker opens its count: 1, a, A, i, I.
fn opens_count(label: &str) -> bool {
    let bare = label.trim().trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | '.' | ':' | '-'));
    matches!(bare, "1" | "a" | "A" | "i" | "I")
}

/// Each run's numbered paragraphs and items take its family's kind. The candidate's own evidence
/// decides whether a marker numbers anything (exclusions, prose flow, composed readings); the
/// family decides what, once for the run. A count of numbers that opens inside another run's
/// numbered paragraph (it fits before that run's next marker, or within its last paragraph) is a
/// list: an order's 1. 2. 3. after "[45] ... I order:". A parallel translation's count, which
/// interleaves with the run, fits inside no one paragraph of it. A provision ladder's markers
/// that number prose rather than sections take their own families' kinds: a decimal clause is a
/// numbered paragraph wherever it stands, and any other count under a parent marker opens inside
/// its parent's paragraph, so is a list.
fn family_kinds(runs: &[StructureCandidateRun], spans: &[std::ops::Range<usize>], resolved: &mut [ResolvedCandidate<'_>]) {
    let own = |item: &ResolvedCandidate<'_>| {
        !item.proof.observations.contains(&CandidateObservationV2::Quotation)
            && matches!(item.role, Some(ResolvedRole::NumberedParagraph | ResolvedRole::ListItem))
    };
    let kinds: Vec<Option<ResolvedRole>> = runs
        .iter()
        .map(|run| run.markers.first().filter(|_| run.grammar != CandidateGrammar::Hierarchy).map(|marker| family_kind(&marker.label)))
        .collect();
    // A list's marker its own evidence left unread ("- I was not", an item a scan set apart) is an
    // item all the same where others of its run are: the run, not the line, says it is a list.
    let unread = |item: &ResolvedCandidate<'_>| {
        item.role.is_none()
            && item.proof.rule == ResolutionRuleV2::InsufficientEvidence
            && !item.proof.observations.contains(&CandidateObservationV2::Quotation)
    };
    let items: Vec<Vec<usize>> = spans
        .iter()
        .zip(&kinds)
        .map(|(span, kind)| {
            let listed = *kind == Some(ResolvedRole::ListItem) && span.clone().any(|i| own(&resolved[i]));
            span.clone().filter(|&i| own(&resolved[i]) || listed && unread(&resolved[i])).collect()
        })
        .collect();
    // "1.01", "3.2.4": digits with a point between them.
    let decimal = |label: &str| {
        let bare = label.trim().trim_start_matches('*').trim_end_matches(['.', ')']);
        bare.contains('.') && bare.split('.').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    };
    for (k, _) in runs.iter().enumerate().filter(|(_, run)| run.grammar == CandidateGrammar::Hierarchy) {
        for &i in &items[k] {
            let candidate = resolved[i].candidate;
            let kind = if decimal(&candidate.label)
                || (candidate.level == 0 && family_kind(&candidate.label) == ResolvedRole::NumberedParagraph)
            {
                ResolvedRole::NumberedParagraph
            } else {
                ResolvedRole::ListItem
            };
            if resolved[i].role != Some(kind) {
                resolved[i].role = Some(kind);
                resolved[i].proof.rule = if kind == ResolvedRole::NumberedParagraph {
                    ResolutionRuleV2::RootedNumericProse
                } else {
                    ResolutionRuleV2::ListItemLayout
                };
            }
        }
    }
    // Each paragraph run's markers and paragraph ends, in reading order.
    let outer: Vec<(usize, Vec<(usize, usize)>)> = (0..runs.len())
        .filter(|&k| kinds[k] == Some(ResolvedRole::NumberedParagraph))
        .map(|k| (k, items[k].iter().map(|&i| (resolved[i].candidate.marker_range.start, resolved[i].candidate.range.end)).collect()))
        .collect();
    for (k, kind) in kinds.iter().enumerate() {
        let (Some(mut kind), Some(&first), Some(&last)) = (*kind, items[k].first(), items[k].last()) else {
            continue;
        };
        if kind == ResolvedRole::NumberedParagraph && opens_count(&resolved[first].candidate.label) {
            let (opens, closes) = (resolved[first].candidate.marker_range.start, resolved[last].candidate.marker_range.start);
            let inside = outer.iter().filter(|(other, _)| *other != k).any(|(_, marks)| {
                let at = marks.partition_point(|(start, _)| *start < opens);
                at > 0 && marks.get(at).map_or(opens < marks[at - 1].1, |next| next.0 > closes)
            });
            if inside {
                kind = ResolvedRole::ListItem;
            }
        }
        let rule = if kind == ResolvedRole::NumberedParagraph {
            ResolutionRuleV2::RootedNumericProse
        } else {
            ResolutionRuleV2::ListItemLayout
        };
        for &i in &items[k] {
            if resolved[i].role != Some(kind) {
                resolved[i].role = Some(kind);
                resolved[i].proof.rule = rule;
            }
        }
    }
}

#[cfg(feature = "structure-inference")]
thread_local! {
    /// The document being resolved, for the candidate trace.
    static TRACE_DOCUMENT: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

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
    if std::env::var_os("LEGALPDF_CANDIDATE_TRACE").is_some() {
        // One line per raw marker before resolution: which run read it, and where it sits.
        let document = TRACE_DOCUMENT.with(|d| d.borrow().clone());
        for run in runs {
            for candidate in &run.markers {
                let item = evidence_by_candidate.get(candidate.id.as_str());
                let page = item.and_then(|i| i.page_indexes.first()).map_or(usize::MAX, |p| *p);
                let first = item.and_then(|i| i.line_ids.first()).map_or("", String::as_str);
                let label = candidate.label.replace(['\t', '\n'], " ");
                eprintln!(
                    "crun\t{document}\t{page}\t{first}\t{:?}\trooted={}\tconsecutive={}\t{}\t{label}",
                    run.grammar, run.rooted, run.consecutive, run.markers.len()
                );
            }
        }
    }
    // A count of numbers printed with their period ("1.", "2.") that the count from 1 reads is the
    // text's numbered paragraphs, not its sections: a statute prints its sections' numbers bare.
    // Their sub-items are then lists.
    let counted_starts = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Numeric && run.rooted && run.consecutive)
        .flat_map(|run| {
            run.markers
                .iter()
                .map(|candidate| candidate.marker_range.start)
        })
        .collect::<HashSet<_>>();
    let period_counted = |run: &StructureCandidateRun| {
        run.grammar == CandidateGrammar::Hierarchy
            && run.markers.first().is_some_and(|first| {
                let label = first.label.trim();
                label.ends_with('.')
                    && label[..label.len() - 1].chars().all(|c| c.is_ascii_digit())
                    && counted_starts.contains(&first.marker_range.start)
            })
    };
    let paragraph_runs = runs
        .iter()
        .filter(|run| period_counted(run))
        .flat_map(|run| {
            run.markers
                .iter()
                .map(|candidate| candidate.marker_range.start)
        })
        .collect::<HashSet<_>>();
    provision_starts.retain(|start| !paragraph_runs.contains(start));
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
    // Whether a candidate numbers anything, read over the whole document (the witnesses measured
    // against the structure gold, batch 7: 32,096 candidates). A number its count goes on from or
    // to is a marker: a paragraph's, a list's, a quoted text's (99.6% where prose follows it, 89%
    // with no other witness); one that counts nothing is a stray (an address, a year, a citation:
    // 23%). The page's furniture and a transcript's line numbers are never markers (0%), the
    // parser's composed reading of a paragraph nearly always is (99%), and a table's or a form's
    // cell is not without prose after it. The local witnesses that read lists (layout, syntax, a
    // composed list reading) add nothing to a count, so they decide only what a marker is.
    let counted = counted_markers(runs);
    let mut resolved = Vec::new();
    let mut spans = Vec::with_capacity(runs.len());
    for run in runs {
        let run_start = resolved.len();
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
            let seen = |observation: CandidateObservationV2| observations.contains(&observation);
            let vetoed = seen(CandidateObservationV2::Furniture) || seen(CandidateObservationV2::TranscriptLineNumber);
            let marker = !vetoed
                && (counted.contains(&candidate.marker_range.start) || seen(CandidateObservationV2::ComposedParagraph))
                && !(seen(CandidateObservationV2::TableOrForm) && !seen(CandidateObservationV2::BodyProseFlow));
            let quotation = seen(CandidateObservationV2::Quotation);
            // What a marker numbers; a numbered unit's kind here is provisional, the family's
            // (`family_kinds`) is final.
            let (role, rule) = if !marker {
                (None, if vetoed { ResolutionRuleV2::DirectExclusion } else { ResolutionRuleV2::InsufficientEvidence })
            // The parser's composed reading of the marker's line decides first where it has one.
            } else if seen(CandidateObservationV2::ComposedParagraph)
                && !quotation
                && run.grammar != CandidateGrammar::Enumerator
            {
                (
                    Some(ResolvedRole::NumberedParagraph),
                    ResolutionRuleV2::RootedNumericProse,
                )
            } else if seen(CandidateObservationV2::ComposedListItem) {
                (
                    Some(ResolvedRole::ListItem),
                    ResolutionRuleV2::ListItemLayout,
                )
            // A number a statute's sections claim is theirs, not a paragraph's.
            } else if run.grammar == CandidateGrammar::Numeric
                && (provision_starts.contains(&candidate.marker_range.start) || sectioned(run))
            {
                (None, ResolutionRuleV2::InsufficientEvidence)
            } else if run.grammar == CandidateGrammar::Numeric {
                (
                    Some(ResolvedRole::NumberedParagraph),
                    ResolutionRuleV2::RootedNumericProse,
                )
            } else if run.grammar == CandidateGrammar::Hierarchy
                && !quotation
                && !period_counted(run)
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
            } else if seen(CandidateObservationV2::ListItemSyntax) {
                (
                    Some(ResolvedRole::ListItem),
                    ResolutionRuleV2::ListItemSyntax,
                )
            } else {
                (
                    Some(ResolvedRole::ListItem),
                    ResolutionRuleV2::ListItemLayout,
                )
            };
            if std::env::var_os("LEGALPDF_CANDIDATE_TRACE").is_some() {
                // One line per candidate: where it is, what it became and why.
                let first = item.and_then(|i| i.line_ids.first()).map(String::as_str).unwrap_or("");
                let page = item.and_then(|i| i.page_indexes.first()).copied().unwrap_or(usize::MAX);
                let snippet: String = candidate.label.replace(['\t', '\n'], " ");
                let document = TRACE_DOCUMENT.with(|d| d.borrow().clone());
                eprintln!(
                    "ctrace\t{document}\t{page}\t{first}\t{:?}\t{:?}\t{:?}\trooted={}\tconsecutive={}\t{:?}\t{snippet}",
                    role, rule, run.grammar, run.rooted, run.consecutive, observations
                );
            }
            resolved.push(ResolvedCandidate {
                candidate,
                role,
                proof: ResolutionProofV2 { rule, observations },
                page_indexes: item.map_or(&[], |item| item.page_indexes.as_slice()),
                line_ids: item.map_or(&[], |item| item.line_ids.as_slice()),
            });
        }
        spans.push(run_start..resolved.len());
    }
    family_kinds(runs, &spans, &mut resolved);
    resolved.sort_by_key(|value| (value.candidate.range.start, value.candidate.range.end));
    Ok(resolved)
}

/// The marker positions whose number counts in the document: read in order, each one the next of
/// its count after the last of that count read before it, and that last one with it. A count is a
/// family (its wrapping, kind of numeral, a decimal's leading numbers, a division's word); bullets
/// of one glyph count each other.
fn counted_markers(runs: &[StructureCandidateRun]) -> HashSet<usize> {
    let mut positions: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
    for candidate in runs.iter().flat_map(|run| &run.markers) {
        let labels = positions.entry(candidate.marker_range.start).or_default();
        if !labels.contains(&candidate.label.as_str()) {
            labels.push(candidate.label.as_str());
        }
    }
    let mut last: HashMap<String, (u32, usize)> = HashMap::new();
    let mut counted = HashSet::new();
    for (&start, labels) in &positions {
        for (family, value) in labels.iter().flat_map(|label| marker_readings(label)) {
            if let Some(&(prior, at)) = last.get(&family) {
                if family.ends_with("bullet") || prior + 1 == value {
                    counted.insert(at);
                    counted.insert(start);
                }
            }
            last.insert(family, (value, start));
        }
    }
    counted
}

/// A marker's readings as counts, (family, value): "(a)", "12.", "[12]", "iv)", "3.05" (its count
/// is 3's), "1st." (an ordinal), "ARTICLE 4" (a division's), "•". A letter that is also a roman
/// numeral ("i", "v") reads both ways.
fn marker_readings(label: &str) -> Vec<(String, u32)> {
    let label = label.trim();
    if let Some((word, rest)) = label.split_once(char::is_whitespace) {
        if word.chars().all(char::is_alphabetic) {
            return marker_readings(rest).into_iter().map(|(family, value)| (format!("{}:{family}", word.to_lowercase()), value)).collect();
        }
    }
    let core = label.trim_start_matches(['(', '[']).trim_end_matches([')', ']', '.']);
    let digits = core.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    if !digits.is_empty() && digits.len() < core.len() && digits.bytes().all(|b| b.is_ascii_digit())
        && matches!(&core[digits.len()..], "st" | "nd" | "rd" | "th" | "d")
    {
        return digits.parse().map(|value| vec![("ordinal".to_owned(), value)]).unwrap_or_default();
    }
    let mut found: Vec<(String, u32)> = Vec::new();
    for letter in [true, false] {
        let Some((family, value)) = marker_family(label, |_: Family, _: u32, _: bool| letter) else { continue };
        let mut key = format!("{}:{}", family.wrap, family.count);
        if family.count.contains('.') {
            let parts: Vec<&str> = core.trim_start_matches('*').split('.').collect();
            key = format!("{key}@{}", parts[..parts.len().saturating_sub(1)].join("."));
        }
        if !found.iter().any(|(f, v)| *f == key && *v == value) {
            found.push((key, value));
        }
    }
    found
}

#[cfg(feature = "structure-inference")]
pub fn resolve_structure_graph_with_assembler(
    document_id: String,
    provider: String,
    text: &str,
    source_sha256: Option<String>,
    mut nodes: Vec<StructureNode>,
    runs: &[StructureCandidateRun],
    evidence: &[CandidateEvidenceV2],
    note_pairs: &[NotePairClaimV2],
    mut diagnostics: Vec<StructureDiagnostic>,
    assembler: &mut dyn FnMut(&mut [StructureNode]),
) -> Result<DocumentStructure, EngineError> {
    TRACE_DOCUMENT.with(|d| *d.borrow_mut() = source_sha256.clone().unwrap_or_else(|| document_id.clone()));
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
    let note_pairs = note_pairs
        .iter()
        .filter(|pair| pair.support.supports_note())
        .collect::<Vec<_>>();
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
                !range.valid(scalar_len)
                    || range.start == range.end
                    || range.start < pair.body.range.start
                    || range.end > pair.body.range.end
            })
            || (!pair.body.ranges.is_empty()
                && (pair.body.ranges.iter().map(|range| range.start).min()
                    != Some(pair.body.range.start)
                    || pair.body.ranges.iter().map(|range| range.end).max()
                        != Some(pair.body.range.end)))
            || pair.body.page_indexes.is_empty()
            || pair.body.line_ids.is_empty()
            || pair.body.line_ids.iter().any(String::is_empty)
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
            NoteKindV2::Sidenote | NoteKindV2::TableNote | NoteKindV2::AuthorNote => {
                let mut seen = HashSet::new();
                let references = pair.references.iter().filter(|reference| seen.insert(reference.range))
                    .map(|reference| NoteReference { range: reference.range, anchor: None,
                        page_indexes: vec![reference.page_index], line_ids: vec![reference.line_id.clone()] }).collect::<Vec<_>>();
                notes.push(Note { id: pair.pair_id.clone(), node_id: None, kind: pair.kind,
                    label_range: pair.label.range, body_range: pair.body.range, label_ranges: vec![pair.label.range],
                    body_ranges: if pair.body.ranges.is_empty() { vec![pair.body.range] } else { pair.body.ranges.clone() },
                    primary_reference: references.first().map(|reference| reference.range), references });
                continue;
            }
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
        // Keep the paired label and body parts, never the intervening text in their envelope.
        let mut membership = if pair.body.ranges.is_empty() {
            vec![pair.body.range]
        } else {
            pair.body.ranges.clone()
        };
        membership.push(pair.label.range);
        membership.sort_by_key(|range| (range.start, range.end));
        let node = &mut nodes[note_node_index];
        node.ranges.clear();
        for range in membership {
            if let Some(last) = node
                .ranges
                .last_mut()
                .filter(|last| range.start <= last.end)
            {
                last.end = last.end.max(range.end);
            } else {
                node.ranges.push(range);
            }
        }
        node.range = ScalarRange {
            start: node
                .ranges
                .first()
                .expect("validated note membership")
                .start,
            end: node.ranges.last().expect("validated note membership").end,
        };
        let upto = by_marker_start.partition_point(|&slot| markers[slot].marker_range.start <= pair.label.range.start);
        let from = marker_reach[..upto].partition_point(|end| *end < pair.label.range.end);
        let mut holding = by_marker_start[from..upto].iter().copied()
            .filter(|&slot| pair.label.range.end <= markers[slot].marker_range.end).collect::<Vec<_>>();
        holding.sort_unstable();
        for candidate in holding.into_iter().map(|slot| markers[slot]) {
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
        let mut seen = HashSet::new();
        let references = pair
            .references
            .iter()
            .filter(|reference| seen.insert(reference.range))
            .map(|reference| NoteReference {
                range: reference.range,
                anchor: None,
                page_indexes: vec![reference.page_index],
                line_ids: vec![reference.line_id.clone()],
            })
            .collect::<Vec<_>>();
        notes.push(Note {
            id: pair.pair_id.clone(),
            node_id: Some(nodes[note_node_index].id.clone()),
            kind: pair.kind,
            label_range: pair.label.range,
            body_range: pair.body.range,
            label_ranges: vec![pair.label.range],
            body_ranges: if pair.body.ranges.is_empty() { vec![pair.body.range] } else { pair.body.ranges.clone() },
            primary_reference: references.first().map(|reference| reference.range),
            references,
        });
    }

    let mut candidate_node_ids = paired_candidate_nodes;
    let mut resolved_candidates = resolved_candidates;
    // A note's label opens no provision: the items a hierarchy hangs from one are lists, not its
    // subsections.
    let note_rooted = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Hierarchy)
        .filter(|run| {
            run.markers.first().is_some_and(|root| {
                root.parent_candidate_id.is_none()
                    && notes.iter().any(|note| {
                        root.marker_range.start <= note.label_range.start
                            && note.label_range.end <= root.marker_range.end
                    })
            })
        })
        .flat_map(|run| run.markers.iter().map(|candidate| candidate.id.as_str()))
        .collect::<HashSet<_>>();
    let note_roots = runs
        .iter()
        .filter(|run| note_rooted.contains(run.markers[0].id.as_str()))
        .map(|run| run.markers[0].id.as_str())
        .collect::<HashSet<_>>();
    for resolved in &mut resolved_candidates {
        if resolved.role == Some(ResolvedRole::Section)
            && note_rooted.contains(resolved.candidate.id.as_str())
        {
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
    // Native PDF paragraph grouping is source layout, not a second structural boundary truth.
    // Expose grammar starts within those groups before attaching locators. Learned exact spans
    // already own their boundaries and are never split by this source-layout adjustment.
    let mut starts = resolved_candidates
        .iter()
        .filter(|candidate| candidate.role.is_some())
        .map(|candidate| candidate.candidate.marker_range.start)
        .collect::<Vec<_>>();
    starts.sort_unstable();
    starts.dedup();
    let mut fragments = Vec::new();
    for node in &mut nodes {
        if !matches!(node.kind, NodeKind::Prose | NodeKind::Heading)
            || !node.origin_id.starts_with("legalpdf.")
            || node.source == Derivation::Model
        {
            continue;
        }
        let within = starts.partition_point(|start| *start <= node.range.start)
            ..starts.partition_point(|start| *start < node.range.end);
        let mut cuts = starts[within]
            .iter()
            .copied()
            .filter(|start| {
                node.ranges.is_empty()
                    || node.ranges.iter().any(|part| part.start <= *start && *start < part.end)
            })
            .collect::<Vec<_>>();
        if cuts.is_empty() {
            continue;
        }
        cuts.insert(0, node.range.start);
        cuts.push(node.range.end);
        let original = node.clone();
        for (position, bounds) in cuts.windows(2).enumerate() {
            let mut fragment = original.clone();
            fragment.range = ScalarRange {
                start: bounds[0],
                end: bounds[1],
            };
            let membership = if original.ranges.is_empty() {
                std::slice::from_ref(&original.range)
            } else {
                &original.ranges
            };
            fragment.ranges = membership
                .iter()
                .filter_map(|part| {
                    let start = part.start.max(bounds[0]);
                    let end = part.end.min(bounds[1]);
                    (start < end).then_some(ScalarRange { start, end })
                })
                .collect();
            let (Some(first), Some(last)) = (fragment.ranges.first(), fragment.ranges.last())
            else {
                continue;
            };
            fragment.range = ScalarRange {
                start: first.start,
                end: last.end,
            };
            if fragment.kind == NodeKind::Heading {
                fragment.heading_range = (fragment.ranges.len() == 1).then_some(fragment.range);
                fragment.label = Some(
                    fragment
                        .ranges
                        .iter()
                        .filter_map(|part| scalar_text.slice(part.start..part.end))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            if position == 0 {
                *node = fragment;
            } else {
                fragment.id = format!("{}@{}", original.id, bounds[0]);
                if !node_ids.insert(fragment.id.clone()) {
                    return Err(EngineError::invalid("Split source unit ID collides"));
                }
                fragments.push(fragment);
            }
        }
    }
    nodes.extend(fragments);
    // Source units by where they open, so each candidate finds its own in one lookup.
    let mut units_at = HashMap::<usize, Vec<usize>>::new();
    for (index, node) in nodes.iter().enumerate() {
        units_at.entry(node.range.start).or_default().push(index);
    }
    let has_locator = |node: &StructureNode| {
        node.marker_range.is_some()
            || node.label.is_some()
            || node.locator_kind.is_some()
            || node
                .aliases
                .as_ref()
                .is_some_and(|aliases| !aliases.is_empty())
    };
    let is_pdf_unit = |node: &StructureNode| {
        node.origin_id.starts_with("legalpdf.")
            && (matches!(
                node.kind,
                NodeKind::Prose | NodeKind::Heading | NodeKind::Paragraph | NodeKind::ListItem
            ) || node.kind == NodeKind::Section && (has_locator(node) || node.role.is_some()))
    };
    for resolved in &mut resolved_candidates {
        if candidate_node_ids.contains_key(resolved.candidate.id.as_str()) {
            continue;
        }
        let Some(role) = resolved.role else { continue };
        let kind = role.node_kind();
        // A PDF source unit owns its role and exact membership; numbering adds locator facts
        // to that unit rather than manufacturing a second outline unit at the same start.
        let source_unit = units_at
            .get(&resolved.candidate.marker_range.start)
            .into_iter()
            .flatten()
            .copied()
            .find(|&index| {
                let node = &nodes[index];
                is_pdf_unit(node)
                    && node.range.start == resolved.candidate.marker_range.start
                    && resolved.candidate.marker_range.end <= node.range.end
                    && (node.ranges.is_empty()
                        || node.ranges.iter().any(|part| {
                            part.start <= resolved.candidate.marker_range.start
                                && resolved.candidate.marker_range.end <= part.end
                        }))
            });
        let claimed_source = source_unit.filter(|&index| {
            let node = &nodes[index];
            node.marker_range.is_some()
                || matches!(
                    node.kind,
                    NodeKind::Paragraph | NodeKind::Section | NodeKind::ListItem
                ) && (node.kind != kind || has_locator(node))
        });
        if let Some(index) = identities
            .get(&(kind, resolved.candidate.marker_range.start))
            .copied()
            .filter(|&index| {
                let node = &nodes[index];
                node.kind == kind && (!node.origin_id.starts_with("legalpdf.") || is_pdf_unit(node))
            })
        {
            if source_unit != Some(index) || claimed_source == Some(index) {
                candidate_node_ids.insert(resolved.candidate.id.as_str(), index);
                pending_parents
                    .entry(index)
                    .or_insert(resolved.candidate.parent_candidate_id.as_deref());
                continue;
            }
        }
        if let Some(index) = claimed_source {
            candidate_node_ids.insert(resolved.candidate.id.as_str(), index);
            if nodes[index].kind == kind {
                identities.insert((kind, resolved.candidate.marker_range.start), index);
                pending_parents
                    .entry(index)
                    .or_insert(resolved.candidate.parent_candidate_id.as_deref());
            }
            continue;
        }
        let index = source_unit.unwrap_or(nodes.len());
        let id = if let Some(index) = source_unit {
            nodes[index].id.clone()
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
            generated_node_ids.insert(index);
            id
        };
        if let Some(source) = source_unit {
            let old = (nodes[source].kind, nodes[source].range.start);
            if identities.get(&old) == Some(&source) {
                identities.remove(&old);
            }
        }
        identities.insert((kind, resolved.candidate.marker_range.start), index);
        candidate_node_ids.insert(resolved.candidate.id.as_str(), index);
        pending_parents.insert(index, resolved.candidate.parent_candidate_id.as_deref());
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
        let mut node = if let Some(index) = source_unit {
            let mut node = nodes[index].clone();
            if node.role.is_none() {
                node.role = Some(if node.kind == NodeKind::Heading {
                    PrimaryRole::Heading
                } else {
                    PrimaryRole::Text
                });
            }
            // A heading's range is its one membership part; a heading over several lines has
            // no single range, as composition derives it.
            if node.role == Some(PrimaryRole::Heading) && node.heading_range.is_none() && node.ranges.len() <= 1 {
                node.heading_range = Some(node.range);
            }
            node.kind = kind;
            node
        } else {
            StructureNode::new(
                id,
                kind,
                resolved.candidate.range,
                ENGINE_ORIGIN,
                Derivation::Heuristic,
                None,
            )
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
        node.proof = Some(ResolutionProofV2 {
            rule: resolved.proof.rule,
            observations: std::mem::take(&mut resolved.proof.observations),
        });
        if source_unit.is_some() {
            nodes[index] = node;
        } else {
            units_at.entry(node.range.start).or_default().push(nodes.len());
            nodes.push(node);
        }
    }

    // Aliases keep their source binding, but only the canonical node's kind can form lists or
    // paragraph boundaries. A paired note label supplies neither role.
    for resolved in &mut resolved_candidates {
        let Some(role) = resolved.role else { continue };
        let Some(&index) = candidate_node_ids.get(resolved.candidate.id.as_str()) else {
            continue;
        };
        if nodes[index].kind != role.node_kind() {
            resolved.role = None;
            resolved.proof.rule =
                if matches!(nodes[index].kind, NodeKind::Footnote | NodeKind::Endnote) {
                    ResolutionRuleV2::PairedNote
                } else {
                    ResolutionRuleV2::ConflictingRoles
                };
        }
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
        let mut membership = items
            .iter()
            .filter_map(|item| candidate_node_ids.get(item.candidate.id.as_str()))
            .flat_map(|&item| own_membership(&nodes[item]).iter().copied())
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
            node.range = ScalarRange {
                start: first.start,
                end: last.end,
            };
        }
        node.grammar = Some("enumerator_hierarchy".to_owned());
        node.proof = Some(ResolutionProofV2 {
            rule: ResolutionRuleV2::ListItemLayout,
            observations: vec![CandidateObservationV2::ListItemLayout],
        });
        nodes.push(node);
        generated_node_ids.insert(index);
        for item in items.iter().filter(|item| {
            item.candidate
                .parent_candidate_id
                .as_deref()
                .is_none_or(|parent| note_roots.contains(parent))
        }) {
            if let Some(&index) = candidate_node_ids.get(item.candidate.id.as_str()) {
                nodes[index].parent_id = Some(id.clone());
            }
        }
    }

    // A numbered paragraph holds the lists set after its words before the next paragraph or
    // heading.
    let mut paragraphs = (0..nodes.len())
        .filter(|index| {
            nodes[*index].kind == NodeKind::Paragraph && generated_node_ids.contains(index)
        })
        .collect::<Vec<_>>();
    paragraphs.sort_by_key(|index| nodes[*index].range.start);
    let mut lists = (0..nodes.len())
        .filter(|index| nodes[*index].kind == NodeKind::List && generated_node_ids.contains(index))
        .collect::<Vec<_>>();
    lists.sort_by_key(|index| nodes[*index].range.start);
    let heading_starts = nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Heading)
        .map(|node| node.range.start)
        .collect::<Vec<_>>();
    for list in lists {
        let range = nodes[list].range;
        let after = paragraphs.partition_point(|index| nodes[*index].range.start < range.start);
        let Some(&paragraph) = after.checked_sub(1).and_then(|slot| paragraphs.get(slot)) else {
            continue;
        };
        let held = nodes[paragraph].range;
        if held.end < range.end
            && paragraphs
                .get(after)
                .is_none_or(|next| nodes[*next].range.start >= range.end)
            && !heading_starts
                .iter()
                .any(|start| held.start < *start && *start < range.end)
        {
            // The list belongs to the paragraph; its text is not the paragraph's own text.
            // Literal assembly derives the resulting coverage through this parent edge.
            if nodes[list].parent_id.is_none() {
                nodes[list].parent_id = Some(nodes[paragraph].id.clone());
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
        nodes[index].parent_id =
            smallest_container(&nodes, &sections, own_membership(&nodes[index]), |_| true)
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
        // A declared parent that resolved to nothing leaves its item to whatever node holds it.
        let enclosing = (candidate_parent.is_none() && generated_node_ids.contains(&index))
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
                smallest_container(&nodes, &enclosures, own_membership(&nodes[index]), holds)
                    .and_then(|best| {
                        if nodes[best].kind != NodeKind::Page {
                            return Some(best);
                        }
                        // So does it to a paragraph read around it.
                        smallest_container(
                            &nodes,
                            &enclosures,
                            own_membership(&nodes[index]),
                            |candidate| {
                                nodes[candidate].kind != NodeKind::Page
                                    && (!generated_node_ids.contains(&candidate)
                                        || nodes[candidate].kind == NodeKind::Paragraph)
                                    && holds(candidate)
                            },
                        )
                        .or(Some(best))
                    })
                    .map(|candidate| nodes[candidate].id.clone())
            })
            .flatten();
        nodes[index].parent_id = candidate_parent.or(enclosing);
    }

    let candidate_ids = candidate_node_ids.iter().map(|(candidate, index)| (*candidate, nodes[*index].id.clone())).collect::<HashMap<_, _>>();
    join_broken_paragraphs(&mut nodes, &scalar_text);
    assembler(&mut nodes);
    for diagnostic in &mut diagnostics {
        diagnostic.node_ids.sort();
        diagnostic.node_ids.dedup();
    }

    diagnostics.extend(runs.iter().map(|run| {
        let node_ids = run
            .markers
            .iter()
            .filter_map(|candidate| {
                candidate_ids.get(candidate.id.as_str()).cloned()
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

/// A numbered paragraph the page's geometry broke in two (a page break, a wide gap, a line cut
/// mid-word) goes on in the prose unit that follows it in reading order, past the page's notes and
/// furniture, when the follower opens in lower case or the paragraph broke off on a comma, hyphen
/// or dash: one paragraph, as the text runs. The follower's own unit is gone, its children the
/// paragraph's.
fn join_broken_paragraphs(nodes: &mut Vec<StructureNode>, text: &ScalarText<'_>) {
    let flow = |node: &StructureNode| match node.kind {
        NodeKind::Paragraph | NodeKind::ListItem | NodeKind::Heading | NodeKind::Quote | NodeKind::Table => true,
        NodeKind::Prose => matches!(node.role, None | Some(PrimaryRole::Text | PrimaryRole::Caption | PrimaryRole::Heading)),
        _ => false,
    };
    let mut order = (0..nodes.len()).filter(|&index| flow(&nodes[index])).collect::<Vec<_>>();
    order.sort_by_key(|&index| (nodes[index].range.start, nodes[index].range.end));
    let mut absorbed = HashMap::<String, String>::new();
    let mut slot = 0;
    while slot < order.len() {
        let head = order[slot];
        let numbered = nodes[head].kind == NodeKind::Paragraph
            && nodes[head].marker_range.is_some()
            && nodes[head].grammar.as_deref() == Some("numeric");
        let mut next = slot + 1;
        while numbered && next < order.len() {
            let follower = &nodes[order[next]];
            let plain = follower.kind == NodeKind::Prose
                && follower.marker_range.is_none()
                && matches!(follower.role, None | Some(PrimaryRole::Text))
                && follower.origin_id.starts_with("legalpdf.")
                && !matches!(follower.grammar.as_deref(), Some("contents" | "translation"));
            if !plain {
                break;
            }
            let last = nodes[head].ranges.last().copied().unwrap_or(nodes[head].range);
            // Broken off: on a comma, a hyphen or a dash. A bare unpunctuated end is no witness: the
            // gold keeps such a paragraph apart from a capitalized follower as often as not.
            let broken = text.slice(last.start..last.end)
                .is_some_and(|words| words.trim_end().ends_with([',', '-', '\u{2013}', '\u{2014}', '(']));
            let lower = text.slice(follower.range.start..follower.range.end)
                .and_then(|words| words.trim_start().chars().next())
                .is_some_and(char::is_lowercase);
            if !broken && !lower {
                break;
            }
            let follower = nodes[order[next]].clone();
            let paragraph = &mut nodes[head];
            if paragraph.ranges.is_empty() {
                paragraph.ranges.push(paragraph.range);
            }
            paragraph.ranges.extend(if follower.ranges.is_empty() { vec![follower.range] } else { follower.ranges.clone() });
            paragraph.range.end = paragraph.range.end.max(follower.range.end);
            paragraph.line_ids.extend(follower.line_ids.iter().cloned());
            paragraph.page_indexes.extend(follower.page_indexes.iter().copied());
            paragraph.page_indexes.sort_unstable();
            paragraph.page_indexes.dedup();
            absorbed.insert(follower.id.clone(), paragraph.id.clone());
            next += 1;
        }
        slot = next.max(slot + 1);
    }
    if absorbed.is_empty() {
        return;
    }
    nodes.retain(|node| !absorbed.contains_key(&node.id));
    for node in nodes.iter_mut() {
        if let Some(parent) = node.parent_id.as_ref().and_then(|parent| absorbed.get(parent)) {
            node.parent_id = Some(parent.clone());
        }
    }
}

/// How a marker counts: its brackets, and whether it counts in digits, dotted digits, letters or
/// roman numerals of either case, or is a bullet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Family {
    wrap: &'static str,
    count: &'static str,
}

fn roman_value(token: &str) -> Option<u32> {
    legal_structure_model::layout::roman_value(token).filter(|value| *value < 90)
}

/// A marker's family and the value it counts, read whole: "(a)", "1.", "iv)", "[12]", "2.3", "•".
/// A single letter that is also a roman numeral reads as a letter when `letter` says the letters
/// of its brackets stand open at the letter before it.
fn marker_family(
    marker: &str,
    letter: impl Fn(Family, u32, bool) -> bool,
) -> Option<(Family, u32)> {
    let text = marker.trim();
    if text.chars().count() == 1 && (text == "o" || text.chars().all(|c| inference::BULLETS.contains(&c))) {
        return Some((
            Family {
                wrap: "•",
                count: "bullet",
            },
            1,
        ));
    }
    let (wrap, inner) =
        if let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            ("()", inner)
        } else if let Some(inner) = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            ("[]", inner)
        } else if let Some(inner) = text.strip_suffix(')') {
            (")", inner)
        } else if let Some(inner) = text.strip_suffix('.') {
            (".", inner)
        } else {
            ("", text)
        };
    let insertion = inference::inserted_letter_token(inner.trim());
    // An inserted letter retains its base counter's outline level; its label keeps the suffix.
    let inner = insertion.map_or(inner.trim(), |(letter, _)| letter);
    if inner.is_empty() || inner.chars().count() > 12 {
        return None;
    }
    if inner.chars().all(|c| c.is_ascii_digit()) {
        return Some((Family { wrap, count: "1" }, inner.parse().ok()?));
    }
    let parts = inner.split('.').collect::<Vec<_>>();
    if parts.len() > 1
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        let count = match parts.len() {
            2 => "1.1",
            3 => "1.1.1",
            _ => "1.1.1.1",
        };
        return Some((Family { wrap, count }, parts.last()?.parse().ok()?));
    }
    let single = inner.chars().count() == 1 && inner.chars().all(|c| c.is_ascii_alphabetic());
    let letter_value = |c: char| u32::from(c.to_ascii_lowercase()) - u32::from('a') + 1;
    if let Some(value) = roman_value(inner) {
        let upper = inner.chars().all(|c| c.is_ascii_uppercase());
        let lower = inner.chars().all(|c| c.is_ascii_lowercase());
        if upper || lower {
            if single {
                let c = inner.chars().next()?;
                let alpha = Family {
                    wrap,
                    count: if upper { "A" } else { "a" },
                };
                if letter(alpha, letter_value(c), insertion.is_some()) {
                    return Some((alpha, letter_value(c)));
                }
            }
            return Some((
                Family {
                    wrap,
                    count: if upper { "I" } else { "i" },
                },
                value,
            ));
        }
    }
    if single {
        let c = inner.chars().next()?;
        let count = if c.is_ascii_uppercase() { "A" } else { "a" };
        return Some((Family { wrap, count }, letter_value(c)));
    }
    None
}

pub(crate) fn numbering_marker(
    marker: &str,
    previous: Option<&legal_structure_model::NumberingMarker>,
) -> Option<legal_structure_model::NumberingMarker> {
    let (family, ordinal) = marker_family(marker, |family, ordinal, inserted| {
        previous.is_some_and(|previous| {
            previous.family == format!("{}:{}", family.wrap, family.count)
                && (previous.ordinal.checked_add(1) == Some(ordinal)
                    || inserted && previous.ordinal == ordinal)
        })
    })?;
    let mut components = marker
        .trim()
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default();
    components.pop();
    Some(legal_structure_model::NumberingMarker {
        family: format!("{}:{}", family.wrap, family.count),
        ordinal,
        prefix: components,
    })
}

