//! Legislation's sections: their labels, families and scopes.

use super::*;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum SectionStyle {
    Integer,
    Dot,
    DotTerm,
    Hyphen,
    Mixed,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum SectionFamily {
    Bare,
    DotTerm,
    Markdown,
    Emphasis,
    Range,
}

#[derive(Clone)]
pub(crate) struct SectionMark {
    pub(crate) label: String,
    pub(crate) start: usize,
    pub(crate) content_start: usize,
    pub(crate) style: SectionStyle,
    pub(crate) family: SectionFamily,
    pub(crate) aliases: Vec<String>,
}

pub(super) struct LabelPart<'a> {
    separator: char,
    digits: Option<&'a str>,
    text: &'a str,
    suffix: u32,
}

/// A label's letter suffix as a number ("a" 1, "z" 26, "aa" 27); only letters reach it.
pub(super) fn suffix_value(value: &str) -> u32 {
    value.bytes().fold(0u32, |total, value| {
        total.saturating_mul(26).saturating_add(u32::from(
            value
                .to_ascii_uppercase()
                .wrapping_sub(b'A')
                .wrapping_add(1),
        ))
    })
}

pub(super) fn label_parts(label: &str) -> impl Iterator<Item = LabelPart<'_>> {
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
            // A part that is not a number and a letter suffix ("Schedule", "2(a)") orders by its text.
            suffix: numeric.map_or(0, |_| suffix_value(&body[digits..])),
        };
        separator = next;
        Some(value)
    })
}

pub(crate) fn compare_labels(left: &str, right: &str, fraction: bool) -> std::cmp::Ordering {
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

pub(super) fn numeric_label(value: &str, markdown: bool) -> Option<(&str, usize)> {
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
        // Saskatchewan prints its part-numbered sections with a non-breaking hyphen ("1‑3").
        let Some(separator) = value[end..]
            .chars()
            .next()
            .filter(|value| matches!(value, '.' | '-' | '\u{2010}' | '\u{2011}'))
        else {
            break;
        };
        let digits = bytes[end + separator.len_utf8()..]
            .iter()
            .take(8)
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits == 0 {
            break;
        }
        end += digits + separator.len_utf8();
    }
    end += bytes[end..]
        .iter()
        .take(2)
        .take_while(|byte| byte.is_ascii_uppercase())
        .count();
    let label = &value[..end];
    (!markdown || label.contains(['.', '-', '\u{2010}', '\u{2011}'])).then_some((label, end))
}

pub(crate) fn provision_label(value: &str) -> Option<(&str, usize)> {
    let matched = cached_regex!(VALUE,
    r"^(?:\d{1,8}[A-Za-z]{0,3}(?:[.-]\d{1,8}[A-Za-z]{0,3}){0,3}|[A-Za-z]{1,3}(?:[.-][0-9A-Za-z]{1,8}){1,3})"
).find(value)?;
    Some((matched.as_str(), matched.end()))
}

pub(super) fn section_style(label: &str, trailing: bool) -> SectionStyle {
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

pub(super) fn bare_content_starts(value: &str) -> bool {
    value.chars().next().is_some_and(|character| {
        character.is_alphabetic() || character.is_ascii_digit() || "([*“\"«".contains(character)
    })
}

pub(super) fn dotterm_content_starts(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|character| character.is_alphanumeric() || "\"'“«(".contains(character))
}

pub(super) fn markdown_range_continuation(value: &str) -> bool {
    cached_regex!(
        VALUE,
        r"(?iu)^[ \t]*#{1,6}[ \t]+.*(?:[ \t](?:to|à)|[-–—])[ \t]*$"
    )
    .is_match(value)
}

/// Enactment-history notes printed under provisions ("2009 c50 s7", "2003 cP‑6.5 s13;2009 c50 s6",
/// "2014, c. 28, s. 6") begin with a year followed by a chapter reference; the year is not a
/// section number.
pub(super) fn enactment_history_note(label: &str, after_label: &str) -> bool {
    label.len() == 4
        && label.starts_with(['1', '2'])
        && cached_regex!(
            VALUE,
            r"^,?[ \t]*c\.?[ \t]?[A-Za-z]{0,3}[\u{2010}\u{2011}-]?[0-9][0-9A-Za-z.\u{2010}\u{2011}-]*(?:[ \t,;]|$)"
        )
        .is_match(after_label)
}

