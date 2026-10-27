//! Text as JavaScript reads it ([`legal_text`]), and the readings of a document's text built on it.

pub use legal_text::*;

/// Read an exact UTF-16 membership union in the text's order. Touching and overlapping members are read once; real
/// whitespace between members is retained. Substantive omitted text is replaced by a newline, keeping the remaining
/// components separate. Component edges are trimmed using ECMAScript whitespace, as with ordinary readable document
/// blocks.
pub fn membership_text_utf16(text: &ScalarText, ranges: &[crate::ScalarRange]) -> String {
    let mut ranges = ranges.to_vec();
    crate::ScalarRange::normalize_union(&mut ranges);
    let mut parts = Vec::<crate::ScalarRange>::new();
    for range in ranges {
        if range.start == range.end { continue; }
        if let Some(previous) = parts.last_mut().filter(|previous|
            text.slice_utf16(previous.end..range.start)
                .is_some_and(|gap| trim_javascript_whitespace(gap).is_empty())) {
            previous.end = range.end;
        } else {
            parts.push(range);
        }
    }
    parts.iter().filter_map(|range| text.slice_utf16(range.start..range.end))
        .map(trim_javascript_whitespace).filter(|part| !part.is_empty())
        .collect::<Vec<_>>().join("\n")
}

#[inline]
pub fn normalize_decimal_digit(character: char) -> Option<char> {
    Some(match character {
        '\u{2070}' => '0',
        '\u{00b9}' => '1',
        '\u{00b2}' => '2',
        '\u{00b3}' => '3',
        '\u{2074}' => '4',
        '\u{2075}' => '5',
        '\u{2076}' => '6',
        '\u{2077}' => '7',
        '\u{2078}' => '8',
        '\u{2079}' => '9',
        value if value.is_ascii_digit() => value,
        _ => return None,
    })
}

#[inline]
pub fn normalize_note_symbol(character: char) -> char {
    match character {
        '\u{2217}' | '\u{f02a}' => '*',
        other => other,
    }
}

/// The longest run of note symbols a label prints ("***").
pub const MAX_SYMBOL_LABEL_LEN: usize = 8;

/// A note label opening a line: its normalized label ("12", "*") and where it sits in the line's
/// characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelPrefix {
    pub label: String,
    pub start: usize,
    pub end: usize,
}

/// A note label's text normalized: superscript digits as digits, a number without leading zeros,
/// symbol look-alikes as their symbol.
pub fn normalize_label(value: &str) -> String {
    let translated: String = value
        .trim()
        .chars()
        .map(|character| normalize_decimal_digit(character).unwrap_or_else(|| normalize_note_symbol(character)))
        .collect();
    translated.parse::<u64>().map_or(translated, |number| number.to_string())
}

pub fn is_note_symbol(character: char) -> bool {
    matches!(character, '*' | '∗' | '\u{f02a}' | '†' | '‡' | '§' | '¶' | '#')
}

fn scalar_suffix(value: &str, start: usize) -> &str {
    let byte = value.char_indices().nth(start).map_or(value.len(), |at| at.0);
    &value[byte..]
}

/// A note label opening a line before its text: up to four digits or a run of note symbols,
/// followed by a space, or a stop or bracket and a space; "1endnote 1" (an endnote's number glued to
/// its call) reads as one label.
pub fn line_start_label_prefix(text: &str) -> Option<LabelPrefix> {
    let leading = text.chars().take_while(|character| character.is_whitespace()).count();
    let trimmed = text.trim_start();
    let symbols: String = trimmed.chars().take(MAX_SYMBOL_LABEL_LEN).take_while(|character| is_note_symbol(*character)).collect();
    let token = if symbols.is_empty() { trimmed.chars().take(4).take_while(char::is_ascii_digit).collect() } else { symbols };
    if token.is_empty() {
        return None;
    }
    let token_chars = token.chars().count();
    let remainder = scalar_suffix(trimmed, token_chars);
    let embedded_endnote = format!("endnote {token}");
    if remainder.get(..embedded_endnote.len()).is_some_and(|value| value.eq_ignore_ascii_case(&embedded_endnote)) {
        return Some(LabelPrefix { label: normalize_label(&token), start: leading, end: leading + token_chars + embedded_endnote.chars().count() });
    }
    let mut remainder_chars = remainder.chars();
    let valid_boundary = match remainder_chars.next() {
        Some(character) if character.is_whitespace() => true,
        Some(character) if ".)],:;-".contains(character) => remainder_chars.next().is_none_or(|next| next.is_whitespace()),
        _ => false,
    };
    valid_boundary.then(|| LabelPrefix { label: normalize_label(&token), start: leading, end: leading + token_chars })
}

/// A note label opening a line, or standing alone on it ("12", "*").
pub fn label_prefix(text: &str) -> Option<LabelPrefix> {
    if let Some(prefix) = line_start_label_prefix(text) {
        return Some(prefix);
    }
    let leading = text.chars().take_while(|character| character.is_whitespace()).count();
    let stripped = text.trim();
    let count = stripped.chars().count();
    let pure = ((1..=MAX_SYMBOL_LABEL_LEN).contains(&count) && stripped.chars().all(is_note_symbol))
        || ((1..=4).contains(&count) && stripped.chars().all(|character| character.is_ascii_digit()));
    pure.then(|| LabelPrefix { label: normalize_label(stripped), start: leading, end: leading + count })
}
