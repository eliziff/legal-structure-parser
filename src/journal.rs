use crate::{
    CoverageState, DetectionProfile, DocumentInput, DocumentStructure, EngineError, EvidenceKind,
    NativeClaim, Origin, ParagraphBreak, ScalarRange, ScalarText, Scope, EVIDENCE_SCHEMA,
};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::sync::OnceLock;

const ORIGIN: &str = "provider-adapter";

#[derive(Clone)]
pub struct JournalPageLabel {
    pub label: String,
    pub pdf_page: usize,
}

pub struct JournalPairNote {
    pub label: String,
    pub restart_sequence: usize,
    pub note_page_index: usize,
    pub ref_page_index: Option<usize>,
    pub body: String,
    pub truncated: bool,
    pub proposition: Option<String>,
    pub passage: Option<String>,
}

pub struct JournalFootnotePairing {
    pub notes: Vec<JournalPairNote>,
    pub symbol_labels_dropped: usize,
    pub labels_candidates: usize,
    pub labels_selected: usize,
    pub refs_assigned: usize,
    pub ambiguous_sites: usize,
    pub footnote_mode: bool,
    pub crossrefs: usize,
    pub crossrefs_unresolved: usize,
    pub pages: usize,
}

struct JournalLabel {
    value: String,
    page: usize,
    order: usize,
    body: String,
    truncated: bool,
}

#[derive(Clone, Copy)]
struct JournalSite {
    start: usize,
    end: usize,
    page: usize,
    strong: bool,
}

fn collapse_python_whitespace(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut space = false;
    for character in value.chars() {
        if character.is_whitespace() {
            space = !output.is_empty();
        } else {
            if space {
                output.push(' ');
            }
            output.push(character);
            space = false;
        }
    }
    output
}

