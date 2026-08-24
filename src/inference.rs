use super::*;
use std::{
    collections::{hash_map::Entry, BTreeMap},
    sync::Arc,
};

macro_rules! cached_regex {
    ($name:ident, $pattern:expr) => {{
        static $name: OnceLock<Regex> = OnceLock::new();
        $name.get_or_init(|| Regex::new($pattern).unwrap())
    }};
}

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(usize)]
enum MarkerStyle {
    Bracket,
    Dot,
    Bare,
}

#[derive(Clone)]
struct Marker {
    number: u32,
    start: usize,
    content_start: usize,
    style: MarkerStyle,
    score: f64,
    formal: bool,
    sentence: bool,
}

#[derive(Clone, Copy)]
struct Line<'a> {
    byte_start: usize,
    byte_end: usize,
    scalar_start: usize,
    text: &'a str,
}

fn lines<'a>(text: &'a ScalarText<'a>) -> impl Iterator<Item = Line<'a>> + 'a {
    text.lines().iter().map(move |line| Line {
        byte_start: line[0],
        byte_end: line[1],
        scalar_start: line[2],
        text: &text.value[line[0]..line[1]],
    })
}

fn javascript_lines<'a>(text: &'a ScalarText<'a>) -> Vec<Line<'a>> {
    // This is JavaScript regexp line segmentation, not coordinate conversion:
    // CRLF is one break while its two source scalars remain counted.
    let mut result = Vec::new();
    let mut chars = text.value.char_indices().peekable();
    let (mut byte_start, mut scalar_start, mut scalar) = (0, 0, 0);
    while let Some((byte, character)) = chars.next() {
        if matches!(character, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
            result.push(Line {
                byte_start,
                byte_end: byte,
                scalar_start,
                text: &text.value[byte_start..byte],
            });
            scalar += 1;
            let mut next_byte = byte + character.len_utf8();
            if character == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
                let (next, _) = chars.next().unwrap();
                next_byte = next + 1;
                scalar += 1;
            }
            byte_start = next_byte;
            scalar_start = scalar;
        } else {
            scalar += 1;
        }
    }
    result.push(Line {
        byte_start,
        byte_end: text.value.len(),
        scalar_start,
        text: &text.value[byte_start..],
    });
    result
}

fn leading_ascii_space(value: &str) -> usize {
    value
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()
}

fn decimal_prefix(value: &str, maximum: usize) -> Option<(&str, usize)> {
    let length = value.bytes().take_while(u8::is_ascii_digit).count();
    (length > 0 && length <= maximum).then(|| (&value[..length], length))
}

fn paragraph_markers(text: &ScalarText<'_>, contiguous: bool) -> Vec<Marker> {
    let mut result = Vec::new();
    for line in lines(text) {
        let lead = leading_ascii_space(line.text);
        let value = &line.text[lead..];
        let start = line.scalar_start;
        let basic = if let Some(rest) = value.strip_prefix('[') {
            decimal_prefix(rest, 4).and_then(|(number, length)| {
                (rest.as_bytes().get(length) == Some(&b']'))
                    .then(|| (number, MarkerStyle::Bracket, length + 2))
            })
        } else {
            decimal_prefix(value, 4).and_then(|(number, length)| {
                let rest = &value[length..];
                if rest.starts_with('.')
                    && (rest[1..].chars().next().is_some_and(char::is_whitespace)
                        || (rest.len() == 1 && line.byte_end < text.value.len()))
                {
                    Some((number, MarkerStyle::Dot, length + 1))
                } else if contiguous
                    && rest.starts_with('.')
                    && rest[1..].chars().next().is_some_and(char::is_uppercase)
                {
                    Some((number, MarkerStyle::Dot, length + 1))
                } else if rest.chars().next().is_some_and(char::is_whitespace)
                    || (rest.is_empty() && line.byte_end < text.value.len())
                {
                    Some((
                        number,
                        if contiguous {
                            MarkerStyle::Dot
                        } else {
                            MarkerStyle::Bare
                        },
                        length,
                    ))
                } else {
                    None
                }
            })
        };
        if let Some((number, style, marker_end)) = basic {
            let content = marker_end + leading_ascii_space(&value[marker_end..]);
            result.push(Marker {
                number: number.parse().unwrap(),
                start,
                content_start: line.scalar_start + lead + content,
                style,
                score: 1.0,
                formal: false,
                sentence: false,
            });
        }
        {
            let glyph = value
                .chars()
                .next()
                .filter(|value| matches!(value, '¶' | '\u{95}' | '•'));
            if let Some(glyph) = glyph {
                let rest = value[glyph.len_utf8()..].trim_start_matches([' ', '\t']);
                if let Some((number, length)) = decimal_prefix(rest, 4) {
                    let after = rest[length..].chars().next();
                    if after.is_none_or(|value| value.is_whitespace() || ".,;:—-".contains(value))
                    {
                        result.push(Marker {
                            number: number.parse().unwrap(),
                            start,
                            content_start: text
                                .scalar(line.byte_start + line.text.len() - rest.len() + length),
                            style: MarkerStyle::Dot,
                            score: 1.0,
                            formal: false,
                            sentence: false,
                        });
                    }
                }
            }
        }
    }
    result
}

fn marker_visible(marker: &Marker, excluded: &[ScalarRange]) -> bool {
    !excluded
        .iter()
        .any(|range| range.start <= marker.start && marker.start < range.end)
}

fn word_count(value: &str, letters_only: bool) -> usize {
    let mut count = 0;
    let mut inside = false;
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        let member = if letters_only {
            character.is_alphabetic()
        } else {
            character.is_alphanumeric()
        };
        if member {
            count += usize::from(!inside);
            inside = true;
        } else if matches!(character, '\'' | '’')
            && inside
            && characters.peek().is_some_and(|next| {
                if letters_only {
                    next.is_alphabetic()
                } else {
                    next.is_alphanumeric()
                }
            })
        {
            continue;
        } else {
            inside = false;
        }
    }
    count
}

fn median(values: &mut [usize]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle] as f64
    } else {
        (values[middle - 1] + values[middle]) as f64 / 2.0
    }
}

fn heading_enumerator(value: &str) -> bool {
    cached_regex!(VALUE, r"^(?:\([\p{L}\p{N}]{1,5}\)|\p{L}[.)]|[IVXLCDM]{1,4}[.)]|[ivxlcdm]{1,4}[.)]|\d{1,3}(?:\.\d{1,3})*[.)])$").is_match(value)
}

fn level_opens(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|character| character.is_uppercase() || character.is_numeric())
}

fn heading_level(words: &[&str], enumerated: bool) -> bool {
    if words.is_empty() || words.len() > 12 {
        return false;
    }
    if words.len() == 1 && heading_enumerator(words[0]) {
        return true;
    }
    let last = words.last().unwrap();
    if !level_opens(words[0]) || last.ends_with(['.', ',', ';']) {
        return false;
    }
    if last.ends_with(['?', ':']) {
        return true;
    }
    let title = words.iter().all(|word| {
        let (length, first) = word.chars().filter(|value| value.is_alphabetic()).fold(
            (0, None),
            |(length, first), character| {
                (length + character.len_utf16(), first.or(Some(character)))
            },
        );
        length < 4 || first.is_some_and(char::is_uppercase)
    });
    title || enumerated || words.len() <= 6
}

fn trim_leading_parenthetical(value: &str) -> &str {
    let value = value.trim();
    let Some(rest) = value.strip_prefix('(') else {
        return value;
    };
    let Some(close) = rest.find(')') else {
        return value;
    };
    if !rest[..close].is_empty()
        && rest[..close].chars().all(char::is_alphanumeric)
        && rest[close + 1..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    {
        rest[close + 1..].trim_start()
    } else {
        value
    }
}

pub(super) fn formal_heading(value: &str) -> bool {
    let heading = trim_leading_parenthetical(value);
    if heading.is_empty()
        || utf16_len(heading) > 120
        || heading.chars().any(|value| ";![]{}".contains(value))
    {
        return false;
    }
    let words = heading.split_whitespace().collect::<Vec<_>>();
    let mut start = 0;
    let mut enumerated = false;
    for (index, word) in words.iter().enumerate() {
        let opener = words
            .get(index + 1)
            .is_some_and(|next| !heading_enumerator(next) && level_opens(next));
        if heading_enumerator(word) && opener {
            if start < index && !heading_level(&words[start..index], enumerated) {
                return false;
            }
            start = index + 1;
            enumerated = true;
        }
    }
    heading_level(&words[start..], enumerated)
}

fn sentence_heading(value: &str, following: &str) -> bool {
    let heading = trim_leading_parenthetical(value);
    let mut words = heading.split_whitespace();
    let word_count = words.clone().count();
    utf16_len(heading) <= 120
        && (4..=18).contains(&word_count)
        && heading.chars().next().is_some_and(char::is_uppercase)
        && words.any(|word| word.chars().next().is_some_and(char::is_lowercase))
        && !heading.chars().any(|value| "[].,;:!?".contains(value))
        && following
            .trim_start()
            .chars()
            .next()
            .is_some_and(char::is_uppercase)
}

fn heading_joined(text: &ScalarText<'_>, known: [Option<&HashSet<usize>>; 2]) -> [Vec<Marker>; 2] {
    let mut result = std::array::from_fn(|_| Vec::new());
    for line in lines(text) {
        let bytes = line.text.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            let (style, index, digits) = match bytes[at] {
                b'[' if known[0].is_some() => (MarkerStyle::Bracket, 0, at + 1),
                byte if byte.is_ascii_digit() && known[1].is_some() => (MarkerStyle::Dot, 1, at),
                _ => {
                    at += 1;
                    continue;
                }
            };
            let length = bytes[digits..]
                .iter()
                .take(4)
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if length == 0 {
                at += 1;
                continue;
            }
            let tail = digits + length;
            let end = if style == MarkerStyle::Bracket {
                (bytes.get(tail) == Some(&b']')).then_some(tail + 1)
            } else if bytes.get(tail) == Some(&b'.') {
                let after = tail + 1;
                if after == bytes.len() {
                    Some(after)
                } else {
                    line.text[after..]
                        .chars()
                        .next()
                        .filter(|character| character.is_whitespace())
                        .map(|character| after + character.len_utf8())
                }
            } else {
                None
            };
            let Some(end) = end else {
                at += 1;
                continue;
            };
            let start = text.scalar(line.byte_start + at);
            if known[index].is_some_and(|known| known.contains(&start)) {
                at = end;
                continue;
            }
            let heading = &line.text[..at];
            let formal = formal_heading(heading)
                && (style == MarkerStyle::Bracket || !heading.contains('.'));
            let sentence = style == MarkerStyle::Bracket
                && sentence_heading(heading, &text.value[line.byte_start + end..]);
            if formal || sentence {
                result[index].push(Marker {
                    number: line.text[digits..tail].parse().unwrap(),
                    start,
                    content_start: text.scalar(line.byte_start + end),
                    style,
                    score: if formal { 0.6 } else { 0.35 },
                    formal,
                    sentence,
                });
            }
            at = end;
        }
    }
    result
}

fn rooted_chain(candidates: &[Marker]) -> (Vec<Marker>, f64) {
    let selected = select_numeric_sequence(
        candidates
            .iter()
            .enumerate()
            .map(|(index, marker)| NumericSequenceCandidate {
                index,
                value: marker.number,
                position: (marker.start, 0),
                page: 0,
                score: marker.score,
                start_supported: false,
            })
            .collect(),
        NumericSequencePolicy::RootedConsecutive,
    );
    (
        selected
            .indices
            .into_iter()
            .map(|index| candidates[index].clone())
            .collect(),
        selected.score,
    )
}

fn sole_chain(chain: &[Marker], candidates: &[Marker]) -> bool {
    let claimed = chain
        .iter()
        .map(|value| value.start)
        .collect::<HashSet<_>>();
    let last = chain.last().map_or(0, |value| value.number);
    let mut rest = candidates
        .iter()
        .filter(|value| !claimed.contains(&value.start))
        .collect::<Vec<_>>();
    rest.sort_by_key(|value| value.start);
    !rest
        .iter()
        .any(|value| (1..=last + 1).contains(&value.number))
        && rest.windows(2).all(|pair| pair[1].number <= pair[0].number)
}

