//! Paragraphs: one to a place, rejoined across a page, numbered, and who speaks them.

use super::*;

/// One paragraph to a place: the engine's numbered hull stops at the next paragraph, heading or note; one that repeats
/// another gives way to the numbered one; a paragraph that is mostly a note is the note.
pub(super) fn settle(paragraphs: &mut Vec<StructureNode>, headings: &[StructureNode], notes: &[ScalarRange]) {
    paragraphs.sort_by_key(|paragraph| (paragraph.range.start, paragraph.label.is_none()));
    let mut stops = headings.iter().map(|heading| heading.range.start).chain(notes.iter().map(|note| note.range.start)).collect::<Vec<_>>();
    stops.sort_unstable();
    let mut kept = Vec::<StructureNode>::with_capacity(paragraphs.len());
    let mut notes = notes.to_vec();
    notes.sort_by_key(|note| note.range.end);
    // A note a page's foot set in a paragraph's course: the words past it are still the paragraph's.
    let mut pieces = Vec::with_capacity(paragraphs.len());
    for mut paragraph in std::mem::take(paragraphs) {
        for note in &notes[notes.partition_point(|note| note.range.end <= paragraph.range.start)..] {
            if note.range.start >= paragraph.range.end {
                break;
            }
            if paragraph.range.start < note.range.start && note.range.end < paragraph.range.end {
                let mut head = paragraph.clone();
                head.end = note.range.start;
                paragraph.range.start = note.range.end;
                paragraph.label = (None).map(|number| format!("par{number}"));
                pieces.push(head);
            }
        }
        pieces.push(paragraph);
    }
    for mut paragraph in pieces {
        if let Some(stop) = stops.get(stops.partition_point(|stop| *stop <= paragraph.range.start)).filter(|stop| **stop < paragraph.range.end) {
            paragraph.range.end = *stop;
        }
        let touched = &notes[notes.partition_point(|note| note.range.end <= paragraph.range.start)..];
        let covered = touched.iter().take_while(|note| note.range.start < paragraph.range.end || note.range.end < paragraph.range.end)
            .map(|note| note.range.end.min(paragraph.range.end).saturating_sub(note.range.start.max(paragraph.range.start))).sum::<usize>();
        if covered * 2 >= (paragraph.range.end - paragraph.range.start).max(1) {
            continue;
        }
        if let Some(last) = kept.last_mut() {
            if paragraph.range.start <= last.start + 2 {
                if last.number.is_none() && paragraph.label.is_some() {
                    *last = paragraph;
                }
                continue;
            }
            if paragraph.range.start < last.end {
                last.end = paragraph.range.start;
            }
        }
        if paragraph.range.end > paragraph.range.start {
            kept.push(paragraph);
        }
    }
    *paragraphs = kept;
}

/// Paragraph numbers the text prints but the markup does not mark ("¶ 12", "{12}", "[12]", "12.").
pub(super) fn number_paragraphs(structure: &mut DocumentStructure, pending: &mut Pending) {
    let mut paragraphs = take(structure, Of::Paragraphs);

    put(structure, std::mem::take(&mut paragraphs)); join_block_counters(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); numbers_off_titles(structure); paragraphs = take(structure, Of::Paragraphs);
    let coordinates = ScalarText::new(&structure.text);
    // A quotation's numbers are the quotation's.
    let openings = paragraphs.iter().enumerate().filter(|(_, paragraph)| !(paragraph.kind == NodeKind::Quote)).filter_map(|(index, paragraph)| {
        Some((index, trim_js(coordinates.slice_utf16(paragraph.range.start..paragraph.range.end)?)))
    }).collect::<Vec<_>>();
    let numbered = text::own_numbers(openings.iter().copied(), 3);
    let lettered = lettered_paragraphs(&openings, &numbered);
    for (index, number) in numbered {
        paragraphs[index].label.get_or_insert(format!("par{number}"));
    }
    for (index, label) in lettered {
        paragraphs[index].label.get_or_insert(format!("par{label}"));
    }
    // A certified question's own number.
    static QUESTION: OnceLock<Regex> = OnceLock::new();
    let question = QUESTION.get_or_init(|| Regex::new(r#"^["“]?(Question \d{1,2})[.:]"#).unwrap());
    let asked = paragraphs.iter().enumerate().filter(|(_, paragraph)| paragraph.label.is_none()).filter_map(|(index, paragraph)| {
        Some((index, question.captures(trim_js(coordinates.slice_utf16(paragraph.range.start..paragraph.range.end)?))?[1].to_owned()))
    }).collect::<Vec<_>>();
    for (index, label) in asked {
        paragraphs[index].label = Some(format!("par{}", label));
    }
    put(structure, std::mem::take(&mut paragraphs)); split_numbered_blocks(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); fill_skipped_numbers(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); pilcrow_read_as_one(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); fill_skipped_numbers(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); misread_numbers(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); carry_title_numbers(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); walk_scanned_numbers(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); numbers_from_elsewhere(structure, pending); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); fill_skipped_numbers(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); closing_numbers(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, std::mem::take(&mut paragraphs)); list_labels(structure); paragraphs = take(structure, Of::Paragraphs);
    put(structure, paragraphs);
}

/// The lines past the last numbered paragraph that went out of the paragraphs as the opinion's foot, "[¶ 16.] GILBERTSON, Chief
/// Justice, concurs.": a line that prints the number due is that paragraph, whatever else it says.
fn closing_numbers(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    let mut notes = take(structure, Of::Notes);
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> () {
    const LEAST: usize = 3;
    if { put(structure, std::mem::take(&mut paragraphs)); put(structure, std::mem::take(&mut notes)); let result = numbers_held(structure); paragraphs = take(structure, Of::Paragraphs); notes = take(structure, Of::Notes); result } < LEAST {
        return;
    }
    let Some((from, mut due)) = paragraphs.iter().rev().find_map(|paragraph| Some((paragraph.range.end, paragraph.label.as_deref().and_then(|label| label.strip_prefix("par")).as_deref()?.parse::<u32>().ok()? + 1))) else { return };
    let coordinates = ScalarText::new(&structure.text);
    let Some(rest) = coordinates.slice_utf16(from..utf16_len(&structure.text)) else { return };
    let held = paragraphs.iter().map(|paragraph| (paragraph.range.start, paragraph.range.end)).chain(notes.iter().flat_map(|note| note.ranges.iter().map(|range| (range.start, range.end)))).collect::<Vec<_>>();
    let mut at = from;
    let mut found = Vec::new();
    for line in rest.split_inclusive('\n') {
        let start = at + utf16_len(&line[..line.len() - line.trim_start_matches(javascript_whitespace).len()]);
        let words = trim_js(line);
        at += utf16_len(line);
        if !words.is_empty() && !held.iter().any(|range| range.0 <= start && start < range.1) && (text::paragraph_marker(words).is_some_and(|(number, _)| number == due) || scanned_number(words, due - 1, due + 1).is_some()) {
            found.push(read_paragraph(start, start + utf16_len(words), Some(due.to_string())));
            due += 1;
        }
    }
    if !found.is_empty() {
        paragraphs.append(&mut found);
        paragraphs.sort_by_key(|paragraph| paragraph.range.start);
    }
})();
    put(structure, paragraphs);
    put(structure, notes);
    result
}

