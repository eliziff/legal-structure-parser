//! A judgment's outline read from its text: its headings in their hierarchy, its numbered
//! paragraphs, the lists within them, and the matter before and after its reasons, in document
//! order, so a caller can lay the judgment out.

use crate::inference::{enumerated, heading_between, joined_heading_levels, may_head, run_in_heading, Levels};
use crate::filing::{front_matter_line, FrontMatterLine};
use crate::analysis::citation_spans;
use crate::{
    Derivation, DocumentStructure, EngineError, NodeKind, ProviderTextInput, ProviderTextSourceKind, ScalarRange,
    ScalarText, StructureNode,
};
use crate::judgment::{author_line, date_line, signature_line};
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

/// Where lines between paragraphs stand: before the reasons, among them, or after them.
#[derive(Clone, Copy)]
enum Place {
    Front,
    Between,
    Tail,
}

/// Reads a judgment's text, as A2AJ and other providers give it (a line to a paragraph, its
/// headings on lines of their own or run into the paragraph they open), into its outline. The
/// paragraphs are the structure layer's; the headings are the lines between them its heading
/// grammar takes as headings.
pub fn case_outline(text: &str) -> Result<Vec<CaseOutlineEntry>, EngineError> {
    let mut structure = reading(text)?;
    add_case_nodes(&mut structure);
    Ok(read(&structure))
}

/// A judgment's text read as a provider gives it, before the nodes its outline adds (`add_case_nodes`).
fn reading(text: &str) -> Result<DocumentStructure, EngineError> {
    crate::provider_text::provider_text_reading(ProviderTextInput::new("", ProviderTextSourceKind::Cases, text))
}

/// Where a judgment's outline opens its first paragraph (`case_outline`'s first "paragraph" entry), in UTF-16 code
/// units: where its reading's first paragraph opens, which the headings and lists its outline adds leave in place.
pub fn case_paragraph_start(text: &str) -> Option<usize> {
    let structure = reading(text).ok()?;
    let coordinates = ScalarText::new(&structure.text);
    let chars = structure.text.chars().collect::<Vec<_>>();
    let first = paragraph_spans(&structure, &coordinates, &chars).first()?.0;
    Some(coordinates.utf16(first))
}

/// A line ending a judge's paragraphs by what it says: the next reasons' introduction, the matter after the reasons.
pub fn judgment_end(line: &str) -> bool {
    crate::inference::closing_line(line.trim())
}

/// A line no numbered paragraph holds, whatever stands around it: a formal heading ("IV. Judicial History"), or a line
/// ending the paragraphs (`judgment_end`).
pub fn judgment_division(line: &str) -> bool {
    // (A heading's words may end on a stop of their own: "II. The Sexual Activity with A.C.".)
    let initials = line.split_whitespace().last().is_some_and(|word| word.trim_end_matches('.').contains('.'));
    judgment_end(line) || crate::inference::formal_heading(line.trim()) && (initials || !line.trim().ends_with(['.', ';', ':', ',']))
}

/// A judgment's numbered paragraphs as a provider's text of it reads, every printing an entry: a decision printed
/// again whole after its reasons (its translation set after them) is read the same way, a count that holds four
/// fifths of the first's numbers being that decision again and not a list after its reasons.
pub(crate) fn judgment_paragraphs(text: &str) -> Vec<(usize, Vec<ScalarRange>)> {
    let mut paragraphs = printings(text);
    // A count holds four fifths of the numbers from 1 to its last: a number past the highest that does is no
    // paragraph's (a stray "272" among a few low numbers), and a text none of whose numbers do numbers none.
    let numbers: std::collections::BTreeSet<usize> = paragraphs.iter().map(|(number, _)| *number).collect();
    let reach = numbers.iter().copied().filter(|&number| numbers.range(..=number).count() * 5 >= number * 4).max().unwrap_or(0);
    paragraphs.retain(|(number, _)| *number <= reach);
    // A few numbers starting over and over are lists in unnumbered reasons: the relief sought, an affidavit's, the issues.
    if reach <= 12 && paragraphs.iter().filter(|(number, _)| *number == 1).count() >= 3 {
        paragraphs.clear();
    }
    // (The first printing's numbers: a second printing's would stand the count's end at the text's.)
    let mut seen = std::collections::HashSet::new();
    let starts: Vec<usize> = paragraphs.iter().take_while(|(number, _)| seen.insert(*number)).filter_map(|(_, ranges)| ranges.first()).map(|range| range.start).collect();
    if let (Some(&first), Some(&last), true) = (starts.iter().min(), starts.iter().max(), reach <= LIST_NUMBERS) {
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let part = |range: &[u16]| String::from_utf16_lossy(range);
        if reasons_go_on(&part(utf16.get(..first).unwrap_or_default()), &part(utf16.get(first..last).unwrap_or_default()), &part(utf16.get(last..).unwrap_or_default())) {
            paragraphs.clear();
        }
    }
    paragraphs
}

/// The most numbers a list a decision quotes runs to.
pub const LIST_NUMBERS: usize = 40;

