//! Paragraphs: one to a place, rejoined across a page, numbered, and who speaks them.

use super::*;

/// One paragraph to a place: the engine's numbered hull stops at the next paragraph, heading or note; one that repeats
/// another gives way to the numbered one; a paragraph that is mostly a note is the note.
pub(super) fn settle(paragraphs: &mut Vec<OpinionParagraph>, headings: &[OpinionHeading], notes: &[ScalarRange]) {
    paragraphs.sort_by_key(|paragraph| (paragraph.start, paragraph.number.is_none()));
    let mut stops = headings.iter().map(|heading| heading.start).chain(notes.iter().map(|note| note.start)).collect::<Vec<_>>();
    stops.sort_unstable();
    let mut kept = Vec::<OpinionParagraph>::with_capacity(paragraphs.len());
    let mut notes = notes.to_vec();
    notes.sort_by_key(|note| note.end);
    // A note a page's foot set in a paragraph's course: the words past it are still the paragraph's.
    let mut pieces = Vec::with_capacity(paragraphs.len());
    for mut paragraph in std::mem::take(paragraphs) {
        for note in &notes[notes.partition_point(|note| note.end <= paragraph.start)..] {
            if note.start >= paragraph.end {
                break;
            }
            if paragraph.start < note.start && note.end < paragraph.end {
                let mut head = paragraph.clone();
                head.end = note.start;
                paragraph.start = note.end;
                paragraph.number = None;
                pieces.push(head);
            }
        }
        pieces.push(paragraph);
    }
    for mut paragraph in pieces {
        if let Some(stop) = stops.get(stops.partition_point(|stop| *stop <= paragraph.start)).filter(|stop| **stop < paragraph.end) {
            paragraph.end = *stop;
        }
        let touched = &notes[notes.partition_point(|note| note.end <= paragraph.start)..];
        let covered = touched.iter().take_while(|note| note.start < paragraph.end || note.end < paragraph.end)
            .map(|note| note.end.min(paragraph.end).saturating_sub(note.start.max(paragraph.start))).sum::<usize>();
        if covered * 2 >= (paragraph.end - paragraph.start).max(1) {
            continue;
        }
        if let Some(last) = kept.last_mut() {
            if paragraph.start <= last.start + 2 {
                if last.number.is_none() && paragraph.number.is_some() {
                    *last = paragraph;
                }
                continue;
            }
            if paragraph.start < last.end {
                last.end = paragraph.start;
            }
        }
        if paragraph.end > paragraph.start {
            kept.push(paragraph);
        }
    }
    *paragraphs = kept;
}

