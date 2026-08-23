//! Coordinates over one unchanged Rust string.
//!
//! Bytes are UTF-8 boundaries, scalars are Rust `char`s, and UTF-16 offsets are
//! JavaScript code units. Exact conversions reject split characters; only the
//! named floor/ceil methods round. CR, LF, and CRLF are never normalized.

use std::{borrow::Cow, ops::Range, sync::OnceLock};

pub(crate) const JS_WHITESPACE_CLASS: &str = r"[\u{0009}-\u{000d}\u{0020}\u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]";

pub struct ScalarText<'a> {
    pub(crate) value: &'a str,
    /// Sparse `[scalar, byte, utf16]` checkpoints; ASCII is identity.
    offsets: Cow<'a, [[usize; 3]]>,
    scalar_len: usize,
    utf16_len: usize,
    lines: OnceLock<Vec<[usize; 3]>>,
}

impl<'a> ScalarText<'a> {
    pub fn new(value: &'a str) -> Self {
        if value.is_ascii() {
            return Self {
                value,
                offsets: Cow::Owned(Vec::new()),
                scalar_len: value.len(),
                utf16_len: value.len(),
                lines: OnceLock::new(),
            };
        }
        const STRIDE: usize = 256;
        let mut offsets = Vec::new();
        let mut scalar_len = 0;
        let mut utf16_len = 0;
        for (scalar, (byte, character)) in value.char_indices().enumerate() {
            if scalar % STRIDE == 0 {
                offsets.push([scalar, byte, utf16_len]);
            }
            scalar_len = scalar + 1;
            utf16_len += character.len_utf16();
        }
        if offsets.last().is_none_or(|offset| offset[0] != scalar_len) {
            offsets.push([scalar_len, value.len(), utf16_len]);
        }
        Self {
            value,
            offsets: Cow::Owned(offsets),
            scalar_len,
            utf16_len,
            lines: OnceLock::new(),
        }
    }