fn endnote_shaped(text: &ScalarText<'_>, chain: &[Marker]) -> bool {
    let length = text.utf16_len();
    chain.len() >= 8
        && length > 0
        && chain
            .iter()
            .filter(|value| text.utf16(value.start) as f64 > length as f64 * 0.75)
            .count() as f64
            / chain.len() as f64
            >= 0.7
}

fn monotone_scopes<'a>(
    markers: impl IntoIterator<Item = &'a Marker>,
    max_gap: u32,
) -> Vec<Vec<Marker>> {
    let mut scopes = Vec::<Vec<Marker>>::new();
    let mut by_last = HashMap::<u32, Vec<usize>>::new();
    for marker in markers {
        let index = (marker.number.saturating_sub(max_gap)..marker.number)
            .flat_map(|value| by_last.get(&value).into_iter().flatten().copied())
            .reduce(|best, current| {
                let left = scopes[current][0].number;
                let right = scopes[best][0].number;
                if left < right || (left == right && current < best) {
                    current
                } else {
                    best
                }
            })
            .unwrap_or(scopes.len());
        if index == scopes.len() {
            scopes.push(vec![marker.clone()]);
        } else {
            let previous = scopes[index].last().unwrap().number;
            if let Some(values) = by_last.get_mut(&previous) {
                values.retain(|value| *value != index);
            }
            scopes[index].push(marker.clone());
        }
        by_last.entry(marker.number).or_default().push(index);
    }
    scopes
}

fn contiguous_scopes(markers: &[Marker]) -> Vec<Vec<Marker>> {
    let mut scopes: Vec<Vec<Marker>> = Vec::new();
    for marker in markers {
        if scopes
            .last()
            .and_then(|scope| scope.last())
            .is_some_and(|prior| marker.number == prior.number + 1)
        {
            scopes.last_mut().unwrap().push(marker.clone());
        } else {
            scopes.push(vec![marker.clone()]);
        }
    }
    scopes
}

fn next_boundary(boundaries: &[usize], start: usize, end: usize) -> usize {
    boundaries
        .get(boundaries.partition_point(|boundary| *boundary <= start))
        .copied()
        .unwrap_or(end)
}

pub(super) fn raw_numeric_runs(text: &ScalarText<'_>) -> Vec<StructureCandidateRun> {
    let all = paragraph_markers(text, false);
    let mut boundaries = all.iter().map(|marker| marker.start).collect::<Vec<_>>();
    boundaries.push(text.len());
    let mut runs = Vec::new();
    for style in [MarkerStyle::Bracket, MarkerStyle::Dot, MarkerStyle::Bare] {
        for scope in monotone_scopes(all.iter().filter(|marker| marker.style == style), 8) {
            if scope.len() < 2 {
                continue;
            }
            let candidates = scope
                .iter()
                .map(|marker| {
                    let end = next_boundary(&boundaries, marker.start, text.len());
                    let surface_label = text
                        .slice(marker.start..marker.content_start)
                        .expect("numeric marker range is bounded")
                        .trim()
                        .to_owned();
                    StructureMarkerCandidate {
                        id: String::new(),
                        range: ScalarRange {
                            start: marker.start,
                            end,
                        },
                        marker_range: ScalarRange {
                            start: marker.start,
                            end: marker.content_start,
                        },
                        label: surface_label,
                        grammar_value: marker.number.to_string(),
                        parent_candidate_id: None,
                        level: 0,
                        content_start: marker.content_start,
                    }
                })
                .collect::<Vec<_>>();
            let range = ScalarRange {
                start: candidates[0].range.start,
                end: candidates.last().unwrap().range.end,
            };
            runs.push(StructureCandidateRun {
                id: String::new(),
                grammar: CandidateGrammar::Numeric,
                range,
                rooted: scope[0].number == 1,
                consecutive: scope
                    .windows(2)
                    .all(|pair| pair[1].number == pair[0].number + 1),
                markers: candidates,
            });
        }
    }
    runs.sort_by_key(|run| (run.range.start, run.range.end));
    for (run_index, run) in runs.iter_mut().enumerate() {
        run.id = format!("numeric-{:06}", run_index + 1);
        for (marker_index, marker) in run.markers.iter_mut().enumerate() {
            marker.id = format!("{}-{:04}", run.id, marker_index + 1);
        }
    }
    runs
}

pub(super) fn raw_enumerator_runs(text: &ScalarText<'_>) -> Vec<StructureCandidateRun> {
    struct RawEnumerator {
        value: u32,
        start: usize,
        content_start: usize,
    }

    let mut by_family = BTreeMap::<u8, Vec<RawEnumerator>>::new();
    for line in lines(text) {
        let trimmed = line.text.trim_start_matches(instrument_space);
        let trimmed_byte = line.byte_start + line.text.len() - trimmed.len();
        let start = text.scalar(trimmed_byte);
        let Some((token, at)) = instrument_marker(trimmed, true, true) else {
            continue;
        };
        let content_start = text.scalar(trimmed_byte + at);
        for (family, value) in enum_readings(token).into_iter().flatten() {
            let Ok(value) = value.parse::<u32>() else {
                continue;
            };
            by_family.entry(family).or_default().push(RawEnumerator {
                value,
                start,
                content_start,
            });
        }
    }
    let mut boundaries = by_family
        .values()
        .flatten()
        .map(|marker| marker.start)
        .chain([text.len()])
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut runs = Vec::new();
    for (family, markers) in by_family {
        let mut scopes = Vec::<Vec<RawEnumerator>>::new();
        for marker in markers {
            let target = scopes
                .iter()
                .enumerate()
                .filter(|(_, scope)| {
                    scope.last().is_some_and(|prior| {
                        prior.value < marker.value && marker.value - prior.value <= 8
                    })
                })
                .max_by_key(|(_, scope)| scope.last().unwrap().value)
                .map(|(index, _)| index);
            if marker.value == 1 || target.is_none() {
                scopes.push(vec![marker]);
            } else {
                scopes[target.unwrap()].push(marker);
            }
        }
        for scope in scopes.into_iter().filter(|scope| scope.len() >= 2) {
            let mut candidates = scope
                .iter()
                .map(|marker| {
                    let end = next_boundary(&boundaries, marker.start, text.len());
                    let surface_label = text
                        .slice(marker.start..marker.content_start)
                        .expect("enumerator marker range is bounded")
                        .trim()
                        .to_owned();
                    StructureMarkerCandidate {
                        id: String::new(),
                        range: ScalarRange {
                            start: marker.start,
                            end,
                        },
                        marker_range: ScalarRange {
                            start: marker.start,
                            end: marker.content_start,
                        },
                        label: surface_label,
                        grammar_value: format!("{family}:{}", marker.value),
                        parent_candidate_id: None,
                        level: 0,
                        content_start: marker.content_start,
                    }
                })
                .collect::<Vec<_>>();
            let range = ScalarRange {
                start: candidates[0].range.start,
                end: candidates.last().unwrap().range.end,
            };
            let ordinal = runs.len() + 1;
            for (index, candidate) in candidates.iter_mut().enumerate() {
                candidate.id = format!("enumerator-{ordinal:06}-{:04}", index + 1);
            }
            runs.push(StructureCandidateRun {
                id: format!("enumerator-{ordinal:06}"),
                grammar: CandidateGrammar::Enumerator,
                range,
                rooted: scope[0].value == 1,
                consecutive: scope
                    .windows(2)
                    .all(|pair| pair[1].value == pair[0].value + 1),
                markers: candidates,
            });
        }
    }
    runs.sort_by_key(|run| (run.range.start, run.range.end));
    runs
}

fn recover_contiguous(
    text: &ScalarText<'_>,
    line: Vec<Marker>,
    candidates: &[Marker],
) -> Vec<Marker> {
    if line.is_empty() {
        return line;
    }
    let within = |number: u32, from: usize, to: usize, formal: bool, sentence: bool| {
        let start = candidates.partition_point(|value| value.start <= from);
        let end = candidates.partition_point(|value| value.start < to);
        let mut matching = candidates[start..end].iter().filter(|value| {
            value.number == number && ((!formal || value.formal) && (!sentence || value.sentence))
        });
        let candidate = matching.next()?;
        matching.next().is_none().then_some(candidate)
    };
    let mut recovered = HashMap::<usize, Marker>::new();
    for pair in line.windows(2) {
        if pair[0].number >= pair[1].number {
            continue;
        }
        let mut found = Vec::new();
        for number in pair[0].number + 1..pair[1].number {
            let Some(candidate) = within(number, pair[0].start, pair[1].start, true, false) else {
                found.clear();
                break;
            };
            found.push(candidate);
        }
        for marker in found {
            recovered.insert(marker.start, marker.clone());
        }
        if pair[1].number == pair[0].number + 2 {
            if let Some(candidate) = within(
                pair[0].number + 1,
                pair[0].start,
                pair[1].start,
                false,
                true,
            ) {
                recovered.insert(candidate.start, candidate.clone());
            }
        }
    }
    if let Some(first) = line.first().filter(|value| value.number > 1) {
        let end = candidates.partition_point(|value| value.start < first.start);
        let first_utf16 = text.utf16(first.start);
        let mut matching = candidates[..end].iter().filter(|value| {
            value.number == first.number - 1
                && value.formal
                && first_utf16 - text.utf16(value.start) <= 2_000
        });
        if let Some(candidate) = matching.next() {
            if matching.next().is_none() {
                recovered.insert(candidate.start, candidate.clone());
            }
        }
    }
    let mut result = line;
    result.extend(recovered.into_values());
    result.sort_by_key(|value| value.start);
    result
}

fn fill_lossy_marker_gaps(
    text: &ScalarText<'_>,
    spine: &[Marker],
    style: MarkerStyle,
) -> Vec<Marker> {
    let known = spine
        .iter()
        .map(|value| value.start)
        .collect::<HashSet<_>>();
    let known = match style {
        MarkerStyle::Bracket => [Some(&known), None],
        MarkerStyle::Dot => [None, Some(&known)],
        MarkerStyle::Bare => return spine.to_vec(),
    };
    let mut candidates = heading_joined(text, known);
    let candidates = std::mem::take(&mut candidates[style as usize]);
    let mut recovered = HashMap::<u32, Vec<Marker>>::new();
    for candidate in candidates {
        let at = spine.partition_point(|value| value.start < candidate.start);
        let before = spine[..at].last();
        let after = spine.get(at);
        let between = before.zip(after).is_some_and(|(left, right)| {
            left.number < candidate.number && candidate.number < right.number
        });
        let leading = before.is_none()
            && after.is_some_and(|right| {
                candidate.number > 0
                    && candidate.number < right.number
                    && right.number - candidate.number <= 2
                    && text.utf16(right.start) - text.utf16(candidate.start) <= 2_000
            });
        let sentence = before.zip(after).is_some_and(|(left, right)| {
            left.number + 1 == candidate.number && candidate.number + 1 == right.number
        }) && candidate.sentence;
        if (between || leading) && (candidate.formal || sentence) {
            recovered
                .entry(candidate.number)
                .or_default()
                .push(candidate);
        }
    }
    let mut result = spine.to_vec();
    result.extend(
        recovered
            .into_values()
            .filter(|values| values.len() == 1)
            .flatten(),
    );
    result.sort_by_key(|value| value.start);
    result
}

fn quoted_dot(text: &ScalarText<'_>, marker: &Marker) -> bool {
    if marker.style != MarkerStyle::Dot {
        return false;
    }
    let start = text.byte(marker.start);
    let end = text.value[start..]
        .find('\n')
        .map_or(text.value.len(), |at| start + at);
    let line = &text.value[start..end];
    cached_regex!(OPEN, r"^\d{1,4}\.\s+\(\d{1,4}\)\s+").is_match(line)
        && cached_regex!(WORD, r"(?i)\b(?:Act|Code|Regulations?|Rules?|shall|must)\b")
            .is_match(line)
}

