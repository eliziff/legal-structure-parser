//! Numbers recognition misread, restored from the numbering run they stand in: note labels ("10
//! Id." read "0 Id.", ":IO." for 30, "2s" for 28; ported from the Open Access Journals Database's
//! `eol_label_dropout.py`) and paragraph and item numbers ("[11" for "[1]", "[131" for "[13]",
//! "[20j", "(ci" for "(c)", "7,Although").
//!
//! One reading serves both. Every line opening with a marker joins its family's chain; one a
//! neighbour at one more or one less supports within a window is supported. Between supported a
//! and b that start within a line's height of each other, the recognized lines starting there whose
//! opening token reads, however misread, as the family's marker carry a + 1 to b - 1 in order, each
//! taking the value it reads as best (the best order-keeping match of lines to values). Printed text
//! is exact and never touched; running heads and feet join no chain.
//!
//! Notes: a page's first two and last line never count as labels, so running heads and folios,
//! which also climb by one a page, forge no chain. Paragraphs: a lone line in a gap of one that kept
//! the family's bracket carries that value however its digits read, a run's first supported marker
//! is supported from below by its count's start, and a line two runs read takes the number it reads
//! as best, and none when they read it alike.

use legal_structure_model::layout::{Line, Page};
use legal_structure_model::NumberingMarker;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const X0_LOW: f64 = -0.5;
const X0_HIGH: f64 = 1.0;

/// Whether recognition read the line, the only reading that drops or misreads digits: our own
/// recognition, or a scan's invisible recognition layer. Printed glyphs are exact.
fn recognized_line(line: &Line) -> bool {
    line.source == "ocr"
        || line
            .spans
            .iter()
            .any(|span| span.flags & legal_structure_model::layout::INVISIBLE != 0)
}

/// The lines `keep` admits in print order, as (page slot, line slot).
fn print_order(pages: &[Page], keep: impl Fn(&Line) -> bool) -> Vec<(usize, usize)> {
    let mut order = Vec::new();
    let mut slots = (0..pages.len()).collect::<Vec<_>>();
    slots.sort_by_key(|&slot| pages[slot].index);
    for page in slots {
        let mut lines = (0..pages[page].lines.len())
            .filter(|&line| keep(&pages[page].lines[line]))
            .collect::<Vec<_>>();
        lines.sort_by_key(|&line| pages[page].lines[line].reading_order);
        order.extend(lines.iter().map(|&line| (page, line)));
    }
    order
}

/// The markers a neighbour at one more or one less supports, within `lines` lines and
/// `candidates` markers of it.
fn supported(entries: &[(usize, u32)], lines: usize, candidates: usize) -> Vec<(usize, u32)> {
    entries
        .iter()
        .enumerate()
        .filter(|(index, (position, value))| {
            let low = index.saturating_sub(candidates);
            let high = (index + candidates + 1).min(entries.len());
            entries[low..high].iter().any(|(other, other_value)| {
                other.abs_diff(*position) <= lines
                    && ((other < position && *other_value + 1 == *value)
                        || (other > position && *other_value == value + 1))
            })
        })
        .map(|(_, entry)| *entry)
        .collect()
}

/// Whether a line starts where its neighbouring markers start, within a line's height.
fn aligned(line: &Line, neighbours: &[&Line]) -> bool {
    let neighbour = neighbours.iter().map(|other| other.bbox[0]).fold(f64::INFINITY, f64::min);
    let (height, delta) = (line.bbox[3] - line.bbox[1], line.bbox[0] - neighbour);
    height > 0.0 && (X0_LOW * height..=X0_HIGH * height).contains(&delta)
}

/// One way a misread marker reads: its numeral, whether it printed exactly the family's marker,
/// and whether it kept the family's own opening bracket.
struct Reading {
    numeral: String,
    exact: bool,
    shaped: bool,
}

