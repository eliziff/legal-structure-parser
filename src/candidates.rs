use std::collections::{HashMap, HashSet};
use legal_structure_model::{
    CandidateEvidenceV2, CandidateGrammar, CandidateObservationV2, CandidateRoleV2, EngineError, ResolutionRuleV2,
    ScalarRange, StructureCandidateRun, StructureMarkerCandidate, UnitRole,
};
use legal_structure_model::text::{utf16_len, ScalarText};
use crate::inference;
use std::collections::BTreeMap;

#[cfg(feature = "structure-inference")]
pub fn detect_structure_candidate_runs(value: &str, refinements: bool, instrument: bool) -> Vec<StructureCandidateRun> {
    let text = ScalarText::new(value);
    let mut runs = inference::raw_numeric_runs(&text);
    let mut raw_enumerators = inference::raw_enumerator_runs(&text);
    raw_enumerators.extend(inference::raw_bullet_runs(&text));
    let (points, direct) = inference::detect_instrument_grammar(&text, refinements);
    let parent_indexes = points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            point.parent_id.as_ref().and_then(|parent| points[..index].iter().rposition(|candidate| candidate.id == *parent))
        })
        .collect::<Vec<_>>();
    // A section read from nothing but its number (no statute's run of sections, and no article, part or division), on
    // the line of a number the text counts from 1, is a numbered paragraph; its sub-items are then read as lists of
    // their own. In an instrument (`instrument`, the reader's measure of the document) such a count is its sections:
    // a by-law's or an agreement's "1.", "2.", "3.".
    let counted = runs
        .iter()
        .filter(|run| run.rooted && run.consecutive)
        .flat_map(|run| run.markers.iter().map(|marker| marker.marker_range.start))
        .collect::<HashSet<_>>();
    let paragraphs = (0..points.len())
        .filter(|index| {
            parent_indexes[*index].is_none()
                && direct
                && !instrument
                && !matches!(points[*index].locator_kind.as_deref(), Some("article" | "part" | "division"))
                && counted.contains(&points[*index].range.start)
        })
        .collect::<HashSet<_>>();
    let mut grouped = BTreeMap::<usize, (Vec<StructureMarkerCandidate>, usize, usize)>::new();
    for (index, point) in points.into_iter().enumerate() {
        let grammar_value = point.label.unwrap_or_default();
        let mut root = index;
        let mut level = 0;
        while let Some(parent) = parent_indexes[root] {
            root = parent;
            level += 1;
        }
        if paragraphs.contains(&root) {
            continue;
        }
        let content_start = point.content_start.expect("a grammar point opens its words");
        let entry = grouped.entry(root).or_insert_with(|| (Vec::new(), 0, 0));
        // A ladder is consecutive where none of its markers past the root breaks its count; an instrument's, where they
        // break it at most a third of the time (a statute's ladder, a few of its numbers misread or repealed). A judgment's
        // headings hung under a section it quoted pages before are no such ladder.
        if index != root {
            if matches!(
                point.properties.get("diagnostic").map(String::as_str),
                Some(
                    "instrument_ladder_forward_jump"
                        | "instrument_ladder_midcounter_open"
                        | "instrument_ladder_violation"
                )
            ) {
                entry.2 += 1;
            } else {
                entry.1 += 1;
            }
        }
        let printed = text
            .slice_utf16(point.range.start..content_start)
            .expect("grammar point range is bounded");
        let label = printed.trim().to_owned();
        entry.0.push(StructureMarkerCandidate {
            id: format!("grammar-point-{index:06}"),
            range: point.range,
            marker_range: ScalarRange {
                start: point.range.start,
                end: point.range.start + utf16_len(printed.trim_end()),
            },
            label,
            grammar_value,
            locator_kind: point.locator_kind.clone(),
            parent_candidate_id: parent_indexes[index]
                .map(|parent| format!("grammar-point-{parent:06}")),
            level,
            content_start,
        });
    }
    let mut hierarchy = grouped
        .into_values()
        .filter(|(markers, _, _)| !markers.is_empty())
        .map(|(markers, good, bad)| StructureCandidateRun {
            id: String::new(),
            grammar: CandidateGrammar::Hierarchy,
            range: ScalarRange {
                start: markers[0].range.start,
                end: markers.iter().map(|marker| marker.range.end).max().unwrap(),
            },
            rooted: markers[0].parent_candidate_id.is_none(),
            consecutive: bad == 0 || instrument && good >= bad * 2,
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

/// Whether a marker opens its count: 1, a, A, i, I.
fn opens_count(label: &str) -> bool {
    let bare = label.trim().trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | '.' | ':' | '-'));
    matches!(bare, "1" | "a" | "A" | "i" | "I")
}