struct Hypothesis {
    style: MarkerStyle,
    markers: Vec<Marker>,
    all: Arc<[Marker]>,
    short: bool,
    score: f64,
}

fn paragraph_ranges(
    text: &ScalarText<'_>,
    selected: &[Marker],
    all: &[Marker],
    style: MarkerStyle,
    fill_gaps: bool,
    extra: &[ScalarRange],
) -> Vec<Block> {
    let selected = if fill_gaps && style != MarkerStyle::Bare {
        fill_lossy_marker_gaps(text, selected, style)
    } else {
        selected.to_vec()
    };
    let mut boundaries = all
        .iter()
        .map(|value| value.start)
        .chain(selected.iter().map(|value| value.start))
        .chain(extra.iter().map(|value| value.start))
        .chain([text.len()])
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    selected
        .into_iter()
        .map(|marker| {
            let end = next_boundary(&boundaries, marker.start, text.len());
            Block::labelled(
                NodeKind::Paragraph,
                format!("par{}", marker.number),
                marker.start,
                end,
            )
        })
        .collect()
}

fn detect_paragraphs(
    text: &ScalarText<'_>,
    profile: DetectionProfile,
    excluded: &[ScalarRange],
) -> Vec<Block> {
    let text_length = text.utf16_len();
    let strict = profile != DetectionProfile::CaseLossy;
    let contiguous = profile == DetectionProfile::CaseContiguousComplete;
    let rooted = profile == DetectionProfile::CaseRootedComplete;
    let markers = paragraph_markers(text, contiguous);
    let mut by_style: [Vec<Marker>; 3] = std::array::from_fn(|_| Vec::new());
    for marker in markers {
        let filtered = rooted || (contiguous && marker.style != MarkerStyle::Bare);
        if !filtered || marker_visible(&marker, excluded) && (!strict || !quoted_dot(text, &marker))
        {
            by_style[marker.style as usize].push(marker);
        }
    }
    let mut joined: [Vec<Marker>; 2] = std::array::from_fn(|_| Vec::new());
    if rooted || contiguous {
        let known: [HashSet<usize>; 2] = std::array::from_fn(|index| {
            by_style[index].iter().map(|marker| marker.start).collect()
        });
        joined = heading_joined(
            text,
            [
                (rooted || !by_style[0].is_empty()).then_some(&known[0]),
                (rooted || !by_style[1].is_empty()).then_some(&known[1]),
            ],
        );
    }
    let mut hypotheses = Vec::<Hypothesis>::new();
    for style in [MarkerStyle::Bracket, MarkerStyle::Dot, MarkerStyle::Bare] {
        let style_index = style as usize;
        if rooted {
            let mut candidates = std::mem::take(&mut by_style[style_index]);
            if style != MarkerStyle::Bare {
                candidates.append(&mut joined[style_index]);
            }
            let (chain, score) = rooted_chain(&candidates);
            if chain.len() < 2 || endnote_shaped(text, &chain) {
                continue;
            }
            if chain.len() >= 5 {
                hypotheses.push(Hypothesis {
                    style,
                    markers: chain,
                    all: candidates.into(),
                    short: false,
                    score,
                });
            } else if style == MarkerStyle::Bracket && sole_chain(&chain, &candidates) {
                hypotheses.push(Hypothesis {
                    style,
                    markers: chain,
                    all: candidates.into(),
                    short: true,
                    score,
                });
            }
            continue;
        }
        let style_markers: Vec<Marker> = if contiguous && style != MarkerStyle::Bare {
            recover_contiguous(
                text,
                std::mem::take(&mut by_style[style_index]),
                &joined[style_index],
            )
            .into_iter()
            .filter(|value| marker_visible(value, excluded))
            .collect()
        } else {
            std::mem::take(&mut by_style[style_index])
        };
        let scopes = if contiguous {
            contiguous_scopes(&style_markers)
        } else {
            monotone_scopes(&style_markers, 8)
        };
        let all: Arc<[Marker]> = style_markers.into();
        for scope in scopes.iter() {
            if scope.len() >= 5 {
                hypotheses.push(Hypothesis {
                    style,
                    markers: scope.clone(),
                    all: Arc::clone(&all),
                    short: false,
                    score: 0.0,
                });
            } else if style == MarkerStyle::Bracket
                && scope.len() >= 2
                && scope
                    .iter()
                    .enumerate()
                    .all(|(index, value)| value.number == index as u32 + 1)
                && (!strict
                    || (scopes
                        .iter()
                        .all(|other| std::ptr::eq(other, scope) || other.len() == 1)
                        && all.iter().all(|value| {
                            scope.iter().any(|mark| mark.start == value.start)
                                || value.number > scope.last().unwrap().number + 1
                        })))
            {
                hypotheses.push(Hypothesis {
                    style,
                    markers: scope.clone(),
                    all: Arc::clone(&all),
                    short: true,
                    score: 0.0,
                });
            }
        }
    }
    if profile != DetectionProfile::CaseRootedComplete
        && hypotheses
            .iter()
            .any(|value| !value.short && value.markers[0].number <= 5)
    {
        hypotheses.retain(|value| value.short || value.markers[0].number <= 5);
    }
    let rank = |style| match style {
        MarkerStyle::Bracket => 2,
        MarkerStyle::Dot => 1,
        MarkerStyle::Bare => 0,
    };
    hypotheses.sort_by(|left, right| {
        left.short.cmp(&right.short).then_with(|| {
            if profile == DetectionProfile::CaseRootedComplete {
                right
                    .score
                    .total_cmp(&left.score)
                    .then(rank(right.style).cmp(&rank(left.style)))
            } else {
                right
                    .markers
                    .len()
                    .cmp(&left.markers.len())
                    .then(rank(right.style).cmp(&rank(left.style)))
                    .then(left.markers[0].number.cmp(&right.markers[0].number))
            }
        })
    });
    for hypothesis in hypotheses {
        let mut next = HashMap::with_capacity(hypothesis.all.len());
        for pair in hypothesis.all.windows(2) {
            next.insert(pair[0].start, pair[1].start);
        }
        if let Some(last) = hypothesis.all.last() {
            next.insert(last.start, text.len());
        }
        let first_start = hypothesis.markers[0].start;
        let last_start = hypothesis.markers.last().unwrap().start;
        let mut counts = hypothesis
            .markers
            .iter()
            .map(|marker| {
                let end = next.get(&marker.start).copied().unwrap_or(text.len());
                if end >= marker.start {
                    word_count(
                        text.slice(marker.start..end)
                            .expect("section range is bounded"),
                        contiguous,
                    )
                } else {
                    0
                }
            })
            .collect::<Vec<_>>();
        let bounded = if counts.len() > 1 {
            counts.len() - 1
        } else {
            counts.len()
        };
        let mean = counts[..bounded].iter().sum::<usize>() as f64 / bounded.max(1) as f64;
        let maximum = counts[..bounded].iter().copied().max().unwrap_or(0);
        let median = median(&mut counts[..bounded]);
        // The structure engine scores the unmodified hypothesis. Lossy heading inference
        // changes only the returned ranges after that hypothesis is accepted.
        let first_utf16 = text.utf16(first_start);
        let start = first_utf16 as f64 / text_length.max(1) as f64;
        let span = (text.utf16(last_start) - first_utf16) as f64 / text_length.max(1) as f64;
        if hypothesis.short {
            if text_length <= 6_000
                && (first_utf16 <= 1_200 || start <= 0.5)
                && counts.iter().copied().max().unwrap_or(0) >= 30
            {
                return paragraph_ranges(
                    text,
                    &hypothesis.markers,
                    &hypothesis.all,
                    hypothesis.style,
                    !strict,
                    excluded,
                );
            }
            continue;
        }
        let substantive = counts.iter().filter(|value| **value >= 12).count() as f64
            / hypothesis.markers.len() as f64;
        if !(median >= 12.0 || mean >= 20.0 || maximum >= 30)
            || span < 0.05
            || (hypothesis.style == MarkerStyle::Bracket
                && text_length > 6_000
                && start > 0.7
                && substantive < 0.5)
            || (hypothesis.style != MarkerStyle::Bracket && substantive < 0.7)
            || (hypothesis.style == MarkerStyle::Bare
                && (median < 20.0 || span < 0.15 || start > 0.7))
        {
            continue;
        }
        return paragraph_ranges(
            text,
            &hypothesis.markers,
            &hypothesis.all,
            hypothesis.style,
            !strict,
            excluded,
        );
    }
    Vec::new()
}

fn gapped_paragraphs(blocks: &[Block]) -> bool {
    let mut prior = None;
    blocks
        .iter()
        .filter(|value| value.kind == NodeKind::Paragraph)
        .filter_map(|value| {
            value
                .label
                .as_deref()?
                .strip_prefix("par")?
                .parse::<u32>()
                .ok()
        })
        .any(|value| {
            let gapped = prior.is_some_and(|prior| value != prior + 1);
            prior = Some(value);
            gapped
        })
}

fn clipped_case_paragraphs(
    mut blocks: Vec<Block>,
    excluded: &[ScalarRange],
    text: &ScalarText<'_>,
) -> Vec<Block> {
    blocks.retain_mut(|block| {
        for range in excluded {
            if range.start >= block.range.end {
                break;
            }
            if range.end <= block.range.start {
                continue;
            }
            if range.start <= block.range.start {
                return false;
            }
            block.range.end = range.start;
        }
        block.range.end > block.range.start
            && text
                .slice(block.range.start..block.range.end)
                .expect("block range is bounded")
                .chars()
                .any(char::is_alphabetic)
    });
    blocks
}

struct PageMarker {
    number: u32,
    start: usize,
    content_start: usize,
}

fn page_markers(text: &ScalarText<'_>, report_start: Option<u32>) -> Vec<PageMarker> {
    let regex = cached_regex!(
        VALUE,
        r"(?imu)\[[ \t]*pages?[ \t]*[.:,;]?[ \t]*(\d{1,4})[ \t]*[.:,;]?[ \t]*[\]\[)}]?[ \t]*[.,;:]?|^[ \t]*\[?[ \t]*page[ \t]*[.:,;]?[ \t]*(\d{1,4})[ \t]*[\])}]?[ \t]*[.,;:]?[ \t]*$"
    );
    let mut result = Vec::new();
    for line in lines(text).filter(|line| {
        line.text
            .as_bytes()
            .windows(4)
            .any(|word| word.eq_ignore_ascii_case(b"page"))
    }) {
        for capture in regex.captures_iter(line.text) {
            let whole = capture.get(0).unwrap();
            let number = capture
                .get(1)
                .or_else(|| capture.get(2))
                .unwrap()
                .as_str()
                .parse::<u32>()
                .unwrap();
            if report_start.is_some_and(|start| number < start) {
                continue;
            }
            let start = text.scalar(line.byte_start + whole.start());
            let content_start = text.scalar(line.byte_start + whole.end());
            result.push(PageMarker {
                number,
                start,
                content_start,
            });
        }
    }
    result
}

fn detect_pages(
    text: &ScalarText<'_>,
    report_start: Option<u32>,
    require_report_start: bool,
) -> Vec<Block> {
    if require_report_start && report_start.is_none() {
        return Vec::new();
    }
    let mut scopes = Vec::<Vec<PageMarker>>::new();
    let mut by_last = HashMap::<u32, Vec<usize>>::new();
    for marker in page_markers(text, report_start) {
        let number = marker.number;
        let index = marker
            .number
            .checked_sub(1)
            .and_then(|number| by_last.get(&number))
            .into_iter()
            .flatten()
            .copied()
            .reduce(|best, current| {
                if scopes[current].last().unwrap().start > scopes[best].last().unwrap().start {
                    current
                } else {
                    best
                }
            })
            .unwrap_or(scopes.len());
        if index == scopes.len() {
            scopes.push(vec![marker]);
        } else {
            let previous = scopes[index].last().unwrap().number;
            if let Some(values) = by_last.get_mut(&previous) {
                values.retain(|value| *value != index);
            }
            scopes[index].push(marker);
        }
        by_last.entry(number).or_default().push(index);
    }
    let mut best = Vec::new();
    let mut tied = false;
    for scope in scopes.into_iter().filter(|scope| scope.len() >= 3) {
        match scope.len().cmp(&best.len()) {
            std::cmp::Ordering::Greater => {
                best = scope;
                tied = false;
            }
            std::cmp::Ordering::Equal => tied = true,
            std::cmp::Ordering::Less => {}
        }
    }
    if best.is_empty() || tied {
        return Vec::new();
    }
    let mut blocks = best
        .windows(2)
        .map(|pair| {
            Block::labelled(
                NodeKind::Page,
                format!("page{}", pair[0].number),
                pair[0].content_start,
                pair[1].start,
            )
        })
        .collect::<Vec<_>>();
    if report_start.is_some_and(|start| best[0].number == start + 1) {
        blocks.insert(
            0,
            Block::labelled(
                NodeKind::Page,
                format!("page{}", report_start.unwrap()),
                0,
                best[0].start,
            ),
        );
    }
    blocks
}

