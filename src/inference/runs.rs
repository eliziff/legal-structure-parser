//! Raw numeric, bullet and enumerator runs, the candidates structure resolution starts from.

use super::*;

pub(super) fn next_boundary(boundaries: &[usize], start: usize, end: usize) -> usize {
    boundaries
        .get(boundaries.partition_point(|boundary| *boundary <= start))
        .copied()
        .unwrap_or(end)
}

pub(crate) fn raw_numeric_runs(text: &ScalarText<'_>) -> Vec<StructureCandidateRun> {
    let all = paragraph_markers(text, false, false);
    // Each style's numbering from 1 is read first, as the longest unbroken count: a number
    // quoted from another judgment, or a list's own count, is left out of it rather than
    // breaking it. What no count claims is grouped as before. A second count set within an
    // earlier one is a list quoted in its paragraphs or a parallel column's (a bilingual text's
    // other language), not paragraphs.
    let mut chains = Vec::new();
    let mut scopes = Vec::new();
    for style in [MarkerStyle::Bracket, MarkerStyle::Dot, MarkerStyle::Bare] {
        let mut rest = all
            .iter()
            .filter(|marker| marker.style == style)
            .cloned()
            .collect::<Vec<_>>();
        // Of two equal numbers, the count takes the one set as most of its style's are: alone
        // on its line or followed there by its text (a list quoted in a paragraph is the other).
        let alone = |marker: &Marker| {
            text.value[text.byte(marker.content_start)..]
                .chars()
                .next()
                .is_none_or(|next| matches!(next, '\n' | '\r'))
        };
        let usual = 2 * rest.iter().filter(|marker| alone(marker)).count() >= rest.len();
        for marker in &mut rest {
            if alone(marker) == usual {
                marker.score += 0.25;
            }
        }
        let first = chains.len();
        loop {
            let (mut chain, _) = rooted_chain(&rest);
            if chain.len() < 2 {
                break;
            }
            // A count resumes past a few numbers its printer skipped.
            while let Some(resumed) = chain.last().and_then(|last| {
                rest.iter()
                    .filter(|marker| {
                        marker.start > last.start
                            && (last.number + 2..=last.number + 8).contains(&marker.number)
                    })
                    .min_by_key(|marker| marker.start)
            }) {
                let mut tail = vec![resumed.clone()];
                for marker in rest.iter().filter(|marker| marker.start > resumed.start) {
                    if marker.number == tail.last().unwrap().number + 1 {
                        tail.push(marker.clone());
                    }
                }
                if tail.len() < 2 {
                    break;
                }
                chain.extend(tail);
            }
            let claimed = chain
                .iter()
                .map(|marker| marker.start)
                .collect::<HashSet<_>>();
            rest.retain(|marker| !claimed.contains(&marker.start));
            let within = chains[first..].iter().any(|prior: &Vec<Marker>| {
                let span = prior[0].start..=prior.last().unwrap().start;
                2 * chain
                    .iter()
                    .filter(|marker| span.contains(&marker.start))
                    .count()
                    >= chain.len()
            });
            if !within {
                chains.push(chain);
            }
        }
        scopes.extend(
            monotone_scopes(&rest, 8)
                .into_iter()
                .filter(|scope| scope.len() >= 2),
        );
    }
    // A lone year or number rejected by every sequence cannot terminate prose.
    let mut boundaries = chains
        .iter()
        .chain(&scopes)
        .flatten()
        .map(|marker| marker.start)
        .chain([text.len()])
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    // A count's numbers end only at its own next number, or past its last at another count's.
    let chain_boundaries = chains
        .iter()
        .map(|chain| {
            let span = chain[0].start..chain.last().unwrap().start;
            let mut ends = chains
                .iter()
                .flatten()
                .map(|marker| marker.start)
                .filter(|start| !span.contains(start))
                .chain(chain.iter().map(|marker| marker.start))
                .chain([text.len()])
                .collect::<Vec<_>>();
            ends.sort_unstable();
            ends.dedup();
            ends
        })
        .collect::<Vec<_>>();
    let mut runs = Vec::new();
    for (index, scope) in chains.into_iter().chain(scopes).enumerate() {
        let boundaries = chain_boundaries.get(index).unwrap_or(&boundaries);
        let candidates = scope
            .iter()
            .map(|marker| {
                let end = next_boundary(boundaries, marker.start, text.len());
                let printed = text
                    .slice(marker.start..marker.content_start)
                    .expect("numeric marker range is bounded");
                let surface_label = printed.trim().to_owned();
                StructureMarkerCandidate {
                    id: String::new(),
                    range: ScalarRange {
                        start: marker.start,
                        end,
                    },
                    marker_range: ScalarRange {
                        start: marker.start,
                        end: marker.start + printed.trim_end().chars().count(),
                    },
                    label: surface_label,
                    grammar_value: marker.number.to_string(),
                    parent_candidate_id: None,
                    level: 0,
                    content_start: marker.content_start,
                }
            })
            .collect::<Vec<_>>();
        let range = ScalarRange {
            start: candidates[0].range.start,
            end: candidates.last().unwrap().range.end,
        };
        runs.push(StructureCandidateRun {
            id: String::new(),
            grammar: CandidateGrammar::Numeric,
            range,
            rooted: scope[0].number == 1,
            // A count is unbroken but for the numbers its printer skipped.
            consecutive: index < chain_boundaries.len()
                || scope
                    .windows(2)
                    .all(|pair| pair[1].number == pair[0].number + 1),
            markers: candidates,
        });
    }
    runs.sort_by_key(|run| (run.range.start, run.range.end));
    for (run_index, run) in runs.iter_mut().enumerate() {
        run.id = format!("numeric-{:06}", run_index + 1);
        for (marker_index, marker) in run.markers.iter_mut().enumerate() {
            marker.id = format!("{}-{:04}", run.id, marker_index + 1);
        }
    }
    runs
}

