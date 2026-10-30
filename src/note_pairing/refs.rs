//! Note references: the sites in a line's text where a reference may stand, scored.

use super::*;

/// Preserve the old extractor's detached-marker evidence when the faster PDF
/// backend emits the same raised run inline. A visibly separated line-final
/// digit is the exact case PyMuPDF exposed as its own row; contiguous native
/// superscripts remain on the monotone pairing path.
pub(super) fn isolated_inline_references(line: &Line) -> Vec<DetachedReference> {
    let text_len = line.text.chars().count();
    line.spans
        .iter()
        .filter_map(|marker| {
            let selected = marker.text.trim();
            if !marker.superscript
                || marker.end != text_len
                || marker.start >= marker.end
                || selected.is_empty()
                || selected.len() > 4
                || !selected.chars().all(|character| character.is_ascii_digit())
            {
                return None;
            }
            let host = line
                .spans
                .iter()
                .filter(|span| !span.superscript && span.end == marker.start)
                .max_by_key(|span| span.end)?;
            let gap = marker.bbox[0] - host.bbox[2];
            (gap >= host.size.max(1.0) * 0.25).then(|| DetachedReference {
                note_id: selected.to_owned(),
                selected_text: selected.to_owned(),
                start_offset: marker.start,
                end_offset: marker.end,
                source_line_id: line.id.clone(),
            })
        })
        .collect()
}

/// What a reference read on the line scores for where the line stands: a running head's or
/// number's line holds one only where it reads as prose. The page's furniture read as body (its
/// running foot and folio at the foot, its line labels) holds none.
pub(super) fn ref_zone_score(line: &PairLine<'_>) -> Option<f64> {
    if line.furniture {
        return None;
    }
    match line.zone {
        Zone::Body => Some(0.6),
        Zone::Title => Some(0.1),
        Zone::Visual | Zone::Note => Some(-0.4),
        Zone::Header | Zone::Number if line.prose_like => Some(-0.5),
        Zone::Header | Zone::Number => None,
    }
}

/// Whether the line's spans set its characters `start..end` apart as a reference's figures are
/// set, even where the PDF does not flag them: spans of their own, without letters, each flagged
/// superscript, smaller than the line's type (`size`) or raised above its baseline.
pub(super) fn raised_or_small(line: &PairLine<'_>, start: usize, end: usize, size: f64) -> bool {
    let height = line.height();
    let mut spans = line.spans.iter().filter(|span| span.start < end && start < span.end).peekable();
    spans.peek().is_some()
        && spans.all(|span| {
            !span.text.chars().any(char::is_alphabetic)
                && (span.superscript
                    || (size > 0.0 && span.size > 0.0 && span.size * 1.25 <= size)
                    || (height > 0.0 && span.bbox[3] <= line.bbox[3] - 0.25 * height))
        })
}

/// The places on a body line where a note's reference stands though its reading passes over
/// them, for a note the page's band holds: a number ending its word, set as a superscript, smaller
/// than the line's type or raised above it, or run on after a word's punctuation
/// ("licensing.59"). Each is its start, its end and whether its type sets it apart.
pub(super) fn expected_sites(
    line: &PairLine<'_>,
    values: &[char],
    digit_runs: &[(usize, usize)],
    superscripts: &[(usize, usize, String)],
) -> Vec<(usize, usize, bool)> {
    let mut size = None;
    let mut sites = Vec::new();
    let tokens = digit_runs
        .iter()
        .filter(|(start, end)| end - start <= 3 && values[*start] != '0')
        .map(|&(start, end)| (start, end, false))
        .chain(superscripts.iter().map(|(start, end, _)| (*start, *end, true)));
    for (start, end, superscript) in tokens {
        let left = start.checked_sub(1).map(|at| values[at]);
        let right = values.get(end).copied();
        if left.is_some_and(|value| value.is_ascii_digit() || is_superscript(value))
            || right.is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        let raised = superscript
            || raised_or_small(line, start, end, *size.get_or_insert_with(|| line_font_size(line.line)));
        let glued = start >= 2
            && left.is_some_and(|value| REF_PUNCTUATION.contains(value) || QUOTES.contains(value))
            && (values[start - 2].is_alphabetic() || ")]”’\"'".contains(values[start - 2]))
            && !(right.is_some_and(|value| ".,:".contains(value)) && values.get(end + 1).is_some_and(char::is_ascii_digit))
            && !abbreviation_pinpoint(&chars_slice(values, 0, start));
        if raised || glued {
            sites.push((start, end, raised));
        }
    }
    sites
}

