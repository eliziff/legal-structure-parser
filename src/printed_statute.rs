//! A statute read from its print: the paragraphs and headings of a PDF's body, without the
//! running heads, folios and notes between them, as the instrument grammar reads them, and
//! the lines and pages that print a cited provision.

use crate::{
    analyze_instrument, DocumentLookupStatus, DocumentQuery, DocumentStructure, NodeKind,
};
use std::collections::{HashMap, HashSet};

/// A line of print: its id, its text, its box (left, top, right, bottom) and its page.
#[derive(Clone, Copy)]
pub struct PrintedLine<'a> {
    pub id: &'a str,
    pub text: &'a str,
    pub rect: [f64; 4],
    pub page: u32,
}

/// The instrument grammar's reading of a PDF's body text: its paragraphs and headings
/// without the running heads, folios and notes between them, each at its offset.
pub struct PrintedStatute {
    structure: DocumentStructure,
    /// Each paragraph read: its (start, end) in the reading's text, the PDF structure node
    /// it comes from, that node's lines it holds, whether it is a title (a heading or a
    /// marginal note) that opens what follows, and whether it is a history note that closes
    /// what precedes. A marginal note and the provision under it can share a node, as can a
    /// provision and its history note; each is its own paragraph.
    parts: Vec<(usize, usize, usize, Vec<String>, bool, bool)>,
}

/// Where a cited provision is printed: its lines and pages, or that the body prints its
/// label twice.
pub enum ProvisionPlacement {
    Found { lines: Vec<String>, pages: Vec<u32> },
    Ambiguous,
}

/// A line opening a provision: "(2) ...", "(a) ...", or a section number before its text
/// ("12 (1) Every ...", "33(1) The ...", "1‑3(1) For ...", "205 [Repealed ...]"), not a
/// number in prose ("900 metres ...").
fn provision_opening(line: &str) -> bool {
    if line.starts_with('(') { return line.contains(')'); }
    let rest = provision_label(line);
    rest.len() < line.len() && rest.starts_with(char::is_whitespace)
        && rest.trim_start().starts_with(|c: char| c == '(' || c == '[' || c.is_uppercase())
}

/// What follows a provision's number at the start of a line: its digits, the points and
/// hyphens joining them ("4.09", "1‑3"), and the subsections it names ("33(1)", "2(1)(a)").
fn provision_label(line: &str) -> &str {
    if !line.starts_with(|c: char| c.is_ascii_digit()) { return line; }
    let mut rest = line.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | '-' | '\u{2010}' | '\u{2011}'));
    while let Some(inner) = rest.strip_prefix('(') {
        match inner.split_once(')') {
            Some((label, after)) if (1..=6).contains(&label.len())
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') => rest = after,
            _ => break,
        }
    }
    rest
}

/// A line's text with its provision's number set apart from what follows it: a print that runs a
/// subsection's number into its marginal note ("50.4(9)Extension of time", as Westlaw prints a
/// statute) is read as "50.4(9) Extension of time".
fn spaced(text: &str) -> String {
    let rest = provision_label(text);
    let label = &text[..text.len() - rest.len()];
    if label.ends_with(')') && rest.starts_with(char::is_uppercase) { format!("{label} {rest}") } else { text.to_owned() }
}

/// A line as the instrument grammar reads it, where the print repeats a section's number before
/// each of its subsections ("50.4(1) …", "50.4(2) …", as Westlaw prints a statute): the section's
/// number before its first subsection, set apart ("50.4 (1) …"), and only the subsection's after
/// it ("(2) …"). `section` is the section the reading is in.
fn sectioned(text: &str, section: &mut Option<String>) -> String {
    // A section's number printed alone opens it.
    if text.starts_with(|c: char| c.is_ascii_digit()) && provision_label(text).trim_end_matches('.').is_empty()
        && !text.contains('(') {
        *section = Some(text.trim_end_matches('.').to_owned());
        return text.to_owned();
    }
    if !text.starts_with(|c: char| c.is_ascii_digit()) || !provision_opening(text) { return text.to_owned(); }
    let end = text.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '\u{2010}' | '\u{2011}'))).unwrap_or(text.len());
    let (number, rest) = text.split_at(end);
    let number = number.trim_end_matches('.');
    if !rest.starts_with('(') {
        *section = Some(number.to_owned());
        return text.to_owned();
    }
    if section.as_deref() == Some(number) { return rest.to_owned(); }
    *section = Some(number.to_owned());
    format!("{number} {rest}")
}

