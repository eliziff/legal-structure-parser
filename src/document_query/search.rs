use super::{js_trim, regex, DocumentQuery};
use crate::{DocumentStructure, ScalarText};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{atomic::Ordering, OnceLock};

pub(super) fn word_regex() -> &'static Regex {
    static WORD: OnceLock<Regex> = OnceLock::new();
    regex(r"[\p{L}\p{N}]+(?:['\u{2019}][\p{L}\p{N}]+)*", &WORD)
}

pub(super) fn lowercase_words(value: &str) -> Vec<String> {
    word_regex()
        .find_iter(value)
        .map(|word| word.as_str().to_lowercase())
        .collect()
}

#[derive(Clone, Serialize)]
pub(crate) struct DocumentWordSpan {
    pub word: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy)]
pub(super) struct WordOffset {
    pub(super) start: usize,
    pub(super) end: usize,
}

pub(super) struct SearchIndex {
    pub(super) tokens: Vec<WordOffset>,
    postings: HashMap<u64, Vec<u32>>,
}

impl SearchIndex {
    pub(super) fn with_scalar(text: &str, scalar: &ScalarText<'_>) -> Self {
        let mut tokens = Vec::new();
        let mut postings = HashMap::<u64, Vec<u32>>::new();
        for found in word_regex().find_iter(text) {
            tokens.push(WordOffset {
                start: scalar.utf16_at_byte(found.start()).expect("token boundary"),
                end: scalar.utf16_at_byte(found.end()).expect("token boundary"),
            });
            postings
                .entry(normalized_word_hash(found.as_str()))
                .or_default()
                .push((tokens.len() - 1) as u32);
        }
        Self { tokens, postings }
    }
}

pub(super) fn word_hash(word: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    word.chars().for_each(|character| character.hash(&mut hash));
    hash.finish()
}

fn normalized_word_hash(word: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    word.chars()
        .flat_map(char::to_lowercase)
        .for_each(|character| character.hash(&mut hash));
    hash.finish()
}

