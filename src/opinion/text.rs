//! An opinion as plain text: what a PDF extractor writes, fixed-width, its page furniture in the flow. Its layout
//! is the evidence: which lines repeat with a climbing number, where a line sits, what stands alone.
//!
//! Pages come from form feeds and from page furniture, found by shape: lines alike but for one number that climbs
//! from line to line, far apart ("-5-", "Page 3 of 10", an ECF stamp, a running title and its number). Note zones are
//! the page ends that open with the next note's number; note pairing pairs their labels with the calls.

use super::*;
use crate::note_pairing::pair_footnotes;
use legal_citations::cues;
use legal_structure_model::layout::{Line as LaidLine, Page as LaidPage, PdfSourceSpan};
use std::collections::BTreeMap;

/// The lines between one page number and the next, at the least: a list's numbers come closer than a page.
const LEAST_PAGE_LINES: usize = 6;

struct Line<'a> {
    text: &'a str,
    /// The line's words, past a margin's line number.
    body: &'a str,
    start: usize,
    /// Columns before `body`.
    indent: usize,
    fed: bool,
    margin: Option<ScalarRange>,
    margin_number: u32,
}

impl Line<'_> {
    fn blank(&self) -> bool { self.body.is_empty() }
    fn width(&self) -> usize { self.indent + self.body.chars().count() }
    fn range(&self) -> ScalarRange {
        let lead = self.text.len() - self.text.trim_start_matches(javascript_whitespace).len();
        let start = self.start + utf16_len(&self.text[..lead]);
        ScalarRange { start, end: start + utf16_len(self.text.trim_matches(javascript_whitespace)) }
    }
    fn body_range(&self) -> ScalarRange {
        let end = self.start + utf16_len(self.text.trim_end_matches(javascript_whitespace));
        ScalarRange { start: end - utf16_len(self.body), end }
    }
}

fn split_lines(text: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let (mut at, mut fed) = (0, false);
    for part in text.split_inclusive(['\n', '\u{c}']) {
        let content = part.trim_end_matches(['\n', '\r', '\u{c}']);
        let body = content.trim_matches(javascript_whitespace);
        let indent = content.len() - content.trim_start_matches(javascript_whitespace).len();
        let indent = content[..indent].chars().map(|c| if c == '\t' { 8 } else { 1 }).sum();
        lines.push(Line { text: content, body, start: at, indent, fed, margin: None, margin_number: 0 });
        at += utf16_len(part);
        fed = part.ends_with('\u{c}');
    }
    lines
}

/// A pleading's line numbers down the margin, counted from 1 on each page ("12  Defendant moved ..."): taken off
/// each line, where most lines carry one. Returns whether they did.
fn margin_numbers(lines: &mut [Line<'_>]) -> bool {
    static MARGIN: OnceLock<Regex> = OnceLock::new();
    let margin = MARGIN.get_or_init(|| Regex::new(r"^[ \t]{0,6}(\d{1,2})(?:[ \t]+(\S.*))?$").unwrap());
    let numbers = lines.iter().map(|line| margin.captures(line.text.trim_end_matches(javascript_whitespace))
        .map(|capture| capture[1].parse::<u32>().unwrap())).collect::<Vec<_>>();
    let written = lines.iter().filter(|line| !line.blank()).count();
    let (mut counted, mut last) = (0, 0);
    for number in numbers.iter().flatten() {
        if *number == last + 1 || *number == 1 { counted += 1; }
        last = *number;
    }
    if written < 12 || counted * 2 < written {
        return false;
    }
    for (line, number) in lines.iter_mut().zip(&numbers) {
        let Some(number) = *number else { continue };
        line.margin_number = number;
        let trimmed = line.text.trim_end_matches(javascript_whitespace);
        let capture = margin.captures(trimmed).unwrap();
        let digits = capture.get(1).unwrap();
        let start = line.start + utf16_len(&trimmed[..digits.start()]);
        line.margin = Some(ScalarRange { start, end: start + utf16_len(digits.as_str()) });
        let rest = capture.get(2).map_or("", |rest| rest.as_str());
        line.indent = capture.get(2).map_or(0, |rest| rest.start());
        line.body = rest;
    }
    true
}

/// The line's shape, its numbers masked, and each number with its byte range.
fn shape(body: &str) -> (String, Vec<(u32, usize, usize)>) {
    let mut shaped = String::with_capacity(body.len());
    let mut numbers = Vec::new();
    let mut chars = body.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c.is_ascii_digit() {
            let mut end = at + 1;
            while let Some(&(next, c)) = chars.peek() {
                if !c.is_ascii_digit() { break; }
                end = next + 1;
                chars.next();
            }
            shaped.push('#');
            if let Ok(value) = body[at..end].parse() { numbers.push((value, at, end)); }
        } else if c.is_whitespace() {
            if !shaped.ends_with(' ') { shaped.push(' '); }
        } else {
            shaped.push(c);
        }
    }
    (shaped, numbers)
}