/// Paragraph numbers the text prints but the markup does not mark ("¶ 12", "{12}", "[12]", "12.").
pub(super) fn number_paragraphs(layer: &mut OpinionLayer) {
    join_block_counters(layer);
    numbers_off_titles(layer);
    let coordinates = ScalarText::new(&layer.text);
    // A quotation's numbers are the quotation's.
    let openings = layer.paragraphs.iter().enumerate().filter(|(_, paragraph)| !paragraph.quote).filter_map(|(index, paragraph)| {
        Some((index, trim_js(coordinates.slice_utf16(paragraph.start..paragraph.end)?)))
    }).collect::<Vec<_>>();
    let numbered = text::own_numbers(openings.iter().copied(), 3);
    let lettered = lettered_paragraphs(&openings, &numbered);
    for (index, number) in numbered {
        layer.paragraphs[index].number.get_or_insert(number.to_string());
    }
    for (index, label) in lettered {
        layer.paragraphs[index].number.get_or_insert(label);
    }
    // A certified question's own number.
    static QUESTION: OnceLock<Regex> = OnceLock::new();
    let question = QUESTION.get_or_init(|| Regex::new(r#"^["“]?(Question \d{1,2})[.:]"#).unwrap());
    let asked = layer.paragraphs.iter().enumerate().filter(|(_, paragraph)| paragraph.number.is_none()).filter_map(|(index, paragraph)| {
        Some((index, question.captures(trim_js(coordinates.slice_utf16(paragraph.start..paragraph.end)?))?[1].to_owned()))
    }).collect::<Vec<_>>();
    for (index, label) in asked {
        layer.paragraphs[index].number = Some(label);
    }
    split_numbered_blocks(layer);
    fill_skipped_numbers(layer);
    pilcrow_read_as_one(layer);
    fill_skipped_numbers(layer);
    misread_numbers(layer);
    carry_title_numbers(layer);
    list_labels(layer);
}

/// Every paragraph that opens on a list's label keeps it, numbered run or not.
fn list_labels(layer: &mut OpinionLayer) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    // A block's counter may stand ahead of the label: "6\n(A) verify the wages".
    let label = LABEL.get_or_init(|| Regex::new(r"^(?:\d{1,4}\s*\n\s*)?(\(?(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})\)|(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})[.)])\s+\S").unwrap());
    static WITHIN: OnceLock<Regex> = OnceLock::new();
    let within = WITHIN.get_or_init(|| Regex::new(r"(?m)^[ \t]*(\(?(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})\)|(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})[.)])[ \t]+\S").unwrap());
    let coordinates = ScalarText::new(&layer.text);
    // A court's own named paragraph: an advisory opinion's question and answer, a certified question.
    static NAMED: OnceLock<Regex> = OnceLock::new();
    let named = NAMED.get_or_init(|| Regex::new(r"^(QUESTIONS?(?: (?:NO\. )?(?:\d{1,2}|[IVX]{1,4}))?(?: PRESENTED)?|WE ANSWER|ANSWERS?(?: (?:NO\. )?(?:\d{1,2}|[IVX]{1,4}))?|CERTIFIED QUESTIONS?|Certified [Qq]uestions?|Question(?: (?:No\. )?\d{1,2})?(?: [Pp]resented)?|Answer(?: \d{1,2})?)[:.]").unwrap());
    let items = layer.paragraphs.iter().map(|paragraph| coordinates.slice_utf16(paragraph.start..paragraph.end).map(trim_js).and_then(|opening| Some(label.captures(opening).or_else(|| named.captures(opening))?[1].to_owned()))).collect::<Vec<_>>();
    for (paragraph, item) in layer.paragraphs.iter_mut().zip(items) {
        paragraph.item = item;
    }
    let lists = layer.notes.iter().map(|note| note.ranges.iter().filter_map(|range| coordinates.slice_utf16(range.start..range.end)).flat_map(|body| within.captures_iter(body).map(|capture| capture[1].to_owned())).collect::<Vec<_>>()).collect::<Vec<_>>();
    for (note, items) in layer.notes.iter_mut().zip(lists) {
        note.items = items;
    }
}

/// A block's counter split from its words, a paragraph of "57" alone: it opens the paragraph after it.
fn join_block_counters(layer: &mut OpinionLayer) {
    let bare = {
        let coordinates = ScalarText::new(&layer.text);
        layer.paragraphs.iter().map(|paragraph| coordinates.slice_utf16(paragraph.start..paragraph.end).map(trim_js).is_some_and(|printed| !printed.is_empty() && printed.len() <= 4 && printed.bytes().all(|byte| byte.is_ascii_digit()))).collect::<Vec<_>>()
    };
    for index in (0..layer.paragraphs.len().saturating_sub(1)).rev() {
        if bare[index] && !bare[index + 1] && layer.paragraphs[index].number.is_none() {
            let counter = layer.paragraphs.remove(index);
            layer.paragraphs[index].start = counter.start;
        }
    }
}

/// A title under a paragraph's mark, "¶ 30 Conclusion": the mark stands apart as a paragraph until the numbers are read. A
/// numbered sentence whose number the paragraphs either side leave open, "6. Robert Poe is the Chairman.", is no title at all;
/// an outline's "3. The Order Was Proper." sits among paragraphs numbered in their own right.
fn numbers_off_titles(layer: &mut OpinionLayer) {
    let coordinates = ScalarText::new(&layer.text);
    let opening = |paragraph: &OpinionParagraph| coordinates.slice_utf16(paragraph.start..paragraph.end).map(trim_js).unwrap_or("");
    let plain_numbers = layer.paragraphs.iter().filter_map(|paragraph| text::paragraph_marker(opening(paragraph)).filter(|(_, family)| *family == 4).map(|(number, _)| number)).collect::<HashSet<_>>();
    let left_open = |number: u32| !plain_numbers.contains(&number) && plain_numbers.contains(&(number + 1)) && (number == 1 || plain_numbers.contains(&(number - 1)));
    let stub = |start: usize, end: usize| OpinionParagraph { continues: false, item: None, number: None, start, end, page: None, quote: false, part: None, speaker: None, pins: Vec::new() };
    // A paragraph that opens with the title already holds its number.
    let held = layer.paragraphs.iter().map(|paragraph| paragraph.start).collect::<HashSet<_>>();
    let mut stubs = Vec::new();
    layer.headings.retain_mut(|heading| {
        let Some(printed) = coordinates.slice_utf16(heading.start..heading.end).filter(|_| !held.contains(&heading.start)) else { return true };
        let opened = trim_js(printed);
        let title = text::past_marker(opened);
        if text::says_paragraph(opened) {
            if let Some(at) = printed.find(title).filter(|_| !title.is_empty()) {
                let start = heading.start + utf16_len(&printed[..at]);
                stubs.push(stub(heading.start, start));
                heading.start = start;
            }
            return true;
        }
        let sentence = heading.source != "other-rendition" && text::paragraph_marker(opened).is_some_and(|(number, family)| family == 4 && left_open(number))
            && text::ended_sentence(opened) && title.split_whitespace().count() >= 5 && !title_cased(title);
        if sentence {
            stubs.push(stub(heading.start, heading.end));
        }
        !sentence
    });
    if !stubs.is_empty() {
        layer.paragraphs.append(&mut stubs);
        layer.paragraphs.sort_by_key(|paragraph| paragraph.start);
    }
}

/// A scanner's "1" for "¶" fuses with the number: "15, 116, 118, 119" is 15, 16, 18, 19. Taken only where the number
/// without its first digit fits the one before, and the next fits it, as printed or fused alike. A printed "¶ 17" is no such
/// thing: a separate opinion may open at it.
fn pilcrow_read_as_one(layer: &mut OpinionLayer) {
    let printed = {
        let coordinates = ScalarText::new(&layer.text);
        layer.paragraphs.iter().map(|paragraph| coordinates.slice_utf16(paragraph.start..paragraph.end).map_or(false, |opening| trim_js(opening).trim_start_matches(['[', '{']).starts_with('¶'))).collect::<Vec<_>>()
    };
    let shed = |number: u32| number.to_string().strip_prefix('1').and_then(|rest| rest.parse::<u32>().ok()).filter(|rest| *rest > 0);
    let numbered = layer.paragraphs.iter().enumerate().filter_map(|(index, paragraph)| Some((index, paragraph.number.as_deref()?.parse::<u32>().ok()?))).collect::<Vec<_>>();
    let mut last = 0;
    for (at, &(index, number)) in numbered.iter().enumerate() {
        let fits = shed(number).filter(|rest| !printed[index] && number > last + 3 && *rest > last && *rest <= last + 3).filter(|rest| {
            numbered.get(at + 1).is_none_or(|next| next.1 > *rest && next.1 <= rest + 3 || shed(next.1).is_some_and(|after| after > *rest && after <= rest + 3))
        });
        if let Some(rest) = fits {
            layer.paragraphs[index].number = Some(rest.to_string());
        }
        last = fits.unwrap_or(number);
    }
}

/// A number left alone by the title it stood on belongs to the text under the title, unless that has its own.
fn carry_title_numbers(layer: &mut OpinionLayer) {
    let coordinates = ScalarText::new(&layer.text);
    let bare = layer.paragraphs.iter().enumerate().filter(|(_, paragraph)| paragraph.number.is_some()).filter(|(_, paragraph)| {
        coordinates.slice_utf16(paragraph.start..paragraph.end).map(trim_js).is_some_and(|opening| text::says_paragraph(opening) && text::past_marker(opening).is_empty())
    }).map(|(index, _)| index).collect::<Vec<_>>();
    for index in bare.into_iter().rev() {
        if layer.paragraphs.get(index + 1).is_some_and(|next| next.number.is_none() && !next.quote) {
            let number = layer.paragraphs.remove(index).number;
            layer.paragraphs[index].number = number;
        }
    }
}

/// In an opinion that numbers its paragraphs a number its neighbours contradict was misread: "1, 2, 8, 4" is 3, and "1, 3, 2, 4" two
/// marks swapped. The neighbours say so only where they run by ones either side.
fn misread_numbers(layer: &mut OpinionLayer) {
    let mut numbered = layer.paragraphs.iter().enumerate().filter_map(|(index, paragraph)| Some((index, paragraph.number.as_deref()?.parse::<u32>().ok()?))).collect::<Vec<_>>();
    if numbered.len() * 2 < layer.paragraphs.len() {
        return;
    }
    for at in 1..numbered.len().saturating_sub(1) {
        let (before, here, after) = (numbered[at - 1].1, numbered[at].1, numbered[at + 1].1);
        if after == before + 2 && here != before + 1 {
            numbered[at].1 = before + 1;
            layer.paragraphs[numbered[at].0].number = Some((before + 1).to_string());
        } else if here == before + 2 && after == before + 1 && numbered.get(at + 2).is_some_and(|next| next.1 == before + 3) {
            numbered[at].1 = before + 1;
            numbered[at + 1].1 = before + 2;
            layer.paragraphs[numbered[at].0].number = Some((before + 1).to_string());
            layer.paragraphs[numbered[at + 1].0].number = Some((before + 2).to_string());
        }
    }
}

/// Where a number due next stands in a block's course after a finished sentence, "... complaints. ¶21 On appeal", a new
/// paragraph opens. It is due while the numbered paragraphs that follow have not printed it.
fn split_numbered_blocks(layer: &mut OpinionLayer) {
    let number_of = |paragraph: &OpinionParagraph| paragraph.number.as_deref().and_then(|number| number.parse::<u32>().ok());
    if layer.paragraphs.iter().filter(|paragraph| number_of(paragraph).is_some()).count() < 3 {
        return;
    }
    let coordinates = ScalarText::new(&layer.text);
    let upcoming = {
        let mut later = None;
        let mut upcoming = layer.paragraphs.iter().rev().map(|paragraph| { let next = later; later = number_of(paragraph).or(later); next }).collect::<Vec<_>>();
        upcoming.reverse();
        upcoming
    };
    let mut kept = Vec::with_capacity(layer.paragraphs.len());
    // Numbers that open past 1 leave 1 due: it may stand on the author's line, "GILBERTSON, Chief Justice [¶1.] Scotty".
    let mut due = layer.paragraphs.iter().find_map(number_of).filter(|first| *first > 1).map(|_| 1);
    for (index, mut paragraph) in std::mem::take(&mut layer.paragraphs).into_iter().enumerate() {
        due = number_of(&paragraph).map(|number| number + 1).or(due);
        while let Some(number) = due.filter(|number| !paragraph.quote && upcoming[index].is_none_or(|next| *number < next)) {
            let Some(raw) = coordinates.slice_utf16(paragraph.start..paragraph.end) else { break };
            let Some(place) = marker_in_course(raw, number) else { break };
            let split = paragraph.start + utf16_len(&raw[..place]);
            let mut next = paragraph.clone();
            next.start = split;
            next.number = Some(number.to_string());
            paragraph.end = split;
            kept.push(std::mem::replace(&mut paragraph, next));
            due = Some(number + 1);
        }
        kept.push(paragraph);
    }
    layer.paragraphs = kept;
}

/// An author's line run into the block it heads is no longer than this.
const HEAD_OF_BLOCK: usize = 160;

/// Where "¶ 21", "{21}" or "21." stands past a sentence's end in a block, a new sentence following.
fn marker_in_course(raw: &str, number: u32) -> Option<usize> {
    let digits = number.to_string();
    raw.match_indices(&digits).find_map(|(at, _)| {
        let lead = raw[..at].trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '¶' | '{' | '['));
        let marked = raw[..at].trim_end().ends_with(['¶', '{', '[']);
        let rest = &raw[at + digits.len()..];
        let closed = rest.trim_start_matches(['.', '}', ']']);
        let stopped = closed.len() != rest.len();
        // A headnote's mark may stand between the number and its words: "¶31 [6] For the reasons".
        let words = closed.trim_start();
        let words = words.strip_prefix('[').and_then(|inner| inner.split_once(']')).filter(|(mark, _)| mark.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b',' | b'-' | b' '))).map_or(words, |(_, past)| past.trim_start());
        let opens = words.starts_with(|c: char| c.is_uppercase() || matches!(c, '"' | '“'));
        // A named mark also follows a title in capitals, or the author's line at a block's head.
        let after_title = lead.rsplit(char::is_whitespace).next().is_some_and(|word| word.len() >= 2 && word.chars().any(char::is_alphabetic) && !word.chars().any(char::is_lowercase));
        let placed = text::ended_sentence(lead) || (marked && (after_title || lead.len() < HEAD_OF_BLOCK));
        (lead.len() < at && (marked || stopped) && placed && opens).then_some(lead.len() + (raw[lead.len()..].len() - raw[lead.len()..].trim_start().len()))
    })
}

