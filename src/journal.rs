use crate::locator::literal_page_marker;
use crate::{
    Derivation, DetectionProfile, Evidence, DocumentStructure, EngineError, NodeKind,
    ScalarRange, ScalarText, StructureNode, utf16_len,
};
use serde::Deserialize;
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::BufRead;

const ORIGIN: &str = "provider-adapter";

#[derive(Clone, Deserialize)]
pub struct JournalPageLabel {
    #[serde(rename = "page_label")]
    pub label: String,
    pub pdf_page: usize,
}

#[derive(Deserialize)]
struct Page {
    article_id: Option<Value>,
    text: String,
    pdf_page: Option<usize>,
    #[serde(default)]
    regions: Vec<PageRegion>,
    #[serde(default)]
    annotations: Vec<Annotation>,
}

#[derive(Deserialize)]
struct PageRegion {
    order: Option<f64>,
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    lines: Vec<PageLine>,
}

#[derive(Deserialize)]
struct PageLine {
    codex_text_order: Option<usize>,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct Annotation {
    pair_id: Option<String>,
    pair_status: Option<String>,
    taxonomy_name: Option<String>,
    note_id: Option<Value>,
    selected_text: Option<Value>,
    start_line_order: Option<usize>,
}

#[derive(Clone, Copy)]
struct Region {
    start: usize,
    end: usize,
    pdf_page: Option<usize>,
}

struct Title {
    start: usize,
    label: Option<String>,
    aliases: Vec<String>,
}

fn public_label(value: &str) -> String {
    value.parse::<u64>().map_or_else(|_| value.to_owned(), |value| value.to_string())
}

fn title(value: &str) -> (Option<String>, Vec<String>) {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let numbered = compact.split_once('.').and_then(|(label, rest)| {
        (!rest.is_empty()
            && rest.starts_with(char::is_whitespace)
            && (label.len() == 1 && label.bytes().all(|byte| byte.is_ascii_uppercase())
                || !label.is_empty()
                    && label.bytes().all(|byte| {
                        matches!(byte, b'I' | b'V' | b'X' | b'L' | b'C' | b'D' | b'M')
                    })))
        .then_some((label, rest.trim()))
    });
    let name = numbered.map_or(compact.as_str(), |(_, rest)| rest);
    let mut normalized = String::new();
    let mut separated = true;
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            normalized.push(character);
            separated = false;
        } else if !separated {
            normalized.push(' ');
            separated = true;
        }
    }
    let label = numbered.map(|(label, _)| label.to_owned());
    let normalized = normalized.trim();
    let aliases = (!normalized.is_empty()).then(|| format!("sectitle:{normalized}")).into_iter().collect();
    (label, aliases)
}

fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn page_marker(line: &str) -> Option<&str> {
    let label = literal_page_marker(line, false)?.trim();
    (!label.is_empty() && label.len() <= 40 && !label.contains(']')).then_some(label)
}

fn positive_usize(value: Option<&Value>) -> Option<usize> {
    let value = match value? {
        Value::Number(value) => value.as_u64()?,
        Value::String(value) => value.trim().parse().ok()?,
        _ => return None,
    };
    (value > 0 && value <= 9_007_199_254_740_991)
        .then_some(value)
        .and_then(|value| value.try_into().ok())
}

/// A node the export gives the text, under its `aliases`; `structure` numbers it.
fn claim(kind: NodeKind, label: String, range: ScalarRange, aliases: Vec<String>) -> StructureNode {
    let mut node = StructureNode::new(String::new(), kind, range, ORIGIN, Derivation::Native, None);
    node.label = Some(label);
    node.aliases = (!aliases.is_empty()).then_some(aliases);
    node
}

