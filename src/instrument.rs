#[cfg(feature = "structure-inference")]
use crate::{
    definitions::{derive_definitions_bytes, DefinitionHit},
    instrument_contents::instrument_contents_outline_indexed,
    instrument_references::{
        find_provision_references, populate_instrument_node_metadata,
        resolve_instrument_references, FindProvisionReferencesOptions,
    },
    javascript_whitespace, node_depths, AuthoritativeTableCell, AuthoritativeTables, Block,
    CoverageState, DefinedTerm, DefinitionOccurrence, DetectionProfile, DocumentInput,
    DocumentStructure, EngineError, NodeKind, Origin, ScalarRange, ScalarText, Scope,
};
#[cfg(feature = "structure-inference")]
use regex::Regex;
use serde::{Deserialize, Serialize};
#[cfg(feature = "structure-inference")]
use sha2::{Digest, Sha256};
#[cfg(feature = "structure-inference")]
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::OnceLock,
};

#[cfg(feature = "structure-inference")]
fn split_instrument_space_runs(text: &str) -> Option<String> {
    let mut recovered: Option<String> = None;
    let mut characters = text.char_indices().peekable();
    while let Some((start, character)) = characters.next() {
        if !matches!(character, ' ' | '\t') {
            if let Some(recovered) = &mut recovered {
                recovered.push(character);
            }
            continue;
        }
        let mut count = 1;
        while characters
            .next_if(|(_, character)| matches!(character, ' ' | '\t'))
            .is_some()
        {
            count += 1;
        }
        let end = characters.peek().map_or(text.len(), |(byte, _)| *byte);
        let internal_run = count >= 2
            && start > 0
            && end < text.len()
            && !javascript_whitespace(text[..start].chars().next_back().unwrap())
            && !javascript_whitespace(text[end..].chars().next().unwrap());
        if internal_run {
            let recovered = recovered.get_or_insert_with(|| text[..start].to_owned());
            recovered.push('\n');
            recovered.push_str(&text[start + 1..end]);
        } else if let Some(recovered) = &mut recovered {
            recovered.push_str(&text[start..end]);
        }
    }
    recovered
}

#[cfg(feature = "structure-inference")]
fn split_instrument_sentence_joins(text: &str) -> Option<String> {
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let head = HEAD.get_or_init(|| {
        Regex::new(
            r"^(?:(?:ARTICLE|Article|PART|Part|DIVISION|Division|Section|SECTION|SCHEDULE|Schedule|EXHIBIT|Exhibit|ANNEX|Annex|APPENDIX|Appendix)[\s\u{feff}]+[IVXLCDM0-9]|[0-9]{1,3}\.[0-9]{1,3}(?:\.[0-9]{1,3})*[\s\u{feff}]+\S|\([A-Za-z0-9_]{1,3}\)[\s\u{feff}])",
        )
        .expect("valid instrument sentence-join grammar")
    });
    let mut recovered: Option<String> = None;
    let mut previous = None;
    let mut previous_previous = None;
    for (byte, character) in text.char_indices() {
        let preceded_by_terminator = previous.is_some()
            && (matches!(previous, Some('.' | ';' | ':'))
                || (matches!(
                    previous,
                    Some(')' | ']' | '"' | '\'' | '\u{201d}' | '\u{2019}' | '\u{00bb}')
                ) && matches!(previous_previous, Some('.' | ';' | ':'))));
        let after = byte + character.len_utf8();
        if matches!(character, ' ' | '\t')
            && preceded_by_terminator
            && head.is_match(&text[after..])
        {
            let recovered = recovered.get_or_insert_with(|| text[..byte].to_owned());
            recovered.push('\n');
        } else if let Some(recovered) = &mut recovered {
            recovered.push(character);
        }
        previous_previous = previous;
        previous = Some(character);
    }
    recovered
}