/// How well a misread marker reads as the numeral `wanted`: exactly, by a reading of its marks,
/// with one digit misread (of several, unless `single` lets a lone digit be misread too), or with
/// its leading digits lost.
fn fit(readings: &[Reading], wanted: &str, single: bool) -> u32 {
    readings
        .iter()
        .map(|Reading { numeral: reading, exact, .. }| {
            if reading == wanted {
                if *exact { 4 } else { 3 }
            } else if (single || wanted.chars().count() > 1)
                && reading.chars().count() == wanted.chars().count()
                && reading.chars().zip(wanted.chars()).filter(|(a, b)| a != b).count() == 1
            {
                2
            } else if wanted.len() > reading.len() && wanted.ends_with(reading.as_str()) && !reading.contains('.') {
                2
            } else {
                0
            }
        })
        .max()
        .unwrap_or(0)
}

/// A line of a gap: where it lies in the print order, where its marker lies in its characters, and
/// the numerals the marker may read as.
struct Candidate {
    position: usize,
    start: usize,
    end: usize,
    readings: Vec<Reading>,
}

/// The lines between two supported markers and the markers they may carry, each as printed and as
/// its numeral; `lone` lets a lone line that kept the family's bracket carry a gap of one.
struct Gap {
    lines: Vec<Candidate>,
    values: Vec<(String, String)>,
    lone: bool,
}

/// Each line a gap reads a marker on, by the best order-keeping match of its lines to its values:
/// (position, start, end, marker, fit).
fn claims(gap: &Gap, single: bool) -> Vec<(usize, usize, usize, String, u32)> {
    let lone = gap.lone && gap.lines.len() == 1 && gap.values.len() == 1;
    let fits = |x: usize, y: usize| {
        let found = fit(&gap.lines[x].readings, &gap.values[y].1, single);
        let shaped = gap.lines[x]
            .readings
            .iter()
            .any(|reading| reading.shaped && reading.numeral.chars().count() <= gap.values[y].1.chars().count());
        if found == 0 && lone && shaped { 1 } else { found }
    };
    let (n, m) = (gap.lines.len(), gap.values.len());
    let mut score = vec![vec![0u32; m + 1]; n + 1];
    for x in 1..=n {
        for y in 1..=m {
            let found = fits(x - 1, y - 1);
            let take = if found > 0 { score[x - 1][y - 1] + found } else { 0 };
            score[x][y] = score[x - 1][y].max(score[x][y - 1]).max(take);
        }
    }
    let mut found = Vec::new();
    let (mut x, mut y) = (n, m);
    while x > 0 && y > 0 {
        let fit = fits(x - 1, y - 1);
        if fit > 0 && score[x][y] == score[x - 1][y - 1] + fit {
            let line = &gap.lines[x - 1];
            found.push((line.position, line.start, line.end, gap.values[y - 1].0.clone(), fit));
            x -= 1;
            y -= 1;
        } else if score[x][y] == score[x - 1][y] {
            x -= 1;
        } else {
            y -= 1;
        }
    }
    found
}

/// Write `label` over a line's characters `start..end`: the words after it move with it, and a word
/// holding part of it ends where it ends.
fn replace_label(line: &mut Line, start: usize, end: usize, label: &str) {
    let (length, written) = (end - start, label.chars().count());
    let rest = line.text.chars().skip(end).collect::<String>();
    line.text = line.text.chars().take(start).chain(label.chars()).chain(rest.chars()).collect();
    let new_end = start + written;
    let mut words: Vec<legal_structure_model::layout::Word> = Vec::with_capacity(line.words.len());
    for mut word in line.words.drain(..) {
        if word.end > start {
            word.start = if word.start <= start {
                word.start
            } else if word.start >= end {
                word.start + written - length
            } else {
                new_end
            };
            word.end = if word.end >= end { word.end + written - length } else { new_end };
        }
        match words.last_mut() {
            Some(prior) if word.start >= word.end || word.start < prior.end => {
                prior.end = prior.end.max(word.end);
                prior.bbox = legal_structure_model::layout::union_bbox([prior.bbox, word.bbox]);
            }
            _ if word.start >= word.end => {}
            _ => words.push(word),
        }
    }
    for word in &mut words {
        word.text = line.text.chars().skip(word.start).take(word.end - word.start).collect();
    }
    line.words = words;
}

