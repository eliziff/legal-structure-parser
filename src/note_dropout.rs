//! Note labels whose leading digits recognition dropped at the line's start ("10 Id." read
//! "0 Id.", "224 Julietta" read "24 Julietta"), restored from the note chain itself: ported
//! from the Open Access Journals Database's `eol_label_dropout.py`.
//!
//! Between two sequence-supported labels a and a + 2, a label-shaped line whose leading digits
//! are a proper suffix of a + 1 names itself: it is a + 1 with its prefix lost. The line must
//! start within a line's height of its neighbouring labels; a numbered heading or a citation
//! opening a wrapped line sits elsewhere. A page's first two and last line never count as
//! labels, so running heads and folios, which also climb by one a page, forge no chain.
//!
//! The chain reads the rest of a run too: between supported labels a and b, the label-shaped lines
//! where they start carry a + 1 to b - 1 in order, each taking the value its digits or marks read as
//! best (":IO." 30, "JI." 31, "2s" 28, "12" for 82 when its 8 was misread).

use legal_structure_model::layout::Page;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

const SUPPORT_WINDOW_LINES: usize = 150;
const SUPPORT_WINDOW_CANDIDATES: usize = 12;
const MAX_NOTE_VALUE: u32 = 899;
const X0_LOW: f64 = -0.5;
const X0_HIGH: f64 = 1.0;

fn label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(\d{1,3})(?:[\s.).\u{b0}]|$)").expect("label regex"))
}

/// Every non-empty line in print order as (page slot, line slot), the positions of each page's
/// first two and last line, and the label-shaped lines a neighbouring label supports.
fn chain(pages: &[Page]) -> (Vec<(usize, usize)>, HashSet<usize>, Vec<(usize, u32)>) {
    let mut order = Vec::new();
    let mut edges = HashSet::new();
    let mut slots = (0..pages.len()).collect::<Vec<_>>();
    slots.sort_by_key(|&slot| pages[slot].index);
    for page in slots {
        let mut lines = (0..pages[page].lines.len())
            .filter(|&line| !pages[page].lines[line].text.trim().is_empty())
            .collect::<Vec<_>>();
        lines.sort_by_key(|&line| pages[page].lines[line].reading_order);
        let first = order.len();
        order.extend(lines.iter().map(|&line| (page, line)));
        let last = order.len();
        edges.extend((first..last).take(2));
        if last > first {
            edges.insert(last - 1);
        }
    }
    let text = |position: usize| {
        let (page, line) = order[position];
        pages[page].lines[line].text.trim()
    };
    let candidates = (0..order.len())
        .filter(|position| !edges.contains(position))
        .filter_map(|position| {
            let value = label().captures(text(position))?[1].parse::<u32>().ok()?;
            (value > 0 && value <= MAX_NOTE_VALUE).then_some((position, value))
        })
        .collect::<Vec<_>>();
    let supported = candidates
        .iter()
        .enumerate()
        .filter(|(index, (position, value))| {
            let low = index.saturating_sub(SUPPORT_WINDOW_CANDIDATES);
            let high = (index + SUPPORT_WINDOW_CANDIDATES + 1).min(candidates.len());
            candidates[low..high].iter().any(|(other, other_value)| {
                other.abs_diff(*position) <= SUPPORT_WINDOW_LINES
                    && ((other < position && *other_value + 1 == *value)
                        || (other > position && *other_value == value + 1))
            })
        })
        .map(|(_, candidate)| *candidate)
        .collect::<Vec<_>>();
    (order, edges, supported)
}

/// Whether recognition read the line, the only reading that drops or misreads label digits: our
/// own recognition, or a scan's invisible recognition layer. Printed glyphs are exact.
fn recognized(pages: &[Page], order: &[(usize, usize)], position: usize) -> bool {
    let (page, line) = order[position];
    recognized_line(&pages[page].lines[line])
}

pub(crate) fn recognized_line(line: &legal_structure_model::layout::Line) -> bool {
    line.source == "ocr"
        || line
            .spans
            .iter()
            .any(|span| span.flags & legal_structure_model::layout::INVISIBLE != 0)
}

/// Whether a line starts where its neighbouring labels start, within a line's height.
fn aligned(
    pages: &[Page],
    order: &[(usize, usize)],
    position: usize,
    neighbours: [usize; 2],
) -> bool {
    let bbox = |position: usize| {
        let (page, line) = order[position];
        pages[page].lines[line].bbox
    };
    let (fired, neighbour) = (
        bbox(position),
        bbox(neighbours[0])[0].min(bbox(neighbours[1])[0]),
    );
    let height = fired[3] - fired[1];
    let delta = fired[0] - neighbour;
    height > 0.0 && (X0_LOW * height..=X0_HIGH * height).contains(&delta)
}

/// Restore the dropped leading digits of note labels in place, and the labels recognition misread
/// within a run of them; returns how many lines it repaired.
pub fn repair_label_dropouts(pages: &mut [Page]) -> usize {
    let (order, _, supported) = chain(pages);
    let text = |position: usize| {
        let (page, line) = order[position];
        pages[page].lines[line].text.trim()
    };
    let mut repairs = Vec::new();
    for pair in supported.windows(2) {
        let ((before, value), (after, next)) = (pair[0], pair[1]);
        if next != value + 2 || after <= before + 1 {
            continue;
        }
        let expected = (value + 1).to_string();
        for position in (before + 1..after).filter(|&position| recognized(pages, &order, position))
        {
            let Some(kept) = label().captures(text(position)).map(|c| c[1].to_owned()) else {
                continue;
            };
            if kept == expected || !expected.ends_with(&kept) {
                continue;
            }
            if aligned(pages, &order, position, [before, after]) {
                repairs.push((position, expected[..expected.len() - kept.len()].to_owned()));
            }
        }
    }
    for (position, prefix) in &repairs {
        let (page, line) = order[*position];
        let line = &mut pages[page].lines[line];
        let lead = line.text.chars().take_while(|c| c.is_whitespace()).count();
        let at = line
            .text
            .char_indices()
            .nth(lead)
            .map_or(line.text.len(), |(at, _)| at);
        line.text.insert_str(at, prefix);
        let shift = prefix.chars().count();
        for word in &mut line.words {
            if word.start == lead {
                word.text.insert_str(0, prefix);
                word.end += shift;
            } else if word.start > lead {
                word.start += shift;
                word.end += shift;
            }
        }
    }
    repairs.len() + repair_label_runs(pages)
}