    pub(crate) fn with_same_coordinates<'b>(&'b self, value: &'b str) -> ScalarText<'b> {
        debug_assert!(
            self.value
                .char_indices()
                .map(|(byte, character)| (byte, character.len_utf8()))
                .eq(value
                    .char_indices()
                    .map(|(byte, character)| (byte, character.len_utf8())))
        );
        ScalarText {
            value,
            offsets: Cow::Borrowed(self.offsets.as_ref()),
            scalar_len: self.scalar_len,
            utf16_len: self.utf16_len,
            lines: OnceLock::new(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.scalar_len
    }

    pub(crate) fn utf16_len(&self) -> usize {
        self.utf16_len
    }

    pub(crate) fn lines(&self) -> &[[usize; 3]] {
        self.lines.get_or_init(|| {
            let mut lines = Vec::new();
            let (mut byte_start, mut scalar_start) = (0, 0);
            for raw in self.value.split('\n') {
                let text = raw.strip_suffix('\r').unwrap_or(raw);
                lines.push([byte_start, byte_start + text.len(), scalar_start]);
                let newline = usize::from(byte_start + raw.len() < self.value.len());
                byte_start += raw.len() + newline;
                if self.offsets.is_empty() {
                    scalar_start = byte_start;
                } else {
                    scalar_start += raw.chars().count() + newline;
                }
            }
            lines
        })
    }

    fn checkpoint(&self, value: usize, axis: usize) -> [usize; 3] {
        self.offsets[self.offsets.partition_point(|offset| offset[axis] <= value) - 1]
    }

    pub(crate) fn scalar_at_byte(&self, byte: usize) -> Option<usize> {
        if byte > self.value.len() || !self.value.is_char_boundary(byte) {
            return None;
        }
        if self.offsets.is_empty() {
            return Some(byte);
        }
        let offset = self.checkpoint(byte, 1);
        Some(offset[0] + self.value[offset[1]..byte].chars().count())
    }

    pub(crate) fn scalar(&self, byte: usize) -> usize {
        self.scalar_at_byte(byte)
            .expect("byte offset must be an in-bounds UTF-8 boundary")
    }

    pub(crate) fn byte_at_scalar(&self, scalar: usize) -> Option<usize> {
        if scalar > self.scalar_len {
            return None;
        }
        if self.offsets.is_empty() {
            return Some(scalar);
        }
        let offset = self.checkpoint(scalar, 0);
        if offset[0] == scalar {
            return Some(offset[1]);
        }
        self.value[offset[1]..]
            .char_indices()
            .nth(scalar - offset[0])
            .map(|(byte, _)| offset[1] + byte)
    }

    pub(crate) fn byte(&self, scalar: usize) -> usize {
        self.byte_at_scalar(scalar)
            .expect("scalar offset must be in bounds")
    }

    pub(crate) fn utf16_at_scalar(&self, scalar: usize) -> Option<usize> {
        if scalar > self.scalar_len {
            return None;
        }
        if self.offsets.is_empty() {
            return Some(scalar);
        }
        let offset = self.checkpoint(scalar, 0);
        let utf16 = self.value[offset[1]..]
            .chars()
            .take(scalar - offset[0])
            .fold(offset[2], |sum, c| sum + c.len_utf16());
        Some(utf16)
    }

    pub(crate) fn utf16(&self, scalar: usize) -> usize {
        self.utf16_at_scalar(scalar)
            .expect("scalar offset must be in bounds")
    }

    pub fn scalar_at_utf16(&self, utf16: usize) -> Option<usize> {
        self.byte_bounds_at_utf16(utf16)
            .and_then(|(_, _, scalar)| scalar)
    }

    pub(crate) fn utf16_at_byte(&self, byte: usize) -> Option<usize> {
        if byte > self.value.len() || !self.value.is_char_boundary(byte) {
            return None;
        }
        if self.offsets.is_empty() {
            return Some(byte);
        }
        let offset = self.checkpoint(byte, 1);
        Some(offset[2] + self.value[offset[1]..byte].encode_utf16().count())
    }

    pub(crate) fn byte_at_utf16(&self, utf16: usize) -> Option<usize> {
        self.byte_bounds_at_utf16(utf16)
            .and_then(|(floor, ceil, _)| (floor == ceil).then_some(floor))
    }

    fn byte_bounds_at_utf16(&self, utf16: usize) -> Option<(usize, usize, Option<usize>)> {
        if utf16 > self.utf16_len {
            return None;
        }
        if self.offsets.is_empty() {
            return Some((utf16, utf16, Some(utf16)));
        }
        let offset = self.checkpoint(utf16, 2);
        if offset[2] == utf16 {
            return Some((offset[1], offset[1], Some(offset[0])));
        }
        let mut used = offset[2];
        for (scalar, (relative, character)) in self.value[offset[1]..].char_indices().enumerate() {
            let byte = offset[1] + relative;
            let next = used + character.len_utf16();
            if utf16 < next {
                return Some((byte, byte + character.len_utf8(), None));
            }
            if utf16 == next {
                let byte = byte + character.len_utf8();
                return Some((byte, byte, Some(offset[0] + scalar + 1)));
            }
            used = next;
        }
        None
    }

    pub(crate) fn byte_at_utf16_floor(&self, utf16: usize) -> Option<usize> {
        self.byte_bounds_at_utf16(utf16).map(|(floor, _, _)| floor)
    }

    pub(crate) fn byte_at_utf16_ceil(&self, utf16: usize) -> Option<usize> {
        self.byte_bounds_at_utf16(utf16).map(|(_, ceil, _)| ceil)
    }

    pub(crate) fn slice(&self, range: Range<usize>) -> Option<&'a str> {
        self.value
            .get(self.byte_at_scalar(range.start)?..self.byte_at_scalar(range.end)?)
    }
}

pub fn utf16_len(value: &str) -> usize {
    if value.is_ascii() {
        value.len()
    } else {
        value.encode_utf16().count()
    }
}

/// The code points matched by ECMAScript `\s`: Unicode WhiteSpace plus line
/// terminators and BOM, deliberately excluding U+0085.
pub(crate) fn javascript_whitespace(character: char) -> bool {
    character == '\u{feff}' || (character != '\u{0085}' && character.is_whitespace())
}

pub(crate) fn trim_javascript_whitespace(value: &str) -> &str {
    value.trim_matches(javascript_whitespace)
}

pub(crate) fn trim_javascript_start(value: &str) -> &str {
    value.trim_start_matches(javascript_whitespace)
}

/// Collapse ECMAScript whitespace runs to one ASCII space and trim runs at
/// both ends. Non-whitespace code points, including U+0085, are unchanged.
pub fn normalize_javascript_whitespace(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut separating = false;
    for character in value.chars() {
        if javascript_whitespace(character) {
            separating = !normalized.is_empty();
        } else {
            if separating {
                normalized.push(' ');
            }
            normalized.push(character);
            separating = false;
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_round_trip_empty_combining_and_non_bmp_text() {
        let empty = ScalarText::new("");
        assert_eq!(empty.byte_at_scalar(0), Some(0));
        assert_eq!(empty.scalar_at_byte(0), Some(0));
        assert_eq!(empty.byte_at_utf16(0), Some(0));
        assert_eq!(empty.scalar_at_utf16(0), Some(0));
        assert_eq!(empty.slice(0..0), Some(""));

        let value = "A\u{301}\u{1f9ab}\u{6587}";
        let text = ScalarText::new(value);
        let mut utf16 = 0;
        for (scalar, (byte, character)) in value.char_indices().enumerate() {
            assert_eq!(text.byte_at_scalar(scalar), Some(byte));
            assert_eq!(text.scalar_at_byte(byte), Some(scalar));
            assert_eq!(text.utf16_at_scalar(scalar), Some(utf16));
            assert_eq!(text.utf16_at_byte(byte), Some(utf16));
            assert_eq!(text.scalar_at_utf16(utf16), Some(scalar));
            assert_eq!(text.byte_at_utf16(utf16), Some(byte));
            utf16 += character.len_utf16();
        }
        assert_eq!(text.byte_at_scalar(text.len()), Some(value.len()));
        assert_eq!(text.scalar_at_byte(value.len()), Some(text.len()));
        assert_eq!(text.utf16_at_scalar(text.len()), Some(utf16));
        assert_eq!(text.byte_at_utf16(utf16), Some(value.len()));
        assert_eq!(text.scalar_at_utf16(utf16), Some(text.len()));
    }

    #[test]
    fn non_ascii_coordinate_index_stays_sparse() {
        let value = format!("\u{201c}{}", "a".repeat(1024));
        assert!(ScalarText::new(&value).offsets.len() < 10);
    }

    #[test]
    fn same_length_view_borrows_coordinates_but_indexes_its_own_lines() {
        let original = ScalarText::new("\u{1f9ab} a");
        let recovered = original.with_same_coordinates("\u{1f98a}\na");

        assert!(matches!(&recovered.offsets, Cow::Borrowed(_)));
        assert_eq!(recovered.utf16_at_byte(5), original.utf16_at_byte(5));
        assert_eq!(recovered.lines(), &[[0, 4, 0], [5, 6, 2]]);
        assert_eq!(original.lines(), &[[0, 6, 0]]);
    }

    #[test]
    fn exact_coordinates_and_slices_reject_non_boundaries() {
        let text = ScalarText::new("a\u{1f9ab}b");
        assert_eq!(text.scalar_at_byte(2), None);
        assert_eq!(text.utf16_at_byte(2), None);
        assert_eq!(text.byte_at_scalar(4), None);
        assert_eq!(text.byte_at_utf16(2), None);
        assert_eq!(text.scalar_at_utf16(2), None);
        assert_eq!(text.byte_at_utf16_floor(2), Some(1));
        assert_eq!(text.byte_at_utf16_ceil(2), Some(5));
        assert_eq!(text.byte_at_utf16(1), Some(1));
        assert_eq!(text.byte_at_utf16(3), Some(5));
        assert_eq!(text.byte_at_utf16(4), Some(6));
        assert_eq!(text.slice(1..2), Some("\u{1f9ab}"));
        assert_eq!(text.slice(2..1), None);
        assert_eq!(text.slice(0..4), None);
        assert_eq!(text.byte_at_utf16_floor(5), None);
        assert_eq!(text.byte_at_utf16_ceil(5), None);
    }

    #[test]
    fn cr_lf_and_crlf_keep_original_coordinates() {
        let value = "a\rb\r\nc\nd";
        let text = ScalarText::new(value);
        assert_eq!(text.len(), 8);
        assert_eq!(text.utf16_len(), 8);
        assert_eq!(text.utf16_at_byte(value.find('\n').unwrap()), Some(4));
        assert_eq!(text.slice(1..5), Some("\rb\r\n"));
    }

    #[test]
    fn javascript_whitespace_matches_ecmascript_exactly() {
        let whitespace = [
            '\u{0009}', '\u{000a}', '\u{000b}', '\u{000c}', '\u{000d}', '\u{0020}', '\u{00a0}',
            '\u{1680}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}',
            '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}',
            '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
        ];
        assert!(whitespace.into_iter().all(javascript_whitespace));
        assert!(!javascript_whitespace('\u{0085}'));
        assert!(!javascript_whitespace('\u{180e}'));
    }

    #[test]
    fn javascript_whitespace_normalization_preserves_non_whitespace() {
        assert_eq!(
            normalize_javascript_whitespace("\r\n A\u{feff}\tB\u{0085}\n"),
            "A B\u{0085}"
        );
        assert_eq!(normalize_javascript_whitespace("\r"), "");
        assert_eq!(normalize_javascript_whitespace("\n"), "");
        assert_eq!(normalize_javascript_whitespace("\r\n"), "");
    }
}