/// Read every gap's claims, settle a line two gaps read (the better fit, none on a tie), and write
/// the markers that change a line; `spaced` keeps a space after a marker that ran into the next
/// word. Returns how many lines it repaired.
fn repair(pages: &mut [Page], order: &[(usize, usize)], gaps: &[Gap], single: bool, spaced: bool) -> usize {
    let mut best = HashMap::<usize, (usize, usize, String, u32, bool)>::new();
    for (position, start, end, marker, found) in gaps.iter().flat_map(|gap| claims(gap, single)) {
        match best.get_mut(&position) {
            Some(held) if held.3 > found => {}
            Some(held) if held.3 == found => held.4 |= held.2 != marker,
            _ => {
                best.insert(position, (start, end, marker, found, false));
            }
        }
    }
    let mut repaired = 0;
    for (position, (start, end, marker, _, tied)) in best {
        let (page, line) = order[position];
        let line = &mut pages[page].lines[line];
        if tied || line.text.chars().skip(start).take(end - start).collect::<String>() == marker {
            continue;
        }
        let joined = spaced && line.text.chars().nth(end).is_some_and(|c| !c.is_whitespace());
        replace_label(line, start, end, &if joined { format!("{marker} ") } else { marker });
        repaired += 1;
    }
    repaired
}

// Note labels.

const NOTE_WINDOW_LINES: usize = 150;
const NOTE_WINDOW_CANDIDATES: usize = 12;
const MAX_NOTE_VALUE: u32 = 899;

fn note_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d{1,3})(?:[\s.).\u{b0}]|$)").expect("label regex"))
}

/// The note label a line may open with however recognition misread it: up to four digits or marks
/// standing for digits, after at most two stray marks and before a stop or a bracket and a space.
fn misread_note_label(text: &str) -> Option<(usize, usize, Vec<Reading>)> {
    let characters = text.chars().collect::<Vec<_>>();
    let lead = characters.iter().take_while(|c| c.is_whitespace()).count();
    let mut at = lead;
    while at < lead + 2 && characters.get(at).is_some_and(|c| ":;'\"`,.(".contains(*c)) {
        at += 1;
    }
    let start = at;
    let mut readings = vec![String::new()];
    let mut digits = 0;
    while at < characters.len() && at - start < 4 {
        let choices = match characters[at] {
            c @ '0'..='9' => {
                digits += 1;
                vec![c]
            }
            'I' | 'l' | '|' | 'i' | '\'' | '\u{2019}' | '`' | '!' => vec!['1'],
            'O' | 'o' | '\u{b0}' => vec!['0'],
            'J' | 'j' => vec!['3'],
            'S' | 's' => vec!['5', '8'],
            'B' => vec!['8'],
            'Z' | 'z' => vec!['2'],
            'g' => vec!['9'],
            'G' | 'b' => vec!['6'],
            _ => break,
        };
        readings = readings
            .iter()
            .flat_map(|reading| choices.iter().map(move |choice| format!("{reading}{choice}")))
            .collect();
        at += 1;
    }
    let end = at;
    if end == start {
        return None;
    }
    // A word of marks alone ("Is", "lo") reads as a label only when it is two marks or more and
    // stands alone with its stop.
    if digits == 0 && (end - start < 2 || !characters.get(end).is_some_and(|c| ".)".contains(*c))) {
        return None;
    }
    let mut after = end;
    if characters.get(after).is_some_and(|c| ".)".contains(*c)) {
        after += 1;
    }
    if !characters.get(after).is_none_or(|c| c.is_whitespace()) {
        return None;
    }
    let exact = digits == end - start;
    Some((start, end, readings.into_iter().map(|numeral| Reading { numeral, exact, shaped: false }).collect()))
}