/// Each run's numbered paragraphs and items take its family's kind. The candidate's own evidence
/// decides whether a marker numbers anything (exclusions, prose flow, composed readings); the
/// family decides what, once for the run. A count of numbers that opens inside another run's
/// numbered paragraph (it fits before that run's next marker, or within its last paragraph) is a
/// list: an order's 1. 2. 3. after "[45] ... I order:". A stray of three markers or fewer (a
/// counsel's initial, a capital heading, a wrapped citation's volume) encloses no longer count. A
/// parallel translation's count, which interleaves with the run, fits inside no one paragraph of
/// it. A provision ladder's markers
/// that number prose rather than sections take their own families' kinds: a decimal clause is a
/// numbered paragraph wherever it stands, and any other count under a parent marker opens inside
/// its parent's paragraph, so is a list.
fn family_kinds(runs: &[StructureCandidateRun], spans: &[std::ops::Range<usize>], resolved: &mut [ResolvedCandidate<'_>]) {
    let own = |item: &ResolvedCandidate<'_>| {
        !item.quotation && matches!(item.role.role, Some(UnitRole::Paragraph | UnitRole::Item))
    };
    let kinds: Vec<Option<UnitRole>> = runs
        .iter()
        .map(|run| run.markers.first().filter(|_| run.grammar != CandidateGrammar::Hierarchy).map(|marker| UnitRole::of_family(&marker.label)))
        .collect();
    // A list's marker its own evidence left unread ("- I was not", an item a scan set apart) is an
    // item all the same where others of its run are: the run, not the line, says it is a list.
    let unread = |item: &ResolvedCandidate<'_>| {
        item.role.role.is_none() && item.role.rule == ResolutionRuleV2::InsufficientEvidence && !item.quotation
    };
    let items: Vec<Vec<usize>> = spans
        .iter()
        .zip(&kinds)
        .map(|(span, kind)| {
            let listed = *kind == Some(UnitRole::Item) && span.clone().any(|i| own(&resolved[i]));
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
                || (candidate.level == 0 && UnitRole::of_family(&candidate.label) == UnitRole::Paragraph)
            {
                UnitRole::Paragraph
            } else {
                UnitRole::Item
            };
            if resolved[i].role.role != Some(kind) {
                resolved[i].role.role = Some(kind);
                resolved[i].role.rule = if kind == UnitRole::Paragraph {
                    ResolutionRuleV2::RootedNumericProse
                } else {
                    ResolutionRuleV2::ListItemLayout
                };
            }
        }
    }
    // Each paragraph run's markers and paragraph ends, in reading order.
    let outer: Vec<(usize, Vec<(usize, usize)>)> = (0..runs.len())
        .filter(|&k| kinds[k] == Some(UnitRole::Paragraph))
        .map(|k| (k, items[k].iter().map(|&i| (resolved[i].candidate.marker_range.start, resolved[i].candidate.range.end)).collect()))
        .collect();
    let mut decided: Vec<Option<UnitRole>> = kinds.clone();
    for (k, kind) in kinds.iter().enumerate() {
        let (Some(UnitRole::Paragraph), Some(&first), Some(&last)) = (*kind, items[k].first(), items[k].last()) else {
            continue;
        };
        if opens_count(&resolved[first].candidate.label) {
            let (opens, closes) = (resolved[first].candidate.marker_range.start, resolved[last].candidate.marker_range.start);
            // A stray capital or number (a counsel's initial, a wrapped citation's volume) holds none
            // of the document's paragraphs.
            let encloses = |marks: &Vec<(usize, usize)>| marks.len() > 3 || marks.len() >= items[k].len();
            let inside = outer.iter().filter(|(other, marks)| *other != k && encloses(marks)).any(|(_, marks)| {
                let at = marks.partition_point(|(start, _)| *start < opens);
                at > 0 && marks.get(at).map_or(opens < marks[at - 1].1, |next| next.0 > closes)
            });
            if inside {
                decided[k] = Some(UnitRole::Item);
            }
        }
    }
    for (k, kind) in decided.iter().enumerate() {
        let Some(kind) = *kind else { continue };
        for &i in &items[k] {
            let rule = if kind == UnitRole::Paragraph { ResolutionRuleV2::RootedNumericProse } else { ResolutionRuleV2::ListItemLayout };
            if resolved[i].role.role != Some(kind) {
                resolved[i].role.role = Some(kind);
                resolved[i].role.rule = rule;
            }
        }
    }
}