fn detect_case(
    text: &ScalarText<'_>,
    profile: DetectionProfile,
    report_start_page: Option<u32>,
    require_report_start: bool,
    excluded: &[ScalarRange],
) -> Vec<Block> {
    let mut paragraphs = match profile {
        DetectionProfile::CaseContiguousComplete => {
            let complete = clipped_case_paragraphs(
                detect_paragraphs(text, DetectionProfile::CaseLossy, excluded),
                excluded,
                text,
            );
            if !complete.is_empty() && !gapped_paragraphs(&complete) {
                complete
            } else {
                detect_paragraphs(text, DetectionProfile::CaseContiguousComplete, excluded)
            }
        }
        profile => detect_paragraphs(text, profile, excluded),
    };
    paragraphs.extend(detect_pages(text, report_start_page, require_report_start));
    paragraphs
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum SectionStyle {
    Integer,
    Dot,
    DotTerm,
    Hyphen,
    Mixed,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum SectionFamily {
    Bare,
    DotTerm,
    Markdown,
    Emphasis,
    Range,
}

#[derive(Clone)]
pub(super) struct SectionMark {
    pub(super) label: String,
    pub(super) start: usize,
    pub(super) content_start: usize,
    pub(super) style: SectionStyle,
    pub(super) family: SectionFamily,
    pub(super) aliases: Vec<String>,
}

struct LabelPart<'a> {
    separator: char,
    digits: Option<&'a str>,
    text: &'a str,
    suffix: u32,
}

fn suffix_value(value: &str) -> u32 {
    value.bytes().fold(0, |total, value| {
        total * 26 + u32::from(value.to_ascii_uppercase() - b'A' + 1)
    })
}

fn label_parts(label: &str) -> impl Iterator<Item = LabelPart<'_>> {
    let mut separator = '\0';
    label.split_inclusive(['.', '-']).filter_map(move |piece| {
        let (body, next) = piece
            .strip_suffix('.')
            .map(|value| (value, '.'))
            .or_else(|| piece.strip_suffix('-').map(|value| (value, '-')))
            .unwrap_or((piece, '\0'));
        if body.is_empty() {
            separator = next;
            return None;
        }
        let digits = body.bytes().take_while(u8::is_ascii_digit).count();
        let numeric = (digits > 0
            && body[digits..]
                .chars()
                .all(|value| value.is_ascii_alphabetic()))
        .then_some(&body[..digits]);
        let value = LabelPart {
            separator,
            digits: numeric,
            text: body,
            suffix: suffix_value(&body[digits..]),
        };
        separator = next;
        Some(value)
    })
}

pub(super) fn compare_labels(left: &str, right: &str, fraction: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let (mut left, mut right) = (label_parts(left), label_parts(right));
    loop {
        let (a, b) = match (left.next(), right.next()) {
            (None, None) => return Equal,
            (None, Some(_)) => return Less,
            (Some(_), None) => return Greater,
            (Some(a), Some(b)) => (a, b),
        };
        if a.separator != b.separator {
            return a.separator.cmp(&b.separator);
        }
        match (a.digits, b.digits) {
            (Some(a_digits), Some(b_digits)) => {
                let width = a_digits.len().max(b_digits.len());
                let ordered = if fraction && a.separator == '.' {
                    (0..width)
                        .map(|index| *a_digits.as_bytes().get(index).unwrap_or(&b'0'))
                        .cmp(
                            (0..width)
                                .map(|index| *b_digits.as_bytes().get(index).unwrap_or(&b'0')),
                        )
                } else {
                    let a_digits = a_digits.trim_start_matches('0').as_bytes();
                    let b_digits = b_digits.trim_start_matches('0').as_bytes();
                    (0..width)
                        .map(|index| {
                            index
                                .checked_sub(width - a_digits.len())
                                .map_or(b'0', |index| a_digits[index])
                        })
                        .cmp((0..width).map(|index| {
                            index
                                .checked_sub(width - b_digits.len())
                                .map_or(b'0', |index| b_digits[index])
                        }))
                };
                if ordered != Equal {
                    return ordered;
                }
                if a_digits.len() != b_digits.len() {
                    return a_digits.len().cmp(&b_digits.len());
                }
                if a.suffix != b.suffix {
                    return a.suffix.cmp(&b.suffix);
                }
            }
            (Some(_), None) => return Less,
            (None, Some(_)) => return Greater,
            (None, None) => {
                let ordered = a
                    .text
                    .bytes()
                    .map(|byte| byte.to_ascii_uppercase())
                    .cmp(b.text.bytes().map(|byte| byte.to_ascii_uppercase()));
                if ordered != Equal {
                    return ordered;
                }
            }
        }
    }
}

fn numeric_label(value: &str, markdown: bool) -> Option<(&str, usize)> {
    let bytes = value.as_bytes();
    let mut end = bytes
        .iter()
        .take(8)
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if end == 0 {
        return None;
    }
    for _ in 0..3 {
        if !bytes
            .get(end)
            .is_some_and(|byte| matches!(byte, b'.' | b'-'))
        {
            break;
        }
        let digits = bytes[end + 1..]
            .iter()
            .take(8)
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits == 0 {
            break;
        }
        end += digits + 1;
    }
    end += bytes[end..]
        .iter()
        .take(2)
        .take_while(|byte| byte.is_ascii_uppercase())
        .count();
    let label = &value[..end];
    (!markdown || label.contains(['.', '-'])).then_some((label, end))
}

pub(crate) fn provision_label(value: &str) -> Option<(&str, usize)> {
    let matched = cached_regex!(VALUE,
    r"^(?:\d{1,8}[A-Za-z]{0,3}(?:[.-]\d{1,8}[A-Za-z]{0,3}){0,3}|[A-Za-z]{1,3}(?:[.-][0-9A-Za-z]{1,8}){1,3})"
).find(value)?;
    Some((matched.as_str(), matched.end()))
}

fn section_style(label: &str, trailing: bool) -> SectionStyle {
    if trailing {
        SectionStyle::DotTerm
    } else if label.contains('.') && label.contains('-') {
        SectionStyle::Mixed
    } else if label.contains('-') {
        SectionStyle::Hyphen
    } else if label.contains('.') {
        SectionStyle::Dot
    } else {
        SectionStyle::Integer
    }
}

fn bare_content_starts(value: &str) -> bool {
    value.chars().next().is_some_and(|character| {
        character.is_alphabetic() || character.is_ascii_digit() || "([*“\"«".contains(character)
    })
}

fn dotterm_content_starts(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|character| character.is_alphanumeric() || "\"'“«(".contains(character))
}

fn markdown_range_continuation(value: &str) -> bool {
    cached_regex!(
        VALUE,
        r"(?iu)^[ \t]*#{1,6}[ \t]+.*(?:[ \t](?:to|à)|[-–—])[ \t]*$"
    )
    .is_match(value)
}

fn section_mark(
    text: &ScalarText<'_>,
    line: &Line<'_>,
    family: SectionFamily,
    previous_nonblank: Option<&str>,
) -> Option<SectionMark> {
    let lead = leading_ascii_space(line.text);
    let mut value = &line.text[lead..];
    if family == SectionFamily::Markdown {
        let hashes = value.bytes().take_while(|byte| *byte == b'#').count();
        if !(1..=6).contains(&hashes) || !value[hashes..].starts_with([' ', '\t']) {
            return None;
        }
        value = value[hashes..].trim_start_matches([' ', '\t']);
    }
    let bold = value.starts_with("**");
    if bold {
        value = &value[2..];
    }
    let (label, length) = numeric_label(value, family == SectionFamily::Markdown)?;
    let mut after = length;
    if bold {
        if !value[after..].starts_with("**") {
            return None;
        }
        after += 2;
    }
    let mut trailing = false;
    if family == SectionFamily::DotTerm {
        let punctuation = value[after..]
            .chars()
            .next()
            .filter(|value| matches!(value, '.' | ')'))?;
        after += punctuation.len_utf8();
        trailing = true;
    } else if family == SectionFamily::Markdown && value[after..].starts_with('.') {
        after += 1;
        trailing = true;
    }
    let rest = &value[after..];
    let spaces = leading_ascii_space(rest);
    let content = &rest[spaces..];
    let accepted = match family {
        SectionFamily::Bare => {
            content.is_empty()
                || (spaces > 0 && bare_content_starts(content))
                || (spaces == 0 && content.starts_with('('))
        }
        SectionFamily::DotTerm => {
            !content.is_empty()
                && dotterm_content_starts(content)
                && (spaces > 0 || content.starts_with('('))
        }
        SectionFamily::Markdown => content.is_empty() || (spaces > 0 && !content.is_empty()),
        _ => false,
    };
    if !accepted
        || family == SectionFamily::Bare
            && content.is_empty()
            && previous_nonblank.is_some_and(markdown_range_continuation)
    {
        return None;
    }
    Some(SectionMark {
        label: label.to_owned(),
        start: text.scalar(line.byte_start + lead),
        content_start: text.scalar(line.byte_end - content.len()),
        style: section_style(label, trailing),
        family,
        aliases: Vec::new(),
    })
}

fn collect_section_families(text: &ScalarText<'_>, source: &[Line<'_>]) -> [Vec<SectionMark>; 3] {
    let mut result = std::array::from_fn(|_| Vec::new());
    let mut previous_nonblank = None;
    for source_line in source {
        let line = source_line.text.trim_start_matches([' ', '\t']);
        let numeric = line.as_bytes().first().is_some_and(u8::is_ascii_digit)
            || line
                .strip_prefix("**")
                .and_then(|value| value.as_bytes().first())
                .is_some_and(u8::is_ascii_digit);
        if numeric {
            for (family, marks) in [SectionFamily::Bare, SectionFamily::DotTerm]
                .into_iter()
                .zip(&mut result[..2])
            {
                if let Some(mark) = section_mark(text, source_line, family, previous_nonblank) {
                    marks.push(mark);
                }
            }
        } else if line.starts_with('#') {
            if let Some(mark) = section_mark(
                text,
                source_line,
                SectionFamily::Markdown,
                previous_nonblank,
            ) {
                result[2].push(mark);
            }
        }
        if !source_line.text.trim().is_empty() {
            previous_nonblank = Some(source_line.text);
        }
    }
    result
}

fn section_key(label: &str) -> impl Iterator<Item = u64> + '_ {
    label.split(['.', '-']).filter_map(|value| {
        value
            .bytes()
            .take_while(u8::is_ascii_digit)
            .fold(None, |total, digit| {
                Some(total.unwrap_or(0) * 10 + u64::from(digit - b'0'))
            })
    })
}