/// Whether a decision's reasons go on around its numbers, unnumbered, as long as those numbers run: `before` is its text
/// up to the first of them, `numbered` from there to the last, `rest` from the last. Those numbers are a list the reasons
/// quote (the agreed facts, an agreement's terms, the grounds of appeal), not their paragraphs. An order's numbered
/// paragraphs stop at its signature, whatever schedule follows. Running words count: a stamp, a name or a back page's
/// title are none, and of what stands before the numbers only what follows the reasons' author (a report's headnote does not).
pub fn reasons_go_on(before: &str, numbered: &str, rest: &str) -> bool {
    let running = |line: &str| line.split_whitespace().filter(|word| word.starts_with(char::is_lowercase)).count();
    let mut above = "";
    // (A date under a finished sentence ends them; one inside a quotation, under its title, does not.)
    // (`rest` opens on the last number's own words: they are the count's, not what follows it.)
    let after: usize = rest.lines().skip(1).take_while(|line| {
        let dated = date_line(line.trim()) && above.trim_end().ends_with(['.', '?', '!', '"', '\u{201d}']);
        above = line;
        // (A sentence going on over its line signs nothing: "... by letter" / "dated August 22, 1994.  Shortly thereafter, ...".)
        !(judgment_end(line) || signature_line(line.trim()) && !line.trim_start().starts_with(char::is_lowercase) || dated)
    }).map(running).sum();
    let opening = before.lines().collect::<Vec<_>>();
    // (Or a judge's name on its own line, what its bracket says none of its words: "HALLETT, J.A.", "BATEMAN, J.A.:", "PUGSLEY, J.A.:   (In Chambers)", "CHIPMAN, J.A.: (Dissenting in Part)".)
    let author = opening.iter().rposition(|line| {
        let name = line.split('(').next().unwrap_or(line).trim();
        author_line(line) || name.split_whitespace().count() <= 4 && signature_line(name.trim_end_matches(':'))
    });
    let earlier: usize = author.map_or(0, |at| opening[at..].iter().map(|line| running(line)).sum());
    let within = numbered.lines().map(running).sum::<usize>();
    within >= 1 && (earlier + after) * 5 >= within * 4
}

/// Every printing of each number `text` reads: a decision printed twice over is read as two.
fn printings(text: &str) -> Vec<(usize, Vec<ScalarRange>)> {
    if let Some(printings) = two_printings(text) {
        return printings;
    }
    let mut paragraphs = one_printing(text);
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let mut from = 0;
    let mut wanted = paragraphs.len();
    while wanted > 0 {
        let end = paragraphs.iter().flat_map(|(_, ranges)| ranges).map(|range| range.end).max().unwrap_or(from);
        // A printing as long as half the text before it and four fifths its count.
        if end <= from || (utf16.len() - end) * 2 < end - from {
            break;
        }
        let again = one_printing(&String::from_utf16_lossy(utf16.get(end..).unwrap_or_default()));
        if again.len() * 5 < wanted * 4 {
            break;
        }
        (from, wanted) = (end, again.len());
        paragraphs.extend(again.into_iter().map(|(number, ranges)| {
            (number, ranges.into_iter().map(|range| ScalarRange { start: range.start + end, end: range.end + end }).collect())
        }));
    }
    paragraphs
}

/// A decision printed twice over, its translation following it: read whole, its count would take a number's later
/// printing and hop from the one to the other, so a later line opening as the text's first number does ("[1]" or "1."),
/// past the first third of the text and not under a line introducing it ("The orders sought were as follows:", a list),
/// parts them where each part then reads a count of four fifths of the other's or more.
fn two_printings(text: &str) -> Option<Vec<(usize, Vec<ScalarRange>)>> {
    const TRIALS: usize = 3;
    let shift = |ranges: Vec<ScalarRange>, by: usize| ranges.into_iter().map(|range| ScalarRange { start: range.start + by, end: range.end + by }).collect::<Vec<_>>();
    let (mut at, mut trials) = (0, 0);
    let third = text.len() / 3;
    let first_number = text.lines().map(str::trim_start).find_map(|line| ["[1]", "1. "].into_iter().find(|number| line.starts_with(number)))?;
    for line in text.split_inclusive('\n') {
        let opening = line.trim_start();
        let introduced = text[..at].trim_end().ends_with(':');
        if at >= third && trials < TRIALS && opening.starts_with(first_number) && !introduced {
            trials += 1;
            let (first, second) = text.split_at(at);
            let before = one_printing(first);
            if before.len() >= 5 {
                let after = one_printing(second);
                if before.len() * 5 >= after.len() * 4 && after.len() * 5 >= before.len() * 4 {
                    let offset = first.encode_utf16().count();
                    let mut printings = before;
                    printings.extend(after.into_iter().map(|(number, ranges)| (number, shift(ranges, offset))));
                    return Some(printings);
                }
            }
        }
        at += line.len();
    }
    None
}