/// Each candidate's evidence by its id. Every candidate has an id of its own, and every item of
/// evidence names one candidate, once, with its pages and lines.
fn evidence_by_candidate<'a>(
    runs: &[StructureCandidateRun],
    evidence: &'a [CandidateEvidenceV2],
) -> Result<HashMap<&'a str, &'a CandidateEvidenceV2>, EngineError> {
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
    Ok(evidence_by_candidate)
}

/// A candidate's observations, each once, in the order its evidence gives them.
fn distinct_observations(item: Option<&CandidateEvidenceV2>) -> Vec<CandidateObservationV2> {
    let mut seen = HashSet::new();
    item.map_or_else(Vec::new, |item| {
        item.observations
            .iter()
            .copied()
            .filter(|observation| seen.insert(*observation))
            .collect()
    })
}

/// A candidate as it is resolved: its role, and whether it stands in a passage the document quotes.
struct ResolvedCandidate<'a> {
    candidate: &'a StructureMarkerCandidate,
    quotation: bool,
    role: CandidateRoleV2,
}

/// Each candidate of `runs` resolved, in text order, to the unit it opens, if any, and whether its
/// number counts (`counted_markers`).
pub fn resolve_candidates(
    runs: &[StructureCandidateRun],
    evidence: &[CandidateEvidenceV2],
    instrument: bool,
) -> Result<Vec<CandidateRoleV2>, EngineError> {
    let counted = counted_markers(runs);
    let mut provision_starts = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Hierarchy && run.rooted && run.consecutive)
        .flat_map(|run| run.markers.iter().map(|candidate| candidate.marker_range.start))
        .collect::<HashSet<_>>();
    let evidence_by_candidate = evidence_by_candidate(runs, evidence)?;
    let shows = |candidate: &StructureMarkerCandidate, observation: CandidateObservationV2| {
        evidence_by_candidate.get(candidate.id.as_str()).is_some_and(|item| item.observations.contains(&observation))
    };
    // A count from 1 is the text's numbered paragraphs, unless the text is an instrument (`instrument`, the reader's
    // measure of the document): a statute's, a by-law's or an agreement's count from 1 is its sections.
    let counts = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Numeric && run.rooted && run.consecutive && !instrument)
        .collect::<Vec<_>>();
    // Such a count of numbers printed with their period ("1.", "2.") is the text's paragraphs; their
    // sub-items are lists. A ladder under a Part's or an Exhibit's heading is read by its first
    // number a count reads, as the count prints it, where its numbers are paragraphs, not titles:
    // a factum's "PART III – ARGUMENT", "36." beside its prose (21 of 145 numbers set as a
    // heading). A schedule's entries under its Parts, most of them set as a provision's heading
    // ("PART 1 – DEPARTMENTS ...", "1. Department of Agriculture": 76 of 86), are the instrument's
    // provisions.
    let counted_labels = counts
        .iter()
        .flat_map(|run| run.markers.iter().map(|candidate| (candidate.marker_range.start, candidate.label.trim())))
        .collect::<HashMap<_, _>>();
    let period_counted = |run: &StructureCandidateRun| {
        let numbered = run.markers.iter().filter(|candidate| counted_labels.contains_key(&candidate.marker_range.start));
        run.grammar == CandidateGrammar::Hierarchy
            && numbered.clone().next().is_some_and(|first| {
                let label = counted_labels[&first.marker_range.start];
                label.ends_with('.')
                    && label[..label.len() - 1].chars().all(|c| c.is_ascii_digit())
                    && (std::ptr::eq(first, &run.markers[0])
                        || 2 * numbered.clone().filter(|candidate| shows(candidate, CandidateObservationV2::SectionHeading)).count()
                            <= numbered.clone().count())
            })
    };
    let paragraph_runs = runs
        .iter()
        .filter(|run| period_counted(run))
        .flat_map(|run| run.markers.iter().map(|candidate| candidate.marker_range.start))
        .collect::<HashSet<_>>();
    provision_starts.retain(|start| !paragraph_runs.contains(start));
    // A quotation's numbers are the quoted text's: they open none of this text's sections or
    // paragraphs, and decide nothing about them.
    let quoted = |candidate: &StructureMarkerCandidate| shows(candidate, CandidateObservationV2::Quotation);
    let quoted_starts = runs
        .iter()
        .flat_map(|run| &run.markers)
        .filter(|candidate| quoted(candidate))
        .map(|candidate| candidate.marker_range.start)
        .collect::<HashSet<_>>();
    provision_starts.retain(|start| !quoted_starts.contains(start));
    // Such a count a quarter or more of whose numbers open prose that no section reads, or that
    // opens on more such paragraphs than a list runs to, is the text's numbered paragraphs: the
    // sections read on its other numbers yield to it, each ladder they stand in whole, and their
    // sub-items are read as lists.
    let opens_prose = |candidate: &&StructureMarkerCandidate| {
        !provision_starts.contains(&candidate.marker_range.start)
            && !quoted(candidate)
            && shows(candidate, CandidateObservationV2::BodyProseFlow)
    };
    let mut yielded = HashSet::new();
    for run in &counts {
        let unread = |candidate: &&StructureMarkerCandidate| !provision_starts.contains(&candidate.marker_range.start);
        let read = run.markers.iter().filter(|candidate| !unread(candidate));
        let opening = run.markers.iter().take_while(unread).filter(opens_prose).count();
        if read.clone().next().is_some()
            && (4 * run.markers.iter().filter(opens_prose).count() >= run.markers.len() || opening > crate::LIST_NUMBERS)
        {
            yielded.extend(read.map(|candidate| candidate.marker_range.start));
        }
    }
    let ladders = runs
        .iter()
        .filter(|run| run.markers.iter().any(|candidate| yielded.contains(&candidate.marker_range.start)))
        .map(|run| run.markers[0].marker_range.start)
        .collect::<Vec<_>>();
    yielded.extend(ladders);
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
    let mut resolved = Vec::new();
    let mut spans = Vec::with_capacity(runs.len());
    for run in runs {
        let run_start = resolved.len();
        for candidate in &run.markers {
            let item = evidence_by_candidate.get(candidate.id.as_str()).copied();
            let observations = distinct_observations(item);
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
                    Some(UnitRole::Paragraph),
                    ResolutionRuleV2::RootedNumericProse,
                )
            // What an instrument's ladder reads under a section is the section's own (a subsection, a paragraph, a
            // clause: "8(1)(a)"), however its line is set.
            } else if instrument
                && !quotation
                && candidate.level > 0
                && run.grammar == CandidateGrammar::Hierarchy
                && run.rooted
                && run.consecutive
                && !yielded.contains(&run.markers[0].marker_range.start)
            {
                (
                    Some(UnitRole::Section),
                    ResolutionRuleV2::HierarchySection,
                )
            } else if seen(CandidateObservationV2::ComposedListItem) {
                (
                    Some(UnitRole::Item),
                    ResolutionRuleV2::ListItemLayout,
                )
            // A number a statute's sections claim is theirs, not a paragraph's.
            } else if run.grammar == CandidateGrammar::Numeric
                && (provision_starts.contains(&candidate.marker_range.start) || sectioned(run))
            {
                (None, ResolutionRuleV2::InsufficientEvidence)
            // A number inside a citation ("[1993] 1 S.C.R. 497" opening a line) numbers nothing, whatever it counts
            // on from: the citations of a passage can count on by one.
            } else if run.grammar == CandidateGrammar::Numeric && seen(CandidateObservationV2::CrossReference) {
                (None, ResolutionRuleV2::InsufficientEvidence)
            } else if run.grammar == CandidateGrammar::Numeric {
                (
                    Some(UnitRole::Paragraph),
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
                    Some(UnitRole::Section),
                    ResolutionRuleV2::HierarchySection,
                )
            } else if seen(CandidateObservationV2::ListItemSyntax) {
                (
                    Some(UnitRole::Item),
                    ResolutionRuleV2::ListItemSyntax,
                )
            } else {
                (
                    Some(UnitRole::Item),
                    ResolutionRuleV2::ListItemLayout,
                )
            };
            resolved.push(ResolvedCandidate {
                candidate,
                quotation,
                role: CandidateRoleV2 {
                    candidate_id: candidate.id.clone(),
                    role,
                    rule,
                    counted: counted.contains(&candidate.marker_range.start),
                },
            });
        }
        spans.push(run_start..resolved.len());
    }
    family_kinds(runs, &spans, &mut resolved);
    resolved.sort_by_key(|value| (value.candidate.range.start, value.candidate.range.end));
    Ok(resolved.into_iter().map(|resolved| resolved.role).collect())
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
        for (family, value, inserted) in labels.iter().flat_map(|label| marker_readings(label)) {
            if let Some(&(prior, at)) = last.get(&family) {
                if family.ends_with("bullet") || prior + 1 == value || inserted && prior == value {
                    counted.insert(at);
                    counted.insert(start);
                }
            } else if value == 1 {
                if let Some((head, prefix)) = family.split_once('@') {
                    if let Some(parent_head) = head.strip_suffix(".1") {
                        let (parent_prefix, parent_val) =
                            prefix.rsplit_once('.').map_or(("", prefix), |(p, v)| (p, v));
                        if let Ok(target) = parent_val.parse::<u32>() {
                            let parent_key = if parent_prefix.is_empty() {
                                parent_head.to_owned()
                            } else {
                                format!("{parent_head}@{parent_prefix}")
                            };
                            let alt_key = (parent_key == ":1").then_some(".:1");
                            if let Some(&(_, at)) = last
                                .get(&parent_key)
                                .or_else(|| alt_key.and_then(|k| last.get(k)))
                                .filter(|&&(prior, _)| prior == target)
                            {
                                counted.insert(at);
                                counted.insert(start);
                            }
                        }
                    }
                }
            }
            last.insert(family, (value, start));
        }
    }
    counted
}

