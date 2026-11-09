//! Pairs made notes: each a Footnote (or Endnote) node placed in the reader's text, and the diagnostics.

use super::*;

/// One part's pairs made notes, added to `pairing`'s: each note's id counts on from the notes before it, and its
/// numbering's restart from `restarts`, the restarts of the parts before; the count of restarts after this part's.
pub(super) fn materialize(
    pairs: Vec<Pair>,
    lines: &[PairLine<'_>],
    place: &dyn Fn(usize, usize, usize) -> Option<ScalarRange>,
    restarts: usize,
    pairing: &mut NotePairing,
) -> usize {
    let continuation_pages: HashSet<u32> = lines
        .iter()
        .filter(|line| line.note_mode() == Some(NoteMode::Continuation))
        .map(|line| line.page)
        .collect();
    let mut last_restart = restarts;
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
    // A pair is a note on positive evidence, plain numbers alone being none: a reference read for it that is typographic
    // (raised, detached, a symbol), stands by a label in the note region, or belongs to a run whose references the text
    // shows for a third of it at least (three at least); or its label alone (`label_only`), where the label stands in its
    // page's note band, which is final, or in the note region within a run whose references the text shows for a third
    // at least (two at least), or whose labels stand there for three quarters (three at least). Each count is of distinct
    // labels within one numbering's restart, after detached references are recovered, over the whole run the selector saw.
    let pairs = pairs
        .into_iter()
        .filter_map(|pair| {
            let label_line = &lines[pair.label.line];
            let run = (pair.restart_sequence, pair.endnote);
            let numbered = pair.label.value.is_some();
            let observed = observed_pair(&pair);
            let note_region = label_line.zone == Zone::Note && !label_line.region_witness_demoted;
            let labels = pair.sequence_labels;
            let observed_pairs = if numbered { observed_sequences.get(&run).copied().unwrap_or(0) } else { usize::from(observed) };
            let note_labels = if numbered { note_sequences.get(&run).copied().unwrap_or(0) } else { usize::from(note_region) };
            let label_only = label_line.in_note_band()
                || note_region && (observed_pairs >= 2 && labels <= 3 * observed_pairs || note_labels >= 3 && 4 * note_labels >= 3 * labels);
            let typographic = pair.refs.iter().any(|reference| !reference.repaired && matches!(reference.form, "sup" | "detached" | "symbol"));
            let corroborated = observed_pairs >= 3 && 3 * observed_pairs >= labels;
            (observed && (typographic || note_region || corroborated) || label_only).then_some((pair, label_only))
        })
        .collect::<Vec<_>>();
    for (index, &(ref pair, label_only)) in pairs.iter().enumerate() {
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
                    && (line.note_line()
                        || line.idx == label_line.idx
                        || (label_line.block.is_some() && line.page == label_line.page && line.block == label_line.block))
            })
            .collect();
        if let (false, Some(block)) = (pair.endnote, label_line.block) {
            // The blocks of the label's page the note runs through.
            let mut accepted = HashSet::from([block]);
            for line in &body_lines {
                if line.page == label_line.page
                    && line.bbox[1] <= label_line.bbox[3]
                    && line.bbox[3] >= label_line.bbox[1]
                {
                    accepted.extend(line.block);
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
                let mut include = line.block.is_some_and(|block| accepted.contains(&block));
                if !include && !blocked {
                    if let Some(previous) = prior {
                        let height = previous.height().max(1.0);
                        let gap = line.bbox[1] - previous.bbox[3];
                        let previous_size = previous.font_size();
                        let size = line.font_size();
                        include = gap <= (3.0_f64).max(height * 0.5)
                            && previous_size > 0.0
                            && (previous_size * 0.75..=previous_size * 1.25).contains(&size);
                    }
                }
                if include {
                    accepted.extend(line.block);
                    prior = Some(line);
                    bounded.push(line);
                } else {
                    blocked = true;
                }
            }
            body_lines = bounded;
        }
        // The body opens after the label and what sets it off (".", ")", a space) on the label's line.
        let start_of_body = label_token(label_line.text, provider_text(label_line))
            .filter(|token| token.start == pair.label.start && token.value == pair.label.value)
            .map_or(pair.label.end, |token| token.match_end);
        let after = label_line.tail(start_of_body);
        let body_start = start_of_body + after.chars().count()
            - after.trim_start_matches(|character: char| character.is_whitespace() || ".)],:;-".contains(character)).chars().count();
        let display = pair.label.note_id();
        // Placed in the reader's text, in UTF-16: the label, the calls, and each body line (the label's from where the body
        // opens).
        let label_range = place(label_line.at, label_line.unit(pair.label.start), label_line.unit(pair.label.end))
            .filter(|range| range.start < range.end);
        let calls = pair.refs.iter().map(|reference| {
            let line = &lines[reference.line];
            place(line.at, line.unit(reference.start), line.unit(reference.end)).filter(|range| range.start < range.end)
        }).collect::<Option<Vec<_>>>();
        let whole = body_lines.iter().any(|line| place(line.at, 0, line.coordinates.utf16_len()).is_some_and(|range| range.start < range.end));
        let (Some(label_range), Some(calls), true) = (label_range, calls, whole && (!pair.refs.is_empty() || label_only)) else {
            continue;
        };
        let ranges: Vec<ScalarRange> = body_lines.iter()
            .filter_map(|line| place(line.at, if line.idx == label_line.idx { line.unit(body_start) } else { 0 }, line.coordinates.utf16_len()))
            .filter(|range| range.start < range.end)
            .collect();
        if pair.refs.is_empty() {
            pairing.diagnostics.push(Diagnostic::new("FOOTNOTE_UNMATCHED_LABEL", Some(label_line.page_index)));
        }
        // The source's physical note kind survives a document-wide numbering run. That run's
        // inferred endnote mode is only a fallback for labels recovered outside a note region.
        let kind = match label_line.note_mode() {
            Some(NoteMode::Footnote) => NodeKind::Footnote,
            Some(NoteMode::Endnote) => NodeKind::Endnote,
            _ if pair.endnote => NodeKind::Endnote,
            _ => NodeKind::Footnote,
        };
        let range = ranges.first().zip(ranges.last()).map_or(ScalarRange { start: label_range.end, end: label_range.end },
            |(first, last)| ScalarRange { start: first.start, end: last.end });
        let id = format!("fnv2-pair-LEGALPDF-document-{:06}", pairing.notes.len() + 1);
        let mut node = StructureNode::new(id, kind, range, NOTE_PAIRING_ORIGIN, Derivation::Heuristic, None);
        node.ranges = ranges;
        node.line_ids = body_lines.iter().map(|line| line.id.to_owned()).collect();
        node.page_indexes = body_lines.iter().map(|line| line.page_index).collect::<BTreeSet<_>>().into_iter().collect();
        node.marker_range = Some(label_range);
        node.calls = calls;
        node.label = Some(format!("{display}"));
        last_restart = last_restart.max(restarts + pair.restart_sequence);
        node.series = (restarts + pair.restart_sequence).to_string();
        if !pair.provenance.is_empty() {
            node.properties.insert("provenance".to_owned(), pair.provenance.clone());
        }
        pairing.notes.push(node);
    }
    last_restart
}
