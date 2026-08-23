use crate::{
    utf16_len, Block, CoverageState, Derivation, DetectionProfile, DocumentInput,
    DocumentStructure, DocumentType, EngineError, EvidenceKind, NativeClaim, NodeKind, Origin,
    ScalarRange, ScalarText, Scope, ScopeKind, EVIDENCE_SCHEMA,
};
use aho_corasick::AhoCorasick;
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const ORIGIN: &str = "provider-adapter";

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum A2ajSourceKind {
    Cases,
    Laws,
}

/// A section map is ordered provider data, not a generic JSON object.
pub type A2ajSectionMap = Vec<(String, String)>;

#[derive(Deserialize)]
pub struct A2ajInput {
    pub citation: String,
    pub source_kind: A2ajSourceKind,
    pub text: String,
    pub id: Option<String>,
    pub url: Option<String>,
    pub dataset: Option<String>,
    pub name: Option<String>,
    pub alternate_citation: Option<String>,
    pub section_map: Option<A2ajSectionMap>,
    pub excerpt_of: Option<String>,
}

impl A2ajInput {
    pub fn new(
        citation: impl Into<String>,
        source_kind: A2ajSourceKind,
        text: impl Into<String>,
    ) -> Self {
        Self {
            citation: citation.into(),
            source_kind,
            text: text.into(),
            id: None,
            url: None,
            dataset: None,
            name: None,
            alternate_citation: None,
            section_map: None,
            excerpt_of: None,
        }
    }
}

fn validate_section_map(map: &A2ajSectionMap) -> Result<(), EngineError> {
    let mut seen = HashSet::new();
    if map.iter().any(|(key, _)| !seen.insert(key)) {
        return Err(EngineError::source("duplicate A2AJ section-map key"));
    }
    Ok(())
}

