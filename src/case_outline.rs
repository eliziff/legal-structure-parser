//! A judgment's reading past its numbered paragraphs, read from a provider's text of it as nodes: its headings in their
//! hierarchy, the lists within its paragraphs, the passages they quote, and the lines of the matter before its reasons.

use crate::inference::{enumerated, heading_between, joined_heading_levels, may_head, run_in_heading, Levels};
use crate::filing::{front_matter_line, FrontMatterLine};
use crate::analysis::CitedText;
use crate::{
    Derivation, DocumentStructure, NodeKind, PrimaryRole, ScalarRange, StructureNode, utf16_len,
};
use crate::judgment::{author_line, date_line, signature_line};
use std::collections::HashMap;

/// What a line before a judgment's reasons is: the opening line that names the decision by its citation ("R. v.
/// Murphy, 2022 ONCA 615"), a party's role, a short line that is no label's and no counsel's (the caption's), a
/// labelled line, or counsel's.
fn front_role(line: &str, opening: bool, text: &CitedText<'_>) -> Option<PrimaryRole> {
    if opening && citation_title(line, text) {
        return Some(PrimaryRole::Title);
    }
    match front_matter_line(line) {
        FrontMatterLine::Party => Some(PrimaryRole::Party),
        FrontMatterLine::Labelled => Some(PrimaryRole::Labelled),
        FrontMatterLine::Counsel => Some(PrimaryRole::Counsel),
        FrontMatterLine::Other => {
            (line.chars().count() <= 72 && !line.ends_with(['.', ';', ':'])).then_some(PrimaryRole::Caption)
        }
    }
}

/// Whether a line of `text` is a citation and nothing else: the provider's line naming the document by it.
pub(crate) fn citation_title(line: &str, text: &CitedText<'_>) -> bool {
    // (A citation holds a figure: a line with none, a statute's preamble or title in words, is not read.)
    line.bytes().any(|byte| byte.is_ascii_digit()) && text.spans(line).iter().any(|span| span.range == (0..line.len()))
}

/// A line of the front matter as a node: a Span the `front` grammar names, its role the line's.
pub(crate) fn front_line(id: String, role: PrimaryRole, start: usize, end: usize) -> StructureNode {
    let mut line = StructureNode::new(id, NodeKind::Span, ScalarRange { start, end }, crate::ENGINE_ORIGIN, Derivation::Heuristic, None);
    line.grammar = Some("front".to_owned());
    line.role = Some(role);
    line.level = Some(1);
    line
}

/// Where lines between paragraphs stand: before the reasons, among them, or after them.
#[derive(Clone, Copy)]
enum Place {
    Front,
    Between,
    Tail,
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
    judgment_end(line) || crate::inference::formal_heading(line.trim(), &CitedText::new(line)) && (initials || !line.trim().ends_with(['.', ';', ':', ',']))
}