/// Text nothing holds is the opinion's: paragraphs by the blank lines of a plain text, by the lines of markup. What the headnotes
/// already hold stays out. Ahead of the first paragraph only running prose counts: a reader can find its first paragraph late,
/// and a caption's lines are no prose.
pub(super) fn hold_leftovers(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> () {
    const WORDS: usize = 4;
    const PROSE_WORDS: usize = 15;
    let Some(first) = paragraphs.iter().map(|paragraph| paragraph.range.start).min() else { return };
    static BLANK: OnceLock<Regex> = OnceLock::new();
    let blank = BLANK.get_or_init(|| Regex::new(r"\n[ \t]*\n").unwrap());
    let plain = structure.opinion.as_mut().unwrap().rendition == "plain_text.txt";
    let coordinates = ScalarText::new(&structure.text);
    let summaries = structure.opinion.as_mut().unwrap().headnotes.iter().flat_map(|headnote| places(&headnote.text)).collect::<HashSet<_>>();
    let mut found = Vec::new();
    for (start, end) in unheld_stretches(structure) {
        let Some(raw) = coordinates.slice_utf16(start..end) else { continue };
        let cuts = if plain { blank.find_iter(raw).map(|cut| (cut.start(), cut.end())).collect::<Vec<_>>() } else { raw.match_indices('\n').map(|(at, _)| (at, at + 1)).collect() };
        let mut from = 0;
        for (cut, past) in cuts.into_iter().chain([(raw.len(), raw.len())]) {
            let piece = &raw[from..cut];
            let words = trim_js(piece);
            let lead = start + utf16_len(&raw[..from]) + utf16_len(&piece[..piece.len() - piece.trim_start_matches(javascript_whitespace).len()]);
            from = past;
            let count = words.split_whitespace().count();
            let running = || count >= PROSE_WORDS && words.split_whitespace().filter(|word| word.starts_with(char::is_lowercase)).count() * 5 >= count * 3;
            if count >= WORDS && words.chars().any(char::is_lowercase) && (lead >= first || running()) && !opening_words(words).is_some_and(|opening| summaries.contains(&opening)) {
                found.push(read_paragraph(lead, lead + utf16_len(words), None));
            }
        }
    }
    if !found.is_empty() {
        paragraphs.append(&mut found);
        paragraphs.sort_by_key(|paragraph| paragraph.range.start);
    }
})();
    put(structure, paragraphs);
    result
}

/// How many of the paragraphs carry a number.
fn numbers_held(structure: &DocumentStructure) -> usize {
    let paragraphs: Vec<StructureNode> = structure.nodes.iter().filter(|node| Of::Paragraphs.holds(node)).cloned().collect();
    paragraphs.iter().filter(|paragraph| paragraph.label.as_deref().and_then(|label| label.strip_prefix("par")).as_deref().is_some_and(|number| number.parse::<u32>().is_ok())).count()
}

/// A better reading replaces a worse one. A number of ours that stands between its new neighbours may stay: the reading can
/// have passed over its paragraph. One out of its order was a misreading: "[110]" where the opinion prints "[¶ 10]".
fn take_numbers(structure: &mut DocumentStructure, mut taken: Vec<(usize, u32)>) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    taken.sort_unstable();
    for index in 0..paragraphs.len() {
        let at = taken.partition_point(|entry| entry.0 < index);
        if taken.get(at).is_some_and(|entry| entry.0 == index) {
            continue;
        }
        let low = at.checked_sub(1).map_or(0, |before| taken[before].1);
        let high = taken.get(at).map_or(u32::MAX, |entry| entry.1);
        if paragraphs[index].label.as_deref().and_then(|label| label.strip_prefix("par")).and_then(|number| number.parse::<u32>().ok()).is_some_and(|number| number <= low || number >= high) {
            paragraphs[index].label = None;
        }
    }
    for (index, number) in taken {
        paragraphs[index].label = Some(format!("par{}", number.to_string()));
    }
    put(structure, paragraphs);
}

