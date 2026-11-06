//! Formal and sentence headings, as a judgment's text prints them.

use super::*;
use legal_structure_model::layout::ends_on;

pub(crate) fn heading_enumerator(value: &str) -> bool {
    // Every enumerator the grammar admits ends in a point or a closing parenthesis, but a section's own figures: "5.2.2".
    value.ends_with(['.', ')']) && cached_regex!(VALUE, r"^(?:\([\p{L}\p{N}]{1,5}\)|\p{L}[.)]|[IVXLCDM]{1,4}[.)]|[ivxlcdm]{1,4}[.)]|\d{1,3}(?:\.\d{1,3})*[.)])$").is_match(value)
        || value.contains('.') && cached_regex!(SECTION, r"^\d{1,3}(?:\.\d{1,3}){2,}$").is_match(value)
}

pub(super) fn level_opens(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|character| character.is_uppercase() || character.is_numeric())
}

pub(super) fn heading_level(words: &[&str], enumerated: bool) -> bool {
    if words.is_empty() || (!enumerated && words.len() > 12) {
        return false;
    }
    if words.len() == 1 && heading_enumerator(words[0]) {
        return true;
    }
    let last = words.last().unwrap();
    const CLOSERS: [char; 4] = ['"', '\u{201d}', '\u{bb}', '\u{2019}'];
    // (A sentence may close on its quotation: "(3d) 88, Gillese J.A. said, ... is still developing.”".)
    // (Not on a name's initials, under an enumerator: "I. The Sexual Activity with A.G.".)
    let initials = enumerated && words.len() <= 12 && last.trim_end_matches('.').contains('.') && last.chars().all(|c| c.is_uppercase() || c == '.');
    let closed = ends_on(last, &CLOSERS, &['.', ',', ';']) && !initials;
    // (Or on a note's call after its stop: "... as an adult.14", "... costs awards.”60".)
    let bare = last.trim_end_matches(|c: char| c.is_ascii_digit());
    let called = bare != *last && bare.trim_end_matches(CLOSERS).strip_suffix(['.', '?', '!']).is_some_and(|word| word.ends_with(char::is_alphabetic));
    // (A heading closes no bracket it never opened, "Legal Probs. 23)", "... was English.)"; a clause's letter does, "de l'al.
    // 11b)", "... Conspiracy?a)", "Le sous-al. 686(1)b)(iii)".)
    let clause = |word: &str| cached_regex!(CLAUSE, r"(?:^|[^\p{L}])\p{Ll}\)").is_match(word);
    let count = |bracket: char| words.iter().filter(|word| !clause(word)).map(|word| word.matches(bracket).count()).sum::<usize>();
    if !level_opens(words[0]) || closed || called || count(')') > count('(') {
        return false;
    }
    if last.ends_with(['?', ':']) {
        return true;
    }
    heading_title_case(words) || enumerated || words.len() <= 6
}

pub(super) fn heading_title_case(words: &[&str]) -> bool {
    words.iter().all(|word| {
        let (length, first) = word.chars().filter(|value| value.is_alphabetic()).fold(
            (0, None),
            |(length, first), character| {
                (length + character.len_utf16(), first.or(Some(character)))
            },
        );
        length < 4 || first.is_some_and(char::is_uppercase)
    })
}