/// Each number of the first count `text` reads (the structure of a case's text), once, with the spans its first
/// printing holds: the structure's paragraph, which ends where its own lines do (`paragraph_tail`).
fn one_printing(text: &str) -> Vec<(usize, Vec<ScalarRange>)> {
    let Ok(mut structure) = reading(text) else {
        return Vec::new();
    };
    add_case_nodes(&mut structure);
    crate::numbered_paragraphs::paragraph_printings(&structure)
        .into_iter()
        .map(|(number, printings)| {
            let node = &structure.nodes[printings[0]];
            (number, if node.ranges.is_empty() { vec![node.range] } else { node.ranges.clone() })
        })
        .collect()
}

/// Each paragraph a judgment's outline reads, in scalars: where its number opens and closes, where its range ends,
/// and its number as printed ("[12]"), in order. A judgment that numbers no paragraphs has its unnumbered ones, each
/// without a label.
fn paragraph_spans(structure: &DocumentStructure, coordinates: &ScalarText<'_>, chars: &[char]) -> Vec<(usize, usize, usize, String)> {
    let scalar = |at: usize| coordinates.scalar_at_utf16(at).expect("a paragraph's range is on scalar boundaries");
    let mut paragraphs = structure
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Paragraph)
        .filter_map(|node| {
            let number = node.label.as_deref()?.strip_prefix("par")?;
            let marker = format!("[{number}]").chars().collect::<Vec<_>>();
            let (paragraph_start, end) = (scalar(node.range.start), scalar(node.range.end));
            let start = (paragraph_start..end.saturating_sub(marker.len()) + 1)
                .find(|at| chars[*at..].starts_with(&marker))
                .unwrap_or(paragraph_start);
            Some((start, (start + marker.len()).min(end), end, format!("[{number}]")))
        })
        .collect::<Vec<_>>();
    if paragraphs.is_empty() {
        paragraphs = structure
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Prose && node.label.is_none())
            .map(|node| {
                let (start, end) = (scalar(node.range.start), scalar(node.range.end));
                (start, start, end, String::new())
            })
            .collect();
    }
    paragraphs.sort_by_key(|paragraph| paragraph.0);
    paragraphs.dedup_by_key(|paragraph| paragraph.0);
    paragraphs
}

/// A judgment's outline from its structure's numbered paragraphs and the lines around them.
fn read(structure: &DocumentStructure) -> Vec<CaseOutlineEntry> {
    let text = structure.text.as_str();
    let coordinates = ScalarText::new(text);
    let chars = text.chars().collect::<Vec<_>>();
    let slice = |start: usize, end: usize| chars[start..end].iter().collect::<String>();
    // Where each line opens, in scalars.
    let line_start = |at: usize| {
        chars[..at]
            .iter()
            .rposition(|c| *c == '\n')
            .map_or(0, |at| at + 1)
    };
    let paragraphs = paragraph_spans(structure, &coordinates, &chars);

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
            start: coordinates.utf16(start),
            end: coordinates.utf16(end),
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
            // The last line, its paragraph's number run in after it, is read as the paragraph reader reads it.
            let joined_levels = (joined && index + 1 == lines.len() && may_head(line))
                .then(|| joined_heading_levels(line).or_else(|| run_in_heading(line).then(|| vec![line.to_owned()])))
                .flatten();
            joined_levels.or_else(|| match lines.get(index + 1) {
                Some((_, _, next)) => heading_between(line, next),
                None => heading_between(line, &slice(end, (end + 200).min(chars.len()))),
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
                matches!(place, Place::Front) && index < opening && Levels::opens(token)
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
            if !chars[at..close].iter().all(|c| c.is_whitespace()) {
                lines.push((at, close));
            }
            at = close + 1;
        }
        // The paragraph ends where its own lines do (`paragraph_tail`): the headings after it, the matter after the
        // reasons, are read with the lines between the paragraphs.
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
        if let Some(ordinal) = node
            .id
            .strip_prefix("heuristic-")
            .and_then(|rest| rest.strip_prefix(node.kind.name()))
            .and_then(|rest| rest.strip_prefix('-'))
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
    // A paragraph ends where a heading after it opens what follows: the heading is no paragraph's. (The whitespace
    // before the heading is walked back over in scalars; the ranges are UTF-16.)
    let coordinates = ScalarText::new(&structure.text);
    let chars = structure.text.chars().collect::<Vec<_>>();
    let heading_starts = added.iter().filter(|node| node.kind == NodeKind::Heading).map(|node| node.range.start).collect::<Vec<_>>();
    for paragraph in structure.nodes.iter_mut().filter(|node| node.kind == NodeKind::Paragraph) {
        let Some(&start) = heading_starts.iter().filter(|&&start| paragraph.range.start < start && start < paragraph.range.end).min() else { continue };
        let (Some(floor), Some(mut end)) = (coordinates.scalar_at_utf16(paragraph.range.start), coordinates.scalar_at_utf16(start)) else { continue };
        while end > floor && chars.get(end - 1).is_some_and(|c| c.is_whitespace()) {
            end -= 1;
        }
        let end = coordinates.utf16(end);
        paragraph.range.end = end;
        for range in &mut paragraph.ranges {
            range.end = range.end.min(end);
        }
        paragraph.ranges.retain(|range| range.start < range.end);
    }
    structure.nodes.extend(added);
}
