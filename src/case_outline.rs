//! A judgment's outline read from its text: its headings in their hierarchy, its numbered
//! paragraphs, the lists within them, and the matter before and after its reasons, in document
//! order, so a caller can lay the judgment out.

use crate::inference::{
    enum_readings, heading_enumerator, heading_levels, joined_heading_levels, sentence_heading,
};
use crate::filing::{front_matter_line, FrontMatterLine};
use crate::analysis::citation_spans;
use crate::{
    provider_text_document_structure, Derivation, DocumentStructure, EngineError, NodeKind,
    ProviderTextInput, ProviderTextSourceKind, ScalarRange, StructureNode,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseOutlineEntry {
    /// "heading", "paragraph" (a numbered paragraph), "item" (an entry of a list within a
    /// paragraph) or "text" (the matter before and after the reasons, and a paragraph's lines
    /// that are not a list's: a quotation, or its words after a list).
    pub kind: &'static str,
    /// A heading's depth among the headings; an item's depth in its list, from 1; a paragraph's
    /// other lines 1; 0 otherwise.
    pub level: usize,
    /// As printed: "[12]", "IV.", "(a)", "•"; empty where there is none.
    pub label: String,
    /// The entry's words, without its label.
    pub text: String,
    /// The entry's span in the input, in UTF-16 code units.
    pub start: usize,
    pub end: usize,
    /// A front-matter line's part: "title" (the provider's line naming the decision by its
    /// citation), "party" (a party's role), "caption" (a short line of the court's heading),
    /// "labelled" ("DOCKET: C69420") or "counsel".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
}

/// What a line before a judgment's reasons is: the opening line that names the decision by its
/// citation ("R. v. Murphy, 2022 ONCA 615"), a party's role, a short line that is no label's and no
/// counsel's, a labelled line, or counsel's.
fn front_role(line: &str, opening: bool) -> Option<&'static str> {
    if opening && citation_spans(line).iter().any(|span| span.range == (0..line.len())) {
        return Some("title");
    }
    match front_matter_line(line) {
        FrontMatterLine::Party => Some("party"),
        FrontMatterLine::Labelled => Some("labelled"),
        FrontMatterLine::Counsel => Some("counsel"),
        FrontMatterLine::Other => {
            (line.chars().count() <= 72 && !line.ends_with(['.', ';', ':'])).then_some("caption")
        }
    }
}

const BULLETS: [&str; 8] = ["•", "-", "–", "—", "*", "·", "◦", "▪"];

/// An enumerator's readings: its family as the instrument grammar reads it (a letter that is
/// also a numeral has both), whether it is parenthesized, and its value.
fn readings(token: &str) -> Option<Vec<((u8, bool), u32)>> {
    if BULLETS.contains(&token) {
        return Some(vec![((u8::MAX, false), 1)]);
    }
    let (inner, wrapped) = match token.strip_prefix('(') {
        Some(rest) => (rest.strip_suffix(')')?, true),
        None => (token.strip_suffix(['.', ')'])?, false),
    };
    let readings = enum_readings(inner)
        .into_iter()
        .flatten()
        .filter_map(|(family, value)| Some(((family, wrapped), value.parse::<u32>().ok()?)))
        .collect::<Vec<_>>();
    (!readings.is_empty()).then_some(readings)
}

/// The open levels of a hierarchy of enumerators, outermost first: each one's family (the
/// instrument grammar's reading of the enumerator, and whether it is parenthesized) and the
/// value last read in it.
#[derive(Default)]
struct Levels(Vec<((u8, bool), u32)>);

impl Levels {
    /// The depth an enumerator opens at, from 0, opening or closing levels to it. Of a letter
    /// that is also a numeral ("i", "v", "C") the reading taken continues an open level, or else
    /// starts one at its first value ("i" is 1, not the ninth letter).
    fn read(&mut self, token: &str) -> Option<usize> {
        let readings = readings(token)?;
        let (family, value) = readings
            .iter()
            .find(|reading| self.continued(reading))
            .or_else(|| readings.iter().find(|(_, value)| *value == 1))
            .or_else(|| {
                readings
                    .iter()
                    .find(|(family, _)| self.0.iter().any(|(open, _)| open == family))
            })
            .or(readings.first())
            .copied()?;
        Some(self.open(family, value))
    }

    /// Whether an enumerator goes on with an open level: "2." after "1.".
    fn continues(&self, token: &str) -> bool {
        readings(token)
            .is_some_and(|readings| readings.iter().any(|reading| self.continued(reading)))
    }