/// Restore the note labels recognition dropped digits of or misread within a run of them; returns
/// how many lines it repaired.
pub fn repair_label_dropouts(pages: &mut [Page]) -> usize {
    let order = print_order(pages, |line| !line.text.trim().is_empty());
    let line = |position: usize| {
        let (page, line) = order[position];
        &pages[page].lines[line]
    };
    // A page's first two and last line.
    let mut edges = HashSet::new();
    for (position, &(page, _)) in order.iter().enumerate() {
        if position < 2 || order[position - 1].0 != page || order[position - 2].0 != page {
            edges.insert(position);
        }
        if order.get(position + 1).is_none_or(|next| next.0 != page) {
            edges.insert(position);
        }
    }
    let entries = (0..order.len())
        .filter(|position| !edges.contains(position))
        .filter_map(|position| {
            let value = note_label().captures(line(position).text.trim())?[1].parse::<u32>().ok()?;
            (value > 0 && value <= MAX_NOTE_VALUE).then_some((position, value))
        })
        .collect::<Vec<_>>();
    let supported = supported(&entries, NOTE_WINDOW_LINES, NOTE_WINDOW_CANDIDATES);
    let mut gaps = Vec::new();
    for pair in supported.windows(2) {
        let ((before, value), (after, next)) = (pair[0], pair[1]);
        if next < value + 2 || after <= before + 1 || after - before > NOTE_WINDOW_LINES || next - value > 61 {
            continue;
        }
        // A page's edges and its running heads and folios are no labels: a page's folio "2" between
        // notes 11 and 13 would otherwise read as 12.
        let lines = (before + 1..after)
            .filter(|position| {
                let candidate = line(*position);
                !edges.contains(position)
                    && !matches!(candidate.region_type.as_str(), "header" | "footer")
                    && recognized_line(candidate)
                    && aligned(candidate, &[line(before), line(after)])
            })
            .filter_map(|position| {
                let (start, end, readings) = misread_note_label(&line(position).text)?;
                Some(Candidate { position, start, end, readings })
            })
            .collect::<Vec<_>>();
        let values = (value + 1..next).map(|v| (v.to_string(), v.to_string())).collect();
        gaps.push(Gap { lines, values, lone: false });
    }
    repair(pages, &order, &gaps, true, false)
}

// Paragraph and item numbers.

const PARAGRAPH_WINDOW_LINES: usize = 300;
const PARAGRAPH_WINDOW_CANDIDATES: usize = 12;
const GAP_LINES: usize = 400;
const GAP_VALUES: u32 = 30;

/// A family as its markers print: the wrapping, the count, and the numbers before the last.
type Family = (String, Vec<u32>);

/// The wrapping a marker token prints: "[]", "()", ")", "." or none.
fn wrap_of(token: &str) -> &'static str {
    if token.starts_with('[') && token.ends_with(']') {
        "[]"
    } else if token.starts_with('(') && token.ends_with(')') {
        "()"
    } else if token.ends_with(')') {
        ")"
    } else if token.ends_with('.') {
        "."
    } else {
        ""
    }
}

/// A family's marker for `value` as the family prints it: its wrapping around its count's numeral.
fn render(family: &Family, value: u32) -> Option<(String, String)> {
    let (wrap, count) = family.0.split_once(':')?;
    let numeral = match count {
        "1" | "1.1" | "1.1.1" | "1.1.1.1" => {
            family.1.iter().chain([&value]).map(u32::to_string).collect::<Vec<_>>().join(".")
        }
        "a" | "A" if (1..=26).contains(&value) => {
            let letter = char::from(b'a' + (value - 1) as u8);
            if count == "A" { letter.to_ascii_uppercase().to_string() } else { letter.to_string() }
        }
        "i" => legal_structure_model::layout::roman_numeral(value),
        "I" => legal_structure_model::layout::roman_numeral(value).to_ascii_uppercase(),
        _ => return None,
    };
    let (open, close) = match wrap {
        "[]" => ("[", "]"),
        "()" => ("(", ")"),
        ")" => ("", ")"),
        "." => ("", "."),
        "" if count.contains('.') => ("", ""),
        _ => return None,
    };
    Some((format!("{open}{numeral}{close}"), numeral))
}

/// The characters recognition prints for one of a count's characters, each with what it may stand
/// for.
fn readings(c: char, count: &str) -> Vec<char> {
    match count {
        "1" | "1.1" | "1.1.1" | "1.1.1.1" => match c {
            '0'..='9' => vec![c],
            '.' | ',' if count != "1" => vec!['.'],
            'I' | 'l' | '|' | 'i' | '!' => vec!['1'],
            'O' | 'o' | '\u{b0}' => vec!['0'],
            'S' | 's' => vec!['5', '8'],
            'B' => vec!['8'],
            'Z' | 'z' => vec!['2'],
            'g' => vec!['9'],
            'G' | 'b' => vec!['6'],
            _ => Vec::new(),
        },
        "a" | "A" => {
            let mut found = match c {
                '@' => vec!['a'],
                '1' | '|' | 'I' | 'l' => vec!['l', 'i'],
                '0' => vec!['o'],
                '8' => vec!['g', 'b'],
                '6' => vec!['b'],
                '5' => vec!['s'],
                c if c.is_ascii_alphabetic() => vec![c.to_ascii_lowercase()],
                _ => Vec::new(),
            };
            if count == "A" {
                found.iter_mut().for_each(|c| *c = c.to_ascii_uppercase());
            }
            found
        }
        "i" | "I" => {
            let found = match c.to_ascii_lowercase() {
                'i' | '1' | 'l' | '|' | '!' => 'i',
                'v' | 'y' => 'v',
                'x' => 'x',
                _ => return Vec::new(),
            };
            vec![if count == "I" { found.to_ascii_uppercase() } else { found }]
        }
        _ => Vec::new(),
    }
}