/// Items opened by the same bullet at a line's start, one run for each bullet: a glyph and
/// its text, or the glyph alone on a line its text follows. Each item runs to the next item.
pub(crate) fn raw_bullet_runs(text: &ScalarText<'_>) -> Vec<StructureCandidateRun> {
    const BULLETS: &[char] = &[
        '\u{2022}', '\u{25cf}', '\u{25aa}', '\u{25e6}', '\u{2023}', '\u{2043}', '\u{25a0}',
        '\u{25a1}', '\u{f0b7}', '\u{f0a7}', '\u{f0d8}', '\u{2013}', '\u{2014}', '-', '*',
    ];
    let mut by_glyph = BTreeMap::<char, Vec<(usize, usize)>>::new();
    for line in lines(text) {
        let trimmed = line.text.trim_start_matches(instrument_space);
        let Some(glyph) = trimmed
            .chars()
            .next()
            .filter(|glyph| BULLETS.contains(glyph))
        else {
            continue;
        };
        let rest = &trimmed[glyph.len_utf8()..];
        let content = rest.trim_start_matches(instrument_space);
        let alone = content.is_empty() && line.byte_end < text.value.len();
        if !alone && (content.len() == rest.len() || content.starts_with(BULLETS)) {
            continue;
        }
        let at = line.byte_start + line.text.len() - trimmed.len();
        by_glyph.entry(glyph).or_default().push((
            text.scalar(at),
            text.scalar(at + trimmed.len() - content.len()),
        ));
    }
    let mut starts = by_glyph
        .values()
        .flatten()
        .map(|(start, _)| *start)
        .chain([text.len()])
        .collect::<Vec<_>>();
    starts.sort_unstable();
    let mut runs = Vec::new();
    for (glyph, items) in by_glyph.into_iter().filter(|(_, items)| items.len() >= 2) {
        let markers = items
            .iter()
            .map(|&(start, content_start)| StructureMarkerCandidate {
                id: String::new(),
                range: ScalarRange {
                    start,
                    end: next_boundary(&starts, start, text.len()),
                },
                marker_range: ScalarRange {
                    start,
                    end: start + 1,
                },
                label: glyph.to_string(),
                grammar_value: format!("bullet:{glyph}"),
                parent_candidate_id: None,
                level: 0,
                content_start,
            })
            .collect::<Vec<_>>();
        runs.push(StructureCandidateRun {
            id: String::new(),
            grammar: CandidateGrammar::Enumerator,
            range: ScalarRange {
                start: markers[0].range.start,
                end: markers.iter().map(|marker| marker.range.end).max().unwrap(),
            },
            rooted: true,
            consecutive: true,
            markers,
        });
    }
    runs
}