/// Write `label` over a line's characters `start..end`: the words after it move with it, and a word
/// holding part of it ends where it ends.
pub(crate) fn replace_label(
    line: &mut legal_structure_model::layout::Line,
    start: usize,
    end: usize,
    label: &str,
) {
    let (length, written) = (end - start, label.chars().count());
    let rest = line.text.chars().skip(end).collect::<String>();
    line.text = line
        .text
        .chars()
        .take(start)
        .chain(label.chars())
        .chain(rest.chars())
        .collect();
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
            word.end = if word.end >= end {
                word.end + written - length
            } else {
                new_end
            };
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
        word.text = line
            .text
            .chars()
            .skip(word.start)
            .take(word.end - word.start)
            .collect();
    }
    line.words = words;
}

/// The note label a line may open with however recognition misread it: up to four digits or marks
/// standing for digits, after at most two stray marks and before a stop or a bracket and a space.
/// Returns where the label starts and ends in the line's characters and the digit strings it may
/// read as, its marks taken each way they are misread.
fn misread_label(text: &str) -> Option<(usize, usize, Vec<String>)> {
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
            .flat_map(|reading| {
                choices
                    .iter()
                    .map(move |choice| format!("{reading}{choice}"))
            })
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
    Some((start, end, readings))
}

/// How well a misread label reads as `value`: its digits exactly, a reading of its marks, the
/// value with a digit misread, or with its leading digits lost.
fn label_fit(readings: &[String], exact: bool, value: u32) -> u32 {
    let wanted = value.to_string();
    readings
        .iter()
        .map(|reading| {
            if *reading == wanted {
                if exact {
                    4
                } else {
                    3
                }
            } else if reading.len() == wanted.len()
                && reading
                    .chars()
                    .zip(wanted.chars())
                    .filter(|(a, b)| a != b)
                    .count()
                    == 1
            {
                2
            } else if wanted.len() > reading.len() && wanted.ends_with(reading.as_str()) {
                2
            } else {
                0
            }
        })
        .max()
        .unwrap_or(0)
}

/// Between two supported labels a and b, the lines starting where they start that open with a label
/// however misread carry a + 1 to b - 1 in order: each takes the value it reads as best, in the
/// order of the page, and a line reading as none of them keeps its text.
fn repair_label_runs(pages: &mut [Page]) -> usize {
    let (order, edges, supported) = chain(pages);
    let text = |position: usize| {
        let (page, line) = order[position];
        pages[page].lines[line].text.clone()
    };
    let mut repairs = Vec::<(usize, usize, usize, u32)>::new();
    for pair in supported.windows(2) {
        let ((before, value), (after, next)) = (pair[0], pair[1]);
        if next < value + 2 || after <= before + 1 || after - before > SUPPORT_WINDOW_LINES {
            continue;
        }
        let lines = (before + 1..after)
            .filter(|position| {
                !edges.contains(position)
                    && recognized(pages, &order, *position)
                    && aligned(pages, &order, *position, [before, after])
            })
            .filter_map(|position| {
                let text = text(position);
                let (start, end, readings) = misread_label(&text)?;
                let exact = text
                    .chars()
                    .skip(start)
                    .take(end - start)
                    .all(|c| c.is_ascii_digit());
                Some((position, start, end, readings, exact))
            })
            .collect::<Vec<_>>();
        let values = (value + 1..next).collect::<Vec<_>>();
        if lines.is_empty() || values.len() > 60 {
            continue;
        }
        // The best order-keeping match of lines to values.
        let (n, m) = (lines.len(), values.len());
        let mut score = vec![vec![0u32; m + 1]; n + 1];
        for x in 1..=n {
            for y in 1..=m {
                let fit = label_fit(&lines[x - 1].3, lines[x - 1].4, values[y - 1]);
                let take = if fit > 0 {
                    score[x - 1][y - 1] + fit
                } else {
                    0
                };
                score[x][y] = score[x - 1][y].max(score[x][y - 1]).max(take);
            }
        }
        let (mut x, mut y) = (n, m);
        while x > 0 && y > 0 {
            let fit = label_fit(&lines[x - 1].3, lines[x - 1].4, values[y - 1]);
            if fit > 0 && score[x][y] == score[x - 1][y - 1] + fit {
                let (position, start, end, readings, exact) = &lines[x - 1];
                if !(*exact && readings[0] == values[y - 1].to_string()) {
                    repairs.push((*position, *start, *end, values[y - 1]));
                }
                x -= 1;
                y -= 1;
            } else if score[x][y] == score[x - 1][y] {
                x -= 1;
            } else {
                y -= 1;
            }
        }
    }
    for (position, start, end, value) in &repairs {
        let (page, line) = order[*position];
        replace_label(
            &mut pages[page].lines[line],
            *start,
            *end,
            &value.to_string(),
        );
    }
    repairs.len()
}
