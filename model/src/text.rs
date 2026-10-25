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