fn structure(
    article_id: usize,
    url: Option<String>,
    text: String,
    mut claims: Vec<StructureNode>,
) -> Result<DocumentStructure, EngineError> {
    for (index, claim) in claims.iter_mut().enumerate() {
        claim.id = format!("native-{:06}", index + 1);
    }
    let mut structure = DocumentStructure::new(article_id.to_string(), "journal", text, ORIGIN);
    structure.url = url;
    structure.nodes = claims;
    let mut structure = crate::derive::derive(Evidence::new(structure, DetectionProfile::Journal), Some(Vec::new()));
    structure.refresh_revision().expect("Typed document structure is serializable");
    Ok(structure)
}

pub fn journal_text_document_structure(
    article_id: usize,
    url: Option<String>,
    text: String,
    page_labels: &[JournalPageLabel],
) -> Result<DocumentStructure, EngineError> {
    if !text
        .split_inclusive('\n')
        .any(|line| page_marker(line).is_some())
    {
        return structure(article_id, url, text, Vec::new());
    }
    let mut starts = Vec::new();
    let mut clean = String::with_capacity(text.len());
    let (mut clean_offset, mut page_cursor) = (0, 0);
    for line in text.split_inclusive('\n') {
        if let Some(label) = page_marker(line) {
            let row = page_labels[page_cursor..]
                .iter()
                .position(|row| row.label.trim() == label)
                .map(|index| page_cursor + index);
            let pdf_page = row.map(|index| {
                page_cursor = index + 1;
                page_labels[index].pdf_page
            });
            starts.push((label.to_owned(), pdf_page, clean_offset));
        } else {
            clean.push_str(line);
            clean_offset += utf16_len(line);
        }
    }
    let mut claims = Vec::with_capacity(starts.len());
    for (index, (label, pdf_page, start)) in starts.iter().enumerate() {
        let mut item = claim(
            NodeKind::Page,
            public_label(label),
            ScalarRange {
                start: *start,
                end: starts.get(index + 1).map_or(clean_offset, |value| value.2),
            },
            vec![label.clone()],
        );
        item.anchor = pdf_page.map(|pdf_page| format!("page={pdf_page}"));
        item.page_indexes = pdf_page.and_then(|pdf_page| pdf_page.checked_sub(1)).into_iter().collect();
        claims.push(item);
    }
    structure(article_id, url, clean, claims)
}