pub(super) fn section_mark(
    text: &ScalarText<'_>,
    line: &Line<'_>,
    family: SectionFamily,
    previous_nonblank: Option<&str>,
    provider_refinements: bool,
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
    // A printed statute may set its number apart with a wide space (an em space).
    let spaces = rest.len()
        - rest
            .trim_start_matches(|c: char| c.is_whitespace() && !matches!(c, '\n' | '\r'))
            .len();
    let content = &rest[spaces..];
    let accepted = match family {
        // A number whose next group is three digits is one amount set with spaced thousands
        // ("1 687 106,60 USD", "12 500 $"), not a section and its text.
        SectionFamily::Bare => {
            content.is_empty()
                || (spaces > 0
                    && bare_content_starts(content)
                    && !cached_regex!(GROUPED, r"^\d{3}(?:[^\d]|$)").is_match(content))
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
        || provider_refinements && enactment_history_note(label, &value[length..])
    {
        return None;
    }
    let label = label.replace(['\u{2010}', '\u{2011}'], "-");
    Some(SectionMark {
        style: section_style(&label, trailing),
        label,
        start: text.scalar(line.byte_start + lead),
        content_start: text.scalar(line.byte_end - content.len()),
        family,
        aliases: Vec::new(),
    })
}

pub(super) fn collect_section_families(
    text: &ScalarText<'_>,
    source: &[Line<'_>],
    provider_refinements: bool,
) -> [Vec<SectionMark>; 3] {
    let mut result = std::array::from_fn(|_| Vec::new());
    let mut previous_nonblank = None;
    // (non-blank line ordinal, index into the bare family) of number-only lines.
    let mut number_only = Vec::<(usize, usize)>::new();
    let mut ordinal = 0;
    for source_line in source {
        let line = source_line.text.trim_start_matches([' ', '\t']);
        if !line.trim().is_empty() {
            ordinal += 1;
        }
        let bare_before = result[0].len();
        let number_line = !line.trim().is_empty()
            && line
                .trim()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.');
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
                if let Some(mark) = section_mark(
                    text,
                    source_line,
                    family,
                    previous_nonblank,
                    provider_refinements,
                ) {
                    marks.push(mark);
                }
            }
        } else if line.starts_with('#') {
            if let Some(mark) = section_mark(
                text,
                source_line,
                SectionFamily::Markdown,
                previous_nonblank,
                provider_refinements,
            ) {
                result[2].push(mark);
            }
        }
        if number_line && result[0].len() > bare_before {
            number_only.push((ordinal, bare_before));
        }
        if !source_line.text.trim().is_empty() {
            previous_nonblank = Some(source_line.text);
        }
    }
    // PDF candidate runs retain their original broad observations. Provider
    // text can reject contents columns, regnal heads and isolated years here.
    if !provider_refinements {
        return result;
    }
    // A table-of-contents column prints "1", "2", "3" … on consecutive lines above the titles. Only a
    // run of three or more number-only lines counting 1, 2, 3 … is taken for one; repealed sections
    // printed as bare numbers ("5.3", "5.4") and table columns do not start at 1.
    let mut contents = HashSet::new();
    let mut run_start = 0;
    let counts_from_one = |run: &[(usize, usize)]| {
        run.iter()
            .enumerate()
            .all(|(offset, (_, mark))| result[0][*mark].label == (offset + 1).to_string())
    };
    for index in 1..=number_only.len() {
        if index < number_only.len() && number_only[index].0 == number_only[index - 1].0 + 1 {
            continue;
        }
        let run = &number_only[run_start..index];
        if run.len() >= 3 && counts_from_one(run) {
            contents.extend(run.iter().map(|(_, mark)| *mark));
        }
        run_start = index;
    }
    if !contents.is_empty() {
        let bare = std::mem::take(&mut result[0]);
        result[0] = bare
            .into_iter()
            .enumerate()
            .filter_map(|(index, mark)| (!contents.contains(&index)).then_some(mark))
            .collect();
    }
    // The regnal-year running head of printed statutes ("68 ELIZ. 2", "12-13 GEO. VI") is page
    // furniture, not a section.
    let regnal = cached_regex!(
        VALUE,
        r"^\d{1,2}(?:[-–]\d{1,2})?\s+(?:ELIZ|Eliz|GEO|Geo|EDW|Edw|VICT|Vict|WILL|Will)\.?\s+(?:\d{1,2}|[IVX]{1,5})\.?$"
    );
    let running = |mark: &SectionMark| {
        let start = text.byte(mark.start);
        let end = text.value[start..]
            .find('\n')
            .map_or(text.value.len(), |at| start + at);
        regnal.is_match(text.value[start..end].trim())
    };
    for marks in &mut result {
        marks.retain(|mark| !running(mark));
    }
    result.map(drop_isolated_year_sections)
}

pub(super) fn section_key(label: &str) -> impl Iterator<Item = u64> + '_ {
    label.split(['.', '-']).filter_map(|value| {
        value
            .bytes()
            .take_while(u8::is_ascii_digit)
            .fold(None, |total, digit| {
                Some(total.unwrap_or(0) * 10 + u64::from(digit - b'0'))
            })
    })
}