/// An opinion numbered throughout under a scanner: "[11] [T2] [13] [14]" is 1, 2, 3, 4. Walked from 1, each paragraph tried for
/// the one number due. A count that climbs by ones over most of the opinion is no accident, however its marks were read.
fn walk_scanned_numbers(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);

    const LEAST: usize = 4;
    /// How many marks in a row the walk may find past reading.
    const STEP: u32 = 2;
    let coordinates = ScalarText::new(&structure.text);
    let mut due = 1;
    let mut taken = Vec::new();
    let mut open = 0;
    for (index, paragraph) in paragraphs.iter().enumerate().filter(|(_, paragraph)| !(paragraph.kind == NodeKind::Quote)) {
        open += 1;
        let opening = coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map_or("", trim_js);
        // A number on a line of its own is a source's block counter.
        if opening.split_once('\n').is_some_and(|(line, _)| line.trim().bytes().all(|byte| byte.is_ascii_digit())) {
            continue;
        }
        // A mark may be past reading, "118" for "¶ 13": the next one or two say the count has gone on.
        let read = (due..=due + STEP).find(|number| paragraph.label.as_deref().and_then(|label| label.strip_prefix("par")).as_deref() == Some(number.to_string().as_str()) || text::paragraph_marker(opening).is_some_and(|(marked, _)| marked == *number) || scanned_number(opening, number - 1, number + 1).is_some());
        if let Some(number) = read {
            taken.push((index, number));
            due = number + 1;
        }
    }
    if taken.len() >= LEAST && taken.len() * 2 >= open && taken.len() > { put(structure, std::mem::take(&mut paragraphs)); let result = numbers_held(structure); paragraphs = take(structure, Of::Paragraphs); result } {
        put(structure, std::mem::take(&mut paragraphs)); take_numbers(structure, taken); paragraphs = take(structure, Of::Paragraphs);
    }
    put(structure, paragraphs);
}

/// Numbers another rendition prints where ours lost them, from the rendition that prints the most, each on the paragraph its
/// words open. A rendition whose words mostly fall in no paragraph of ours is reading something else, its notes as like as not.
fn numbers_from_elsewhere(structure: &mut DocumentStructure, pending: &mut Pending) {
    let mut paragraphs = take(structure, Of::Paragraphs);

    const LEAST: usize = 3;
    const REACH: usize = 40;
    let best = std::mem::take(&mut pending.printed_elsewhere).iter().map(|(named, numbers)| {
        let mut taken = BTreeMap::new();
        for &(number, at) in numbers {
            let Some(index) = paragraphs.partition_point(|paragraph| paragraph.range.start <= at).checked_sub(1) else { continue };
            let paragraph = &paragraphs[index];
            if !(paragraph.kind == NodeKind::Quote) && at < paragraph.range.end && at - paragraph.range.start <= REACH {
                taken.entry(index).or_insert(number);
            }
        }
        (*named, (taken.len() * 2 >= numbers.len()).then(|| taken.into_iter().collect::<Vec<_>>()).unwrap_or_default())
    }).max_by_key(|(_, taken)| taken.len()).unwrap_or_default();
    let (named, best) = best;
    // Fewer than ours is still the better reading where they run in order: ours may hold every mark, half of them misread.
    let held = put(structure, std::mem::take(&mut paragraphs)); numbers_held(structure); paragraphs = take(structure, Of::Paragraphs);
    if best.len() >= LEAST && (best.len() > held || named && best.len() * 2 >= held && best.windows(2).all(|pair| pair[0].1 < pair[1].1)) {
        put(structure, std::mem::take(&mut paragraphs)); take_numbers(structure, best); paragraphs = take(structure, Of::Paragraphs);
    }
    put(structure, paragraphs);
}

/// Every paragraph that opens on a list's label keeps it, numbered run or not.
fn list_labels(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    let mut notes = take(structure, Of::Notes);

    static LABEL: OnceLock<Regex> = OnceLock::new();
    // A block's counter may stand ahead of the label: "6\n(A) verify the wages".
    let label = LABEL.get_or_init(|| Regex::new(r"^(?:\d{1,4}\s*\n\s*)?(\(?(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})\)|(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})[.)])\s+\S").unwrap());
    static WITHIN: OnceLock<Regex> = OnceLock::new();
    let within = WITHIN.get_or_init(|| Regex::new(r"(?m)^[ \t]*(\(?(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})\)|(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})[.)])[ \t]+\S").unwrap());
    let coordinates = ScalarText::new(&structure.text);
    // A court's own named paragraph: an advisory opinion's question and answer, a certified question.
    static NAMED: OnceLock<Regex> = OnceLock::new();
    let named = NAMED.get_or_init(|| Regex::new(r"^(QUESTIONS?(?: (?:NO\. )?(?:\d{1,2}|[IVX]{1,4}))?(?: PRESENTED)?|WE ANSWER|ANSWERS?(?: (?:NO\. )?(?:\d{1,2}|[IVX]{1,4}))?|CERTIFIED QUESTIONS?|Certified [Qq]uestions?|Question(?: (?:No\. )?\d{1,2})?(?: [Pp]resented)?|Answer(?: \d{1,2})?)[:.]").unwrap());
    let items = paragraphs.iter().map(|paragraph| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map(trim_js).and_then(|opening| Some(label.captures(opening).or_else(|| named.captures(opening))?[1].to_owned()))).collect::<Vec<_>>();
    for (paragraph, item) in paragraphs.iter_mut().zip(items) {
        if let Some(item) = item { paragraph.properties.insert("item".to_owned(), item); }
    }
    let lists = notes.iter().map(|note| note.ranges.iter().filter_map(|range| coordinates.slice_utf16(range.start..range.end)).flat_map(|body| within.captures_iter(body).map(|capture| capture[1].to_owned())).collect::<Vec<_>>()).collect::<Vec<_>>();
    for (note, items) in notes.iter_mut().zip(lists) {
        note.properties.insert("items".to_owned(), items.join("\n"));
    }
    put(structure, paragraphs);
    put(structure, notes);
}

