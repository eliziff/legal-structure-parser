use super::*;
use std::collections::BTreeMap;

#[cfg(feature = "structure-inference")]
pub fn detect_structure_candidate_runs(value: &str, refinements: bool) -> Vec<StructureCandidateRun> {
    let text = ScalarText::new(value);
    let mut runs = inference::raw_numeric_runs(&text);
    let mut raw_enumerators = inference::raw_enumerator_runs(&text);
    raw_enumerators.extend(inference::raw_bullet_runs(&text));
    let (points, direct) = inference::detect_instrument_grammar(&text, refinements);
    let parent_indexes = points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            point.parent_id.as_ref().and_then(|parent| {
                points[..index].iter().rposition(|candidate| {
                    candidate.label.as_ref() == Some(parent)
                        && candidate.range.start <= point.range.start
                        && point.range.start < candidate.range.end
                })
            })
        })
        .collect::<Vec<_>>();
    // A section read from nothing but its number (no statute's run of sections, and no article, part or division), on
    // the line of a number the text counts from 1, is a numbered paragraph; its sub-items are then read as lists of
    // their own.
    let counted = runs
        .iter()
        .filter(|run| run.rooted && run.consecutive)
        .flat_map(|run| run.markers.iter().map(|marker| marker.marker_range.start))
        .collect::<HashSet<_>>();
    let paragraphs = (0..points.len())
        .filter(|index| {
            parent_indexes[*index].is_none()
                && direct
                && !points[*index].label.as_deref().is_some_and(|label| ["art", "part", "div"].iter().any(|word| label.starts_with(word)))
                && counted.contains(&points[*index].range.start)
        })
        .collect::<HashSet<_>>();
    let mut grouped = BTreeMap::<usize, (Vec<StructureMarkerCandidate>, bool)>::new();
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
        let entry = grouped.entry(root).or_insert_with(|| (Vec::new(), true));
        entry.1 &= !matches!(
            point.properties.get("diagnostic").map(String::as_str),
            Some(
                "instrument_ladder_forward_jump"
                    | "instrument_ladder_midcounter_open"
                    | "instrument_ladder_violation"
            )
        );
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
pub(crate) fn resolve_candidates(
    runs: &[StructureCandidateRun],
    evidence: &[CandidateEvidenceV2],
) -> Result<Vec<CandidateRoleV2>, EngineError> {
    let counted = counted_markers(runs);
    let mut provision_starts = runs
        .iter()
        .filter(|run| run.grammar == CandidateGrammar::Hierarchy && run.rooted && run.consecutive)
        .flat_map(|run| {
            run.markers
                .iter()
                .map(|candidate| candidate.marker_range.start)
        })
        .collect::<HashSet<_>>();
    let evidence_by_candidate = evidence_by_candidate(runs, evidence)?;
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
#[cfg(feature = "structure-inference")]

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