pub(crate) fn raw_enumerator_runs(text: &ScalarText<'_>) -> Vec<StructureCandidateRun> {
    struct RawEnumerator {
        value: String,
        ordinal: u32,
        start: usize,
        content_start: usize,
    }

    let mut by_family = BTreeMap::<u8, Vec<RawEnumerator>>::new();
    for line in lines(text) {
        let trimmed = line.text.trim_start_matches(instrument_space);
        let trimmed_byte = line.byte_start + line.text.len() - trimmed.len();
        let start = text.scalar(trimmed_byte);
        let Some((token, at)) = instrument_marker(trimmed, true, true) else {
            continue;
        };
        let content_start = text.scalar(trimmed_byte + at);
        for (family, value) in enum_readings(token).into_iter().flatten() {
            if family == 0 && value.contains('.') {
                continue;
            }
            let ordinal = value
                .split('.')
                .next()
                .and_then(|value| value.parse::<u32>().ok())
                .expect("enumerator readings have a numeric base");
            by_family.entry(family).or_default().push(RawEnumerator {
                value: if family == 0 {
                    ordinal.to_string()
                } else {
                    value
                },
                ordinal,
                start,
                content_start,
            });
        }
    }
    let mut boundaries = by_family
        .values()
        .flatten()
        .map(|marker| marker.start)
        .chain([text.len()])
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut runs = Vec::new();
    for (family, markers) in by_family {
        let mut scopes = Vec::<Vec<RawEnumerator>>::new();
        for marker in markers {
            let target = scopes
                .iter()
                .enumerate()
                .filter(|(_, scope)| {
                    scope.last().is_some_and(|prior| {
                        prior.ordinal <= marker.ordinal
                            && marker.ordinal - prior.ordinal <= 8
                            && compare_child_values(&marker.value, &prior.value).is_gt()
                    })
                })
                .max_by_key(|(_, scope)| scope.last().unwrap().ordinal)
                .map(|(index, _)| index);
            if marker.value == "1" || target.is_none() {
                scopes.push(vec![marker]);
            } else {
                scopes[target.unwrap()].push(marker);
            }
        }
        for scope in scopes.into_iter().filter(|scope| scope.len() >= 2) {
            let mut candidates = scope
                .iter()
                .map(|marker| {
                    let end = next_boundary(&boundaries, marker.start, text.len());
                    let printed = text
                        .slice(marker.start..marker.content_start)
                        .expect("enumerator marker range is bounded");
                    let surface_label = printed.trim().to_owned();
                    StructureMarkerCandidate {
                        id: String::new(),
                        range: ScalarRange {
                            start: marker.start,
                            end,
                        },
                        marker_range: ScalarRange {
                            start: marker.start,
                            end: marker.start + printed.trim_end().chars().count(),
                        },
                        label: surface_label,
                        grammar_value: format!("{family}:{}", marker.value),
                        parent_candidate_id: None,
                        level: 0,
                        content_start: marker.content_start,
                    }
                })
                .collect::<Vec<_>>();
            let range = ScalarRange {
                start: candidates[0].range.start,
                end: candidates.last().unwrap().range.end,
            };
            let ordinal = runs.len() + 1;
            for (index, candidate) in candidates.iter_mut().enumerate() {
                candidate.id = format!("enumerator-{ordinal:06}-{:04}", index + 1);
            }
            runs.push(StructureCandidateRun {
                id: format!("enumerator-{ordinal:06}"),
                grammar: CandidateGrammar::Enumerator,
                range,
                rooted: scope[0].value == "1",
                consecutive: scope
                    .windows(2)
                    .all(|pair| enum_increment(family, &pair[0].value, &pair[1].value)),
                markers: candidates,
            });
        }
    }
    runs.sort_by_key(|run| (run.range.start, run.range.end));
    runs
}