/// A line's opening token read as a misread marker of `family`: where it starts and ends in the
/// line's characters and the numerals its characters may stand for. The wrapping's own marks may be
/// misread ("[" as "(" or "|", "]" as "1", "J" or ")", "." as ","), the closing mark may run into the
/// numeral ("[11" for "[1]"), and one of the two marks of a bracket may be lost ("5]"), never both.
fn misread_marker(text: &str, family: &Family) -> Option<(usize, usize, Vec<Reading>)> {
    let (wrap, count) = family.0.split_once(':')?;
    let (opens, closes): (&str, &str) = match wrap {
        "[]" => ("[({|1lI", "]1lIJj|)}!"),
        "()" => ("([{", ")]}ijlI1J|"),
        ")" => ("", ")]}"),
        "." => ("", ".,"),
        "" => ("", ""),
        _ => return None,
    };
    let characters = text.chars().collect::<Vec<_>>();
    let start = characters.iter().take_while(|c| c.is_whitespace()).count();
    let mut end = characters[start..].iter().position(|c| c.is_whitespace()).map_or(characters.len(), |at| start + at);
    // "( d)", "(a )": a space inside the marks.
    if matches!(wrap, "[]" | "()") && end - start <= 2 && characters.get(end + 1).is_some_and(|c| !c.is_whitespace()) {
        let next = characters[end + 1..].iter().position(|c| c.is_whitespace()).map_or(characters.len(), |at| end + 1 + at);
        if next - start <= 6 {
            end = next;
        }
    }
    // "7.Although": the stop runs into the paragraph's first word.
    if wrap == "." {
        if let Some(stop) = characters[start..end].iter().position(|c| ".,".contains(*c)).map(|at| start + at) {
            if characters.get(stop + 1).is_some_and(|c| c.is_uppercase())
                && stop > start
                && characters[start..stop].iter().all(|c| !readings(*c, count).is_empty())
            {
                end = stop + 1;
            }
        }
    }
    let token = characters[start..end].iter().filter(|c| !c.is_whitespace()).copied().collect::<Vec<_>>();
    if token.is_empty() || token.len() > 8 {
        return None;
    }
    let mut found = Vec::new();
    let open_choices: Vec<usize> = if !opens.is_empty() && opens.contains(token[0]) { vec![1, 0] } else { vec![0] };
    for open in open_choices {
        for close in [1usize, 0] {
            if open + close > token.len() {
                continue;
            }
            let body = &token[open..token.len() - close];
            if body.is_empty() || body.len() > 6 || close == 1 && !closes.contains(token[token.len() - 1]) {
                continue;
            }
            // A bracket keeps one of its marks, and a digit read for its opening one stands only
            // before a closing one; a stop or a closing mark is kept, misread or not; a section
            // number keeps its point.
            let (has_open, has_close) = (open == 1, close == 1);
            let own_open = has_open && opens.starts_with(token[0]);
            let kept = match wrap {
                "[]" | "()" => (own_open || has_close) && (!has_open || own_open || has_close),
                ")" | "." => has_close,
                _ => body.iter().any(|c| ".,".contains(*c)),
            };
            if !kept || matches!(count, "a" | "A") && body.len() != 1 {
                continue;
            }
            let mut numerals = vec![String::new()];
            for c in body {
                let choices = readings(*c, count);
                if choices.is_empty() {
                    numerals.clear();
                    break;
                }
                numerals = numerals.iter().flat_map(|numeral| choices.iter().map(move |c| format!("{numeral}{c}"))).collect();
                numerals.truncate(64);
            }
            let printed = token.iter().collect::<String>();
            let exact = crate::candidates::numbering_marker(&printed, None).is_some_and(|marker| marker.family == family.0);
            found.extend(numerals.into_iter().map(|numeral| Reading {
                exact: exact && numeral.chars().eq(body.iter().copied()),
                numeral,
                shaped: own_open,
            }));
        }
    }
    (!found.is_empty()).then_some((start, end, found))
}

