//! An instrument's enumerated grammar.

use super::*;

pub(super) struct EnumFrame {
    family: u8,
    value: String,
    label: String,
}

pub(super) struct ChildMark<'a> {
    token: &'a str,
    start: usize,
    content_start: usize,
}

pub(crate) struct GrammarPoint {
    pub(crate) range: ScalarRange,
    pub(crate) label: String,
    pub(crate) parent_label: Option<String>,
    pub(crate) content_start: usize,
    pub(crate) diagnostic: Option<&'static str>,
    /// A section read from its number alone, in a text with no statute's run of sections.
    pub(crate) direct: bool,
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
            marker_range: None,
            note: None,
            diagnostic: self.diagnostic,
            source: Derivation::Heuristic,
            origin_id: ENGINE_ORIGIN,
        }
    }
}

#[derive(Default)]
pub(super) struct StructureState {
    nodes: Vec<(GrammarPoint, usize)>,
    container: Option<String>,
    section: Option<(String, usize)>,
    stack: Vec<EnumFrame>,
    used: HashMap<String, usize>,
}

pub(crate) fn inserted_letter_token(token: &str) -> Option<(&str, &str)> {
    let (letter, suffix) = token.split_once('.')?;
    (letter.len() == 1
        && letter.as_bytes()[0].is_ascii_alphabetic()
        && !suffix.is_empty()
        && suffix.bytes().all(|byte| byte.is_ascii_digit()))
    .then_some((letter, suffix))
}

pub(crate) fn enum_increment(family: u8, prior: &str, value: &str) -> bool {
    if let (Ok(prior), Ok(value)) = (prior.parse::<u32>(), value.parse::<u32>()) {
        return prior.checked_add(1) == Some(value);
    }
    if family == 0 {
        return compare_labels(value, prior, true).is_gt();
    }
    let base = |value: &str| {
        value
            .split('.')
            .next()
            .unwrap()
            .parse::<u32>()
            .expect("enumerator readings have a numeric base")
    };
    let (before, after) = (base(prior), base(value));
    (before == after && compare_child_values(value, prior).is_gt())
        || before.checked_add(1) == Some(after)
}