pub(super) fn ref_left(character: char) -> bool {
    character.is_alphabetic() || REF_PUNCTUATION.contains(character) || QUOTES.contains(character)
}

pub(super) fn ref_right(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\u{00a0}')
        || REF_RIGHT_PUNCTUATION.contains(character)
        || QUOTES.contains(character)
        || DASHES.contains(character)
}

pub(super) fn chars_at_line_end(values: &[char], end: usize) -> bool {
    let tail = values.get(end..).unwrap_or_default();
    let start = tail
        .iter()
        .position(|character| !character.is_whitespace())
        .unwrap_or(tail.len());
    let end = tail
        .iter()
        .rposition(|character| !character.is_whitespace())
        .map_or(start, |index| index + 1);
    tail[start..end]
        .iter()
        .all(|character| REF_PUNCTUATION.contains(*character) || QUOTES.contains(*character))
}

pub(super) fn at_line_end(text: &str, end: usize) -> bool {
    let text = ScalarText::new(text);
    text.slice(end.min(text.len())..text.len())
        .is_some_and(|tail| {
            tail.trim()
                .chars()
                .all(|character| REF_PUNCTUATION.contains(character) || QUOTES.contains(character))
        })
}

pub(super) fn preceding_word(values: &[char], start: usize) -> String {
    let end = start.min(values.len());
    let start = values[..end]
        .iter()
        .rposition(|character| !character.is_ascii_alphabetic())
        .map_or(0, |index| index + 1);
    values[start..end].iter().collect()
}

pub(super) fn spaced_preceding_word(values: &[char], start: usize) -> String {
    let mut end = start.min(values.len());
    while end > 0 && matches!(values[end - 1], ' ' | '\t') {
        end -= 1;
    }
    preceding_word(values, end)
}

pub(super) fn abbreviation_pinpoint(prefix: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:^|[\s(\[])(?:pp?|paras?|ss?|secs?|arts?|vols?|nos?|c|ch|cc|pts?|eds?|figs?|tabs?|apps?|cls?)\.$",
        )
        .unwrap()
    })
    .is_match(prefix)
}

pub(super) fn date_day_site(values: &[char], start: usize, end: usize) -> bool {
    static MONTH_BEFORE: OnceLock<Regex> = OnceLock::new();
    static MONTH_AFTER: OnceLock<Regex> = OnceLock::new();
    static COMMA_YEAR: OnceLock<Regex> = OnceLock::new();
    static DAY_LIST: OnceLock<Regex> = OnceLock::new();
    let month = MONTH;
    let prefix = chars_slice(values, 0, start);
    let tail = chars_slice(values, end, values.len());
    MONTH_BEFORE
        .get_or_init(|| Regex::new(&format!(r"(?i)\b{month}\.?[ \t]+$")).unwrap())
        .is_match(&prefix)
        || MONTH_AFTER
            .get_or_init(|| Regex::new(&format!(r"(?i)^[ \t]+{month}\b[.,]?")).unwrap())
            .is_match(&tail)
        || COMMA_YEAR
            .get_or_init(|| Regex::new(r"^,\s*(?:1[6-9]|20)\d\d\b").unwrap())
            .is_match(&tail)
        || DAY_LIST
            .get_or_init(|| {
                Regex::new(&format!(
                    r"(?i)^\s*\d{{1,2}}(?:\s*,\s*\d{{1,2}})+\s+{month}\b"
                ))
                .unwrap()
            })
            .is_match(&chars_slice(values, start, values.len()))
}

