#[cfg(feature = "structure-inference")]
use crate::{
    instrument_contents::instrument_contents_outline_indexed,
    instrument_references::{
        find_provision_references, populate_instrument_node_metadata,
        resolve_instrument_references, FindProvisionReferencesOptions,
    },
    javascript_whitespace, node_depths, provision_mark, AuthoritativeTableCell, AuthoritativeTables,
    DetectionProfile, Evidence,
    DocumentStructure, EngineError, NodeKind, ScalarRange, ScalarText, StructureNode,
};
#[cfg(feature = "structure-inference")]
use legal_structure_model::{DefinedTerm, DefinitionOccurrence};
#[cfg(feature = "structure-inference")]
use regex::Regex;
pub use legal_structure_model::{
    InstrumentContentsEntry, InstrumentContentsOutline, InstrumentContentsReading,
    InstrumentContentsRefusal, InstrumentCrossReferenceCounts, InstrumentCrossReferenceEdge,
    InstrumentCrossReferenceGraph, InstrumentCrossReferenceReason, InstrumentCrossReferenceStatus,
};
#[cfg(feature = "structure-inference")]
use serde::Serialize;
#[cfg(feature = "structure-inference")]
use sha2::{Digest, Sha256};
#[cfg(feature = "structure-inference")]
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::OnceLock,
};

/// The words a scan of a text has passed: the one it is in (its text, letters and whether any character
/// is no capital), the one before, and whether that one is a heading's, in capitals ("GENERAL POWERS 49.
/// The ..."); a word citing a provision ("article 116. The") opens none.
#[cfg(feature = "structure-inference")]
#[derive(Default)]
struct PassedWords {
    text: String,
    letters: usize,
    uncapitalized: bool,
    before: String,
    capitals: bool,
}

