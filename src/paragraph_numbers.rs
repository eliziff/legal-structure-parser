//! Paragraph and item numbers recognition misread ("[11" for "[1]", "[131" for "[13]", "[20j",
//! "[5S]", "(ci" for "(c)", "7,Although"), restored from the numbering run they stand in: the note
//! label repair's principles (`note_dropout`) applied to a document's numbered paragraphs.
//!
//! Every line opening with a marker the numbering grammar reads ("[12]", "12.", "(c)", "4.3") joins
//! its family's chain; one a neighbour at one more or one less supports within a window is
//! supported. Between supported a and b of one family that start within a line's height of each
//! other, the recognized lines starting there whose opening token reads, however misread, as the
//! family's marker carry a + 1 to b - 1 in order, each taking the value it reads as best; a lone such
//! line in a gap of one carries that value however its digits read. A run's first supported marker
//! is supported from below by its count's start, so a misread first paragraph is read as the
//! first. Printed text is exact and never touched; running heads, feet and note bands join no
//! chain.

use crate::note_dropout::{recognized_line, replace_label};
use legal_structure_model::layout::{Line, Page};
use legal_structure_model::NumberingMarker;
use std::collections::HashMap;

const SUPPORT_WINDOW_LINES: usize = 300;
const SUPPORT_WINDOW_CANDIDATES: usize = 12;
const GAP_LINES: usize = 400;
const GAP_VALUES: u32 = 30;
const X0_LOW: f64 = -0.5;
const X0_HIGH: f64 = 1.0;

/// A family as its markers print: the wrapping, the count, and the numbers before the last.
type Family = (String, Vec<u32>);

struct Entry {
    position: usize,
    family: Family,
    value: u32,
}

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
        "1" | "1.1" | "1.1.1" | "1.1.1.1" => family
            .1
            .iter()
            .chain([&value])
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("."),
        "a" | "A" if (1..=26).contains(&value) => {
            let letter = char::from(b'a' + (value - 1) as u8);
            if count == "A" {
                letter.to_ascii_uppercase().to_string()
            } else {
                letter.to_string()
            }
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
            vec![if count == "I" {
                found.to_ascii_uppercase()
            } else {
                found
            }]
        }
        _ => Vec::new(),
    }
}