pub(super) fn section_scopes<'a>(
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

pub(super) fn expand_descendants<'a>(
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
        // A provision inserted after one whose own number is not printed ("668 and 669
        // [Repealed]" then "669.1") sits under the parent before it: its whole number is the one
        // right after the parent's, and the next parent's is higher still.
        let next_root = parents
            .peek()
            .and_then(|value| section_key(&value.label).next());
        let mut descendants = Vec::new();
        let mut counts = HashMap::<&str, usize>::new();
        for &mark in &marks[begin..cursor] {
            let own = section_key(&mark.label).next();
            if matches!(mark.style, SectionStyle::Dot | SectionStyle::DotTerm)
                && mark.label.contains('.')
                && (own == root
                    || own == root.map(|root| root + 1)
                        && next_root.is_some_and(|next| own < Some(next)))
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

pub(super) fn same_labels(left: &[&SectionMark], right: &[&SectionMark]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.label == right.label)
}

pub(super) fn choose_sections<'a>(
    left: Option<Vec<&'a SectionMark>>,
    right: Option<Vec<&'a SectionMark>>,
) -> Option<Vec<&'a SectionMark>> {
    match (left, right) {
        (None, value) | (value, None) => value,
        (Some(left), Some(right)) if same_labels(&left, &right) => Some(left),
        (Some(left), Some(right)) if left[0].start != right[0].start => {
            let (early, late) = if left[0].start < right[0].start {
                (left, right)
            } else {
                (right, left)
            };
            // The reading that starts first wins, unless the other, longer, reads nearly all its
            // provisions too: then the first is a part of it that a stray number ahead of the
            // body (a contents entry's leftover) made start sooner.
            let read = late
                .iter()
                .map(|mark| mark.label.as_str())
                .collect::<HashSet<_>>();
            let shared = early
                .iter()
                .filter(|mark| read.contains(mark.label.as_str()))
                .count();
            Some(
                if late.len() > early.len() && shared * 10 >= early.len() * 9 {
                    late
                } else {
                    early
                },
            )
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

pub(super) fn section_guard(value: &[&SectionMark], text: &ScalarText<'_>) -> bool {
    let length = text.utf16_len();
    !value.is_empty() && length > 0 && text.utf16(value[0].start) as f64 / length as f64 <= 0.7
}

pub(super) fn scope_winner<'a>(
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
        // The same provisions numbered twice, once as a contents list (each number with its
        // title alone, no sentence ended before the next) and once with their text: the second
        // is the body.
        let listed = |scope: &[&SectionMark]| {
            scope.windows(2).all(|pair| {
                let (from, to) = (text.byte(pair[0].content_start), text.byte(pair[1].start));
                from <= to
                    && !text.value[from..to]
                        .lines()
                        .any(|line| line.trim_end().ends_with(['.', ';', ':']))
            })
        };
        let better = scope.len() > current.len()
            || scope.len() == current.len()
                && if same_labels(&scope, current) && listed(current) != listed(&scope) {
                    listed(current)
                } else {
                    scope[0].start < current[0].start
                };
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

pub(super) fn statute_winner<'a>(
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

pub(super) fn inline_section(text: &ScalarText<'_>, mark: &SectionMark) -> bool {
    let start = text.byte(mark.content_start);
    let end = text.value[start..]
        .find('\n')
        .map_or(text.value.len(), |value| start + value);
    !text.value[start..end].trim().is_empty()
}

pub(super) fn next_nonblank<'a>(source: &'a [Line<'a>], start: usize) -> Option<&'a Line<'a>> {
    source[source.partition_point(|line| line.scalar_start <= start)..]
        .iter()
        .find(|line| !line.text.trim().is_empty())
}

pub(super) fn short_root(
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

pub(super) fn statute_spine_over(
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

pub(super) fn statute_spine_from_lines(
    text: &ScalarText<'_>,
    allow_hyphen: bool,
    source: &[Line<'_>],
    provider_refinements: bool,
) -> Vec<SectionMark> {
    let mut families = collect_section_families(text, source, provider_refinements);
    // A section's number repeated on each of its subsections ("3.02(1) The ...", "3.02(2) A
    // ...") opens the section once; the later ones are its subsections.
    for marks in &mut families {
        let mut last = None::<String>;
        marks.retain(|mark| {
            let repeated = last.as_deref() == Some(mark.label.as_str())
                && repeated_subsection(&text.value[text.byte(mark.content_start)..]);
            last = Some(mark.label.clone());
            !repeated
        });
    }
    let spine = |allow_hyphen| {
        let result = statute_spine_over(text, allow_hyphen, false, &families, source);
        if result.is_empty() || result.iter().any(|value| inline_section(text, value)) {
            result
        } else {
            // Prefer sections with text on the number's line, but a statute that prints every
            // number on its own line (as printed BC Acts do) keeps its own-line run.
            let inline = statute_spine_over(text, allow_hyphen, true, &families, source);
            if provider_refinements && inline.is_empty() {
                result
            } else {
                inline
            }
        }
    };
    match spine(allow_hyphen) {
        // A statute numbering every section within its part ("1-1", "2-3") has no other spine.
        result if result.is_empty() && !allow_hyphen => spine(true),
        result => result,
    }
}

#[cfg(test)]
pub(crate) fn statute_spine(text: &ScalarText<'_>, allow_hyphen: bool) -> Vec<SectionMark> {
    statute_spine_from_lines(text, allow_hyphen, &lines(text).collect::<Vec<_>>(), true)
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

pub(super) fn emphasis_sections(text: &ScalarText<'_>, source: &[Line<'_>]) -> Vec<SectionMark> {
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

pub(super) fn status_sections(text: &ScalarText<'_>, allow_hyphen: bool) -> Vec<SectionMark> {
    let regex = cached_regex!(
        VALUE,
        r"(?imu)^[ \t]*(?:\*\*)?(\d{1,4})(?:[ \t]+(?:to|through|and|à|a|et)[ \t]+|[ \t]*([-–—])[ \t]*)(\d{1,4})(?:\*\*)?[ \t]*[,;:]?[ \t]*(?:\[[ \t]*)?(?:repealed|revoked|abrog(?:ated|é|ée|és|ées)|renumbered|spent|not (?:yet )?in force|omitted)\b"
    );
    // A range is read within its line: the grammar crosses no line break. It opens its line with
    // its first number, after spaces and emphasis; a number in other digits than ASCII reads as
    // none, so only a line opening with an ASCII digit is tried.
    let mut line_start = 0;
    text.value
        .split('\n')
        .filter_map(|line| {
            let start = line_start;
            line_start += line.len() + 1;
            let opening = line.trim_start_matches([' ', '\t']);
            let opening = opening.strip_prefix("**").unwrap_or(opening);
            opening
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_digit)
                .then(|| regex.captures(line).map(|capture| (start, capture)))
                .flatten()
        })
        .filter_map(|(line_start, capture)| {
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
                start: text.scalar(line_start + whole.start() + leading_ascii_space(whole.as_str())),
                content_start: text.scalar(line_start + whole.end()),
                style: SectionStyle::Integer,
                family: SectionFamily::Range,
                aliases: (from + 1..=to).map(|value| value.to_string()).collect(),
            })
        })
        .collect()
}

pub(super) fn coherent_sections(marks: &[SectionMark]) -> bool {
    let Some(fraction) = dotted_order(marks.iter().map(|mark| mark.label.as_str())) else {
        return false;
    };
    marks
        .windows(2)
        .all(|pair| compare_labels(&pair[0].label, &pair[1].label, fraction).is_lt())
}

pub(super) fn selected_sections(text: &ScalarText<'_>, allow_hyphen: bool) -> Vec<SectionMark> {
    let source = lines(text).collect::<Vec<_>>();
    let emphasis = emphasis_sections(text, &source);
    let flat = statute_spine_from_lines(text, allow_hyphen, &source, true);
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

/// A year-shaped section label ("2007 Convention", "1973" in a history or schedule line) that jumps
/// from a section below 1000 with no neighbouring number is text, not a section; genuine runs such
/// as 1799, 1800, 1801 keep their neighbours.
pub(super) fn drop_isolated_year_sections(marks: Vec<SectionMark>) -> Vec<SectionMark> {
    let leading = |mark: &SectionMark| {
        mark.label.split(['.', '-']).next().and_then(|value| {
            value
                .trim_end_matches(|c: char| c.is_ascii_alphabetic())
                .parse::<u32>()
                .ok()
        })
    };
    let values = marks.iter().map(leading).collect::<Vec<_>>();
    // Nearest candidates with a different label; repeated lines of the same text do not vouch.
    let neighbour = |index: usize, forward: bool| {
        let label = &marks[index].label;
        let mut cursor = index;
        loop {
            cursor = if forward {
                cursor + 1
            } else {
                cursor.checked_sub(1)?
            };
            let mark = marks.get(cursor)?;
            if &mark.label != label {
                return values[cursor];
            }
        }
    };
    let keep = (0..marks.len())
        .map(|index| {
            let Some(year) = values[index].filter(|_| marks[index].label.len() == 4) else {
                return true;
            };
            let near = |value: Option<u32>| value.is_some_and(|value| value.abs_diff(year) <= 3);
            let (previous, next) = (neighbour(index, false), neighbour(index, true));
            !((1800..=2099).contains(&year)
                && previous.is_some_and(|value| value < 1000)
                && !near(previous)
                && !near(next))
        })
        .collect::<Vec<_>>();
    let marks = marks
        .into_iter()
        .zip(keep)
        .filter_map(|(mark, keep)| keep.then_some(mark))
        .collect::<Vec<_>>();
    drop_integer_spikes(marks)
}

/// A lone far jump in a run of plain integer sections ("… 204, 900, 205 …", a wrapped line or
/// table row) would otherwise strand every later section in a separate run.
pub(super) fn drop_integer_spikes(marks: Vec<SectionMark>) -> Vec<SectionMark> {
    let integer = |mark: &SectionMark| {
        mark.label
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| mark.label.parse::<u32>().ok())
            .flatten()
    };
    let positions = (0..marks.len())
        .filter(|&index| integer(&marks[index]).is_some())
        .collect::<Vec<_>>();
    let mut spikes = HashSet::new();
    // The run must visibly continue for two steps after the spike.
    for window in positions.windows(4) {
        let [a, x, b, c] = [window[0], window[1], window[2], window[3]]
            .map(|index| integer(&marks[index]).unwrap());
        if a < b && b <= a + 3 && b < c && c <= b + 3 && x > b.saturating_add(20) {
            spikes.insert(window[1]);
        }
    }
    marks
        .into_iter()
        .enumerate()
        .filter_map(|(index, mark)| (!spikes.contains(&index)).then_some(mark))
        .collect()
}

pub(super) fn roman_value(value: &str) -> Option<u32> {
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

/// Where a provider's markdown opens a schedule between `from` and `end` ("## Schedule 1", "## SCHEDULE I —
/// (Section 2)", "## Schedule Reference Generating Unit …"), else `end`: a provision ends where a schedule begins. A
/// heading that only names one ("### Schedule of rates") is a marginal note.
pub(super) fn schedule_start(text: &ScalarText<'_>, from: usize, end: usize) -> usize {
    let heading = cached_regex!(
        VALUE,
        r"^[ \t]*#{1,6}[ \t]+(?:SCHEDULE|Schedule|ANNEX|Annex)(?:[ \t]*$|[ \t]*[^a-z \t])"
    );
    let index = text.lines();
    let first = index.partition_point(|line| line[2] <= from);
    let last = index.partition_point(|line| line[2] < end);
    index[first..last.max(first)]
        .iter()
        .find(|line| heading.is_match(&text.value[line[0]..line[1]]))
        .map_or(end, |line| line[2])
}

pub(super) fn detect_legislation(
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
        let end = schedule_start(text, section.content_start, end);
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