/// A line of a provision as the reading takes it: the PDF lines printed on it, in order.
struct Row {
    text: String,
    rect: [f64; 4],
    ids: Vec<String>,
    /// The row opens with a number printed apart from its text.
    labelled: bool,
}

/// A node's lines as rows of print. A provision's number set apart in the margin, on its own
/// line beside the text it opens ("33(1)" | "The court shall ..."), is read with that text,
/// before it even where the PDF writes it after.
fn rows<'a>(lines: impl Iterator<Item = PrintedLine<'a>>) -> Vec<(Row, u32)> {
    let mut lines = lines.collect::<Vec<_>>();
    let same_row = |a: &[f64; 4], b: &[f64; 4]| a[3].min(b[3]) - a[1].max(b[1])
        >= (a[3] - a[1]).min(b[3] - b[1]) * 0.5;
    let label = |line: &PrintedLine| {
        let text = line.text.trim();
        !text.is_empty() && provision_label(text).trim_end_matches('.').is_empty()
    };
    for at in 1..lines.len() {
        let (before, after) = (lines[at - 1], lines[at]);
        if before.page == after.page && label(&after) && after.rect[2] <= before.rect[0] && same_row(&after.rect, &before.rect) {
            lines.swap(at - 1, at);
        }
    }
    let mut rows = Vec::<(Row, u32)>::new();
    let mut lines = lines.into_iter().peekable();
    while let Some(line) = lines.next() {
        let mut row = Row { text: spaced(line.text.trim()), rect: line.rect, ids: vec![line.id.to_owned()], labelled: false };
        if label(&line) {
            if let Some(text) = lines.next_if(|text| text.page == line.page
                && text.rect[0] >= line.rect[2] && same_row(&text.rect, &line.rect)) {
                row.text = format!("{} {}", row.text, text.text.trim());
                row.rect = [row.rect[0], row.rect[1].min(text.rect[1]), text.rect[2], row.rect[3].max(text.rect[3])];
                row.ids.push(text.id.to_owned());
                row.labelled = true;
            }
        }
        rows.push((row, line.page));
    }
    rows
}

/// A contents list's entry: a provision's or a Part's number and its title ("12 Early
/// retirement", "Part 2 Alberta Blue Cross Plan"), several to a line or one, ending no sentence;
/// not a provision repealed ("12 [Repealed]").
fn contents_entry(text: &str) -> bool {
    let text = text.trim();
    let rest = provision_label(text);
    let numbered = rest.len() < text.len() && rest.starts_with(char::is_whitespace)
        || ["Part ", "PART ", "Division ", "DIVISION ", "Schedule ", "SCHEDULE "].iter().any(|word| text.starts_with(word));
    numbered && !rest.trim_start().starts_with('[') && !text.ends_with(['.', ';', ':', ','])
}

/// A history note under a provision lists the enactments that made or amended it, each a
/// year or revision and its chapter: "1991, c. 43, s. 4; 2005, c. 22, s. 20", "R.S., c. C-34,
/// s. 1", "R.S., 1985, c. 27 (1st Supp.), s. 13", "2009 c50 s7". It is references only, with
/// no word longer than "suppl."; an amendment's text ("2019, c. 25, s. 5, is replaced by")
/// is no history note.
fn history_note(line: &str) -> bool {
    if line.split(|c: char| !c.is_alphabetic()).any(|word| word.chars().count() > 5) {
        return false;
    }
    let revised = ["R.S.C.", "R.S.", "L.R.C.", "S.R.", "S.C."].iter()
        .find_map(|prefix| line.strip_prefix(prefix)).map(|rest| rest.trim_start_matches([',', ' ']));
    let rest = revised.unwrap_or(line);
    let year = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let rest = match year {
        4 => rest[4..].trim_start_matches([',', ' ']),
        0 if revised.is_some() => rest,
        _ => return false,
    };
    rest.starts_with("c. ") || rest.starts_with("ch. ")
        || rest.strip_prefix('c').is_some_and(|number| number.starts_with(|c: char| c.is_ascii_digit()))
}

impl PrintedStatute {
    /// Reads a PDF's body: `document` is the PDF's structure, `lines` its lines of print by id,
    /// and `page_count` its number of pages.
    pub fn read(document: &DocumentStructure, lines: &HashMap<&str, PrintedLine<'_>>, page_count: usize) -> Option<Self> {
        Self::read_nodes(document, lines, page_count, 0..document.nodes.len())
    }

