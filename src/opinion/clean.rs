//! The reading text: page marks and furniture out, broken words whole, every range moved to match.

use std::collections::HashMap;
use std::sync::OnceLock;
use legal_structure_model::ScalarRange;
use legal_structure_model::document::{DocumentStructure, NodeKind, PrimaryRole};
use legal_structure_model::text::{javascript_whitespace, utf16_len, ScalarText};
use regex::Regex;
use super::{span, squeezed, Kinds, ScalarCoordinates};

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
/// The engine reads the text only once it is read back (`structure_of`).
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

/// Takes the page marks and the page furniture out of the text and moves every range to match, each furniture line held as
/// a span where it stood. A word a mark split is joined: where its fragments are not words of this text on their own, or
/// the whole word is. The text's index is built again over the new text.
pub(super) fn excise(structure: &mut DocumentStructure, kinds: &mut Kinds, furniture: Vec<ScalarRange>, printed_elsewhere: &mut [(bool, Vec<(u32, usize)>)]) {
    struct Cut { start: usize, end: usize, keep: &'static str }
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let end = utf16_len(&structure.text);
    let lowered = structure.text.to_lowercase();
    let mut counts = HashMap::<&str, usize>::new();
    for word in lowered.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()) {
        *counts.entry(word).or_default() += 1;
    }
    let mut cuts = Vec::new();
    for mark in kinds.pages.iter().filter(|page| page.grammar.as_deref() != Some("furniture")).filter_map(|page| page.marker_range) {
        let before = (0..4).find_map(|short| coordinates.slice_utf16(mark.start.saturating_sub(40 - short)..mark.start)).unwrap_or("");
        let after = (0..4).find_map(|short| coordinates.slice_utf16(mark.end..(mark.end + 40 - short).min(end))).unwrap_or("");
        let left = before.trim_end_matches(javascript_whitespace);
        let right = after.trim_start_matches(javascript_whitespace);
        let (space_before, space_after) = (&before[left.len()..], &after[..after.len() - right.len()]);
        let last = left.rsplit(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_lowercase();
        let first = right.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
        let broke_line = space_before.contains('\n') || space_after.contains('\n');
        let touching = space_before.is_empty() && space_after.is_empty();
        let joined = !first.is_empty() && first.starts_with(char::is_lowercase) && (left.ends_with('-') && !broke_line || (!last.is_empty() && {
            let known = |word: &str| counts.get(word).copied().unwrap_or(0) >= 2;
            let fragments = known(&last) as usize + known(first) as usize;
            // Spaced off both sides they were two words unless the text knows the one; on a line of its own the mark may
            // split a word no fragment of which is one; touching, one known fragment is no word.
            counts.contains_key(format!("{last}{first}").as_str()) || (broke_line && fragments == 0) || (touching && !broke_line && fragments < 2)
        }));
        let keep = if joined { "" } else if left.is_empty() { "" } else if broke_line { "\n" } else { " " };
        cuts.push(Cut { start: mark.start - utf16_len(space_before), end: mark.end + utf16_len(space_after), keep });
    }
    // Repairs of the words themselves, by byte.
    static BROKEN: OnceLock<Regex> = OnceLock::new();
    static GLUED: OnceLock<Regex> = OnceLock::new();
    static NOISE: OnceLock<Regex> = OnceLock::new();
    let known = |word: &str| counts.get(word).copied().unwrap_or(0) >= 2;
    let compound = |left: &str| matches!(left, "self" | "non" | "pre" | "co" | "anti" | "quasi" | "cross" | "ex" | "well" | "full" | "long" | "short" | "post" | "multi" | "semi" | "sub" | "over" | "under" | "third" | "first" | "second");
    let mut repairs = Vec::<(usize, usize, &'static str)>::new();
    // A word a line's end broke: "pro-\nvided" is whole, "self-\ninsured" keeps its hyphen.
    if kinds.opinion.rendition == "plain_text.txt" {
        for capture in BROKEN.get_or_init(|| Regex::new(r"(\p{Ll}{2,})-[ \t]*\r?\n[ \t\u{c}]*(\p{Ll}{2,})").unwrap()).captures_iter(&structure.text) {
            let (left, right) = (capture.get(1).unwrap(), capture.get(2).unwrap());
            let (last, first) = (left.as_str().to_lowercase(), right.as_str());
            if !compound(&last) && (counts.contains_key(format!("{last}{first}").as_str()) || !(known(&last) && known(first))) {
                repairs.push((left.end(), right.start(), ""));
            }
        }
    }
    // The same left in a reporter's line: "exac-tions", where the text has "exactions".
    for (hyphen, _) in structure.text.match_indices('-') {
        let left = structure.text[..hyphen].rsplit(|c: char| !c.is_alphabetic()).next().unwrap_or("");
        let right = structure.text[hyphen + 1..].split(|c: char| !c.is_alphabetic()).next().unwrap_or("");
        if left.len() < 2 || right.len() < 2 || !right.chars().all(char::is_lowercase) {
            continue;
        }
        let last = left.to_lowercase();
        if !compound(&last) && counts.contains_key(format!("{last}{right}").as_str()) {
            repairs.push((hyphen, hyphen + 1, ""));
        }
    }
    // A small word run into the name after it: "andCooley".
    for capture in GLUED.get_or_init(|| Regex::new(r"(?-u:\b)(?:and|the|of|in|to|by|for|with|that|at|on|from|or)(\p{Lu}\p{Ll}{2,})").unwrap()).captures_iter(&structure.text) {
        let at = capture.get(1).unwrap().start();
        repairs.push((at, at, " "));
    }
    // A scanner's black square, and a document's own tags carried as text.
    for found in NOISE.get_or_init(|| Regex::new(r"■|\[EDITORS?['’]? ?NOTE:[^\]]*\]|(?m)^[ \t]*(?:</?[a-zA-Z][^>\n]{0,60}>|class=\w+>)[ \t]*$").unwrap()).find_iter(&structure.text) {
        repairs.push((found.start(), found.end(), ""));
    }
    repairs.sort_unstable();
    let mut reached = 0;
    for (start, end, keep) in repairs {
        if start < reached {
            continue;
        }
        reached = end;
        let unit = coordinates.utf16_of_byte(start);
        cuts.push(Cut { start: unit, end: unit + utf16_len(&structure.text[start..end]), keep });
    }
    // A note's call is out of the words too, its place kept: "defendant7 s" reads "defendant s", the note still called there.
    if kinds.opinion.rendition != "plain_text.txt" {
        for call in kinds.notes.iter().flat_map(|note| note.calls.iter()).chain(kinds.spans_of("unmatched-call").map(|call| &call.range)) {
            if call.end > call.start && call.end - call.start <= 8 {
                cuts.push(Cut { start: call.start, end: call.end, keep: "" });
            }
        }
    }
    cuts.extend(furniture.iter().map(|range| Cut { start: range.start, end: range.end, keep: "" }));
    if cuts.is_empty() {
        return;
    }
    // A furniture line stands where it was, as the cut moves that place.
    let lines = furniture.iter().map(|range| {
        let mut line = span("furniture", range.start, range.start);
        line.role = Some(PrimaryRole::Furniture);
        line.display_value = Some(coordinates.slice_utf16(range.start..range.end).map(squeezed).unwrap_or_default());
        line
    }).collect::<Vec<_>>();
    kinds.spans.extend(lines);
    cuts.sort_by_key(|cut| cut.start);
    let mut merged = Vec::<Cut>::with_capacity(cuts.len());
    for cut in cuts {
        match merged.last_mut().filter(|last| cut.start <= last.end) {
            Some(last) => {
                last.end = last.end.max(cut.end);
                // A break outlasts what is cut beside it.
                if cut.keep.len() > last.keep.len() || cut.keep == "\n" {
                    last.keep = cut.keep;
                }
            }
            None => merged.push(cut),
        }
    }
    // Each cut's start, and what the text has lost by its end.
    let mut lost = Vec::with_capacity(merged.len());
    let mut total = 0isize;
    for cut in &merged {
        total += (cut.end - cut.start) as isize - cut.keep.len() as isize;
        lost.push(total);
    }
    let moved = |offset: usize| {
        let at = merged.partition_point(|cut| cut.start < offset);
        let Some(previous) = at.checked_sub(1) else { return offset };
        let cut = &merged[previous];
        let before = previous.checked_sub(1).map_or(0, |index| lost[index]);
        if offset >= cut.end { (offset as isize - lost[previous]) as usize } else { (cut.start as isize - before) as usize + (offset - cut.start).min(cut.keep.len()) }
    };
    let shift = |range: &mut ScalarRange| { range.start = moved(range.start); range.end = moved(range.end); };
    for node in structure.nodes.iter_mut().chain(kinds.all_mut()) {
        shift(&mut node.range);
        node.ranges.iter_mut().chain(node.calls.iter_mut()).for_each(shift);
        if node.kind == NodeKind::Page {
            node.marker_range = None;
        }
    }
    for (_, numbers) in printed_elsewhere.iter_mut() {
        numbers.iter_mut().for_each(|number| number.1 = moved(number.1));
    }
    let mut text = String::with_capacity(structure.text.len());
    let (mut unit, mut index) = (0, 0);
    for c in structure.text.chars() {
        while index < merged.len() && unit >= merged[index].end {
            // What goes in where nothing comes out.
            if merged[index].start == merged[index].end { text.push_str(merged[index].keep); }
            index += 1;
        }
        match merged.get(index).filter(|cut| unit >= cut.start) {
            Some(cut) => if unit == cut.start { text.push_str(cut.keep); },
            None => text.push(c),
        }
        unit += c.len_utf16();
    }
    kinds.coordinates = ScalarCoordinates::new(&text);
    structure.text = text;
}
