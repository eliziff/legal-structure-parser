//! Pairs made notes: their bodies, anchors, claims and diagnostics.

use super::*;

pub(super) fn materialize(
    pairs: Vec<Pair>,
    lines: &[PairLine<'_>],
    pairing_summary: Option<Value>,
) -> PairingOutput {
    let mut occurrences: HashMap<String, usize> = HashMap::new();
    let continuation_pages: HashSet<u32> = lines
        .iter()
        .filter(|line| line.note_region_mode == "footnote_continuation")
        .map(|line| line.page)
        .collect();
    let mut footnotes = Vec::new();
    let mut diagnostics = Vec::new();
    let mut anchors: HashMap<String, Vec<Anchor>> = HashMap::new();
    let mut pair_claims = Vec::new();
    let observed_pair =
        |pair: &Pair| !pair.label.repaired && pair.refs.iter().any(|reference| !reference.repaired);
    // Count after detached-reference recovery, once per distinct numeric label.
    // The denominator remains the full pre-filter run saved by the selector.
    let mut observed_sequences = HashMap::<(usize, bool), usize>::new();
    let mut note_sequences = HashMap::<(usize, bool), usize>::new();
    for pair in &pairs {
        let line = &lines[pair.label.line];
        if pair.label.value.is_some() && line.zone == Zone::Note && !line.region_witness_demoted {
            *note_sequences
                .entry((pair.restart_sequence, pair.endnote))
                .or_default() += 1;
        }
        if pair.label.value.is_some() && observed_pair(pair) {
            *observed_sequences
                .entry((pair.restart_sequence, pair.endnote))
                .or_default() += 1;
        }
    }
    let pairs = pairs
        .into_iter()
        .filter_map(|pair| {
            let label_line = &lines[pair.label.line];
            let support = NotePairSupportV2 {
                sequence_id: pair.restart_sequence,
                sequence_labels: pair.sequence_labels,
                sequence_observed_pairs: if pair.label.value.is_some() {
                    observed_sequences
                        .get(&(pair.restart_sequence, pair.endnote))
                        .copied()
                        .unwrap_or(0)
                } else {
                    usize::from(observed_pair(&pair))
                },
                sequence_note_labels: if pair.label.value.is_some() {
                    note_sequences
                        .get(&(pair.restart_sequence, pair.endnote))
                        .copied()
                        .unwrap_or(0)
                } else {
                    usize::from(label_line.zone == Zone::Note && !label_line.region_witness_demoted)
                },
                observed_pair: observed_pair(&pair),
                typographic_reference: pair.refs.iter().any(|reference| {
                    !reference.repaired && matches!(reference.form, "sup" | "detached" | "symbol")
                }),
                note_region: label_line.zone == Zone::Note && !label_line.region_witness_demoted,
            };
            support.supports_note().then_some((pair, support))
        })
        .collect::<Vec<_>>();
    let pairing_audit = pairing_summary.map(|mut summary| {
        let markers = pair_markers(&pairs, lines);
        refresh_summary(&mut summary, &markers);
        PdfPairingAudit {
            markers,
            pairing_summary: summary,
        }
    });
    for (index, (pair, support)) in pairs.iter().enumerate() {
        let label_line = &lines[pair.label.line];
        let next = pairs.get(index + 1).map(|(pair, _)| pair);
        let next_page = next.map(|value| lines[value.label.line].page);
        let mut allowed_pages = BTreeSet::new();
        allowed_pages.insert(label_line.page);
        if pair.endnote {
            let last_page =
                next_page.unwrap_or_else(|| lines.last().map_or(label_line.page, |line| line.page));
            allowed_pages.extend(label_line.page..=last_page);
        } else {
            if next_page == Some(label_line.page + 1) {
                allowed_pages.insert(label_line.page + 1);
            }
            let mut continuation = label_line.page + 1;
            while continuation_pages.contains(&continuation) {
                allowed_pages.insert(continuation);
                continuation += 1;
            }
        }
        let stop = next
            .filter(|value| pair.endnote || allowed_pages.contains(&lines[value.label.line].page))
            .map_or(usize::MAX, |value| lines[value.label.line].idx);
        let mut body_lines: Vec<&PairLine<'_>> = lines[label_line.idx..stop.min(lines.len())]
            .iter()
            // The label's own line and the lines of its block are the note's, whatever region
            // the layout called them (a note set as body text or in the footer).
            .filter(|line| {
                allowed_pages.contains(&line.page)
                    && (line.region_type == "footnote"
                        || line.idx == label_line.idx
                        || (!label_line.region_id.is_empty()
                            && line.region_id == label_line.region_id))
            })
            .collect();
        if !pair.endnote && !label_line.region_id.is_empty() {
            let mut accepted = HashSet::from([label_line.region_id.as_str()]);
            for line in &body_lines {
                if line.page == label_line.page
                    && !line.region_id.is_empty()
                    && line.bbox[1] <= label_line.bbox[3]
                    && line.bbox[3] >= label_line.bbox[1]
                {
                    accepted.insert(line.region_id.as_str());
                }
            }
            let mut bounded = Vec::new();
            let mut prior: Option<&PairLine<'_>> = None;
            let mut blocked = false;
            for line in body_lines {
                if line.page != label_line.page {
                    bounded.push(line);
                    continue;
                }
                let mut include = accepted.contains(line.region_id.as_str());
                if !include && !blocked {
                    if let Some(previous) = prior {
                        let height = previous.height().max(1.0);
                        let gap = line.bbox[1] - previous.bbox[3];
                        let previous_size = line_font_size(previous.line);
                        let size = line_font_size(line.line);
                        include = gap <= (3.0_f64).max(height * 0.5)
                            && previous_size > 0.0
                            && (previous_size * 0.75..=previous_size * 1.25).contains(&size);
                    }
                }
                if include {
                    if !line.region_id.is_empty() {
                        accepted.insert(line.region_id.as_str());
                    }
                    prior = Some(line);
                    bounded.push(line);
                } else {
                    blocked = true;
                }
            }
            body_lines = bounded;
        }
        let mut parts = Vec::new();
        let mut source_spans = Vec::new();
        let mut output_position = 0;
        for line in &body_lines {
            let mut text = line.text.as_str();
            if line.id == label_line.id {
                let body_start = label_token(text, provider_text(line))
                    .filter(|token| {
                        token.start == pair.label.start && token.value == pair.label.value
                    })
                    .map_or(pair.label.end, |token| token.match_end);
                text = char_slice(text, body_start, text.chars().count()).trim_start_matches(
                    |character: char| character.is_whitespace() || ".)],:;-".contains(character),
                );
            }
            if text.is_empty() {
                continue;
            }
            if !parts.is_empty() {
                output_position += 1;
            }
            let source_start =
                legal_structure_model::utf16_len(&line.text[..line.text.len() - text.len()]);
            let length = legal_structure_model::utf16_len(text);
            source_spans.push(legal_structure_model::layout::SourceTextSpan {
                start: output_position,
                end: output_position + length,
                line_id: line.id.clone(),
                source_start,
                source_end: source_start + length,
            });
            output_position += length;
            parts.push(text);
        }
        let display = pair.label.note_id();
        let joined = parts.join(" ");
        let leading =
            legal_structure_model::utf16_len(&joined[..joined.len() - joined.trim_start().len()]);
        let body = joined.trim().to_owned();
        let stop = leading + legal_structure_model::utf16_len(&body);
        source_spans = source_spans
            .iter()
            .filter_map(|span| span.retain(leading, stop, 0))
            .collect();
        let body = if body.is_empty() {
            display.clone()
        } else {
            body
        };
        let occurrence = occurrences.entry(display.clone()).or_insert(0);
        *occurrence += 1;
        for reference in &pair.refs {
            let line = &lines[reference.line];
            anchors.entry(line.id.clone()).or_default().push(Anchor {
                pair_id: pair.pair_id.clone(),
                label: display.clone(),
                start: reference.start,
                end: reference.end,
            });
        }
        let primary = pair.refs.first();
        let mut warnings = Vec::new();
        if primary.is_none() {
            warnings.push("label_only".to_owned());
            let mut diagnostic = Diagnostic::warning(
                "FOOTNOTE_UNMATCHED_LABEL",
                format!("Footnote label '{display}' has no paired reference."),
                Some(label_line.page_index),
            );
            diagnostic.line_ids.push(label_line.id.clone());
            diagnostic
                .details
                .insert("pair_id".to_owned(), json!(pair.pair_id));
            diagnostic
                .details
                .insert("label".to_owned(), json!(display));
            diagnostics.push(diagnostic);
        }
        footnotes.push(Footnote {
            source_spans,
            pair_id: pair.pair_id.clone(),
            label: display,
            occurrence: *occurrence,
            restart_sequence: pair.restart_sequence,
            reference_page: primary.map(|reference| lines[reference.line].page),
            body_pages: body_lines
                .iter()
                .map(|line| line.page)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            reference_line_id: primary.map(|reference| lines[reference.line].id.clone()),
            body_line_ids: body_lines.iter().map(|line| line.id.clone()).collect(),
            body,
            sentence_proposition: String::new(),
            passage_since_prior_note: String::new(),
            // The Python materializer reads the marker row's `confidence`
            // field. Canonical pairer rows expose `candidate_confidence`
            // instead, so the established public value is the 0.75 default.
            confidence: 0.75,
            provenance: pair.provenance.clone(),
            warnings,
            crossrefs: Vec::new(),
        });
        pair_claims.push(NotePairClaim {
            pair_id: pair.pair_id.clone(),
            support: support.clone(),
            label: pair.label.note_id(),
            kind: if pair.endnote {
                NotePairKind::Endnote
            } else {
                NotePairKind::Footnote
            },
            label_anchor: SourceAnchor {
                line_id: label_line.id.clone(),
                start: pair.label.start,
                end: pair.label.end,
            },
            reference_anchors: pair
                .refs
                .iter()
                .map(|reference| SourceAnchor {
                    line_id: lines[reference.line].id.clone(),
                    start: reference.start,
                    end: reference.end,
                })
                .collect(),
            body_line_ids: body_lines.iter().map(|line| line.id.clone()).collect(),
        });
    }
    PairingOutput {
        footnotes,
        diagnostics,
        anchors,
        pair_claims,
        pairing_audit,
    }
}