    /// Reads one instrument of a PDF that prints several ("The Constitution Acts 1867 to 1982"):
    /// from the heading that is its title ("CONSTITUTION ACT, 1982", "PART I Canadian Charter of
    /// Rights and Freedoms", a note's number after it read past) to the next heading that is
    /// another Act's title in capitals ("CANADA ACT 1982", "LOI CONSTITUTIONNELLE DE 1867"). A
    /// contents list that names it comes before its body: the last heading that is its title opens
    /// it. None where no heading is its title, or where the PDF prints one Act.
    pub fn read_within(document: &DocumentStructure, lines: &HashMap<&str, PrintedLine<'_>>, page_count: usize,
        instrument: &str) -> Option<Self> {
        // A heading's words, without a Part's number before them or a note's number after a year.
        let title = |text: &str| {
            let mut words = text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty())
                .map(str::to_uppercase).collect::<Vec<_>>();
            if words.first().is_some_and(|word| word == "PART") && words.len() > 2 { words.drain(..2); }
            let year = |word: &String| word.len() == 4 && word.chars().all(|c| c.is_ascii_digit());
            if words.len() > 1 && words.last().is_some_and(|note| note.len() <= 3 && note.chars().all(|c| c.is_ascii_digit()))
                && year(&words[words.len() - 2]) { words.pop(); }
            words
        };
        let wanted = title(instrument);
        if wanted.is_empty() { return None; }
        let text_of = |node: &crate::StructureNode| node.line_ids.iter().filter_map(|id| lines.get(id.as_str()))
            .map(|line| line.text.trim()).collect::<Vec<_>>().join(" ");
        let heading = |node: &crate::StructureNode| node.kind == NodeKind::Heading;
        // Another Act's title: a heading in capitals that ends in its year.
        let act_title = |node: &crate::StructureNode| {
            let text = text_of(node);
            let words = title(&text);
            !text.chars().any(char::is_lowercase) && words.iter().any(|word| word == "ACT" || word == "LOI")
                && words.last().is_some_and(|year| year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()))
        };
        // Only a PDF that prints several Acts is read by one of them.
        let acts = document.nodes.iter().filter(|node| heading(node) && act_title(node))
            .map(|node| title(&text_of(node))).collect::<HashSet<_>>();
        if acts.len() < 2 { return None; }
        let titled = document.nodes.iter().enumerate()
            .filter(|(_, node)| heading(node) && title(&text_of(node)) == wanted).map(|(at, _)| at).collect::<Vec<_>>();
        titled.into_iter().rev().find_map(|start| {
            let end = document.nodes.iter().enumerate().skip(start + 1)
                .find(|(_, node)| heading(node) && act_title(node) && title(&text_of(node)) != wanted)
                .map_or(document.nodes.len(), |(end, _)| end);
            Self::read_nodes(document, lines, page_count, start..end)
        })
    }

    fn read_nodes(document: &DocumentStructure, lines: &HashMap<&str, PrintedLine<'_>>, page_count: usize,
        scope: std::ops::Range<usize>) -> Option<Self> {
        let (mut text, mut parts, mut offset) = (String::new(), Vec::new(), 0);
        // The page of the last paragraph read while its sentence is still open, and whether
        // that paragraph is a history note.
        let mut open: Option<u32> = None;
        let mut open_note = false;
        // The provisions follow a contents list at the front; what precedes it is front matter.
        // A list opening past the middle of the document (a rule's own contents) is not at its front.
        let middle = page_count / 2;
        let mut contents = document.nodes.iter().filter(|node| node.grammar.as_deref() == Some("contents")
                && node.page_indexes.first().is_some_and(|page| *page < middle))
            .filter_map(|node| Some((*node.page_indexes.first()?, node.range.end))).collect::<Vec<_>>();
        contents.sort_unstable();
        // The list runs page after page from where it opens; a leader row further on is not it.
        let listed = contents.iter().enumerate().take_while(|(at, (page, _))|
            *at == 0 || *page <= contents[at - 1].0 + 1).map(|(_, entry)| *entry).collect::<Vec<_>>();
        let front = listed.iter().map(|(_, end)| *end).max().unwrap_or(0);
        // The page the list ends on, where it may run on into the body.
        let last_listed = listed.iter().map(|(page, _)| *page).max();
        // The nodes read, each as its rows of print and their pages.
        let mut nodes = Vec::new();
        for (index, node) in document.nodes.iter().enumerate() {
            if !scope.contains(&index) || !matches!(node.kind, NodeKind::Prose | NodeKind::Heading) { continue; }
            // A contents list and a parallel translation repeat the body's sections; the body is
            // read. A list may run on into the body on its last page: what follows its last entry
            // there is read. Before that page, what follows an entry is the rest of its title.
            let contents = node.grammar.as_deref() == Some("contents");
            let before_last = contents && node.range.start < front
                && node.page_indexes.first().is_some_and(|page| Some(*page) < last_listed);
            if node.grammar.as_deref() == Some("translation") || node.range.start < front && !contents || before_last { continue; }
            let (mut found, mut found_pages): (Vec<_>, Vec<_>) = rows(node.line_ids.iter().filter_map(|id| lines.get(id.as_str()).copied())
                .filter(|line| !line.text.trim().is_empty())).into_iter().unzip();
            if contents {
                let last = found.iter().rposition(|row| contents_entry(&row.text)).map_or(found.len(), |last| last + 1);
                found.drain(..last);
                found_pages.drain(..last);
            }
            if !found.is_empty() { nodes.push((index, node.kind == NodeKind::Heading, found, found_pages)); }
        }
        // Three or more paragraphs in a row, each a numbered title and nothing more, are a contents
        // list the PDF does not mark; a provision's number printed alone opens the paragraph after it.
        let listed = |rows: &[Row]| rows.iter().all(|row| contents_entry(&row.text));
        let mut skip = vec![false; nodes.len()];
        let mut at = 0;
        while at < nodes.len() {
            let run = nodes[at..].iter().take_while(|(_, heading, rows, _)| !heading && listed(rows)).count();
            if run >= 3 { skip[at..at + run].fill(true); }
            at += run.max(1);
        }
        let mut pending: Option<Row> = None;
        let nodes = nodes.into_iter().zip(skip).filter(|(_, skip)| !skip).filter_map(|((index, heading, mut found, pages), _)| {
            if let Some(label) = pending.take() {
                let first = &mut found[0];
                // A number alone before a line that opens with its own number ("50.4" over "50.4(1) …")
                // or with a paragraph's letter ("3" over "(a) …"), or set past the middle of the line
                // after it (a folio at the page's foot), is no provision's number: a page's folio, or a
                // section's number its subsections repeat.
                let opens = first.text.trim();
                let folio = provision_label(opens).len() < opens.len()
                    || opens.strip_prefix('(').is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_lowercase()))
                    || label.rect[0] > (first.rect[0] + first.rect[2]) / 2.0;
                if !folio {
                    first.text = format!("{} {}", label.text, first.text);
                    first.ids.splice(0..0, label.ids);
                    first.labelled = true;
                }
            }
            // A number alone is no heading, whatever its type.
            if found.len() == 1 && provision_label(found[0].text.trim()).trim_end_matches('.').is_empty() {
                pending = found.pop();
                return None;
            }
            Some((index, heading, found, pages))
        }).collect::<Vec<_>>();
        let mut section = None;
        for (index, heading, found, found_pages) in nodes {
            let (left, right) = found.iter().fold((f64::MAX, f64::MIN), |(left, right), line|
                (left.min(line.rect[0]), right.max(line.rect[2])));
            // A line ends its paragraph when it stops short of the column or closes a clause.
            let ends = |at: usize| found[at].rect[2] < right - (right - left) * 0.2
                || found[at].text.trim_end().ends_with(['.', ';', ':']);
            // A sentence a page break cuts ("... made under paragraph" / "672.54(b) that ...")
            // goes on in the next page's first paragraph, whose reference opens no provision.
            let first = found[0].text.trim();
            let carried = !heading && open.is_some_and(|page| page < found_pages[0])
                && first.starts_with(|c: char| c.is_lowercase() || c.is_ascii_digit()) && !provision_opening(first);
            if carried { text.push(' '); offset += 1; }
            else if !text.is_empty() { text.push_str("\n\n"); offset += 2; }
            // A lone line ending no clause titles the provision below it.
            let title = |lines: &[String], last: &str| heading
                || lines.len() == 1 && !last.ends_with(['.', ';', ':', ',', ')', ']']);
            let mut part = (offset, Vec::new());
            // Whether the paragraph being read is a history note.
            let mut noted = history_note(first) || carried && open_note;
            for (at, line) in found.iter().enumerate() {
                let line_text = &sectioned(line.text.trim(), &mut section);
                let note = history_note(line_text);
                // A provision's number opening a line starts a new paragraph after one that
                // ended, or after a marginal note: a line standing alone above it. A history
                // note starts one after the provision it follows ends.
                // Numbered rows one after another are a contents list's entries, not provisions.
                let listed = line.labelled && found.get(at + 1).is_some_and(|next| next.labelled);
                if at > 0 && (provision_opening(line_text) && !listed && (ends(at - 1) || at == 1 || ends(at - 2))
                    || note && ends(at - 1)) {
                    let title = title(&part.1, found[at - 1].text.trim());
                    parts.push((part.0, offset, index, std::mem::take(&mut part.1), title && !noted, noted));
                    noted = note;
                    text.push_str("\n\n");
                    offset += 2;
                    part.0 = offset;
                } else if at > 0 {
                    text.push(' ');
                    offset += 1;
                }
                text.push_str(line_text);
                offset += line_text.encode_utf16().count();
                part.1.extend(line.ids.iter().cloned());
            }
            let last = found[found.len() - 1].text.trim();
            let title = title(&part.1, last);
            open = (!title && !last.ends_with(['.', ';', ':'])).then(|| found_pages[found.len() - 1]);
            open_note = noted;
            parts.push((part.0, offset, index, part.1, title && !noted, noted));
        }
        // Read as statuteOutline reads text: lines recovered where provisions open run together
        // ("... a. 48. 49. The courts ..."); the recovery only breaks lines, so offsets hold.
        // A section's number printed alone over its first subsection ("11.4" / "(1) Critical
        // supplier …") is read as the grammar reads the two on one line ("11.4  (1) …"): the
        // paragraph break between them becomes two spaces, so offsets hold here too.
        let mut joined = String::with_capacity(text.len());
        let paragraphs = text.split("\n\n").collect::<Vec<_>>();
        for (at, paragraph) in paragraphs.iter().enumerate() {
            joined.push_str(paragraph);
            if at + 1 == paragraphs.len() { break; }
            let alone = paragraph.starts_with(|c: char| c.is_ascii_digit())
                && provision_label(paragraph).trim_end_matches('.').is_empty() && !paragraph.contains('(');
            joined.push_str(if alone && paragraphs[at + 1].starts_with('(') { "  " } else { "\n\n" });
        }
        let structure = analyze_instrument(joined, document.document_id.clone(), &[], true).ok()?;
        Some(Self { structure, parts })
    }

    /// The reading's own structure: its sections as the instrument grammar reads them.
    pub fn structure(&self) -> &DocumentStructure {
        &self.structure
    }

    /// The paragraphs a range of the reading covers: their PDF node, lines, and whether
    /// each is a title and a history note.
    pub fn parts(&self, start: usize, end: usize) -> impl Iterator<Item = (usize, &[String], bool, bool)> + '_ {
        self.parts.iter().filter(move |(from, to, ..)| *from < end && start < *to)
            .map(|(_, _, node, lines, title, note)| (*node, lines.as_slice(), *title, *note))
    }

    /// Whether the reading places provisions of a kind: sections, a code's articles and rules,
    /// and the Parts, Divisions and Schedules that hold them.
    pub fn places(kind: &str) -> bool {
        matches!(kind, "section" | "article" | "rule" | "part" | "division" | "schedule")
    }

    /// A provision as the reading reads the PDF's body (`document`, the PDF's structure): its
    /// lines and pages. A contents list and a parallel translation are not read; a label the
    /// body prints twice is ambiguous.
    pub fn provision(&self, document: &DocumentStructure, kind: &str, locator: &str) -> Option<ProvisionPlacement> {
        let instrument = &self.structure;
        // A Part, Division or Schedule runs from its heading to the next of its kind.
        let container = match kind {
            "part" | "division" => {
                let number = locator.trim().trim_start_matches(|c: char| c.is_alphabetic() && !"IVXLCDM".contains(c)).trim();
                let value = number.parse::<u32>().ok().or_else(|| roman(number))?;
                Some(format!("{}{value}", if kind == "part" { "part" } else { "div" }))
            }
            "schedule" => Some(format!("sched{}", locator.trim().to_lowercase())),
            "section" | "article" | "rule" => None,
            _ => return None,
        };
        if let Some(label) = container {
            let mut found = instrument.nodes.iter().filter(|node| node.label.as_deref() == Some(label.as_str()));
            let node = found.next()?;
            if found.next().is_some() { return Some(ProvisionPlacement::Ambiguous); }
            let lines = self.parts(node.range.start, node.range.end).flat_map(|(_, lines, ..)| lines.iter().cloned()).collect();
            return self.placed(document, lines);
        }
        let query = DocumentQuery::new();
        // A provision's span in the reading, or None where it is not read; Err where it is read twice.
        let span = |locator: &str| {
            let found = query.structure_block(instrument, locator, 0);
            let block = found.block.filter(|_| matches!(found.status, DocumentLookupStatus::Found))?;
            let repeated = format!("{}@", block.block.label);
            Some(if instrument.nodes.iter().any(|node| node.label.as_deref().is_some_and(|label| label.starts_with(&repeated))) {
                Err(())
            } else {
                Ok((block.block.start, block.block.end))
            })
        };
        // A range ("49-51", "5-2-5-3") runs from its first provision through its last, unless a
        // provision is itself numbered so ("1-2"); its dash is the one both ends are read at.
        let range = || {
            locator.char_indices().filter(|(_, c)| matches!(c, '-' | '\u{2013}')).find_map(|(at, dash)| {
                let (first, last) = (locator[..at].trim(), locator[at + dash.len_utf8()..].trim());
                let (first, last) = (span(first)?, span(last)?);
                Some(first.and_then(|first| last.map(|last| (first.0, last))).and_then(|(start, last)|
                    if start <= last.0 { Ok((start, last)) } else { Err(()) }))
            })
        };
        let (start, (last, end)) = match span(locator).map(|found| found.map(|(start, end)| (start, (start, end))))
            .or_else(range)? {
            Ok(found) => found,
            Err(()) => return Some(ProvisionPlacement::Ambiguous),
        };
        let mut parts = self.parts(last, end).collect::<Vec<_>>();
        // A history note lists a provision's enactments and closes it: neither it nor what
        // follows it is the provision's text. A heading or marginal note closing the block
        // opens the next provision.
        if let Some(note) = parts.iter().skip(1).position(|(.., note)| *note) {
            parts.truncate(note + 1);
        }
        while parts.len() > 1 && parts.last().is_some_and(|(_, _, title, _)| *title) {
            parts.pop();
        }
        let lines = self.parts(start, last).chain(parts).flat_map(|(_, lines, ..)| lines.iter().cloned()).collect();
        self.placed(document, lines)
    }

    /// Lines placed, once each, with their pages from the PDF's page nodes.
    fn placed(&self, document: &DocumentStructure, lines: Vec<String>) -> Option<ProvisionPlacement> {
        let mut seen = HashSet::new();
        let lines = lines.into_iter().filter(|line| seen.insert(line.clone())).collect::<Vec<_>>();
        // Each line's page, from the PDF's page nodes.
        let page_of = document.nodes.iter().filter(|node| node.kind == NodeKind::Page)
            .flat_map(|node| node.line_ids.iter().map(move |line| (line.as_str(),
                node.page_indexes.first().map_or(0, |index| *index as u32 + 1))))
            .collect::<HashMap<_, _>>();
        let mut pages = lines.iter().filter_map(|line| page_of.get(line.as_str()).copied()).collect::<Vec<_>>();
        pages.sort_unstable();
        pages.dedup();
        (!lines.is_empty()).then_some(ProvisionPlacement::Found { lines, pages })
    }
}

/// A Roman numeral's value ("XXIII" is 23).
fn roman(value: &str) -> Option<u32> {
    let digit = |c: char| Some(match c { 'I' => 1, 'V' => 5, 'X' => 10, 'L' => 50, 'C' => 100, 'D' => 500, 'M' => 1000, _ => return None });
    let digits = value.chars().map(digit).collect::<Option<Vec<u32>>>()?;
    let total = digits.iter().enumerate().fold(0i64, |total, (at, value)|
        if digits.get(at + 1).is_some_and(|next| next > value) { total - i64::from(*value) } else { total + i64::from(*value) });
    (total > 0 && !digits.is_empty()).then_some(total as u32)
}
