//! The pairing audit: each pair's markers as the goldset's rows.

use super::*;

pub(super) fn candidate_key(candidate: &Candidate) -> (usize, usize, usize) {
    (candidate.line, candidate.start, candidate.end)
}

pub(super) fn marker_confidence(value: f64) -> f64 {
    (value * 1_000.0).round_ties_even() / 1_000.0
}

pub(super) fn normalized_observed(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter_map(|character| {
            normalize_decimal_digit(character).or_else(|| {
                (!character.is_whitespace()).then_some(normalize_note_symbol(character))
            })
        })
        .collect::<String>()
        .trim_end_matches(['.', ')', ':', ']'])
        .trim_start_matches(['(', '['])
        .to_owned()
}

pub(super) fn canonical_marker_row(
    candidate: &Candidate,
    lines: &[PairLine<'_>],
    role: &str,
    marker_id: String,
    strategy: &str,
    confidence: f64,
) -> Map<String, Value> {
    let line = &lines[candidate.line];
    let note_id = candidate.note_id();
    let source_note_id = {
        let normalized = normalized_observed(&candidate.observed);
        if normalized.is_empty() {
            note_id.clone()
        } else {
            normalized
        }
    };
    let mut row = Map::new();
    row.insert(
        "schema_version".to_owned(),
        json!("oajd.footnote_pairing_v2_marker.v1"),
    );
    row.insert("marker_id".to_owned(), json!(marker_id));
    row.insert("role".to_owned(), json!(role));
    row.insert("safe_to_use".to_owned(), json!(true));
    row.insert("note_id".to_owned(), json!(note_id));
    row.insert("source_note_id".to_owned(), json!(source_note_id));
    row.insert("selected_text".to_owned(), json!(candidate.observed));
    row.insert("visible_marker_value".to_owned(), json!(candidate.observed));
    row.insert(
        "article_context_note_id_repaired".to_owned(),
        json!(candidate.repaired && candidate.repair_kind == "confusable_value_repair"),
    );
    row.insert(
        "repair_lane_status".to_owned(),
        json!(if candidate.repaired && !candidate.repair_kind.is_empty() {
            format!("repaired_{}", candidate.repair_kind)
        } else {
            "visible_glyph".to_owned()
        }),
    );
    row.insert("image_filename".to_owned(), json!(""));
    row.insert("dataset".to_owned(), json!("LEGALPDF"));
    row.insert("article_id".to_owned(), json!("document"));
    row.insert("pdf_page".to_owned(), json!(line.page));
    row.insert("line_id".to_owned(), json!(line.id));
    row.insert("region_id".to_owned(), json!(line.region_id));
    row.insert("reading_order_index".to_owned(), json!(line.order));
    row.insert("start_offset".to_owned(), json!(candidate.start));
    row.insert("end_offset".to_owned(), json!(candidate.end));
    row.insert("line_text".to_owned(), json!(line.text));
    row.insert("region_type".to_owned(), json!(line.region_type));
    row.insert(
        "line_type".to_owned(),
        json!(if line.region_type == "footnote" {
            "footnote"
        } else {
            "paragraph"
        }),
    );
    row.insert(
        "candidate_confidence".to_owned(),
        json!(marker_confidence(confidence)),
    );
    row.insert("candidate_reason".to_owned(), json!(candidate.reason));
    row.insert("pairing_strategy_family".to_owned(), json!(strategy));
    row.insert(
        "label_sequence_guard_status".to_owned(),
        json!(if role == "fn_label" {
            "passed"
        } else {
            "not_applicable"
        }),
    );
    row.insert("protected_span_guard_status".to_owned(), json!("passed"));
    row.insert(
        "materialization_source".to_owned(),
        json!("tools.footnotes.footnote_pairing_v2"),
    );
    row
}

#[allow(clippy::too_many_arguments)]
pub(super) fn insert_shared_marker_fields(
    row: &mut Map<String, Value>,
    pair_id: &str,
    note_id: &str,
    label_marker_id: &str,
    ref_marker_ids: &[String],
    status: &str,
    same_page: bool,
    sequence_context: &Value,
) {
    row.insert("materialized_pair_id".to_owned(), json!(pair_id));
    row.insert("materialized_pair_status".to_owned(), json!(status));
    row.insert("materialized_note_id".to_owned(), json!(note_id));
    row.insert(
        "materialized_ref_count".to_owned(),
        json!(ref_marker_ids.len()),
    );
    row.insert("materialized_label_count".to_owned(), json!(1));
    row.insert(
        "materialized_pair_scope".to_owned(),
        json!("full_article_sequence_context"),
    );
    row.insert(
        "materialized_label_marker_id".to_owned(),
        json!(label_marker_id),
    );
    row.insert(
        "materialized_ref_marker_ids".to_owned(),
        json!(ref_marker_ids),
    );
    row.insert(
        "materialized_label_same_page_as_ref".to_owned(),
        json!(same_page),
    );
    row.insert(
        "article_sequence_context".to_owned(),
        sequence_context.clone(),
    );
}

pub(super) fn pair_markers(pairs: &[(Pair, NotePairSupportV2)], lines: &[PairLine<'_>]) -> Vec<Value> {
    let mut ordered_pairs = pairs.iter().map(|(pair, _)| pair).collect::<Vec<_>>();
    ordered_pairs.sort_by(|left, right| left.pair_id.cmp(&right.pair_id));
    let mut rows = Vec::<Value>::new();
    let mut marker_sequence = 0;
    for pair in ordered_pairs {
        marker_sequence += 1;
        let label_marker_id = format!("fnv2-label-LEGALPDF-document-{marker_sequence:06}");
        let symbol_pair = !pair.label.symbol.is_empty();
        let label_strategy = if symbol_pair {
            "article_start_custom_symbol_label"
        } else if pair.label.repaired && pair.label.repair_kind == "confusable_value_repair" {
            "article_sequence_gap_glyph_repair"
        } else {
            "article_sequence_line_start_label"
        };
        let label_confidence = if symbol_pair {
            0.9
        } else {
            (0.7 + 0.05 * pair.label.score.max(0.0)).min(0.98)
        };
        let mut label_row = canonical_marker_row(
            &pair.label,
            lines,
            "fn_label",
            label_marker_id.clone(),
            label_strategy,
            label_confidence,
        );
        let mut canonical_refs: Vec<&Candidate> = pair
            .refs
            .iter()
            .filter(|candidate| candidate.form != "detached")
            .collect();
        canonical_refs.sort_by_key(|candidate| {
            (
                pair.primary_ref != Some(candidate_key(candidate)),
                candidate.pos(lines),
            )
        });
        let mut ref_rows = Vec::<Map<String, Value>>::new();
        for (ref_index, reference) in canonical_refs.iter().enumerate() {
            marker_sequence += 1;
            let marker_id = format!("fnv2-ref-LEGALPDF-document-{marker_sequence:06}");
            let primary = pair.primary_ref == Some(candidate_key(reference));
            let strategy = if symbol_pair {
                "article_context_custom_marker_ref_same_page_label"
            } else if !primary {
                "article_context_repeated_superscript_ref"
            } else if reference.requires_visual_cue {
                "article_context_visual_region_ref_same_page_label"
            } else if reference.repaired
                && matches!(
                    reference.repair_kind,
                    "confusable_value_repair" | "truncated_value_repair"
                )
            {
                "article_sequence_ref_value_repair"
            } else if lines[reference.line].page == lines[pair.label.line].page {
                "article_context_body_ref_same_page_label_sequence"
            } else {
                "article_context_body_ref_cross_page_label_sequence"
            };
            let confidence = if symbol_pair {
                0.9
            } else if !primary {
                0.85
            } else {
                (0.65 + 0.06 * reference.score.max(0.0)).min(0.98)
            };
            let mut row =
                canonical_marker_row(reference, lines, "fn_ref", marker_id, strategy, confidence);
            row.insert("valid_repeated_ref".to_owned(), json!(ref_index > 0));
            ref_rows.push(row);
        }
        let label_line = &lines[pair.label.line];
        let same_page = ref_rows.first().is_some_and(|row| {
            row.get("pdf_page").and_then(Value::as_u64) == Some(u64::from(label_line.page))
        });
        let sequence_context = if symbol_pair {
            json!({
                "value": pair.label.symbol,
                "selected_label_image_filename": "",
                "selected_label_pdf_page": label_line.page,
                "same_page_as_selected_label": same_page,
            })
        } else {
            json!({
                "value": pair.label.value,
                "previous_value": pair.previous_value,
                "next_value": pair.next_value,
                "endnote_mode": pair.endnote,
                "segment_index": pair.restart_sequence - 1,
                "selected_label_image_filename": "",
                "selected_label_pdf_page": label_line.page,
                "same_page_as_selected_label": same_page,
            })
        };
        let status = if ref_rows.is_empty() {
            "label_only"
        } else {
            "paired"
        };
        let ref_marker_ids: Vec<String> = ref_rows
            .iter()
            .filter_map(|row| {
                row.get("marker_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        insert_shared_marker_fields(
            &mut label_row,
            &pair.pair_id,
            &pair.label.note_id(),
            &label_marker_id,
            &ref_marker_ids,
            status,
            same_page,
            &sequence_context,
        );
        rows.push(Value::Object(label_row));
        for mut row in ref_rows {
            insert_shared_marker_fields(
                &mut row,
                &pair.pair_id,
                &pair.label.note_id(),
                &label_marker_id,
                &ref_marker_ids,
                status,
                same_page,
                &sequence_context,
            );
            rows.push(Value::Object(row));
        }
    }

    let detached_pairs: HashMap<(usize, usize, usize, String), &Pair> = pairs
        .iter()
        .flat_map(|(pair, _)| {
            pair.refs
                .iter()
                .filter(|candidate| candidate.form == "detached")
                .map(move |candidate| {
                    (
                        (
                            candidate.line,
                            candidate.start,
                            candidate.end,
                            candidate.note_id(),
                        ),
                        pair,
                    )
                })
        })
        .collect();
    let mut detached_count = 0;
    for (line_index, line) in lines.iter().enumerate() {
        for detached in line.detached_references.iter() {
            let raw = detached.note_id.as_str();
            let note_id =
                numeric_value(raw).map_or_else(|| normalize_symbol(raw), |n| n.to_string());
            let start = detached.start_offset;
            let end = detached.end_offset;
            let Some(pair) = detached_pairs.get(&(line_index, start, end, note_id.clone())) else {
                continue;
            };
            detached_count += 1;
            rows.push(json!({
                "schema_version": "oajd.footnote_pairing_v2_marker.v1",
                "marker_id": format!("legalpdf-detached-ref-{detached_count:06}"),
                "role": "fn_ref",
                "safe_to_use": true,
                "note_id": note_id,
                "selected_text": detached.selected_text,
                "line_id": line.id,
                "region_id": line.region_id,
                "region_type": line.region_type,
                "pdf_page": line.page,
                "reading_order_index": line.order,
                "start_offset": start,
                "end_offset": end,
                "confidence": 0.84,
                "pairing_strategy_family": "detached_pdf_superscript",
                "materialized_pair_id": pair.pair_id,
                "materialized_note_id": pair.label.note_id(),
                "materialized_pair_status": "paired",
                "restart_sequence": 1,
            }));
        }
    }

    let pair_indexes: HashMap<&str, usize> = pairs
        .iter()
        .enumerate()
        .map(|(index, (pair, _))| (pair.pair_id.as_str(), index))
        .collect();
    let mut grouped_rows = vec![(None, Vec::new()); pairs.len()];
    for (row_index, row) in rows.iter().enumerate() {
        let Some(pair_index) = row
            .get("materialized_pair_id")
            .and_then(Value::as_str)
            .and_then(|pair_id| pair_indexes.get(pair_id))
            .copied()
        else {
            continue;
        };
        match row.get("role").and_then(Value::as_str) {
            Some("fn_label") => {
                grouped_rows[pair_index].0.get_or_insert(row_index);
            }
            Some("fn_ref") => grouped_rows[pair_index].1.push(row_index),
            _ => {}
        }
    }
    for (label_index, ref_indexes) in grouped_rows {
        let Some(label_index) = label_index else {
            continue;
        };
        let label_page = rows[label_index]
            .get("pdf_page")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let status = if ref_indexes.is_empty() {
            "label_only"
        } else {
            "paired"
        };
        let ref_ids: Vec<String> = ref_indexes
            .iter()
            .filter_map(|&index| {
                rows[index]
                    .get("marker_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        let same_page = ref_indexes
            .iter()
            .any(|&index| rows[index].get("pdf_page").and_then(Value::as_u64) == Some(label_page));
        for index in std::iter::once(label_index).chain(ref_indexes) {
            if let Some(row) = rows[index].as_object_mut() {
                row.insert("materialized_pair_status".to_owned(), json!(status));
                row.insert("materialized_ref_count".to_owned(), json!(ref_ids.len()));
                row.insert("materialized_ref_marker_ids".to_owned(), json!(ref_ids));
                row.insert(
                    "materialized_label_same_page_as_ref".to_owned(),
                    json!(same_page),
                );
            }
        }
    }
    rows.sort_by_key(|row| {
        (
            row.get("reading_order_index")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            row.get("start_offset").and_then(Value::as_u64).unwrap_or(0),
        )
    });
    rows
}

pub(super) fn refresh_summary(summary: &mut Value, markers: &[Value]) {
    let mut role_counts = BTreeMap::<String, usize>::new();
    let mut pair_statuses = BTreeMap::<String, usize>::new();
    let mut marker_statuses = BTreeMap::<String, usize>::new();
    let mut cross_page_ids = HashSet::<String>::new();
    for marker in markers {
        let role = marker
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        *role_counts.entry(role.clone()).or_default() += 1;
        let status = marker
            .get("materialized_pair_status")
            .and_then(Value::as_str)
            .unwrap_or("label_only")
            .to_owned();
        *marker_statuses.entry(status.clone()).or_default() += 1;
        if role == "fn_label" {
            *pair_statuses.entry(status.clone()).or_default() += 1;
            if status == "paired" {
                if let Some(pair_id) = marker.get("materialized_pair_id").and_then(Value::as_str) {
                    if marker
                        .get("materialized_label_same_page_as_ref")
                        .and_then(Value::as_bool)
                        == Some(false)
                    {
                        cross_page_ids.insert(pair_id.to_owned());
                    }
                }
            }
        }
    }
    let paired = pair_statuses.get("paired").copied().unwrap_or(0);
    let label_only = pair_statuses.get("label_only").copied().unwrap_or(0);
    let has_lines = summary
        .get("line_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0;
    summary["dataset"] = json!(if has_lines { "LEGALPDF" } else { "" });
    summary["article_id"] = json!(if has_lines { "document" } else { "" });
    summary["selected_image_count"] = json!(usize::from(has_lines));
    summary["marker_count"] = json!(markers.len());
    summary["safe_marker_count"] = json!(markers.len());
    summary["role_counts"] = json!(role_counts);
    summary["safe_role_counts"] = json!(role_counts);
    summary["pair_count"] = json!(paired);
    summary["pair_status_counts"] = json!(pair_statuses);
    summary["materialized_marker_count"] = json!(markers.len());
    summary["materialized_pair_count"] = json!(paired);
    summary["materialized_label_only_count"] = json!(label_only);
    summary["materialized_marker_status_counts"] = json!(marker_statuses);
    summary["materialized_pair_status_counts"] = json!(pair_statuses);
    summary["article_footnote_pair_materialization"]["materialized_marker_count"] =
        json!(markers.len());
    summary["article_footnote_pair_materialization"]["materialized_pair_count"] = json!(paired);
    summary["article_footnote_pair_materialization"]["materialized_label_only_count"] =
        json!(label_only);
    summary["article_footnote_pair_materialization"]["cross_page_pair_count"] =
        json!(cross_page_ids.len());
    summary["article_footnote_pair_materialization"]["synthesized_label_marker_count"] =
        json!(markers
            .iter()
            .filter(|marker| {
                marker.get("role").and_then(Value::as_str) == Some("fn_label")
                    && marker
                        .get("article_context_note_id_repaired")
                        .and_then(Value::as_bool)
                        == Some(true)
            })
            .count());
    summary["workflow_stage_summary"] = json!({
        "stages": [
            "extract_candidates",
            "label_backbone_dp",
            "gap_glyph_repair",
            "monotone_ref_assignment",
            "custom_symbol_pairs",
            "materialize"
        ]
    });
}
