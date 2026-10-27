//! The label backbone: the runs of labels that count on, and where a run of endnotes stands.

use super::*;

pub(super) fn select_backbone(
    candidates: &[Candidate],
    lines: &[PairLine<'_>],
    include_audit: bool,
) -> (Vec<Candidate>, Option<Value>) {
    let numeric = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            Some(NumericSequenceCandidate {
                index,
                value: candidate.value?,
                position: candidate.pos(lines),
                page: lines[candidate.line].page,
                score: candidate.score,
                start_supported: candidate.flags.ref_supported,
            })
        })
        .collect::<Vec<_>>();
    let candidate_count = numeric.len();
    if candidate_count == 0 {
        return (
            Vec::new(),
            include_audit.then(|| json!({"candidate_count": 0})),
        );
    }
    let selected = select_numeric_sequence(numeric, NumericSequencePolicy::FootnoteBackbone);
    let chain = selected
        .indices
        .into_iter()
        .map(|index| candidates[index].clone())
        .collect::<Vec<_>>();
    let diagnostic = include_audit.then(|| {
        json!({
            "candidate_count": candidate_count,
            "selected_count": chain.len(),
            "chain_score": (selected.score * 1_000.0).round_ties_even() / 1_000.0,
            "first_value": chain.first().and_then(|candidate| candidate.value),
            "last_value": chain.last().and_then(|candidate| candidate.value),
        })
    });
    (chain, diagnostic)
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
    include_audit: bool,
) -> (Vec<Vec<Candidate>>, Option<Value>) {
    let mut remaining: Vec<Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.value.is_some())
        .cloned()
        .collect();
    let mut segments: Vec<Vec<Candidate>> = Vec::new();
    let mut diagnostics = include_audit.then(Vec::new);
    for _ in 0..6 {
        let (chain, diagnostic) = select_backbone(&remaining, lines, include_audit);
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
        if let (Some(diagnostics), Some(diagnostic)) = (&mut diagnostics, diagnostic) {
            diagnostics.push(diagnostic);
        }
        segments.push(chain);
    }
    segments.sort_by_key(|segment| lines[segment[0].line].idx);
    let count = segments.len();
    (
        segments,
        diagnostics.map(|diagnostics| json!({"segments": diagnostics, "segment_count": count})),
    )
}

pub(super) fn endnote_mode(chain: &[Candidate], lines: &[PairLine<'_>]) -> bool {
    if chain
        .iter()
        .any(|candidate| lines[candidate.line].note_region_mode == "endnote")
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