/// A block's counter split from its words, a paragraph of "57" alone: it opens the paragraph after it.
fn join_block_counters(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);

    let bare = {
        let coordinates = ScalarText::new(&structure.text);
        paragraphs.iter().map(|paragraph| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map(trim_js).is_some_and(|printed| !printed.is_empty() && printed.len() <= 4 && printed.bytes().all(|byte| byte.is_ascii_digit()))).collect::<Vec<_>>()
    };
    for index in (0..paragraphs.len().saturating_sub(1)).rev() {
        if bare[index] && !bare[index + 1] && paragraphs[index].label.is_none() {
            let counter = paragraphs.remove(index);
            paragraphs[index].range.start = counter.start;
        }
    }
    put(structure, paragraphs);
}

/// A title under a paragraph's mark, "¶ 30 Conclusion": the mark stands apart as a paragraph until the numbers are read. A
/// numbered sentence whose number the paragraphs either side leave open, "6. Robert Poe is the Chairman.", is no title at all;
/// an outline's "3. The Order Was Proper." sits among paragraphs numbered in their own right.
fn numbers_off_titles(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    let mut headings = take(structure, Of::Headings);
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> () {
    let coordinates = ScalarText::new(&structure.text);
    let opening = |paragraph: &StructureNode| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map(trim_js).unwrap_or("");
    let plain_numbers = paragraphs.iter().filter_map(|paragraph| text::paragraph_marker(opening(paragraph)).filter(|(_, family)| *family == 4).map(|(number, _)| number)).collect::<HashSet<_>>();
    let left_open = |number: u32| !plain_numbers.contains(&number) && plain_numbers.contains(&(number + 1)) && (number == 1 || plain_numbers.contains(&(number - 1)));
    let stub = |start: usize, end: usize| read_paragraph(start, end, None);
    // A paragraph that opens with the title already holds its number.
    let held = paragraphs.iter().map(|paragraph| paragraph.range.start).collect::<HashSet<_>>();
    let mut stubs = Vec::new();
    headings.retain_mut(|heading| {
        let Some(printed) = coordinates.slice_utf16(heading.range.start..heading.range.end).filter(|_| !held.contains(&heading.range.start)) else { return true };
        let opened = trim_js(printed);
        let title = text::past_marker(opened);
        if text::says_paragraph(opened) {
            if let Some(at) = printed.find(title).filter(|_| !title.is_empty()) {
                let start = heading.range.start + utf16_len(&printed[..at]);
                stubs.push(stub(heading.range.start, start));
                heading.range.start = start;
            }
            return true;
        }
        let sentence = heading.grammar.as_deref() != Some("other-rendition") && text::paragraph_marker(opened).is_some_and(|(number, family)| family == 4 && left_open(number))
            && text::ended_sentence(opened) && title.split_whitespace().count() >= 5 && !title_cased(title);
        if sentence {
            stubs.push(stub(heading.range.start, heading.range.end));
        }
        !sentence
    });
    if !stubs.is_empty() {
        paragraphs.append(&mut stubs);
        paragraphs.sort_by_key(|paragraph| paragraph.range.start);
    }
})();
    put(structure, paragraphs);
    put(structure, headings);
    result
}

/// A scanner's "1" for "¶" fuses with the number: "15, 116, 118, 119" is 15, 16, 18, 19. Taken only where the number
/// without its first digit fits the one before, and the next fits it, as printed or fused alike. A printed "¶ 17" is no such
/// thing: a separate opinion may open at it.
fn pilcrow_read_as_one(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> () {
    let printed = {
        let coordinates = ScalarText::new(&structure.text);
        paragraphs.iter().map(|paragraph| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map_or(false, |opening| trim_js(opening).trim_start_matches(['[', '{']).starts_with('¶'))).collect::<Vec<_>>()
    };
    let shed = |number: u32| number.to_string().strip_prefix('1').and_then(|rest| rest.parse::<u32>().ok()).filter(|rest| *rest > 0);
    let numbered = paragraphs.iter().enumerate().filter_map(|(index, paragraph)| Some((index, paragraph.label.as_deref().and_then(|label| label.strip_prefix("par")).as_deref()?.parse::<u32>().ok()?))).collect::<Vec<_>>();
    let mut last = 0;
    for (at, &(index, number)) in numbered.iter().enumerate() {
        let fits = shed(number).filter(|rest| !printed[index] && number > last + 3 && *rest > last && *rest <= last + 3).filter(|rest| {
            numbered.get(at + 1).is_none_or(|next| next.1 > *rest && next.1 <= rest + 3 || shed(next.1).is_some_and(|after| after > *rest && after <= rest + 3))
        });
        if let Some(rest) = fits {
            paragraphs[index].label = Some(format!("par{}", rest.to_string()));
        }
        last = fits.unwrap_or(number);
    }
})();
    put(structure, paragraphs);
    result
}

/// A number left alone by the title it stood on belongs to the text under the title, unless that has its own.
fn carry_title_numbers(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);

    let coordinates = ScalarText::new(&structure.text);
    let bare = paragraphs.iter().enumerate().filter(|(_, paragraph)| paragraph.label.is_some()).filter(|(_, paragraph)| {
        coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map(trim_js).is_some_and(|opening| text::says_paragraph(opening) && text::past_marker(opening).is_empty())
    }).map(|(index, _)| index).collect::<Vec<_>>();
    for index in bare.into_iter().rev() {
        if paragraphs.get(index + 1).is_some_and(|next| next.number.is_none() && !next.quote) {
            let number = paragraphs.remove(index).number;
            paragraphs[index].label = number.map(|number| format!("par{number}"));
        }
    }
    put(structure, paragraphs);
}

