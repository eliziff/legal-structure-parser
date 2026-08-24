use crate::{text::ScalarText, utf16_len, ScalarRange, JS_WHITESPACE_CLASS};
use aho_corasick::AhoCorasick;
use regex::Regex as R;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DefinitionOccurrence {
    pub range: ScalarRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub source_paragraph_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_artifact_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DefinedTerm {
    pub term: String,
    pub definitions: Vec<DefinitionOccurrence>,
    pub uses: Vec<DefinitionOccurrence>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DefinitionsResult {
    pub terms: Vec<DefinedTerm>,
}

static PAREN: LazyLock<R> = LazyLock::new(|| R::new(r"\(([^()]*)\)").unwrap());
static QUOTED: LazyLock<R> = LazyLock::new(|| R::new(r#""([A-Z][A-Za-z0-9&' -]{0,79})""#).unwrap());
static LIST: LazyLock<R> = LazyLock::new(|| {
    R::new(
        &[
            r#"^"([A-Z][A-Za-z0-9&'\- ]{0,79})""#,
            JS_WHITESPACE_CLASS,
            r"+(?:means|shall mean|has the meaning|shall have the meaning)",
        ]
        .concat(),
    )
    .unwrap()
});

impl DefinitionOccurrence {
    fn at(&self, document: &ScalarText<'_>, start: usize, end: usize) -> Self {
        let mut hit = self.clone();
        (hit.range.start, hit.range.end) = (
            document.utf16_at_byte(start).unwrap(),
            document.utf16_at_byte(end).unwrap(),
        );
        hit
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DefinitionHit {
    pub paragraph: usize,
    pub start: usize,
    pub end: usize,
}

pub(crate) struct ByteDefinedTerm {
    pub term: String,
    pub definitions: Vec<DefinitionHit>,
    pub uses: Vec<DefinitionHit>,
}

fn bounded(text: &str, start: usize, end: usize) -> bool {
    let bytes = text.as_bytes();
    let left = bytes.get(start.wrapping_sub(1)).copied().unwrap_or(b' ');
    let right = bytes.get(end).copied().unwrap_or(b' ');
    !left.is_ascii_alphanumeric() && !right.is_ascii_lowercase() && !right.is_ascii_digit()
}

pub fn derive_definitions(text: &str, paragraphs: &[DefinitionOccurrence]) -> DefinitionsResult {
    let document = ScalarText::new(text);
    let spans = paragraphs
        .iter()
        .map(|paragraph| {
            (
                document.byte_at_utf16(paragraph.range.start).unwrap(),
                document.byte_at_utf16(paragraph.range.end).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let terms = derive_definitions_bytes(&document, paragraphs.len(), |index| spans[index]);
    DefinitionsResult {
        terms: terms
            .into_iter()
            .map(|term| DefinedTerm {
                term: term.term,
                definitions: term
                    .definitions
                    .into_iter()
                    .map(|hit| paragraphs[hit.paragraph].at(&document, hit.start, hit.end))
                    .collect(),
                uses: term
                    .uses
                    .into_iter()
                    .map(|hit| paragraphs[hit.paragraph].at(&document, hit.start, hit.end))
                    .collect(),
            })
            .collect(),
    }
}

pub(crate) fn derive_definitions_bytes(
    document: &ScalarText<'_>,
    paragraph_count: usize,
    span: impl Fn(usize) -> (usize, usize),
) -> Vec<ByteDefinedTerm> {
    let text = document.value;
    let mut terms = Vec::<(String, Vec<DefinitionHit>)>::new();
    let mut term_indices = HashMap::<&str, usize>::new();
    for paragraph in 0..paragraph_count {
        let (base, end) = span(paragraph);
        let text = &text[base..end];
        if !text.contains('"') {
            continue;
        }
        let mut found = Vec::new();
        for content in PAREN.captures_iter(text).filter_map(|c| c.get(1)) {
            if !(1..=200).contains(&utf16_len(content.as_str())) {
                continue;
            }
            found.extend(QUOTED.captures_iter(content.as_str()).map(|c| {
                let term = c.get(1).unwrap();
                (
                    term.as_str(),
                    content.start() + term.start(),
                    content.start() + term.end(),
                )
            }));
        }
        if let Some(c) = LIST.captures(text).filter(|c| bounded(text, 0, c[0].len())) {
            let term = c.get(1).unwrap();
            found.push((term.as_str(), term.start(), term.end()));
        }
        let mut seen = HashSet::new();
        for (term, start, end) in found.into_iter().filter(|(term, _, _)| seen.insert(*term)) {
            let hit = DefinitionHit {
                paragraph,
                start: base + start,
                end: base + end,
            };
            if let Some(&term_index) = term_indices.get(term) {
                terms[term_index].1.push(hit);
            } else {
                term_indices.insert(term, terms.len());
                terms.push((term.to_owned(), vec![hit]));
            }
        }
    }

    let mut uses = vec![Vec::<DefinitionHit>::new(); terms.len()];
    if !terms.is_empty() {
        let matcher = AhoCorasick::new(terms.iter().flat_map(|(term, _)| {
            [
                Cow::Borrowed(term.as_bytes()),
                Cow::Owned(
                    term.strip_suffix('s')
                        .map_or_else(|| format!("{term}s"), str::to_owned)
                        .into_bytes(),
                ),
            ]
        }))
        .unwrap();
        let mut pattern_ends = vec![0; terms.len() * 2];
        for paragraph in 0..paragraph_count {
            let (base, end) = span(paragraph);
            let text = &text[base..end];
            pattern_ends.fill(0);
            for hit in matcher.find_overlapping_iter(text) {
                let pattern_index = hit.pattern().as_usize();
                if hit.start() < pattern_ends[pattern_index] {
                    continue;
                }
                pattern_ends[pattern_index] = hit.end();
                if !bounded(text, hit.start(), hit.end()) {
                    continue;
                }
                let term_index = pattern_index / 2;
                if terms[term_index]
                    .1
                    .binary_search_by_key(&paragraph, |defined| defined.paragraph)
                    .is_err()
                {
                    uses[term_index].push(DefinitionHit {
                        paragraph,
                        start: base + hit.start(),
                        end: base + hit.end(),
                    });
                }
            }
        }
    }

    terms
        .into_iter()
        .zip(uses)
        .map(|((term, definitions), mut uses)| {
            uses.sort_unstable_by_key(|hit| (hit.paragraph, hit.start, hit.end));
            ByteDefinedTerm {
                term,
                definitions,
                uses,
            }
        })
        .collect()
}