fn section_scopes<'a>(
    marks: &[&'a SectionMark],
    styles: &[SectionStyle],
    root: bool,
    fraction: bool,
) -> Vec<Vec<&'a SectionMark>> {
    let mut scopes = Vec::<Vec<&SectionMark>>::new();
    for mark in marks
        .iter()
        .copied()
        .filter(|value| styles.contains(&value.style))
    {
        let parts = label_parts(&mark.label).count();
        let best = scopes
            .iter()
            .enumerate()
            .filter(|(_, scope)| {
                let last = scope.last().unwrap();
                parts == label_parts(&last.label).count()
                    && compare_labels(&mark.label, &last.label, fraction).is_gt()
            })
            .reduce(|best, candidate| {
                if compare_labels(
                    &candidate.1.last().unwrap().label,
                    &best.1.last().unwrap().label,
                    fraction,
                )
                .is_gt()
                {
                    candidate
                } else {
                    best
                }
            })
            .map(|value| value.0);
        if let Some(best) = best {
            scopes[best].push(mark);
        } else {
            scopes.push(vec![mark]);
        }
        if scopes.len() > 8 {
            let smallest = (0..scopes.len())
                .min_by_key(|index| scopes[*index].len())
                .unwrap();
            scopes.remove(smallest);
        }
    }
    scopes
        .into_iter()
        .filter(|scope| {
            scope.len() >= 3 && (!root || section_key(&scope[0].label).all(|value| value == 1))
        })
        .collect()
}

fn expand_descendants<'a>(
    scope: Vec<&'a SectionMark>,
    marks: &[&'a SectionMark],
    length: usize,
) -> Vec<&'a SectionMark> {
    if scope
        .first()
        .is_none_or(|value| section_key(&value.label).count() != 1)
    {
        return scope;
    }
    let mut result = Vec::with_capacity(scope.len());
    let mut cursor = 0;
    let mut parents = scope.into_iter().peekable();
    while let Some(parent) = parents.next() {
        let end = parents.peek().map_or(length, |value| value.start);
        while marks
            .get(cursor)
            .is_some_and(|mark| mark.start <= parent.start)
        {
            cursor += 1;
        }
        let begin = cursor;
        while marks.get(cursor).is_some_and(|mark| mark.start < end) {
            cursor += 1;
        }
        let root = section_key(&parent.label).next();
        let mut descendants = Vec::new();
        let mut counts = HashMap::<&str, usize>::new();
        for &mark in &marks[begin..cursor] {
            if matches!(mark.style, SectionStyle::Dot | SectionStyle::DotTerm)
                && mark.label.contains('.')
                && section_key(&mark.label).next() == root
            {
                descendants.push(mark);
                *counts.entry(mark.label.as_str()).or_default() += 1;
            }
        }
        result.push(parent);
        result.extend(
            descendants
                .into_iter()
                .filter(|value| counts.get(value.label.as_str()) == Some(&1)),
        );
    }
    result
}

fn same_labels(left: &[&SectionMark], right: &[&SectionMark]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.label == right.label)
}

fn choose_sections<'a>(
    left: Option<Vec<&'a SectionMark>>,
    right: Option<Vec<&'a SectionMark>>,
) -> Option<Vec<&'a SectionMark>> {
    match (left, right) {
        (None, value) | (value, None) => value,
        (Some(left), Some(right)) if same_labels(&left, &right) => Some(left),
        (Some(left), Some(right)) if left[0].start != right[0].start => {
            Some(if left[0].start < right[0].start {
                left
            } else {
                right
            })
        }
        (Some(left), Some(right)) if left.len() != right.len() => {
            Some(if left.len() > right.len() {
                left
            } else {
                right
            })
        }
        _ => None,
    }
}

fn section_guard(value: &[&SectionMark], text: &ScalarText<'_>) -> bool {
    let length = text.utf16_len();
    !value.is_empty() && length > 0 && text.utf16(value[0].start) as f64 / length as f64 <= 0.7
}

fn scope_winner<'a>(
    scopes: Vec<Vec<&'a SectionMark>>,
    marks: &[&'a SectionMark],
    text: &ScalarText<'_>,
) -> Option<Vec<&'a SectionMark>> {
    let mut best: Option<Vec<&'a SectionMark>> = None;
    let mut ambiguous = false;
    for scope in scopes.into_iter().map(|scope| {
        if scope.first().is_some_and(|value| {
            value.style != SectionStyle::DotTerm && section_key(&value.label).count() == 1
        }) {
            expand_descendants(scope, marks, text.len())
        } else {
            scope
        }
    }) {
        if !section_guard(&scope, text) {
            continue;
        }
        let Some(current) = best.as_ref() else {
            best = Some(scope);
            continue;
        };
        let better = scope.len() > current.len()
            || scope.len() == current.len() && scope[0].start < current[0].start;
        let competing = scope.len() == current.len()
            && scope[0].start == current[0].start
            && !same_labels(&scope, current);
        if better {
            best = Some(scope);
            ambiguous = false;
        } else if competing {
            ambiguous = true;
        }
    }
    if ambiguous {
        None
    } else {
        best
    }
}

fn statute_winner<'a>(
    marks: &[&'a SectionMark],
    text: &ScalarText<'_>,
    allow_hyphen: bool,
) -> Option<Vec<&'a SectionMark>> {
    if marks.len() < 3 {
        return None;
    }
    let mut component = section_scopes(
        marks,
        &[
            SectionStyle::Integer,
            SectionStyle::Dot,
            SectionStyle::DotTerm,
        ],
        false,
        false,
    );
    if allow_hyphen {
        component.extend(section_scopes(marks, &[SectionStyle::Hyphen], true, false));
        component.extend(section_scopes(marks, &[SectionStyle::Mixed], true, false));
    }
    choose_sections(
        scope_winner(component, marks, text),
        scope_winner(
            section_scopes(marks, &[SectionStyle::Dot], false, true),
            marks,
            text,
        ),
    )
}

fn inline_section(text: &ScalarText<'_>, mark: &SectionMark) -> bool {
    let start = text.byte(mark.content_start);
    let end = text.value[start..]
        .find('\n')
        .map_or(text.value.len(), |value| start + value);
    !text.value[start..end].trim().is_empty()
}

fn next_nonblank<'a>(source: &'a [Line<'a>], start: usize) -> Option<&'a Line<'a>> {
    source[source.partition_point(|line| line.scalar_start <= start)..]
        .iter()
        .find(|line| !line.text.trim().is_empty())
}

fn short_root(
    text: &ScalarText<'_>,
    families: &[Vec<SectionMark>; 3],
    source: &[Line<'_>],
) -> Vec<SectionMark> {
    let status = cached_regex!(
        STATUS,
        r"(?iu)^(?:\[\s*)?(?:repealed|revoked|abrog(?:ated|é|ée|és|ées)|renumbered|spent|not (?:yet )?in force|omitted)\b"
    );
    let heading = cached_regex!(HEADING, r#"^(?:(?:["'“«]\s*)?\p{Lu}|\(\d+\))"#);
    let mut candidates = families
        .iter()
        .flatten()
        .filter(|value| matches!(value.label.as_str(), "1" | "2"))
        .cloned()
        .collect::<Vec<_>>();
    for line in source {
        let value = line.text.trim_matches([' ', '\t']);
        if matches!(value, "1" | "2") {
            candidates.push(SectionMark {
                label: value.to_owned(),
                start: line.scalar_start + leading_ascii_space(line.text),
                content_start: text.scalar(line.byte_end),
                style: SectionStyle::Integer,
                family: SectionFamily::Bare,
                aliases: Vec::new(),
            });
        }
    }
    let mut invalid = false;
    for marker in &mut candidates {
        let start = text.byte(marker.content_start);
        let end = text.value[start..]
            .find('\n')
            .map_or(text.value.len(), |value| start + value);
        // Preserve the source grammar's raw offset check: on CRLF input a
        // label ending immediately before `\r` is not "at" the `\n` line
        // end, even though the intervening scalar is whitespace.
        if start >= end {
            let Some(next) = next_nonblank(source, marker.start) else {
                invalid = true;
                continue;
            };
            let next_value = next.text.trim_start();
            let parenthetical = next_value.starts_with('(')
                && next_value[1..].chars().next().is_some_and(char::is_numeric);
            if !heading.is_match(next_value) && !parenthetical && !status.is_match(next_value) {
                invalid = true;
            } else {
                marker.content_start = next.scalar_start + leading_ascii_space(next.text);
            }
        }
    }
    if invalid {
        return Vec::new();
    }
    candidates.sort_by_key(|value| value.start);
    candidates.dedup_by(|left, right| left.label == right.label && left.start == right.start);
    let mut ones = candidates.iter().filter(|value| value.label == "1");
    let Some(one) = ones.next() else {
        return Vec::new();
    };
    if ones.next().is_some() {
        return Vec::new();
    }
    let mut twos = candidates.iter().filter(|value| value.label == "2");
    let two = twos.next();
    if twos.next().is_some() || two.is_some_and(|two| two.start <= one.start) {
        return Vec::new();
    }
    let accepted = text.utf16(one.start) as f64 / text.utf16_len().max(1) as f64 <= 0.7;
    if accepted {
        candidates
    } else {
        Vec::new()
    }
}

fn statute_spine_over(
    text: &ScalarText<'_>,
    allow_hyphen: bool,
    inline_only: bool,
    all_families: &[Vec<SectionMark>; 3],
    source: &[Line<'_>],
) -> Vec<SectionMark> {
    let all = all_families
        .each_ref()
        .map(|marks| marks.iter().collect::<Vec<_>>());
    let filtered;
    let families = if inline_only {
        filtered = all.each_ref().map(|marks| {
            marks
                .iter()
                .copied()
                .filter(|mark| inline_section(text, mark))
                .collect::<Vec<_>>()
        });
        &filtered
    } else {
        &all
    };
    let mut candidates = families
        .iter()
        .filter_map(|marks| statute_winner(marks, text, allow_hyphen))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|value| value[0].start);
    if candidates.is_empty() {
        return short_root(text, all_families, source);
    }
    let mut candidates = candidates.into_iter();
    let mut best = candidates.next().unwrap();
    let first_start = best[0].start;
    for candidate in candidates.take_while(|value| value[0].start == first_start) {
        let Some(chosen) = choose_sections(Some(best), Some(candidate)) else {
            return Vec::new();
        };
        best = chosen;
    }
    let best = if best[0].family == SectionFamily::DotTerm {
        let mut marks = families[0]
            .iter()
            .chain(&families[1])
            .copied()
            .collect::<Vec<_>>();
        marks.sort_by_key(|value| value.start);
        expand_descendants(best, &marks, text.len())
    } else {
        best
    };
    best.into_iter().cloned().collect()
}

fn statute_spine_from_lines(
    text: &ScalarText<'_>,
    allow_hyphen: bool,
    source: &[Line<'_>],
) -> Vec<SectionMark> {
    let families = collect_section_families(text, source);
    let result = statute_spine_over(text, allow_hyphen, false, &families, source);
    if result.is_empty() || result.iter().any(|value| inline_section(text, value)) {
        result
    } else {
        statute_spine_over(text, allow_hyphen, true, &families, source)
    }
}

#[cfg(test)]
pub(super) fn statute_spine(text: &ScalarText<'_>, allow_hyphen: bool) -> Vec<SectionMark> {
    statute_spine_from_lines(text, allow_hyphen, &lines(text).collect::<Vec<_>>())
}

pub(crate) fn dotted_order<'a>(labels: impl Iterator<Item = &'a str>) -> Option<bool> {
    let mut dotted = labels.filter(|value| value.contains('.') && !value.contains('-'));
    let Some(mut prior) = dotted.next() else {
        return Some(false);
    };
    let (mut component, mut fraction, mut different) = (0, 0, false);
    for value in dotted {
        let component_order = compare_labels(prior, value, false);
        let fraction_order = compare_labels(prior, value, true);
        component += usize::from(component_order.is_gt());
        fraction += usize::from(fraction_order.is_gt());
        different |= component_order != fraction_order;
        prior = value;
    }
    if component != fraction {
        return Some(fraction < component);
    }
    (!different).then_some(false)
}

fn emphasis_sections(text: &ScalarText<'_>, source: &[Line<'_>]) -> Vec<SectionMark> {
    if !text.value.contains("**") {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for line in source {
        let lead = leading_ascii_space(line.text);
        let value = &line.text[lead..];
        let Some(value) = value.strip_prefix("**") else {
            continue;
        };
        let Some((label, length)) = provision_label(value) else {
            continue;
        };
        if !value[length..].starts_with("**") {
            continue;
        }
        let rest = &value[length + 2..];
        if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
            continue;
        }
        candidates.push(SectionMark {
            label: label.to_owned(),
            start: line.scalar_start + lead,
            content_start: line.scalar_start
                + lead
                + 2
                + label.len()
                + 2
                + leading_ascii_space(rest),
            style: section_style(label, false),
            family: SectionFamily::Emphasis,
            aliases: Vec::new(),
        });
    }
    let Some(first) = candidates.first() else {
        return candidates;
    };
    let numeric = first
        .label
        .starts_with(|value: char| value.is_ascii_digit());
    candidates.retain(|value| {
        value
            .label
            .starts_with(|character: char| character.is_ascii_digit())
            == numeric
    });
    let Some(fraction) = dotted_order(candidates.iter().map(|mark| mark.label.as_str())) else {
        return Vec::new();
    };
    let mut result = Vec::<SectionMark>::new();
    for marker in candidates {
        if result
            .last()
            .is_none_or(|prior| compare_labels(&marker.label, &prior.label, fraction).is_gt())
        {
            result.push(marker);
        }
    }
    let length = text.utf16_len();
    let start = text.utf16(result[0].start);
    if start as f64 / length.max(1) as f64 > 0.7
        || (length - start) as f64 / (length.max(1) as f64) < 0.1
    {
        Vec::new()
    } else {
        result
    }
}

fn status_sections(text: &ScalarText<'_>, allow_hyphen: bool) -> Vec<SectionMark> {
    let regex = cached_regex!(
        VALUE,
        r"(?imu)^[ \t]*(?:\*\*)?(\d{1,4})(?:[ \t]+(?:to|through|and|à|a|et)[ \t]+|[ \t]*([-–—])[ \t]*)(\d{1,4})(?:\*\*)?[ \t]*[,;:]?[ \t]*(?:\[[ \t]*)?(?:repealed|revoked|abrog(?:ated|é|ée|és|ées)|renumbered|spent|not (?:yet )?in force|omitted)\b"
    );
    regex
        .captures_iter(text.value)
        .filter_map(|capture| {
            if allow_hyphen && capture.get(2).is_some() {
                return None;
            }
            let from = capture[1].parse::<u32>().ok()?;
            let to = capture[3].parse::<u32>().ok()?;
            if from >= to || to > from + 400 {
                return None;
            }
            let whole = capture.get(0).unwrap();
            Some(SectionMark {
                label: from.to_string(),
                start: text.scalar(whole.start() + leading_ascii_space(whole.as_str())),
                content_start: text.scalar(whole.end()),
                style: SectionStyle::Integer,
                family: SectionFamily::Range,
                aliases: (from + 1..=to).map(|value| value.to_string()).collect(),
            })
        })
        .collect()
}

fn coherent_sections(marks: &[SectionMark]) -> bool {
    let Some(fraction) = dotted_order(marks.iter().map(|mark| mark.label.as_str())) else {
        return false;
    };
    marks
        .windows(2)
        .all(|pair| compare_labels(&pair[0].label, &pair[1].label, fraction).is_lt())
}

fn selected_sections(text: &ScalarText<'_>, allow_hyphen: bool) -> Vec<SectionMark> {
    let source = lines(text).collect::<Vec<_>>();
    let emphasis = emphasis_sections(text, &source);
    let flat = statute_spine_from_lines(text, allow_hyphen, &source);
    let mut selected = if emphasis.is_empty() {
        flat
    } else if flat.is_empty() {
        emphasis
    } else {
        let occurrences = emphasis
            .iter()
            .map(|value| (value.label.to_ascii_lowercase(), value.content_start))
            .collect::<HashSet<_>>();
        if !flat.iter().any(|value| {
            occurrences.contains(&(value.label.to_ascii_lowercase(), value.content_start))
        }) {
            emphasis
        } else {
            let mut labels = flat
                .into_iter()
                .map(|value| (value.label.to_ascii_lowercase(), value))
                .collect::<HashMap<_, _>>();
            for marker in emphasis.iter().cloned() {
                let key = marker.label.to_ascii_lowercase();
                if labels
                    .get(&key)
                    .is_none_or(|value| value.content_start == marker.content_start)
                {
                    labels.insert(key, marker);
                }
            }
            let mut combined = labels.into_values().collect::<Vec<_>>();
            combined.sort_by_key(|value| value.start);
            if coherent_sections(&combined) {
                combined
            } else {
                emphasis
            }
        }
    };
    let ranges = status_sections(text, allow_hyphen);
    if !ranges.is_empty() {
        let mut labels = selected
            .iter()
            .cloned()
            .map(|value| (value.label.to_ascii_lowercase(), value))
            .collect::<HashMap<_, _>>();
        for marker in ranges {
            for alias in &marker.aliases {
                labels.remove(&alias.to_ascii_lowercase());
            }
            labels.insert(marker.label.to_ascii_lowercase(), marker);
        }
        let mut combined = labels.into_values().collect::<Vec<_>>();
        combined.sort_by_key(|value| value.start);
        if coherent_sections(&combined) {
            selected = combined;
        }
    }
    selected
}

fn roman_value(value: &str) -> Option<u32> {
    let mut total = 0i32;
    let mut prior = 0;
    for character in value.bytes().rev() {
        let value = match character.to_ascii_lowercase() {
            b'i' => 1,
            b'v' => 5,
            b'x' => 10,
            b'l' => 50,
            b'c' => 100,
            b'd' => 500,
            b'm' => 1000,
            _ => return None,
        };
        total += if value < prior { -value } else { value };
        prior = prior.max(value);
    }
    (total > 0).then_some(total as u32)
}

struct EnumFrame {
    family: u8,
    value: String,
    label: String,
}

struct ChildMark<'a> {
    token: &'a str,
    start: usize,
    content_start: usize,
}