/// A marker's readings as counts, (family, value, inserted): "(a)", "12.", "[12]", "iv)", "3.05"
/// (its count is 3's), "1st." (an ordinal), "ARTICLE 4" (a division's), "•". A letter that is also
/// a roman numeral ("i", "v") reads both ways.
fn marker_readings(label: &str) -> Vec<(String, u32, bool)> {
    let label = label.trim();
    if let Some((word, rest)) = label.split_once(char::is_whitespace) {
        if word.chars().all(char::is_alphabetic) {
            return marker_readings(rest)
                .into_iter()
                .map(|(family, value, inserted)| {
                    (format!("{}:{family}", word.to_lowercase()), value, inserted)
                })
                .collect();
        }
    }
    let core = label.trim_start_matches(['(', '[']).trim_end_matches([')', ']', '.']);
    let digits = core.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    if !digits.is_empty() && digits.len() < core.len() && digits.bytes().all(|b| b.is_ascii_digit())
        && matches!(&core[digits.len()..], "st" | "nd" | "rd" | "th" | "d")
    {
        return digits
            .parse()
            .map(|value| vec![("ordinal".to_owned(), value, false)])
            .unwrap_or_default();
    }
    let inserted = inference::inserted_letter_token(core.trim()).is_some();
    let mut found: Vec<(String, u32, bool)> = Vec::new();
    for letter in [true, false] {
        let Some((family, value)) = marker_family(label, |_: Family, _: u32, _: bool| letter) else { continue };
        let mut key = format!("{}:{}", family.wrap, family.count);
        if family.count.contains('.') {
            let parts: Vec<&str> = core.trim_start_matches('*').split('.').collect();
            key = format!("{key}@{}", parts[..parts.len().saturating_sub(1)].join("."));
        }
        if !found.iter().any(|(f, v, _)| *f == key && *v == value) {
            found.push((key, value, inserted));
        }
    }
    found
}