/// A judgment's numbered paragraphs as a provider's text of it reads, every printing an entry: a decision printed
/// again whole after its reasons (its translation set after them) is read the same way, a count that holds four
/// fifths of the first's numbers being that decision again and not a list after its reasons.
pub fn judgment_paragraphs(text: &str) -> Vec<(usize, Vec<ScalarRange>)> {
    let mut paragraphs = printings(&CitedText::new(text));
    // A count holds four fifths of the numbers from 1 to its last: a number past the highest that does is no
    // paragraph's (a stray "272" among a few low numbers), and a text none of whose numbers do numbers none.
    let numbers: std::collections::BTreeSet<usize> = paragraphs.iter().map(|(number, _)| *number).collect();
    let counted = numbers.iter().copied().filter(|&number| numbers.range(..=number).count() * 5 >= number * 4).max();
    // A text none of whose numbers count from 1 may be an excerpt opening mid-count (`excerpt_run`): its run is its count.
    // (A run of bare or stopped numbers an unnumbered decision quotes, "deposes:" / "7. THAT …", is no count.)
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let part = |range: &[u16]| String::from_utf16_lossy(range);
    let excerpt = || {
        let starts = paragraphs.iter().filter_map(|(_, ranges)| ranges.first()).map(|range| range.start.min(utf16.len()));
        let blank = |unit: &&u16| char::from_u32(u32::from(**unit)).is_some_and(char::is_whitespace);
        let bracketed = starts.clone().all(|start| utf16[start..].iter().find(|unit| !blank(unit)) == Some(&u16::from(b'[')));
        let introduced = starts.min().is_some_and(|start| utf16[..start].iter().rev().find(|unit| !blank(unit)) == Some(&u16::from(b':')));
        (bracketed && crate::inference::excerpt_run(numbers.iter().copied(), introduced)).then(|| *numbers.last().expect("two numbers"))
    };
    let reach = counted.or_else(excerpt).unwrap_or(0);
    paragraphs.retain(|(number, _)| *number <= reach);
    // A few numbers starting over and over are lists in unnumbered reasons: the relief sought, an affidavit's, the issues.
    if reach <= 12 && paragraphs.iter().filter(|(number, _)| *number == 1).count() >= 3 {
        paragraphs.clear();
    }
    // (The first printing's numbers: a second printing's would stand the count's end at the text's.)
    let mut seen = std::collections::HashSet::new();
    let starts: Vec<usize> = paragraphs.iter().take_while(|(number, _)| seen.insert(*number)).filter_map(|(_, ranges)| ranges.first()).map(|range| range.start).collect();
    if let (Some(&first), Some(&last), true) = (starts.iter().min(), starts.iter().max(), reach <= LIST_NUMBERS) {
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
fn printings(text: &CitedText<'_>) -> Vec<(usize, Vec<ScalarRange>)> {
    if let Some(printings) = two_printings(text) {
        return printings;
    }
    let mut paragraphs = one_printing(text);
    let utf16: Vec<u16> = text.value.encode_utf16().collect();
    let mut from = 0;
    let mut wanted = paragraphs.len();
    while wanted > 0 {
        let end = paragraphs.iter().flat_map(|(_, ranges)| ranges).map(|range| range.end).max().unwrap_or(from);
        // A printing as long as half the text before it and four fifths its count.
        if end <= from || (utf16.len() - end) * 2 < end - from {
            break;
        }
        let again = one_printing(&text.part(&text.value[text.byte_of_utf16(end)..]));
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
fn two_printings(text: &CitedText<'_>) -> Option<Vec<(usize, Vec<ScalarRange>)>> {
    let (cited, text) = (text, text.value);
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
            let before = one_printing(&cited.part(first));
            if before.len() >= 5 {
                let after = one_printing(&cited.part(second));
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
fn one_printing(text: &CitedText<'_>) -> Vec<(usize, Vec<ScalarRange>)> {
    // (A printing is read as a text of its own, not as a view of the whole one's structure: read whole, the count
    // would take a number's later printing and hop from the one to the other.) Its paragraphs are what the case reader
    // reads in a provider's text (the windows-1252 punctuation it carries read as such), each ending where a heading
    // read with its lines opens (`end_at_heading`); a number's first printing is the one at the earliest place. (A
    // provider's text has no pages, so no later printing on the first's pages counts as it.)
    let read = crate::provider_text::windows_1252_punctuation(text.value);
    let text = &text.part(&read);
    let units = text.value.encode_utf16().collect::<Vec<_>>();
    let mut paragraphs = crate::inference::case_paragraphs(text)
        .into_iter()
        .filter(|node| node.kind == NodeKind::Paragraph && node.range.start < node.range.end)
        .collect::<Vec<_>>();
    let starts = heading_starts(text, &units, &paragraphs);
    let mut first = std::collections::BTreeMap::<usize, (usize, Vec<ScalarRange>)>::new();
    for paragraph in &mut paragraphs {
        end_at_heading(paragraph, &starts, &units);
        let Some(number) = paragraph.label.as_deref().and_then(|label| label.parse::<usize>().ok()) else { continue };
        let ranges = if paragraph.ranges.is_empty() { vec![paragraph.range] } else { paragraph.ranges.clone() };
        let at = ranges.iter().map(|range| range.start).min().unwrap_or(paragraph.range.start);
        if first.get(&number).is_none_or(|(known, _)| at < *known) {
            first.insert(number, (at, ranges));
        }
    }
    first.into_iter().map(|(number, (_, ranges))| (number, ranges)).collect()
}

/// Each paragraph a judgment's reading holds: where its number opens and closes and where its range ends, in order. A
/// judgment that numbers no paragraphs has its unnumbered ones.
fn paragraph_spans(nodes: &[StructureNode], units: &[u16]) -> Vec<(usize, usize, usize)> {
    let mut paragraphs = nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Paragraph)
        .filter_map(|node| {
            let number = node.label.as_deref()?;
            let marker = format!("[{number}]").encode_utf16().collect::<Vec<_>>();
            let (paragraph_start, end) = (node.range.start, node.range.end);
            let start = (paragraph_start..end.saturating_sub(marker.len()) + 1)
                .find(|at| units[*at..].starts_with(&marker))
                .unwrap_or(paragraph_start);
            Some((start, (start + marker.len()).min(end), end))
        })
        .collect::<Vec<_>>();
    if paragraphs.is_empty() {
        paragraphs = nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Prose && node.label.is_none())
            .map(|node| (node.range.start, node.range.start, node.range.end))
            .collect();
    }
    paragraphs.sort_by_key(|paragraph| paragraph.0);
    paragraphs.dedup_by_key(|paragraph| paragraph.0);
    paragraphs
}

/// The nodes a judgment's reading adds, written as its lines are read: each heading under the heading it falls within,
/// each list's item under its list (a nested item under the item it continues), the passages a paragraph quotes, and
/// the front matter's lines.
struct Writer {
    /// The paragraphs (their span, id and level) the lines fall in.
    paragraphs: Vec<(ScalarRange, String, usize)>,
    nodes: Vec<StructureNode>,
    levels: HashMap<String, usize>,
    /// The open headings, outermost first: each one's depth and id.
    headings: Vec<(usize, String)>,
    /// The open list: the paragraph it is in, its node, and each level's last item.
    list: Option<(Option<String>, usize, Vec<String>)>,
    /// The open quotation: its span and the paragraph quoting it.
    quote: Option<(usize, usize, String)>,
    previous_item: bool,
}

/// A line as the reading takes it, for what it ends of the open list and quotation.
enum Read<'a> {
    Heading,
    Item,
    /// A line no heading or item reads: `inner` where it is a paragraph's own (after its first), `words` its text.
    Text { inner: bool, words: &'a str },
}

impl Writer {
    fn owner(&self, start: usize) -> Option<String> {
        self.paragraphs.iter().find(|(range, ..)| range.start <= start && start < range.end).map(|(_, id, _)| id.clone())
    }

    fn add(&mut self, kind: NodeKind, start: usize, end: usize, parent: Option<String>) -> usize {
        let id = format!("case-{}-{:06}", kind.name(), self.nodes.len() + 1);
        let level = parent.as_ref().map_or(1, |parent| self.levels[parent] + 1);
        let mut node = StructureNode::new(id.clone(), kind, ScalarRange { start, end }, crate::ENGINE_ORIGIN, Derivation::Heuristic, parent);
        node.level = Some(level);
        self.levels.insert(id, level);
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn close_quote(&mut self) {
        if let Some((start, end, owner)) = self.quote.take() {
            self.add(NodeKind::Quote, start, end, Some(owner));
        }
    }

    /// A line starting at `start`, as read: a paragraph's list ends where anything else of it starts, its quotation where
    /// a line no quotation's does. A line after a list that carries on the paragraph's sentence in lower case is the
    /// paragraph's own words.
    fn read(&mut self, start: usize, end: usize, read: Read<'_>) {
        let owner = self.owner(start);
        if !matches!(read, Read::Item) || self.list.as_ref().is_some_and(|(list_owner, ..)| *list_owner != owner) {
            self.list = None;
        }
        let quoted = matches!(read, Read::Text { inner: true, words } if owner.is_some()
            && !(self.previous_item && words.starts_with(char::is_lowercase)));
        if !quoted || self.quote.as_ref().map(|(.., quoting)| quoting) != owner.as_ref() {
            self.close_quote();
        }
        if let (true, Some(owner)) = (quoted, owner) {
            match self.quote.as_mut() {
                Some((_, quote_end, _)) => *quote_end = end,
                None => self.quote = Some((start, end, owner)),
            }
        }
        self.previous_item = matches!(read, Read::Item);
    }

    /// A heading at `depth` among the headings, `title` as printed, its enumerator printed at `marker`.
    fn heading(&mut self, depth: usize, title: String, marker: Option<ScalarRange>, start: usize, end: usize) {
        self.read(start, end, Read::Heading);
        while self.headings.last().is_some_and(|(open, _)| *open >= depth) {
            self.headings.pop();
        }
        let parent = self.headings.last().map(|(_, id)| id.clone());
        let at = self.add(NodeKind::Heading, start, end, parent);
        let heading = &mut self.nodes[at];
        heading.label = Some(title);
        heading.marker_range = marker;
        self.headings.push((depth, heading.id.clone()));
    }

    /// An item at `depth` in its list (from 1), its enumerator printed at `marker`.
    fn item(&mut self, depth: usize, token: &str, marker: ScalarRange, end: usize) {
        let start = marker.start;
        self.read(start, end, Read::Item);
        let owner = self.owner(start);
        let list = match self.list.as_ref() {
            Some((_, list, _)) => *list,
            None => {
                let list = self.add(NodeKind::List, start, end, owner.clone());
                self.list = Some((owner, list, vec![self.nodes[list].id.clone()]));
                list
            }
        };
        let items = &mut self.list.as_mut().unwrap().2;
        items.truncate(depth.max(1));
        let under = items.last().cloned();
        let at = self.add(NodeKind::ListItem, start, end, under);
        self.nodes[at].label = Some(token.to_owned());
        self.nodes[at].marker_range = Some(marker);
        let id = self.nodes[at].id.clone();
        self.list.as_mut().unwrap().2.push(id);
        let range = &mut self.nodes[list].range;
        (range.start, range.end) = (range.start.min(start), range.end.max(end));
    }

    fn front(&mut self, role: PrimaryRole, start: usize, end: usize, words: &str) {
        self.read(start, end, Read::Text { inner: false, words });
        let id = format!("case-span-{:06}", self.nodes.len() + 1);
        self.nodes.push(front_line(id, role, start, end));
    }

    /// The catalogue record before the decision, a line of furniture.
    fn record(&mut self, end: usize) {
        self.nodes.push(front_line("case-span-record".to_owned(), PrimaryRole::Furniture, 0, end));
    }
}

/// A judgment's lines read around its paragraphs (`paragraph_spans`), as A2AJ and other providers give a judgment's text
/// (a line to a paragraph, its headings on lines of their own or run into the paragraph they open), each told to `lines`
/// as it is read: the headings are the lines between the paragraphs the heading grammar takes as headings, the items the
/// lines a list's enumerator opens, the front matter's lines its roles.
fn read_case_lines(text: &CitedText<'_>, units: &[u16], paragraphs: &[(usize, usize, usize)], lines_read: &mut Writer) {
    let newline = u16::from(b'\n');
    let slice = |start: usize, end: usize| String::from_utf16_lossy(&units[start..end]);
    let line_start = |at: usize| units[..at].iter().rposition(|unit| *unit == newline).map_or(0, |at| at + 1);
    let mut headings = Levels::default();
    let mut last_heading = 0;
    let mut front_lines = 0;
    // The lines between two paragraphs (or before the first): each a heading where the heading grammar reads it as one,
    // before what follows it; the matter before the reasons ends in the headings that open them.
    let mut between = |writer: &mut Writer, list: &mut Levels, start: usize, end: usize, place: Place| {
        let mut lines = Vec::new();
        let mut at = start;
        while at < end {
            let close = (at..end).find(|at| units[*at] == newline).unwrap_or(end);
            let value = &text.value[text.byte_of_utf16(at)..text.byte_of_utf16(close)];
            if !value.trim().is_empty() {
                let lead = value.len() - value.trim_start().len();
                let trimmed = value.trim();
                let from = at + utf16_len(&value[..lead]);
                lines.push((from, from + utf16_len(trimmed), trimmed));
            }
            at = close + 1;
        }
        // A heading's path, a level at a time; a sentence heading is one level. The last line, where the paragraph
        // after it opens on that line, is a path its number vouches for.
        let joined = end > start && end < units.len() && units[end - 1] != newline;
        let heading = |index: usize, line: &str| {
            // The last line, its paragraph's number run in after it, is read as the paragraph reader reads it.
            let joined_levels = (joined && index + 1 == lines.len() && may_head(line))
                .then(|| joined_heading_levels(line).or_else(|| run_in_heading(line).then(|| vec![line.to_owned()])))
                .flatten();
            joined_levels.or_else(|| match lines.get(index + 1) {
                Some((_, _, next)) => heading_between(line, next, text),
                None => heading_between(line, &slice(end, (end + 200).min(units.len())), text),
            })
        };
        // Before the reasons, only the headings that open them count: the enumerated ones just before the first
        // paragraph, and the line above them ("REASONS FOR JUDGMENT"); the matter after the reasons has none.
        let opening = match place {
            Place::Front => {
                let mut opening = lines.len();
                while let Some(index) = opening.checked_sub(1) {
                    let Some(levels) = heading(index, lines[index].2) else {
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
        // Between paragraphs, five or more unnumbered heading-shaped lines in a row are a quoted form's or a list's, not
        // headings ("THE GOVERNMENT OF" / "THE PROVINCE OF BRITISH COLUMBIA" / "WARRANT OF COMMITTAL ...").
        let read = (0..lines.len()).map(|index| matches!(place, Place::Between).then(|| heading(index, lines[index].2)).flatten()).collect::<Vec<_>>();
        let plain = |read: &&Option<Vec<String>>| read.as_ref().is_some_and(|levels| enumerated(&levels[0]).is_none());
        let stacked = |index: usize| plain(&&read[index]) && read[index..].iter().take_while(plain).count() + read[..index].iter().rev().take_while(plain).count() >= 5;
        for (index, (from, to, line)) in lines.iter().enumerate() {
            // A line going on with the list the paragraph above ends in is that list's; before the reasons (a
            // publication ban quoting its enactment), a line may also open a list.
            let front = matches!(place, Place::Front) && index < opening;
            let opening_line = front && front_lines == 0;
            front_lines += usize::from(front);
            if let Some((token, _)) = enumerated(line).filter(|(token, _)| list.continues(token) || front && Levels::opens(token)) {
                let depth = list.read(token).unwrap_or_default();
                writer.item(depth + 1, token, ScalarRange { start: *from, end: *from + utf16_len(token) }, *to);
                continue;
            }
            let Some(levels) = (index >= opening && !stacked(index))
                .then(|| if matches!(place, Place::Between) { read[index].clone() } else { heading(index, line) }).flatten() else {
                match front.then(|| front_role(line, opening_line, text)).flatten() {
                    Some(role) => writer.front(role, *from, *to, line),
                    None => writer.read(*from, *to, Read::Text { inner: false, words: line }),
                }
                continue;
            };
            // Each level takes its depth from its enumerator; one without keeps the depth of the heading before it.
            let mut cursor = 0;
            for level in levels {
                let found = line[cursor..].find(level.as_str()).map(|at| cursor + at);
                let read = enumerated(&level).and_then(|(token, _)| headings.read(token).map(|depth| (depth, token)));
                let marker = read.and_then(|(depth, token)| {
                    last_heading = depth;
                    found.map(|at| {
                        let start = *from + utf16_len(&line[..at]);
                        ScalarRange { start, end: start + utf16_len(token) }
                    })
                });
                if let Some(at) = found {
                    cursor = at + level.len();
                }
                writer.heading(last_heading, level, marker, *from, *to);
            }
        }
    };
    let first = paragraphs.first().map_or(units.len(), |paragraph| paragraph.0);
    // A2AJ opens a decision with its catalogue record (court, date, citation, file numbers) up to a line "Decision
    // Content"; the decision starts after it.
    let record = "Decision Content".encode_utf16().collect::<Vec<_>>();
    let opens = (0..first)
        .find(|at| {
            (*at == 0 || units[at - 1] == newline)
                && units[*at..].starts_with(&record)
                && units.get(at + record.len()).is_none_or(|unit| *unit == newline || *unit == u16::from(b'\r'))
        })
        .map_or(0, |at| at + record.len() + 1);
    if opens > 0 {
        lines_read.record(opens.min(first));
    }
    between(lines_read, &mut Levels::default(), opens.min(first), first, Place::Front);
    for (index, (start, marker_end, range_end)) in paragraphs.iter().enumerate() {
        let next = paragraphs.get(index + 1).map(|paragraph| paragraph.0);
        // A heading run into the next paragraph's line ("IV. Standard of Review [50] ...") is not this paragraph's.
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
            let close = (at..body_end).find(|at| units[*at] == newline).unwrap_or(body_end);
            if !char::decode_utf16(units[at..close].iter().copied()).all(|c| c.is_ok_and(char::is_whitespace)) {
                lines.push((at, close));
            }
            at = close + 1;
        }
        // The paragraph's first line is its own; the headings after it, the matter after the reasons, are read with
        // the lines between the paragraphs (`paragraph_tail`).
        let (first_start, first_end) = lines.first().copied().unwrap_or((*marker_end, *marker_end));
        lines_read.read(first_start, first_end, Read::Text { inner: false, words: "" });
        let mut list = Levels::default();
        for (from, to) in lines.into_iter().skip(1) {
            let value = slice(from, to);
            let trimmed = value.trim();
            if trimmed.is_empty() {
                continue;
            }
            let from = from + utf16_len(&value[..value.len() - value.trim_start().len()]);
            let to = from + utf16_len(trimmed);
            match enumerated(trimmed).and_then(|(token, _)| list.read(token).map(|level| (token, level))) {
                Some((token, level)) => lines_read.item(level + 1, token, ScalarRange { start: from, end: from + utf16_len(token) }, to),
                None => lines_read.read(from, to, Read::Text { inner: true, words: trimmed }),
            }
        }
        let gap_end = next.unwrap_or(units.len());
        between(lines_read, &mut list, body_end, gap_end, if next.is_some() { Place::Between } else { Place::Tail });
    }
}

/// A paragraph cut where a heading after its start opens within it, the whitespace before the heading walked back over:
/// the heading is no paragraph's. The one rule for where a judgment's paragraph ends.
fn end_at_heading(paragraph: &mut StructureNode, heading_starts: &[usize], units: &[u16]) {
    let Some(&start) = heading_starts.iter().filter(|&&start| paragraph.range.start < start && start < paragraph.range.end).min() else { return };
    let (floor, mut end) = (paragraph.range.start, start);
    while end > floor && units.get(end - 1).and_then(|unit| char::from_u32(u32::from(*unit))).is_some_and(char::is_whitespace) {
        end -= 1;
    }
    paragraph.range.end = end;
    for range in &mut paragraph.ranges {
        range.end = range.end.min(end);
    }
    paragraph.ranges.retain(|range| range.start < range.end);
}

/// A judgment's reading beyond its numbered paragraphs (`nodes`), as nodes (`Writer`) read from its lines
/// (`read_case_lines`): its headings in their hierarchy, the lists within its paragraphs, the passages they quote and
/// the front matter's lines.
fn case_nodes(text: &CitedText<'_>, units: &[u16], nodes: &[StructureNode]) -> Vec<StructureNode> {
    let paragraphs = nodes.iter().filter(|node| node.kind == NodeKind::Paragraph)
        .map(|node| (node.range, node.id.clone(), node.level.unwrap_or(1))).collect::<Vec<_>>();
    let mut writer = Writer {
        levels: paragraphs.iter().map(|(_, id, level)| (id.clone(), *level)).collect(),
        paragraphs,
        nodes: Vec::new(),
        headings: Vec::new(),
        list: None,
        quote: None,
        previous_item: false,
    };
    read_case_lines(text, units, &paragraph_spans(nodes, units), &mut writer);
    writer.close_quote();
    writer.nodes
}

/// Where the headings of a judgment's lines start, read around its paragraphs: where its paragraphs end.
fn heading_starts(text: &CitedText<'_>, units: &[u16], nodes: &[StructureNode]) -> Vec<usize> {
    case_nodes(text, units, nodes).iter().filter(|node| node.kind == NodeKind::Heading).map(|node| node.range.start).collect()
}

/// A judgment's structure with its reading beyond its numbered paragraphs (`case_nodes`), each paragraph ending where
/// a heading after it opens (`end_at_heading`).
pub(crate) fn add_case_nodes(structure: &mut DocumentStructure) {
    let units = structure.text.encode_utf16().collect::<Vec<_>>();
    let added = case_nodes(&CitedText::new(&structure.text), &units, &structure.nodes);
    let heading_starts = added.iter().filter(|node| node.kind == NodeKind::Heading).map(|node| node.range.start).collect::<Vec<_>>();
    for paragraph in structure.nodes.iter_mut().filter(|node| node.kind == NodeKind::Paragraph) {
        end_at_heading(paragraph, &heading_starts, &units);
    }
    structure.nodes.extend(added);
}