pub(super) fn ref_site_penalty(
    line: &PairLine<'_>,
    values: &[char],
    start: usize,
    end: usize,
    form: &str,
) -> Option<f64> {
    if form == "sup" {
        return Some(0.0);
    }
    // A number in a web address ("ubclawreview/vol50/iss2/6", a repository's stamp) is part of it.
    let word_start = values[..start]
        .iter()
        .rposition(|value| value.is_whitespace())
        .map_or(0, |at| at + 1);
    let word_end = values[end..]
        .iter()
        .position(|value| value.is_whitespace())
        .map_or(values.len(), |at| end + at);
    let word = chars_slice(values, word_start, word_end);
    if word.contains('/') || word.contains("www.") {
        return None;
    }
    let left = start
        .checked_sub(1)
        .and_then(|index| values.get(index))
        .copied();
    let right = values.get(end).copied();
    let mut penalty = 0.0;
    if let Some(right) = right {
        if !ref_right(right) {
            let tail = chars_slice(values, end, (end + 2).min(values.len())).to_lowercase();
            if ["th", "st", "nd", "rd", "am", "pm"].contains(&tail.as_str())
                && values
                    .get(end + 2)
                    .is_none_or(|value| !value.is_alphabetic())
            {
                return None;
            }
            if right.is_uppercase() {
                penalty += 0.4;
            } else if right.is_lowercase()
                && values
                    .get(end + 1)
                    .is_none_or(|value| value.is_whitespace())
            {
                penalty += 0.7;
            } else if right.is_lowercase() {
                penalty += 1.0;
            } else {
                return None;
            }
        }
        if DASHES.contains(right) && values.get(end + 1).is_some_and(char::is_ascii_digit) {
            return None;
        }
        if right == '.' && values.get(end + 1).is_some_and(char::is_ascii_digit) {
            let left_char = left.unwrap_or_default();
            let following_end = values[end + 1..]
                .iter()
                .position(|value| !value.is_ascii_digit())
                .map_or(values.len(), |offset| end + 1 + offset);
            if !(left_char.is_alphabetic()
                && values.get(end + 1..following_end) == values.get(start..end))
            {
                return None;
            }
        }
        // Hours and minutes ("2:47"), or chapter and verse.
        if right == ':' && values.get(end + 1).is_some_and(char::is_ascii_digit) {
            return None;
        }
        if right == ','
            && values
                .get(end + 1..end + 4)
                .is_some_and(|tail| tail.iter().all(char::is_ascii_digit))
            && values
                .get(end + 4)
                .is_none_or(|value| !value.is_ascii_digit())
        {
            return None;
        }
    }
    if left.is_some_and(|value| DASHES.contains(value))
        && start >= 2
        && values[start - 2].is_ascii_digit()
    {
        return None;
    }
    if left == Some('.') && start >= 2 && values[start - 2] == '.' {
        let mut run_start = start - 2;
        while run_start > 0 && values[run_start - 1] == '.' {
            run_start -= 1;
        }
        if run_start == 0 || !values[run_start - 1].is_alphabetic() {
            return None;
        }
        penalty += 0.4;
    }
    if left == Some('.') && start >= 2 && values[start - 2].is_ascii_digit() {
        // A number running on into lower-case words ("rule 4.4 and") is a decimal or
        // section number; a reference after a sentence's final number starts a sentence.
        if values[end..]
            .iter()
            .find(|value| !value.is_whitespace())
            .is_some_and(|value| value.is_lowercase())
        {
            return None;
        }
        penalty += 0.8;
    }
    if values[start.saturating_sub(3)..start].contains(&'$') {
        return None;
    }
    if overlaps(&line.outline_spans, start, end) {
        return None;
    }
    if line.protected(start, end) {
        let tail_glued = left.is_some_and(|value| ".,".contains(value))
            && start >= 2
            && values[start - 2].is_ascii_digit()
            && line
                .citation_spans
                .iter()
                .any(|span| end == span.end && start > span.start);
        if !tail_glued {
            return None;
        }
        penalty += 1.2;
    }
    let word = preceding_word(values, start);
    if counter_noun(&word) {
        if line.zone == Zone::Note {
            return None;
        }
        penalty += 1.1;
    }
    let prefix = chars_slice(values, 0, start);
    if abbreviation_pinpoint(&prefix) {
        return None;
    }
    // A page number or page count ("page 4", "Page 1 of 3").
    static PAGE_COUNTER: OnceLock<Regex> = OnceLock::new();
    if PAGE_COUNTER
        .get_or_init(|| {
            Regex::new(r"(?i)(?:^|[^\p{L}])(?:pages?|p\.|pp\.)\s*(?:\d+\s*(?:of|/|de)\s*)?$")
                .unwrap()
        })
        .is_match(&prefix)
    {
        return None;
    }
    if matches!(form, "spaced_eol" | "spaced_mid")
        && counter_noun(&spaced_preceding_word(values, start))
    {
        return None;
    }
    // A spaced number counts a measure mid-line or at a line's start, and is a date's
    // day wherever it sits.
    if matches!(form, "spaced_mid" | "line_start") {
        static MEASURE: OnceLock<Regex> = OnceLock::new();
        if MEASURE
            .get_or_init(|| Regex::new(r"(?i)^[ \t]+(?:years?|days?|months?|weeks?|hours?|minutes?|seconds?|per|percent|p\.c\b|cents?|dollars?|pounds?|shillings?|pence|feet|foot|acres?|miles?|inches?|yards?|tons?|o.?clock)\b").unwrap())
            .is_match(&chars_slice(values, end, values.len()))
        {
            return None;
        }
    }
    if matches!(form, "spaced_mid" | "spaced_eol" | "line_start")
        && date_day_site(values, start, end)
    {
        return None;
    }
    Some(penalty)
}

