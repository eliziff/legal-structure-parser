//! Repairs to a backbone and its references: gaps, misread glyphs, labels out of order.

use super::*;

pub(super) fn confusable_variants(value: &str) -> HashSet<u32> {
    if value.is_empty() || value.chars().count() > 3 {
        return HashSet::new();
    }
    let mut result = String::new();
    for character in value.chars() {
        let mapped = normalize_decimal_digit(character).or(match character {
            'l' | 'I' | 'i' | '|' | '!' | 'í' | 't' => Some('1'),
            'o' | 'O' | '°' | 'º' | 'ð' | 'D' | 'Q' => Some('0'),
            's' | 'S' => Some('5'),
            'B' => Some('8'),
            'G' | 'b' => Some('6'),
            'Z' | 'z' => Some('2'),
            'g' | 'q' => Some('9'),
            'A' => Some('4'),
            'T' | '?' | 'n' => Some('7'),
            'J' => Some('3'),
            _ => None,
        });
        let Some(mapped) = mapped else {
            return HashSet::new();
        };
        result.push(mapped);
    }
    result
        .parse::<u32>()
        .ok()
        .filter(|value| (1..=MAX_VALUE).contains(value))
        .into_iter()
        .collect()
}

pub(super) fn gap_line_score(line: &PairLine<'_>, body: &str) -> f64 {
    let mut score = if line.zone == Zone::Note && !line.region_witness_demoted {
        3.0
    } else {
        0.6
    };
    if line.note_column_fit {
        score += 0.7;
    }
    if cues::layout_has_citation_cue(body) {
        score += 0.7;
    }
    score
}

pub(super) fn gap_repair_tokens(line: &PairLine<'_>) -> Vec<(String, usize, usize)> {
    let mut tokens = Vec::new();
    if let Some(token) = label_token(&line.text, provider_text(line)) {
        if token.value.is_some() && matches!(token.form, "plain" | "sup") {
            tokens.push((token.observed, token.start, token.end));
        }
    }
    let values = chars(&line.text);
    let lead = values
        .iter()
        .take_while(|value| value.is_whitespace())
        .count();
    let mut end = lead;
    while end < values.len()
        && end - lead < 3
        && (values[end].is_ascii_digit() || "lIi|!íoO°ºDQsSBGbZzgqATnJt?".contains(values[end]))
    {
        end += 1;
    }
    if end > lead {
        let observed: String = values[lead..end].iter().collect();
        let has_digit = observed.chars().any(|value| value.is_ascii_digit());
        let follow_ok = values
            .get(end)
            .is_none_or(|value| value.is_whitespace() || *value == '.');
        if has_digit
            && follow_ok
            && !tokens
                .iter()
                .any(|(_, start, stop)| *start == lead && *stop == end)
        {
            tokens.push((observed, lead, end));
        }
    }
    tokens
}

pub(super) fn gap_repair_claim(
    line_index: usize,
    line: &PairLine<'_>,
    unclaimed: &HashSet<u32>,
    last_value: u32,
) -> Option<Candidate> {
    for (observed, start, end) in gap_repair_tokens(line) {
        let exact = numeric_value(&observed)
            .filter(|value| unclaimed.contains(value) && *value > last_value);
        let mut variants: Vec<u32> = confusable_variants(&observed)
            .into_iter()
            .filter(|value| unclaimed.contains(value) && *value > last_value)
            .collect();
        variants.sort_unstable();
        let (value, repair_kind) = if let Some(value) = exact {
            (value, "weak_form_promoted")
        } else {
            (variants.first().copied()?, "confusable_value_repair")
        };
        let body = char_slice(&line.text, end, line.text.chars().count())
            .trim_start_matches(|character: char| " .)]".contains(character))
            .to_owned();
        let score = gap_line_score(line, &body);
        if body.chars().count() < 4 || score < 1.2 {
            continue;
        }
        return Some(Candidate {
            line: line_index,
            start,
            end,
            observed,
            value: Some(value),
            symbol: String::new(),
            form: "plain",
            score,
            reason: "sequence_gap_glyph_repair",
            repaired: true,
            repair_kind,
            requires_visual_cue: false,
            flags: CandidateFlags::default(),
        });
    }
    None
}