pub(crate) fn enum_readings(token: &str) -> [Option<(u8, String)>; 2] {
    if let Some((letter, suffix)) = inserted_letter_token(token) {
        return enum_readings(letter)
            .map(|reading| reading.map(|(family, value)| (family, format!("{value}.{suffix}"))));
    }
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
pub(super) enum InstrumentMarkerKind {
    Parenthesized,
    Tail,
    Dot,
}

/// An item's marker at the start of `value`: a bracketed number, letter or numeral ("(a)", "(12)",
/// "(iv)"), or a letter or numeral before a stop or bracket ("a.", "iv)"). A capital letter alone
/// ("A." of a contract's recitals) is one too: the runs that read markers take only a count of them,
/// so a name's initial ("J. Smith") numbers nothing.
pub(super) fn any_instrument_marker(value: &str) -> Option<(&str, usize, InstrumentMarkerKind)> {
    let capital = |token: &str| token.len() == 1 && token.bytes().all(|byte| byte.is_ascii_uppercase());
    if !value
        .as_bytes()
        .first()
        .is_some_and(|byte| *byte == b'(' || byte.is_ascii_alphabetic())
    {
        return None;
    }
    let valid_letters = |token: &str, upper: bool| {
        let token = inserted_letter_token(token).map_or(token, |(letter, _)| letter);
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
    let delimiter = value.split_whitespace().next()?.rfind([')', '.'])?;
    let token = &value[..delimiter];
    if !valid_letters(token, false) && !capital(token) {
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

pub(super) fn instrument_marker(value: &str, tail: bool, dot: bool) -> Option<(&str, usize)> {
    any_instrument_marker(value).and_then(|(token, at, kind)| match kind {
        InstrumentMarkerKind::Parenthesized => Some((token, at)),
        InstrumentMarkerKind::Tail if tail => Some((token, at)),
        InstrumentMarkerKind::Dot if dot => Some((token, at)),
        _ => None,
    })
}

pub(super) fn instrument_space(character: char) -> bool {
    // Instrument grammar historically admits Rust whitespace, including U+0085;
    // it is therefore not the shared ECMAScript whitespace contract.
    character.is_whitespace() || character == '\u{feff}'
}

pub(super) fn legislation_marker(value: &str, followed_by_newline: bool) -> Option<(&str, usize, usize)> {
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

pub(super) fn compare_child_values(left: &str, right: &str) -> std::cmp::Ordering {
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

pub(super) fn admitted_dialects<'a>(
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
                direct: false,
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
        // Ambiguous insertions continue the innermost open counter, as their base marker does.
        let inserted = inserted_letter_token(token).and_then(|_| {
            readings
                .iter()
                .flatten()
                .filter(|(family, value, at)| {
                    at.is_some_and(|index| enum_increment(*family, &self.stack[index].value, value))
                })
                .max_by_key(|(_, _, at)| *at)
                .map(|(family, value, at)| (0, *family, value.as_str(), *at))
        });
        let selected = inserted.or_else(|| {
            (0..4).find_map(|pass| {
                readings.iter().flatten().find_map(|(family, value, at)| {
                    match (pass, *at) {
                        (0, Some(index)) => {
                            enum_increment(*family, &self.stack[index].value, value)
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
            // A head of several letters that is no roman numeral ("aa") is no marker this ladder reads.
            let Some(roman) = roman else { return };
            (3, roman.to_string())
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

/// What follows a section's number repeated on a subsection: "(2) The ...", "(2) [Repealed]
/// ...", or a run of repealed subsections ("(7) to (10) [Repealed]", "(1.1) and (1.2)
/// [Repealed]"), not a reference wrapped onto the line ("(1)(c) of the Act", "(1) and (2) of").
pub(super) fn repeated_subsection(rest: &str) -> bool {
    fn subsection(value: &str) -> Option<&str> {
        value
            .strip_prefix('(')
            .and_then(|value| value.split_once(')'))
            .filter(|(token, _)| {
                (1..=6).contains(&token.len())
                    && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
            })
            .map(|(_, text)| text)
            .filter(|text| text.starts_with([' ', '\t']))
    }
    subsection(rest).is_some_and(|text| {
        text.trim_start()
            .starts_with(|c: char| c.is_uppercase() || "[\u{201c}\"\u{ab}".contains(c))
            || [" to ", " and "]
                .into_iter()
                .filter_map(|joint| text.strip_prefix(joint))
                .filter_map(subsection)
                .any(|text| text.trim_start().starts_with('['))
    })
}

pub(super) fn enumerated_children(
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
    // A section that opens without a numbered subsection has none of its own: its number
    // repeated further on is quoted, as an amendment quotes the text it substitutes.
    let numbered = markers
        .first()
        .is_some_and(|marker| marker.token.starts_with(|c: char| c.is_ascii_digit()));
    let (mut line_byte, mut line_scalar) = (bytes.start, range.start);
    for raw in value.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let trimmed = line.trim_start_matches(instrument_space);
        // A subsection may repeat its section's number, as Manitoba prints "33(2) The ...".
        let trimmed = root
            .strip_prefix("sec")
            .and_then(|number| trimmed.strip_prefix(number))
            .filter(|rest| numbered && line_scalar != range.start && repeated_subsection(rest))
            .unwrap_or(trimmed);
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

pub(super) fn direct_section(value: &str) -> Option<(String, usize)> {
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

pub(super) fn instrument_top(value: &str, direct: bool) -> Option<(String, usize, bool)> {
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
        // "Article 40.4.2°" at a line's start is a provision cited by a wrapped sentence.
        if value[found.get(token)?.end()..]
            .strip_prefix('.')
            .is_some_and(|after| after.starts_with(|c: char| c.is_ascii_digit()))
        {
            return None;
        }
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

pub(crate) fn detect_instrument_grammar(
    text: &ScalarText<'_>,
    provider_refinements: bool,
) -> Vec<GrammarPoint> {
    let mut lines = lines(text).collect::<Vec<_>>();
    let mut spine = statute_spine_from_lines(text, false, &lines, provider_refinements)
        .into_iter()
        .peekable();
    let direct = spine.peek().is_none();
    let (dialects, dialect_markers) = admitted_dialects(text, &mut lines);
    let mut dialect_markers = dialect_markers.into_iter().peekable();
    let mut state = StructureState::default();
    // Whether the open section opened with a numbered subsection; one that did not has none
    // of its own (see enumerated_children).
    let mut numbered = false;
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
                    direct: direct && !container,
                },
                depth,
            ));
            state.stack.clear();
            numbered = false;
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
                    numbered = token.starts_with(|c: char| c.is_ascii_digit());
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
            // A subsection may repeat its section's number, as Manitoba prints "33(2) The ...".
            // A PDF's own lines also hold a parallel column's numbers; its candidate grammar
            // weighs those itself.
            None if !provider_refinements || !numbered => None,
            None => state.section.as_ref().and_then(|(label, _)| {
                let number = label.strip_prefix("sec")?;
                let rest = value
                    .strip_prefix(number)
                    .filter(|rest| repeated_subsection(rest))?;
                instrument_marker(rest, false, false).map(|(token, at)| (token, at + number.len()))
            }),
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
    detect_instrument_grammar(text, true)
        .into_iter()
        .map(GrammarPoint::into_section)
        .collect()
}