pub(super) fn digit_runs(values: &[char]) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < values.len() {
        if !values[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < values.len() && values[index].is_ascii_digit() {
            index += 1;
        }
        if index - start <= 7 {
            result.push((start, index));
        }
    }
    result
}

pub(super) fn ascii_number(values: &[char], start: usize, end: usize) -> u32 {
    values[start..end]
        .iter()
        .fold(0, |number, digit| number * 10 + digit.to_digit(10).unwrap())
}

pub(super) fn superscript_runs(values: &[char]) -> Vec<(usize, usize, String)> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < values.len() {
        if !is_superscript(values[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < values.len() && is_superscript(values[index]) && index - start < 4 {
            index += 1;
        }
        result.push((start, index, values[start..index].iter().collect()));
    }
    result
}

pub(super) fn symbol_runs(values: &[char]) -> Vec<(usize, usize, String)> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < values.len() {
        if !is_symbol(values[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < values.len() && is_symbol(values[index]) && index - start < MAX_SYMBOL_RUN {
            index += 1;
        }
        result.push((start, index, values[start..index].iter().collect()));
    }
    result
}

/// The references read on the document's lines, and the places a reference a band's note expects
/// may stand besides (`expected_sites`), each in line order.
pub(super) fn extract_refs(lines: &[PairLine<'_>]) -> (Vec<Candidate>, Vec<Candidate>) {
    let mut candidates = Vec::new();
    let mut expected = Vec::new();
    let mut previous_body = None;
    let mut previous_page = None;
    let mut previous_by_line = Vec::with_capacity(lines.len());
    static PAREN_REF: OnceLock<Regex> = OnceLock::new();
    static SECTION_BEFORE_PAREN: OnceLock<Regex> = OnceLock::new();
    let paren_ref = PAREN_REF.get_or_init(|| Regex::new(r"\(\s*(\d{1,3})\s*\)").unwrap());
    static PROVIDER_CALL: OnceLock<Regex> = OnceLock::new();
    let provider_call =
        PROVIDER_CALL.get_or_init(|| Regex::new(r"\[(\d{1,3})\]|Footnote (\d{1,3})\b").unwrap());
    let section_before = SECTION_BEFORE_PAREN
        .get_or_init(|| Regex::new(r"(?i)\b(?:ss?|sub-?ss?|arts?|paras?|cls?)\.?\s*$").unwrap());
    // A subsection number after the section number opening its line: "2. (1) The ...".
    static SECTION_NUMBER: OnceLock<Regex> = OnceLock::new();
    let section_number =
        SECTION_NUMBER.get_or_init(|| Regex::new(r"^\s*\d{1,4}[A-Za-z]?\.\s*$").unwrap());
    for (line_index, line) in lines.iter().enumerate() {
        if previous_page != Some(line.page) {
            previous_body = None;
            previous_page = Some(line.page);
        }
        previous_by_line.push(previous_body);
        if line.zone == Zone::Body && !line.text.trim().is_empty() {
            previous_body = Some(line_index);
        }
    }
    for (line_index, line) in lines.iter().enumerate() {
        let zone = ref_zone_score(line);
        // A superscript on a running head's or number's line of six characters or more is read too.
        let sup_zone_score = zone.or_else(|| {
            (!line.furniture && matches!(line.zone, Zone::Header | Zone::Number) && line.text.trim().chars().count() >= 6)
                .then_some(0.1)
        });
        let Some(sup_zone_score) = sup_zone_score else {
            continue;
        };
        let label_end = if line.zone == Zone::Note {
            label_token(&line.text, provider_text(line)).map_or(0, |token| token.match_end)
        } else {
            0
        };
        let line_chars = chars(&line.text);
        // A provider's flattened text runs a note's call into the word before it: "counsel[1] for",
        // "appeal.[1] The", "job.Footnote 1 The".
        // Its calls take no other form: no superscript survives the flattening, and its digits are text.
        if provider_text(line) {
            let Some(zone_score) = zone else {
                continue;
            };
            for captures in provider_call.captures_iter(&line.text) {
                let whole = captures.get(0).expect("provider call");
                let digits = captures.get(1).or_else(|| captures.get(2)).expect("call digits");
                let open = line.text[..whole.start()].chars().count();
                let start = line.text[..digits.start()].chars().count();
                let end = line.text[..digits.end()].chars().count();
                // A bracketed call may follow a number: a citation's page runs into it ("2011 SKCA 136[1]");
                // a year in brackets has four digits, which no call has.
                let left = line_chars.get(open.wrapping_sub(1)).copied();
                if open == 0
                    || start < label_end
                    || !left.is_some_and(|left| ref_left(left) || left.is_ascii_digit() && whole.as_str().starts_with('['))
                    || line.protected(start, end)
                {
                    continue;
                }
                let Some(value) = numeric_value(digits.as_str()) else {
                    continue;
                };
                candidates.push(Candidate {
                    line: line_index,
                    start: open,
                    end: line.text[..whole.end()].chars().count(),
                    observed: digits.as_str().to_owned(),
                    value: Some(value),
                    symbol: String::new(),
                    form: "paren",
                    score: zone_score + 0.4,
                    reason: "provider_call",
                    repaired: false,
                    repair_kind: "",
                    requires_visual_cue: false,
                    flags: CandidateFlags {
                        paren_ref: true,
                        ..CandidateFlags::default()
                    },
                });
            }
            continue;
        }
        let trimmed_len = line_chars
            .iter()
            .rposition(|character| !character.is_whitespace())
            .map_or(0, |index| index + 1);
        let digit_runs = digit_runs(&line_chars);
        let superscripts = superscript_runs(&line_chars);
        let mut seen = HashSet::new();
        for (start, end, observed) in &superscripts {
            let (start, end) = (*start, *end);
            if start < label_end || line.protected(start, end) {
                continue;
            }
            let Some(value) = numeric_value(observed) else {
                continue;
            };
            seen.insert((start, end));
            candidates.push(Candidate {
                line: line_index,
                start,
                end,
                observed: observed.clone(),
                value: Some(value),
                symbol: String::new(),
                form: "sup",
                score: 1.6 + sup_zone_score,
                reason: "superscript_marker",
                repaired: false,
                repair_kind: "",
                requires_visual_cue: line.zone == Zone::Visual,
                flags: CandidateFlags::default(),
            });
        }
        for &(start, end) in &line.native_superscript_spans {
            // A raised native call can follow a citation whose lexical span runs over it.
            if seen.contains(&(start, end))
                || start < label_end
                || overlaps(&line.outline_spans, start, end)
            {
                continue;
            }
            let observed = chars_slice(&line_chars, start, end);
            let value = numeric_value(&observed);
            let symbol = value.map_or_else(
                || {
                    if (1..=MAX_SYMBOL_RUN).contains(&observed.chars().count())
                        && observed.chars().all(is_symbol)
                    {
                        normalize_symbol(&observed)
                    } else {
                        String::new()
                    }
                },
                |_| String::new(),
            );
            if value.is_none() && symbol.is_empty() {
                continue;
            };
            seen.insert((start, end));
            candidates.push(Candidate {
                line: line_index,
                start,
                end,
                observed,
                value,
                symbol,
                form: if value.is_some() { "sup" } else { "symbol" },
                score: 1.6 + sup_zone_score,
                reason: "native_superscript_span",
                repaired: false,
                repair_kind: "",
                requires_visual_cue: line.zone == Zone::Visual,
                flags: CandidateFlags::default(),
            });
        }
        let Some(zone_score) = zone else {
            continue;
        };
        if line.zone == Zone::Body {
            for (start, end, raised) in expected_sites(line, &line_chars, &digit_runs, &superscripts) {
                let observed = chars_slice(&line_chars, start, end);
                let Some(value) = numeric_value(&observed) else {
                    continue;
                };
                expected.push(Candidate {
                    line: line_index,
                    start,
                    end,
                    observed,
                    value: Some(value),
                    symbol: String::new(),
                    form: if raised { "sup" } else { "tight" },
                    score: zone_score + if raised { 1.6 } else { 0.9 },
                    reason: "expected_reference",
                    repaired: false,
                    repair_kind: "",
                    requires_visual_cue: false,
                    flags: CandidateFlags::default(),
                });
            }
        }
        let stripped = line.text.trim();
        if stripped.chars().all(|character| character.is_ascii_digit())
            && (1..=3).contains(&stripped.chars().count())
            && line.zone == Zone::Body
            // A folio's place at the page's top; a line without a box has none.
            && !(line.bbox[3] > 0.0
                && line.page_height > 0.0
                && (line.bbox[1] + line.bbox[3]) / 2.0 / line.page_height <= 0.05)
        {
            if let Some(value) = numeric_value(stripped) {
                let start = line
                    .text
                    .chars()
                    .position(|character| character.is_ascii_digit())
                    .unwrap_or(0);
                candidates.push(Candidate {
                    line: line_index,
                    start,
                    end: start + stripped.chars().count(),
                    observed: stripped.to_owned(),
                    value: Some(value),
                    symbol: String::new(),
                    form: "standalone",
                    score: 0.5 + zone_score,
                    reason: "standalone_marker_line",
                    repaired: false,
                    repair_kind: "",
                    requires_visual_cue: false,
                    flags: CandidateFlags::default(),
                });
                continue;
            }
        }
        for &(start, end) in digit_runs
            .iter()
            .filter(|(start, end)| (1..=3).contains(&(end - start)))
        {
            // A note is never numbered with a leading zero ("2023.11.05", "F28.02A").
            if seen.contains(&(start, end))
                || start < label_end
                || (end - start > 1 && line_chars[start] == '0')
            {
                continue;
            }
            let value = ascii_number(&line_chars, start, end);
            if !(1..=MAX_VALUE).contains(&value) {
                continue;
            }
            let left = start
                .checked_sub(1)
                .and_then(|index| line_chars.get(index))
                .copied();
            let at_eol = chars_at_line_end(&line_chars, end);
            let form = if start == 0 {
                let previous = previous_by_line[line_index].map(|index| &lines[index]);
                let follow_ok = line_chars
                    .get(end)
                    .is_none_or(|value| value.is_whitespace());
                if line.zone == Zone::Note
                    || !follow_ok
                    || previous.is_none_or(|value| {
                        value
                            .text
                            .trim_end()
                            .ends_with(|value| TERMINAL_PUNCTUATION.contains(value))
                    })
                {
                    continue;
                }
                "line_start"
            } else if left.is_some_and(ref_left) {
                "tight"
            } else if left.is_some_and(char::is_whitespace) && at_eol && line.zone != Zone::Note {
                "spaced_eol"
            } else if left.is_some_and(char::is_whitespace) && line.zone == Zone::Body {
                "spaced_mid"
            } else if left.is_some_and(|value| value.is_lowercase()) {
                let word = preceding_word(&line_chars, start);
                if word.len() < 3
                    || counter_noun(&word)
                    || line_chars.get(end).is_some_and(|value| !ref_right(*value))
                {
                    continue;
                }
                "letter_glued"
            } else {
                continue;
            };
            let Some(penalty) = ref_site_penalty(line, &line_chars, start, end, form) else {
                continue;
            };
            let mut score = zone_score - penalty
                + match form {
                    "tight" => 0.9,
                    "line_start" => -0.3,
                    "spaced_eol" => 0.1,
                    "spaced_mid" | "letter_glued" => -0.6,
                    _ => 0.0,
                };
            if end == trimmed_len {
                score += 0.15;
            }
            if matches!(line.zone, Zone::Title | Zone::Visual) && form != "tight" {
                continue;
            }
            candidates.push(Candidate {
                line: line_index,
                start,
                end,
                observed: chars_slice(&line_chars, start, end),
                value: Some(value),
                symbol: String::new(),
                form,
                score,
                reason: match form {
                    "tight" => "attached_digit_marker",
                    "line_start" => "line_start_marker",
                    "spaced_eol" => "spaced_end_of_line_marker",
                    "spaced_mid" => "spaced_mid_line_marker",
                    _ => "word_glued_marker",
                },
                repaired: false,
                repair_kind: "",
                requires_visual_cue: line.zone == Zone::Visual,
                flags: CandidateFlags::default(),
            });
        }
        for &(start, end) in digit_runs
            .iter()
            .filter(|(start, end)| (5..=7).contains(&(end - start)))
        {
            if start < label_end || matches!(line.zone, Zone::Title | Zone::Visual) {
                continue;
            }
            let year = ascii_number(&line_chars, start, start + 4);
            let tail = ascii_number(&line_chars, start + 4, end);
            let left = start
                .checked_sub(1)
                .and_then(|index| line_chars.get(index))
                .copied();
            if !(1600..=2069).contains(&year)
                || tail == 0
                || left.is_some_and(|value| !value.is_whitespace() && !ref_left(value))
                || line_chars[start.saturating_sub(3)..start].contains(&'$')
            {
                continue;
            }
            let sub_start = start + 4;
            let Some(penalty) = ref_site_penalty(line, &line_chars, sub_start, end, "tight") else {
                continue;
            };
            let mut score = zone_score - penalty;
            if end == trimmed_len {
                score += 0.15;
            }
            candidates.push(Candidate {
                line: line_index,
                start: sub_start,
                end,
                observed: chars_slice(&line_chars, sub_start, end),
                value: Some(tail),
                symbol: String::new(),
                form: "year_glued",
                score,
                reason: "year_glued_marker",
                repaired: false,
                repair_kind: "",
                requires_visual_cue: false,
                flags: CandidateFlags::default(),
            });
        }
        for captures in paren_ref.captures_iter(&line.text) {
            let whole = captures.get(0).expect("paren ref");
            let digits = captures.get(1).expect("paren digits");
            let open = line.text[..whole.start()].chars().count();
            let start = line.text[..digits.start()].chars().count();
            let end = line.text[..digits.end()].chars().count();
            if start < label_end
                || (open > 0 && line_chars[open - 1].is_alphanumeric())
                || section_before.is_match(&char_slice(&line.text, 0, open))
                || section_number.is_match(&char_slice(&line.text, 0, open))
                || line.protected(start, end)
            {
                continue;
            }
            let Some(value) = numeric_value(digits.as_str()) else {
                continue;
            };
            let Some(penalty) = ref_site_penalty(line, &line_chars, start, end, "tight") else {
                continue;
            };
            candidates.push(Candidate {
                line: line_index,
                start,
                end,
                observed: digits.as_str().to_owned(),
                value: Some(value),
                symbol: String::new(),
                form: "paren",
                score: zone_score - penalty + 0.4,
                reason: "paren_marker",
                repaired: false,
                repair_kind: "",
                requires_visual_cue: line.zone == Zone::Visual,
                flags: CandidateFlags {
                    paren_ref: true,
                    ..CandidateFlags::default()
                },
            });
        }
        if line.zone != Zone::Note {
            for (start, end, observed) in symbol_runs(&line_chars) {
                if start == 0
                    || seen.contains(&(start, end))
                    || start < label_end
                    || line.protected(start, end)
                {
                    continue;
                }
                let left = line_chars[start - 1];
                let mut spaced = left.is_whitespace() && line.zone == Zone::Title;
                if left.is_whitespace() && line.zone == Zone::Body && !spaced {
                    let prefix = chars_slice(&line_chars, 0, start);
                    let tail = chars_slice(&line_chars, end, line_chars.len());
                    let run_head = prefix
                        .trim_end()
                        .chars()
                        .last()
                        .is_none_or(|character| !is_symbol(character));
                    let starts_with_run =
                        line.text.trim_start().chars().next().is_some_and(is_symbol);
                    let more_symbols = tail.trim_start().chars().next().is_some_and(is_symbol);
                    let prose = line_chars
                        .iter()
                        .filter(|character| character.is_alphabetic())
                        .count()
                        >= 12;
                    spaced = run_head && !starts_with_run && more_symbols && prose;
                }
                let glued_byline = left.is_alphabetic() && line.zone == Zone::Title;
                if !ref_left(left) && !left.is_ascii_digit() && !spaced && !glued_byline {
                    continue;
                }
                if line_chars.get(end).is_some_and(|right| !ref_right(*right)) {
                    continue;
                }
                candidates.push(Candidate {
                    line: line_index,
                    start,
                    end,
                    observed: observed.clone(),
                    value: None,
                    symbol: normalize_symbol(&observed),
                    form: "symbol",
                    score: 0.9 + zone_score - if spaced { 0.3 } else { 0.0 },
                    reason: "attached_symbol_marker",
                    repaired: false,
                    repair_kind: "",
                    requires_visual_cue: line.zone == Zone::Visual,
                    flags: CandidateFlags::default(),
                });
            }
        }
    }
    (candidates, expected)
}