fn normalize_journal_flow(raw: &str) -> String {
    static PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    let cleaned = raw
        .replace("\u{ad}\r\n", "")
        .replace("\u{ad}\n", "")
        .replace('\u{ad}', "")
        .chars()
        .map(|character| {
            if matches!(character, '\0'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}') {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    PARAGRAPH
        .get_or_init(|| Regex::new(r"\n[ \t]*\n+").unwrap())
        .split(&cleaned)
        .map(collapse_python_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn journal_label(line: &str) -> Option<(&str, usize)> {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let first = *line
        .trim_start_matches(|character: char| character.is_ascii_whitespace())
        .as_bytes()
        .first()?;
    if !(first.is_ascii_digit() || matches!(first, b'*' | b'#' | 0xc2 | 0xe2)) {
        return None;
    }
    let captures = LABEL
        .get_or_init(|| {
            Regex::new(r#"^(?-u:\s)*(?P<label>[0-9]{1,4}|[*†‡§¶#])(?:(?-u:\s)|[.)\],:;-])"#)
                .unwrap()
        })
        .captures(line)?;
    let label = captures.name("label")?;
    Some((label.as_str(), label.end()))
}

fn journal_segments(text: &str, labels: &[String]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut cursor = 0;
    for label in labels
        .iter()
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
    {
        let marker = format!("[page {label}]");
        let mut probe = cursor;
        while let Some(relative) = text[probe..].find(&marker) {
            let at = probe + relative;
            let line_start = text[..at].rfind('\n').map_or(0, |value| value + 1);
            let line_end = at + marker.len();
            let next_break = text[line_end..].find('\n').map(|value| line_end + value);
            let tail_end = next_break.unwrap_or(text.len());
            if text[line_start..at].trim_matches([' ', '\t']).is_empty()
                && text[line_end..tail_end]
                    .trim_matches([' ', '\t', '\r'])
                    .is_empty()
            {
                found.push((line_start, next_break.map_or(text.len(), |value| value + 1)));
                cursor = line_end;
                break;
            }
            probe = line_end;
        }
    }
    if found.is_empty() {
        return vec![(0, text.len())];
    }
    let mut segments = Vec::with_capacity(found.len() + 1);
    if found[0].0 > 0 {
        segments.push((0, found[0].0));
    }
    segments.extend(found.windows(2).map(|pair| (pair[0].1, pair[1].0)));
    segments.push((found.last().unwrap().1, text.len()));
    segments
}

fn segment_journal_notes(
    text: &str,
    segment: (usize, usize),
    page: usize,
) -> (Vec<JournalLabel>, String) {
    let (start, end) = segment;
    let mut label_lines = Vec::new();
    let mut offset = start;
    for line in text[start..end].split_inclusive('\n') {
        let Some((value, mut after)) = journal_label(line) else {
            offset += line.len();
            continue;
        };
        if line[after..].starts_with(['.', ')', ']']) && line[after + 1..].starts_with(['\t', ' '])
        {
            after += 1;
        }
        let body_column = if line[after..].starts_with('\t') {
            Some(after + 1)
        } else if line[after..].starts_with("  ") {
            let content = line[after..].trim_start_matches(' ');
            (!content.trim().is_empty()).then_some(line.len() - content.len())
        } else {
            None
        };
        if let Some(body_column) = body_column {
            label_lines.push((offset, value.to_owned(), body_column));
        }
        offset += line.len();
    }
    let mut notes = Vec::with_capacity(label_lines.len());
    for (position, (offset, value, body_column)) in label_lines.iter().enumerate() {
        let body_end = label_lines.get(position + 1).map_or(end, |value| value.0);
        let body = normalize_journal_flow(&text[offset + body_column..body_end]);
        notes.push(JournalLabel {
            value: value.clone(),
            page,
            order: *offset,
            truncated: body_end == end
                && position + 1 == label_lines.len()
                && !body.is_empty()
                && !body.ends_with(['.', '!', '?', '"', '\'', '”', '’', ')', ']']),
            body,
        });
    }
    let body_end = label_lines.first().map_or(end, |value| value.0);
    (notes, text[start..body_end].to_owned())
}

fn journal_sites(stream: &str, pages: &[(usize, usize)]) -> HashMap<u32, Vec<JournalSite>> {
    let mut sites = HashMap::<u32, Vec<JournalSite>>::new();
    let bytes = stream.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_digit() {
            start += 1;
            continue;
        }
        let end = (start..bytes.len())
            .find(|index| !bytes[*index].is_ascii_digit())
            .unwrap_or(bytes.len());
        let previous = stream[..start].chars().next_back();
        let following = stream[end..].chars().next();
        if end - start <= 3
            && previous.is_some_and(|value| !value.is_whitespace() && !value.is_numeric())
            && following.is_none_or(char::is_whitespace)
        {
            let value = stream[start..end].parse::<u32>().unwrap();
            let page = pages[pages.partition_point(|value| value.0 <= start) - 1].1;
            sites.entry(value).or_default().push(JournalSite {
                start,
                end,
                page,
                strong: previous.is_some_and(|value| ".,;:!?\"'”’)]".contains(value)),
            });
        }
        start = end;
    }
    sites
}

fn journal_backbone(labels: &[JournalLabel], supported: &HashSet<usize>) -> HashSet<usize> {
    let numeric = labels
        .iter()
        .enumerate()
        .filter(|(_, label)| label.value.bytes().all(|value| value.is_ascii_digit()))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut selected = HashSet::new();
    fn select(
        indexes: &[usize],
        numeric_len: usize,
        labels: &[JournalLabel],
        supported: &HashSet<usize>,
        selected: &mut HashSet<usize>,
    ) {
        if indexes.is_empty() {
            return;
        }
        let mut states = HashMap::<usize, (usize, usize, isize, Vec<usize>)>::new();
        let mut best = (0, 0, 0, Vec::new());
        for (position, index) in indexes.iter().copied().enumerate() {
            let value = labels[index].value.parse::<u32>().unwrap();
            let support = usize::from(supported.contains(&index));
            let mut state = (1, support, 0, vec![index]);
            for previous in indexes[..position].iter().copied() {
                let gap = value as isize - labels[previous].value.parse::<isize>().unwrap();
                if !(1..=5).contains(&gap) {
                    continue;
                }
                let prior = &states[&previous];
                let candidate = (
                    prior.0 + 1,
                    prior.1 + support,
                    prior.2 - (gap - 1),
                    prior.3.iter().copied().chain([index]).collect(),
                );
                if (candidate.0, candidate.1, candidate.2) > (state.0, state.1, state.2) {
                    state = candidate;
                }
            }
            if (state.0, state.1, state.2) > (best.0, best.1, best.2) {
                best = state.clone();
            }
            states.insert(index, state);
        }
        let chain = best.3;
        if chain.len() < 2
            && !chain.iter().any(|index| supported.contains(index))
            && !(numeric_len == 1 && labels[chain[0]].value == "1")
        {
            return;
        }
        selected.extend(chain.iter().copied());
        let first = indexes.iter().position(|index| *index == chain[0]).unwrap();
        let last = indexes
            .iter()
            .position(|index| *index == *chain.last().unwrap())
            .unwrap();
        select(&indexes[..first], numeric_len, labels, supported, selected);
        select(
            &indexes[last + 1..],
            numeric_len,
            labels,
            supported,
            selected,
        );
    }
    select(&numeric, numeric.len(), labels, supported, &mut selected);
    selected
}

fn sentence_boundaries(text: &str) -> Vec<(usize, usize)> {
    let mut boundaries = Vec::new();
    for (start, character) in text.char_indices() {
        if !matches!(character, '.' | '!' | '?') {
            continue;
        }
        let mut end = start + 1;
        for closer in text[end..]
            .chars()
            .take_while(|value| "\"'”’)]".contains(*value))
        {
            end += closer.len_utf8();
        }
        if end == text.len()
            || text[end..].starts_with("⟦FN:")
            || text[end..].chars().next().is_some_and(char::is_whitespace)
        {
            boundaries.push((start, end));
        }
    }
    boundaries
}

fn strip_journal_sites(text: &str, base: usize, sites: &[(usize, usize)]) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    for &(start, end) in sites {
        let (start, end) = (start.saturating_sub(base), end.saturating_sub(base));
        if end == 0 || start >= text.len() {
            continue;
        }
        output.push_str(&text[cursor..cursor.max(start)]);
        cursor = cursor.max(end);
    }
    output.push_str(&text[cursor..]);
    collapse_python_whitespace(&output)
}

pub fn pair_journal_footnotes(text: &str, page_labels: &[String]) -> JournalFootnotePairing {
    let segments = journal_segments(text, page_labels);
    let mut labels = Vec::new();
    let mut bodies = Vec::with_capacity(segments.len());
    for (page, segment) in segments.iter().copied().enumerate() {
        let (mut notes, body) = segment_journal_notes(text, segment, page);
        labels.append(&mut notes);
        bodies.push(normalize_journal_flow(&body));
    }
    labels.sort_by_key(|label| label.order);
    let mut stream = String::new();
    let mut stream_pages = Vec::with_capacity(bodies.len());
    for (page, body) in bodies.iter().enumerate() {
        if page > 0 {
            stream.push_str("\n\n");
        }
        stream_pages.push((stream.len(), page));
        stream.push_str(body);
    }
    let sites = journal_sites(&stream, &stream_pages);
    let supported = labels
        .iter()
        .enumerate()
        .filter_map(|(index, label)| {
            let value = label.value.parse::<u32>().ok()?;
            sites
                .get(&value)?
                .iter()
                .any(|site| site.page.abs_diff(label.page) <= 1)
                .then_some(index)
        })
        .collect::<HashSet<_>>();
    let selected = journal_backbone(&labels, &supported);
    let same_page = selected
        .iter()
        .filter(|index| {
            let label = &labels[**index];
            sites
                .get(&label.value.parse().unwrap())
                .is_some_and(|sites| sites.iter().any(|site| site.page == label.page))
        })
        .count();
    let footnote_mode = !selected.is_empty() && same_page * 2 >= selected.len();
    let mut previous_numeric = None;
    let mut restart_sequence = 1;
    let mut assigned_cursor = 0;
    let mut ambiguous_sites = 0;
    let mut notes = Vec::with_capacity(selected.len());
    for (_index, label) in labels
        .iter()
        .enumerate()
        .filter(|(index, _)| selected.contains(index))
    {
        let numeric = label.value.parse::<u32>().unwrap();
        if previous_numeric.is_some_and(|previous| numeric <= previous) {
            restart_sequence += 1;
            assigned_cursor = 0;
        }
        previous_numeric = Some(numeric);
        let mut candidates = sites
            .get(&numeric)
            .into_iter()
            .flatten()
            .filter(|site| {
                site.start >= assigned_cursor && (!footnote_mode || site.page <= label.page + 1)
            })
            .copied()
            .collect::<Vec<_>>();
        candidates.sort_by_key(|site| {
            (
                !site.strong,
                if footnote_mode {
                    site.page.abs_diff(label.page)
                } else {
                    0
                },
                site.start,
            )
        });
        let chosen = candidates.first().copied();
        if chosen.is_some_and(|site| {
            candidates
                .get(1)
                .is_some_and(|next| next.strong == site.strong && next.page != site.page)
        }) {
            ambiguous_sites += 1;
        }
        if let Some(site) = chosen {
            assigned_cursor = site.end;
        }
        notes.push((
            JournalPairNote {
                label: numeric.to_string(),
                restart_sequence,
                note_page_index: label.page,
                ref_page_index: chosen.map(|site| site.page),
                body: label.body.clone(),
                truncated: label.truncated,
                proposition: None,
                passage: None,
            },
            chosen,
        ));
    }
    let mut spans = notes
        .iter()
        .filter_map(|(_, site)| site.map(|site| (site.start, site.end)))
        .collect::<Vec<_>>();
    spans.sort_unstable();
    let boundaries = sentence_boundaries(&stream);
    let mut order = notes
        .iter()
        .enumerate()
        .filter_map(|(index, (_, site))| site.map(|site| (site.start, index)))
        .collect::<Vec<_>>();
    order.sort_unstable();
    let mut previous_end = 0;
    for (_, index) in order {
        let site = notes[index].1.unwrap();
        let prior = boundaries.partition_point(|boundary| boundary.1 <= site.start);
        let prior_end = prior.checked_sub(1).map_or(0, |index| boundaries[index].1);
        let (start, end) = if prior > 0 && stream[prior_end..site.start].trim().is_empty() {
            (
                prior.checked_sub(2).map_or(0, |index| boundaries[index].1),
                prior_end,
            )
        } else {
            let following = boundaries.partition_point(|boundary| boundary.0 < site.start);
            (
                prior_end,
                boundaries
                    .get(following)
                    .map_or(stream.len(), |boundary| boundary.1),
            )
        };
        notes[index].0.proposition = Some(strip_journal_sites(&stream[start..end], start, &spans));
        let passage = if previous_end <= site.start {
            strip_journal_sites(&stream[previous_end..site.start], previous_end, &spans)
        } else {
            String::new()
        };
        let coordinates = ScalarText::new(&passage);
        let tail = coordinates.len().saturating_sub(1200)..coordinates.len();
        notes[index].0.passage = Some(coordinates.slice(tail).unwrap().to_owned());
        previous_end = site.end;
    }
    static CROSSREF: OnceLock<Regex> = OnceLock::new();
    let crossref = CROSSREF.get_or_init(|| Regex::new(r"(?i)\b(?:(?:supra|infra),?\s+(?:foot)?notes?|op\.?\s*cit\.?,?\s+(?:foot)?notes?|see\s+(?:also\s+)?footnote)\s+([0-9]{1,3})\b").unwrap());
    let backbone = notes
        .iter()
        .map(|(note, _)| note.label.parse::<u32>().unwrap())
        .collect::<HashSet<_>>();
    let mut crossrefs = 0;
    let mut crossrefs_unresolved = 0;
    for (note, _) in &notes {
        let candidate = note
            .body
            .bytes()
            .any(|byte| byte >= 0x80 || matches!(byte, b's' | b'S' | b'i' | b'I' | b'o' | b'O'));
        for capture in crossref.captures_iter(if candidate { &note.body } else { "" }) {
            crossrefs += 1;
            crossrefs_unresolved += usize::from(!backbone.contains(&capture[1].parse().unwrap()));
        }
    }
    let symbol_labels_dropped = labels
        .iter()
        .filter(|label| label.value.parse::<u32>().is_err())
        .count();
    JournalFootnotePairing {
        labels_candidates: labels.len(),
        labels_selected: selected.len(),
        refs_assigned: spans.len(),
        notes: notes.into_iter().map(|(note, _)| note).collect(),
        symbol_labels_dropped,
        ambiguous_sites,
        footnote_mode,
        crossrefs,
        crossrefs_unresolved,
        pages: segments.len(),
    }
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

fn public_label(prefix: &str, value: &str) -> String {
    let value = value
        .parse::<u64>()
        .map_or_else(|_| value.to_owned(), |value| value.to_string());
    format!("{prefix}{value}")
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
    let mut aliases = label.iter().cloned().collect::<Vec<_>>();
    let normalized = normalized.trim();
    if !normalized.is_empty() {
        aliases.push(format!("sectitle:{normalized}"));
    }
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
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let label = line.strip_prefix("[page ")?.strip_suffix(']')?.trim();
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

fn claim(kind: EvidenceKind, label: String, range: ScalarRange) -> NativeClaim {
    NativeClaim {
        id: String::new(),
        kind,
        label: Some(label),
        aliases: Vec::new(),
        range,
        origin_id: ORIGIN.to_owned(),
        parent_label: None,
        anchor: None,
    }
}

fn structure(
    article_id: usize,
    url: Option<String>,
    text: String,
    mut claims: Vec<NativeClaim>,
) -> Result<DocumentStructure, EngineError> {
    for (index, claim) in claims.iter_mut().enumerate() {
        claim.id = format!("native-{:06}", index + 1);
    }
    let end = text.chars().count();
    let coverage = crate::whole_document_coverage(end, |_| CoverageState::Complete);
    let text_sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
    crate::derive_native_structure_evidence(DocumentInput {
        schema_version: EVIDENCE_SCHEMA.to_owned(),
        document_id: article_id.to_string(),
        provider: "journal".to_owned(),
        url,
        doc_type: None,
        provider_revision: "journal-adapter-v1".to_owned(),
        profile: DetectionProfile::Journal,
        report_start_page: None,
        require_report_start: false,
        allow_hyphenated_sections: false,
        text,
        text_sha256,
        source_sha256: None,
        offset_unit: "unicode-scalar".to_owned(),
        scope: Scope::complete(),
        origins: vec![Origin {
            id: ORIGIN.to_owned(),
        }],
        native_claims: claims,
        coverage,
        exclusions: Vec::new(),
        paragraph_breaks: Vec::<ParagraphBreak>::new(),
    })
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
    let (mut clean_scalars, mut page_cursor) = (0, 0);
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
            starts.push((label.to_owned(), pdf_page, clean_scalars));
        } else {
            clean.push_str(line);
            clean_scalars += line.chars().count();
        }
    }
    let mut claims = Vec::with_capacity(starts.len());
    for (index, (label, pdf_page, start)) in starts.iter().enumerate() {
        let mut item = claim(
            EvidenceKind::Page,
            public_label("page", label),
            ScalarRange {
                start: *start,
                end: starts.get(index + 1).map_or(clean_scalars, |value| value.2),
            },
        );
        item.anchor = pdf_page.map(|pdf_page| format!("page={pdf_page}"));
        item.aliases.push(label.clone());
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
        offset += page_coordinates.len();
        let pdf_page = page.pdf_page.filter(|value| *value > 0);
        if let Some(pdf_page) = pdf_page {
            let label = page_labels
                .get(&pdf_page)
                .copied()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| pdf_page.to_string());
            let mut item = claim(
                EvidenceKind::Page,
                public_label("page", &label),
                ScalarRange {
                    start: page_start,
                    end: offset,
                },
            );
            item.anchor = Some(format!("page={pdf_page}"));
            item.aliases.push(label);
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
                        .scalar_at_byte(start_byte)
                        .expect("matched journal region starts at a UTF-8 boundary"),
                end: page_start
                    + page_coordinates
                        .scalar_at_byte(cursor)
                        .expect("matched journal region ends at a UTF-8 boundary"),
                pdf_page,
            };
            if region.kind.as_deref() == Some("text") {
                paragraphs += 1;
                claims.push(claim(
                    EvidenceKind::Paragraph,
                    format!("par{paragraphs}"),
                    ScalarRange {
                        start: placed.start,
                        end: placed.end,
                    },
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
        let mut item = claim(
            EvidenceKind::Section,
            title.label.as_ref().map_or_else(
                || format!("secTitle{}", index + 1),
                |label| format!("sec{label}"),
            ),
            ScalarRange {
                start: title.start,
                end: titles.get(index + 1).map_or(offset, |value| value.start),
            },
        );
        item.aliases.clone_from(&title.aliases);
        claims.push(item);
    }
    let mut used_pairs = HashSet::new();
    for (pair, note, region) in notes {
        if !paired_refs.contains(&pair) || !used_pairs.insert(pair) {
            continue;
        }
        let Some(region) = region else { continue };
        let mut item = claim(
            EvidenceKind::Footnote,
            public_label("fn", &note),
            ScalarRange {
                start: region.start,
                end: region.end,
            },
        );
        item.aliases.push(note);
        item.anchor = region.pdf_page.map(|page| format!("page={page}"));
        claims.push(item);
    }
    claims.sort_by_key(|claim| claim.range);
    structure(article_id, url, text, claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentBlock, DocumentKind, DocumentOrigin, DocumentQuery};
    use std::io::Cursor;

    fn project_json(
        article_id: usize,
        url: Option<String>,
        reader: impl BufRead,
        page_labels: &[JournalPageLabel],
    ) -> (DocumentStructure, Vec<DocumentBlock>) {
        let document = journal_document_structure(article_id, url, reader, page_labels).unwrap();
        let blocks = DocumentQuery::new().blocks(&document, None).collect();
        (document, blocks)
    }

    fn project_text(
        article_id: usize,
        url: Option<String>,
        text: String,
        page_labels: &[JournalPageLabel],
    ) -> (DocumentStructure, Vec<DocumentBlock>) {
        let document = journal_text_document_structure(article_id, url, text, page_labels).unwrap();
        let blocks = DocumentQuery::new().blocks(&document, None).collect();
        (document, blocks)
    }

    #[test]
    fn authoritative_pages_preserve_every_native_region() {
        let pages = concat!(
            r#"{"article_id":"1","text":"TITLE\nBody\n7\n1 Note","pdf_page":1,"regions":[{"order":0,"type":"paragraph_title","text":"TITLE"},{"order":1,"type":"text","lines":[{"text":"Body"}]},{"order":2,"type":"number","text":"7"},{"order":3,"type":"footnote","text":"1 Note","lines":[{"codex_text_order":7}]}],"annotations":[{"pair_id":"p","pair_status":"paired","taxonomy_name":"fn_ref"},{"pair_id":"p","pair_status":"paired","taxonomy_name":"fn_label","note_id":"1","start_line_order":7}]}"#,
            "\n",
        );
        let (doc, blocks) = project_json(
            1,
            None,
            Cursor::new(pages),
            &[JournalPageLabel {
                label: "7".into(),
                pdf_page: 1,
            }],
        );
        assert_eq!(doc.query_text(), "TITLE\nBody\n7\n1 Note");
        assert_eq!(
            blocks
                .iter()
                .filter(|block| block.kind == DocumentKind::Paragraph)
                .map(|block| (block.label.as_str(), block.origin))
                .collect::<Vec<_>>(),
            [("par1", DocumentOrigin::Native)]
        );
        for label in ["page7", "secTitle1", "fn1"] {
            assert!(blocks.iter().any(|block| block.label == label));
        }
    }

    #[test]
    fn plain_text_uses_only_page_markers() {
        let text = "[page 9]\nFirst page.\n\n[page x]\nSecond page.";
        let (doc, blocks) = project_text(
            3,
            Some("https://example.test/article".into()),
            text.into(),
            &[
                JournalPageLabel {
                    label: "9".into(),
                    pdf_page: 4,
                },
                JournalPageLabel {
                    label: "x".into(),
                    pdf_page: 5,
                },
            ],
        );
        assert_eq!(doc.url.as_deref(), Some("https://example.test/article"));
        assert_eq!(doc.query_text(), "First page.\n\nSecond page.");
        assert_eq!(
            blocks
                .iter()
                .map(|block| (
                    block.label.as_str(),
                    block.start,
                    block.end,
                    block.anchor.as_deref(),
                    block.origin,
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "page9",
                    0,
                    "First page.\n\n".len(),
                    Some("page=4"),
                    DocumentOrigin::Native,
                ),
                (
                    "pagex",
                    "First page.\n\n".len(),
                    "First page.\n\nSecond page.".len(),
                    Some("page=5"),
                    DocumentOrigin::Native,
                ),
            ]
        );
        assert!(blocks.iter().all(|block| block.kind == DocumentKind::Page));
    }

    #[test]
    fn plain_text_page_offsets_use_clean_text_utf16_coordinates() {
        let (doc, blocks) = project_text(
            4,
            None,
            "[page 1]\r\n\u{1f9ab}e\u{301}\r\n[page 2]\nZ".to_owned(),
            &[
                JournalPageLabel {
                    label: "1".to_owned(),
                    pdf_page: 1,
                },
                JournalPageLabel {
                    label: "2".to_owned(),
                    pdf_page: 2,
                },
            ],
        );
        assert_eq!(doc.query_text(), "\u{1f9ab}e\u{301}\r\nZ");
        assert_eq!(
            blocks
                .iter()
                .map(|block| (block.start, block.end))
                .collect::<Vec<_>>(),
            [(0, 6), (6, 7)]
        );
    }

    #[test]
    fn json_regions_convert_original_page_bytes_to_rendered_utf16() {
        let pages = concat!(
            r#"{"article_id":"5","text":"\ud83e\uddab\nBody","pdf_page":1,"regions":[{"order":0,"type":"text","text":"Body"}]}"#,
            "\n",
        );
        let (_, blocks) = project_json(
            5,
            None,
            Cursor::new(pages),
            &[JournalPageLabel {
                label: "1".to_owned(),
                pdf_page: 1,
            }],
        );
        let paragraph = blocks
            .iter()
            .find(|block| block.kind == DocumentKind::Paragraph)
            .expect("native paragraph");
        assert_eq!((paragraph.start, paragraph.end), (3, 7));
    }
}
