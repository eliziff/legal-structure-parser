//! References matched to the backbone's labels, in order.

use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) struct RefTail {
    value: u32,
    reference: usize,
    parent: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RefState {
    position: Option<(usize, usize)>,
    score: f64,
    tail: Option<usize>,
}

pub(super) fn select_refs(
    backbone: &[Candidate],
    refs: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> (HashMap<u32, Candidate>, BTreeMap<String, usize>) {
    let mut drop_reasons = BTreeMap::<String, usize>::new();
    let labels: HashMap<u32, &Candidate> = backbone
        .iter()
        .filter_map(|candidate| candidate.value.map(|value| (value, candidate)))
        .collect();
    let label_spans: HashSet<(usize, usize)> = backbone
        .iter()
        .map(|candidate| (candidate.line, candidate.start))
        .collect();
    let mut candidates_by_value: HashMap<u32, Vec<usize>> = HashMap::new();
    for (index, candidate) in refs.iter().enumerate() {
        let Some(value) = candidate.value else {
            continue;
        };
        if label_spans.contains(&(candidate.line, candidate.start)) {
            *drop_reasons
                .entry("is_selected_label_span".to_owned())
                .or_default() += 1;
            continue;
        }
        let Some(label) = labels.get(&value) else {
            *drop_reasons
                .entry("no_selected_label".to_owned())
                .or_default() += 1;
            continue;
        };
        let table_superscript = candidate.form == "sup"
            && lines[candidate.line]
                .region_type
                .to_lowercase()
                .contains("table");
        if candidate.requires_visual_cue && !table_superscript {
            let label_body = char_slice(
                &lines[label.line].text,
                label.end,
                lines[label.line].text.chars().count(),
            );
            if !visual_label_cue(&label_body) {
                *drop_reasons
                    .entry("visual_zone_without_visual_label_cue".to_owned())
                    .or_default() += 1;
                continue;
            }
        }
        candidates_by_value.entry(value).or_default().push(index);
    }
    let mut values: Vec<u32> = labels.keys().copied().collect();
    values.sort_unstable();
    let max_pool_index = candidates_by_value
        .values()
        .flatten()
        .map(|index| lines[refs[*index].line].idx)
        .max()
        .unwrap_or(0)
        + 1;
    type MatchChoice = (f64, (usize, usize), usize);
    let mut by_value: HashMap<u32, Vec<MatchChoice>> = HashMap::new();
    for value in &values {
        let label = labels[value];
        let mut choices = Vec::new();
        for index in candidates_by_value.get(value).into_iter().flatten() {
            let candidate = &refs[*index];
            let page_delta =
                i64::from(lines[label.line].page) - i64::from(lines[candidate.line].page);
            if !endnote && labels.len() >= ENDNOTE_MIN_LABELS && page_delta.unsigned_abs() >= 2 {
                continue;
            }
            let proximity = if endnote {
                0.08 * lines[candidate.line].idx as f64 / max_pool_index as f64
            } else if page_delta == 0 {
                1.0
            } else if page_delta == 1 {
                0.35
            } else if page_delta == -1 && lines[candidate.line].order <= 10 {
                -0.2
            } else if page_delta < 0 {
                -2.0 - 0.45 * (-page_delta - 1) as f64
            } else {
                -0.45 * (page_delta - 1) as f64
            };
            choices.push((candidate.score + proximity, candidate.pos(lines), *index));
        }
        choices.sort_by(|left, right| {
            right
                .0
                .total_cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        by_value.insert(*value, choices);
    }
    let mut tails = Vec::<RefTail>::new();
    let mut states = vec![RefState {
        position: None,
        score: 0.0,
        tail: None,
    }];
    for value in values {
        let mut next = states.clone();
        for state in &states {
            for (option_score, position, reference) in
                by_value.get(&value).into_iter().flatten().take(6)
            {
                if *option_score <= -1.0
                    || state.position.is_some_and(|previous| *position <= previous)
                {
                    continue;
                }
                let tail = tails.len();
                tails.push(RefTail {
                    value,
                    reference: *reference,
                    parent: state.tail,
                });
                next.push(RefState {
                    position: Some(*position),
                    score: state.score + option_score + 0.5,
                    tail: Some(tail),
                });
            }
        }
        next.sort_by(|left, right| {
            left.position
                .cmp(&right.position)
                .then_with(|| right.score.total_cmp(&left.score))
        });
        let mut best = f64::NEG_INFINITY;
        let mut pruned = Vec::new();
        for state in next {
            if state.score > best + 1e-9 {
                best = state.score;
                pruned.push(state);
            }
        }
        if pruned.len() > 400 {
            pruned.drain(..pruned.len() - 400);
        }
        states = pruned;
    }
    let mut result = HashMap::new();
    let mut final_state = &states[0];
    for state in &states[1..] {
        if state.score > final_state.score {
            final_state = state;
        }
    }
    let mut tail = final_state.tail;
    while let Some(index) = tail {
        let value = tails[index];
        result.insert(value.value, refs[value.reference].clone());
        tail = value.parent;
    }
    for value in labels.keys() {
        if !result.contains_key(value) {
            if let Some(count) = candidates_by_value.get(value).map(Vec::len) {
                *drop_reasons
                    .entry("window_conflict".to_owned())
                    .or_default() += count;
            }
        }
    }
    (result, drop_reasons)
}

pub(super) fn repeated_refs(
    chosen: &HashMap<u32, Candidate>,
    refs: &[Candidate],
    lines: &[PairLine<'_>],
) -> HashMap<u32, Vec<Candidate>> {
    let primary: HashSet<(usize, usize, usize)> = chosen
        .values()
        .map(|candidate| (candidate.line, candidate.start, candidate.end))
        .collect();
    let mut result: HashMap<u32, Vec<Candidate>> = HashMap::new();
    for candidate in refs {
        let Some(value) = candidate.value else {
            continue;
        };
        let Some(selected) = chosen.get(&value) else {
            continue;
        };
        if candidate.form != "sup"
            || primary.contains(&(candidate.line, candidate.start, candidate.end))
            || candidate.pos(lines) <= selected.pos(lines)
        {
            continue;
        }
        result.entry(value).or_default().push(candidate.clone());
    }
    result
}

pub(super) fn label_only_supported(backbone: &[Candidate], position: usize, lines: &[PairLine<'_>]) -> bool {
    let label = &backbone[position];
    label.zone_is_noteish(lines)
        && (label.score >= 1.5
            || [
                position.checked_sub(1),
                (position + 1 < backbone.len()).then_some(position + 1),
            ]
            .into_iter()
            .flatten()
            .any(|neighbor| {
                let neighbor = &backbone[neighbor];
                label.value.unwrap().abs_diff(neighbor.value.unwrap()) == 1
                    && lines[label.line].page.abs_diff(lines[neighbor.line].page) <= 1
            }))
}