fn object_entries(map: &A2ajSectionMap) -> Result<Vec<(usize, &str, &str)>, EngineError> {
    validate_section_map(map)?;
    let mut entries = map
        .iter()
        .enumerate()
        .map(|(index, (key, value))| (index, key.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    entries.sort_by_key(|(index, key, _)| {
        let integer = key
            .parse::<u32>()
            .ok()
            .filter(|number| number.to_string() == *key && *number < u32::MAX);
        (integer.is_none(), integer.unwrap_or_default(), *index)
    });
    Ok(entries)
}

fn ordered_sections(map: &A2ajSectionMap) -> Result<Vec<(&str, &str)>, EngineError> {
    let mut entries = object_entries(map)?;
    let order = crate::inference::dotted_order(entries.iter().map(|(_, label, _)| *label));
    entries.sort_by(|left, right| {
        let (a, b) = (left.1.trim(), right.1.trim());
        let preamble =
            |value: &str| matches!(value.to_lowercase().as_str(), "preamble" | "préambule");
        let provisions = (
            crate::inference::provision_label(a).is_some_and(|(_, end)| end == a.len()),
            crate::inference::provision_label(b).is_some_and(|(_, end)| end == b.len()),
        );
        preamble(b)
            .cmp(&preamble(a))
            .then_with(|| provisions.1.cmp(&provisions.0))
            .then_with(|| {
                if !provisions.0 {
                    Ordering::Equal
                } else if let Some(order) = order {
                    crate::inference::compare_labels(a, b, order)
                } else {
                    let component = crate::inference::compare_labels(a, b, false);
                    let fraction = crate::inference::compare_labels(a, b, true);
                    if component == fraction {
                        component
                    } else {
                        left.0.cmp(&right.0)
                    }
                }
            })
    });
    Ok(entries
        .into_iter()
        .map(|(_, label, value)| (label, value))
        .collect())
}

#[cfg(test)]
fn utf16_at(text: &str, byte: usize) -> usize {
    utf16_len(&text[..byte])
}

fn provider_source(mut text: String, entries: &[(&str, &str)]) -> (String, Vec<NativeClaim>) {
    if !text.trim().is_empty() || entries.is_empty() {
        return (text, Vec::new());
    }
    text.clear();
    text.reserve(entries.iter().map(|(_, value)| value.len() + 1).sum());
    let mut claims = Vec::with_capacity(entries.len());
    let mut scalar = 0;
    for (label, value) in entries.iter().filter(|(_, value)| {
        !value.trim().is_empty() && !value.trim().eq_ignore_ascii_case("[blank]")
    }) {
        if !text.is_empty() {
            text.push('\n');
            scalar += 1;
        }
        let start = scalar;
        text.push_str(value);
        scalar += value.chars().count();
        claims.push(NativeClaim {
            id: format!("native-{:06}", claims.len() + 1),
            kind: EvidenceKind::Section,
            label: Some(format!("sec{}", label.trim())),
            aliases: Vec::new(),
            range: ScalarRange { start, end: scalar },
            origin_id: ORIGIN.to_owned(),
            parent_label: None,
            anchor: None,
        });
    }
    (text, claims)
}

fn provider_claims(coordinates: &ScalarText<'_>, map: &A2ajSectionMap) -> Vec<NativeClaim> {
    static PRINTED: OnceLock<Regex> = OnceLock::new();
    let text = coordinates.value;
    let candidates = map
        .iter()
        .filter_map(|(raw_label, value)| {
            let label = raw_label.trim();
            (!label.is_empty()
                && !value.trim().is_empty()
                && !value.trim().eq_ignore_ascii_case("[blank]"))
            .then_some((label, value.as_str()))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Vec::new();
    }
    let mut patterns = Vec::new();
    let mut pattern_ids = Vec::with_capacity(candidates.len());
    let mut by_value = HashMap::new();
    for (_, value) in &candidates {
        pattern_ids.push((value.len() <= text.len()).then(|| {
            *by_value.entry(*value).or_insert_with(|| {
                patterns.push(*value);
                patterns.len() - 1
            })
        }));
    }
    let mut matches = vec![(0, 0, 0); patterns.len()];
    if !patterns.is_empty() {
        let matcher = AhoCorasick::new(patterns).unwrap();
        for found in matcher.find_overlapping_iter(text) {
            let matched = &mut matches[found.pattern().as_usize()];
            if matched.2 < 2 && found.start() >= matched.1 {
                *matched = (found.start(), found.end(), matched.2 + 1);
            }
        }
    }
    let mut claims = Vec::new();
    for ((label, value), pattern) in candidates.into_iter().zip(pattern_ids) {
        let (start, _, count) = pattern.map(|index| matches[index]).unwrap_or_default();
        if count != 1 {
            continue;
        }
        let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
        if PRINTED
            .get_or_init(|| Regex::new(r"^([^\s.)]+)[.)]?$").unwrap())
            .is_match(text[line_start..start].trim())
        {
            continue;
        }
        claims.push(NativeClaim {
            id: format!("native-{:06}", claims.len() + 1),
            kind: EvidenceKind::Section,
            label: Some(format!("sec{label}")),
            aliases: Vec::new(),
            range: ScalarRange {
                start: coordinates.scalar(start),
                end: coordinates.scalar(start + value.len()),
            },
            origin_id: ORIGIN.to_owned(),
            parent_label: None,
            anchor: None,
        });
    }
    claims
}

fn evidence(
    input: &A2ajInput,
    text: String,
    claims: Vec<NativeClaim>,
) -> Result<DocumentInput, EngineError> {
    let profile = if input.source_kind == A2ajSourceKind::Cases {
        DetectionProfile::CaseRootedComplete
    } else {
        DetectionProfile::Legislation
    };
    let report_start_page = report_start(input);
    let require_report_start = input.source_kind == A2ajSourceKind::Cases
        && input
            .dataset
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("SCC"));
    let allow_hyphenated_sections = input.source_kind == A2ajSourceKind::Laws
        && input.name.as_deref().is_some_and(|value| {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| {
                Regex::new(r"(?iu)\b(?:rules?|regulations?|r[eè]glements?)\b").unwrap()
            })
            .is_match(value)
        });
    let text_sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
    Ok(DocumentInput {
        schema_version: EVIDENCE_SCHEMA.to_owned(),
        document_id: input.id.clone().unwrap_or_else(|| input.citation.clone()),
        provider: "a2aj".to_owned(),
        url: input.url.clone(),
        doc_type: Some(if input.source_kind == A2ajSourceKind::Cases {
            DocumentType::Cases
        } else {
            DocumentType::Laws
        }),
        provider_revision: "a2aj-adapter-v1".to_owned(),
        profile,
        report_start_page,
        require_report_start,
        allow_hyphenated_sections,
        text,
        text_sha256,
        source_sha256: None,
        offset_unit: "unicode-scalar".to_owned(),
        scope: Scope {
            kind: if input.excerpt_of.is_some() {
                ScopeKind::Excerpt
            } else {
                ScopeKind::Complete
            },
            excerpt_of: input.excerpt_of.clone(),
        },
        origins: vec![Origin {
            id: ORIGIN.to_owned(),
        }],
        native_claims: claims,
        coverage: Vec::new(),
        exclusions: Vec::new(),
        paragraph_breaks: Vec::new(),
    })
}

fn report_start(input: &A2ajInput) -> Option<u32> {
    std::iter::once(input.citation.as_str())
        .chain(input.alternate_citation.as_deref())
        .find_map(crate::canadian_report_start)
}

fn words(text: &str) -> Vec<(Cow<'_, str>, usize, usize, usize, usize)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Keep these running totals local: tokenization already walks provider text
    // once and needs byte and UTF-16 spans for every match.
    let mut previous = 0;
    let mut utf16 = 0;
    RE.get_or_init(|| Regex::new(r"[\p{L}\p{N}]+(?:['’][\p{L}\p{N}]+)*").unwrap())
        .find_iter(text)
        .map(|item| {
            utf16 += utf16_len(&text[previous..item.start()]);
            let start = utf16;
            utf16 += utf16_len(item.as_str());
            previous = item.end();
            let word = item.as_str();
            let word = if word.is_ascii() && !word.bytes().any(|byte| byte.is_ascii_uppercase()) {
                Cow::Borrowed(word)
            } else {
                Cow::Owned(word.to_lowercase())
            };
            (word, start, utf16, item.start(), item.end())
        })
        .collect()
}

fn apply_provider_section_evidence(
    coordinates: &ScalarText<'_>,
    blocks: &mut Vec<Block>,
    native_claims: &[NativeClaim],
    map: &A2ajSectionMap,
) {
    let text = coordinates.value;
    let mut counts = HashMap::new();
    for (label, _) in map {
        *counts.entry(label.trim().to_lowercase()).or_insert(0) += 1;
    }
    let native_top_sections = native_claims
        .iter()
        .filter(|claim| claim.kind == EvidenceKind::Section && claim.parent_label.is_none())
        .flat_map(|claim| claim.label.iter().chain(&claim.aliases))
        .map(|label| label.to_lowercase())
        .collect::<HashSet<_>>();
    let selected_sections = map
        .iter()
        .filter(|(label, provider_text)| {
            let label = label.trim();
            !label.is_empty()
                && counts.get(&label.to_lowercase()) == Some(&1)
                && !provider_text.trim().is_empty()
                && !provider_text.trim().eq_ignore_ascii_case("[blank]")
                && !native_top_sections.contains(&format!("sec{label}").to_lowercase())
        })
        .collect::<Vec<_>>();
    let tokens = (!selected_sections.is_empty())
        .then(|| words(text))
        .unwrap_or_default();
    let mut postings = HashMap::<&str, Vec<usize>>::new();
    for (index, (word, ..)) in tokens.iter().enumerate() {
        postings.entry(word.as_ref()).or_default().push(index);
    }
    let mut top_sections = HashMap::<String, Vec<usize>>::new();
    for (index, block) in blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.kind == NodeKind::Section && block.parent_label.is_none())
    {
        for label in block.label.iter().chain(&block.aliases) {
            let candidates = top_sections.entry(label.to_lowercase()).or_default();
            if candidates.last() != Some(&index) {
                candidates.push(index);
            }
        }
    }
    for (label, provider_text) in selected_sections {
        let label = label.trim();
        let phrase = words(provider_text);
        if phrase.is_empty() {
            continue;
        }
        let (anchor_offset, anchor_word) = phrase
            .iter()
            .enumerate()
            .min_by_key(|(_, word)| postings.get(word.0.as_ref()).map_or(0, Vec::len))
            .unwrap();
        let mut spans = Vec::new();
        for &position in postings.get(anchor_word.0.as_ref()).into_iter().flatten() {
            let Some(start) = position.checked_sub(anchor_offset) else {
                continue;
            };
            if start + phrase.len() <= tokens.len()
                && tokens[start..start + phrase.len()]
                    .iter()
                    .map(|token| &token.0)
                    .eq(phrase.iter().map(|token| &token.0))
            {
                spans.push((start, tokens[start].1, tokens[start + phrase.len() - 1].2));
                if spans.len() == 2 {
                    break;
                }
            }
        }
        if spans.len() != 1 {
            continue;
        }
        let first_token = spans[0].0;
        let last_token = first_token + phrase.len() - 1;
        let body = tokens[first_token]
            .3
            .checked_sub(phrase[0].3)
            .and_then(|body_start| {
                body_start
                    .checked_add(provider_text.len())
                    .filter(|&body_end| text.get(body_start..body_end) == Some(provider_text))
                    .map(|body_end| {
                        let body_utf16_start = tokens[first_token].1 - phrase[0].1;
                        let body_utf16_end = tokens[last_token].2
                            + utf16_len(&provider_text[phrase[phrase.len() - 1].4..]);
                        (body_start, body_end, body_utf16_start, body_utf16_end)
                    })
            });
        let provider_label = format!("sec{label}");
        let key = provider_label.to_lowercase();
        let candidates = top_sections.get(&key).map(Vec::as_slice).unwrap_or(&[]);
        if candidates.len() == 1 {
            let index = candidates[0];
            if spans[0].1 < coordinates.utf16(blocks[index].range.start)
                || spans[0].2 > coordinates.utf16(blocks[index].range.end)
            {
                continue;
            }
            blocks[index].source = Derivation::Native;
            blocks[index].origin_id = ORIGIN;
            blocks[index].diagnostic = None;
        } else if candidates.is_empty() {
            let Some((body_start, _, body_utf16_start, body_utf16_end)) = body else {
                continue;
            };
            let line_start = text[..body_start].rfind('\n').map_or(0, |at| at + 1);
            let prefix = &text[line_start..body_start];
            let lead = prefix.len() - prefix.trim_start_matches([' ', '\t']).len();
            let printed = prefix[lead..].trim_end_matches([' ', '\t']);
            let printed = std::iter::once(printed)
                .chain(
                    printed
                        .strip_suffix(['.', ')', ':', '-', '–', '—'])
                        .map(str::trim_end),
                )
                .find(|printed| printed.eq_ignore_ascii_case(label));
            let start = printed.map_or(body_utf16_start, |_| {
                body_utf16_start - utf16_len(&text[line_start + lead..body_start])
            });
            let mut block = Block::labelled(
                NodeKind::Section,
                provider_label,
                coordinates.scalar_at_utf16(start).unwrap(),
                coordinates.scalar_at_utf16(body_utf16_end).unwrap(),
            );
            block.source = Derivation::Native;
            block.origin_id = ORIGIN;
            blocks.push(block);
        }
    }
    let mut seen = HashSet::new();
    blocks.retain(|block| seen.insert((block.label.clone(), block.range)));
    blocks.sort_by_key(|block| (block.range.start, block.parent_label.is_some()));
}