pub(super) struct GrammarPoint {
    pub(super) range: ScalarRange,
    pub(super) label: String,
    pub(super) parent_label: Option<String>,
    pub(super) content_start: usize,
    pub(super) diagnostic: Option<&'static str>,
}

impl GrammarPoint {
    fn into_section(self) -> Block {
        Block {
            kind: NodeKind::Section,
            range: self.range,
            label: Some(self.label),
            aliases: Vec::new(),
            parent_label: self.parent_label,
            content_start: Some(self.content_start),
            diagnostic: self.diagnostic,
            source: Derivation::Heuristic,
            origin_id: ENGINE_ORIGIN,
        }
    }
}

#[derive(Default)]
struct StructureState {
    nodes: Vec<(GrammarPoint, usize)>,
    container: Option<String>,
    section: Option<(String, usize)>,
    stack: Vec<EnumFrame>,
    used: HashMap<String, usize>,
}

fn enum_readings(token: &str) -> [Option<(u8, String)>; 2] {
    if token
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return [Some((0, token.to_owned())), None];
    }
    let bytes = token.as_bytes();
    let alpha = match bytes {
        [value @ (b'a'..=b'z' | b'A'..=b'Z')] => {
            Some(u32::from(value.to_ascii_lowercase() - b'a' + 1))
        }
        [left @ (b'a'..=b'z' | b'A'..=b'Z'), right] if left.eq_ignore_ascii_case(right) => {
            Some(26 + u32::from(left.to_ascii_lowercase() - b'a' + 1))
        }
        _ => None,
    };
    let upper = bytes.iter().any(u8::is_ascii_uppercase);
    let alpha = alpha.map(|value| (if upper { 3 } else { 1 }, value));
    let roman = roman_value(token).map(|value| (if upper { 4 } else { 2 }, value));
    if token.len() > 1 {
        [
            roman
                .filter(|(_, value)| *value <= 50)
                .or(alpha)
                .or(roman)
                .map(|(family, value)| (family, value.to_string())),
            None,
        ]
    } else {
        [alpha, roman].map(|reading| reading.map(|(family, value)| (family, value.to_string())))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum InstrumentMarkerKind {
    Parenthesized,
    Tail,
    Dot,
}

fn any_instrument_marker(value: &str) -> Option<(&str, usize, InstrumentMarkerKind)> {
    if !value
        .as_bytes()
        .first()
        .is_some_and(|byte| *byte == b'(' || byte.is_ascii_lowercase())
    {
        return None;
    }
    let valid_letters = |token: &str, upper: bool| {
        let bytes = token.as_bytes();
        let case = |byte: &u8| {
            if upper {
                byte.is_ascii_uppercase()
            } else {
                byte.is_ascii_lowercase()
            }
        };
        (1..=2).contains(&bytes.len()) && bytes.iter().all(case)
            || (1..=6).contains(&bytes.len())
                && bytes.iter().all(|byte| {
                    matches!(
                        byte.to_ascii_lowercase(),
                        b'i' | b'v' | b'x' | b'l' | b'c' | b'd' | b'm'
                    )
                })
                && bytes.iter().all(case)
    };
    let valid_numeric = |token: &str| {
        let mut parts = token.split('.');
        let valid = |part: &str| {
            (1..=3).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit())
        };
        parts.next().is_some_and(valid) && parts.next().is_none_or(valid) && parts.next().is_none()
    };
    if let Some(rest) = value.strip_prefix('(') {
        let close = rest.find(')')?;
        let token = &rest[..close];
        if !valid_numeric(token) && !valid_letters(token, false) && !valid_letters(token, true) {
            return None;
        }
        let after = &rest[close + 1..];
        return Some((
            token,
            close + 2 + leading_ascii_space(after),
            InstrumentMarkerKind::Parenthesized,
        ));
    }
    let delimiter = value.find([')', '.'])?;
    let token = &value[..delimiter];
    if !valid_letters(token, false) {
        return None;
    }
    let after = &value[delimiter + 1..];
    let gap = leading_ascii_space(after);
    let content = &after[gap..];
    if gap == 0 || content.chars().next().is_none_or(char::is_whitespace) {
        return None;
    }
    Some((
        token,
        delimiter + 1 + gap,
        if value.as_bytes()[delimiter] == b')' {
            InstrumentMarkerKind::Tail
        } else {
            InstrumentMarkerKind::Dot
        },
    ))
}

fn instrument_marker(value: &str, tail: bool, dot: bool) -> Option<(&str, usize)> {
    any_instrument_marker(value).and_then(|(token, at, kind)| match kind {
        InstrumentMarkerKind::Parenthesized => Some((token, at)),
        InstrumentMarkerKind::Tail if tail => Some((token, at)),
        InstrumentMarkerKind::Dot if dot => Some((token, at)),
        _ => None,
    })
}

fn instrument_space(character: char) -> bool {
    // Instrument grammar historically admits Rust whitespace, including U+0085;
    // it is therefore not the shared ECMAScript whitespace contract.
    character.is_whitespace() || character == '\u{feff}'
}

fn legislation_marker(value: &str, followed_by_newline: bool) -> Option<(&str, usize, usize)> {
    let found = cached_regex!(
        MARKER,
        r"^\((\d+(?:\.\d+)?|[A-Za-z](?:\.\d+)?|[ivxlcdmIVXLCDM]+)\)"
    )
    .captures(value)?;
    let whole = found.get(0).unwrap();
    let rest = &value[whole.end()..];
    (rest.chars().next().is_some_and(char::is_whitespace)
        || (rest.is_empty() && followed_by_newline))
        .then(|| {
            (
                found.get(1).unwrap().as_str(),
                whole.end() + leading_ascii_space(rest),
                whole.end() - 1,
            )
        })
}

fn compare_child_values(left: &str, right: &str) -> std::cmp::Ordering {
    let (left_head, left_tail) = left.split_once('.').unwrap_or((left, ""));
    let (right_head, right_tail) = right.split_once('.').unwrap_or((right, ""));
    let head = left_head
        .parse::<f64>()
        .unwrap_or_default()
        .partial_cmp(&right_head.parse::<f64>().unwrap_or_default())
        .unwrap_or(std::cmp::Ordering::Equal);
    if !head.is_eq() {
        return head;
    }
    left_tail
        .trim_end_matches('0')
        .cmp(right_tail.trim_end_matches('0'))
}

