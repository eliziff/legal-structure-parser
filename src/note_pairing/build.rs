//! The pairs of one document: labels, references and repairs read into pairs.

use super::*;

pub(super) fn build_pairs(lines: &[PairLine<'_>]) -> Vec<Pair> {
    // Source page numbers remain physical; introductory symbols belong to this document's start.
    let first_page = lines.iter().map(|line| line.page).min().unwrap_or(1);
    let (refs, expected) = extract_refs(lines);
    let labels = extract_labels(&lines, &refs, first_page);
    let mut segments: Vec<Vec<Candidate>> = select_segments(&labels, &lines)
        .into_iter()
        .map(trim_unsupported_tail)
        .filter(|segment| !segment.is_empty())
        .collect();
    let mut used: HashSet<usize> = segments
        .iter()
        .flatten()
        .map(|candidate| candidate.line)
        .collect();
    for segment in &mut segments {
        let repaired = repair_gaps(std::mem::take(segment), &lines, &mut used);
        *segment = recover_out_of_order_labels(repaired, &labels, &mut used, &lines);
    }
    segments.retain(|segment| !segment.is_empty());
    let endnote = segments
        .first()
        .is_some_and(|segment| endnote_mode(segment, &lines));

    let mut pools = Vec::<Vec<Candidate>>::new();
    let mut expected_pools = Vec::<&[Candidate]>::new();
    let mut previous_boundary = None;
    for (segment_index, segment) in segments.iter().enumerate() {
        let last_page = segment
            .last()
            .map(|candidate| lines[candidate.line].page)
            .unwrap_or(0);
        let boundary = if segment_index + 1 == segments.len() {
            usize::MAX
        } else {
            lines
                .iter()
                .filter(|line| line.page <= last_page)
                .map(|line| line.idx)
                .max()
                .unwrap_or(0)
        };
        let label_pages: HashMap<u32, HashSet<u32>> =
            segment.iter().fold(HashMap::new(), |mut pages, candidate| {
                if let Some(value) = candidate.value {
                    pages
                        .entry(value)
                        .or_insert_with(HashSet::new)
                        .insert(lines[candidate.line].page);
                }
                pages
            });
        let pool = refs
            .iter()
            .filter(|candidate| {
                let position = lines[candidate.line].idx;
                previous_boundary.is_none_or(|prior| position > prior)
                    && position <= boundary
                    && (!candidate.flags.paren_ref
                        || candidate.value.is_some_and(|value| {
                            label_pages
                                .get(&value)
                                .is_some_and(|pages| pages.contains(&lines[candidate.line].page))
                        }))
            })
            .cloned()
            .collect();
        pools.push(pool);
        let from = previous_boundary.map_or(0, |prior| expected.partition_point(|site| lines[site.line].idx <= prior));
        let to = expected.partition_point(|site| lines[site.line].idx <= boundary);
        expected_pools.push(&expected[from..to.max(from)]);
        previous_boundary = Some(boundary);
    }

    // A segment whose labels stand for the most part in the note band: its notes stand, whatever
    // references were read for them.
    let banded = |segment: &[Candidate]| segment.iter().filter(|candidate| lines[candidate.line].in_note_band()).count() * 2 > segment.len();
    let mut chosen_by_segment = Vec::<HashMap<u32, Candidate>>::new();
    let mut repeated_by_segment = Vec::<HashMap<u32, Vec<Candidate>>>::new();
    let mut suppressed = HashSet::<usize>::new();
    for (segment_index, ((segment, pool), expected)) in segments.iter().zip(&pools).zip(&expected_pools).enumerate() {
        let mut chosen = select_refs(segment, pool, &lines, endnote);
        // The labels' references read as themselves in a third of the run at least: notes, not
        // numbered paragraphs, rows or fields that happen to find a number or two.
        let clean = chosen
            .values()
            .filter(|candidate| !candidate.repaired)
            .count();
        let apparatus = clean >= 3 && 3 * clean >= segment.len();
        repair_missing_refs(&mut chosen, segment, pool, expected, &lines, endnote);
        rekey_same_value_ref_runs(&mut chosen, segment, pool, &lines, endnote);
        repair_glyph_refs(&mut chosen, segment, &lines, endnote);
        if apparatus {
            repair_page_glyph_refs(&mut chosen, segment, pool, &lines, endnote);
        }

        let paren_count = segment
            .iter()
            .filter(|candidate| candidate.form == "paren")
            .count();
        if !segment.is_empty() && paren_count * 2 >= segment.len() && !banded(segment) {
            let label_pages: HashMap<u32, HashSet<u32>> =
                segment.iter().fold(HashMap::new(), |mut pages, candidate| {
                    if let Some(value) = candidate.value {
                        pages
                            .entry(value)
                            .or_insert_with(HashSet::new)
                            .insert(lines[candidate.line].page);
                    }
                    pages
                });
            let same_page = chosen.iter().any(|(value, candidate)| {
                label_pages
                    .get(value)
                    .is_some_and(|pages| pages.contains(&lines[candidate.line].page))
            });
            if !same_page {
                suppressed.insert(segment_index);
            }
        }

        if !suppressed.contains(&segment_index) && segment.len() >= 20 {
            let noteish = segment
                .iter()
                .filter(|candidate| {
                    lines[candidate.line].zone == Zone::Note
                        && !lines[candidate.line].region_witness_demoted
                })
                .count();
            let small = segment
                .iter()
                .filter(|candidate| lines[candidate.line].small_font)
                .count();
            let segment_pages: HashSet<u32> = segment
                .iter()
                .map(|candidate| lines[candidate.line].page)
                .collect();
            let article_pages: HashSet<u32> = lines.iter().map(|line| line.page).collect();
            let fired = chosen.len() <= (segment.len() / 50).max(1)
                && noteish * 5 < segment.len()
                && small * 5 < segment.len()
                && segment_pages.len() * 10 >= article_pages.len() * 6;
            if fired {
                suppressed.insert(segment_index);
            }
        }
        repeated_by_segment.push(repeated_refs(&chosen, pool, &lines));
        chosen_by_segment.push(chosen);
    }

    let mut ref_bearing_values = HashSet::<u32>::new();
    for (segment_index, (segment, chosen)) in segments.iter().zip(&chosen_by_segment).enumerate() {
        if !suppressed.contains(&segment_index)
            && !segment.is_empty()
            && chosen.len() * 2 >= segment.len()
        {
            ref_bearing_values.extend(segment.iter().filter_map(|candidate| candidate.value));
        }
    }
    for (segment_index, (segment, chosen)) in segments.iter().zip(&chosen_by_segment).enumerate() {
        if suppressed.contains(&segment_index) || segment.len() < 10 || !chosen.is_empty() || banded(segment) {
            continue;
        }
        let values: HashSet<u32> = segment
            .iter()
            .filter_map(|candidate| candidate.value)
            .collect();
        let segment_pages: HashSet<u32> = segment
            .iter()
            .map(|candidate| lines[candidate.line].page)
            .collect();
        let page_lines = lines
            .iter()
            .filter(|line| segment_pages.contains(&line.page))
            .count();
        let fired = !values.is_empty()
            && segment_pages.len() <= 2
            && values.is_subset(&ref_bearing_values)
            && segment.len() * 5 >= page_lines * 2;
        if fired {
            suppressed.insert(segment_index);
        }
    }

    for (segment_index, (segment, chosen)) in segments.iter().zip(&chosen_by_segment).enumerate() {
        if segment_index == 0 || suppressed.contains(&segment_index) || segment.len() < 6 {
            continue;
        }
        let values: HashSet<u32> = segment
            .iter()
            .filter_map(|candidate| candidate.value)
            .collect();
        let noteish = segment
            .iter()
            .filter(|candidate| {
                lines[candidate.line].zone == Zone::Note || lines[candidate.line].note_column_fit
            })
            .count();
        let fired = !values.is_empty()
            && values.is_subset(&ref_bearing_values)
            && chosen.len() * 3 <= segment.len()
            && (segment.len() - noteish) * 5 >= segment.len() * 3;
        if fired {
            suppressed.insert(segment_index);
        }
    }

    let mut pairs = Vec::new();
    for (segment_index, ((backbone, chosen), repeated)) in segments
        .iter()
        .zip(&chosen_by_segment)
        .zip(&repeated_by_segment)
        .enumerate()
    {
        if suppressed.contains(&segment_index) {
            continue;
        }
        for (position, label) in backbone.iter().enumerate() {
            let value = label.value.expect("numeric backbone");
            let primary = chosen.get(&value).cloned();
            if primary.is_none() && !label_only_supported(backbone, position, &lines) {
                continue;
            }
            let mut refs = primary.into_iter().collect::<Vec<_>>();
            if !refs.is_empty() {
                refs.extend(repeated.get(&value).into_iter().flatten().cloned());
            }
            pairs.push(Pair {
                label: label.clone(),
                refs,
                restart_sequence: segment_index + 1,
                // Count the whole run, including labels later omitted as unsupported.
                sequence_labels: backbone.len(),
                endnote,
                provenance: if label.repaired && label.repair_kind == "confusable_value_repair" {
                    "article_sequence_gap_glyph_repair"
                } else {
                    "article_sequence_line_start_label"
                }
                .to_owned(),
            });
        }
    }

    let mut used_symbols = HashSet::new();
    for label in labels
        .iter()
        .filter(|candidate| !candidate.symbol.is_empty())
    {
        if label.score < 0.4 {
            continue;
        }
        let mut options: Vec<&Candidate> = refs
            .iter()
            .filter(|reference| {
                reference.symbol == label.symbol
                    && !used_symbols.contains(&reference.pos(&lines))
                    && matches!(
                        i64::from(lines[label.line].page) - i64::from(lines[reference.line].page),
                        0 | 1
                    )
            })
            .collect();
        options.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.pos(&lines).cmp(&right.pos(&lines)))
        });
        let reference = options.first().copied().cloned();
        if reference.is_none()
            && !(lines[label.line].page - first_page < SYMBOL_START_PAGE_LIMIT
                && label.zone_is_noteish(&lines))
        {
            continue;
        }
        if let Some(reference) = &reference {
            used_symbols.insert(reference.pos(&lines));
        }
        pairs.push(Pair {
            label: label.clone(),
            sequence_labels: 1,
            refs: reference.into_iter().collect(),
            restart_sequence: 1,
            endnote: false,
            provenance: "article_start_custom_symbol_label".to_owned(),
        });
    }

    pairs.sort_by_key(|pair| pair.label.pos(lines));
    pairs
}
