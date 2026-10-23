//! Keep gold fixed; match page text, then project the candidate's spans into gold coordinates.
use crate::{state::State, text_diff};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

type Mapping = (usize, usize, usize, usize, usize);

pub struct Alignment {
    /// Candidate start/end, reference line, reference start/end (Unicode scalar coordinates).
    pub mappings: Vec<Vec<Mapping>>,
    /// For each reference character, the candidate line and position that own it. A mapped
    /// character is owned by its counterpart; an unmapped one (a transcription difference) by the
    /// mapped character before it, or after it at the start of a line. A line nothing maps to
    /// has no owners.
    pub owners: Vec<Vec<Option<(usize, usize)>>>,
    pub stats: Stats,
}

#[derive(Serialize)]
pub struct Stats {
    pub method: &'static str,
    pub elapsed_ms: f64,
    pub reference_sources: usize,
    pub candidate_sources: usize,
    pub shared_witness_sources: usize,
    pub unmatched_reference_sources: usize,
    pub unmatched_candidate_sources: usize,
    pub split_reference_sources: usize,
    pub merged_candidate_sources: usize,
    pub reference_character_coverage: f64,
    pub candidate_character_coverage: f64,
}

fn surface(state: &State, rows: &[usize]) -> (Vec<char>, Vec<(usize, usize, usize)>) {
    let mut text = Vec::new();
    let mut spans = Vec::new();
    for &n in rows {
        let start = text.len();
        text.extend(&state.lines[n].text);
        spans.push((start, text.len(), n));
    }
    (text, spans)
}

pub fn align(reference: &State, candidate: &State) -> Result<Alignment, String> {
    let started = Instant::now();
    let mut mappings = vec![Vec::new(); candidate.lines.len()];
    let mut used = HashSet::new();
    let mut shared = 0;
    for (c, line) in candidate.lines.iter().enumerate() {
        if let Some(&r) = reference.index.get(&line.id) {
            let target = &reference.lines[r];
            if target.page == line.page && target.text == line.text {
                mappings[c].push((0, line.text.len(), r, 0, target.text.len()));
                used.insert(r);
                shared += 1;
            }
        }
    }
    let mut pages: BTreeMap<usize, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (r, line) in reference.lines.iter().enumerate() {
        if !used.contains(&r) {
            pages.entry(line.page).or_default().0.push(r);
        }
    }
    for (c, line) in candidate.lines.iter().enumerate() {
        if mappings[c].is_empty() {
            pages.entry(line.page).or_default().1.push(c);
        }
    }
    for (mut a, mut b) in pages.into_values() {
        if a.is_empty() || b.is_empty() {
            continue;
        }
        // Unique exact lines retain identity even when the parser puts them in the wrong order.
        let mut by_text: HashMap<&[char], Vec<usize>> = HashMap::new();
        let mut counts: HashMap<&[char], usize> = HashMap::new();
        for &r in &a {
            by_text.entry(&reference.lines[r].text).or_default().push(r);
        }
        for &c in &b {
            *counts.entry(&candidate.lines[c].text).or_default() += 1;
        }
        for &c in &b {
            let text = candidate.lines[c].text.as_slice();
            if let Some(targets) = by_text.get(text) {
                if !text.is_empty() && targets.len() == 1 && counts[text] == 1 {
                    mappings[c].push((0, text.len(), targets[0], 0, text.len()));
                    used.insert(targets[0]);
                }
            }
        }
        a.retain(|r| !used.contains(r));
        b.retain(|&c| mappings[c].is_empty());
        a.sort_by(|&x, &y| {
            reference.lines[x]
                .spatial
                .partial_cmp(&reference.lines[y].spatial)
                .unwrap()
        });
        b.sort_by(|&x, &y| {
            candidate.lines[x]
                .spatial
                .partial_cmp(&candidate.lines[y].spatial)
                .unwrap()
        });
        let (left, spans_a) = surface(reference, &a);
        let (right, spans_b) = surface(candidate, &b);
        let (mut ia, mut ib) = (0, 0);
        for (mut x, mut y, mut remaining) in text_diff::matching_blocks(&left, &right)? {
            while remaining > 0 {
                while spans_a[ia].1 <= x {
                    ia += 1;
                }
                while spans_b[ib].1 <= y {
                    ib += 1;
                }
                let (a0, a1, r) = spans_a[ia];
                let (b0, b1, c) = spans_b[ib];
                let size = remaining.min(a1 - x).min(b1 - y);
                mappings[c].push((y - b0, y - b0 + size, r, x - a0, x - a0 + size));
                x += size;
                y += size;
                remaining -= size;
            }
        }
    }
    for spans in &mut mappings {
        spans.sort_unstable();
        spans.dedup();
        let mut end = 0;
        spans.retain(|span| {
            if span.0 < end {
                return false;
            }
            end = span.1;
            true
        });
    }
    let mut owners: Vec<Vec<Option<(usize, usize)>>> =
        reference.lines.iter().map(|l| vec![None; l.text.len()]).collect();
    for (c, spans) in mappings.iter().enumerate() {
        for &(lo, hi, r, offset, _) in spans {
            for k in 0..hi - lo {
                owners[r][offset + k] = Some((c, lo + k));
            }
        }
    }
    for line in &mut owners {
        let mut last = None;
        for slot in line.iter_mut() {
            match slot {
                Some(owner) => last = Some(*owner),
                None => *slot = last,
            }
        }
        if let Some(first) = line.iter().position(Option::is_some) {
            let owner = line[first];
            line[..first].fill(owner);
        }
    }
    let mut targets = vec![HashSet::new(); reference.lines.len()];
    let mut covered = vec![Vec::new(); reference.lines.len()];
    for (c, spans) in mappings.iter().enumerate() {
        for &(_, _, r, start, end) in spans {
            targets[r].insert(c);
            covered[r].push((start, end));
        }
    }
    let mut reference_covered = 0;
    for spans in &mut covered {
        spans.sort_unstable();
        let mut cursor = 0;
        for &(start, end) in spans.iter() {
            reference_covered += end.saturating_sub(start.max(cursor));
            cursor = cursor.max(end);
        }
    }
    let coverage = |covered: usize, total: usize| {
        if total == 0 {
            1.0
        } else {
            covered as f64 / total as f64
        }
    };
    let stats = Stats {
        method: "shared-witness-then-page-local-myers-unicode-scalars",
        elapsed_ms: started.elapsed().as_secs_f64() * 1e3,
        reference_sources: reference.lines.len(),
        candidate_sources: candidate.lines.len(),
        shared_witness_sources: shared,
        unmatched_reference_sources: targets.iter().filter(|s| s.is_empty()).count(),
        unmatched_candidate_sources: mappings.iter().filter(|s| s.is_empty()).count(),
        split_reference_sources: targets.iter().filter(|s| s.len() > 1).count(),
        merged_candidate_sources: mappings
            .iter()
            .filter(|s| s.iter().map(|m| m.2).collect::<HashSet<_>>().len() > 1)
            .count(),
        reference_character_coverage: coverage(
            reference_covered,
            reference.lines.iter().map(|l| l.text.len()).sum(),
        ),
        candidate_character_coverage: coverage(
            mappings.iter().flatten().map(|m| m.1 - m.0).sum(),
            candidate.lines.iter().map(|l| l.text.len()).sum(),
        ),
    };
    Ok(Alignment { mappings, owners, stats })
}