fn admitted_dialects<'a>(
    text: &ScalarText<'_>,
    source: &mut [Line<'a>],
) -> ([bool; 2], Vec<(usize, &'a str, usize, usize)>) {
    let mut live: [[(String, usize); 5]; 2] = Default::default();
    let mut best = [0, 0];
    let mut markers = Vec::new();
    for line in source {
        let trimmed = line.text.trim_start_matches(instrument_space);
        let lead = line.text.len() - trimmed.len();
        line.byte_start += lead;
        line.scalar_start = text.scalar(line.byte_start);
        line.text = trimmed.trim_end_matches(instrument_space);
        let value = line.text;
        if value.starts_with('(') {
            continue;
        }
        let Some((token, at, kind)) = any_instrument_marker(value) else {
            continue;
        };
        let index = match kind {
            InstrumentMarkerKind::Parenthesized => continue,
            InstrumentMarkerKind::Tail => 0,
            InstrumentMarkerKind::Dot => 1,
        };
        markers.push((line.byte_start, token, at, index));
        for (family, value) in enum_readings(token).into_iter().flatten() {
            let state = &mut live[index][usize::from(family)];
            if value == "1" {
                *state = (value, 1);
            } else if state.1 > 0 && compare_labels(&value, &state.0, true).is_gt() {
                *state = (value, state.1 + 1);
            }
            best[index] = best[index].max(state.1);
        }
    }
    ([best[0] >= 3, best[1] >= 3], markers)
}

impl StructureState {
    fn emit_child(
        &mut self,
        token: &str,
        start: usize,
        content_start: usize,
        parent: String,
        depth: usize,
        code: &'static str,
    ) -> String {
        let base = format!("{parent}({token})");
        let label = match self.used.entry(base) {
            Entry::Vacant(entry) => {
                let label = entry.key().clone();
                entry.insert(2);
                label
            }
            Entry::Occupied(mut entry) => {
                let occurrence = *entry.get();
                let label = format!("{}@{occurrence}", entry.key());
                *entry.get_mut() += 1;
                label
            }
        };
        self.nodes.push((
            GrammarPoint {
                range: ScalarRange {
                    start,
                    end: usize::MAX,
                },
                label: label.clone(),
                parent_label: Some(parent),
                content_start,
                diagnostic: Some(code),
            },
            depth,
        ));
        label
    }

    fn child(&mut self, token: &str, start: usize, content_start: usize) {
        let (root, root_depth) = self.section.as_ref().unwrap();
        let readings = enum_readings(token).map(|reading| {
            reading.map(|(family, value)| {
                let at = self.stack.iter().rposition(|frame| frame.family == family);
                (family, value, at)
            })
        });
        let selected = (0..4).find_map(|pass| {
            readings.iter().flatten().find_map(|(family, value, at)| {
                match (pass, *at) {
                    (0, Some(index)) => {
                        let prior = &self.stack[index].value;
                        match (prior.parse::<u32>(), value.parse::<u32>()) {
                            (Ok(prior), Ok(value)) => prior + 1 == value,
                            _ => compare_labels(value, prior, true).is_gt(),
                        }
                    }
                    (1, None) => value == "1" && self.stack.len() < 6,
                    (2, Some(index)) => {
                        value == "1"
                            || compare_labels(value, &self.stack[index].value, true).is_gt()
                    }
                    (3, None) => self.stack.len() < 6,
                    _ => false,
                }
                .then(|| (pass, *family, value.as_str(), *at))
            })
        });
        let (pass, family, value, at) = selected.unwrap_or((4, 0, "", None));
        let (parent, depth) = if pass == 4 {
            (root.clone(), root_depth + 1)
        } else if let Some(index) = at {
            self.stack.truncate(index + 1);
            self.stack[index].value = value.to_owned();
            (
                index
                    .checked_sub(1)
                    .map_or_else(|| root.clone(), |parent| self.stack[parent].label.clone()),
                root_depth + index + 1,
            )
        } else {
            let parent = self
                .stack
                .last()
                .map_or_else(|| root.clone(), |frame| frame.label.clone());
            let depth = root_depth + self.stack.len() + 1;
            self.stack.push(EnumFrame {
                family,
                value: value.to_owned(),
                label: String::new(),
            });
            (parent, depth)
        };
        let code = match (pass, value) {
            (0, _) => "instrument_ladder_increment",
            (1, _) => "instrument_ladder_level_open",
            (2, "1") => "instrument_ladder_restart",
            (2, _) => "instrument_ladder_forward_jump",
            (3, _) => "instrument_ladder_midcounter_open",
            _ => "instrument_ladder_violation",
        };
        let label = self.emit_child(token, start, content_start, parent, depth, code);
        if pass < 4 {
            self.stack.last_mut().unwrap().label = label;
        }
    }

    fn legislation_child(
        &mut self,
        token: &str,
        next: Option<&str>,
        start: usize,
        content_start: usize,
    ) {
        let (head, suffix) = token.split_once('.').unwrap_or((token, ""));
        let numeric = token
            .as_bytes()
            .first()
            .is_some_and(|value| value.is_ascii_digit());
        let roman = roman_value(head);
        let upper = head.bytes().any(|byte| byte.is_ascii_uppercase());
        let alpha_level = if upper { 4 } else { 2 };
        let alpha_value = head
            .as_bytes()
            .first()
            .filter(|value| value.is_ascii_alphabetic())
            .map(|value| u32::from(value.to_ascii_lowercase() - b'a' + 1));
        let prior = |family| {
            self.stack
                .iter()
                .find(|frame| frame.family == family)
                .map(|frame| frame.value.as_str())
        };
        let roman_preferred = if head.len() > 1 {
            true
        } else if let Some(value) = roman {
            if prior(3)
                .and_then(|prior| prior.parse::<u32>().ok())
                .is_some_and(|prior| prior + 1 == value)
            {
                true
            } else if alpha_value.is_some_and(|value| {
                prior(alpha_level)
                    .and_then(|prior| prior.parse::<u32>().ok())
                    .is_some_and(|prior| prior + 1 == value)
            }) {
                head.eq_ignore_ascii_case("i")
                    && next.is_some_and(|next| next.eq_ignore_ascii_case("ii"))
            } else {
                !upper && head == "i" && self.stack.iter().any(|frame| frame.family == 2)
            }
        } else {
            false
        };
        let (family, value) = if numeric {
            (1, token.to_owned())
        } else if roman_preferred {
            (3, roman.unwrap().to_string())
        } else {
            let Some(alpha) = alpha_value else { return };
            (
                alpha_level,
                if suffix.is_empty() {
                    alpha.to_string()
                } else {
                    format!("{alpha}.{suffix}")
                },
            )
        };
        if prior(family).is_some_and(|prior| !compare_child_values(&value, prior).is_gt()) {
            return;
        }

        self.stack.retain(|frame| frame.family <= family);
        let at = self.stack.iter().position(|frame| frame.family == family);
        let (root, root_depth) = self.section.as_ref().unwrap();
        let root_depth = *root_depth;
        let parent = at
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| self.stack.get(index))
            .or_else(|| self.stack.iter().rev().find(|frame| frame.family < family))
            .map_or_else(|| root.clone(), |frame| frame.label.clone());
        if let Some(index) = at {
            self.stack[index].value = value;
        } else {
            self.stack.push(EnumFrame {
                family,
                value,
                label: String::new(),
            });
        }
        let label = self.emit_child(
            token,
            start,
            content_start,
            parent,
            root_depth + usize::from(family),
            "legislation_child",
        );
        self.stack
            .iter_mut()
            .find(|frame| frame.family == family)
            .unwrap()
            .label = label;
    }
}

fn enumerated_children(
    text: &ScalarText<'_>,
    range: std::ops::Range<usize>,
    root: &str,
    content_start: usize,
    inline_at_root: bool,
    leading_label: Option<&str>,
) -> Vec<Block> {
    let bytes = text.byte(range.start)..text.byte(range.end);
    let value = &text.value[bytes.clone()];
    let mut markers = Vec::new();
    let leading = leading_label.and_then(|label| {
        let lead = leading_ascii_space(value);
        let rest = value[lead..].strip_prefix(label)?;
        let gap = leading_ascii_space(rest);
        let prefix = lead + label.len() + gap;
        let line = value[prefix..].lines().next().unwrap_or_default();
        let newline = line.len() < value[prefix..].len();
        let (token, content, close) = legislation_marker(line, newline)?;
        Some(ChildMark {
            token,
            start: text.scalar(bytes.start + prefix + close),
            content_start: text.scalar(bytes.start + prefix + content),
        })
    });
    if let Some(marker) = leading {
        markers.push(marker);
    } else {
        let inline = &text.value[text.byte(content_start)..bytes.end];
        let inline_line = inline.lines().next().unwrap_or_default();
        if let Some((token, at, _)) =
            legislation_marker(inline_line, inline_line.len() < inline.len())
        {
            let start = inline_at_root
                .then_some(range.start)
                .unwrap_or(content_start);
            markers.push(ChildMark {
                token,
                start,
                content_start: text.scalar(text.byte(content_start) + at),
            });
        }
    }
    let seeded_start = markers.first().map(|marker| marker.start);
    let (mut line_byte, mut line_scalar) = (bytes.start, range.start);
    for raw in value.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = line.trim_start_matches(instrument_space);
        let newline = line_byte + line.len() < bytes.end;
        if let Some((token, at, _)) = legislation_marker(trimmed, newline) {
            if seeded_start != Some(line_scalar) {
                markers.push(ChildMark {
                    token,
                    start: line_scalar,
                    content_start: text.scalar(line_byte + line.len() - trimmed.len() + at),
                });
            }
        }
        line_byte += raw.len();
        line_scalar += raw.chars().count();
    }
    if markers.is_empty() {
        return Vec::new();
    }
    markers.sort_by_key(|marker| marker.start);
    let mut state = StructureState {
        section: Some((root.to_owned(), 0)),
        ..Default::default()
    };
    for index in 0..markers.len() {
        let marker = &markers[index];
        state.legislation_child(
            marker.token,
            markers.get(index + 1).map(|next| next.token),
            marker.start,
            marker.content_start,
        );
    }
    let mut next_marker = 0;
    for (node, _) in &mut state.nodes {
        while markers
            .get(next_marker)
            .is_some_and(|marker| marker.start <= node.range.start)
        {
            next_marker += 1;
        }
        node.range.end = markers
            .get(next_marker)
            .map_or(range.end, |marker| marker.start);
        node.parent_label = Some(root.to_owned());
    }
    state
        .nodes
        .into_iter()
        .map(|(point, _)| point.into_section())
        .collect()
}

fn direct_section(value: &str) -> Option<(String, usize)> {
    if !value.starts_with("Section")
        && !value.starts_with("SECTION")
        && !value.as_bytes().first().is_some_and(u8::is_ascii_digit)
    {
        return None;
    }
    let found = cached_regex!(SECTION,
        r#"^(?:(?:Section|SECTION)\s+(\d{1,3}(?:\.\d{1,3})*[A-Za-z]?)[.)]?\s*[—–\-:]?\s*(["'“(A-Z].*|)|(\d{1,3}\.\d{1,3}(?:\.\d{1,3})*)\s+(["'“(A-Z].*)|((?:[0-4]?\d{1,2}|500))[.)]\s+(["'“(A-Z].*)|(\d{1,3}(?:\.\d{1,3}){0,3})[ \t]+(\(\d.*))$"#
    ).captures(value)?;
    let label = [1, 3, 5, 7]
        .into_iter()
        .find(|index| found.get(*index).is_some())?;
    Some((found[label].to_owned(), found.get(label + 1)?.start()))
}