fn normalized_word_matches(word: &str, normalized: &str) -> bool {
    if word.is_ascii() {
        word.bytes()
            .map(|byte| byte.to_ascii_lowercase())
            .eq(normalized.bytes())
    } else {
        word.to_lowercase() == normalized
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PhraseOptions {
    pub start: Option<usize>,
    pub end: Option<usize>,
    #[serde(default)]
    pub same_line: bool,
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PhraseSpan {
    pub start: usize,
    pub end: usize,
    pub first_word: usize,
    pub last_word: usize,
}

impl DocumentQuery {
    pub(super) fn index_with_text(
        &self,
        document: &DocumentStructure,
        text: &ScalarText<'_>,
    ) -> &SearchIndex {
        self.search
            .get_or_init(|| SearchIndex::with_scalar(document.query_text(), text))
    }

    pub(super) fn phrase_spans_with_text(
        &self,
        document: &DocumentStructure,
        words: &[String],
        options: PhraseOptions,
        text: &ScalarText<'_>,
    ) -> Vec<PhraseSpan> {
        if words.is_empty() {
            return Vec::new();
        }
        let searched = self.searched.swap(true, Ordering::Relaxed);
        if !searched
            && self.search.get().is_none()
            && options.start.is_none()
            && options.end.is_none()
        {
            return scan_phrase_spans_with_text(document.query_text(), text, words, options);
        }
        indexed_phrase_spans(self.index_with_text(document, text), text, words, options)
    }
}

pub(super) fn tokenize_with_scalar(text: &str, scalar: &ScalarText<'_>) -> Vec<DocumentWordSpan> {
    word_regex()
        .find_iter(text)
        .map(|found| DocumentWordSpan {
            word: found.as_str().to_lowercase(),
            start: scalar.utf16_at_byte(found.start()).expect("token boundary"),
            end: scalar.utf16_at_byte(found.end()).expect("token boundary"),
        })
        .collect()
}

pub(crate) fn tokenize_source_text(text: &str) -> Vec<DocumentWordSpan> {
    tokenize_with_scalar(text, &ScalarText::new(text))
}

pub(super) fn quote_text(value: &str) -> String {
    static BRACKET_LETTER: OnceLock<Regex> = OnceLock::new();
    static BRACKETS: OnceLock<Regex> = OnceLock::new();
    static ELISION: OnceLock<Regex> = OnceLock::new();
    let value =
        js_trim(value).trim_matches(|character| matches!(character, '"' | '\'' | '“' | '”'));
    let value = regex(r"\[([A-Za-z])\]([A-Za-z])", &BRACKET_LETTER).replace_all(value, "$1$2");
    let value = regex(r"\[([^\]]+)\]", &BRACKETS).replace_all(&value, "$1");
    let value = regex(r"\.{3}|…", &ELISION).replace_all(&value, " ");
    let mut normalized = String::with_capacity(value.len());
    let mut separating = false;
    for character in value.chars() {
        if matches!(
            character,
            ' ' | '\t' | '\r' | '\n' | '\u{000c}' | '\u{000b}'
        ) {
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

pub(super) fn quote_words(quote: &str) -> Vec<String> {
    lowercase_words(&quote_text(quote))
}

pub(super) fn indexed_phrase_spans(
    index: &SearchIndex,
    text: &ScalarText<'_>,
    words: &[String],
    options: PhraseOptions,
) -> Vec<PhraseSpan> {
    let token_at = |offset| index.tokens.partition_point(|token| token.start < offset);
    let from = options.start.map_or(0, token_at);
    let until = options.end.map_or(index.tokens.len(), token_at);
    let limit = options.limit.unwrap_or(usize::MAX);
    let mut anchor: Option<(usize, &[u32])> = None;
    for (offset, word) in words.iter().enumerate() {
        let Some(positions) = index.postings.get(&word_hash(word)).map(Vec::as_slice) else {
            return Vec::new();
        };
        if anchor.is_none_or(|(_, rarest)| positions.len() < rarest.len()) {
            anchor = Some((offset, positions));
        }
    }
    let Some((anchor, positions)) = anchor else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    for &position in positions {
        let position = position as usize;
        let Some(start) = position.checked_sub(anchor) else {
            continue;
        };
        if start < from {
            continue;
        }
        if start + words.len() > until {
            break;
        }
        if index.tokens[start..start + words.len()]
            .iter()
            .zip(words)
            .any(|(token, word)| {
                !normalized_word_matches(
                    text.slice_utf16(token.start..token.end).unwrap_or_default(),
                    word,
                )
            })
        {
            continue;
        }
        let first = &index.tokens[start];
        let last = &index.tokens[start + words.len() - 1];
        if options.same_line
            && text
                .slice_utf16(first.start..last.end)
                .is_some_and(|value| value.contains('\n'))
        {
            continue;
        }
        spans.push(PhraseSpan {
            start: first.start,
            end: last.end,
            first_word: start,
            last_word: start + words.len() - 1,
        });
        if spans.len() >= limit {
            break;
        }
    }
    spans
}

fn scan_phrase_spans_with_text(
    text: &str,
    scalar: &ScalarText<'_>,
    words: &[String],
    options: PhraseOptions,
) -> Vec<PhraseSpan> {
    let size = words.len();
    let limit = options.limit.unwrap_or(usize::MAX);
    let mut ring = vec![("", WordOffset { start: 0, end: 0 }); size];
    let mut spans = Vec::new();
    let mut seen = 0;
    for found in word_regex().find_iter(text) {
        ring[seen % size] = (
            found.as_str(),
            WordOffset {
                start: scalar.utf16_at_byte(found.start()).expect("token boundary"),
                end: scalar.utf16_at_byte(found.end()).expect("token boundary"),
            },
        );
        seen += 1;
        if seen < size
            || (0..size).any(|offset| {
                !normalized_word_matches(ring[(seen - size + offset) % size].0, &words[offset])
            })
        {
            continue;
        }
        let first = ring[(seen - size) % size].1;
        let last = ring[(seen - 1) % size].1;
        if options.start.is_some_and(|start| first.start < start)
            || options.end.is_some_and(|end| last.start >= end)
        {
            continue;
        }
        if options.same_line {
            let start_byte = scalar.byte_at_utf16(first.start).expect("token boundary");
            let end_byte = scalar.byte_at_utf16(last.end).expect("token boundary");
            if text[start_byte..end_byte].contains('\n') {
                continue;
            }
        }
        spans.push(PhraseSpan {
            start: first.start,
            end: last.end,
            first_word: seen - size,
            last_word: seen - 1,
        });
        if spans.len() >= limit {
            break;
        }
    }
    spans
}