#[cfg(feature = "structure-inference")]
/// How a marker counts: its brackets, and whether it counts in digits, dotted digits, letters or
/// roman numerals of either case, or is a bullet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Family {
    wrap: &'static str,
    count: &'static str,
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
    // The enumerator grammar's readings (`inference::counts`); of a token that is a letter and a numeral, the numeral,
    // unless it is one letter `letter` says continues its letters. (A marker's numeral counts below 90.)
    let readings = inference::counts(inner);
    let read = |count: inference::Count, value: u32| {
        let count = match count {
            inference::Count::Decimal => "1",
            inference::Count::Dotted(2) => "1.1",
            inference::Count::Dotted(3) => "1.1.1",
            inference::Count::Dotted(_) => "1.1.1.1",
            inference::Count::Letter { upper } => if upper { "A" } else { "a" },
            inference::Count::Roman { upper } => if upper { "I" } else { "i" },
        };
        (Family { wrap, count }, value)
    };
    let letters = readings.iter().copied().find(|(count, _)| matches!(count, inference::Count::Letter { .. }));
    let numeral = readings.iter().copied().find(|(count, value)| matches!(count, inference::Count::Roman { .. }) && *value < 90);
    if let Some((count, value)) = numeral {
        if let Some((alpha, value)) = letters.filter(|_| inner.chars().count() == 1).map(|(count, value)| read(count, value)) {
            if letter(alpha, value, insertion.is_some()) {
                return Some((alpha, value));
            }
        }
        return Some(read(count, value));
    }
    readings.into_iter().find(|(count, _)| !matches!(count, inference::Count::Roman { .. })).map(|(count, value)| read(count, value))
}

pub fn numbering_marker(
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