#[cfg(feature = "structure-inference")]
impl PassedWords {
    fn pass(&mut self, character: char) {
        if !matches!(character, ' ' | '\t') {
            self.text.push(character);
            self.letters += usize::from(character.is_alphabetic());
            self.uncapitalized |= !character.is_uppercase();
            return;
        }
        if self.letters > 0 {
            self.capitals = self.letters >= 2 && !self.uncapitalized;
        }
        if !self.text.is_empty() {
            // The word's buffer is kept for the next word.
            std::mem::swap(&mut self.before, &mut self.text);
            self.text.clear();
        }
        (self.letters, self.uncapitalized) = (0, false);
    }
}

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
            r"^(?:(?:ARTICLE|Article|PART|Part|DIVISION|Division|Section|SECTION|SCHEDULE|Schedule|EXHIBIT|Exhibit|ANNEX|Annex|APPENDIX|Appendix|BOOK|Book|TITLE|Title|CHAPTER|Chapter|PARTIE|Partie|LIVRE|Livre|TITRE|Titre|CHAPITRE|Chapitre)[\s\u{feff}]+[IVXLCDM0-9]|[0-9]{1,3}\.[0-9]{1,3}(?:\.[0-9]{1,3})*[\s\u{feff}]+\S|\([A-Za-z0-9_]{1,3}\)[\s\u{feff}])",
        )
        .expect("valid instrument sentence-join grammar")
    });
    // A code's article opening its line: its number, a point, and the article's first word
    // ("49. The courts ...", "9.1. Every ...", "1457. Toute personne ...", "107. (Repealed).").
    static ARTICLE: OnceLock<Regex> = OnceLock::new();
    let article = ARTICLE.get_or_init(|| {
        Regex::new(r#"^[0-9]{1,4}(?:\.[0-9]{1,3})*\.[\s\u{feff}]+["“«(]?\p{Lu}"#)
            .expect("valid article opening grammar")
    });
    let mut recovered: Option<String> = None;
    let mut previous = None;
    let mut previous_previous = None;
    let mut words = PassedWords::default();
    // The number of the last article a line was opened for: the next number opens one wherever
    // it stands ("… s. 11. 0 36. Subject …", "§ 1. — General provisions 116. Service …").
    let mut last_article = None::<u32>;
    for (byte, character) in text.char_indices() {
        // A contents list's leader dots ("ADOPTION....... 431.1") end no sentence.
        let leader = previous == Some('.') && previous_previous == Some('.');
        let preceded_by_terminator = previous.is_some()
            && !leader
            && (matches!(previous, Some('.' | ';' | ':'))
                || (matches!(
                    previous,
                    Some(')' | ']' | '"' | '\'' | '\u{201d}' | '\u{2019}' | '\u{00bb}')
                ) && matches!(previous_previous, Some('.' | ';' | ':'))));
        let after = byte + character.len_utf8();
        words.pass(character);
        // Both grammars open with a digit or a heading's first character: a word that cannot
        // open either is not tried.
        let next = text.as_bytes().get(after).copied();
        let opens_article = matches!(character, ' ' | '\t')
            && next.is_some_and(|byte| byte.is_ascii_digit())
            && article.is_match(&text[after..])
            && {
            let number = text[after..].split(['.', ' ']).next().and_then(|value| value.parse::<u32>().ok());
            let cites = matches!(words.before.trim_end_matches(['.', ',']).to_lowercase().as_str(),
                "article" | "articles" | "art" | "arts" | "section" | "sections" | "s" | "ss" | "a" | "aa"
                | "subsection" | "paragraph" | "alinéa" | "paragraphe");
            let continues = number.zip(last_article).is_some_and(|(number, last)| number == last || number == last + 1);
            let opens = !cites && (preceded_by_terminator || words.capitals || continues);
            if opens { last_article = number.or(last_article); }
            opens
        };
        if matches!(character, ' ' | '\t')
            && (preceded_by_terminator
                && next.is_some_and(|byte| {
                    byte.is_ascii_digit()
                        || matches!(byte, b'(' | b'A' | b'B' | b'C' | b'D' | b'E' | b'L' | b'P' | b'S' | b'T')
                })
                && head.is_match(&text[after..])
                || opens_article)
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProvisionReferenceShape {
    Numeric,
    SubOnly,
    Roman,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) continuation_of: Option<usize>,
}

#[cfg(feature = "structure-inference")]
#[cfg(feature = "structure-inference")]
pub(crate) fn cross_reference_graph(
    structure: &DocumentStructure,
) -> Result<InstrumentCrossReferenceGraph, EngineError> {
    let text = ScalarText::new(&structure.text);
    let references = find_provision_references(&text, FindProvisionReferencesOptions::default());
    let depths = node_depths(&structure.nodes);
    resolve_instrument_references(&text, structure, references, &depths)
}

/// Each provision's start by the keys a reference names it by: its place (`StructureNode::fragment`, "sec5(2)",
/// "part2"), and an article's, part's or division's number in roman numerals ("part ii"). A key two provisions share
/// names neither.
#[cfg(feature = "structure-inference")]
fn instrument_reference_index<'a>(sections: impl Iterator<Item = &'a StructureNode>) -> HashMap<String, usize> {
    let mut index = HashMap::new();
    let mut duplicates = HashSet::new();
    for section in sections {
        let Some(place) = section.fragment() else { continue };
        let roman = matches!(section.locator_kind.as_deref(), Some("article" | "part" | "division"))
            .then(|| section.label.as_deref()?.parse::<u32>().ok())
            .flatten()
            .map(|value| format!("{} {}", section.locator_kind.as_deref().unwrap(), legal_structure_model::layout::roman_numeral(value)));
        for key in std::iter::once(place.to_ascii_lowercase()).chain(roman) {
            match index.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(section.range.start);
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
    blocks: &[StructureNode],
    endorsed: &[(String, usize, usize)],
) -> usize {
    let index = instrument_reference_index(blocks.iter().filter(|block| block.kind == NodeKind::Section));
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
fn instrument_block_head_span(blocks: &[StructureNode], coordinates: &ScalarText<'_>) -> f64 {
    instrument_head_span_sections(
        blocks.iter().filter(|block| block.kind == NodeKind::Section && block.label.is_some()),
        coordinates.utf16_len(),
    )
}

#[cfg(feature = "structure-inference")]
fn instrument_head_span_sections<'a>(
    sections: impl Iterator<Item = &'a StructureNode>,
    text_length: usize,
) -> f64 {
    // (A section's own, not its subsections'.)
    let mut starts = sections.filter_map(|section| {
        (provision_mark(section.locator_kind.as_deref()) == "sec" && !section.label.as_deref()?.contains('('))
            .then_some(section.range.start)
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

/// The lineation an instrument is read on, the text it reads (none where that is `coordinates`' own, unmasked) and the
/// provisions inference finds on it: its own text, its tables masked, or with `reconstruct_lineation` the recovery of
/// its lines whose provisions the references it cites (`endorsed`) endorse best, a recovery being hypothesis 1 on.
#[cfg(feature = "structure-inference")]
fn select_lineation(
    coordinates: &ScalarText<'_>,
    tables: &AuthoritativeTables<'_>,
    endorsed: &[(String, usize, usize)],
    reconstruct_lineation: bool,
) -> (usize, Option<String>, Vec<StructureNode>) {
    fn view<'b>(coordinates: &'b ScalarText<'_>, candidate: &'b str) -> ScalarText<'b> {
        if candidate.len() == coordinates.value.len() {
            coordinates.with_same_coordinates(candidate)
        } else {
            ScalarText::new(candidate)
        }
    }
    let mut selected_text = (!tables.is_empty()).then(|| tables.masked_text(coordinates.value.to_owned()));
    let selected_view = selected_text.as_deref().map(|text| view(coordinates, text));
    let selected_coordinates = selected_view.as_ref().unwrap_or(coordinates);
    let mut selected_blocks = crate::inference::detect_instrument(selected_coordinates);
    let mut selected = 0;
    let mut best = if endorsed.is_empty() {
        0
    } else {
        instrument_lineation_score_blocks(&selected_blocks, endorsed)
    };
    drop(selected_view);
    if reconstruct_lineation && best < endorsed.len() {
        for (index, hypothesis) in instrument_lineation_recoveries(coordinates.value).enumerate() {
            let candidate_text = tables.masked_text(hypothesis);
            let candidate_view = view(coordinates, &candidate_text);
            let candidate = crate::inference::detect_instrument(&candidate_view);
            if instrument_block_head_span(&candidate, &candidate_view) < 0.05 {
                continue;
            }
            let score = instrument_lineation_score_blocks(&candidate, endorsed);
            drop(candidate_view);
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
    (selected, selected_text, selected_blocks)
}

/// An instrument's text read into its provisions on the lineation its references endorse best (`select_lineation`,
/// recovering its lines), without the contents, definitions and cross-references `analyze_instrument` adds.
#[cfg(feature = "structure-inference")]
pub(crate) fn instrument_reading(text: &str) -> Result<DocumentStructure, EngineError> {
    let coordinates = ScalarText::new(text);
    let references = find_provision_references(&coordinates, FindProvisionReferencesOptions::default());
    let tables = AuthoritativeTables::new(&coordinates, &[])?;
    let (_, selected_text, blocks) = select_lineation(&coordinates, &tables, &endorsed_references(&references), true);
    let text = selected_text.unwrap_or_else(|| text.to_owned());
    let structure = DocumentStructure::new(String::new(), "internal", text, "provider-adapter");
    Ok(crate::derive::derive(Evidence::new(structure, DetectionProfile::Instrument), Some(blocks)))
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
    let coordinates = ScalarText::new(&text);
    let tables = AuthoritativeTables::new(&coordinates, table_cells)?;
    let references = find_provision_references(&coordinates, FindProvisionReferencesOptions::default());
    let (selected, selected_text, selected_blocks) =
        select_lineation(&coordinates, &tables, &endorsed_references(&references), reconstruct_lineation);
    drop(coordinates);
    let selected_original = selected_text.is_none();
    let (input_text, original_text) = match selected_text {
        None => (text, None),
        Some(selected_text) => (selected_text, Some(text)),
    };
    let structure = DocumentStructure::new(document_id, "internal", input_text, "provider-adapter");
    let mut structure = crate::derive::derive(Evidence::new(structure, DetectionProfile::Instrument), Some(selected_blocks));
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
    // Each line's section: the deepest open where it opens (the last of those, among equals).
    let lines = text.lines();
    let mut next_section = 0;
    let mut active_sections = Vec::new();
    let mut owners = Vec::with_capacity(lines.len());
    for line in lines {
        let start = line[2];
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
    let definitions = defined_terms(&text, |paragraph, start, end| DefinitionOccurrence {
        range: ScalarRange {
            start: text.utf16_at_byte(start).unwrap(),
            end: text.utf16_at_byte(end).unwrap(),
        },
        node_id: owners[paragraph].map(str::to_owned),
        paragraph,
    });
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
    // (Its tables' rows and cells stand under them: the levels are written over every node.)
    legal_structure_model::write_levels(&mut structure.nodes).ok();
    if let Some(original) = original_text {
        structure.text = original;
    }
    structure.revision = original_sha256.clone();
    structure.text_sha256 = original_sha256;
    Ok(structure)
}

#[cfg(feature = "structure-inference")]
static PAREN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\(([^()]*)\)").unwrap());
#[cfg(feature = "structure-inference")]
static QUOTED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#""([A-Z][A-Za-z0-9&' -]{0,79})""#).unwrap());
#[cfg(feature = "structure-inference")]
static LIST: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        &[
            r#"^"([A-Z][A-Za-z0-9&'\- ]{0,79})""#,
            crate::JS_WHITESPACE_CLASS,
            r"+(?:means|shall mean|has the meaning|shall have the meaning)",
        ]
        .concat(),
    )
    .unwrap()
});

#[cfg(feature = "structure-inference")]
fn bounded(text: &str, start: usize, end: usize) -> bool {
    let bytes = text.as_bytes();
    let left = bytes.get(start.wrapping_sub(1)).copied().unwrap_or(b' ');
    let right = bytes.get(end).copied().unwrap_or(b' ');
    !left.is_ascii_alphanumeric() && !right.is_ascii_lowercase() && !right.is_ascii_digit()
}

/// Defined terms: a quoted capitalized term in brackets ("the "Agreement"") or opening a list entry (""Agreement"
/// means"), each with where it is defined and used, read a line to a paragraph; `occurrence` makes each of a paragraph's
/// byte span.
#[cfg(feature = "structure-inference")]
fn defined_terms(
    document: &ScalarText<'_>,
    occurrence: impl Fn(usize, usize, usize) -> DefinitionOccurrence,
) -> Vec<DefinedTerm> {
    let text = document.value;
    let lines = document.lines();
    let span = |paragraph: usize| (lines[paragraph][0], lines[paragraph][1]);
    // Each term, the paragraphs defining it, and its definitions.
    let mut terms = Vec::<(String, Vec<usize>, Vec<DefinitionOccurrence>)>::new();
    let mut term_indices = HashMap::<&str, usize>::new();
    for paragraph in 0..lines.len() {
        let (base, end) = span(paragraph);
        let text = &text[base..end];
        if !text.contains('"') {
            continue;
        }
        let mut found = Vec::new();
        for content in PAREN.captures_iter(text).filter_map(|c| c.get(1)) {
            if !(1..=200).contains(&crate::utf16_len(content.as_str())) {
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
            let index = *term_indices.entry(term).or_insert_with(|| {
                terms.push((term.to_owned(), Vec::new(), Vec::new()));
                terms.len() - 1
            });
            terms[index].1.push(paragraph);
            terms[index].2.push(occurrence(paragraph, base + start, base + end));
        }
    }

    let mut uses = vec![Vec::<(usize, usize, usize)>::new(); terms.len()];
    if !terms.is_empty() {
        let matcher = aho_corasick::AhoCorasick::new(terms.iter().flat_map(|(term, _, _)| {
            [
                std::borrow::Cow::Borrowed(term.as_bytes()),
                std::borrow::Cow::Owned(
                    term.strip_suffix('s')
                        .map_or_else(|| format!("{term}s"), str::to_owned)
                        .into_bytes(),
                ),
            ]
        }))
        .unwrap();
        let mut pattern_ends = vec![0; terms.len() * 2];
        for paragraph in 0..lines.len() {
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
                if terms[term_index].1.binary_search(&paragraph).is_err() {
                    uses[term_index].push((paragraph, base + hit.start(), base + hit.end()));
                }
            }
        }
    }

    terms
        .into_iter()
        .zip(uses)
        .map(|((term, _, definitions), mut uses)| {
            uses.sort_unstable();
            DefinedTerm {
                term,
                definitions,
                uses: uses.into_iter().map(|(paragraph, start, end)| occurrence(paragraph, start, end)).collect(),
            }
        })
        .collect()
}
