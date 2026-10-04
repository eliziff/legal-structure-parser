//! A judgment's outline read from its text: its headings in their hierarchy, its numbered
//! paragraphs, the lists within them, and the matter before and after its reasons, in document
//! order, so a caller can lay the judgment out.

use crate::inference::{enum_readings, heading_enumerator, heading_levels, sentence_heading};
use crate::{
    provider_text_document_structure, EngineError, NodeKind, ProviderTextInput,
    ProviderTextSourceKind,
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
            .or_else(|| readings.iter().find(|(family, _)| self.0.iter().any(|(open, _)| open == family)))
            .or(readings.first())
            .copied()?;
        Some(self.open(family, value))
    }

    /// Whether an enumerator goes on with an open level: "2." after "1.".
    fn continues(&self, token: &str) -> bool {
        readings(token).is_some_and(|readings| readings.iter().any(|reading| self.continued(reading)))
    }

    fn continued(&self, (family, value): &((u8, bool), u32)) -> bool {
        self.0.iter().any(|(open, last)| open == family && (family.0 == u8::MAX || last + 1 == *value))
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
    (!rest.is_empty() && (token.starts_with('(') && token.ends_with(')') && token.len() <= 7
        || heading_enumerator(token)
        || BULLETS.contains(&token)))
        .then_some((token, rest))
}

/// What follows a line, read for whether the line heads it: without the number of the paragraph it
/// opens ("[11] The court ...").
fn after_marker(following: &str) -> &str {
    let following = following.trim_start();
    following.strip_prefix('[').and_then(|rest| rest.split_once(']'))
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
    let line_start = |at: usize| chars[..at].iter().rposition(|c| *c == '\n').map_or(0, |at| at + 1);
    // Each numbered paragraph: where its number opens and closes, and where its range ends.
    let mut paragraphs = structure
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Paragraph)
        .filter_map(|node| {
            let number = node.label.as_deref()?.strip_prefix("par")?;
            let marker = format!("[{number}]").chars().collect::<Vec<_>>();
            let end = node.range.end.min(chars.len());
            let start = (node.range.start..end.saturating_sub(marker.len()) + 1)
                .find(|at| chars[*at..].starts_with(&marker))
                .unwrap_or(node.range.start);
            Some((start, start + marker.len(), end, format!("[{number}]")))
        })
        .collect::<Vec<_>>();
    paragraphs.sort_by_key(|paragraph| paragraph.0);
    paragraphs.dedup_by_key(|paragraph| paragraph.0);

    let mut entries = Vec::new();
    let mut headings = Levels::default();
    let mut last_heading = 0;
    let push = |entries: &mut Vec<CaseOutlineEntry>, kind, level, label: String, start: usize, end: usize, own: &str| {
        entries.push(CaseOutlineEntry {
            kind,
            level,
            label,
            text: own.trim().to_owned(),
            start: utf16[start],
            end: utf16[end],
        });
    };
    // The lines between two paragraphs (or before the first): each a heading where the heading
    // grammar reads it as one, before what follows it; the matter before the reasons ends in the
    // headings that open them.
    let mut between = |entries: &mut Vec<CaseOutlineEntry>, list: &mut Levels, start: usize, end: usize, place: Place| {
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
        // A heading's path, a level at a time; a sentence heading is one level.
        let heading = |index: usize, line: &str| {
            let following = lines.get(index + 1).map_or_else(
                || slice(end, (end + 200).min(chars.len())),
                |(_, _, next): &(usize, usize, String)| next.clone(),
            );
            heading_levels(line).or_else(|| sentence_heading(line, after_marker(&following)).then(|| vec![line.to_owned()]))
        };
        // Before the reasons, only the headings that open them count: the enumerated ones just
        // before the first paragraph, and the line above them ("REASONS FOR JUDGMENT"); the
        // matter after the reasons has none.
        let opening = match place {
            Place::Front => {
                let mut opening = lines.len();
                while let Some(index) = opening.checked_sub(1) {
                    let Some(levels) = heading(index, &lines[index].2) else { break };
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
            let opens = |token: &str| matches!(place, Place::Front) && index < opening
                && readings(token).is_some_and(|readings| readings.iter().any(|(_, value)| *value == 1));
            if let Some((token, rest)) = enumerated(line).filter(|(token, _)| list.continues(token) || opens(token)) {
                let depth = list.read(token).unwrap_or_default();
                push(entries, "item", depth + 1, token.to_owned(), *from, *to, rest);
                continue;
            }
            let Some(levels) = heading(index, line).filter(|_| index >= opening) else {
                push(entries, "text", 0, String::new(), *from, *to, line);
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
    let first = paragraphs.first().map_or(chars.len(), |paragraph| paragraph.0);
    // A2AJ opens a decision with its catalogue record (court, date, citation, file numbers) up to
    // a line "Decision Content"; the decision starts after it.
    let record = "Decision Content".chars().collect::<Vec<_>>();
    let opens = (0..first).find(|at| {
        (*at == 0 || chars[at - 1] == '\n')
            && chars[*at..].starts_with(&record)
            && chars.get(at + record.len()).is_none_or(|c| *c == '\n' || *c == '\r')
    }).map_or(0, |at| at + record.len() + 1);
    between(&mut entries, &mut Levels::default(), opens.min(first), first, Place::Front);
    for (index, (start, marker_end, range_end, label)) in paragraphs.iter().enumerate() {
        let next = paragraphs.get(index + 1).map(|paragraph| paragraph.0);
        // A heading run into the next paragraph's line ("IV. Standard of Review [50] ...") is
        // not this paragraph's.
        let body_end = next.map_or(*range_end, |next| {
            let line = line_start(next);
            if line > *start { (*range_end).min(line).max(*marker_end) } else { (*range_end).min(next) }
        });
        let mut lines = Vec::new();
        let mut at = *marker_end;
        while at < body_end {
            let close = (at..body_end).find(|at| chars[*at] == '\n').unwrap_or(body_end);
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
            let (line, above) = (slice(from, to), slice(lines[lines.len() - 2].0, lines[lines.len() - 2].1));
            let following = slice(body_end, (body_end + 200).min(chars.len()));
            let ended = above.trim_end().ends_with(['.', ':', ';', '?', '!', '"', '\u{201d}', ')']);
            let heading = !line.trim_start().starts_with('(') && (heading_levels(&line).is_some()
                || sentence_heading(&line, after_marker(&following)));
            if !ended || !heading {
                break;
            }
            body_end = from;
            lines.pop();
        }
        let (first_start, first_end) = lines.first().copied().unwrap_or((*marker_end, *marker_end));
        push(&mut entries, "paragraph", 0, label.clone(), *start, first_end, &slice(first_start, first_end));
        let mut list = Levels::default();
        for (from, to) in lines.into_iter().skip(1) {
            let value = slice(from, to);
            let trimmed = value.trim();
            if trimmed.is_empty() {
                continue;
            }
            let from = from + value[..value.len() - value.trim_start().len()].chars().count();
            let to = from + trimmed.chars().count();
            match enumerated(trimmed).and_then(|(token, rest)| list.read(token).map(|level| (token, rest, level))) {
                Some((token, rest, level)) => push(&mut entries, "item", level + 1, token.to_owned(), from, to, rest),
                None => push(&mut entries, "text", 1, String::new(), from, to, trimmed),
            }
        }
        let gap_end = next.unwrap_or(chars.len());
        between(&mut entries, &mut list, body_end, gap_end, if next.is_some() { Place::Between } else { Place::Tail });
    }
    Ok(entries)
}
