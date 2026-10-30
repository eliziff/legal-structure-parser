//! The pairs of one document: labels, references and repairs read into pairs.

use super::*;

pub(super) fn build_pairs<'a>(
    pages: &'a [Page],
    protected_citation_spans: &'a BTreeMap<String, Vec<PdfSourceSpan>>,
    include_audit: bool,
) -> (Vec<Pair>, Vec<PairLine<'a>>, Option<Value>) {
    let lines = build_lines(pages, protected_citation_spans);
    // Source page numbers remain physical; introductory symbols belong to this document's start.
    let first_page = pages.iter().map(|page| page.number).min().unwrap_or(1);
    let (refs, expected) = extract_refs(&lines);
    let labels = extract_labels(&lines, &refs, first_page);
    let (segments, backbone_summary) = select_segments(&labels, &lines, include_audit);
    let mut segments: Vec<Vec<Candidate>> = segments
        .into_iter()
        .map(trim_unsupported_tail)
        .filter(|segment| !segment.is_empty())
        .collect();
    let mut used: HashSet<usize> = segments
        .iter()
        .flatten()
        .map(|candidate| candidate.line)
        .collect();
    let mut sequence_holes = include_audit.then(Vec::<Value>::new);
    for segment in &mut segments {
        let (repaired, holes) =
            repair_gaps(std::mem::take(segment), &lines, &mut used, include_audit);
        if let (Some(sequence_holes), Some(holes)) = (&mut sequence_holes, holes) {
            sequence_holes.extend(holes);
        }
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
    let mut suppressed_reasons = HashMap::<usize, &'static str>::new();
    let mut numbered_diagnostics = include_audit.then(Vec::<Value>::new);
    let mut ref_drop_reasons = BTreeMap::<String, usize>::new();
    let mut ref_repair_counts = BTreeMap::<String, usize>::new();
    for (segment_index, ((segment, pool), expected)) in segments.iter().zip(&pools).zip(&expected_pools).enumerate() {
        let (mut chosen, drops) = select_refs(segment, pool, &lines, endnote);
        for (reason, count) in drops {
            *ref_drop_reasons.entry(reason).or_default() += count;
        }
        // The labels' references read as themselves in a third of the run at least: notes, not
        // numbered paragraphs, rows or fields that happen to find a number or two.
        let clean = chosen
            .values()
            .filter(|candidate| !candidate.repaired)
            .count();
        let apparatus = clean >= 3 && 3 * clean >= segment.len();
        for (kind, count) in repair_missing_refs(&mut chosen, segment, pool, expected, &lines, endnote) {
                *ref_repair_counts.entry(kind).or_default() += count;
            }
        for (kind, count) in rekey_same_value_ref_runs(&mut chosen, segment, pool, &lines, endnote) {
                *ref_repair_counts.entry(kind).or_default() += count;
            }
        let glyphs = repair_glyph_refs(&mut chosen, segment, &lines, endnote);
        if glyphs > 0 {
            *ref_repair_counts
                .entry("glyph_sequence_repair".to_owned())
                .or_default() += glyphs;
        }
        if apparatus {
            let glyphs = repair_page_glyph_refs(&mut chosen, segment, pool, &lines, endnote);
            if glyphs > 0 {
                *ref_repair_counts
                    .entry("page_glyph_repair".to_owned())
                    .or_default() += glyphs;
            }
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
                suppressed_reasons.insert(segment_index, "paren_list_segment");
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
            if let Some(diagnostics) = &mut numbered_diagnostics {
                diagnostics.push(json!({
                    "segment_index": segment_index,
                    "labels": segment.len(),
                    "chosen_refs": chosen.len(),
                    "noteish_labels": noteish,
                    "small_font_labels": small,
                    "segment_pages": segment_pages.len(),
                    "article_pages": article_pages.len(),
                    "suppressed": fired,
                }));
            }
            if fired {
                suppressed.insert(segment_index);
                suppressed_reasons.insert(segment_index, "numbered_paragraph_segment");
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
    let mut duplicate_diagnostics = include_audit.then(Vec::<Value>::new);
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
        if let Some(diagnostics) = &mut duplicate_diagnostics {
            diagnostics.push(json!({
                "segment_index": segment_index,
                "labels": segment.len(),
                "segment_pages": segment_pages.len(),
                "page_lines": page_lines,
                "values_covered": values.intersection(&ref_bearing_values).count(),
                "values_total": values.len(),
                "suppressed": fired,
            }));
        }
        if fired {
            suppressed.insert(segment_index);
            suppressed_reasons.insert(segment_index, "duplicate_valueset_zero_ref_segment");
        }
    }

    let mut body_restart_diagnostics = include_audit.then(Vec::<Value>::new);
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
        if let Some(diagnostics) = &mut body_restart_diagnostics {
            diagnostics.push(json!({
                "segment_index": segment_index,
                "labels": segment.len(),
                "chosen_refs": chosen.len(),
                "noteish_labels": noteish,
                "values_covered": values.intersection(&ref_bearing_values).count(),
                "values_total": values.len(),
                "suppressed": fired,
            }));
        }
        if fired {
            suppressed.insert(segment_index);
            suppressed_reasons.insert(segment_index, "body_zone_restart_segment");
        }
    }

    let mut pairs = Vec::new();
    let mut pair_number = 0;
    let mut paired_count = 0;
    let mut skipped = HashMap::<&'static str, usize>::new();
    for (segment_index, ((backbone, chosen), repeated)) in segments
        .iter()
        .zip(&chosen_by_segment)
        .zip(&repeated_by_segment)
        .enumerate()
    {
        if suppressed.contains(&segment_index) {
            *skipped
                .entry(
                    suppressed_reasons
                        .get(&segment_index)
                        .copied()
                        .unwrap_or("paren_list_segment"),
                )
                .or_default() += backbone.len();
            continue;
        }
        for (position, label) in backbone.iter().enumerate() {
            let value = label.value.expect("numeric backbone");
            let primary = chosen.get(&value).cloned();
            if primary.is_none() && !label_only_supported(backbone, position, &lines) {
                *skipped.entry("unsupported_label_only").or_default() += 1;
                continue;
            }
            pair_number += 1;
            let primary_ref = primary
                .as_ref()
                .map(|reference| (reference.line, reference.start, reference.end));
            let mut refs = primary.into_iter().collect::<Vec<_>>();
            if !refs.is_empty() {
                paired_count += 1;
                refs.extend(repeated.get(&value).into_iter().flatten().cloned());
            }
            pairs.push(Pair {
                label: label.clone(),
                refs,
                primary_ref,
                previous_value: position
                    .checked_sub(1)
                    .and_then(|index| backbone[index].value),
                next_value: backbone
                    .get(position + 1)
                    .and_then(|candidate| candidate.value),
                restart_sequence: segment_index + 1,
                // Count the whole run, including labels later omitted as unsupported.
                sequence_labels: backbone.len(),
                endnote,
                pair_id: format!("fnv2-pair-LEGALPDF-document-{pair_number:06}"),
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
        pair_number += 1;
        let primary_ref = reference
            .as_ref()
            .map(|candidate| (candidate.line, candidate.start, candidate.end));
        if let Some(reference) = &reference {
            paired_count += 1;
            used_symbols.insert(reference.pos(&lines));
        }
        pairs.push(Pair {
            label: label.clone(),
            sequence_labels: 1,
            refs: reference.into_iter().collect(),
            primary_ref,
            previous_value: None,
            next_value: None,
            restart_sequence: 1,
            endnote: false,
            pair_id: format!("fnv2-pair-LEGALPDF-document-{pair_number:06}"),
            provenance: "article_start_custom_symbol_label".to_owned(),
        });
    }

    pairs.sort_by_key(|pair| pair.label.pos(&lines));
    let summary = include_audit.then(|| {
        let marker_count = pairs.len() + pairs.iter().map(|pair| pair.refs.len()).sum::<usize>();
        let label_only = pairs.len().saturating_sub(paired_count);
        json!({
        "schema_version": "oajd.footnote_pairing_v2.v1",
        "engine": "tools.footnotes.footnote_pairing_v2",
        "line_count": lines.len(),
        "marker_count": marker_count,
        "safe_marker_count": marker_count,
        "role_counts": {
            "fn_label": pairs.len(),
            "fn_ref": pairs.iter().map(|pair| pair.refs.len()).sum::<usize>()
        },
        "pair_count": paired_count,
        "materialized_pair_count": paired_count,
        "materialized_label_only_count": label_only,
        "pair_status_counts": {"paired": paired_count, "label_only": label_only},
        "label_candidate_count": labels.len(),
        "ref_candidate_count": refs.len(),
        "article_footnote_pair_materialization": {
            "materialized_marker_count": marker_count,
            "materialized_pair_count": paired_count,
            "materialized_label_only_count": label_only,
            "endnote_mode": endnote,
            "segment_count": segments.len(),
            "skipped_marker_counts": skipped,
            "numbered_paragraph_guard": numbered_diagnostics.expect("audit enabled"),
            "duplicate_valueset_guard": duplicate_diagnostics.expect("audit enabled"),
            "body_zone_restart_guard": body_restart_diagnostics.expect("audit enabled"),
            "label_backbone": backbone_summary.expect("audit enabled"),
            "sequence_holes": sequence_holes.expect("audit enabled"),
            "monotone_ref_sequence": {"drop_reason_counts": ref_drop_reasons},
            "ref_repair_counts": ref_repair_counts,
        },
        "workflow_stage_summary": {
            "stages": [
                "extract_candidates",
                "label_backbone_dp",
                "gap_glyph_repair",
                "out_of_order_label_recovery",
                "monotone_ref_assignment",
                "ref_glyph_repair",
                "false_positive_suppression",
                "custom_symbol_pairs",
                "materialize"
            ]
        },
        })
    });
    (pairs, lines, summary)
}