fn instrument_top(value: &str, direct: bool) -> Option<(String, usize, bool)> {
    let possible_top = value
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(byte, b'A' | b'P' | b'D' | b'S' | b'E'))
        && [
            "ARTICLE", "Article", "PART", "Part", "DIVISION", "Division", "SCHEDULE", "Schedule",
            "EXHIBIT", "Exhibit", "ANNEX", "Annex", "APPENDIX", "Appendix",
        ]
        .into_iter()
        .any(|word| {
            value
                .strip_prefix(word)
                .and_then(|rest| rest.chars().next())
                .is_some_and(char::is_whitespace)
        });
    if possible_top {
        if let Some(found) = cached_regex!(TOP,
        r"^(?:(ARTICLE|Article|PART|Part|DIVISION|Division)\s+([IVXLCDM]+|\d{1,3})\b\s*[—–\-.:]?\s*(.*)|(SCHEDULE|Schedule|EXHIBIT|Exhibit|ANNEX|Annex|APPENDIX|Appendix)\s+([A-Z0-9][\w.\-]*)\s*[—–\-.:]?\s*(.*))$"
    ).captures(value) {
        let container = found.get(1).is_some();
        let (word, token, rest) = if container { (1, 2, 3) } else { (4, 5, 6) };
        let heading = &found[rest];
        if !container
            && !(heading.is_empty()
                || heading.starts_with(['"', '\'', '“', '('])
                || heading
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_uppercase()))
        {
            return None;
        }
        let word = found[word].to_ascii_lowercase();
        let prefix = match word.as_str() {
            "part" | "annex" => word.as_str(),
            "schedule" => &word[..5],
            _ => &word[..3],
        };
        let suffix = if container {
            found[token].parse().ok()
                .or_else(|| roman_value(&found[token]))?
                .to_string()
        } else {
            found[token].to_ascii_lowercase()
        };
        return Some((format!("{prefix}{suffix}"), found.get(rest)?.start(), true));
        }
    }
    direct
        .then_some(value)
        .and_then(direct_section)
        .map(|(label, at)| (format!("sec{label}"), at, false))
}

pub(super) fn detect_instrument_grammar(text: &ScalarText<'_>) -> Vec<GrammarPoint> {
    let mut lines = lines(text).collect::<Vec<_>>();
    let mut spine = statute_spine_from_lines(text, false, &lines)
        .into_iter()
        .peekable();
    let direct = spine.peek().is_none();
    let (dialects, dialect_markers) = admitted_dialects(text, &mut lines);
    let mut dialect_markers = dialect_markers.into_iter().peekable();
    let mut state = StructureState::default();
    for line in lines.iter().copied() {
        let value = line.text;
        if value.is_empty() {
            continue;
        }
        let dialect_marker = dialect_markers.next_if(|mark| mark.0 == line.byte_start);
        let start = line.scalar_start;
        let selected = spine
            .next_if(|mark| mark.start == start)
            .map(|mark| (format!("sec{}", mark.label), mark.content_start, false))
            .or_else(|| {
                instrument_top(value, direct).map(|(label, at, container)| {
                    (label, text.scalar(line.byte_start + at), container)
                })
            });
        if let Some((label, content_start, container)) = selected {
            let depth = usize::from(!container && state.container.is_some());
            state.nodes.push((
                GrammarPoint {
                    range: ScalarRange {
                        start,
                        end: usize::MAX,
                    },
                    label: label.clone(),
                    parent_label: (!container).then(|| state.container.clone()).flatten(),
                    content_start,
                    diagnostic: None,
                },
                depth,
            ));
            state.stack.clear();
            if container {
                state.container = Some(label);
                state.section = None;
            } else {
                state.section = Some((label, depth));
                let content_byte = text.byte(content_start);
                let inline = if content_byte <= line.byte_end {
                    &text.value[content_byte..line.byte_end]
                } else {
                    ""
                };
                if let Some((token, at)) = instrument_marker(inline, false, false) {
                    state.child(token, content_start, text.scalar(content_byte + at));
                }
            }
            continue;
        }
        if state.section.is_none() {
            continue;
        }
        let marker = match dialect_marker {
            Some((_, token, at, dialect)) if dialects[dialect] => Some((token, at)),
            Some(_) => None,
            None if value.starts_with('(') => instrument_marker(value, false, false),
            None => None,
        };
        if let Some((token, at)) = marker {
            state.child(token, start, text.scalar(line.byte_start + at));
        }
    }
    let mut open = Vec::<usize>::new();
    for index in 0..state.nodes.len() {
        let (start, depth) = (state.nodes[index].0.range.start, state.nodes[index].1);
        while open
            .last()
            .is_some_and(|prior| state.nodes[*prior].1 >= depth)
        {
            state.nodes[open.pop().expect("open node")].0.range.end = start;
        }
        open.push(index);
    }
    for index in open {
        state.nodes[index].0.range.end = text.len();
    }
    state.nodes.into_iter().map(|(point, _)| point).collect()
}

pub(crate) fn detect_instrument(text: &ScalarText<'_>) -> Vec<Block> {
    detect_instrument_grammar(text)
        .into_iter()
        .map(GrammarPoint::into_section)
        .collect()
}

fn detect_legislation(
    text: &ScalarText<'_>,
    allow_hyphenated_sections: bool,
    native_claims: &[NativeClaim],
) -> Vec<Block> {
    let sections = selected_sections(text, allow_hyphenated_sections);
    let merge_native = native_claims
        .iter()
        .any(|claim| claim.kind == EvidenceKind::Section && claim.parent_label.is_none());
    let mut result = Vec::new();
    let mut parent_blocks = HashMap::<String, Vec<usize>>::new();
    for (index, section) in sections.iter().enumerate() {
        let end = sections
            .get(index + 1)
            .map_or(text.len(), |value| value.start);
        let parent = format!("sec{}", section.label);
        let mut top = Block::labelled(NodeKind::Section, parent.clone(), section.start, end);
        top.aliases = section
            .aliases
            .iter()
            .map(|value| format!("sec{value}"))
            .collect();
        top.content_start = Some(section.content_start);
        result.push(top);
        let children = enumerated_children(
            text,
            section.start..end,
            &parent,
            section.content_start,
            matches!(section.family, SectionFamily::Bare | SectionFamily::DotTerm),
            None,
        );
        if merge_native && !children.is_empty() {
            parent_blocks
                .entry(parent.to_ascii_lowercase())
                .or_default()
                .extend(result.len()..result.len() + children.len());
        }
        result.extend(children);
    }
    if !merge_native {
        return result;
    }
    let mut retained = vec![true; result.len()];
    for claim in native_claims
        .iter()
        .filter(|claim| claim.kind == EvidenceKind::Section && claim.parent_label.is_none())
    {
        let Some(label) = claim
            .label
            .as_deref()
            .and_then(|value| value.strip_prefix("sec"))
        else {
            continue;
        };
        if !provision_label(label).is_some_and(|(value, end)| value == label && end == label.len())
        {
            continue;
        }
        let value = text
            .slice(claim.range.start..claim.range.end)
            .expect("native claim range is bounded");
        let lead = leading_ascii_space(value);
        let content_start = value[lead..]
            .strip_prefix(label)
            .map_or(claim.range.start, |_| {
                claim.range.start + lead + label.len()
            });
        let parent = format!("sec{label}");
        let children = enumerated_children(
            text,
            claim.range.start..claim.range.end,
            &parent,
            content_start,
            false,
            Some(label),
        );
        let positions = parent_blocks
            .entry(parent.to_ascii_lowercase())
            .or_default();
        for &index in positions.iter() {
            let block = &result[index];
            if retained[index]
                && block.range.start >= claim.range.start
                && block.range.end <= claim.range.end
            {
                retained[index] = false;
            }
        }
        positions.extend(result.len()..result.len() + children.len());
        retained.resize(result.len() + children.len(), true);
        result.extend(children);
    }
    let mut retained = retained.into_iter();
    result.retain(|_| retained.next().unwrap());
    result
}

fn add_ranges(mut blocks: Vec<Block>, length: usize) -> Vec<Block> {
    for index in 0..blocks.len() {
        blocks[index].range.end = blocks
            .get(index + 1)
            .map_or(length, |value| value.range.start);
    }
    blocks
}

fn detect_journal(text: &ScalarText<'_>) -> Vec<Block> {
    let source = javascript_lines(text);
    let section = cached_regex!(
        SECTION,
        r"(?u)^[ \t]*([IVXLCDM]+|[A-Z])\.[ \t]+([^\x08]{3,180})$"
    );
    let mut result = add_ranges(
        source
            .iter()
            .filter_map(|line| section.captures(line.text).map(|capture| (line, capture)))
            .map(|(line, capture)| {
                let whole = capture.get(0).unwrap();
                let lowercase = capture[2].to_lowercase();
                let alias = lowercase
                    .split(|value: char| !value.is_alphanumeric())
                    .filter(|value| !value.is_empty())
                    .fold(
                        String::with_capacity(lowercase.len()),
                        |mut alias, value| {
                            if !alias.is_empty() {
                                alias.push(' ');
                            }
                            alias.push_str(value);
                            alias
                        },
                    );
                let mut block = Block::labelled(
                    NodeKind::Section,
                    format!("sec{}", &capture[1]),
                    text.scalar(line.byte_start + whole.start()),
                    0,
                );
                block.aliases = std::iter::once(capture[1].to_owned())
                    .chain((!alias.is_empty()).then(|| format!("sectitle:{alias}")))
                    .collect();
                block
            })
            .collect(),
        text.len(),
    );
    let note = cached_regex!(NOTE, r"(?u)^[ \t]*(\d{1,5})\t[ \t]*$");
    result.extend(add_ranges(
        source
            .iter()
            .filter_map(|line| note.captures(line.text).map(|capture| (line, capture)))
            .map(|(line, capture)| {
                let whole = capture.get(0).unwrap();
                let mut block = Block::labelled(
                    NodeKind::Footnote,
                    format!("fn{}", capture[1].parse::<u32>().unwrap()),
                    text.scalar(line.byte_start + whole.start()),
                    0,
                );
                block.aliases.push(capture[1].to_owned());
                block
            })
            .collect(),
        text.len(),
    ));
    let mut start = None;
    for (index, line) in source.iter().enumerate() {
        let blank = line.text.trim_matches([' ', '\t', '\r']).is_empty();
        if start.is_none() && !blank {
            start = line
                .text
                .chars()
                .position(|value| !javascript_whitespace(value))
                .map(|at| line.scalar_start + at);
        }
        let next_blank = source
            .get(index + 1)
            .is_some_and(|value| value.text.trim_matches([' ', '\t', '\r']).is_empty());
        if let Some(block_start) = start.filter(|_| next_blank || index + 1 == source.len()) {
            let end = if index + 1 == source.len() {
                text.len()
            } else {
                text.scalar(line.byte_end)
            };
            let value = text
                .slice(block_start..end)
                .expect("journal block range is bounded");
            let first_line = value.split_once('\n').map_or(value, |(line, _)| line);
            let page_marker = first_line
                .find(']')
                .and_then(|end| crate::locator::literal_page_marker(&first_line[..=end], true))
                .is_some_and(|label| !label.is_empty() && label.len() <= 40);
            let block_start = if page_marker {
                value
                    .find('\n')
                    .map_or(end, |at| text.scalar(text.byte(block_start) + at + 1))
            } else {
                block_start
            };
            if block_start < end {
                result.push(Block {
                    kind: NodeKind::Prose,
                    range: ScalarRange {
                        start: block_start,
                        end,
                    },
                    label: None,
                    aliases: Vec::new(),
                    parent_label: None,
                    content_start: None,
                    diagnostic: None,
                    source: Derivation::Heuristic,
                    origin_id: ENGINE_ORIGIN,
                });
            }
            start = None;
        }
    }
    result
}

pub(super) fn inferred_blocks(evidence: &DocumentInput, text: &ScalarText<'_>) -> Vec<Block> {
    match evidence.profile {
        DetectionProfile::Legislation => detect_legislation(
            text,
            evidence.allow_hyphenated_sections,
            &evidence.native_claims,
        ),
        DetectionProfile::Instrument => detect_instrument(text),
        DetectionProfile::Journal => detect_journal(text),
        _ => {
            let mut exclusions = evidence
                .exclusions
                .iter()
                .filter(|value| value.applies_to.iter().any(|name| name == "paragraph"))
                .map(|value| value.range)
                .collect::<Vec<_>>();
            exclusions.sort_unstable_by_key(|range| (range.start, range.end));
            detect_case(
                text,
                evidence.profile,
                evidence.report_start_page,
                evidence.require_report_start,
                &exclusions,
            )
        }
    }
}
