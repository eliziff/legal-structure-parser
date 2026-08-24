use crate::{javascript_whitespace, text::trim_javascript_start};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxNumberAnchor {
    pub number: String,
    pub paragraph_index: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxNumberingDuplicate {
    pub number: String,
    pub previous_paragraph_index: usize,
    pub paragraph_index: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxNumberingGap {
    pub previous_number: String,
    pub number: String,
    pub paragraph_index: usize,
    pub missing: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocxNumberingResult {
    pub number_anchors: Vec<DocxNumberAnchor>,
    pub roman_article_anchors: Vec<DocxNumberAnchor>,
    pub duplicates: Vec<DocxNumberingDuplicate>,
    pub gaps: Vec<DocxNumberingGap>,
}

fn after_prefix<'a>(value: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    prefixes.iter().find_map(|prefix| {
        let rest = value.strip_prefix(prefix)?;
        let trimmed = trim_javascript_start(rest);
        (trimmed.len() < rest.len()).then_some(trimmed)
    })
}

fn number_head(value: &str) -> Option<(&str, bool, &str)> {
    let value = after_prefix(
        value,
        &[
            "Section", "Clause", "Article", "SECTION", "CLAUSE", "ARTICLE",
        ],
    )
    .unwrap_or(value);
    let bytes = value.as_bytes();
    let mut at = 0;
    loop {
        let start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() && at - start < 3 {
            at += 1;
        }
        if at == start || (at < bytes.len() && bytes[at].is_ascii_digit()) {
            return None;
        }
        if bytes.get(at) != Some(&b'.') || !bytes.get(at + 1).is_some_and(u8::is_ascii_digit) {
            break;
        }
        at += 1;
    }
    let number_end = at;
    let terminated = matches!(bytes.get(at), Some(b'.' | b')'));
    at += usize::from(terminated);
    if let Some(character) = value[at..].chars().next() {
        if !javascript_whitespace(character) {
            return None;
        }
        at += character.len_utf8();
    }
    Some((&value[..number_end], terminated, &value[at..]))
}

fn collect_number_anchors(paragraphs: &[String]) -> Vec<DocxNumberAnchor> {
    paragraphs
        .iter()
        .enumerate()
        .filter_map(|(paragraph_index, text)| {
            let (number, terminated, rest) = number_head(text)?;
            if !number.contains('.') && number.parse::<u32>().ok()? > 500 {
                return None;
            }
            let rest = trim_javascript_start(rest);
            let heading_like = rest.is_empty()
                || rest
                    .chars()
                    .next()
                    .is_some_and(|value| matches!(value, '"' | '\'' | '(' | 'A'..='Z'));
            (terminated || heading_like).then(|| DocxNumberAnchor {
                number: number.to_owned(),
                paragraph_index,
            })
        })
        .collect()
}

fn collect_roman_article_anchors(paragraphs: &[String]) -> Vec<DocxNumberAnchor> {
    paragraphs
        .iter()
        .enumerate()
        .filter_map(|(paragraph_index, text)| {
            let value = after_prefix(text, &["Article", "ARTICLE"])?;
            let end = value
                .char_indices()
                .take_while(|(_, character)| "IVXLCDM".contains(*character))
                .map(|(at, character)| at + character.len_utf8())
                .last()?;
            let number = &value[..end];
            let boundary = value[end..].chars().next();
            (boundary
                .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_'))
            .then(|| DocxNumberAnchor {
                number: number.to_owned(),
                paragraph_index,
            })
        })
        .collect()
}

fn sequence_findings(
    anchors: &[DocxNumberAnchor],
) -> (Vec<DocxNumberingDuplicate>, Vec<DocxNumberingGap>) {
    let mut groups = Vec::<(&str, Vec<usize>)>::new();
    let mut group_indices = HashMap::<&str, usize>::new();
    for (index, anchor) in anchors.iter().enumerate() {
        let parent = anchor.number.rsplit_once('.').map_or("", |value| value.0);
        if let Some(&group_index) = group_indices.get(parent) {
            groups[group_index].1.push(index);
        } else {
            group_indices.insert(parent, groups.len());
            groups.push((parent, vec![index]));
        }
    }
    let mut duplicates = Vec::new();
    let mut gaps = Vec::new();
    for (parent, members) in groups {
        if members.len() < 2 {
            continue;
        }
        let mut previous = None::<(&DocxNumberAnchor, u32)>;
        for index in members {
            let anchor = &anchors[index];
            let value = anchor
                .number
                .rsplit_once('.')
                .map_or(anchor.number.as_str(), |value| value.1)
                .parse::<u32>()
                .unwrap();
            if let Some((prior, prior_value)) = previous {
                if value == prior_value && !parent.is_empty() {
                    duplicates.push(DocxNumberingDuplicate {
                        number: anchor.number.clone(),
                        previous_paragraph_index: prior.paragraph_index,
                        paragraph_index: anchor.paragraph_index,
                    });
                } else if (2..=4).contains(&value.saturating_sub(prior_value))
                    && value > prior_value
                {
                    gaps.push(DocxNumberingGap {
                        previous_number: prior.number.clone(),
                        number: anchor.number.clone(),
                        paragraph_index: anchor.paragraph_index,
                        missing: ((prior_value + 1)..value)
                            .map(|missing| {
                                if parent.is_empty() {
                                    missing.to_string()
                                } else {
                                    format!("{parent}.{missing}")
                                }
                            })
                            .collect(),
                    });
                }
            }
            previous = Some((anchor, value));
        }
    }
    (duplicates, gaps)
}

/// Port of Beaver's literal DOCX numbering detector over its normalized
/// paragraph-text plane. It does not resolve Word field numbering.
pub(crate) fn derive_docx_numbering(paragraphs: &[String]) -> DocxNumberingResult {
    let number_anchors = collect_number_anchors(paragraphs);
    let roman_article_anchors = collect_roman_article_anchors(paragraphs);
    let (duplicates, gaps) = sequence_findings(&number_anchors);
    DocxNumberingResult {
        number_anchors,
        roman_article_anchors,
        duplicates,
        gaps,
    }
}
