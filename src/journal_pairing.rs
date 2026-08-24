use crate::last_scalars;
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

#[derive(Clone, Deserialize)]
pub struct JournalPageLabel {
    #[serde(rename = "page_label")]
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
    for part in value.split_whitespace() {
        if !output.is_empty() {
            output.push(' ');
        }
        output.push_str(part);
    }
    output
}

fn normalize_journal_flow(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let (mut soft_hyphen, mut line_feed) = (false, false);
    let mut separator = "";
    let mut input = raw;
    while let Some(mut character) = input.chars().next() {
        input = &input[character.len_utf8()..];
        if character == '\u{ad}' {
            if input.starts_with("\r\n") {
                input = &input[2..];
            } else {
                soft_hyphen = true;
            }
            continue;
        }
        if std::mem::take(&mut soft_hyphen) && character == '\n' {
            continue;
        }
        if matches!(character, '\0'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}') {
            character = ' ';
        }
        if line_feed && matches!(character, ' ' | '\t') {
            continue;
        }
        if line_feed && character == '\n' {
            line_feed = false;
            separator = if output.is_empty() { "" } else { "\n\n" };
            continue;
        }
        let whitespace = std::mem::take(&mut line_feed) || character.is_whitespace();
        if character == '\n' {
            line_feed = true;
            continue;
        }
        if whitespace && !output.is_empty() && separator.is_empty() {
            separator = " ";
        }
        if character.is_whitespace() {
            continue;
        }
        output.push_str(separator);
        output.push(character);
        separator = "";
    }
    output
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
                footnote_mode
                    .then_some(site.page.abs_diff(label.page))
                    .unwrap_or_default(),
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
        notes[index].0.passage = Some(last_scalars(&passage, 1200).to_owned());
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