/// In an opinion that numbers its paragraphs a number its neighbours contradict was misread: "1, 2, 8, 4" is 3, and "1, 3, 2, 4" two
/// marks swapped. The neighbours say so only where they run by ones either side.
fn misread_numbers(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> () {
    let mut numbered = paragraphs.iter().enumerate().filter_map(|(index, paragraph)| Some((index, paragraph.label.as_deref().and_then(|label| label.strip_prefix("par")).as_deref()?.parse::<u32>().ok()?))).collect::<Vec<_>>();
    if numbered.len() * 2 < paragraphs.len() {
        return;
    }
    for at in 1..numbered.len().saturating_sub(1) {
        let (before, here, after) = (numbered[at - 1].1, numbered[at].1, numbered[at + 1].1);
        if after == before + 2 && here != before + 1 {
            numbered[at].1 = before + 1;
            paragraphs[numbered[at].0].number = Some((before + 1).to_string());
        } else if here == before + 2 && after == before + 1 && numbered.get(at + 2).is_some_and(|next| next.1 == before + 3) {
            numbered[at].1 = before + 1;
            numbered[at + 1].1 = before + 2;
            paragraphs[numbered[at].0].number = Some((before + 1).to_string());
            paragraphs[numbered[at + 1].0].number = Some((before + 2).to_string());
        }
    }
})();
    put(structure, paragraphs);
    result
}

/// Where a number due next stands in a block's course after a finished sentence, "... complaints. ¶21 On appeal", a new
/// paragraph opens. It is due while the numbered paragraphs that follow have not printed it.
fn split_numbered_blocks(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> () {
    let number_of = |paragraph: &StructureNode| paragraph.label.as_deref().and_then(|label| label.strip_prefix("par")).as_deref().and_then(|number| number.parse::<u32>().ok());
    if paragraphs.iter().filter(|paragraph| number_of(paragraph).is_some()).count() < 3 {
        return;
    }
    let coordinates = ScalarText::new(&structure.text);
    let upcoming = {
        let mut later = None;
        let mut upcoming = paragraphs.iter().rev().map(|paragraph| { let next = later; later = number_of(paragraph).or(later); next }).collect::<Vec<_>>();
        upcoming.reverse();
        upcoming
    };
    let mut kept = Vec::with_capacity(paragraphs.len());
    // Numbers that open past 1 leave 1 due: it may stand on the author's line, "GILBERTSON, Chief Justice [¶1.] Scotty".
    let mut due = paragraphs.iter().find_map(number_of).filter(|first| *first > 1).map(|_| 1);
    for (index, mut paragraph) in std::mem::take(&mut paragraphs).into_iter().enumerate() {
        due = number_of(&paragraph).map(|number| number + 1).or(due);
        while let Some(number) = due.filter(|number| !(paragraph.kind == NodeKind::Quote) && upcoming[index].is_none_or(|next| *number < next)) {
            let Some(raw) = coordinates.slice_utf16(paragraph.range.start..paragraph.range.end) else { break };
            let Some(place) = marker_in_course(raw, number) else { break };
            let split = paragraph.range.start + utf16_len(&raw[..place]);
            let mut next = paragraph.clone();
            next.start = split;
            next.number = Some(number.to_string());
            paragraph.range.end = split;
            kept.push(std::mem::replace(&mut paragraph, next));
            due = Some(number + 1);
        }
        kept.push(paragraph);
    }
    paragraphs = kept;
})();
    put(structure, paragraphs);
    result
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
pub(super) fn fill_skipped_numbers(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);

    let coordinates = ScalarText::new(&structure.text);
    let numbered = paragraphs.iter().enumerate().filter_map(|(index, paragraph)| Some((index, paragraph.label.as_deref().and_then(|label| label.strip_prefix("par")).as_deref()?.parse::<u32>().ok()?))).collect::<Vec<_>>();
    let marked_between = |span: std::ops::Range<usize>, low: u32, high: u32| {
        let (mut last, mut found) = (low, Vec::new());
        for index in span {
            let opening = coordinates.slice_utf16(paragraphs[index].range.start..paragraphs[index].range.end).map(trim_js).unwrap_or("");
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
        if high > low + 1 && to - from == (high - low) as usize && (from + 1..to).all(|index| !(paragraphs[index].kind == NodeKind::Quote)) {
            filled.extend((from + 1..to).zip(low + 1..high));
            continue;
        }
        filled.extend(marked_between(from + 1..to, low, high));
    }
    // Before the first number the opinion's first marks may stand in what a scanner made of them. One stray "[1]" is no such
    // run: the marks must fill most of the way up, or the opinion be numbered throughout.
    if let Some(&(to, high)) = numbered.first() {
        let found = marked_between(0..to, 0, high);
        if found.len() * 2 + 1 >= high as usize || numbered.len() * 2 >= paragraphs.len() {
            filled.extend(found);
        }
    }
    // A run from 2 whose first paragraph opens on what a scanner made of "1".
    static FIRST: OnceLock<Regex> = OnceLock::new();
    let first = FIRST.get_or_init(|| Regex::new(r"^\W{0,2}[1IlL][.)T]?\s").unwrap());
    if let Some(&(index, 2)) = numbered.first() {
        let opening = index.checked_sub(1).and_then(|before| coordinates.slice_utf16(paragraphs[before].range.start..paragraphs[before].range.end)).map_or("", trim_js);
        if first.is_match(opening) {
            filled.push((index - 1, 1));
        }
    }
    for (index, number) in filled {
        paragraphs[index].label = Some(format!("par{}", number.to_string()));
    }
    put(structure, paragraphs);
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
pub(super) fn rejoin_paragraphs(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    let mut notes = take(structure, Of::Notes);
    let mut headings = take(structure, Of::Headings);

    let coordinates = ScalarText::new(&structure.text);
    let printed = |paragraph: &StructureNode| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map(trim_js).unwrap_or("");
    // A plain text's short lines are its wrapping, not its margin.
    let columned = structure.opinion.as_mut().unwrap().rendition != "plain_text.txt";
    let interleaved = (1..paragraphs.len().saturating_sub(1)).filter(|&index| {
        let (before, line, after) = (&paragraphs[index - 1], &paragraphs[index], &paragraphs[index + 1]);
        columned && !line.quote && line.number.is_none() && printed(line).len() < 60 && printed(line).starts_with(char::is_uppercase)
            && !text::ended_sentence(printed(before)) && printed(after).starts_with(char::is_lowercase)
    }).collect::<Vec<_>>();
    for &index in interleaved.iter().rev() {
        let line = paragraphs.remove(index);
        structure.opinion.as_mut().unwrap().headnotes.push(headnote_of("catchword", squeezed(printed(&line)), Some(ScalarRange { start: line.start, end: line.end })));
    }
    let mut kept = Vec::<StructureNode>::with_capacity(paragraphs.len());
    let mut titles = headings.iter().map(|heading| heading.range.start).collect::<Vec<_>>();
    titles.sort_unstable();
    let mut notes = notes.iter().flat_map(|note| note.ranges.iter().map(|range| range.start)).collect::<Vec<_>>();
    notes.sort_unstable();
    for mut paragraph in std::mem::take(&mut paragraphs) {
        if let Some(last) = kept.last_mut() {
            let between = |marks: &[usize]| marks.get(marks.partition_point(|mark| *mark < last.end)).is_some_and(|mark| *mark < paragraph.range.start);
            if !between(&titles) && last.quote == (paragraph.kind == NodeKind::Quote) && paragraph.label.is_none() && !text::ended_sentence(printed(last)) && printed(&paragraph).starts_with(char::is_lowercase) {
                if !between(&notes) {
                    last.end = paragraph.range.end;
                    continue;
                }
                if true { paragraph.properties.insert("continues".to_owned(), String::new()); } else { paragraph.properties.remove("continues"); }
            }
        }
        kept.push(paragraph);
    }
    paragraphs = kept;
    put(structure, paragraphs);
    put(structure, notes);
    put(structure, headings);
}

/// Paragraphs that hold no words: a rule, or a page mark's whole block once it has gone.
pub(super) fn drop_empty_paragraphs(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);

    let coordinates = ScalarText::new(&structure.text);
    let spoken = |paragraph: &StructureNode| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).is_some_and(|text| text.chars().any(|c| c.is_alphanumeric() || c == '*'));
    let kept = std::mem::take(&mut paragraphs).into_iter().filter(|paragraph| spoken(paragraph)).collect();
    paragraphs = kept;
    put(structure, paragraphs);
}