struct Chain { series: &'static str, members: Vec<(usize, u32)>, header: bool, shape: String }

/// What a chain of running heads goes by, in place of a line's shape.
const RUNNING_HEADS: &str = "running heads";

/// A slip opinion heads each sheet with its number, on the left and right by turns under a running title, and each opinion
/// in it counts from 1 again: "2      HENDERSON v. UNITED STATES", "Cite as: 568 U. S. ____ (2013)    3". The title repeats
/// sheet to sheet, which a paragraph's or a note's number at a sheet's head does not.
fn running_heads(lines: &[Line<'_>]) -> Option<Vec<(usize, u32)>> {
    const FEWEST_SHEETS: usize = 4;
    const LONGEST_HEAD: usize = 100;
    static LEFT: OnceLock<Regex> = OnceLock::new();
    static RIGHT: OnceLock<Regex> = OnceLock::new();
    let left = LEFT.get_or_init(|| Regex::new(r"^(\d{1,3})\s{2,}\S").unwrap());
    let right = RIGHT.get_or_init(|| Regex::new(r"\S\s{3,}(\d{1,3})$").unwrap());
    let sheets = lines.iter().enumerate().filter(|(_, line)| line.fed).map(|(at, _)| at).collect::<Vec<_>>();
    if sheets.len() < FEWEST_SHEETS {
        return None;
    }
    let mut titles = HashMap::<String, usize>::new();
    let heads = sheets.iter().filter_map(|&sheet| {
        let at = (sheet..lines.len()).take_while(|at| *at == sheet || !lines[*at].fed).find(|at| !lines[*at].blank())?;
        let body = lines[at].body;
        let number = left.captures(body).or_else(|| right.captures(body)).filter(|_| body.len() <= LONGEST_HEAD)?[1].parse::<u32>().ok()?;
        let title = shape(body).0;
        *titles.entry(title.clone()).or_default() += 1;
        Some((at, number, title))
    }).collect::<Vec<_>>();
    let members = heads.into_iter().filter(|(_, _, title)| titles[title] >= 2).map(|(at, number, _)| (at, number)).collect::<Vec<_>>();
    (members.len() * 4 >= sheets.len() * 3).then_some(members)
}

/// The longest run of lines in order whose number climbs by at most `reach`, each far enough from the last.
fn climbing(members: &[(usize, u32)]) -> Vec<(usize, u32)> {
    const REACH: u32 = 3;
    let n = members.len();
    let mut best = vec![1usize; n];
    let mut parent = vec![None; n];
    for i in 0..n {
        for j in i.saturating_sub(64)..i {
            let (line, value) = members[j];
            if members[i].1 > value && members[i].1 - value <= REACH && members[i].0 - line >= LEAST_PAGE_LINES && best[j] + 1 > best[i] {
                best[i] = best[j] + 1;
                parent[i] = Some(j);
            }
        }
    }
    let Some(mut at) = (0..n).max_by_key(|&i| (best[i], std::cmp::Reverse(i))) else { return Vec::new() };
    let mut chain = vec![members[at]];
    while let Some(previous) = parent[at] {
        chain.push(members[previous]);
        at = previous;
    }
    chain.reverse();
    chain
}

/// A page number's own line, or a stamp: the number alone, framed, or among at least three words.
fn page_shape(shaped: &str) -> Option<&'static str> {
    static PAGE: OnceLock<Regex> = OnceLock::new();
    static STAMP: OnceLock<Regex> = OnceLock::new();
    let page = PAGE.get_or_init(|| Regex::new(r"(?i)^(?:#|[-–—~•·*]+ ?# ?[-–—~•·*]+|\(#\)|(?:page|p\.|pg\.?|slip op\.?( at)?) ?#(?: of #)?|# of #|#/#|# / #)$").unwrap());
    let stamp = STAMP.get_or_init(|| Regex::new(r"(?i)\b(?:case|document|filed|pageid|appellate|usca|doc\.)\b").unwrap());
    if page.is_match(shaped) {
        return Some("slip");
    }
    if shaped.split(' ').count() < 2 || !shaped.contains('#') {
        return None;
    }
    Some(if stamp.is_match(shaped) { "ecf" } else { "slip" })
}

fn page_chains(lines: &[Line<'_>], width: usize, page_count: Option<u32>) -> (Vec<Chain>, Vec<usize>) {
    let mut shapes = BTreeMap::<String, Vec<(usize, Vec<(u32, usize, usize)>)>>::new();
    for (index, line) in lines.iter().enumerate() {
        // A sentence is no page stamp, however its numbers run.
        if line.blank() || line.body.len() > 140 || line.body.split_whitespace().count() > 22 { continue; }
        // A scanner's "Page | of 21" is page 1.
        let read = ["Page | of ", "Page l of ", "Page I of "].iter().find(|misread| line.body.starts_with(**misread))
            .map_or_else(|| line.body.to_owned(), |_| format!("Page 1{}", &line.body[6..]));
        let (shaped, numbers) = shape(&read);
        if !numbers.is_empty() && page_shape(&shaped).is_some() {
            shapes.entry(shaped).or_default().push((index, numbers));
        }
    }
    let alone = |index: usize| {
        let empty = |at: Option<&Line>| at.is_none_or(Line::blank);
        let (line, before) = (&lines[index], index.checked_sub(1).and_then(|at| lines.get(at)));
        line.fed || lines.get(index + 1).is_some_and(|next| next.fed)
            || (empty(before) && empty(lines.get(index + 1)))
            || line.indent * 5 >= width
    };
    let mut chains = Vec::new();
    for (shaped, group) in &shapes {
        let series = page_shape(shaped).unwrap();
        let plain = shaped == "#";
        let fields = group.iter().map(|(_, numbers)| numbers.len()).max().unwrap_or(0);
        let mut best = Vec::new();
        for field in 0..fields {
            let members = group.iter()
                .filter(|(index, numbers)| numbers.len() > field && (!plain || alone(*index)))
                .map(|(index, numbers)| (*index, numbers[field].0))
                .collect::<Vec<_>>();
            let chain = climbing(&members);
            if chain.len() > best.len() { best = chain; }
        }
        // The record knows its pages: numbers that run further, or too few of them for its length, are some other count.
        let plausible = page_count.filter(|count| *count > 0).is_none_or(|count| {
            best.first().zip(best.last()).is_none_or(|(first, last)| last.1 - first.1 < count * 2 + 2) && best.len() * 4 >= count.min(12) as usize
        });
        // Pages run about alike: a chain with a gap many times its usual is a list's numbers, "14 Id." and "No. 7."
        let mut gaps = best.windows(2).map(|pair| pair[1].0 - pair[0].0).collect::<Vec<_>>();
        gaps.sort_unstable();
        let even = gaps.last().is_none_or(|longest| *longest <= gaps[gaps.len() / 2] * 3);
        if !plausible || !even {
            continue;
        }
        let fed = best.iter().any(|(index, _)| lines[*index].fed || lines.get(index + 1).is_some_and(|next| next.fed));
        let says_page = shaped.to_lowercase().starts_with("page # of #");
        if best.len() >= if says_page { 1 } else if plain && !fed { 3 } else { 2 } {
            chains.push(Chain { series, members: best, header: false, shape: shaped.clone() });
        }
    }
    chains.sort_by_key(|chain| std::cmp::Reverse(chain.members.len()));
    // One numbering of each kind: the longest; a shorter one of the same kind is the same pages' other shape, or noise.
    let mut kept: Vec<Chain> = Vec::new();
    for chain in chains {
        if !kept.iter().any(|other| other.series == chain.series) { kept.push(chain); }
    }
    // The lines that repeat word for word, far apart: running titles and footers.
    let mut seen = BTreeMap::<&str, Vec<usize>>::new();
    for (index, line) in lines.iter().enumerate() {
        if !line.blank() && line.body.len() <= 110 { seen.entry(line.body).or_default().push(index); }
    }
    let mut running = seen.into_values()
        .filter(|at| {
            let far = at.windows(2).filter(|pair| pair[1] - pair[0] >= LEAST_PAGE_LINES).count();
            // A note's short cite repeats too: "Id. at 5." A running line opens on a word and closes on its number.
            let body = lines[at[0]].body;
            let dated = body.len() <= 40 && body.starts_with(char::is_uppercase) && body.ends_with(|c: char| c.is_ascii_digit()) && !body.starts_with("Id") && !body.starts_with("See");
            at.len() >= if dated { 2 } else { 3 } && far * 4 >= (at.len() - 1) * 3
        })
        .flatten().collect::<Vec<_>>();
    // A framed stamp's lines the climbing run passed over; a bare number may be anything.
    for chain in kept.iter().filter(|chain| chain.shape != "#") {
        running.extend(shapes[&chain.shape].iter().map(|(index, _)| *index).filter(|index| !chain.members.iter().any(|member| member.0 == *index)));
    }
    if let Some(members) = running_heads(lines) {
        running.extend(members.iter().map(|member| member.0));
        kept.retain(|chain| chain.series != "slip");
        kept.push(Chain { series: "slip", members, header: true, shape: RUNNING_HEADS.to_owned() });
    }
    (kept, running)
}

/// Whether a chain's numbers head their pages: the text before the first printed number is then pages below it,
/// and footers it and those on it.
fn heads_pages(chain: &Chain) -> bool {
    let &(first_line, first) = chain.members.first().unwrap();
    let &(last_line, last) = chain.members.last().unwrap();
    let page_lines = (last_line - first_line) as f64 / (last - first).max(1) as f64;
    let pages_before = first_line as f64 / page_lines.max(1.0);
    (pages_before - (first as f64 - 1.0)).abs() < (pages_before - first as f64).abs()
}

#[derive(Default)]
pub(super) struct Reading {
    pub pages: Vec<StructureNode>,
    pub paragraphs: Vec<OpinionParagraph>,
    pub notes: Vec<OpinionNote>,
    pub headings: Vec<OpinionHeading>,
    pub furniture: Vec<ScalarRange>,
    pub front: Vec<FrontLine>,
    pub tables: Vec<OpinionTable>,
}

fn pages_of(lines: &[Line<'_>], text_end: usize, chains: &mut [Chain], margins: bool) -> Vec<StructureNode> {
    let mut pages = Vec::new();
    let breaks = lines.iter().enumerate().filter(|(_, line)| line.fed).map(|(index, _)| index).collect::<Vec<_>>();
    for chain in chains.iter_mut() {
        chain.header = heads_pages(chain);
        let starts = if !breaks.is_empty() {
            // A form feed opens each page: the number within takes it.
            let mut bounds = vec![0];
            bounds.extend(&breaks);
            let number_in = |from: usize, to: usize| chain.members.iter().find(|(line, _)| (from..to).contains(line));
            let offset = chain.members.first().map_or(0i64, |&(line, value)| {
                value as i64 - bounds.partition_point(|&bound| bound <= line) as i64
            });
            let heads = chain.shape == RUNNING_HEADS;
            let mut last = offset;
            bounds.iter().enumerate().map(|(index, &from)| {
                let to = bounds.get(index + 1).copied().unwrap_or(lines.len());
                let member = number_in(from, to);
                // Running heads start again with each opinion: an unnumbered sheet follows the one before it.
                let counted = if heads { last + 1 } else { index as i64 + 1 + offset };
                last = member.map_or(counted, |member| member.1 as i64);
                (from, last, member.map(|member| member.0))
            }).filter(|(_, number, _)| *number >= 1).collect::<Vec<_>>()
        } else if chain.header {
            let mut starts = chain.members.iter().map(|&(line, value)| (line, value as i64, Some(line))).collect::<Vec<_>>();
            if chain.members[0].1 > 1 && chain.members[0].0 > 0 { starts.insert(0, (0, chain.members[0].1 as i64 - 1, None)); }
            starts
        } else {
            // A footer closes its page, and text after the last footer is the page after it.
            let mut starts = Vec::new();
            let mut start = 0;
            for &(line, value) in &chain.members {
                starts.push((start, value as i64, Some(line)));
                start = line + 1;
            }
            if lines[start..].iter().any(|line| !line.blank()) {
                starts.push((start, chain.members.last().unwrap().1 as i64 + 1, None));
            }
            starts
        };
        // Each opinion under running heads is a numbering of its own: "slip", "slip-2", "slip-3".
        let (mut opinion, mut before) = (1, 0);
        for (index, &(line, number, marker)) in starts.iter().enumerate() {
            let end = starts.get(index + 1).map_or(text_end, |next| lines[next.0].start);
            if chain.shape == RUNNING_HEADS && number <= before { opinion += 1; }
            before = number;
            pages.push(read_page(number.to_string(), if opinion == 1 { chain.series.to_owned() } else { format!("{}-{opinion}", chain.series) }, lines[line].start, end, marker.map(|at| lines[at].range()), if marker.is_some() { "furniture" } else { "counted" }));
        }
    }
    if chains.is_empty() {
        // Pages no printed number names, by the form feeds or the margin's count restarting: their places in the text.
        let mut starts = if !breaks.is_empty() { breaks.clone() } else { Vec::new() };
        if starts.is_empty() && margins {
            let mut last = 0;
            for (index, line) in lines.iter().enumerate() {
                if line.margin.is_none() { continue; }
                if line.margin_number <= 2 && last >= 5 { starts.push(index); }
                last = line.margin_number;
            }
        }
        if !starts.is_empty() {
            starts.insert(0, 0);
            for (index, &line) in starts.iter().enumerate() {
                let end = starts.get(index + 1).map_or(text_end, |next| lines[*next].start);
                pages.push(read_page((index + 1).to_string(), "sheet".to_owned(), lines[line].start, end, None, "break"));
            }
        }
    }
    pages
}

fn note_label(body: &str) -> Option<(u32, usize)> {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    // ... or the number run into the note's first word, as a word processor's superscript comes out: "5See".
    let capture = LABEL.get_or_init(|| Regex::new(r#"^(?:(?:\[(\d{1,3})\]|\((\d{1,3})\)|(\d{1,3})[.):]?|FN ?(\d{1,3}) ?[.:]?|Footnote (\d{1,3})[.:]?)(?:\s+|$)\S?|(\d{1,3})[A-Z“"‘]|(\d{1,3})/\s)"#).unwrap()).captures(body)?;
    let number = (1..=7).find_map(|group| capture.get(group))?;
    Some((number.as_str().parse().ok()?, number.end()))
}

/// Where a line calls a note: its number run into the word or stop before it, or standing after a stop.
fn call_in(line: &str, number: u32) -> Option<usize> {
    let digits = number.to_string();
    line.match_indices(&digits).map(|(at, _)| at).find(|&at| {
        let before = line[..at].chars().next_back();
        let after = line[at + digits.len()..].chars().next();
        // "act.” 1 We hold": an extractor set the call off.
        let word = line[..at].trim_end().trim_end_matches('.').rsplit(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_lowercase();
        let set_off = before == Some(' ') && line[..at].trim_end().ends_with(['.', '”', '"']) && !matches!(word.as_str(), "ex" | "no" | "nos" | "doc" | "dkt" | "p" | "pp" | "at" | "para" | "art" | "sec");
        !after.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, ',' | '.' | '/' | ':' | '-'))
            && (set_off || before.is_some_and(|c| c.is_alphabetic() || matches!(c, '.' | ',' | ';' | ':' | ')' | ']' | '"' | '”' | '’' | '\'' | '—' | '…' | '@' | '‖')))
    })
}

fn calls(lines: &[&str], number: u32) -> bool {
    lines.iter().any(|line| call_in(line, number).is_some())
}

struct Zone { lines: Vec<usize>, label: Option<(u32, usize)>, symbol: Option<String> }

/// A wrapped citation opens a line on its volume: "28 F.3d 1146", "483 U.S. 1", "12 U.S.C. § 2614".
fn opens_on_volume(rest: &str) -> bool {
    static VOLUME: OnceLock<Regex> = OnceLock::new();
    VOLUME.get_or_init(|| Regex::new(r"^(?:\p{Lu}\S*\.|\S*\d[a-z]{1,2})\s+[\d§]").unwrap()).is_match(rest)
}

/// How a label is printed: its bracket and what closes it.
fn label_form(body: &str, end: usize) -> (Option<char>, Option<char>) {
    (body.chars().next().filter(|c| !c.is_ascii_digit()), body[end..].chars().next().filter(|c| matches!(c, '.' | ')' | ']' | ':')))
}

/// A note a symbol opens at a page's foot: "* The Honorable ...", "† Sitting by designation".
fn symbol_label(body: &str) -> Option<&str> {
    static SYMBOL: OnceLock<Regex> = OnceLock::new();
    SYMBOL.get_or_init(|| Regex::new(r"^(\*{1,3}|∗|†|‡)(?:\s?\p{Lu}|$)").unwrap()).captures(body).and_then(|capture| capture.get(1)).map(|symbol| symbol.as_str())
}

/// Each page's notes. A note opens at the next note's number where a call in the text before it, or a rule, supports
/// it; on a page, it runs to the next note or the page's end. In a text whose pages were lost a note stands in the flow
/// and the text resumes after it: it ends where its own lines do. A rule over lines that open with no number carries
/// the note before it on.
/// More lines than a page ever holds, blanks and all: the numbers that bound it missed pages between them.
const PAGE_LINES: usize = 150;

fn note_zones(lines: &[Line<'_>], masked: &[bool], pages: &[(usize, usize)], paged: bool, width: usize, bracketed_paragraphs: bool) -> Vec<Vec<Zone>> {
    static RULE: OnceLock<Regex> = OnceLock::new();
    let rule = RULE.get_or_init(|| Regex::new(r"^[_\-—–=](?:\s?[_\-—–=]){4,}$").unwrap());
    let mut zones = Vec::new();
    let (mut next, mut body_from) = (1, 0);
    // The text above a block of notes: their calls stand there.
    let mut above = 0..0;
    for &(from, to) in pages {
        let paged = paged && to - from <= PAGE_LINES;
        let mut page = Vec::new();
        let mut open: Option<Zone> = None;
        let mut ruled = None;
        let mut noted = false;
        for index in from..to {
            let line = &lines[index];
            if line.blank() {
                let finished = open.as_ref().is_some_and(|zone| zone.lines.last().is_some_and(|last| ended_sentence(lines[*last].body)));
                if finished || !paged && open.as_ref().is_some_and(|zone| zone.lines.len() > 1) {
                    page.extend(open.take());
                }
                continue;
            }
            // A rule repeats page to page, furniture but still the line above the notes: below a page's head.
            if rule.is_match(line.body) && (!masked[index] || index - from > 4) {
                ruled = Some(index);
                continue;
            }
            if masked[index] { continue; }
            // Past a page's rule a note may skip one the extractor lost, or a separate opinion's series start again.
            // Notes stand low on a page; high on one a number opens a paragraph.
            let low = !paged || (index - from) * 2 >= to - from;
            let label = note_label(line.body)
                .filter(|_| !(bracketed_paragraphs && line.body.starts_with('[')))
                .filter(|(number, _)| *number == next || (ruled.is_some() && *number == 1) || ((ruled.is_some() || (low && next > 1)) && *number > next && *number <= next + 2));
            let supported = label.is_some_and(|(number, end)| {
                let previous = (from..index).rev().find(|at| !masked[*at] && !lines[*at].blank());
                let clear = index == 0 || lines[index - 1].blank() || ruled.is_some() || open.is_some() || previous.is_none_or(|at| !lines[at].body.ends_with([',', ';']));
                let body = (body_from.max(from)..index).filter(|at| !masked[*at]).map(|at| lines[at].body).collect::<Vec<_>>();
                // A call an extractor lost: the number alone at a page's foot, or the next note's soon after it.
                let rest = line.body[end..].trim_start();
                let bare = !line.body[end..].starts_with(['.', ')', ':']);
                // A number with nothing under it is a page's; one a sentence runs on through is the sentence's.
                let nothing = rest.is_empty() && !(index + 1..to).any(|at| !lines[at].blank());
                let wrapped = ruled.is_none() && rest.starts_with(char::is_lowercase) && previous.is_some_and(|at| !ended_sentence(lines[at].body));
                // The text above this block of notes calls every note in it, not only the first.
                let listed: Vec<&str> = above.clone().filter(|at| !masked[*at]).map(|at| lines[at].body).collect();
                let called = calls(&body, number) || calls(&listed, number);
                // "12 Doc. 217." opens a note where a sentence has ended or a blank stands over it; a wrapped citation does neither.
                let set_apart = index > 0 && lines[index - 1].blank() && !opens_on_reporter(rest);
                let citation_wrapped = opens_on_volume(rest) && !called && !set_apart && !previous.is_some_and(|at| ended_sentence(lines[at].body));
                let clear = (clear || (bare && calls(&body, number))) && !nothing && !wrapped && !citation_wrapped;
                let labelled = label_form(line.body, end);
                let same_form = |zone: &Zone| zone.symbol.is_none() && zone.label.is_some_and(|(_, opened)| label_form(lines[zone.lines[0]].body, opened) == labelled);
                let continuing = open.as_ref().is_some_and(same_form) && called;
                // Directly under the note before it, of its form, nothing between them.
                let under = |zone: &Zone| same_form(zone) && zone.lines.last().is_some_and(|last| (last + 1..index).all(|at| masked[at] || lines[at].blank()));
                let adjacent = page.last().is_some_and(under) || open.as_ref().is_some_and(|zone| under(zone) && zone.lines.last().is_some_and(|last| ended_sentence(lines[*last].body)));
                let at_foot = paged && (index - from) * 20 >= (to - from) * 11;
                let next_follows = (index + 1..(index + 45).min(to)).any(|at| !masked[at] && note_label(lines[at].body).is_some_and(|(label, _)| label == number + 1));
                clear && (continuing || adjacent || ruled.is_some() || ((low || bare) && !line.body.starts_with('(') && called) || (bare && (at_foot || next_follows)))
            });
            // A symbol's note, past a rule or low on a page, whose symbol is called in the text above it.
            let symbol = symbol_label(line.body).filter(|symbol| {
                (ruled.is_some() || (index - from) * 10 >= (to - from) * 7)
                    && (from..index).any(|at| !masked[at] && lines[at].body.match_indices(*symbol).any(|(place, _)| place > 0 && lines[at].body[..place].ends_with(|c: char| c.is_alphanumeric() || matches!(c, '.' | ',' | ')' | '”' | '"'))))
            });
            if let Some(symbol) = symbol {
                page.extend(open.take());
                open = Some(Zone { lines: vec![index], label: Some((0, symbol.len())), symbol: Some(symbol.to_owned()) });
                body_from = index + 1;
                continue;
            }
            if supported {
                if open.is_none() && page.last().is_none_or(|zone: &Zone| zone.lines.last().is_some_and(|last| !(last + 1..index).all(|at| masked[at] || lines[at].blank()))) {
                    above = body_from..index;
                }
                page.extend(open.take());
                open = Some(Zone { lines: vec![index], label, symbol: None });
                next = label.map_or(next, |(number, _)| number) + 1;
                noted = true;
                body_from = index + 1;
            } else if let Some(zone) = &mut open {
                let finished = zone.lines.last().is_some_and(|last| ended_sentence(lines[*last].body));
                if (!paged && paragraph_marker(line.body).is_some()) || (finished && text_resumes(line.body)) {
                    page.extend(open.take());
                    ruled = None;
                    continue;
                }
                zone.lines.push(index);
                body_from = index + 1;
                if !paged && ended_sentence(line.body) && line.width() * 10 < width * 9 {
                    page.extend(open.take());
                }
            } else if !paged {
                ruled = None;
            }
        }
        page.extend(open.take());
        if !noted {
            if let Some(rule_line) = ruled.filter(|&line| line * 2 > from + to && next > 1) {
                let carried = (rule_line + 1..to).filter(|&at| !masked[at] && !lines[at].blank()).take_while(|&at| !text_resumes(lines[at].body)).collect::<Vec<_>>();
                if !carried.is_empty() { page.push(Zone { lines: carried, label: None, symbol: None }); }
            }
        }
        zones.push(page);
    }
    drop_strays(lines, &mut zones);
    fill_skips(lines, masked, pages, &mut zones, width);
    zones
}

/// A number with almost nothing under it is a page's, one in brackets a list's, unless the text before it calls it.
fn drop_strays(lines: &[Line<'_>], zones: &mut [Vec<Zone>]) {
    let noted = zones.iter().flatten().flat_map(|zone| zone.lines.iter().copied()).collect::<HashSet<_>>();
    let stray = |zone: &Zone| {
        let Some((number, _)) = zone.label.filter(|_| zone.symbol.is_none()) else { return false };
        let first = lines[zone.lines[0]].body;
        let bracket = first.chars().next().filter(|c| matches!(c, '(' | '['));
        let words: usize = zone.lines.iter().map(|at| lines[*at].body.split_whitespace().count()).sum();
        // "11 Id." and "20 JX37." say all a note need.
        let says_something = words >= 3 || (words == 2 && zone.lines.last().is_some_and(|last| ended_sentence(lines[*last].body)));
        if bracket.is_none() && says_something { return false; }
        let marked = bracket.map(|open| format!("{open}{number}{}", if open == '(' { ')' } else { ']' }));
        // "text [3] goes on" calls a note; "as (1) this and (2) that" counts.
        !(0..zone.lines[0]).filter(|at| !noted.contains(at)).map(|at| lines[at].body).any(|body| {
            call_in(body, number).is_some() || marked.as_deref().is_some_and(|mark| body.match_indices(mark).any(|(place, _)| place > 0 && (bracket == Some('[') || !body[..place].ends_with(char::is_whitespace))))
        })
    };
    for page in zones.iter_mut() {
        page.retain(|zone| !stray(zone));
    }
}

/// A note in the flow, its own lines: to a blank once it has said something, or a short line that ends a sentence.
fn own_lines(lines: &[Line<'_>], masked: &[bool], at: usize, limit: usize, width: usize) -> Vec<usize> {
    let mut own = vec![at];
    for index in at + 1..limit {
        let line = &lines[index];
        if line.blank() {
            if own.len() > 1 || ended_sentence(lines[at].body) { break; }
            continue;
        }
        if masked[index] { continue; }
        if paragraph_marker(line.body).is_some() { break; }
        own.push(index);
        if ended_sentence(line.body) && line.width() * 10 < width * 9 { break; }
    }
    own
}

/// A number the series skips, opening a line between the notes either side of it in their form: those two notes bound it,
/// so it needs no call of its own, which an extractor may have lost. It is a note unless it reads as the text's: a heading
/// says nothing further, a list item follows a comma, a wrapped line opens lower case, and two such lines leave it open.
fn fill_skips(lines: &[Line<'_>], masked: &[bool], pages: &[(usize, usize)], zones: &mut [Vec<Zone>], width: usize) {
    let held = zones.iter().flatten().filter(|zone| zone.symbol.is_none()).filter_map(|zone| Some((zone.lines[0], zone.label?))).collect::<Vec<_>>();
    let mut found = Vec::new();
    for pair in held.windows(2) {
        let ((first, (low, opened)), (last, (high, _))) = (pair[0], pair[1]);
        if high <= low + 1 { continue; }
        let shown = label_form(lines[first].body, opened);
        let mut candidates = BTreeMap::<u32, Vec<(usize, usize)>>::new();
        for index in first + 1..last {
            if masked[index] || lines[index].blank() { continue; }
            let Some((number, end)) = note_label(lines[index].body).filter(|(number, _)| (low + 1..high).contains(number)) else { continue };
            let rest = lines[index].body[end..].trim_start_matches(['.', ')', ':', ']', '/']).trim_start();
            let previous = (first..index).rev().find(|at| !masked[*at] && !lines[*at].blank());
            let says_more = ended_sentence(lines[index].body) || lines.get(index + 1).is_some_and(|line| !line.blank());
            // Between its neighbours "26 Dkt. 53" is a note; only a reporter's name after the number says otherwise.
            let wrapped = opens_on_reporter(rest) && !previous.is_some_and(|at| ended_sentence(lines[at].body));
            if label_form(lines[index].body, end) == shown && rest.chars().next().is_some_and(|c| !c.is_lowercase()) && says_more && !wrapped && !previous.is_some_and(|at| lines[at].body.ends_with([',', ';'])) {
                candidates.entry(number).or_default().push((index, end));
            }
        }
        found.extend(candidates.into_iter().filter(|(_, lines)| lines.len() == 1).map(|(number, lines)| (lines[0].0, (number, lines[0].1))));
    }
    // The last note has no neighbour past it: bare numbers climbing through its own lines after a finished line are the notes
    // it swallowed. A closed "2." may be a list it quotes.
    if let Some(zone) = zones.iter().flatten().rfind(|zone| zone.symbol.is_none() && zone.label.is_some()) {
        let (number, opened) = zone.label.unwrap_or_default();
        let mut due = number + 1;
        if label_form(lines[zone.lines[0]].body, opened) == (None, None) {
            for pair in zone.lines.windows(2) {
                let body = lines[pair[1]].body;
                if let Some((label, end)) = note_label(body).filter(|(label, end)| *label == due && label_form(body, *end) == (None, None) && body[*end..].starts_with(char::is_whitespace)) {
                    if ended_sentence(lines[pair[0]].body) && body[end..].trim_start().starts_with(|c: char| !c.is_lowercase()) {
                        found.push((pair[1], (label, end)));
                        due += 1;
                    }
                }
            }
        }
    }
    found.sort_unstable();
    for (index, label) in found.into_iter().rev() {
        let page = pages.partition_point(|&(from, _)| from <= index) - 1;
        let own = match zones[page].iter_mut().find(|zone| zone.lines.contains(&index)) {
            Some(zone) => {
                let at = zone.lines.iter().position(|line| *line == index).unwrap();
                zone.lines.split_off(at)
            }
            None => {
                let limit = zones[page].iter().map(|zone| zone.lines[0]).filter(|&start| start > index).min().unwrap_or(pages[page].1);
                own_lines(lines, masked, index, limit, width)
            }
        };
        zones[page].retain(|zone| !zone.lines.is_empty());
        let slot = zones[page].partition_point(|zone| zone.lines[0] < index);
        zones[page].insert(slot, Zone { lines: own, label: Some(label), symbol: None });
    }
}

fn notes_of(lines: &[Line<'_>], zones: &[Vec<Zone>], pages: &[(usize, usize)]) -> Vec<OpinionNote> {
    let in_zone = zones.iter().flatten().flat_map(|zone| zone.lines.iter().copied()).collect::<HashSet<_>>();
    let mut protected = BTreeMap::new();
    let laid = pages.iter().enumerate().map(|(page_index, &(from, to))| {
        let page_lines = (from..to).filter(|&at| !lines[at].blank()).map(|at| {
            let id = format!("l{at}");
            let body = lines[at].body;
            if body.bytes().any(|byte| byte.is_ascii_digit()) {
                let characters = ScalarText::new(body);
                let spans = cues::layout_protected_spans(body).into_iter()
                    .map(|span| PdfSourceSpan { start: characters.scalar(span.start), end: characters.scalar(span.end) })
                    .collect::<Vec<_>>();
                if !spans.is_empty() { protected.insert(id.clone(), spans); }
            }
            let note = in_zone.contains(&at);
            LaidLine {
                id, page_index, page_number: page_index as u32 + 1, source_index: at, reading_order: at, block_index: at,
                text: body.to_owned(), bbox: [0.0; 4], spans: Vec::new(), words: Vec::new(), detached_references: Vec::new(),
                ocr: None, exclude_from_body: false, ink: 0.0, weight: 0.0, cap_height: 0.0,
                suppress_footnote_label: !note, paragraph_label_candidate: false,
                note_region_mode: if note { "footnote".to_owned() } else { String::new() },
                region_id: String::new(), region_type: if note { "footnote" } else { "paragraph" }.to_owned(),
                source: "text".to_owned(),
            }
        }).collect();
        LaidPage {
            id: format!("page-{}", page_index + 1), index: page_index, number: page_index as u32 + 1, width: 0.0, height: 0.0,
            lines: page_lines, regions: Vec::new(), source: "text".to_owned(), text_quality: 1.0,
            printed_label: None, printed_label_source: None, printed_label_line_id: None,
        }
    }).collect::<Vec<_>>();
    let pairing = pair_footnotes(&laid, &protected, false);
    let place = |line_id: &str, start: usize, end: usize| -> Option<ScalarRange> {
        let line = &lines[line_id.strip_prefix('l')?.parse::<usize>().ok()?];
        let origin = line.body_range().start;
        let unit = |scalars: usize| line.body.chars().take(scalars).map(char::len_utf16).sum::<usize>();
        Some(ScalarRange { start: origin + unit(start), end: origin + unit(end) })
    };
    let references = pairing.pair_claims.iter().filter_map(|claim| {
        let line = claim.label_anchor.line_id.strip_prefix('l')?.parse::<usize>().ok()?;
        Some((line, claim.reference_anchors.iter()
            .filter_map(|anchor| place(&anchor.line_id, anchor.start, anchor.end)).collect::<Vec<_>>()))
    }).collect::<HashMap<_, _>>();
    let mut notes = Vec::<OpinionNote>::new();
    for (index, zone) in zones.iter().enumerate().flat_map(|(page, zones)| zones.iter().map(move |zone| (page, zone))) {
        let first = lines[zone.lines[0]].body_range();
        let last = lines[*zone.lines.last().unwrap()].body_range();
        // A symbol's call: the last of it in the page's text above the note.
        let symbol_call = zone.symbol.as_deref().and_then(|symbol| {
            (pages[index].0..zone.lines[0]).rev().find_map(|at| {
                let body = lines[at].body;
                let place = body.rmatch_indices(symbol).map(|(place, _)| place).find(|&place| place > 0)?;
                let start = lines[at].body_range().start + utf16_len(&body[..place]);
                Some(ScalarRange { start, end: start + utf16_len(symbol) })
            })
        });
        match zone.label {
            Some((number, _)) => {
                let mut called = symbol_call.into_iter().chain(references.get(&zone.lines[0]).cloned().unwrap_or_default()).collect::<Vec<_>>();
                if called.is_empty() && zone.symbol.is_none() {
                    called.extend((pages[index].0..zone.lines[0]).rev().filter(|at| !in_zone.contains(at)).find_map(|at| {
                        let start = lines[at].body_range().start + utf16_len(&lines[at].body[..call_in(lines[at].body, number)?]);
                        Some(ScalarRange { start, end: start + utf16_len(&number.to_string()) })
                    }));
                }
                notes.push(OpinionNote {
                    items: Vec::new(), label: zone.symbol.clone().unwrap_or_else(|| number.to_string()), start: first.start, end: last.end, ranges: vec![ScalarRange { start: first.start, end: last.end }],
                    references: called, page: None, called_outside: false, pins: Vec::new(),
                });
            }
            None => if let Some(note) = notes.last_mut() {
                note.end = last.end;
                note.ranges.push(ScalarRange { start: first.start, end: last.end });
            },
        }
    }
    notes
}

/// Where a line's columns open: past each run of three spaces.
fn column_starts(line: &str) -> Vec<usize> {
    static GAP: OnceLock<Regex> = OnceLock::new();
    GAP.get_or_init(|| Regex::new(r"\S[ \t]{3,}").unwrap()).find_iter(line.trim_end()).map(|gap| gap.end()).collect()
}

/// Tables a text has flattened: three lines or more whose columns stand together, at least three columns. Two are a
/// caption's or a signature's, and justified text aligns nowhere.
fn flat_tables(lines: &[Line<'_>], body: &[usize], from: usize) -> Vec<(usize, usize)> {
    let shared = |left: &[usize], right: &[usize]| left.iter().filter(|column| right.iter().any(|other| column.abs_diff(*other) <= 1)).count();
    let mut tables = Vec::new();
    let mut run: Vec<(usize, Vec<usize>)> = Vec::new();
    for &at in body.iter().filter(|&&at| at >= from) {
        let columns = column_starts(lines[at].text);
        let continues = run.last().is_some_and(|last| at <= last.0 + 2 && shared(&last.1, &columns) >= 2);
        if !continues {
            if run.len() >= 3 { tables.push((run[0].0, run[run.len() - 1].0)); }
            run.clear();
        }
        if columns.len() >= 2 {
            run.push((at, columns));
        }
    }
    if run.len() >= 3 { tables.push((run[0].0, run[run.len() - 1].0)); }
    tables
}

/// A line of a table of contents or authorities: its dot leaders and page.
pub(super) fn contents_line(body: &str) -> bool {
    static LEADERS: OnceLock<Regex> = OnceLock::new();
    static TIGHT: OnceLock<Regex> = OnceLock::new();
    // A paragraph may end on a reporter's page, "99 S.Ct.2210": a contents line without its leaders is a short one.
    LEADERS.get_or_init(|| Regex::new(r"(?:\.{4,}|(?:\. ){4,}|(?:_ ){4,})\s*(?:passim|\d{1,4}(?:[,\s]+\d{1,4})*)\s*$").unwrap()).is_match(body)
        || (body.len() <= 200 && TIGHT.get_or_init(|| Regex::new(r"[\p{L})”]\.{1,3}\s?\d{3,4}\s*$").unwrap()).is_match(body))
}

/// A line no note runs on through: a paragraph's own mark, a separate opinion's author, or a section's title.
fn text_resumes(body: &str) -> bool {
    says_paragraph(body) || front_role(body) == Some("author") || section_line(body)
}

/// A line that opens a paragraph of its own whatever came before: a bullet, or a voice in an exchange.
fn opens_alone(body: &str) -> bool {
    static OPENS: OnceLock<Regex> = OnceLock::new();
    OPENS.get_or_init(|| Regex::new(r"^(?:[•·▪◦]\s|Q\.\s|A\.\s|THE COURT:|THE WITNESS:|M(?:R|S|RS)\.\s+\p{Lu}[\p{Lu}'’-]+:)").unwrap()).is_match(body)
}

/// A numbered line's words past its number.
pub(super) fn past_marker(body: &str) -> &str {
    static MARK: OnceLock<Regex> = OnceLock::new();
    let marker = MARK.get_or_init(|| Regex::new(r"^(?:[\[{(]?¶¶?\s*\d{1,4}\.?[\]})]?|[Tf] \d{1,4}\s|\d{1,4}\]\s|\{\s*\d{1,4}\s*\}?|\[\d{1,4}[\]jJ]|\d{1,3}[.)](?:\s|$|\p{Lu}))\s*").unwrap());
    marker.find(body).map_or(body, |found| &body[found.end()..])
}

/// A paragraph's printed number and its family: 1 "¶ 12" (or the "12]" a scanner leaves of "[¶ 12]"), 2 "{12}", 3 "[12]", 4 "12.".
pub(super) fn paragraph_marker(body: &str) -> Option<(u32, usize)> {
    static MARK: OnceLock<Regex> = OnceLock::new();
    let body = body.trim_start_matches('•');
    // A headnote's mark may stand first: "[5] [¶ 17] In McAdoo".
    let body = body.split_once(']').filter(|(mark, rest)| mark.starts_with('[') && mark[1..].bytes().all(|byte| byte.is_ascii_digit()) && rest.trim_start().trim_start_matches(['[', '{']).starts_with('¶')).map_or(body, |(_, rest)| rest.trim_start());
    let capture = MARK.get_or_init(|| Regex::new(r"^(?:[\[{(]?¶¶?\s*(\d{1,4})[\]})]?|\[?I?[Tf] ?(\d{1,4})\]?\s|(\d{1,4})\]\s|\{\s*(\d{1,4})\s*\}?|\[ ?(\d{1,4})[\]jJ]|(\d{1,3})[.)](?:\s|$|\p{Lu}))").unwrap()).captures(body)?;
    let group = (1..=6).find(|&group| capture.get(group).is_some())?;
    let bare = group == 2 || group == 1 && body.starts_with('¶');
    Some((capture[group].parse().ok()?, match group { _ if bare => 5, 1 | 3 => 1, 4 => 2, 5 => 3, _ => 4 }))
}

/// A mark that names itself a paragraph's: "¶ 12", "[¶ 12]", "{12}".
pub(super) fn says_paragraph(body: &str) -> bool {
    paragraph_marker(body).is_some_and(|(_, family)| matches!(family, 1 | 2 | 5))
}

/// The longest run of numbers that only climbs.
pub(super) fn climbing_run(members: &[(usize, u32)]) -> Vec<(usize, u32)> {
    let mut tails = Vec::<usize>::new();
    let mut parent = vec![None; members.len()];
    for at in 0..members.len() {
        let place = tails.partition_point(|&tail| members[tail].1 < members[at].1);
        parent[at] = place.checked_sub(1).map(|previous| tails[previous]);
        if place == tails.len() { tails.push(at); } else { tails[place] = at; }
    }
    let mut run = Vec::with_capacity(tails.len());
    let mut at = tails.last().copied();
    while let Some(index) = at {
        run.push(members[index]);
        at = parent[index];
    }
    run.reverse();
    run
}

/// The paragraph numbers that are the opinion's own, of one family. "¶" and "{n}" say what they are and are kept as
/// printed, whatever their order; "[n]" and "n." must run from 1 for `least` paragraphs, and may start again or repeat.
pub(super) fn own_numbers<'a>(openings: impl Iterator<Item = (usize, &'a str)>, least: usize) -> HashMap<usize, u32> {
    let mut families = [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for (index, opening) in openings {
        if let Some((number, family)) = paragraph_marker(opening) {
            families[family - 1].push((index, number));
        }
    }
    let mut chosen = Vec::new();
    for (family, members) in families.iter().enumerate() {
        let mut found = Vec::new();
        if family < 2 {
            if members.len() >= 2 { found = members.clone(); }
        } else if family == 4 {
            // A wrapped "See id. ¶ 23" opens a line too: paragraph numbers are the ones that climb.
            let climbing = climbing_run(members);
            if climbing.len() >= least.min(3) { found = climbing; }
        } else {
            let (mut run, mut last) = (Vec::new(), 0);
            for &(index, number) in members {
                if number == 1 || number < last || number - last > 3 {
                    if run.len() >= least { found.append(&mut run); }
                    run.clear();
                }
                if number == 1 || !run.is_empty() {
                    run.push((index, number));
                    last = number;
                }
            }
            if run.len() >= least { found.append(&mut run); }
        }
        let by_ones = found.windows(2).filter(|pair| pair[1].1 <= pair[0].1 + 1).count();
        if family >= 2 && by_ones * 2 < found.len().saturating_sub(1) {
            found.clear();
        }
        if found.len() > chosen.len() {
            chosen = found;
        }
    }
    chosen.into_iter().collect()
}

/// An overprinted line reads each letter twice: "OORRDDEERR" is "ORDER".
fn undoubled(line: &str) -> Option<String> {
    let letters = line.chars().filter(|c| !c.is_whitespace()).collect::<Vec<_>>();
    (letters.len() >= 6 && letters.len() % 2 == 0 && letters.chunks(2).all(|pair| pair[0] == pair[1])).then(|| {
        let mut out = String::with_capacity(line.len() / 2);
        let mut second = false;
        for c in line.chars() {
            if c.is_whitespace() { out.push(c); continue; }
            if !second { out.push(c); }
            second = !second;
        }
        out
    })
}

/// A PDF whose font maps its letters 29 places down writes "7KH\x03FRXUW" for "The court", its spaces \x03. Shifted, no letter
/// falls past "a": a line with such a space and hardly a letter past it is read back, character for character, so every offset
/// holds; so is a line of one word between shifted lines, which has no space to show it. Justified lines keep plain spaces too.
pub(super) fn unshifted(text: &str) -> String {
    const SHIFT: u32 = 29;
    let text = &text.split_inclusive('\n').map(|line| undoubled(line).unwrap_or_else(|| line.to_owned())).collect::<String>();
    if !text.contains('\u{3}') {
        return text.to_owned();
    }
    let past_shift = |line: &str| line.chars().filter(|c| ('b'..='z').contains(c)).count();
    let lines = text.split_inclusive('\n').collect::<Vec<_>>();
    let marked = lines.iter().map(|line| line.contains('\u{3}') && past_shift(line) * 20 < line.chars().filter(|c| !c.is_whitespace()).count()).collect::<Vec<_>>();
    let beside = |index: usize, step: isize| {
        let mut at = index as isize + step;
        while let Some(line) = usize::try_from(at).ok().and_then(|at| lines.get(at)) {
            if !line.trim().is_empty() {
                return marked[at as usize];
            }
            at += step;
        }
        false
    };
    lines.iter().enumerate().map(|(index, line)| {
        let lone = !marked[index] && past_shift(line) == 0 && line.chars().any(|c| c.is_ascii_uppercase()) && beside(index, -1) && beside(index, 1);
        let from = if marked[index] || lone { Some(0) } else { line.find('\u{3}') };
        let Some(from) = from else { return (*line).to_owned() };
        let read = line[from..].chars().map(|c| match c as u32 {
            code @ 0x03..=0x61 if !matches!(c, '\n' | '\r' | '\t' | '\u{c}' | ' ') => char::from_u32(code + SHIFT).unwrap_or(c),
            _ => c,
        }).collect::<String>();
        format!("{}{read}", &line[..from])
    }).collect()
}

/// The text read: pages and furniture first, then the notes, then the head matter down to the first line of running
/// text, and what is left into headings and paragraphs.
pub(super) fn read(text: &str, page_count: Option<u32>) -> Reading {
    let mut lines = split_lines(text);
    let margins = margin_numbers(&mut lines);
    let end = utf16_len(text);
    if lines.is_empty() {
        return Reading::default();
    }
    let mut widths = lines.iter().filter(|line| !line.blank()).map(Line::width).collect::<Vec<_>>();
    widths.sort_unstable();
    let width = widths.get(widths.len() * 9 / 10).copied().unwrap_or(0);
    let (mut chains, running) = page_chains(&lines, width, page_count);
    let mut pages = pages_of(&lines, end, &mut chains, margins);
    let mut masked = vec![false; lines.len()];
    let mut furniture = Vec::new();
    // An extractor can drop a text's page numbers and leave a stray: a number alone in the middle of a line, within
    // the record's pages. It says which page ends there and no more.
    if chains.is_empty() {
        let (shaped, stray) = (lines.iter().enumerate(), page_count.unwrap_or(0));
        for (at, line) in shaped {
            let (form, numbers) = shape(line.body);
            let framed = !form.chars().any(char::is_alphabetic) && page_shape(&form).is_some();
            if framed && numbers.len() == 1 && line.indent * 3 >= width && (2..=stray).contains(&numbers[0].0) {
                let range = line.range();
                pages.push(read_page(numbers[0].0.to_string(), "slip".to_owned(), range.start, range.end, Some(range), "stray"));
                masked[at] = true;
                furniture.push(range);
            }
        }
    }
    for at in chains.iter().flat_map(|chain| chain.members.iter().map(|member| member.0)).chain(running) {
        if !masked[at] {
            masked[at] = true;
            furniture.push(lines[at].range());
        }
    }
    furniture.extend(lines.iter().filter_map(|line| line.margin));
    furniture.sort_by_key(|range| range.start);
    // Pages as lines, in the numbering with the most of them.
    let series = chains.first().map_or("sheet", |chain| chain.series);
    let mut bounds = pages.iter().filter(|page| page.series == series)
        .map(|page| lines.partition_point(|line| line.start < page.range.start)).collect::<Vec<_>>();
    if bounds.first() != Some(&0) { bounds.insert(0, 0); }
    let spans = bounds.iter().enumerate()
        .map(|(index, &from)| (from, bounds.get(index + 1).copied().unwrap_or(lines.len())))
        .collect::<Vec<_>>();
    // "[1]" through the text's length, as paragraphs are numbered and a page's notes are not.
    let bracketed = lines.iter().enumerate().filter(|(at, line)| !masked[*at] && paragraph_marker(line.body).is_some_and(|(_, family)| family == 3))
        .map(|(at, line)| (at, line.body)).collect::<Vec<_>>();
    let bracket_run = own_numbers(bracketed.iter().copied(), 5);
    let bracketed_paragraphs = bracket_run.keys().min().zip(bracket_run.keys().max()).is_some_and(|(first, last)| (last - first) * 3 >= lines.len());
    let zones = note_zones(&lines, &masked, &spans, spans.len() > 1, width, bracketed_paragraphs);
    let notes = notes_of(&lines, &zones, &spans);
    for zone in zones.iter().flatten() {
        for &at in &zone.lines { masked[at] = true; }
    }
    // "12." is a paragraph's number only where paragraphs sit, not set in like a list.
    let openings = lines.iter().enumerate().filter(|(index, line)| !masked[*index] && !line.blank()
        && (paragraph_marker(line.body).is_none_or(|(_, family)| family != 4) || line.indent * 3 <= width));
    let numbered = own_numbers(openings.map(|(index, line)| (index, line.body)), 5);
    let body = lines.iter().enumerate().filter(|(index, line)| !masked[*index] && !line.blank()).map(|(index, _)| index).collect::<Vec<_>>();
    let followed = body.windows(2).filter(|pair| pair[1] > pair[0] + 1).count();
    let spaced = followed * 5 >= body.len().max(1) * 3;
    let mut indents = HashMap::<usize, usize>::new();
    for &at in &body { *indents.entry(lines[at].indent).or_default() += 1; }
    let base = indents.into_iter().max_by_key(|&(indent, count)| (count, std::cmp::Reverse(indent))).map_or(0, |pair| pair.0);
    let centered = |line: &Line| line.indent > base + 2 && line.width() + line.indent.saturating_sub(4) >= width.saturating_sub(10) && line.indent + line.width() <= width + line.indent.min(12) + 8;

    // The head matter runs to the first line of running text.
    let opens_text = |at: usize| {
        let line = &lines[at];
        numbered.contains_key(&at) || line.body.to_lowercase().starts_with("syllabus") || (line.width() * 10 >= width * 7 && line.body.split_whitespace().count() >= 8
            && !caption_line(line.body) && line.body.chars().filter(|c| c.is_lowercase()).count() * 2 > line.body.len())
    };
    let head_end = body.iter().copied().find(|&at| opens_text(at)).unwrap_or(0);
    let mut front = Vec::new();
    // A caption's columns each say their own: "JAMES GUTHRIE,      )   Filed: October 15, 2025".
    static COLUMN: OnceLock<Regex> = OnceLock::new();
    let column = COLUMN.get_or_init(|| Regex::new(r"[ \t]{3,}\)?[ \t]*|\)[ \t]+").unwrap());
    for &at in body.iter().take_while(|&&at| at < head_end) {
        let line = lines[at].body;
        let origin = lines[at].body_range().start;
        let mut from = 0;
        for stop in column.find_iter(line).map(|found| (found.start(), found.end())).chain(std::iter::once((line.len(), line.len()))) {
            let piece = line[from..stop.0].trim_matches(javascript_whitespace);
            if let Some(role) = (!piece.is_empty()).then(|| front_role(piece)).flatten() {
                let start = origin + utf16_len(&line[..from]);
                front.push(FrontLine { role: role.to_owned(), text: super::squeezed(piece), range: Some(ScalarRange { start, end: start + utf16_len(piece) }) });
            }
            from = stop.1;
        }
    }

    let table_runs = flat_tables(&lines, &body, head_end);
    let tables = table_runs.iter().map(|&(first, last)| OpinionTable { start: lines[first].start, end: lines[last].body_range().end, page: None }).collect();
    let in_table = |at: usize| table_runs.iter().any(|&(first, last)| first <= at && at <= last);

    let mut paragraphs = Vec::<OpinionParagraph>::new();
    let mut headings = Vec::<OpinionHeading>::new();
    let mut previous: Option<usize> = None;
    // A number on a section's title belongs to the text under it: "¶3   I. BACKGROUND".
    let mut carried: Option<u32> = None;
    for (position, &at) in body.iter().enumerate() {
        let line = &lines[at];
        let range = line.body_range();
        if in_table(at) || !line.body.chars().any(|c| c.is_alphanumeric() || c == '*') {
            continue;
        }
        if contents_line(line.body) {
            front.push(FrontLine { role: "contents".to_owned(), text: super::squeezed(line.body), range: Some(range) });
            continue;
        }
        if let Some(&number) = numbered.get(&at) {
            let title = past_marker(line.body);
            if !title.is_empty() && !title.chars().any(char::is_lowercase) && heading_line(title, true) {
                let start = range.end - utf16_len(title);
                headings.push(OpinionHeading { text: String::new(), level: 0, start, end: range.end, page: None, source: "layout", line: at, pins: Vec::new() });
                // With a number of its own on the next line, it keeps this one where it stands.
                if body.get(position + 1).is_some_and(|next| numbered.contains_key(next)) {
                    paragraphs.push(OpinionParagraph { continues: false, item: None, number: Some(number.to_string()), start: range.start, end: start, page: None, quote: false, part: None, speaker: None, pins: Vec::new() });
                } else {
                    carried = Some(number);
                }
                previous = None;
                continue;
            }
        }
        let (before, after) = (previous, body.get(position + 1).copied());
        let stands = before.is_none_or(|p| p + 1 < at || lines[p].width() * 10 < width * 7) && after.is_none_or(|next| next > at + 1 || lines[next].indent != line.indent || spaced);
        // A Roman numeral after a finished sentence opens a section, however tightly it is set.
        let section = before.is_none_or(|p| ended_sentence(lines[p].body)) && section_line(line.body);
        let in_head = at < head_end;
        // A short line that follows a finished sentence and is none itself, in a text that has lost its spacing.
        let isolated = line.width() * 10 < width * 6 && before.is_none_or(|p| p + 1 < at || ended_sentence(lines[p].body)) && !ended_sentence(line.body);
        let apart = centered(line) || isolated || section;
        if !numbered.contains_key(&at) && (stands || isolated || section || centered(line)) && heading_line(line.body, apart) && (!in_head || title_line(line.body)) {
            match headings.last_mut().filter(|heading| before == Some(heading.line) && centered(line)) {
                Some(heading) => { heading.end = range.end; heading.line = at; }
                None => {
                    // A title's first line, broken at a comma, read as text.
                    let first = before.filter(|&p| lines[p].body.ends_with(',') && lines[p].indent == line.indent)
                        .filter(|&p| paragraphs.last().is_some_and(|last| last.start == lines[p].body_range().start && last.end == lines[p].body_range().end));
                    let start = if first.is_some() { paragraphs.pop().map_or(range.start, |last| last.start) } else { range.start };
                    headings.push(OpinionHeading { text: String::new(), level: 0, start, end: range.end, page: None, source: "layout", line: at, pins: Vec::new() });
                }
            }
            previous = None;
            continue;
        }
        if in_head {
            previous = Some(at);
            continue;
        }
        let number = numbered.get(&at).copied().or_else(|| carried.take());
        let opens = number.is_some() || opens_alone(line.body) || before.is_none_or(|p| {
            let (last, gap) = (&lines[p], at > p + 1);
            let ended = ended_sentence(last.body);
            let short = last.width() * 10 < width * 8;
            let page_turn = lines[p..at].iter().any(|between| between.fed) || masked[p + 1..at].iter().any(|flag| *flag);
            if page_turn && !ended { return false; }
            let indented = line.indent > base + 1 && last.indent <= base + 1;
            (indented && (ended || short)) || (short && ended && !line.body.starts_with(char::is_lowercase))
                || (gap && !spaced && !page_turn)
        });
        let quote = line.indent >= base + 4 && !centered(line) && line.width() <= width;
        match paragraphs.last_mut().filter(|_| !opens) {
            Some(paragraph) => {
                paragraph.end = range.end;
                paragraph.quote &= quote;
            }
            None => paragraphs.push(OpinionParagraph { continues: false, item: None, number: number.map(|number| number.to_string()), start: range.start, end: range.end, page: None, quote: quote && number.is_none(), part: None, speaker: None, pins: Vec::new() }),
        }
        previous = Some(at);
    }
    // A title with no text under it is a name at the foot of the opinion.
    let last_text = paragraphs.last().map_or(0, |paragraph| paragraph.end);
    headings.retain(|heading| heading.start < last_text);
    Reading { pages, paragraphs, notes, headings, furniture, front, tables }
}

pub(super) fn ended_sentence(body: &str) -> bool {
    body.trim_end_matches(['"', '”', '’', '\'', ')', ']']).ends_with(['.', ':', ';', '?', '!'])
}

/// A block of text that is finished, a note's call after its last stop included: "... (Vernon Supp. 2002).\n 5".
pub(super) fn closed_text(body: &str) -> bool {
    let body = body.trim_end();
    ended_sentence(body) || ended_sentence(body.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end())
}