/// A line's opening token read as a misread marker of `family`: where it starts and ends in the
/// line's characters, the numerals its characters may stand for, and whether it printed the
/// family's own marks. The wrapping's own marks may be misread ("[" as "(" or "|", "]" as "1", "J"
/// or ")", "." as ","), the closing mark may run into the numeral ("[11" for "[1]"), and one of the
/// two marks of a bracket may be lost ("5]"), never both.
fn misread(text: &str, family: &Family) -> Option<(usize, usize, Vec<Reading>)> {
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
    let end = characters[start..]
        .iter()
        .position(|c| c.is_whitespace())
        .map_or(characters.len(), |at| start + at);
    // "( d)", "(a )": a space inside the marks.
    let mut end = end;
    if matches!(wrap, "[]" | "()")
        && end - start <= 2
        && characters.get(end + 1).is_some_and(|c| !c.is_whitespace())
    {
        let next = characters[end + 1..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(characters.len(), |at| end + 1 + at);
        if next - start <= 6 {
            end = next;
        }
    }
    // "7.Although": the stop runs into the paragraph's first word.
    if wrap == "." {
        if let Some(stop) = characters[start..end]
            .iter()
            .position(|c| ".,".contains(*c))
            .map(|at| start + at)
        {
            if characters.get(stop + 1).is_some_and(|c| c.is_uppercase())
                && stop > start
                && characters[start..stop]
                    .iter()
                    .all(|c| !readings(*c, count).is_empty())
            {
                end = stop + 1;
            }
        }
    }
    let token = characters[start..end]
        .iter()
        .filter(|c| !c.is_whitespace())
        .copied()
        .collect::<Vec<_>>();
    if token.is_empty() || token.len() > 8 {
        return None;
    }
    let mut found = Vec::new();
    let open_choices: Vec<usize> = if opens.is_empty() {
        vec![0]
    } else if opens.contains(token[0]) {
        vec![1, 0]
    } else {
        vec![0]
    };
    for open in open_choices {
        for close in [1usize, 0] {
            if open + close > token.len() {
                continue;
            }
            let body = &token[open..token.len() - close];
            if body.is_empty() || body.len() > 6 {
                continue;
            }
            if close == 1 && !closes.contains(token[token.len() - 1]) {
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
            if !kept {
                continue;
            }
            if matches!(count, "a" | "A") && body.len() != 1 {
                continue;
            }
            let mut numerals = vec![String::new()];
            for c in body {
                let choices = readings(*c, count);
                if choices.is_empty() {
                    numerals.clear();
                    break;
                }
                numerals = numerals
                    .iter()
                    .flat_map(|numeral| choices.iter().map(move |c| format!("{numeral}{c}")))
                    .collect();
                if numerals.len() > 64 {
                    numerals.truncate(64);
                }
            }
            let printed = token.iter().collect::<String>();
            let exact = crate::candidates::numbering_marker(&printed, None)
                .is_some_and(|marker| marker.family == family.0);
            found.extend(numerals.into_iter().map(|numeral| {
                let exact = exact && numeral.chars().eq(body.iter().copied());
                Reading {
                    numeral,
                    exact,
                    shaped: own_open,
                }
            }));
        }
    }
    (!found.is_empty()).then_some((start, end, found))
}

/// One way a misread marker reads: its numeral, whether it printed exactly the family's marker,
/// and whether it kept the family's own opening bracket.
struct Reading {
    numeral: String,
    exact: bool,
    shaped: bool,
}

/// How well a misread marker reads as the numeral `wanted`: exactly, by a reading of its marks,
/// with one digit of several misread, or with its leading digits lost.
fn fit(readings: &[Reading], wanted: &str) -> u32 {
    readings
        .iter()
        .map(
            |Reading {
                 numeral: reading,
                 exact,
                 ..
             }| {
                if reading == wanted {
                    if *exact {
                        4
                    } else {
                        3
                    }
                } else if wanted.chars().count() > 1
                    && reading.chars().count() == wanted.chars().count()
                    && reading
                        .chars()
                        .zip(wanted.chars())
                        .filter(|(a, b)| a != b)
                        .count()
                        == 1
                {
                    2
                } else if wanted.len() > reading.len()
                    && wanted.ends_with(reading.as_str())
                    && !reading.contains('.')
                {
                    2
                } else {
                    0
                }
            },
        )
        .max()
        .unwrap_or(0)
}

/// Whether a line can open a numbered paragraph: no running head or foot, no note.
fn eligible(line: &Line) -> bool {
    line.note_region_mode.is_empty()
        && !matches!(line.region_type.as_str(), "header" | "footer" | "footnote")
        && !line.text.trim().is_empty()
}

/// Restore the paragraph and item numbers recognition misread within a run of them; returns how
/// many lines it repaired.
pub fn repair_paragraph_numbers(pages: &mut [Page]) -> usize {
    let mut order = Vec::new();
    let mut slots = (0..pages.len()).collect::<Vec<_>>();
    slots.sort_by_key(|&slot| pages[slot].index);
    for page in slots {
        let mut lines = (0..pages[page].lines.len())
            .filter(|&line| eligible(&pages[page].lines[line]))
            .collect::<Vec<_>>();
        lines.sort_by_key(|&line| pages[page].lines[line].reading_order);
        order.extend(lines.iter().map(|&line| (page, line)));
    }
    if !order
        .iter()
        .any(|&(page, line)| recognized_line(&pages[page].lines[line]))
    {
        return 0;
    }
    let line = |position: usize| {
        let (page, line) = order[position];
        &pages[page].lines[line]
    };
    // Every marker the grammar reads opening a line, by its family.
    let mut entries = Vec::<Entry>::new();
    let mut last = HashMap::<&'static str, NumberingMarker>::new();
    for position in 0..order.len() {
        let text = line(position).text.trim();
        let token = text.split_whitespace().next().unwrap_or_default();
        let wrap = wrap_of(token);
        let Some(marker) = crate::candidates::numbering_marker(token, last.get(wrap)) else {
            continue;
        };
        let count = marker.family.split_once(':').map_or("", |(_, count)| count);
        if (wrap.is_empty() && !count.contains('.')) || count == "bullet" {
            continue;
        }
        last.insert(wrap, marker.clone());
        entries.push(Entry {
            position,
            family: (marker.family, marker.prefix),
            value: marker.ordinal,
        });
    }
    let mut by_family = HashMap::<&Family, Vec<usize>>::new();
    for (index, entry) in entries.iter().enumerate() {
        by_family.entry(&entry.family).or_default().push(index);
    }
    let x0 = |position: usize| line(position).bbox[0];
    let height = |position: usize| {
        let bbox = line(position).bbox;
        bbox[3] - bbox[1]
    };
    let aligned = |position: usize, neighbours: &[usize]| {
        let neighbour = neighbours
            .iter()
            .map(|&n| x0(n))
            .fold(f64::INFINITY, f64::min);
        let (h, delta) = (height(position), x0(position) - neighbour);
        h > 0.0 && (X0_LOW * h..=X0_HIGH * h).contains(&delta)
    };
    let supported = by_family
        .iter()
        .map(|(&family, members)| {
            let found = members
                .iter()
                .enumerate()
                .filter(|(index, &member)| {
                    let (position, value) = (entries[member].position, entries[member].value);
                    let low = index.saturating_sub(SUPPORT_WINDOW_CANDIDATES);
                    let high = (index + SUPPORT_WINDOW_CANDIDATES + 1).min(members.len());
                    members[low..high].iter().any(|&other| {
                        let other = &entries[other];
                        other.position.abs_diff(position) <= SUPPORT_WINDOW_LINES
                            && ((other.position < position && other.value + 1 == value)
                                || (other.position > position && other.value == value + 1))
                    })
                })
                .map(|(_, &member)| (entries[member].position, entries[member].value))
                .collect::<Vec<_>>();
            (family, found)
        })
        .collect::<Vec<_>>();
    // A line another run supports is that run's.
    let taken = supported
        .iter()
        .flat_map(|(family, found)| found.iter().map(move |(position, _)| (*position, *family)))
        .collect::<HashMap<_, _>>();
    // Each line a run reads a number on: where its marker lies, the run's number, and how well
    // the line reads as it.
    let mut claims = Vec::<(usize, usize, usize, String, u32)>::new();
    for (family, supported) in &supported {
        let Some(&(first, first_value)) = supported.first() else {
            continue;
        };
        // The gaps the run spans, each between two of its markers starting together; and below
        // the run's first marker, from its count's start.
        let mut gaps = Vec::new();
        if (2..=3).contains(&first_value) {
            let from = by_family[family]
                .iter()
                .map(|&member| entries[member].position)
                .filter(|&position| position < first)
                .next_back();
            let low = first
                .saturating_sub(SUPPORT_WINDOW_LINES)
                .max(from.map_or(0, |position| position + 1));
            gaps.push((low, first, 0, first_value, vec![first]));
        }
        for pair in supported.windows(2) {
            let ((before, value), (after, next)) = (pair[0], pair[1]);
            if next < value + 2
                || next - value > GAP_VALUES
                || after <= before + 1
                || after - before > GAP_LINES
                || !aligned(before, &[after])
                || !aligned(after, &[before])
            {
                continue;
            }
            gaps.push((before + 1, after, value, next, vec![before, after]));
        }
        for (low, high, value, next, neighbours) in gaps {
            let lines = (low..high)
                .filter(|&position| {
                    recognized_line(line(position))
                        && aligned(position, &neighbours)
                        && taken.get(&position).is_none_or(|other| other == family)
                })
                .filter_map(|position| {
                    let (start, end, readings) = misread(&line(position).text, family)?;
                    Some((position, start, end, readings))
                })
                .collect::<Vec<_>>();
            let values = (value + 1..next)
                .filter_map(|v| render(family, v).map(|(marker, numeral)| (v, marker, numeral)))
                .collect::<Vec<_>>();
            if lines.is_empty() || values.is_empty() {
                continue;
            }
            // A lone line kept the family's bracket in a gap of one: it is that number.
            let lone = lines.len() == 1 && values.len() == 1;
            let fits = |x: usize, y: usize| {
                let found = fit(&lines[x].3, &values[y].2);
                let shaped = lines[x].3.iter().any(|reading| {
                    reading.shaped && reading.numeral.chars().count() <= values[y].2.chars().count()
                });
                if found == 0 && lone && shaped {
                    1
                } else {
                    found
                }
            };
            // The best order-keeping match of lines to values.
            let (n, m) = (lines.len(), values.len());
            let mut score = vec![vec![0u32; m + 1]; n + 1];
            for x in 1..=n {
                for y in 1..=m {
                    let found = fits(x - 1, y - 1);
                    let take = if found > 0 {
                        score[x - 1][y - 1] + found
                    } else {
                        0
                    };
                    score[x][y] = score[x - 1][y].max(score[x][y - 1]).max(take);
                }
            }
            let (mut x, mut y) = (n, m);
            while x > 0 && y > 0 {
                let found = fits(x - 1, y - 1);
                if found > 0 && score[x][y] == score[x - 1][y - 1] + found {
                    let (position, start, end, _) = &lines[x - 1];
                    claims.push((*position, *start, *end, values[y - 1].1.clone(), found));
                    x -= 1;
                    y -= 1;
                } else if score[x][y] == score[x - 1][y] {
                    x -= 1;
                } else {
                    y -= 1;
                }
            }
        }
    }
    // A line two runs read takes the number it reads as best, and none when they read it alike.
    let mut best = HashMap::<usize, (usize, usize, String, u32, bool)>::new();
    for (position, start, end, marker, found) in claims {
        match best.get_mut(&position) {
            Some(held) if held.3 > found => {}
            Some(held) if held.3 == found => held.4 |= held.2 != marker,
            _ => {
                best.insert(position, (start, end, marker, found, false));
            }
        }
    }
    let repairs = best
        .into_iter()
        .filter(|(position, (start, end, marker, _, tied))| {
            !tied
                && line(*position)
                    .text
                    .chars()
                    .skip(*start)
                    .take(end - start)
                    .collect::<String>()
                    != *marker
        })
        .map(|(position, (start, end, marker, _, _))| (position, start, end, marker))
        .collect::<Vec<_>>();
    for (position, start, end, marker) in &repairs {
        let (page, line) = order[*position];
        let line = &mut pages[page].lines[line];
        // A stop run into the paragraph's first word keeps a space after it.
        let joined = line
            .text
            .chars()
            .nth(*end)
            .is_some_and(|c| !c.is_whitespace());
        let written = if joined {
            format!("{marker} ")
        } else {
            marker.clone()
        };
        replace_label(line, *start, *end, &written);
    }
    repairs.len()
}