/// Who speaks each paragraph of an exchange. "Q." and "A." count only where a record has several of each, since "A." is
/// also an outline's letter.
pub(super) fn speakers(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    let mut headings = take(structure, Of::Headings);

    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^(Q\.|A\.|THE COURT:|The Court:|THE WITNESS:|The Witness:|M(?:R|S|RS)\.\s+\p{Lu}[\p{Lu}'’-]+:)\s").unwrap());
    let coordinates = ScalarText::new(&structure.text);
    let found = paragraphs.iter().enumerate().filter_map(|(index, paragraph)| {
        let printed = trim_js(coordinates.slice_utf16(paragraph.range.start..paragraph.range.end)?);
        Some((index, label.captures(printed)?[1].trim_end_matches(':').to_owned()))
    }).collect::<Vec<_>>();
    let questions = found.iter().filter(|(_, speaker)| speaker == "Q.").count();
    if questions >= 2 {
        let (answers, headings): (Vec<_>, Vec<_>) = std::mem::take(&mut headings).into_iter()
            .partition(|heading| coordinates.slice_utf16(heading.range.start..heading.range.end).map(trim_js).is_some_and(|printed| printed.starts_with("A. ") || printed.starts_with("Q. ")));
        headings = headings;
        paragraphs.extend(answers.into_iter().map(|heading| { let mut paragraph = read_paragraph(heading.range.start, heading.range.end, None); paragraph.properties.insert("speaker".to_owned(), "A.".to_owned()); paragraph }));
    }
    for (index, speaker) in found {
        if !matches!(speaker.as_str(), "Q." | "A.") || questions >= 2 {
            paragraphs[index].properties.insert("speaker".to_owned(), speaker);
        }
    }
    paragraphs.sort_by_key(|paragraph| paragraph.range.start);
    put(structure, paragraphs);
    put(structure, headings);
}

/// A concurrence or dissent that opens inside a flat body, on a line of its own: "SMITH, J., dissenting.".
pub(super) fn separate_parts(structure: &mut DocumentStructure) {
    let mut paragraphs = take(structure, Of::Paragraphs);
    let mut headings = take(structure, Of::Headings);
    let mut parts = take(structure, Of::Parts);
    #[allow(clippy::redundant_closure_call)]
    let result = (|| -> () {
    /// A writing's opening line, its joiners named, runs no longer than this.
    const LINE_LONG: usize = 460;
    /// How much of a line's text the opening's tests read.
    const OPENING_READ: usize = 520;
    static OPENS: OnceLock<Regex> = OnceLock::new();
    if parts.is_empty() {
        return;
    }
    let opens = OPENS.get_or_init(|| Regex::new(r"(?i)^(?:(?:mr\.\s+|chief\s+|associate\s+)*(?:justice|judge)\s+)?[\p{Lu}][\p{L}'’.\- ]{1,50}?(?:,?\s+(?:c\.\s*)?j\.?j?\.?|,\s*(?:[a-z]\.){2,4}|,?\s+(?:chief\s+|circuit\s+|district\s+|presiding\s+)?(?:judge|justice))?(?:,?\s+with\s+wh\w{1,2}\s+[^:]{1,300}?,)?\s*,?\s*\(?(?:specially\s+)?(concurring|dissenting|concurs|dissents)\b[^.]{0,80}\.?\)?:?$").unwrap());
    static RUN_IN: OnceLock<Regex> = OnceLock::new();
    let run_in = RUN_IN.get_or_init(|| Regex::new(r"^[\p{Lu}][\p{Lu}'’.\-]+,\s[^.:]{0,100}?\b(concurring|dissenting)\b[^.:]{0,60}[:.]").unwrap());
    // Any case's name, with the office to say it is a judge: "Hawkins, J., dissenting. I would ..."
    static NAMED: OnceLock<Regex> = OnceLock::new();
    let named = NAMED.get_or_init(|| Regex::new(r"^[\p{Lu}][\p{L}'’.\- ]{1,40},\s(?:(?:C\.\s?|P\.\s?)?J\.|Chief Justice|Justice|Judge)[,:]?\s*\(?(concurring|dissenting)\b[^.:]{0,60}[:.)]").unwrap());
    // "Justice THOMAS, with whom Justice SCALIA joins in all but\nPart III-A-1-b, dissenting." may run long, wrap, and run on.
    static WHOM: OnceLock<Regex> = OnceLock::new();
    let whom = WHOM.get_or_init(|| Regex::new(r"(?is)^.{1,120}?\bwith\s+wh\w{1,2}\b.{1,300}?\b(?:concurring|dissenting)\b[^.]{0,80}\.").unwrap());
    // What stands ahead of the paragraph's words: a counter, a printed page, a paragraph number.
    static LEAD: OnceLock<Regex> = OnceLock::new();
    let lead = LEAD.get_or_init(|| Regex::new(r"^[\s\d*¶.]*").unwrap());
    // "with whom Mr. Justice Burton concurs, dissenting" is a dissent; "concurring in part and dissenting in part" is both.
    static VERDICT: OnceLock<Regex> = OnceLock::new();
    let verdict = VERDICT.get_or_init(|| Regex::new(r"(?i),\s*\(?(?:specially\s+)?(?:concurring|dissenting)\b.*$").unwrap());
    let kind_of = |line: &str| {
        let lower = line.to_lowercase();
        let lower = verdict.find(&lower).map_or(lower.as_str(), |found| found.as_str());
        match (lower.contains("dissent"), lower.contains("concur")) {
            (true, true) => "concurrence and dissent",
            (true, false) => "dissent",
            _ => "concurrence",
        }.to_owned()
    };
    let coordinates = ScalarText::new(&structure.text);
    // Is a writing opening here? `inner` is a line inside a flat body, where only a finished sentence can open one: a wrapped
    // "Kavanaugh, J., dissenting)." or a running head "ALITO, J., concurring" does not.
    static A_JUDGE: OnceLock<Regex> = OnceLock::new();
    let a_judge = A_JUDGE.get_or_init(|| Regex::new(r"(?i)\b(?:justices?|judges?|c\.\s?j|jj?)\b[.,:\s]|^[\p{Lu}'’.\-]+,\s").unwrap());
    static POSSESSIVE: OnceLock<Regex> = OnceLock::new();
    let possessive = POSSESSIVE.get_or_init(|| Regex::new(r"(?i)^[^,]{0,60}['’]s?\s+(?:dissent|concurr)").unwrap());
    // "Concurring Opinion of ACOBA, J., with whom RAMIL, J., joins."
    static WRITTEN_BY: OnceLock<Regex> = OnceLock::new();
    let written_by = WRITTEN_BY.get_or_init(|| Regex::new(r"(?i)^(?:concurring|dissenting)(?:\s+and\s+(?:concurring|dissenting))?\s+opinion\s+(?:of|by)\b.{2,120}$").unwrap());
    let opening = |printed: &str, inner: bool| -> Option<(String, String)> {
        let finished = |line: &str| !inner || (line.trim_end().ends_with(['.', ':']) && line.matches(')').count() <= line.matches('(').count());
        let first_line = printed.split('\n').next().unwrap_or(printed).trim_end();
        let wrapped = first_line.ends_with(',').then(|| printed.match_indices('\n').nth(1).map_or(printed, |(end, _)| &printed[..end])).map(str::trim_end);
        for line in [whom.find(printed).map(|found| found.as_str()), (!inner).then_some(printed), wrapped, Some(first_line)].into_iter().flatten() {
            if let Some(capture) = opens.captures(line).filter(|_| line.len() <= LINE_LONG && finished(line) && a_judge.is_match(line) && !possessive.is_match(line)) {
                // "HALL, J., concurs." says who joined: it opens no writing. One that names an opinion, or runs on, does.
                if matches!(capture[1].to_lowercase().as_str(), "concurs" | "dissents") && line.ends_with('.') && !line.to_lowercase().contains("opinion") {
                    return None;
                }
                return Some((kind_of(line), squeezed(line)));
            }
        }
        if written_by.is_match(first_line) {
            return Some((kind_of(first_line), squeezed(first_line)));
        }
        let capture = run_in.captures(printed).or_else(|| named.captures(printed)).filter(|capture| finished(&capture[0]))?;
        Some((kind_of(&capture[0]), squeezed(&capture[0])))
    };
    let mut breaks = paragraphs.iter().flat_map(|paragraph| {
        let body = coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).unwrap_or("");
        // Lines start where the line before finished a sentence or stood empty.
        let (mut places, mut bytes, mut units) = (vec![(0, 0, false)], 0, 0);
        for line in body.split_inclusive('\n') {
            bytes += line.len();
            units += line.encode_utf16().count();
            if line.trim_end().is_empty() || line.trim_end().ends_with(['.', ':', '!', '?', '”', '"', ')']) {
                places.push((bytes, units, true));
            }
        }
        places.into_iter().filter(|place| place.0 < body.len()).filter_map(|(byte, unit, inner)| {
            let rest = &body[byte..];
            let rest = trim_js(&rest[lead.find(rest).map_or(0, |found| found.end())..]);
            let read = rest.char_indices().nth(OPENING_READ).map_or(rest, |(end, _)| &rest[..end]);
            let lower = read.to_lowercase();
            if !lower.contains("concur") && !lower.contains("dissent") {
                return None;
            }
            opening(read, inner).map(|(kind, author)| (paragraph.range.start + unit, kind, author))
        }).collect::<Vec<_>>()
    }).chain(headings.iter().filter_map(|heading| {
        // A writing's opening set as a heading.
        let printed = trim_js(coordinates.slice_utf16(heading.range.start..heading.range.end)?);
        opening(&printed[lead.find(printed).map_or(0, |found| found.end())..], false).map(|(kind, author)| (heading.range.start, kind, author))
    })).collect::<Vec<_>>();
    // A second author's line, past the opinion's first text, opens another opinion.
    for line in structure.opinion.as_mut().unwrap().front.iter().filter(|line| line.role == "author") {
        let Some(range) = line.range else { continue };
        let written = paragraphs.iter().filter(|paragraph| paragraph.range.end <= range.start).map(|paragraph| paragraph.range.end - paragraph.range.start).sum::<usize>();
        if written >= 400 && !breaks.iter().any(|known| known.0.abs_diff(range.start) < 4) {
            let says = { let lower = line.text.to_lowercase(); lower.contains("dissent") || lower.contains("concur") };
            breaks.push((range.start, if says { kind_of(&line.text) } else { "opinion".to_owned() }, line.text.clone()));
        }
    }
    // A later document stacked in the record, under a title of its own.
    static STACKED: OnceLock<Regex> = OnceLock::new();
    let stacked = STACKED.get_or_init(|| Regex::new(r"(?i)^(appendix\b.{0,40}|(?:opinion |order )?on (?:petition for )?rehearing.{0,30}|supplemental opinion)$").unwrap());
    // An order or judgment under a title of its own, once a good deal has been written: a separate ruling. At the top
    // of a record the same word is its title.
    static RULING: OnceLock<Regex> = OnceLock::new();
    let ruling = RULING.get_or_init(|| Regex::new(r"(?i)^(?:(?:amended |final |supplemental )?order(?: of (?:the )?court)?|(?:final )?judgment|decree|attachment\b.{0,40})\.?$").unwrap());
    for heading in &headings {
        let printed = coordinates.slice_utf16(heading.range.start..heading.range.end).map(trim_js).unwrap_or("");
        if heading.range.start > 0 && stacked.is_match(printed) {
            let kind = if printed.to_lowercase().starts_with("appendix") { "appendix" } else { "rehearing" };
            breaks.push((heading.range.start, kind.to_owned(), squeezed(printed)));
        } else if printed.starts_with(char::is_uppercase) && ruling.is_match(printed) {
            let written = paragraphs.iter().filter(|paragraph| paragraph.range.end <= heading.range.start).map(|paragraph| paragraph.range.end - paragraph.range.start).sum::<usize>();
            if written >= 1500 {
                let kind = if printed.to_lowercase().starts_with("attachment") { "attachment" } else if printed.to_lowercase().contains("judgment") { "judgment" } else { "order" };
                breaks.push((heading.range.start, kind.to_owned(), squeezed(printed)));
            }
        }
    }
    breaks.sort_by_key(|known| known.0);
    let nexts = breaks.iter().skip(1).map(|known| known.0).chain([usize::MAX]).collect::<Vec<_>>();
    let mut following = nexts.iter();
    // A part has text of its own: a paragraph after its line, or a line that runs on into its opinion.
    const LINE_ALONE: usize = 170;
    breaks.retain(|known| {
        let next = *following.next().unwrap_or(&usize::MAX);
        paragraphs.iter().any(|paragraph| (known.0 < paragraph.range.start && paragraph.range.start < next) || (paragraph.range.start <= known.0 && paragraph.range.end > known.0 + LINE_ALONE))
    });
    // What a source already sets apart stands; a break cuts the part it falls in.
    const SAME_START: usize = 40;
    for (start, kind, author) in breaks {
        if parts.iter().any(|part| part.range.start.abs_diff(start) < SAME_START) {
            continue;
        }
        let Some(at) = parts.iter().position(|part| part.range.start < start && start < part.range.end) else { continue };
        let end = std::mem::replace(&mut parts[at].range.end, start);
        parts.insert(at + 1, read_part(kind, Some(author), start, end));
    }
    // "Justice BAER files a concurring opinion." says what the part under that author's name is.
    static FILES: OnceLock<Regex> = OnceLock::new();
    let files = FILES.get_or_init(|| Regex::new(r"(?m)^\s*(?:(?:Mr\.|Mrs\.|Ms\.|Chief|Justice|Judge)\s+)*([\p{Lu}][\p{L}'’\-]+)[^.\n]{0,40}?\b(?:files?|filed)\s+an?\s+((?:concurring|dissenting)(?:\s+and\s+(?:concurring|dissenting))?)\s+opinion").unwrap());
    let stated = files.captures_iter(&structure.text).map(|capture| (capture[1].to_lowercase(), kind_of(&capture[2]))).collect::<Vec<_>>();
    for part in parts.iter_mut().filter(|part| part.display_value.as_deref().unwrap_or("") == "opinion") {
        let author = part.properties.get("author").map(String::as_str).unwrap_or("").to_lowercase();
        if let Some((_, kind)) = stated.iter().find(|(name, _)| author.contains(name.as_str())) {
            part.display_value.as_deref().unwrap_or("") = kind.clone();
        }
    }
})();
    put(structure, paragraphs);
    put(structure, headings);
    put(structure, parts);
    result
}