/// Offset-preserving lineation recoveries used by the instrument structure profile.
#[cfg(feature = "structure-inference")]
fn instrument_lineation_recoveries(text: &str) -> impl Iterator<Item = String> + '_ {
    (0..3)
        .scan(None, move |joined, recovery| {
            Some(match recovery {
                0 => split_instrument_space_runs(text),
                1 => split_instrument_sentence_joins(text)
                    .inspect(|recovered| *joined = Some(recovered.clone())),
                2 => joined
                    .take()
                    .and_then(|joined| split_instrument_space_runs(&joined)),
                _ => None,
            })
        })
        .flatten()
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProvisionReferenceShape {
    Numeric,
    SubOnly,
    Roman,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProvisionReference {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) raw: String,
    pub(crate) word: String,
    pub(crate) plural: bool,
    pub(crate) label: String,
    pub(crate) shape: ProvisionReferenceShape,
    pub(crate) locator: String,
    pub(crate) alias_key: String,
    pub(crate) external: bool,
    pub(crate) continuation_of: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentContentsEntry {
    pub label: String,
    pub display: String,
    pub heading: String,
    pub depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_label: Option<String>,
    pub page: Option<u32>,
    pub contents_line_start: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentContentsOutline {
    pub entries: Vec<InstrumentContentsEntry>,
    pub region_start: usize,
    pub region_end: usize,
    pub pages_cited: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentContentsRefusal {
    NoContentsMarker,
    NoContentsEntries,
    TooFewContentsEntries,
    ContentsWithoutPageNumbers,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct InstrumentContentsReading {
    pub outline: Option<InstrumentContentsOutline>,
    pub refusal: Option<InstrumentContentsRefusal>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentCrossReferenceStatus {
    Resolved,
    External,
    Unresolved,
    Abstained,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentCrossReferenceReason {
    ExternalInstrument,
    DocumentAbstained,
    NoContainingSection,
    AmbiguousLabel,
    DepthNotNumbered,
    NoSuchProvision,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceEdge {
    pub source_start: usize,
    pub source_end: usize,
    pub source_label: Option<String>,
    pub raw: String,
    pub raw_label: String,
    pub normalized_locator: String,
    pub target_label: Option<String>,
    pub target_start: Option<usize>,
    pub target_end: Option<usize>,
    pub status: InstrumentCrossReferenceStatus,
    pub self_loop: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<InstrumentCrossReferenceReason>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceCounts {
    pub detected: usize,
    pub resolved: usize,
    pub external: usize,
    pub unresolved: usize,
    pub abstained: usize,
    pub self_loops: usize,
    pub integrity: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceGraph {
    pub edges: Vec<InstrumentCrossReferenceEdge>,
    pub document_abstained: bool,
    pub note: Option<String>,
    pub counts: InstrumentCrossReferenceCounts,
}

#[cfg(feature = "structure-inference")]
pub(super) fn instrument_roman(mut value: usize) -> String {
    let mut result = String::new();
    for (amount, numeral) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while value >= amount {
            result.push_str(numeral);
            value -= amount;
        }
    }
    result
}

#[cfg(feature = "structure-inference")]
pub(crate) fn cross_reference_graph(
    structure: &DocumentStructure,
) -> Result<InstrumentCrossReferenceGraph, EngineError> {
    let text = ScalarText::new(&structure.text);
    let references = find_provision_references(&text, FindProvisionReferencesOptions::default());
    let depths = node_depths(&structure.nodes);
    resolve_instrument_references(&text, structure, references, &depths)
}

#[cfg(feature = "structure-inference")]
fn instrument_reference_index<'a>(
    sections: impl Iterator<Item = (&'a str, usize)>,
) -> HashMap<String, usize> {
    let mut index = HashMap::new();
    let mut duplicates = HashSet::new();
    for (label, start) in sections {
        let aliases = [("art", "article"), ("part", "part"), ("div", "division")];
        let keys = std::iter::once(label.to_ascii_lowercase()).chain(
            aliases.into_iter().filter_map(|(prefix, word)| {
                label
                    .strip_prefix(prefix)
                    .and_then(|value| value.parse().ok())
                    .map(|value| format!("{word} {}", instrument_roman(value)))
            }),
        );
        for key in keys {
            match index.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(start);
                }
                Entry::Occupied(entry) => {
                    duplicates.insert(entry.key().clone());
                }
            }
        }
    }
    for key in duplicates {
        index.remove(&key);
    }
    index
}

#[cfg(feature = "structure-inference")]
fn endorsed_references(references: &[ProvisionReference]) -> Vec<(String, usize, usize)> {
    references
        .iter()
        .filter(|reference| !reference.external)
        .filter_map(|reference| {
            let key = if reference.shape == ProvisionReferenceShape::Roman {
                reference.alias_key.clone()
            } else {
                reference.locator.to_lowercase()
            };
            (!key.is_empty()).then_some((key, reference.start, reference.end))
        })
        .collect()
}

#[cfg(feature = "structure-inference")]
fn instrument_lineation_score_blocks(
    blocks: &[Block],
    endorsed: &[(String, usize, usize)],
    coordinates: &ScalarText<'_>,
) -> usize {
    instrument_lineation_score_sections(
        blocks.iter().filter_map(|block| {
            (block.kind == NodeKind::Section)
                .then_some(block.label.as_deref())
                .flatten()
                .map(|label| (label, coordinates.utf16(block.range.start)))
        }),
        endorsed,
    )
}

#[cfg(feature = "structure-inference")]
fn instrument_lineation_score_sections<'a>(
    sections: impl Iterator<Item = (&'a str, usize)>,
    endorsed: &[(String, usize, usize)],
) -> usize {
    let index = instrument_reference_index(sections);
    endorsed
        .iter()
        .filter(|(key, start, end)| {
            index
                .get(key)
                .is_some_and(|target| *target < *start || *target >= *end)
        })
        .count()
}

#[cfg(feature = "structure-inference")]
fn instrument_block_head_span(blocks: &[Block], coordinates: &ScalarText<'_>) -> f64 {
    instrument_head_span_sections(
        blocks.iter().filter_map(|block| {
            let label = block.label.as_deref()?;
            (block.kind == NodeKind::Section)
                .then_some((label, coordinates.utf16(block.range.start)))
        }),
        coordinates.utf16_len(),
    )
}

#[cfg(feature = "structure-inference")]
fn instrument_head_span_sections<'a>(
    sections: impl Iterator<Item = (&'a str, usize)>,
    text_length: usize,
) -> f64 {
    let mut starts = sections.filter_map(|(label, start)| {
        (label.starts_with("sec") && !label.contains('(')).then_some(start)
    });
    let Some(first) = starts.next() else {
        return 0.0;
    };
    let (mut low, mut high) = (first, first);
    for start in starts {
        low = low.min(start);
        high = high.max(start);
    }
    if text_length == 0 {
        0.0
    } else {
        (high - low) as f64 / text_length as f64
    }
}

#[cfg(feature = "structure-inference")]
fn derive_instrument_structure(
    text: String,
    document_id: String,
    original_sha256: &str,
    table_cells: &[AuthoritativeTableCell],
    reconstruct_lineation: bool,
) -> Result<DocumentStructure, EngineError> {
    let coordinates = ScalarText::new(&text);
    let tables = AuthoritativeTables::new(&coordinates, table_cells)?;
    let text_view = &coordinates;
    let references =
        find_provision_references(text_view, FindProvisionReferencesOptions::default());
    let endorsed = endorsed_references(&references);
    let first_is_original = tables.is_empty();
    let mut selected_text = (!first_is_original).then(|| tables.masked_text(text.clone()));
    let selected_view = selected_text.as_ref().map(|selected_text| {
        if selected_text.len() == text_view.value.len() {
            text_view.with_same_coordinates(selected_text)
        } else {
            ScalarText::new(selected_text)
        }
    });
    let selected_coordinates = selected_view.as_ref().unwrap_or(text_view);
    let mut selected_blocks = crate::inference::detect_instrument(selected_coordinates);
    let mut selected = 0;
    let mut best = if endorsed.is_empty() {
        0
    } else {
        instrument_lineation_score_blocks(&selected_blocks, &endorsed, selected_coordinates)
    };
    drop(selected_view);
    if reconstruct_lineation && best < endorsed.len() {
        for (index, hypothesis) in instrument_lineation_recoveries(&text).enumerate() {
            let candidate_text = tables.masked_text(hypothesis);
            let candidate_view = if candidate_text.len() == text_view.value.len() {
                text_view.with_same_coordinates(&candidate_text)
            } else {
                ScalarText::new(&candidate_text)
            };
            let candidate = crate::inference::detect_instrument(&candidate_view);
            if instrument_block_head_span(&candidate, &candidate_view) < 0.05 {
                continue;
            }
            let score = instrument_lineation_score_blocks(&candidate, &endorsed, &candidate_view);
            if score > best {
                selected = index + 1;
                best = score;
                selected_text = Some(candidate_text);
                selected_blocks = candidate;
            }
            if best == endorsed.len() {
                break;
            }
        }
    }
    let selected_original = selected == 0 && first_is_original;
    let scalar_end = text_view.len();
    drop(coordinates);
    let (input_text, original_text) = if selected_original {
        (text, None)
    } else {
        (
            selected_text.expect("a non-original hypothesis owns its text"),
            Some(text),
        )
    };
    let input = DocumentInput {
        document_id,
        provider: "internal".to_owned(),
        url: None,
        doc_type: None,
        profile: DetectionProfile::Instrument,
        report_start_page: None,
        require_report_start: false,
        allow_hyphenated_sections: false,
        text: input_text,
        text_sha256: selected_original
            .then(|| original_sha256.to_owned())
            .unwrap_or_default(),
        source_sha256: None,
        scope: Scope::complete(),
        origins: vec![Origin {
            id: "provider-adapter".to_owned(),
        }],
        native_claims: Vec::new(),
        coverage: crate::whole_document_coverage(scalar_end, |_| CoverageState::Absent),
        exclusions: Vec::new(),
    };
    let mut structure = crate::derive::derive_trusted_inferred(input, selected_blocks)?;
    structure.selected_hypothesis = Some(selected);
    let original = original_text.as_deref().unwrap_or(&structure.text);
    let text = ScalarText::new(original);
    let selected_coordinates = (!selected_original).then(|| {
        if structure.text.len() == text.value.len() {
            text.with_same_coordinates(&structure.text)
        } else {
            ScalarText::new(&structure.text)
        }
    });
    populate_instrument_node_metadata(
        &mut structure.nodes,
        selected_coordinates.as_ref().unwrap_or(&text),
    );
    structure.contents = Some(instrument_contents_outline_indexed(&text));
    let table_nodes = tables.nodes(&structure.nodes, "provider-adapter");
    structure.nodes.extend(table_nodes);
    let depths = node_depths(&structure.nodes);
    structure.cross_references = Some(resolve_instrument_references(
        &text, &structure, references, &depths,
    )?);
    let sections = structure
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == NodeKind::Section)
        .collect::<Vec<_>>();
    let lines = text.lines();
    let raw = derive_definitions_bytes(&text, lines.len(), |index| {
        (lines[index][0], lines[index][1])
    });
    let mut paragraphs = raw
        .iter()
        .flat_map(|term| term.definitions.iter().chain(&term.uses))
        .map(|hit| hit.paragraph)
        .collect::<Vec<_>>();
    paragraphs.sort_unstable();
    paragraphs.dedup();
    let mut next_section = 0;
    let mut active_sections = Vec::new();
    let mut owners = Vec::with_capacity(paragraphs.len());
    for &paragraph in &paragraphs {
        let start = text.utf16_at_byte(lines[paragraph][0]).unwrap();
        while next_section < sections.len() && sections[next_section].1.range.start <= start {
            active_sections.push(sections[next_section]);
            next_section += 1;
        }
        active_sections.retain(|(_, node)| start < node.range.end);
        owners.push(
            active_sections
                .iter()
                .max_by_key(|(index, node)| (depths[node.id.as_str()], *index))
                .map(|(_, node)| node.id.as_str()),
        );
    }
    let occurrence = |hit: DefinitionHit| DefinitionOccurrence {
        range: ScalarRange {
            start: text.utf16_at_byte(hit.start).unwrap(),
            end: text.utf16_at_byte(hit.end).unwrap(),
        },
        node_id: owners[paragraphs.binary_search(&hit.paragraph).unwrap()].map(str::to_owned),
        source_paragraph_id: hit.paragraph.to_string(),
        source_artifact_id: Some(structure.document_id.clone()),
    };
    let definitions = raw
        .into_iter()
        .map(|term| DefinedTerm {
            term: term.term,
            definitions: term.definitions.into_iter().map(&occurrence).collect(),
            uses: term.uses.into_iter().map(&occurrence).collect(),
        })
        .collect();
    structure.definitions = definitions;
    if !tables.is_empty() {
        let depths = depths
            .into_iter()
            .map(|(id, depth)| (id.to_owned(), depth))
            .collect::<HashMap<_, _>>();
        structure
            .nodes
            .sort_by_key(|node| (node.range.start, depths[node.id.as_str()]));
    }
    drop(selected_coordinates);
    drop(text);
    if let Some(original) = original_text {
        structure.text = original;
    }
    structure.text_sha256 = original_sha256.to_owned();
    structure.revision = original_sha256.to_owned();
    Ok(structure)
}

#[cfg(feature = "structure-inference")]
pub fn analyze_instrument(
    text: impl Into<String>,
    document_id: String,
    table_cells: &[AuthoritativeTableCell],
    reconstruct_lineation: bool,
) -> Result<DocumentStructure, EngineError> {
    let text = text.into();
    let original_sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
    derive_instrument_structure(
        text,
        document_id,
        &original_sha256,
        table_cells,
        reconstruct_lineation,
    )
}