pub fn journal_document_structure(
    article_id: usize,
    url: Option<String>,
    reader: impl BufRead,
    page_labels: &[JournalPageLabel],
) -> Result<DocumentStructure, EngineError> {
    let mut text = String::new();
    let mut claims = Vec::new();
    let mut titles = Vec::new();
    let mut paired_refs = HashSet::new();
    let mut notes = Vec::<(String, String, Option<Region>)>::new();
    let mut offset = 0;
    let mut paragraphs = 0;
    let mut pages = 0;
    let page_labels = page_labels
        .iter()
        .rev()
        .map(|value| (value.pdf_page, value.label.trim()))
        .collect::<HashMap<_, _>>();

    for line in reader.lines() {
        let line = line.map_err(EngineError::source)?;
        if line.trim().is_empty() {
            continue;
        }
        let page: Page = serde_json::from_str(&line).map_err(EngineError::source)?;
        if positive_usize(page.article_id.as_ref()).is_some_and(|value| value != article_id) {
            return Err(EngineError::source(
                "journal page belongs to another article",
            ));
        }
        if pages > 0 {
            text.push('\n');
            offset += 1;
        }
        pages += 1;
        // Original-page byte matches enter page text joined by one synthetic LF.
        let page_start = offset;
        let page_coordinates = ScalarText::new(&page.text);
        text.push_str(&page.text);
        offset += page_coordinates.utf16_len();
        let pdf_page = page.pdf_page.filter(|value| *value > 0);
        if let Some(pdf_page) = pdf_page {
            let label = page_labels
                .get(&pdf_page)
                .copied()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| pdf_page.to_string());
            let mut item = claim(
                NodeKind::Page,
                public_label(&label),
                ScalarRange {
                    start: page_start,
                    end: offset,
                },
                vec![label],
            );
            item.anchor = Some(format!("page={pdf_page}"));
            item.page_indexes = vec![pdf_page - 1];
            claims.push(item);
        }

        let mut footnotes = HashMap::new();
        let mut cursor = 0;
        let mut regions = page.regions.into_iter().enumerate().collect::<Vec<_>>();
        regions.sort_by(|(left_index, left), (right_index, right)| {
            left.order
                .unwrap_or(*left_index as f64)
                .partial_cmp(&right.order.unwrap_or(*right_index as f64))
                .unwrap_or(Ordering::Equal)
        });
        for (_, mut region) in regions {
            if region.text.is_empty() {
                region.text = region
                    .lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            if region.text.is_empty() {
                continue;
            }
            let Some(found) = page.text[cursor..].find(&region.text) else {
                continue;
            };
            let start_byte = cursor + found;
            cursor = start_byte + region.text.len();
            let placed = Region {
                start: page_start
                    + page_coordinates
                        .utf16_at_byte(start_byte)
                        .expect("matched journal region starts at a UTF-8 boundary"),
                end: page_start
                    + page_coordinates
                        .utf16_at_byte(cursor)
                        .expect("matched journal region ends at a UTF-8 boundary"),
                pdf_page,
            };
            if region.kind.as_deref() == Some("text") {
                paragraphs += 1;
                claims.push(claim(
                    NodeKind::Paragraph,
                    format!("{paragraphs}"),
                    ScalarRange {
                        start: placed.start,
                        end: placed.end,
                    },
                    Vec::new(),
                ));
            }
            if region.kind.as_deref() == Some("paragraph_title") {
                let (label, aliases) = title(&region.text);
                titles.push(Title {
                    start: placed.start,
                    label,
                    aliases,
                });
            }
            if region.kind.as_deref() == Some("footnote") {
                for line in region.lines {
                    if let Some(order) = line.codex_text_order.filter(|value| *value > 0) {
                        footnotes.entry(order).or_insert(placed);
                    }
                }
            }
        }
        for annotation in page.annotations {
            let pair = annotation.pair_id.as_deref().unwrap_or_default();
            if pair.is_empty() || annotation.pair_status.as_deref() != Some("paired") {
                continue;
            }
            match annotation.taxonomy_name.as_deref() {
                Some("fn_ref") => {
                    paired_refs.insert(pair.to_owned());
                }
                Some("fn_label") => {
                    let note = value_text(
                        annotation
                            .note_id
                            .as_ref()
                            .or(annotation.selected_text.as_ref()),
                    )
                    .trim()
                    .to_owned();
                    if let (false, Some(order)) = (
                        note.is_empty(),
                        annotation.start_line_order.filter(|value| *value > 0),
                    ) {
                        notes.push((pair.to_owned(), note, footnotes.get(&order).copied()));
                    }
                }
                _ => {}
            }
        }
    }
    if pages == 0 || text.trim().is_empty() {
        return Err(EngineError::source("journal export has no usable pages"));
    }

    for (index, title) in titles.iter().enumerate() {
        let mut section = claim(
            NodeKind::Section,
            title.label.clone().unwrap_or_else(|| format!("Title{}", index + 1)),
            ScalarRange {
                start: title.start,
                end: titles.get(index + 1).map_or(offset, |value| value.start),
            },
            title.aliases.clone(),
        );
        section.locator_kind = Some("section".to_owned());
        claims.push(section);
    }
    let mut used_pairs = HashSet::new();
    for (pair, note, region) in notes {
        if !paired_refs.contains(&pair) || !used_pairs.insert(pair) {
            continue;
        }
        let Some(region) = region else { continue };
        let mut item = claim(
            NodeKind::Footnote,
            public_label(&note),
            ScalarRange {
                start: region.start,
                end: region.end,
            },
            vec![note],
        );
        item.anchor = region.pdf_page.map(|page| format!("page={page}"));
        claims.push(item);
    }
    claims.sort_by_key(|claim| claim.range);
    structure(article_id, url, text, claims)
}