pub fn a2aj_document_structure(mut input: A2ajInput) -> Result<DocumentStructure, EngineError> {
    let has_text = !input.text.trim().is_empty();
    let ordered = match (&input.section_map, has_text) {
        (Some(map), true) => {
            validate_section_map(map)?;
            Vec::new()
        }
        (Some(map), false) => ordered_sections(map)?,
        (None, _) => Vec::new(),
    };
    let (text, claims) = provider_source(std::mem::take(&mut input.text), &ordered);
    let mut evidence = evidence(&input, text, claims)?;
    let coordinates = ScalarText::new(&evidence.text);
    if let (true, Some(map)) = (has_text, &input.section_map) {
        evidence.native_claims = provider_claims(&coordinates, map);
    }
    evidence.coverage = crate::whole_document_coverage(coordinates.len(), |kind| {
        if kind == EvidenceKind::Section && !evidence.native_claims.is_empty() {
            CoverageState::Augment
        } else {
            CoverageState::Absent
        }
    });
    let mut inferred = crate::inference::inferred_blocks(&evidence, &coordinates);
    if let (A2ajSourceKind::Laws, true, Some(map)) =
        (input.source_kind, has_text, input.section_map.as_ref())
    {
        apply_provider_section_evidence(&coordinates, &mut inferred, &evidence.native_claims, map);
    }
    let mut structure = crate::derive::derive_trusted_inferred(evidence, inferred)?;
    if input.source_kind == A2ajSourceKind::Laws {
        structure.cross_references = Some(crate::instrument::cross_reference_graph(&structure)?);
    }
    Ok(structure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentBlock, DocumentKind, DocumentOrigin, DocumentQuery};

    fn document_blocks(input: A2ajInput) -> (DocumentStructure, Vec<DocumentBlock>) {
        let document = a2aj_document_structure(input).unwrap();
        let blocks = DocumentQuery::new().blocks(&document, None).collect();
        (document, blocks)
    }

    #[test]
    fn duplicate_provider_bodies_keep_per_label_matches() {
        let map = vec![
            ("1".to_owned(), "Shared body.".to_owned()),
            ("2".to_owned(), "Shared body.".to_owned()),
            ("3".to_owned(), "Shared body. Too long.".to_owned()),
        ];
        let labels = provider_claims(&ScalarText::new("Shared body."), &map)
            .into_iter()
            .filter_map(|claim| claim.label)
            .collect::<Vec<_>>();
        assert_eq!(labels, ["sec1", "sec2"]);
        assert!(provider_claims(&ScalarText::new("Shared body.\nShared body."), &map).is_empty());
    }

    #[test]
    fn map_rendering_and_provider_evidence_match_a2aj() {
        let mut mapped = A2ajInput::new("fixture", A2ajSourceKind::Laws, "");
        mapped.section_map = Some(
            ["1", "2", "4", "4.1", "4.2", "5", "Schedule 2", "Schedule 1"]
                .into_iter()
                .map(|label| (label.into(), format!("Provision {label}.")))
                .collect(),
        );
        let (_, mapped) = document_blocks(mapped);
        assert_eq!(
            mapped
                .iter()
                .filter(|block| block.kind == DocumentKind::Section)
                .map(|block| block.label.as_str())
                .collect::<Vec<_>>(),
            [
                "sec1",
                "sec2",
                "sec4",
                "sec4.1",
                "sec4.2",
                "sec5",
                "secSchedule 2",
                "secSchedule 1"
            ]
        );

        let text = "1 First full-text provision.\n2 Second full-text provision.\n3 Third full-text provision.";
        let mut promoted = A2ajInput::new("fixture", A2ajSourceKind::Laws, text);
        promoted.section_map = Some(vec![("2".into(), "Second full-text provision.".into())]);
        let (_, promoted) = document_blocks(promoted);
        assert_eq!(
            promoted
                .iter()
                .filter(|block| block.kind == DocumentKind::Section)
                .map(|block| (&*block.label, block.origin))
                .collect::<Vec<_>>(),
            [
                ("sec1", DocumentOrigin::Heuristic),
                ("sec2", DocumentOrigin::Native),
                ("sec3", DocumentOrigin::Heuristic)
            ]
        );
        assert_eq!(
            promoted
                .iter()
                .find(|block| block.label == "sec2")
                .unwrap()
                .start,
            utf16_at(text, text.find("2 Second").unwrap())
        );

        let text = "Preamble.\n99 Provider-only provision.";
        let mut missing = A2ajInput::new("fixture", A2ajSourceKind::Laws, text);
        missing.section_map = Some(vec![("99".into(), "Provider-only provision.".into())]);
        let (_, missing) = document_blocks(missing);
        let added = missing.iter().find(|block| block.label == "sec99").unwrap();
        assert_eq!(added.origin, DocumentOrigin::Native);
        assert_eq!(
            added.start,
            utf16_at(text, text.find("99 Provider-only").unwrap())
        );

        let mut sole = A2ajInput::new("fixture", A2ajSourceKind::Laws, "1 Sole provision.");
        sole.section_map = Some(vec![("1".into(), "Sole provision.".into())]);
        let (_, sole) = document_blocks(sole);
        assert_eq!(
            sole.iter()
                .filter(|block| block.kind == DocumentKind::Section && block.parent_label.is_none())
                .map(|block| (&*block.label, block.start, block.origin))
                .collect::<Vec<_>>(),
            [("sec1", 0, DocumentOrigin::Native)]
        );

        let mut printed = A2ajInput::new("fixture", A2ajSourceKind::Laws, "");
        printed.section_map = Some(vec![(
            "34".into(),
            "34(1) Parent provision.\n(a) Child paragraph.".into(),
        )]);
        let (_, printed) = document_blocks(printed);
        let subsection = printed
            .iter()
            .find(|block| block.label == "sec34(1)")
            .unwrap();
        assert_eq!(subsection.start, 4);
        assert_eq!(
            printed
                .iter()
                .find(|block| block.label == "sec34(1)(a)")
                .unwrap()
                .parent_label
                .as_deref(),
            Some("sec34")
        );

        let text = "1 (1) Parent provision.\n(a) Child.\n\n### Next\n2 Next provision.";
        let mut bounded = A2ajInput::new("fixture", A2ajSourceKind::Laws, text);
        bounded.section_map = Some(vec![(
            "1".into(),
            "(1) Parent provision.\n(a) Child.".into(),
        )]);
        let (bounded, blocks) = document_blocks(bounded);
        let child = blocks
            .iter()
            .find(|block| block.label == "sec1(1)(a)")
            .unwrap();
        assert_eq!(&bounded.query_text()[child.start..child.start + 3], "(a)");
        assert_eq!(
            child.end,
            blocks
                .iter()
                .find(|block| block.label == "sec2")
                .unwrap()
                .start
        );
    }
}