pub(super) fn sequence_position_rekey(
    window: &[usize],
    residual: &[u32],
    lines: &[PairLine<'_>],
    used_lines: &mut HashSet<usize>,
) -> Vec<Candidate> {
    if residual.is_empty() || residual.len() > 8 {
        return Vec::new();
    }
    let residual_set: HashSet<u32> = residual.iter().copied().collect();
    let mut qualifying = Vec::<(usize, String, usize, usize)>::new();
    for &line_index in window {
        if used_lines.contains(&line_index) {
            continue;
        }
        let line = &lines[line_index];
        let Some(token) = label_token(&line.text, provider_text(line)) else {
            continue;
        };
        if !matches!(token.form, "plain" | "sup") {
            continue;
        }
        let Some(value) = token.value else {
            continue;
        };
        if residual_set.contains(&value) || value >= residual[0] {
            return Vec::new();
        }
        let body = char_slice(&line.text, token.end, line.text.chars().count())
            .trim_start_matches(|character: char| " .)]".contains(character))
            .to_owned();
        if body.chars().count() < 4 || cues::layout_is_citation_continuation(&body) {
            continue;
        }
        if gap_line_score(line, &body) < 1.2 {
            continue;
        }
        qualifying.push((line_index, token.observed, token.start, token.end));
    }
    if qualifying.len() != residual.len() {
        return Vec::new();
    }
    residual
        .iter()
        .copied()
        .zip(qualifying)
        .map(|(value, (line, observed, start, end))| {
            used_lines.insert(line);
            Candidate {
                line,
                start,
                end,
                observed,
                value: Some(value),
                symbol: String::new(),
                form: "plain",
                score: if lines[line].zone == Zone::Note && !lines[line].region_witness_demoted {
                    3.0
                } else {
                    0.6
                },
                reason: "sequence_position_rekey",
                repaired: true,
                repair_kind: "sequence_position_rekey",
                requires_visual_cue: false,
                flags: CandidateFlags::default(),
            }
        })
        .collect()
}

pub(super) fn repair_gaps(
    chain: Vec<Candidate>,
    lines: &[PairLine<'_>],
    used_lines: &mut HashSet<usize>,
    include_audit: bool,
) -> (Vec<Candidate>, Option<Vec<Value>>) {
    if chain.is_empty() {
        return (chain, include_audit.then(Vec::new));
    }
    let mut windows = Vec::new();
    let mut holes = include_audit.then(Vec::new);
    if chain[0].value.is_some_and(|value| (2..=9).contains(&value)) {
        windows.push((0, lines[chain[0].line].idx, 1, chain[0].value.unwrap() - 1));
    } else if chain[0].value.is_some_and(|value| value > 9) {
        if let Some(holes) = &mut holes {
            holes.push(json!({
                "values_before_first": chain[0].value.unwrap() - 1,
                "reason": "chain_starts_above_one",
            }));
        }
    }
    for pair in chain.windows(2) {
        let left = pair[0].value.unwrap();
        let right = pair[1].value.unwrap();
        if right > left + 1 {
            windows.push((
                lines[pair[0].line].idx + 1,
                lines[pair[1].line].idx,
                left + 1,
                right - 1,
            ));
        }
    }
    let mut repairs = Vec::new();
    for (start, end, low, high) in windows {
        let window: Vec<usize> = lines
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
            .filter(|(line_index, line)| {
                !used_lines.contains(line_index)
                    && ((line.zone == Zone::Note && !line.region_witness_demoted)
                        || ((line.note_column_fit || line.zone == Zone::Body)
                            && cues::layout_has_citation_cue(&line.text)))
            })
            .map(|(line_index, _)| line_index)
            .collect();
        let mut unclaimed: HashSet<u32> = (low..=high).collect();
        let mut last_value = low.saturating_sub(1);
        let mut claims = Vec::<(usize, Candidate)>::new();
        for (position, &line_index) in window.iter().enumerate() {
            if unclaimed.is_empty() {
                break;
            }
            if let Some(candidate) =
                gap_repair_claim(line_index, &lines[line_index], &unclaimed, last_value)
            {
                let value = candidate.value.expect("gap value");
                claims.push((position, candidate.clone()));
                repairs.push(candidate);
                unclaimed.remove(&value);
                last_value = value;
                used_lines.insert(line_index);
            }
        }
        let mut previous_position = 0;
        let mut previous_value = low.saturating_sub(1);
        let mut intervals = Vec::<(Vec<usize>, u32, u32)>::new();
        for (position, candidate) in claims {
            let value = candidate.value.expect("gap value");
            if value > previous_value + 1 {
                intervals.push((
                    window[previous_position..position].to_vec(),
                    previous_value + 1,
                    value - 1,
                ));
            }
            previous_position = position + 1;
            previous_value = value;
        }
        if previous_value < high {
            intervals.push((
                window[previous_position..].to_vec(),
                previous_value + 1,
                high,
            ));
        }
        for (interval, interval_low, interval_high) in intervals {
            let residual: Vec<u32> = (interval_low..=interval_high)
                .filter(|value| unclaimed.contains(value))
                .collect();
            let rekeyed = sequence_position_rekey(&interval, &residual, lines, used_lines);
            if rekeyed.is_empty() {
                if let Some(holes) = &mut holes {
                    for value in residual {
                        holes.push(json!({
                            "value": value,
                            "reason": "no_confusable_glyph_in_window",
                        }));
                    }
                }
            } else {
                for candidate in &rekeyed {
                    if let Some(value) = candidate.value {
                        unclaimed.remove(&value);
                    }
                }
            }
            repairs.extend(rekeyed);
        }
    }
    let mut result = chain;
    result.extend(repairs);
    result.sort_by_key(|candidate| candidate.value);
    (result, holes)
}

pub(super) fn note_zone_column_split(lines: &[PairLine<'_>], page: u32) -> Option<f64> {
    let candidates: Vec<&PairLine<'_>> = lines
        .iter()
        .filter(|line| {
            line.page == page
                && line.zone == Zone::Note
                && line.page_width > 0.0
                && line.page_height > 0.0
                && (line.bbox[2] - line.bbox[0]) / line.page_width <= 0.55
        })
        .collect();
    let boxed = lines
        .iter()
        .filter(|line| line.page == page && line.zone == Zone::Note)
        .count();
    if candidates.len() < 6 || boxed == 0 || 1.0 - candidates.len() as f64 / boxed as f64 > 0.40 {
        return None;
    }
    let mut centers: Vec<f64> = candidates
        .iter()
        .map(|line| (line.bbox[0] + line.bbox[2]) / 2.0 / line.page_width)
        .collect();
    centers.sort_by(f64::total_cmp);
    let (left, right) = centers
        .windows(2)
        .map(|pair| (pair[0], pair[1]))
        .max_by(|left, right| (left.1 - left.0).total_cmp(&(right.1 - right.0)))?;
    let split = (left + right) / 2.0;
    if right - left < 0.12 || !(0.25..=0.75).contains(&split) {
        return None;
    }
    let left_lines: Vec<&&PairLine<'_>> = candidates
        .iter()
        .filter(|line| (line.bbox[0] + line.bbox[2]) / 2.0 / line.page_width < split)
        .collect();
    let right_lines: Vec<&&PairLine<'_>> = candidates
        .iter()
        .filter(|line| (line.bbox[0] + line.bbox[2]) / 2.0 / line.page_width >= split)
        .collect();
    if left_lines.len() < 3 || right_lines.len() < 3 {
        return None;
    }
    let left_y0 = left_lines
        .iter()
        .map(|line| line.bbox[1] / line.page_height)
        .min_by(f64::total_cmp)?;
    let left_y1 = left_lines
        .iter()
        .map(|line| line.bbox[3] / line.page_height)
        .max_by(f64::total_cmp)?;
    let right_y0 = right_lines
        .iter()
        .map(|line| line.bbox[1] / line.page_height)
        .min_by(f64::total_cmp)?;
    let right_y1 = right_lines
        .iter()
        .map(|line| line.bbox[3] / line.page_height)
        .max_by(f64::total_cmp)?;
    let span = left_y1.max(right_y1) - left_y0.min(right_y0);
    let overlap = left_y1.min(right_y1) - left_y0.max(right_y0);
    if span <= 0.0 || overlap.max(0.0) / span < 0.30 {
        return None;
    }
    let left_widths: Vec<f64> = left_lines
        .iter()
        .map(|line| (line.bbox[2] - line.bbox[0]) / line.page_width)
        .collect();
    let right_widths: Vec<f64> = right_lines
        .iter()
        .map(|line| (line.bbox[2] - line.bbox[0]) / line.page_width)
        .collect();
    let (left_width, right_width) = (layout::upper_median(left_widths), layout::upper_median(right_widths));
    let wide = left_width.max(right_width);
    if wide > 0.0 && left_width.min(right_width) / wide < 0.60 {
        None
    } else {
        Some(split)
    }
}

pub(super) fn recover_out_of_order_labels(
    mut chain: Vec<Candidate>,
    label_candidates: &[Candidate],
    used_lines: &mut HashSet<usize>,
    lines: &[PairLine<'_>],
) -> Vec<Candidate> {
    if chain.len() < 2 {
        return chain;
    }
    let have: HashSet<u32> = chain
        .iter()
        .filter_map(|candidate| candidate.value)
        .collect();
    let Some(low) = have.iter().min().copied() else {
        return chain;
    };
    let high = have.iter().max().copied().unwrap_or(low);
    let min_page = chain
        .iter()
        .map(|candidate| lines[candidate.line].page)
        .min()
        .unwrap_or(0);
    let max_page = chain
        .iter()
        .map(|candidate| lines[candidate.line].page)
        .max()
        .unwrap_or(0);
    let mut by_value: HashMap<u32, Vec<Candidate>> = HashMap::new();
    for candidate in label_candidates {
        let Some(value) = candidate.value else {
            continue;
        };
        let line = &lines[candidate.line];
        let noteish = (line.zone == Zone::Note && !line.region_witness_demoted)
            || ((line.note_column_fit || line.zone == Zone::Body)
                && cues::layout_has_citation_cue(&line.text));
        if candidate.symbol.is_empty()
            && candidate.form == "plain"
            && !have.contains(&value)
            && low < value
            && value < high
            && !used_lines.contains(&candidate.line)
            && (min_page..=max_page).contains(&line.page)
            && noteish
            && candidate.score >= 1.2
        {
            by_value.entry(value).or_default().push(candidate.clone());
        }
    }
    let mut restored = Vec::new();
    for options in by_value.values_mut() {
        options.sort_by(|left, right| right.score.total_cmp(&left.score));
        if options.len() > 1 && options[0].score - options[1].score < 0.3 {
            continue;
        }
        let candidate = options[0].clone();
        let Some(split) = note_zone_column_split(lines, lines[candidate.line].page) else {
            continue;
        };
        let mut page_labels: Vec<&Candidate> = chain
            .iter()
            .filter(|entry| lines[entry.line].page == lines[candidate.line].page)
            .collect();
        page_labels.push(&candidate);
        page_labels.sort_by_key(|entry| entry.value);
        let mut prior: Option<(usize, f64)> = None;
        let consistent = page_labels.into_iter().all(|entry| {
            let line = &lines[entry.line];
            let rank = (
                usize::from((line.bbox[0] + line.bbox[2]) / 2.0 / line.page_width >= split),
                line.bbox[1],
            );
            let valid = prior
                .is_none_or(|value| rank.0 > value.0 || (rank.0 == value.0 && rank.1 > value.1));
            prior = Some(rank);
            valid
        });
        if consistent {
            let mut candidate = candidate;
            candidate.repaired = true;
            candidate.repair_kind = "out_of_order_label";
            used_lines.insert(candidate.line);
            restored.push(candidate);
        }
    }
    chain.extend(restored);
    chain.sort_by_key(|candidate| candidate.value);
    chain
}

pub(super) fn truncated_value_match(observed: &str, expected: u32) -> bool {
    let Some(digits) = observed
        .chars()
        .map(normalize_decimal_digit)
        .collect::<Option<String>>()
    else {
        return false;
    };
    let expected = expected.to_string();
    digits != expected && (expected.starts_with(&digits) || expected.ends_with(&digits))
}

/// References for the backbone's labels the assignment left without one, each between the
/// references of the labels around it: a reference read with that value, or one a misread glyph,
/// a dropped digit or the window's sole number stands for. A label in the page's note band also
/// takes a number of its value from `expected`, the places the reading passed over: a footnote's on
/// its page or the one before, an endnote's anywhere between its neighbours' references.
pub(super) fn repair_missing_refs(
    chosen: &mut HashMap<u32, Candidate>,
    backbone: &[Candidate],
    refs: &[Candidate],
    expected: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> BTreeMap<String, usize> {
    let mut repair_counts = BTreeMap::<String, usize>::new();
    let labels: HashMap<u32, &Candidate> = backbone
        .iter()
        .filter_map(|candidate| candidate.value.map(|value| (value, candidate)))
        .collect();
    let mut values: Vec<u32> = labels.keys().copied().collect();
    values.sort_unstable();
    let mut taken: HashSet<(usize, usize, usize)> = chosen
        .values()
        .map(|candidate| (candidate.line, candidate.start, candidate.end))
        .collect();
    let label_spans: HashSet<(usize, usize)> = backbone
        .iter()
        .map(|candidate| (candidate.line, candidate.start))
        .collect();
    for (index, value) in values.iter().copied().enumerate() {
        if chosen.contains_key(&value) {
            continue;
        }
        let floor = values[..index]
            .iter()
            .rev()
            .find_map(|prior| chosen.get(prior).map(|candidate| candidate.pos(lines)));
        let ceiling = values[index + 1..]
            .iter()
            .find_map(|next| chosen.get(next).map(|candidate| candidate.pos(lines)));
        let in_window = |candidate: &Candidate| {
            let position = candidate.pos(lines);
            floor.is_none_or(|bound| position > bound)
                && ceiling.is_none_or(|bound| position < bound)
        };
        let label = labels[&value];
        let immediate_window = index > 0
            && index + 1 < values.len()
            && values[index - 1] == value - 1
            && values[index + 1] == value + 1
            && chosen.contains_key(&(value - 1))
            && chosen.contains_key(&(value + 1));
        let substitution_sites: Vec<&Candidate> = if immediate_window {
            refs.iter()
                .filter(|candidate| {
                    candidate.symbol.is_empty()
                        && candidate.value.is_some()
                        && !candidate.requires_visual_cue
                        && !taken.contains(&(candidate.line, candidate.start, candidate.end))
                        && !label_spans.contains(&(candidate.line, candidate.start))
                        && in_window(candidate)
                        && (endnote
                            || lines[label.line].page.abs_diff(lines[candidate.line].page) <= 1)
                        && at_line_end(&lines[candidate.line].text, candidate.end)
                })
                .collect()
        } else {
            Vec::new()
        };
        let sole_substitution = (substitution_sites.len() == 1).then(|| {
            let candidate = substitution_sites[0];
            (candidate.line, candidate.start, candidate.end)
        });
        let mut best: Option<(f64, Candidate, &'static str)> = None;
        for candidate in refs {
            if !candidate.symbol.is_empty()
                || candidate.value.is_none()
                || candidate.requires_visual_cue
                || taken.contains(&(candidate.line, candidate.start, candidate.end))
                || label_spans.contains(&(candidate.line, candidate.start))
                || !in_window(candidate)
                || (!endnote && lines[label.line].page.abs_diff(lines[candidate.line].page) > 1)
            {
                continue;
            }
            let left = candidate
                .start
                .checked_sub(1)
                .and_then(|position| lines[candidate.line].text.chars().nth(position));
            let repair_kind = if candidate.value == Some(value) {
                "window_rescued"
            } else if confusable_variants(&candidate.observed).contains(&value) {
                "confusable_value_repair"
            } else if truncated_value_match(&candidate.observed, value)
                && (chosen.contains_key(&value.saturating_sub(1))
                    || labels.contains_key(&value.saturating_sub(1))
                    || value == values[0])
                && (chosen.contains_key(&(value + 1))
                    || labels.contains_key(&(value + 1))
                    || value == *values.last().unwrap())
                && (at_line_end(&lines[candidate.line].text, candidate.end)
                    || left.is_some_and(|character| "\"'’”)]»".contains(character)))
                && !left.is_some_and(|character| character.is_uppercase())
            {
                "truncated_value_repair"
            } else if sole_substitution == Some((candidate.line, candidate.start, candidate.end)) {
                "sequence_window_substitution_repair"
            } else {
                continue;
            };
            let score = candidate.score - 0.8
                + if lines[candidate.line].page == lines[label.line].page {
                    1.0
                } else {
                    0.0
                };
            if score <= 0.0 || best.as_ref().is_some_and(|prior| prior.0 >= score) {
                continue;
            }
            let mut repaired = candidate.clone();
            if repair_kind != "window_rescued" {
                repaired.value = Some(value);
                repaired.repaired = true;
                repaired.repair_kind = repair_kind;
                repaired.score -= 0.8;
            }
            best = Some((score, repaired, repair_kind));
        }
        if lines[label.line].in_note_band() {
            let label_page = lines[label.line].page;
            for candidate in expected.iter().filter(|candidate| candidate.value == Some(value)) {
                let page = lines[candidate.line].page;
                if taken.contains(&(candidate.line, candidate.start, candidate.end))
                    || label_spans.contains(&(candidate.line, candidate.start))
                    || !in_window(candidate)
                    || (!endnote && page != label_page && page + 1 != label_page)
                {
                    continue;
                }
                let score = candidate.score - 0.8 + if page == label_page { 1.0 } else { 0.0 };
                if score <= 0.0 || best.as_ref().is_some_and(|prior| prior.0 >= score) {
                    continue;
                }
                best = Some((score, candidate.clone(), "expected_reference"));
            }
        }
        if let Some((_, candidate, repair_kind)) = best {
            taken.insert((candidate.line, candidate.start, candidate.end));
            chosen.insert(value, candidate);
            *repair_counts.entry(repair_kind.to_owned()).or_default() += 1;
        }
    }
    repair_counts
}

/// The stray glyphs a line's text could have made of a small raised number: one to three marks
/// glued to the word or punctuation before them and followed by a space or the line's end
/// ("importance.'", "Act,'=", "Trees\""). A lone apostrophe after a letter is elision or a
/// possessive.
pub(super) fn glyph_sites(text: &[char]) -> Vec<(usize, usize)> {
    const GLYPHS: &str = "'’‘\"”“`°=«";
    let mut sites = Vec::new();
    let mut position = 0;
    while position < text.len() {
        if !GLYPHS.contains(text[position]) {
            position += 1;
            continue;
        }
        let start = position;
        while position < text.len() && position - start < 3 && GLYPHS.contains(text[position]) {
            position += 1;
        }
        let end = position;
        let glued = start
            .checked_sub(1)
            .map(|at| text[at])
            .is_some_and(|character| {
                ".,;:)".contains(character)
                    || (character.is_alphabetic()
                        && !(end - start == 1 && "'’‘".contains(text[start])))
            });
        if glued
            && text
                .get(end)
                .is_none_or(|character| character.is_whitespace())
        {
            sites.push((start, end));
        }
    }
    sites
}

/// Recognition reads a small raised number as a quotation mark, apostrophe or other
/// stray glyph ("importance.'", "Act,'=", "Trees\""). Between two paired references,
/// when the run of labels they skip finds exactly as many such glyphs, in order, glued
/// to the text before them, those glyphs are the run's references.
pub(super) fn repair_glyph_refs(
    chosen: &mut HashMap<u32, Candidate>,
    backbone: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> usize {
    let label_lines: HashSet<usize> = backbone.iter().map(|candidate| candidate.line).collect();
    let mut values: Vec<u32> = backbone
        .iter()
        .filter_map(|candidate| candidate.value)
        .collect();
    values.sort_unstable();
    values.dedup();
    let mut repaired = 0;
    let mut index = 0;
    while index < values.len() {
        if chosen.contains_key(&values[index]) {
            index += 1;
            continue;
        }
        let first = index;
        while index < values.len() && !chosen.contains_key(&values[index]) {
            index += 1;
        }
        let missing = &values[first..index];
        let (Some(floor_value), Some(&ceiling_value)) = (
            first.checked_sub(1).map(|prior| values[prior]),
            values.get(index),
        ) else {
            continue;
        };
        if missing.len() > 6
            || missing
                .iter()
                .zip(missing.iter().skip(1))
                .any(|(left, right)| right - left != 1)
            || missing[0] != floor_value + 1
            || ceiling_value != missing[missing.len() - 1] + 1
        {
            continue;
        }
        let floor = chosen[&floor_value].pos(lines);
        let ceiling = chosen[&ceiling_value].pos(lines);
        let mut sites = Vec::new();
        for line_index in floor.0..=ceiling.0.min(lines.len().saturating_sub(1)) {
            let line = &lines[line_index];
            if line.zone != Zone::Body || line.furniture || label_lines.contains(&line_index) {
                continue;
            }
            for (start, end) in glyph_sites(&chars(&line.text)) {
                let site = (line_index, start);
                if site > floor && site < ceiling {
                    sites.push((line_index, start, end));
                }
            }
        }
        if sites.len() != missing.len() {
            continue;
        }
        let label_pages: HashMap<u32, u32> = backbone
            .iter()
            .filter_map(|candidate| Some((candidate.value?, lines[candidate.line].page)))
            .collect();
        if !endnote
            && missing
                .iter()
                .zip(&sites)
                .any(|(value, site)| label_pages[value].abs_diff(lines[site.0].page) > 1)
        {
            continue;
        }
        for (&value, &(line, start, end)) in missing.iter().zip(&sites) {
            chosen.insert(
                value,
                Candidate {
                    line,
                    start,
                    end,
                    observed: chars_slice(&chars(&lines[line].text), start, end),
                    value: Some(value),
                    symbol: String::new(),
                    form: "glyph",
                    score: 0.5,
                    reason: "glyph_sequence_marker",
                    repaired: true,
                    repair_kind: "glyph_sequence_repair",
                    requires_visual_cue: false,
                    flags: CandidateFlags::default(),
                },
            );
            repaired += 1;
        }
    }
    repaired
}

/// A footnote's reference stands on its label's page. On each page, the notes with no reference
/// fall between the references paired there (or the page's ends); where such a stretch of the
/// page's text holds exactly as many places a small raised number could have been misread (a
/// stray glyph, a lone letter glued after punctuation as in "Act.s", or a number no note claimed)
/// as it lacks references, those places are the references, in order.
pub(super) fn repair_page_glyph_refs(
    chosen: &mut HashMap<u32, Candidate>,
    backbone: &[Candidate],
    pool: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> usize {
    if endnote {
        return 0;
    }
    let label_lines: HashSet<usize> = backbone.iter().map(|candidate| candidate.line).collect();
    let mut missing_by_page = BTreeMap::<u32, Vec<u32>>::new();
    for candidate in backbone {
        if let Some(value) = candidate.value.filter(|value| !chosen.contains_key(value)) {
            missing_by_page
                .entry(lines[candidate.line].page)
                .or_default()
                .push(value);
        }
    }
    let taken: HashSet<(usize, usize)> = chosen
        .values()
        .map(|candidate| (candidate.line, candidate.start))
        .collect();
    let mut repaired = 0;
    for (page, mut missing) in missing_by_page {
        missing.sort_unstable();
        missing.dedup();
        let anchors: Vec<(u32, (usize, usize))> = chosen
            .iter()
            .filter(|(_, candidate)| lines[candidate.line].page == page)
            .map(|(value, candidate)| (*value, candidate.pos(lines)))
            .collect();
        // The stretch of the page each missing value's reference must stand in.
        let window = |value: u32| {
            let floor = anchors
                .iter()
                .filter(|(other, _)| *other < value)
                .max_by_key(|(other, _)| *other)
                .map(|(_, position)| *position);
            let ceiling = anchors
                .iter()
                .filter(|(other, _)| *other > value)
                .min_by_key(|(other, _)| *other)
                .map(|(_, position)| *position);
            (floor, ceiling)
        };
        let mut groups = Vec::<((Option<(usize, usize)>, Option<(usize, usize)>), Vec<u32>)>::new();
        for value in missing {
            let bounds = window(value);
            match groups.last_mut() {
                Some((prior, values)) if *prior == bounds => values.push(value),
                _ => groups.push((bounds, vec![value])),
            }
        }
        for ((floor, ceiling), values) in groups {
            let inside = |position: (usize, usize)| {
                floor.is_none_or(|bound| position > bound)
                    && ceiling.is_none_or(|bound| position < bound)
            };
            let mut sites = Vec::<(usize, usize, usize)>::new();
            for (line_index, line) in lines.iter().enumerate() {
                if line.page != page
                    || line.zone != Zone::Body
                    || line.furniture
                    || label_lines.contains(&line_index)
                {
                    continue;
                }
                let text = chars(&line.text);
                let mut found = glyph_sites(&text);
                // A lone letter glued after punctuation and standing before a space.
                for at in 1..text.len() {
                    if ".,;:)".contains(text[at - 1])
                        && text[at].is_alphabetic()
                        && text.get(at + 1).is_none_or(|next| next.is_whitespace())
                        && (at < 2 || !text[at - 2].is_whitespace() && text[at - 2].is_alphabetic())
                    {
                        found.push((at, at + 1));
                    }
                }
                found.extend(
                    pool.iter()
                        .filter(|candidate| {
                            candidate.line == line_index
                                && candidate.value.is_some()
                                && candidate.symbol.is_empty()
                                && !taken.contains(&(candidate.line, candidate.start))
                        })
                        .map(|candidate| (candidate.start, candidate.end)),
                );
                for (start, end) in found {
                    if inside((line.idx, start)) && !taken.contains(&(line_index, start)) {
                        sites.push((line_index, start, end));
                    }
                }
            }
            sites.sort_by_key(|(line, start, _)| (lines[*line].idx, *start));
            sites.dedup_by_key(|(line, start, _)| (*line, *start));
            if sites.len() != values.len() {
                continue;
            }
            for (value, (line, start, end)) in values.into_iter().zip(sites) {
                chosen.insert(
                    value,
                    Candidate {
                        line,
                        start,
                        end,
                        observed: chars_slice(&chars(&lines[line].text), start, end),
                        value: Some(value),
                        symbol: String::new(),
                        form: "glyph",
                        score: 0.5,
                        reason: "page_glyph_marker",
                        repaired: true,
                        repair_kind: "page_glyph_repair",
                        requires_visual_cue: false,
                        flags: CandidateFlags::default(),
                    },
                );
                repaired += 1;
            }
        }
    }
    repaired
}

pub(super) fn rekey_same_value_ref_runs(
    chosen: &mut HashMap<u32, Candidate>,
    backbone: &[Candidate],
    refs: &[Candidate],
    lines: &[PairLine<'_>],
    endnote: bool,
) -> BTreeMap<String, usize> {
    let mut repair_counts = BTreeMap::<String, usize>::new();
    let labels: HashMap<u32, &Candidate> = backbone
        .iter()
        .filter_map(|candidate| candidate.value.map(|value| (value, candidate)))
        .collect();
    let mut values: Vec<u32> = labels.keys().copied().collect();
    values.sort_unstable();
    let mut taken: HashMap<(usize, usize, usize), u32> = chosen
        .iter()
        .map(|(value, candidate)| ((candidate.line, candidate.start, candidate.end), *value))
        .collect();
    for (w_index, w) in values.iter().copied().enumerate() {
        if !chosen.contains_key(&w) {
            continue;
        }
        let mut run = Vec::new();
        let mut cursor = w_index;
        while cursor > 0 {
            let candidate = values[cursor - 1];
            if candidate != w - run.len() as u32 - 1 || chosen.contains_key(&candidate) {
                break;
            }
            run.push(candidate);
            cursor -= 1;
        }
        if run.is_empty() {
            continue;
        }
        run.reverse();
        let floor = if cursor > 0 {
            chosen
                .get(&values[cursor - 1])
                .map(|candidate| candidate.pos(lines))
        } else {
            None
        };
        if floor.is_none() && run[0] != values[0] {
            continue;
        }
        let ceiling = values[w_index + 1..]
            .iter()
            .find_map(|value| chosen.get(value).map(|candidate| candidate.pos(lines)));
        let mut sites = Vec::new();
        let mut valid = true;
        for candidate in refs {
            if candidate.value != Some(w)
                || !candidate.symbol.is_empty()
                || candidate.requires_visual_cue
                || !matches!(candidate.form, "tight" | "sup")
                || candidate.score <= 0.0
                || floor.is_some_and(|bound| candidate.pos(lines) <= bound)
                || ceiling.is_some_and(|bound| candidate.pos(lines) >= bound)
            {
                continue;
            }
            if taken
                .get(&(candidate.line, candidate.start, candidate.end))
                .is_some_and(|owner| *owner != w)
            {
                valid = false;
                break;
            }
            sites.push(candidate.clone());
        }
        if !valid || sites.len() != run.len() + 1 {
            continue;
        }
        sites.sort_by_key(|candidate| candidate.pos(lines));
        let mut targets = run;
        targets.push(w);
        if !endnote
            && targets.iter().zip(&sites).any(|(value, candidate)| {
                lines[labels[value].line]
                    .page
                    .abs_diff(lines[candidate.line].page)
                    > 1
            })
        {
            continue;
        }
        for (value, mut candidate) in targets.into_iter().zip(sites) {
            let key = (candidate.line, candidate.start, candidate.end);
            if chosen
                .get(&value)
                .is_some_and(|selected| (selected.line, selected.start, selected.end) == key)
            {
                continue;
            }
            candidate.repaired = true;
            candidate.repair_kind = "sequence_position_rekey";
            candidate.value = Some(value);
            chosen.insert(value, candidate);
            taken.insert(key, value);
            *repair_counts
                .entry("sequence_position_rekey".to_owned())
                .or_default() += 1;
        }
    }
    repair_counts
}