/// Restore the paragraph and item numbers recognition misread within a run of them; returns how
/// many lines it repaired.
pub fn repair_paragraph_numbers(pages: &mut [Page]) -> usize {
    // A line that can open a numbered paragraph: no running head or foot, no note.
    let order = print_order(pages, |line| {
        line.note_region_mode.is_empty()
            && !matches!(line.region_type.as_str(), "header" | "footer" | "footnote")
            && !line.text.trim().is_empty()
    });
    let line = |position: usize| {
        let (page, line) = order[position];
        &pages[page].lines[line]
    };
    if !(0..order.len()).any(|position| recognized_line(line(position))) {
        return 0;
    }
    // Every marker the grammar reads opening a line, by its family.
    let mut by_family = HashMap::<Family, Vec<(usize, u32)>>::new();
    let mut last = HashMap::<&'static str, NumberingMarker>::new();
    for position in 0..order.len() {
        let token = line(position).text.trim().split_whitespace().next().unwrap_or_default();
        let wrap = wrap_of(token);
        let Some(marker) = crate::candidates::numbering_marker(token, last.get(wrap)) else {
            continue;
        };
        let count = marker.family.split_once(':').map_or("", |(_, count)| count);
        if (wrap.is_empty() && !count.contains('.')) || count == "bullet" {
            continue;
        }
        last.insert(wrap, marker.clone());
        by_family.entry((marker.family, marker.prefix)).or_default().push((position, marker.ordinal));
    }
    let runs = by_family
        .iter()
        .map(|(family, entries)| (family, supported(entries, PARAGRAPH_WINDOW_LINES, PARAGRAPH_WINDOW_CANDIDATES)))
        .collect::<Vec<_>>();
    // A line another run supports is that run's.
    let taken = runs
        .iter()
        .flat_map(|(family, found)| found.iter().map(move |(position, _)| (*position, *family)))
        .collect::<HashMap<_, _>>();
    let mut gaps = Vec::new();
    for (family, found) in &runs {
        let Some(&(first, first_value)) = found.first() else {
            continue;
        };
        // The gaps the run spans, each between two of its markers starting together; and below the
        // run's first marker, from its count's start.
        let mut spans = Vec::new();
        if (2..=3).contains(&first_value) {
            let from = by_family[*family].iter().map(|(position, _)| *position).filter(|&position| position < first).next_back();
            let low = first.saturating_sub(PARAGRAPH_WINDOW_LINES).max(from.map_or(0, |position| position + 1));
            spans.push((low, first, 0, first_value, vec![first]));
        }
        for pair in found.windows(2) {
            let ((before, value), (after, next)) = (pair[0], pair[1]);
            if next < value + 2
                || next - value > GAP_VALUES
                || after <= before + 1
                || after - before > GAP_LINES
                || !aligned(line(before), &[line(after)])
                || !aligned(line(after), &[line(before)])
            {
                continue;
            }
            spans.push((before + 1, after, value, next, vec![before, after]));
        }
        for (low, high, value, next, neighbours) in spans {
            let neighbours = neighbours.iter().map(|&n| line(n)).collect::<Vec<_>>();
            let lines = (low..high)
                .filter(|&position| {
                    recognized_line(line(position))
                        && aligned(line(position), &neighbours)
                        && taken.get(&position).is_none_or(|other| other == family)
                })
                .filter_map(|position| {
                    let (start, end, readings) = misread_marker(&line(position).text, family)?;
                    Some(Candidate { position, start, end, readings })
                })
                .collect::<Vec<_>>();
            let values = (value + 1..next).filter_map(|v| render(family, v)).collect();
            gaps.push(Gap { lines, values, lone: true });
        }
    }
    repair(pages, &order, &gaps, false, true)
}
