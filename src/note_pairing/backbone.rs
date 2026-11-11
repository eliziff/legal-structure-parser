//! The label backbone: the runs of labels that count on, and where a run of endnotes stands.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use legal_structure_model::layout::NoteMode;
use regex::Regex;
use super::{Candidate, PairLine, ENDNOTE_MIN_LABELS};

/// The best-scoring run of numbered labels in reading order, each counting on from one up to 200 before it: a label
/// opens a run at its score (less a quarter a number past 1, to at most 4, unless a reference supports it), and a step
/// adds its score, a third more where it counts on by one, less a skipped number's 0.4 on its page or 0.12 across pages
/// (to at most 4). A label takes the best run of each number on an earlier page, and on its own page before it.
pub(super) fn select_backbone(
    candidates: &[Candidate],
    lines: &[PairLine<'_>],
) -> Vec<Candidate> {
    let mut order = (0..candidates.len()).filter(|&index| candidates[index].value.is_some()).collect::<Vec<_>>();
    order.sort_by_key(|&index| (candidates[index].pos(lines), candidates[index].value));
    let label = |at: usize| &candidates[order[at]];
    let value = |at: usize| label(at).value.unwrap_or_default();
    let mut best = vec![f64::NEG_INFINITY; order.len()];
    let mut parent = vec![None; order.len()];
    let mut prior_page_best = HashMap::<u32, usize>::new();
    let mut same_page_best = HashMap::<u32, usize>::new();
    let mut current_page = None;
    let mut options = Vec::new();
    let mut group = 0;
    while group < order.len() {
        let position = label(group).pos(lines);
        let end = (group + 1..order.len()).find(|&at| label(at).pos(lines) != position).unwrap_or(order.len());
        let page = lines[label(group).line].page;
        if current_page != Some(page) {
            for (number, at) in same_page_best.drain() {
                if prior_page_best.get(&number).is_none_or(|prior| best[at] > best[*prior] + 1e-9) {
                    prior_page_best.insert(number, at);
                }
            }
            current_page = Some(page);
        }
        for at in group..end {
            let (number, score) = (value(at), label(at).score);
            best[at] = if label(at).flags.ref_supported { score } else { score + (-0.25 * f64::from(number.saturating_sub(1))).max(-4.0) };
            options.clear();
            options.extend((number.saturating_sub(201).max(1)..number).flat_map(|before| {
                [prior_page_best.get(&before).map(|&at| (at, false)), same_page_best.get(&before).map(|&at| (at, true))].into_iter().flatten()
            }));
            options.sort_unstable();
            for &(previous, same_page) in &options {
                let gap = number - value(previous) - 1;
                let penalty = ((if same_page { -0.4 } else { -0.12 }) * f64::from(gap)).max(-4.0);
                let reached = best[previous] + score + penalty + if gap == 0 { 0.3 } else { 0.0 };
                if reached > best[at] + 1e-9 {
                    best[at] = reached;
                    parent[at] = Some(previous);
                }
            }
        }
        for at in group..end {
            if same_page_best.get(&value(at)).is_none_or(|prior| best[at] > best[*prior] + 1e-9) {
                same_page_best.insert(value(at), at);
            }
        }
        group = end;
    }
    let tail = (0..order.len()).max_by(|left, right| {
        best[*left].total_cmp(&best[*right]).then_with(|| label(*right).pos(lines).cmp(&label(*left).pos(lines)))
    });
    let mut chain = Vec::new();
    let mut at = tail;
    while let Some(index) = at {
        chain.push(label(index).clone());
        at = parent[index];
    }
    chain.reverse();
    chain
}

pub(super) fn trim_unsupported_tail(mut segment: Vec<Candidate>) -> Vec<Candidate> {
    while segment.len() >= 2 {
        let tail = &segment[segment.len() - 1];
        let prior = &segment[segment.len() - 2];
        let gap = tail
            .value
            .unwrap_or(0)
            .saturating_sub(prior.value.unwrap_or(0));
        if gap > 20 && tail.score < 3.2 && !tail.flags.ref_supported {
            segment.pop();
        } else {
            break;
        }
    }
    segment
}

pub(super) fn select_segments(
    candidates: &[Candidate],
    lines: &[PairLine<'_>],
) -> Vec<Vec<Candidate>> {
    let mut remaining: Vec<Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.value.is_some())
        .cloned()
        .collect();
    let mut segments: Vec<Vec<Candidate>> = Vec::new();
    for _ in 0..6 {
        let chain = select_backbone(&remaining, lines);
        if chain.is_empty() {
            break;
        }
        if !segments.is_empty() {
            let average =
                chain.iter().map(|candidate| candidate.score).sum::<f64>() / chain.len() as f64;
            let first = chain[0].value.unwrap_or(0);
            let span = (
                lines[chain[0].line].idx,
                lines[chain.last().unwrap().line].idx,
            );
            let overlaps = segments.iter().any(|segment| {
                let other = (
                    lines[segment[0].line].idx,
                    lines[segment.last().unwrap().line].idx,
                );
                !(span.1 < other.0 || other.1 < span.0)
            });
            let strong_short = first == 1
                && average >= 3.2
                && (chain.len() >= 2
                    || (lines[chain[0].line].note_sequence_restart
                        && chain[0].flags.ref_supported));
            if (chain.len() < 4 && !strong_short) || average < 2.0 || first > 3 || overlaps {
                break;
            }
        }
        let used: HashSet<usize> = chain.iter().map(|candidate| candidate.line).collect();
        remaining.retain(|candidate| !used.contains(&candidate.line));
        segments.push(chain);
    }
    segments.sort_by_key(|segment| lines[segment[0].line].idx);
    segments
}

pub(super) fn endnote_mode(chain: &[Candidate], lines: &[PairLine<'_>]) -> bool {
    if chain
        .iter()
        .any(|candidate| lines[candidate.line].note_mode() == Some(NoteMode::Endnote))
    {
        return true;
    }
    if chain.len() < ENDNOTE_MIN_LABELS {
        return false;
    }
    let max_page = chain
        .iter()
        .map(|candidate| lines[candidate.line].page)
        .max()
        .unwrap_or(0);
    max_page > 1
        && chain
            .iter()
            .filter(|candidate| lines[candidate.line].page as f64 > 0.75 * max_page as f64)
            .count() as f64
            / chain.len() as f64
            >= 0.7
}

pub(super) fn visual_label_cue(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:diagram|figure|chart|table|graph|map|illustration|image|photo)")
            .unwrap()
    })
    .is_match(text)
}
