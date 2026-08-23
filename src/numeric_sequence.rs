use std::collections::HashMap;

#[derive(Clone, Copy)]
pub struct NumericSequenceCandidate {
    pub index: usize,
    pub value: u32,
    pub position: (usize, usize),
    pub page: u32,
    pub score: f64,
    pub start_supported: bool,
}
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum NumericSequencePolicy {
    RootedConsecutive,
    FootnoteBackbone,
}

#[derive(Debug, PartialEq)]
pub struct NumericSequenceSelection {
    pub indices: Vec<usize>,
    pub score: f64,
}

pub fn select_numeric_sequence(
    mut candidates: Vec<NumericSequenceCandidate>,
    policy: NumericSequencePolicy,
) -> NumericSequenceSelection {
    candidates.sort_by_key(|candidate| (candidate.position, candidate.value));
    if candidates.is_empty() {
        return NumericSequenceSelection {
            indices: Vec::new(),
            score: 0.0,
        };
    }
    let mut best = vec![f64::NEG_INFINITY; candidates.len()];
    let mut parent = vec![None; candidates.len()];
    let mut prior_page_best = HashMap::<u32, usize>::new();
    let mut same_page_best = HashMap::<u32, usize>::new();
    let mut current_page = None;
    let mut group = 0;
    while group < candidates.len() {
        let end = (group + 1..candidates.len())
            .find(|index| candidates[*index].position != candidates[group].position)
            .unwrap_or(candidates.len());
        let page = candidates[group].page;
        if current_page != Some(page) {
            for (value, index) in same_page_best.drain() {
                if prior_page_best
                    .get(&value)
                    .is_none_or(|prior| best[index] > best[*prior] + 1e-9)
                {
                    prior_page_best.insert(value, index);
                }
            }
            current_page = Some(page);
        }
        for index in group..end {
            let candidate = candidates[index];
            match policy {
                NumericSequencePolicy::RootedConsecutive if candidate.value == 1 => {
                    best[index] = candidate.score
                }
                NumericSequencePolicy::FootnoteBackbone => {
                    best[index] = if candidate.start_supported {
                        candidate.score
                    } else {
                        candidate.score
                            + (-0.25 * f64::from(candidate.value.saturating_sub(1))).max(-4.0)
                    }
                }
                NumericSequencePolicy::RootedConsecutive => {}
            }
            let first = match policy {
                NumericSequencePolicy::RootedConsecutive => candidate.value.saturating_sub(1),
                NumericSequencePolicy::FootnoteBackbone => {
                    candidate.value.saturating_sub(201).max(1)
                }
            };
            let mut options = (first..candidate.value)
                .flat_map(|value| {
                    [
                        prior_page_best
                            .get(&value)
                            .copied()
                            .map(|index| (index, false)),
                        same_page_best
                            .get(&value)
                            .copied()
                            .map(|index| (index, true)),
                    ]
                    .into_iter()
                    .flatten()
                })
                .collect::<Vec<_>>();
            options.sort_unstable();
            for (previous, same_page) in options {
                let gap = candidate.value - candidates[previous].value - 1;
                let penalty = match policy {
                    NumericSequencePolicy::RootedConsecutive => 0.0,
                    NumericSequencePolicy::FootnoteBackbone => {
                        ((if same_page { -0.4 } else { -0.12 }) * f64::from(gap)).max(-4.0)
                    }
                };
                let score =
                    best[previous] + candidate.score + penalty + if gap == 0 { 0.3 } else { 0.0 };
                if score > best[index] + 1e-9 {
                    best[index] = score;
                    parent[index] = Some(previous);
                }
            }
        }
        for index in group..end {
            let value = candidates[index].value;
            if same_page_best
                .get(&value)
                .is_none_or(|prior| best[index] > best[*prior] + 1e-9)
            {
                same_page_best.insert(value, index);
            }
        }
        group = end;
    }
    let tail = match policy {
        NumericSequencePolicy::RootedConsecutive => (0..candidates.len())
            .filter(|index| best[*index].is_finite())
            .reduce(|left, right| {
                if best[right] > best[left] + 1e-9 {
                    right
                } else {
                    left
                }
            }),
        NumericSequencePolicy::FootnoteBackbone => (0..candidates.len()).max_by(|left, right| {
            best[*left]
                .total_cmp(&best[*right])
                .then_with(|| candidates[*right].position.cmp(&candidates[*left].position))
        }),
    };
    let Some(mut tail) = tail else {
        return NumericSequenceSelection {
            indices: Vec::new(),
            score: 0.0,
        };
    };
    let score = best[tail];
    let mut indices = Vec::new();
    loop {
        indices.push(candidates[tail].index);
        if let Some(previous) = parent[tail] {
            tail = previous;
        } else {
            break;
        }
    }
    indices.reverse();
    NumericSequenceSelection { indices, score }
}