pub(super) fn trim_leading_parenthetical(value: &str) -> &str {
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

pub(crate) fn formal_heading(value: &str) -> bool {
    heading_levels(value).is_some()
}

/// A formal heading's path, a level at a time, each with its enumerator ("IV. Analysis A. The
/// Right ..." is "IV. Analysis" then "A. The Right ..."), as A2AJ runs a decision's heading path
/// onto one line; None where the value is no formal heading.
pub(crate) fn heading_levels(value: &str) -> Option<Vec<String>> {
    heading_levels_within(value, false).or_else(|| cited_heading(value).then(|| vec![value.trim().to_owned()]))
}

/// A heading naming a decision by its citation ("(1) British Columbia Supreme Court (Ehrcke J.), 2008 BCSC 1726, 90
/// B.C.L.R. (4th) 177", a judgment's history): read with each citation one word, its points and brackets no level's.
fn cited_heading(value: &str) -> bool {
    if value.len() > 240 || !value.bytes().any(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let mut spans = crate::analysis::citation_spans(value);
    if spans.is_empty() {
        return false;
    }
    spans.sort_by_key(|span| std::cmp::Reverse(span.range.start));
    let mut masked = value.to_owned();
    for span in spans {
        if masked.is_char_boundary(span.range.start) && masked.is_char_boundary(span.range.end) && span.range.start < span.range.end {
            masked.replace_range(span.range.clone(), "Citation");
        }
    }
    heading_levels_within(&masked, false).is_some()
}

/// [`heading_levels`] for a path the paragraph number after it on its line vouches for: each
/// level may run as long as a heading's line, and a level may hold a semicolon ("V. Analysis A.
/// Was there abuse of authority in the assessment of the merit criteria, particularly at the
/// interview and informal discussion stages? 45 Subsection ...").
pub(crate) fn joined_heading_levels(value: &str) -> Option<Vec<String>> {
    heading_levels_within(value, true)
}

pub(super) fn heading_levels_within(value: &str, joined: bool) -> Option<Vec<String>> {
    let heading = trim_leading_parenthetical(value);
    let excluded = if joined { "![]{}" } else { ";![]{}" };
    if heading.is_empty()
        || !joined && utf16_longer(heading, 120)
        || heading.chars().any(|value| excluded.contains(value))
    {
        return None;
    }
    let words = heading.split_whitespace().collect::<Vec<_>>();
    let mut start = 0;
    let mut enumerated = heading != value.trim();
    let mut levels = Vec::new();
    let mut level = 0;
    for (index, word) in words.iter().enumerate() {
        let opener = |next: &&str| !heading_enumerator(next) && level_opens(next);
        if heading_enumerator(word) && words.get(index + 1).is_some_and(opener) {
            if start < index && !heading_level(&words[start..index], enumerated) {
                return None;
            }
            if index > level {
                levels.push(words[level..index].join(" "));
            }
            level = index;
            start = index + 1;
            enumerated = true;
        }
    }
    if !heading_level(&words[start..], enumerated) {
        return None;
    }
    levels.push(words[level..].join(" "));
    if joined && levels.iter().any(|level| utf16_longer(level, 160)) {
        return None;
    }
    // A leading parenthesized enumerator ("(2) Remedy") is its first level's.
    let prefix = value
        .trim()
        .strip_suffix(heading)
        .unwrap_or_default()
        .trim();
    if !prefix.is_empty() {
        levels[0] = format!("{prefix} {}", levels[0]);
    }
    Some(levels)
}

/// Whether a value runs past `limit` UTF-16 code units, read no further than that: a char
/// is never fewer UTF-8 bytes than UTF-16 units, so a value no longer in bytes is not read.
pub(super) fn utf16_longer(value: &str, limit: usize) -> bool {
    let mut length = 0;
    value.len() > limit
        && value.chars().any(|character| {
            length += character.len_utf16();
            length > limit
        })
}

/// Whether a line may head what follows it rather than being matter the judgment quotes: a table's
/// figure ("1,000.00") has no words, and a quoted enactment's provision opens its first subsection
/// on its number's line ("399. (1) On motion, the Court may …", "6 (1) Except where …").
pub(crate) fn may_head(line: &str) -> bool {
    // (A table's row: "David E. Aiken, George W. Matthews \u{2192} G. Bradley Simon (1995)".)
    if line.contains('\u{2192}') {
        return false;
    }
    let mut words = line.split_whitespace();
    let provision = words.next().is_some_and(|first| first.trim_end_matches('.').bytes().all(|byte| byte.is_ascii_digit() || byte == b'.'))
        && words.next().is_some_and(|second| {
            second.len() >= 3 && second.starts_with('(') && second.ends_with(')')
                && second[1..second.len() - 1].bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    line.chars().any(char::is_alphabetic) && !provision
}

/// What follows a line, read for whether the line heads it: without the number of the paragraph it
/// opens ("[11] The court ...").
pub(crate) fn after_marker(following: &str) -> &str {
    let following = following.trim_start();
    following
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .filter(|(number, _)| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        .map_or(following, |(_, rest)| rest)
}

/// A line standing between paragraphs read as the heading of what follows it (`following`): its path
/// a level at a time ("IV. Analysis A. The Right ..."), or a sentence heading as one level.
pub(crate) fn heading_between(line: &str, following: &str) -> Option<Vec<String>> {
    if !may_head(line) {
        return None;
    }
    heading_levels(line).or_else(|| sentence_heading(line, after_marker(following)).then(|| vec![line.to_owned()]))
}

pub(crate) fn sentence_heading(value: &str, following: &str) -> bool {
    let heading = trim_leading_parenthetical(value);
    if utf16_longer(heading, 120) {
        return false;
    }
    let mut words = heading.split_whitespace();
    let word_count = words.clone().count();
    (4..=18).contains(&word_count)
        && heading.chars().next().is_some_and(char::is_uppercase)
        && words.any(|word| word.chars().next().is_some_and(char::is_lowercase))
        && !heading.chars().any(|value| "[].,;:!?".contains(value))
        && following
            .trim_start()
            .chars()
            .next()
            .is_some_and(char::is_uppercase)
}