    fn continued(&self, (family, value): &((u8, bool), u32)) -> bool {
        self.0
            .iter()
            .any(|(open, last)| open == family && (family.0 == u8::MAX || last + 1 == *value))
    }

    fn open(&mut self, family: (u8, bool), value: u32) -> usize {
        match self.0.iter().position(|(open, _)| *open == family) {
            Some(at) => {
                self.0.truncate(at + 1);
                self.0[at].1 = value;
                at
            }
            None => {
                self.0.push((family, value));
                self.0.len() - 1
            }
        }
    }
}

/// Where lines between paragraphs stand: before the reasons, among them, or after them.
#[derive(Clone, Copy)]
enum Place {
    Front,
    Between,
    Tail,
}

/// A line's leading enumerator and the words after it, where it opens with one.
fn enumerated(line: &str) -> Option<(&str, &str)> {
    let (token, rest) = line.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    (!rest.is_empty()
        && (token.starts_with('(') && token.ends_with(')') && token.len() <= 7
            || heading_enumerator(token)
            || BULLETS.contains(&token)))
    .then_some((token, rest))
}

/// Whether a line may head what follows it rather than being matter the judgment quotes: a table's
/// figure ("1,000.00") has no words, and a quoted enactment's provision opens its first subsection
/// on its number's line ("399. (1) On motion, the Court may …", "6 (1) Except where …").
fn may_head(line: &str) -> bool {
    let mut words = line.split_whitespace();
    let provision = words.next().is_some_and(|first| first.trim_end_matches('.').bytes().all(|byte| byte.is_ascii_digit() || byte == b'.'))
        && words.next().is_some_and(|second| {
            second.len() >= 3 && second.starts_with('(') && second.ends_with(')')
                && second[1..second.len() - 1].bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    line.chars().any(char::is_alphabetic) && !provision
}

/// What follows a line, read for whether the line heads it: without the number of the paragraph it
/// opens ("[11] The court ...").
fn after_marker(following: &str) -> &str {
    let following = following.trim_start();
    following
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .filter(|(number, _)| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        .map_or(following, |(_, rest)| rest)
}

/// Reads a judgment's text, as A2AJ and other providers give it (a line to a paragraph, its
/// headings on lines of their own or run into the paragraph they open), into its outline. The
/// paragraphs are the structure layer's; the headings are the lines between them its heading
/// grammar takes as headings.
pub fn case_outline(text: &str) -> Result<Vec<CaseOutlineEntry>, EngineError> {
    let structure = provider_text_document_structure(ProviderTextInput::new(
        "",
        ProviderTextSourceKind::Cases,
        text,
    ))?;
    Ok(read(&structure))
}

/// A judgment's outline from its structure's numbered paragraphs and the lines around them.
fn read(structure: &DocumentStructure) -> Vec<CaseOutlineEntry> {
    let text = structure.text.as_str();
    let chars = text.chars().collect::<Vec<_>>();
    let mut utf16 = Vec::with_capacity(chars.len() + 1);
    let mut at = 0;
    for character in &chars {
        utf16.push(at);
        at += character.len_utf16();
    }
    utf16.push(at);
    let slice = |start: usize, end: usize| chars[start..end].iter().collect::<String>();
    // Where each line opens, in scalars.
    let line_start = |at: usize| {
        chars[..at]
            .iter()
            .rposition(|c| *c == '\n')
            .map_or(0, |at| at + 1)
    };
    // Each numbered paragraph: where its number opens and closes, and where its range ends.
    let mut paragraphs = structure
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Paragraph)
        .filter_map(|node| {
            let number = node.label.as_deref()?.strip_prefix("par")?;
            let marker = format!("[{number}]").chars().collect::<Vec<_>>();
            let paragraph_start = utf16
                .binary_search(&node.range.start)
                .expect("paragraph start is a scalar boundary");
            let end = utf16
                .binary_search(&node.range.end)
                .expect("paragraph end is a scalar boundary");
            let start = (paragraph_start..end.saturating_sub(marker.len()) + 1)
                .find(|at| chars[*at..].starts_with(&marker))
                .unwrap_or(paragraph_start);
            Some((
                start,
                (start + marker.len()).min(end),
                end,
                format!("[{number}]"),
            ))
        })
        .collect::<Vec<_>>();
    // A judgment that numbers no paragraphs has its unnumbered ones, each without a label.
    if paragraphs.is_empty() {
        paragraphs = structure
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Prose && node.label.is_none())
            .map(|node| {
                let start = utf16.binary_search(&node.range.start).expect("prose start is a scalar boundary");
                let end = utf16.binary_search(&node.range.end).expect("prose end is a scalar boundary");
                (start, start, end, String::new())
            })
            .collect();
    }
    paragraphs.sort_by_key(|paragraph| paragraph.0);
    paragraphs.dedup_by_key(|paragraph| paragraph.0);

    let mut entries = Vec::new();
    let mut headings = Levels::default();
    let mut last_heading = 0;
    let push = |entries: &mut Vec<CaseOutlineEntry>,
                kind,
                level,
                label: String,
                start: usize,
                end: usize,
                own: &str| {
        entries.push(CaseOutlineEntry {
            kind,
            level,
            label,
            text: own.trim().to_owned(),
            start: utf16[start],
            end: utf16[end],
            role: None,
        });
    };
    // The lines between two paragraphs (or before the first): each a heading where the heading
    // grammar reads it as one, before what follows it; the matter before the reasons ends in the
    // headings that open them.
    let mut between = |entries: &mut Vec<CaseOutlineEntry>,
                       list: &mut Levels,
                       start: usize,
                       end: usize,
                       place: Place| {
        let mut lines = Vec::new();
        let mut at = start;
        while at < end {
            let close = (at..end).find(|at| chars[*at] == '\n').unwrap_or(end);
            let value = slice(at, close);
            if !value.trim().is_empty() {
                let lead = value.len() - value.trim_start().len();
                let trimmed = value.trim();
                let from = at + value[..lead].chars().count();
                lines.push((from, from + trimmed.chars().count(), trimmed.to_owned()));
            }
            at = close + 1;
        }
        // A heading's path, a level at a time; a sentence heading is one level. The last line, where
        // the paragraph after it opens on that line, is a path its number vouches for.
        let joined = end > start && end < chars.len() && chars[end - 1] != '\n';
        let heading = |index: usize, line: &str| {
            if !may_head(line) {
                return None;
            }
            let levels = if joined && index + 1 == lines.len() { joined_heading_levels(line) } else { heading_levels(line) };
            levels.or_else(|| {
                let sentence = match lines.get(index + 1) {
                    Some((_, _, next)) => sentence_heading(line, after_marker(next)),
                    None => sentence_heading(
                        line,
                        after_marker(&slice(end, (end + 200).min(chars.len()))),
                    ),
                };
                sentence.then(|| vec![line.to_owned()])
            })
        };
        // Before the reasons, only the headings that open them count: the enumerated ones just
        // before the first paragraph, and the line above them ("REASONS FOR JUDGMENT"); the
        // matter after the reasons has none.
        let opening = match place {
            Place::Front => {
                let mut opening = lines.len();
                while let Some(index) = opening.checked_sub(1) {
                    let Some(levels) = heading(index, &lines[index].2) else {
                        break;
                    };
                    opening = index;
                    if enumerated(&levels[0]).is_none() {
                        break;
                    }
                }
                opening
            }
            Place::Between => 0,
            Place::Tail => lines.len(),
        };
        for (index, (from, to, line)) in lines.iter().enumerate() {
            // A line going on with the list the paragraph above ends in is that list's; before the
            // reasons (a publication ban quoting its enactment), a line may also open a list.
            let opens = |token: &str| {
                matches!(place, Place::Front)
                    && index < opening
                    && readings(token)
                        .is_some_and(|readings| readings.iter().any(|(_, value)| *value == 1))
            };
            if let Some((token, rest)) =
                enumerated(line).filter(|(token, _)| list.continues(token) || opens(token))
            {
                let depth = list.read(token).unwrap_or_default();
                push(
                    entries,
                    "item",
                    depth + 1,
                    token.to_owned(),
                    *from,
                    *to,
                    rest,
                );
                continue;
            }
            let Some(levels) = (index >= opening).then(|| heading(index, line)).flatten() else {
                let role = (matches!(place, Place::Front) && index < opening)
                    .then(|| front_role(line, entries.is_empty()));
                push(entries, "text", 0, String::new(), *from, *to, line);
                entries.last_mut().unwrap().role = role.flatten();
                continue;
            };
            // Each level takes its depth from its enumerator; one without keeps the depth of the
            // heading before it.
            for level in levels {
                let (label, words) = match enumerated(&level).and_then(|(token, rest)| {
                    headings.read(token).map(|depth| (depth, token, rest))
                }) {
                    Some((depth, token, rest)) => {
                        last_heading = depth;
                        (token.to_owned(), rest.to_owned())
                    }
                    None => (String::new(), level),
                };
                push(entries, "heading", last_heading, label, *from, *to, &words);
            }
        }
    };
    let first = paragraphs
        .first()
        .map_or(chars.len(), |paragraph| paragraph.0);
    // A2AJ opens a decision with its catalogue record (court, date, citation, file numbers) up to
    // a line "Decision Content"; the decision starts after it.
    let record = "Decision Content".chars().collect::<Vec<_>>();
    let opens = (0..first)
        .find(|at| {
            (*at == 0 || chars[at - 1] == '\n')
                && chars[*at..].starts_with(&record)
                && chars
                    .get(at + record.len())
                    .is_none_or(|c| *c == '\n' || *c == '\r')
        })
        .map_or(0, |at| at + record.len() + 1);
    between(
        &mut entries,
        &mut Levels::default(),
        opens.min(first),
        first,
        Place::Front,
    );
    for (index, (start, marker_end, range_end, label)) in paragraphs.iter().enumerate() {
        let next = paragraphs.get(index + 1).map(|paragraph| paragraph.0);
        // A heading run into the next paragraph's line ("IV. Standard of Review [50] ...") is
        // not this paragraph's.
        let body_end = next.map_or(*range_end, |next| {
            let line = line_start(next);
            if line > *start {
                (*range_end).min(line).max(*marker_end)
            } else {
                (*range_end).min(next)
            }
        });
        let mut lines = Vec::new();
        let mut at = *marker_end;
        while at < body_end {
            let close = (at..body_end)
                .find(|at| chars[*at] == '\n')
                .unwrap_or(body_end);
            if !slice(at, close).trim().is_empty() {
                lines.push((at, close));
            }
            at = close + 1;
        }
        // A heading on the lines a paragraph ends with, after its last sentence, opens what
        // follows ("... as follows.\nAdmission of the fresh evidence\n[11] The ..."): it is
        // read with the lines between the paragraphs.
        let mut body_end = body_end;
        while lines.len() > 1 {
            let (from, to) = lines[lines.len() - 1];
            let above = slice(lines[lines.len() - 2].0, lines[lines.len() - 2].1);
            let ended = above
                .trim_end()
                .ends_with(['.', ':', ';', '?', '!', '"', '\u{201d}', ')']);
            let heading = || {
                let line = slice(from, to);
                !line.trim_start().starts_with('(')
                    && (heading_levels(&line).is_some()
                        || sentence_heading(
                            &line,
                            after_marker(&slice(body_end, (body_end + 200).min(chars.len()))),
                        ))
            };
            if !ended || !heading() {
                break;
            }
            body_end = from;
            lines.pop();
        }
        let (first_start, first_end) = lines.first().copied().unwrap_or((*marker_end, *marker_end));
        push(
            &mut entries,
            "paragraph",
            0,
            label.clone(),
            *start,
            first_end,
            &slice(first_start, first_end),
        );
        let mut list = Levels::default();
        for (from, to) in lines.into_iter().skip(1) {
            let value = slice(from, to);
            let trimmed = value.trim();
            if trimmed.is_empty() {
                continue;
            }
            let from = from
                + value[..value.len() - value.trim_start().len()]
                    .chars()
                    .count();
            let to = from + trimmed.chars().count();
            match enumerated(trimmed)
                .and_then(|(token, rest)| list.read(token).map(|level| (token, rest, level)))
            {
                Some((token, rest, level)) => push(
                    &mut entries,
                    "item",
                    level + 1,
                    token.to_owned(),
                    from,
                    to,
                    rest,
                ),
                None => push(&mut entries, "text", 1, String::new(), from, to, trimmed),
            }
        }
        let gap_end = next.unwrap_or(chars.len());
        between(
            &mut entries,
            &mut list,
            body_end,
            gap_end,
            if next.is_some() {
                Place::Between
            } else {
                Place::Tail
            },
        );
    }
    entries
}

/// A judgment's reading beyond its numbered paragraphs, as nodes: its headings, each under the
/// heading it falls within; the lists within a paragraph, each item under its list (a nested item
/// under the item it continues); and the passages a paragraph quotes. A line after a list that
/// carries on the paragraph's sentence in lower case is the paragraph's own words.
pub(crate) fn add_case_nodes(structure: &mut DocumentStructure) {
    let entries = read(structure);
    let paragraphs = structure
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Paragraph)
        .map(|node| (node.range, node.id.clone()))
        .collect::<Vec<_>>();
    let owner = |start: usize| {
        paragraphs
            .iter()
            .find(|(range, _)| range.start <= start && start < range.end)
            .map(|(_, id)| id.clone())
    };
    let mut ordinals = std::collections::HashMap::<NodeKind, usize>::new();
    for node in &structure.nodes {
        let prefix = format!("heuristic-{}-", node.kind.name());
        if let Some(ordinal) = node
            .id
            .strip_prefix(&prefix)
            .and_then(|rest| rest.parse::<usize>().ok())
        {
            let entry = ordinals.entry(node.kind).or_default();
            *entry = (*entry).max(ordinal);
        }
    }
    let mut added = Vec::new();
    let mut node = |kind: NodeKind,
                    start: usize,
                    end: usize,
                    label: Option<String>,
                    parent: Option<String>| {
        let ordinal = ordinals.entry(kind).or_default();
        *ordinal += 1;
        let mut node = StructureNode::new(
            format!("heuristic-{}-{:06}", kind.name(), *ordinal),
            kind,
            ScalarRange { start, end },
            crate::ENGINE_ORIGIN,
            Derivation::Heuristic,
            parent,
        );
        node.label = label;
        let id = node.id.clone();
        added.push(node);
        id
    };
    let mut headings: Vec<String> = Vec::new();
    // The paragraph a list is in, and its open items: the list, then each level's last item.
    let mut list: Option<(String, Vec<String>)> = None;
    let mut quote: Option<(usize, usize, String)> = None;
    let mut previous_item = false;
    for entry in &entries {
        let parent = owner(entry.start);
        // A paragraph's list and quote end where anything else of it starts.
        if entry.kind != "item"
            || parent.as_deref() != list.as_ref().map(|(owner, ..)| owner.as_str())
        {
            list = None;
        }
        let quoted = entry.kind == "text"
            && entry.level == 1
            && parent.is_some()
            && !(previous_item && entry.text.starts_with(char::is_lowercase));
        if !quoted || parent.as_deref() != quote.as_ref().map(|(.., owner)| owner.as_str()) {
            if let Some((start, end, owner)) = quote.take() {
                node(NodeKind::Quote, start, end, None, Some(owner));
            }
        }
        match entry.kind {
            "heading" => {
                headings.truncate(entry.level);
                let parent = headings.last().cloned();
                let title = [entry.label.as_str(), entry.text.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                let id = node(
                    NodeKind::Heading,
                    entry.start,
                    entry.end,
                    Some(title),
                    parent,
                );
                headings.resize(entry.level, id.clone());
                headings.push(id);
            }
            "item" if parent.is_some() => {
                let paragraph = parent.unwrap();
                let (_, items) = list.get_or_insert_with(|| {
                    let id = node(
                        NodeKind::List,
                        entry.start,
                        entry.end,
                        None,
                        Some(paragraph.clone()),
                    );
                    (paragraph.clone(), vec![id])
                });
                let depth = entry.level.max(1);
                items.truncate(depth);
                let under = items.last().cloned();
                let id = node(
                    NodeKind::ListItem,
                    entry.start,
                    entry.end,
                    Some(entry.label.clone()),
                    under,
                );
                items.push(id);
            }
            "text" if quoted => {
                let owner = parent.unwrap();
                match quote.as_mut() {
                    Some((_, end, _)) => *end = entry.end,
                    None => quote = Some((entry.start, entry.end, owner)),
                }
            }
            _ => {}
        }
        previous_item = entry.kind == "item";
    }
    if let Some((start, end, owner)) = quote.take() {
        node(NodeKind::Quote, start, end, None, Some(owner));
    }
    // Each list spans its items.
    let mut spans = std::collections::HashMap::<String, (usize, usize)>::new();
    let parents = added
        .iter()
        .map(|node| (node.id.clone(), node.parent_id.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    for item in added.iter().filter(|node| node.kind == NodeKind::ListItem) {
        let mut at = item.parent_id.clone();
        while let Some(id) = at {
            if id.starts_with("heuristic-list-") {
                let span = spans
                    .entry(id)
                    .or_insert((item.range.start, item.range.end));
                span.0 = span.0.min(item.range.start);
                span.1 = span.1.max(item.range.end);
                break;
            }
            at = parents.get(&id).cloned().flatten();
        }
    }
    for list in added.iter_mut().filter(|node| node.kind == NodeKind::List) {
        if let Some((start, end)) = spans.get(&list.id) {
            list.range = ScalarRange {
                start: *start,
                end: *end,
            };
        }
    }
    structure.nodes.extend(added);
}
