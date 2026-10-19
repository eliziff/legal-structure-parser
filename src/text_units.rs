//! A text's citable units, the numbered lines a reader shows and a citation names: every line, and with
//! sentences every sentence of a line. A unit never crosses a line break, and a sentence never ends inside a
//! citation ("(Ont. C.A.)", "33 O.R. (3d) 1").

use crate::{citation_occurrences_in_text, sentence_end};

/// The units of `text` in order, as UTF-16 `[start, end)` spans without the whitespace around them. A line holding
/// text is one unit, or with `sentences` one unit per sentence; a line's opening label ("12.", "iv.") opens its
/// first sentence rather than standing as one.
pub fn text_units(text: &str, sentences: bool) -> Vec<[usize; 2]> {
    let citations: Vec<(usize, usize)> = if sentences {
        citation_occurrences_in_text(text).into_iter().map(|cite| (cite.start, cite.end)).collect()
    } else {
        Vec::new()
    };
    let mut units = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        // The line's words with their UTF-16 spans.
        let mut words: Vec<(&str, usize, usize)> = Vec::new();
        let (mut position, mut open) = (offset, None::<(usize, usize)>);
        for (byte, character) in line.char_indices() {
            if character.is_whitespace() {
                if let Some((start_byte, start)) = open.take() {
                    words.push((&line[start_byte..byte], start, position));
                }
            } else if open.is_none() {
                open = Some((byte, position));
            }
            position += character.len_utf16();
        }
        if let Some((start_byte, start)) = open {
            words.push((&line[start_byte..], start, position));
        }
        let mut first = 0;
        for index in 0..words.len() {
            let ends = index + 1 == words.len()
                || sentences
                    && !(index == 0 && line_label(words[index].0))
                    && sentence_end(words[index].0, words[index + 1].0)
                    && !citations.iter().any(|&(start, end)| start < words[index].2 && words[index + 1].1 < end);
            if ends {
                units.push([words[first].1, words[index].2]);
                first = index + 1;
            }
        }
        offset = position + 1;
    }
    units
}

/// Whether a line's first word is its number or letter ("12.", "(iv)."), not a sentence of its own.
fn line_label(word: &str) -> bool {
    let stem = word.trim_start_matches('(').trim_end_matches(['.', ')']);
    !stem.is_empty()
        && stem.len() <= 6
        && (stem.bytes().all(|byte| byte.is_ascii_digit())
            || stem.bytes().all(|byte| b"ivxlcIVXLC".contains(&byte)))
}