/// A number between two known ones under what a scanner made of its mark: "T6", "€ 1", "1 41", "1{29", "4 13", "[118]" or
/// "1830" for "¶18" and "¶30", or behind a headnote's "[19, 20]".
fn scanned_number(opening: &str, low: u32, high: u32) -> Option<u32> {
    static MARK: OnceLock<Regex> = OnceLock::new();
    let found = MARK.get_or_init(|| Regex::new(r#"^(?:\[[\d,\s-]+\]\s*)?(?:[^\s\d]{1,2}\s)?[\[{(]?¶?\s?(?:[^\s\d]{1,2}|[14][\[\]{}]?)? ?(\d{1,4})[\]}).]*\s+["“\[\p{Lu}]"#).unwrap()).captures(opening)?.get(1)?;
    let digits = found.as_str();
    // "1998 OK CR 66" is a citation: "1830 We note" goes on in words.
    if digits.len() == 4 && opening[found.end()..].split_whitespace().next().is_some_and(|word| !word.chars().any(char::is_lowercase)) {
        return None;
    }
    (0..digits.len()).filter_map(|from| digits[from..].parse().ok()).find(|number| (low + 1..high).contains(number))
}

/// A number the reader passed over inside a run: "1, 2, 4" where the paragraph between them opens "3.".
pub(super) fn fill_skipped_numbers(layer: &mut OpinionLayer) {
    let coordinates = ScalarText::new(&layer.text);
    let numbered = layer.paragraphs.iter().enumerate().filter_map(|(index, paragraph)| Some((index, paragraph.number.as_deref()?.parse::<u32>().ok()?))).collect::<Vec<_>>();
    let marked_between = |span: std::ops::Range<usize>, low: u32, high: u32| {
        let (mut last, mut found) = (low, Vec::new());
        for index in span {
            let opening = coordinates.slice_utf16(layer.paragraphs[index].start..layer.paragraphs[index].end).map(trim_js).unwrap_or("");
            let number = text::paragraph_marker(opening).map(|(number, _)| number).filter(|number| *number > last && *number < high).or_else(|| scanned_number(opening, last, high));
            if let Some(number) = number {
                found.push((index, number));
                last = number;
            }
        }
        found
    };
    let mut filled = Vec::new();
    for pair in numbered.windows(2) {
        let ((from, low), (to, high)) = (pair[0], pair[1]);
        // As many paragraphs between n and m as numbers missing: each is its number, however a scanner read its mark.
        if high > low + 1 && to - from == (high - low) as usize && (from + 1..to).all(|index| !layer.paragraphs[index].quote) {
            filled.extend((from + 1..to).zip(low + 1..high));
            continue;
        }
        filled.extend(marked_between(from + 1..to, low, high));
    }
    // Before the first number the opinion's first marks may stand in what a scanner made of them. One stray "[1]" is no such
    // run: the marks must fill most of the way up, or the opinion be numbered throughout.
    if let Some(&(to, high)) = numbered.first() {
        let found = marked_between(0..to, 0, high);
        if found.len() * 2 + 1 >= high as usize || numbered.len() * 2 >= layer.paragraphs.len() {
            filled.extend(found);
        }
    }
    // A run from 2 whose first paragraph opens on what a scanner made of "1".
    static FIRST: OnceLock<Regex> = OnceLock::new();
    let first = FIRST.get_or_init(|| Regex::new(r"^\W{0,2}[1IlL][.)]?\s").unwrap());
    if let Some(&(index, 2)) = numbered.first() {
        let opening = index.checked_sub(1).and_then(|before| coordinates.slice_utf16(layer.paragraphs[before].start..layer.paragraphs[before].end)).map_or("", trim_js);
        if first.is_match(opening) {
            filled.push((index - 1, 1));
        }
    }
    for (index, number) in filled {
        layer.paragraphs[index].number = Some(number.to_string());
    }
}

/// Runs of paragraphs that open "(a)", "(b)" or "(1)", "(2)": their labels as printed, to be cited as such.
pub(super) fn lettered_paragraphs(openings: &[(usize, &str)], numbered: &HashMap<usize, u32>) -> Vec<(usize, String)> {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^\(([a-z]|\d{1,2})\)\s").unwrap());
    let order = |printed: &str| printed.parse::<u32>().ok().or_else(|| printed.chars().next().map(|c| c as u32 - 'a' as u32 + 1));
    let mut found = Vec::new();
    let mut run = Vec::<(usize, String)>::new();
    let mut last = 0;
    for (index, opening) in openings.iter().filter(|(index, _)| !numbered.contains_key(index)) {
        let Some(printed) = label.captures(opening) else { continue };
        let Some(place) = order(&printed[1]) else { continue };
        if place != last + 1 {
            if run.len() >= 2 { found.append(&mut run); }
            run.clear();
        }
        if place == 1 || !run.is_empty() {
            run.push((*index, format!("({})", &printed[1])));
            last = place;
        } else {
            last = 0;
        }
    }
    if run.len() >= 2 { found.append(&mut run); }
    found
}

/// Short lines set in a sentence's course are the margin's: an old report's catchwords, read into the column. The
/// sentence then rejoins over them, as it does over a page's turn.
pub(super) fn rejoin_paragraphs(layer: &mut OpinionLayer) {
    let coordinates = ScalarText::new(&layer.text);
    let printed = |paragraph: &OpinionParagraph| coordinates.slice_utf16(paragraph.start..paragraph.end).map(trim_js).unwrap_or("");
    // A plain text's short lines are its wrapping, not its margin.
    let columned = layer.rendition != "plain_text.txt";
    let interleaved = (1..layer.paragraphs.len().saturating_sub(1)).filter(|&index| {
        let (before, line, after) = (&layer.paragraphs[index - 1], &layer.paragraphs[index], &layer.paragraphs[index + 1]);
        columned && !line.quote && line.number.is_none() && printed(line).len() < 60 && printed(line).starts_with(char::is_uppercase)
            && !text::ended_sentence(printed(before)) && printed(after).starts_with(char::is_lowercase)
    }).collect::<Vec<_>>();
    for &index in interleaved.iter().rev() {
        let line = layer.paragraphs.remove(index);
        layer.headnotes.push(headnote_of("catchword", squeezed(printed(&line)), Some(ScalarRange { start: line.start, end: line.end })));
    }
    let mut kept = Vec::<OpinionParagraph>::with_capacity(layer.paragraphs.len());
    let mut titles = layer.headings.iter().map(|heading| heading.start).collect::<Vec<_>>();
    titles.sort_unstable();
    let mut notes = layer.notes.iter().flat_map(|note| note.ranges.iter().map(|range| range.start)).collect::<Vec<_>>();
    notes.sort_unstable();
    for mut paragraph in std::mem::take(&mut layer.paragraphs) {
        if let Some(last) = kept.last_mut() {
            let between = |marks: &[usize]| marks.get(marks.partition_point(|mark| *mark < last.end)).is_some_and(|mark| *mark < paragraph.start);
            if !between(&titles) && last.quote == paragraph.quote && paragraph.number.is_none() && !text::ended_sentence(printed(last)) && printed(&paragraph).starts_with(char::is_lowercase) {
                if !between(&notes) {
                    last.end = paragraph.end;
                    continue;
                }
                paragraph.continues = true;
            }
        }
        kept.push(paragraph);
    }
    layer.paragraphs = kept;
}

/// Paragraphs that hold no words: a rule, or a page mark's whole block once it has gone.
pub(super) fn drop_empty_paragraphs(layer: &mut OpinionLayer) {
    let coordinates = ScalarText::new(&layer.text);
    let spoken = |paragraph: &OpinionParagraph| coordinates.slice_utf16(paragraph.start..paragraph.end).is_some_and(|text| text.chars().any(|c| c.is_alphanumeric() || c == '*'));
    let kept = std::mem::take(&mut layer.paragraphs).into_iter().filter(|paragraph| spoken(paragraph)).collect();
    layer.paragraphs = kept;
}

/// Who speaks each paragraph of an exchange. "Q." and "A." count only where a record has several of each, since "A." is
/// also an outline's letter.
pub(super) fn speakers(layer: &mut OpinionLayer) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^(Q\.|A\.|THE COURT:|The Court:|THE WITNESS:|The Witness:|M(?:R|S|RS)\.\s+\p{Lu}[\p{Lu}'’-]+:)\s").unwrap());
    let coordinates = ScalarText::new(&layer.text);
    let found = layer.paragraphs.iter().enumerate().filter_map(|(index, paragraph)| {
        let printed = trim_js(coordinates.slice_utf16(paragraph.start..paragraph.end)?);
        Some((index, label.captures(printed)?[1].trim_end_matches(':').to_owned()))
    }).collect::<Vec<_>>();
    let questions = found.iter().filter(|(_, speaker)| speaker == "Q.").count();
    if questions >= 2 {
        let (answers, headings): (Vec<_>, Vec<_>) = std::mem::take(&mut layer.headings).into_iter()
            .partition(|heading| coordinates.slice_utf16(heading.start..heading.end).map(trim_js).is_some_and(|printed| printed.starts_with("A. ") || printed.starts_with("Q. ")));
        layer.headings = headings;
        layer.paragraphs.extend(answers.into_iter().map(|heading| OpinionParagraph { continues: false, item: None,
            number: None, start: heading.start, end: heading.end, page: None, quote: false, part: None, speaker: Some("A.".to_owned()), pins: Vec::new(),
        }));
    }
    for (index, speaker) in found {
        if !matches!(speaker.as_str(), "Q." | "A.") || questions >= 2 {
            layer.paragraphs[index].speaker = Some(speaker);
        }
    }
    layer.paragraphs.sort_by_key(|paragraph| paragraph.start);
}

/// A concurrence or dissent that opens inside a flat body, on a line of its own: "SMITH, J., dissenting.".
pub(super) fn separate_parts(layer: &mut OpinionLayer) {
    static OPENS: OnceLock<Regex> = OnceLock::new();
    if layer.parts.is_empty() {
        return;
    }
    let opens = OPENS.get_or_init(|| Regex::new(r"(?i)^(?:(?:mr\.\s+|chief\s+|associate\s+)*(?:justice|judge)\s+)?[\p{Lu}][\p{L}'’.\- ]{1,50}?(?:,?\s+(?:c\.\s*)?j\.?j?\.?|,?\s+(?:chief\s+|circuit\s+|district\s+)?(?:judge|justice))?,?\s*\(?(concurring|dissenting|concurs|dissents)\b[^.]{0,80}\.?\)?:?$").unwrap());
    static RUN_IN: OnceLock<Regex> = OnceLock::new();
    let run_in = RUN_IN.get_or_init(|| Regex::new(r"^[\p{Lu}][\p{Lu}'’.\-]+,\s[^.:]{0,100}?\b(concurring|dissenting)\b[^.:]{0,60}[:.]").unwrap());
    // Any case's name, with the office to say it is a judge: "Hawkins, J., dissenting. I would ..."
    static NAMED: OnceLock<Regex> = OnceLock::new();
    let named = NAMED.get_or_init(|| Regex::new(r"^[\p{Lu}][\p{L}'’.\- ]{1,40},\s(?:(?:C\.\s?|P\.\s?)?J\.|Chief Justice|Justice|Judge)[,:]?\s*\(?(concurring|dissenting)\b[^.:]{0,60}[:.)]").unwrap());
    let coordinates = ScalarText::new(&layer.text);
    let mut breaks = layer.paragraphs.iter().filter_map(|paragraph| {
        // A block's counter may stand ahead of the author: "5\nCOLE, Judge (dissenting)."
        let printed = trim_js(trim_js(coordinates.slice_utf16(paragraph.start..paragraph.end)?).trim_start_matches(|c: char| c.is_ascii_digit()));
        if let Some(capture) = opens.captures(printed).filter(|_| printed.len() <= 170) {
            let kind = if capture[1].to_lowercase().starts_with("dissent") { "dissent" } else { "concurrence" };
            return Some((paragraph.start, kind.to_owned(), squeezed(printed)));
        }
        let capture = run_in.captures(printed).or_else(|| named.captures(printed))?;
        let kind = if capture[1].to_lowercase().starts_with("dissent") { "dissent" } else { "concurrence" };
        Some((paragraph.start, kind.to_owned(), squeezed(&capture[0])))
    }).collect::<Vec<_>>();
    // A second author's line, past the opinion's first text, opens another opinion.
    for line in layer.front.iter().filter(|line| line.role == "author") {
        let Some(range) = line.range else { continue };
        let written = layer.paragraphs.iter().filter(|paragraph| paragraph.end <= range.start).map(|paragraph| paragraph.end - paragraph.start).sum::<usize>();
        if written >= 400 && !breaks.iter().any(|known| known.0.abs_diff(range.start) < 4) {
            breaks.push((range.start, "opinion".to_owned(), line.text.clone()));
        }
    }
    // A later document stacked in the record, under a title of its own.
    static STACKED: OnceLock<Regex> = OnceLock::new();
    let stacked = STACKED.get_or_init(|| Regex::new(r"(?i)^(appendix\b.{0,40}|(?:opinion |order )?on (?:petition for )?rehearing.{0,30}|supplemental opinion)$").unwrap());
    // An order or judgment under a title of its own, once a good deal has been written: a separate ruling. At the top
    // of a record the same word is its title.
    static RULING: OnceLock<Regex> = OnceLock::new();
    let ruling = RULING.get_or_init(|| Regex::new(r"(?i)^(?:(?:amended |final |supplemental )?order(?: of (?:the )?court)?|(?:final )?judgment|decree|attachment\b.{0,40})\.?$").unwrap());
    for heading in &layer.headings {
        let printed = coordinates.slice_utf16(heading.start..heading.end).map(trim_js).unwrap_or("");
        if heading.start > 0 && stacked.is_match(printed) {
            let kind = if printed.to_lowercase().starts_with("appendix") { "appendix" } else { "rehearing" };
            breaks.push((heading.start, kind.to_owned(), squeezed(printed)));
        } else if printed.starts_with(char::is_uppercase) && ruling.is_match(printed) {
            let written = layer.paragraphs.iter().filter(|paragraph| paragraph.end <= heading.start).map(|paragraph| paragraph.end - paragraph.start).sum::<usize>();
            if written >= 1500 {
                let kind = if printed.to_lowercase().starts_with("attachment") { "attachment" } else if printed.to_lowercase().contains("judgment") { "judgment" } else { "order" };
                breaks.push((heading.start, kind.to_owned(), squeezed(printed)));
            }
        }
    }
    breaks.sort_by_key(|known| known.0);
    let nexts = breaks.iter().skip(1).map(|known| known.0).chain([usize::MAX]).collect::<Vec<_>>();
    let mut opening = nexts.iter();
    // A part has text of its own: a paragraph after its line, or a line that runs on into its opinion.
    const LINE_ALONE: usize = 170;
    breaks.retain(|known| {
        let next = *opening.next().unwrap_or(&usize::MAX);
        layer.paragraphs.iter().any(|paragraph| (known.0 < paragraph.start && paragraph.start < next) || (paragraph.start == known.0 && paragraph.end - paragraph.start > LINE_ALONE))
    });
    // What a source already sets apart stands; a break cuts the part it falls in.
    const SAME_START: usize = 40;
    for (start, kind, author) in breaks {
        if layer.parts.iter().any(|part| part.start.abs_diff(start) < SAME_START) {
            continue;
        }
        let Some(at) = layer.parts.iter().position(|part| part.start < start && start < part.end) else { continue };
        let end = std::mem::replace(&mut layer.parts[at].end, start);
        layer.parts.insert(at + 1, OpinionPart { kind, author: Some(author), start, end });
    }
}
