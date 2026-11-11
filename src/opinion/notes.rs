//! A record's notes beyond what the engine links: labels as printed, anchors, flat notes, and calls matched to bodies.

use super::*;

/// A label as called and as printed compare bare: "(1)", "[1]" and "1" are one note's.
pub(super) fn plain_label(label: &str) -> &str {
    label.trim_matches(['(', ')', '[', ']', '.', ','])
}

/// A note's printed label, and how much of its body merely repeats it.
pub(super) fn note_label_of(coordinates: &ScalarText, note: &StructureNode) -> (String, usize) {
    note_opening(coordinates.slice_utf16(note.range.start..note.range.end).unwrap_or(""), note.label.as_deref().unwrap_or("").to_owned())
}

/// Notes another text of the case prints and this reading lacks: another rendition's, or the head matter's, where a
/// judge's or counsel's note sits ahead of the opinion. One is the same note if a note of ours shares most of its words,
/// whatever a scanner did to the rest; else it is new, and its words join the end of the reading text. Either way a note of
/// ours with no call takes the other's, placed by the words before it. `coordinates` index the other's text; the words a
/// new note brings index ours again.
pub(super) fn supply_notes(structure: &mut DocumentStructure, kinds: &mut Kinds, pending: &mut Pending, other: &DocumentStructure, coordinates: &ScalarText) {
    const SHINGLE: usize = 4;
    if !other.nodes.iter().any(|node| node.kind == NodeKind::Footnote) {
        return;
    }
    let words_of = |text: &str| word_starts(text).into_iter().map(|word| word.0).collect::<HashSet<_>>();
    let held = {
        let ours = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
        kinds.notes.iter().map(|note| {
            note.ranges.iter().filter_map(|range| ours.slice_utf16(range.start..range.end)).map(words_of).fold(HashSet::new(), |mut all, part| { all.extend(part); all })
        }).collect::<Vec<_>>()
    };
    let ours = word_starts(&structure.text);
    let mut ending = HashMap::<String, Option<usize>>::new();
    for (at, window) in ours.windows(SHINGLE).enumerate() {
        let shingle = window.iter().map(|word| word.0.as_str()).collect::<Vec<_>>().join(" ");
        ending.entry(shingle).and_modify(|place| *place = None).or_insert(Some(at + SHINGLE - 1));
    }
    let placed = |call: ScalarRange| {
        let before = (0..4).find_map(|short| coordinates.slice_utf16((call.start.saturating_sub(120) + short).min(call.start)..call.start))?;
        let theirs = word_starts(before);
        let window = theirs.get(theirs.len().checked_sub(SHINGLE)?..)?;
        let at = ending.get(&window.iter().map(|word| word.0.as_str()).collect::<Vec<_>>().join(" ")).copied().flatten()?;
        let unit = ours[at].1 + utf16_len(&ours[at].0);
        Some(ScalarRange { start: unit, end: unit })
    };
    let mut added = HashSet::new();
    let length = structure.text.len();
    for note in other.nodes.iter().filter(|node| node.kind == NodeKind::Footnote) {
        let (label, echo) = note_label_of(coordinates, note);
        let body = coordinates.slice_utf16((note.range.start + echo).min(note.range.end)..note.range.end).map(trim_js).unwrap_or("");
        let words = words_of(body);
        if words.is_empty() || notes_title(body) || !added.insert(body.to_owned()) {
            continue;
        }
        let calls = note.calls.iter().filter_map(|call| placed(*call)).collect::<Vec<_>>();
        if let Some(same) = held.iter().position(|ours| words.iter().filter(|word| ours.contains(*word)).count() * 2 > words.len()) {
            if let Some(witness) = pending.witness.as_mut() {
                witness.note_witness[usize::from(kinds.notes[same].calls.is_empty())] += 1;
            }
            if kinds.notes[same].calls.is_empty() {
                if calls.is_empty() && !note.calls.is_empty() {
                    kinds.notes[same].properties.insert("called_outside".to_owned(), String::new());
                }
                kinds.notes[same].calls = calls;
            }
            continue;
        }
        if let Some(witness) = pending.witness.as_mut() {
            witness.note_witness[2] += 1;
        }
        structure.text.push('\n');
        let start = utf16_len(&structure.text);
        structure.text.push_str(body);
        let range = ScalarRange { start, end: start + utf16_len(body) };
        let called_outside = calls.is_empty() && !note.calls.is_empty();
        kinds.notes.push({ let mut note = read_note(label, start, range.end, vec![range], calls); if called_outside { note.properties.insert("called_outside".to_owned(), String::new()); } note });
    }
    if structure.text.len() != length {
        kinds.coordinates = ScalarCoordinates::new(&structure.text);
    }
}

/// Words that open on a reporter's abbreviation: "F.Supp. at 843", "A.2d 12", "O.S.2021".
pub(super) fn opens_on_reporter(words: &str) -> bool {
    static REPORTER: OnceLock<Regex> = OnceLock::new();
    REPORTER.get_or_init(|| Regex::new(r"^\p{Lu}\p{L}*\.\S").unwrap()).is_match(words.trim_start())
}

/// A citation that crossed a block, `<a class="footnote">830</a><p>F.Supp. at 1081`: the source took its volume for a note's
/// number. It is the end of the note before.
pub(super) fn join_citation_fragments(notes: Vec<StructureNode>, coordinates: &ScalarText) -> Vec<StructureNode> {
    let mut joined = Vec::<StructureNode>::with_capacity(notes.len());
    for note in notes {
        let number = note.label.as_deref().unwrap_or("").parse::<u32>().ok();
        let before = joined.iter().rev().find_map(|held| held.label.as_deref().unwrap_or("").parse::<u32>().ok());
        let fragment = number.zip(before).is_some_and(|(number, before)| number > before + 1)
            && coordinates.slice_utf16(note.range.start..note.range.end).is_some_and(opens_on_reporter);
        match joined.last_mut().filter(|_| fragment) {
            Some(last) => {
                last.range.end = note.range.end;
                last.ranges.extend(note.ranges);
            }
            None => joined.push(note),
        }
    }
    joined
}

/// A note's label where the source gives none ("", or a symbol's slug), and how much of its opening merely repeats it:
/// Harvard's ". The court", Lawbox's "1\n. The court", "[1]  The court".
pub(super) fn note_opening(body: &str, label: String) -> (String, usize) {
    static OPENING: OnceLock<Regex> = OnceLock::new();
    let opening = OPENING.get_or_init(|| Regex::new(r"^\s*(?:[\[(]?(\d{1,3}[a-zA-Z]?|[a-z]|\*+|†|‡|§|[①-⑳])[\])]?\s*[./]?|\.)\s+").unwrap());
    // CourtListener's slug for a symbol: "-s" a star, "-s-s" two, "-s--2" a star's second use.
    let label = label.trim().to_owned();
    let label = if label.starts_with("-s") { "*".repeat(label.split("--").next().unwrap_or("").matches("-s").count()) } else { label };
    let label = if plain_label(&label).starts_with(['*', '†', '‡']) { plain_label(&label).to_owned() } else { label };
    let unnamed = label.is_empty() || label.starts_with('-') || label == "None";
    let Some(capture) = opening.captures(body) else { return (if unnamed { String::new() } else { label }, 0) };
    let printed = capture.get(1).map(|printed| printed.as_str());
    // "330 F.Supp. at 843", "43 O.S.2021 § 109.3": a note that opens on a citation, its volume no label.
    if unnamed && !capture[0].contains(['.', ')', ']']) && opens_on_reporter(&body[capture[0].len()..]) {
        return (String::new(), 0);
    }
    match printed {
        // A lone letter opens a sentence as readily as a note ("A court ..."), so it is named only if the label says so.
        Some(printed) if unnamed && !printed.chars().all(char::is_alphabetic) => (printed.to_owned(), utf16_len(&capture[0])),
        Some(_) if unnamed => (String::new(), 0),
        Some(printed) if printed != label.trim_matches(['(', ')', '[', ']', '.', '/']) => (label, 0),
        _ => (label, utf16_len(&capture[0])),
    }
}

/// Notes a word processor exported as styled paragraphs, `<p class="MsoFootnoteText">[1] We decide this case ...`: the label opens
/// the note, and a paragraph of the style with none carries the note before it on.
pub(super) fn word_notes(structure: &DocumentStructure, coordinates: &ScalarText, notes: &mut Vec<StructureNode>) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^\[?(\d{1,3}|\*+)\]?[.)]?\s+\S").unwrap());
    let held = notes.iter().flat_map(|note| note.ranges.iter().copied()).collect::<Vec<_>>();
    let mut found = Vec::<StructureNode>::new();
    for element in structure.markup_elements.iter().filter(|element| element.tag == "p" && element.attributes.get("class").is_some_and(|class| class == "MsoFootnoteText")) {
        let range = element.range;
        if held.iter().any(|known| known.start < range.end && range.start < known.end) {
            continue;
        }
        let Some(printed) = coordinates.slice_utf16(range.start..range.end).map(trim_js).filter(|printed| !printed.is_empty()) else { continue };
        match label.captures(printed) {
            Some(capture) => found.push(read_note(capture[1].to_owned(), range.start, range.end, vec![range], Vec::new())),
            None => if let Some(last) = found.last_mut() {
                last.range.end = range.end;
                last.ranges.push(range);
            },
        }
    }
    notes.append(&mut found);
    notes.sort_by_key(|note| note.range.start);
}

/// Notes that are only anchors: a number named by a link from earlier in the text (`<a name="N_1_">1.</a>` and
/// `<a href="#N_1_">`). Each runs to the next.
pub(super) fn anchored_notes(structure: &DocumentStructure, coordinates: &ScalarText, end: usize, notes: &mut Vec<StructureNode>) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^\[?(?:Footnote |FN ?)?(\d{1,3}|\*+|†|‡)\]?[.:)]?$").unwrap());
    let mut links = HashMap::<&str, Vec<ScalarRange>>::new();
    for element in structure.markup_elements.iter().filter(|element| element.tag == "a") {
        if let Some(target) = element.attributes.get("href").and_then(|href| href.strip_prefix('#')) {
            links.entry(target).or_default().push(element.range);
        }
    }
    let taken = |range: ScalarRange, notes: &[StructureNode]| notes.iter().any(|note| {
        note.ranges.iter().chain(&note.calls).any(|known| known.start < range.end.max(range.start + 1) && range.start < known.end)
    });
    let elements = &structure.markup_elements;
    let mut targets = elements.iter().filter(|element| element.tag == "a").filter_map(|element| {
        let name = element.attributes.get("name").or_else(|| element.attributes.get("id"))?;
        let calls = links.get(name.as_str())?;
        let printed = coordinates.slice_utf16(element.range.start..element.range.end).map(trim_js)?;
        let number = label.captures(printed)?[1].to_owned();
        // A note is its own block: the text after it is not its to take.
        let block = element.parent.map_or(end, |parent| elements[parent].range.end);
        (calls.iter().any(|call| call.start < element.range.start) && !taken(element.range, notes)).then(|| (element.range, number, calls.clone(), block))
    }).collect::<Vec<_>>();
    targets.sort_by_key(|target| target.0.start);
    let stops = targets.iter().map(|target| target.0.start).collect::<Vec<_>>();
    for (index, (range, number, calls, block)) in targets.into_iter().enumerate() {
        let stop = stops.get(index + 1).copied().unwrap_or(end).min(block);
        let start = range.end.min(stop);
        notes.push(read_note(number, start, stop, vec![ScalarRange { start, end: stop }], calls));
    }
    notes.sort_by_key(|note| note.range.start);
}

/// A note's call with no note anywhere in the record: the mark stands, its note was never reported.
pub(super) fn unmatched_call(label: String, range: ScalarRange) -> StructureNode {
    let mut call = span("unmatched-call", range.start, range.end);
    call.label = Some(label);
    call
}

/// The label a call names, bare.
fn called(call: &StructureNode) -> &str {
    plain_label(call.label.as_deref().unwrap_or(""))
}

/// Note calls the source marks and no note answers.
pub(super) fn unmatched_calls(structure: &DocumentStructure, coordinates: &ScalarText, notes: &[StructureNode]) -> Vec<StructureNode> {
    static SYMBOL: OnceLock<Regex> = OnceLock::new();
    let symbol = SYMBOL.get_or_init(|| Regex::new(r"^\[?(\d{1,3}|\*+|†|‡|\([a-z0-9]\))\]?$").unwrap());
    let mut answered = notes.iter().flat_map(|note| note.calls.iter().chain(&note.ranges).copied()).collect::<Vec<_>>();
    answered.sort_by_key(|known| known.end);
    structure.markup_elements.iter().filter_map(|element| {
        let linked = element.tag == "a" && element.attributes.get("class").is_some_and(|class| class == "footnote")
            && !element.attributes.get("href").is_some_and(|href| href.ends_with("_ref"));
        if !(linked || matches!(element.tag.as_str(), "footnotemark" | "sup")) {
            return None;
        }
        // Colorado's mark is an empty sup after the digits; 10<sup>8</sup> is a power.
        let before = (0..4).find_map(|short| coordinates.slice_utf16(element.range.start.saturating_sub(4 - short)..element.range.start)).unwrap_or("");
        let trailing = before.len() - before.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let empty = element.range.start == element.range.end && element.attributes.get("class").is_some_and(|class| class == "ldml-superscript");
        if trailing > 0 && !empty && element.tag == "sup" {
            return None;
        }
        let range = if empty { ScalarRange { start: element.range.start - trailing, end: element.range.end } } else { element.range };
        let printed = coordinates.slice_utf16(range.start..range.end).map(trim_js)?;
        let label = symbol.captures(printed)?[1].to_owned();
        // `<a class="footnote">830</a> F.Supp. at 1081` is a citation's volume.
        let unlinked = element.tag == "a" && !element.attributes.contains_key("href");
        if unlinked && (0..12).find_map(|short| coordinates.slice_utf16(range.end..range.end + 12 - short)).is_some_and(opens_on_reporter) {
            return None;
        }
        let known = answered[answered.partition_point(|known| known.end <= range.start)..].iter().take_while(|known| known.start < range.end || known.end <= range.end).any(|known| known.start < range.end);
        (!known).then(|| unmatched_call(label, range))
    }).collect()
}

/// Calls the source left unlinked find their notes by label, in order; and where no note answers, the numbered paragraphs
/// past the last call that run through their labels are the notes, set as text.
pub(super) fn answer_calls(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    static OPENING: OnceLock<Regex> = OnceLock::new();
    let (mut calls, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut kinds.spans).into_iter().partition(|span| reads(span, "unmatched-call"));
    kinds.spans = rest;
    calls.sort_by_key(|call| call.range.start);
    for note in kinds.notes.iter_mut().filter(|note| note.calls.is_empty()) {
        if let Some(at) = calls.iter().position(|call| called(call) == plain_label(note.label.as_deref().unwrap_or(""))) {
            let call = calls.remove(at);
            note.calls.push(call.range);
        }
    }
    calls.retain(|call| {
        let Some(note) = kinds.notes.iter_mut().find(|note| plain_label(note.label.as_deref().unwrap_or("")) == called(call)) else { return true };
        note.calls.push(call.range);
        false
    });
    let mut wanted = calls.iter().filter_map(|call| called(call).parse::<u32>().ok()).collect::<Vec<_>>();
    wanted.sort_unstable();
    wanted.dedup();
    // A note list's own markers open their paragraphs: the last call is the last in the text's run.
    let opens = |call: &&StructureNode| kinds.paragraphs.binary_search_by_key(&call.range.start, |paragraph| paragraph.range.start).is_ok();
    let mut last_call = calls.iter().filter(|call| !opens(call)).map(|call| call.range.end).max().unwrap_or(0);
    // A list under a notes title needs no call at all.
    static TITLE: OnceLock<Regex> = OnceLock::new();
    let title = TITLE.get_or_init(|| Regex::new(r"(?mi)^[ \t]*(?:(?:foot|end)?notes?|escolios?)[ \t]*:?[ \t]*$").unwrap()).find_iter(&structure.text).last();
    let titled = title.is_some() && kinds.notes.is_empty();
    if let Some(title) = title.filter(|_| kinds.notes.is_empty()) {
        last_call = utf16_len(&structure.text[..title.end()]);
        if wanted.is_empty() {
            wanted = (1..1000).collect();
        }
    }
    if !wanted.is_empty() {
        let opening = OPENING.get_or_init(|| Regex::new(r"^[\[(]{0,2}(\d{1,3})[\])\.]{0,2}(?:\s|\p{Lu})").unwrap());
        let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
        let mut found = Vec::new();
        let printed_of = |paragraph: &StructureNode| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map(trim_js);
        // A list of kinds.notes stands last, and a list in the opinion may print its numbers first: "(3) Was the conduct ...". Read from
        // the tail back, each note is the last paragraph that prints its number.
        let mut chosen = HashMap::new();
        let mut ceiling = u32::MAX;
        for paragraph in kinds.paragraphs.iter().rev().take_while(|paragraph| paragraph.range.start >= last_call) {
            let number = printed_of(paragraph).and_then(|printed| opening.captures(printed)).and_then(|capture| capture[1].parse::<u32>().ok());
            if let Some(number) = number.filter(|number| *number < ceiling && wanted.contains(number)) {
                chosen.insert(paragraph.range.start, number);
                ceiling = number;
            }
        }
        // The kinds.paragraphs under a note belong to it only where another note closes them, or a kinds.notes title heads the list: past
        // the last note of no list is the opinion, a number having opened a paragraph of it. A paragraph's own mark ends any note.
        let mut under = None;
        let mut closed = false;
        for paragraph in kinds.paragraphs.iter().filter(|paragraph| paragraph.range.start >= last_call) {
            let Some(printed) = printed_of(paragraph) else { continue };
            if let Some(number) = chosen.get(&paragraph.range.start) {
                if let (Some(last), Some(end)) = (found.last_mut(), under.take()) {
                    let last: &mut (u32, usize, usize) = last;
                    last.2 = end;
                }
                found.push((*number, paragraph.range.start, paragraph.range.end));
                closed = false;
            } else if text::says_paragraph(printed) {
                under = None;
                closed = true;
            } else if !found.is_empty() && !closed {
                under = Some(paragraph.range.end);
            }
        }
        if let (true, Some(last), Some(end)) = (titled, found.last_mut(), under) {
            last.2 = end;
        }
        if found.len() * 2 >= wanted.len().min(found.len() + 1) {
            for (number, start, end) in found {
                let label = number.to_string();
                let references = calls.iter().filter(|call| called(call) == label).map(|call| call.range).collect();
                kinds.notes.push(read_note(label, start, end, vec![ScalarRange { start, end }], references));
            }
            let answered = kinds.notes.iter().map(|note| plain_label(note.label.as_deref().unwrap_or(""))).collect::<HashSet<_>>();
            calls.retain(|call| !answered.contains(called(call)));
            kinds.notes.sort_by_key(|note| note.range.start);
        }
    }
    kinds.spans.extend(calls);
}

/// Notes set flat in the text, each under its own "Footnote 1:" or "[Note 1]", with the calls that name them ("[FN1]",
/// "[Note 1]"). A source's one note round all of them gives way to them.
pub(super) fn flat_notes(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    static CALL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"(?m)^[ \t]*(?:(?:Footnote|FN) ?(\d{1,3}|\*+)(?: ?[:.]|[ \t]+\p{Lu}|[ \t]*$)|\[(?:Note|FN) ?(\d{1,3})\])").unwrap());
    let opened = label.captures_iter(&structure.text).filter_map(|capture| {
        let whole = capture.get(0)?;
        Some((whole.start(), whole.end(), capture.get(1).or_else(|| capture.get(2))?.as_str().to_owned()))
    }).collect::<Vec<_>>();
    if opened.is_empty() {
        return;
    }
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let calls = CALL.get_or_init(|| Regex::new(r"\[(?:FN ?|fn\.? ?|Note )(\d{1,3})\]").unwrap()).captures_iter(&structure.text)
        .filter(|capture| !opened.iter().any(|open| open.0 <= capture.get(0).unwrap().start() && capture.get(0).unwrap().start() < open.1))
        .map(|capture| { let whole = capture.get(0).unwrap(); (capture[1].to_owned(), coordinates.utf16_of_byte(whole.start()), utf16_len(whole.as_str())) }).collect::<Vec<_>>();
    let starts = opened.iter().map(|open| coordinates.utf16_of_byte(open.1)).collect::<Vec<_>>();
    // A note set flat ends with its own paragraph, ahead of the next note or not: the opinion goes on below it. A plain text's
    // lines wrap, so its paragraphs end at a blank line; a paragraph's own mark ends one anywhere. Under a "Footnotes" line
    // the notes are a list of their own, as a slip opinion sets them: each runs to the next.
    static PAST: OnceLock<Regex> = OnceLock::new();
    static BROKEN: OnceLock<Regex> = OnceLock::new();
    static LIST: OnceLock<Regex> = OnceLock::new();
    let past = PAST.get_or_init(|| Regex::new(r"\n[ \t]*\n|\n[ \t]*[\[{(]?¶").unwrap());
    let broken = BROKEN.get_or_init(|| Regex::new(r"\n").unwrap());
    let listed = LIST.get_or_init(|| Regex::new(r"(?m)^[ \t]*Footnotes[ \t]*$").unwrap()).find(&structure.text).map(|head| head.end());
    let closes = if kinds.opinion.rendition == "plain_text.txt" { past } else { broken };
    let stops = opened.iter().enumerate().map(|(index, open)| {
        let limit = opened.get(index + 1).map_or(structure.text.len(), |next| next.0);
        let body = &structure.text[open.1..limit];
        if listed.is_some_and(|head| head <= open.0) {
            return coordinates.utf16_of_byte(open.1 + body.trim_end_matches(javascript_whitespace).len());
        }
        let lead = body.len() - body.trim_start().len();
        coordinates.utf16_of_byte(closes.find(&body[lead..]).map_or(limit, |close| open.1 + lead + close.start()))
    }).collect::<Vec<_>>();
    let found = opened.iter().enumerate().map(|(index, (_, _, number))| (number.clone(), starts[index], stops[index])).collect::<Vec<_>>();
    for (number, start, stop) in found {
        if kinds.notes.iter().any(|note| note.label.as_deref().unwrap_or("") == number && note.range.start <= start && start < note.range.end) {
            continue;
        }
        kinds.notes.retain(|note| !(note.range.start <= start && start < note.range.end));
        let references = calls.iter().filter(|call| call.0 == number).map(|call| ScalarRange { start: call.1, end: call.1 + call.2 }).collect();
        kinds.notes.push(read_note(number, start, stop, vec![ScalarRange { start, end: stop }], references));
    }
    kinds.notes.sort_by_key(|note| note.range.start);
}

/// A note whose body runs on through the next notes' numbers, "... 110. Id. 111. See note 86 supra.": a source's list of notes set
/// as one block. Numbers climbing by ones from the note's own, after a finished sentence, are each a note if the text calls them.
pub(super) fn split_note_lists(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    const LEAST_RUN: usize = 2;
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    let number = NUMBER.get_or_init(|| Regex::new(r"(?-u:\b)(\d{1,3})\. +\p{Lu}").unwrap());
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let called = kinds.spans_of("unmatched-call").filter_map(|call| called(call).parse::<u32>().ok()).collect::<HashSet<_>>();
    let mut added = Vec::new();
    for note in kinds.notes.iter_mut().filter(|note| note.ranges.len() == 1) {
        let Ok(mut next) = note.label.as_deref().unwrap_or("").parse::<u32>().map(|label| label + 1) else { continue };
        let range = note.ranges[0];
        let Some(body) = coordinates.slice_utf16(range.start..range.end) else { continue };
        let mut opens = Vec::new();
        for capture in number.captures_iter(body) {
            let whole = capture.get(0).unwrap();
            let closed = crate::inference::ends_sentence(body[..whole.start()].trim_end(), true);
            if closed && called.contains(&next) && capture[1].parse::<u32>().is_ok_and(|found| found == next) {
                opens.push((next, whole.start(), capture.get(1).unwrap().end() + 1));
                next += 1;
            }
        }
        if opens.len() < LEAST_RUN {
            continue;
        }
        let first = range.start + utf16_len(&body[..opens[0].1]);
        note.range.end = first;
        note.ranges[0].end = first;
        for (index, (label, _, words)) in opens.iter().enumerate() {
            let stop = opens.get(index + 1).map_or(range.end, |next| range.start + utf16_len(&body[..next.1]));
            let begin = range.start + utf16_len(&body[..*words]);
            added.push(read_note(label.to_string(), begin, stop, vec![ScalarRange { start: begin, end: stop }], Vec::new()));
        }
    }
    if !added.is_empty() {
        kinds.notes.append(&mut added);
        kinds.notes.sort_by_key(|note| note.range.start);
    }
}

/// Notes of one label more than once (each opinion's own series) share their calls in order; a note with none takes a
/// bracketed number in the text, where the source left it as words.
pub(super) fn share_note_calls(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    static CALL: OnceLock<Regex> = OnceLock::new();
    let mut by_label = HashMap::<String, Vec<usize>>::new();
    for (index, note) in kinds.notes.iter().enumerate() {
        by_label.entry(note.label.as_deref().unwrap_or("").to_owned()).or_default().push(index);
    }
    for indices in by_label.values().filter(|indices| indices.len() > 1) {
        let mut calls = indices.iter().flat_map(|&index| kinds.notes[index].calls.iter().copied()).collect::<Vec<_>>();
        calls.sort_by_key(|call| call.start);
        // One call found twice, by the engine and through another rendition's copy of its note, a unit or two apart, is one call.
        calls.dedup_by(|later, kept| later.start <= kept.end + 2);
        if calls.len() <= indices.len() {
            // In the text's order each opinion's series follows the last; a note left over, another rendition's copy set at the
            // end, has no call of its own and takes none.
            let mut order = indices.clone();
            order.sort_by_key(|&index| kinds.notes[index].range.start);
            for (position, &index) in order.iter().enumerate() {
                kinds.notes[index].calls = calls.get(position).copied().into_iter().collect();
            }
        }
    }
    if !kinds.notes.iter().any(|note| note.calls.is_empty() && note.label.as_deref().unwrap_or("").bytes().all(|byte| byte.is_ascii_digit())) {
        return;
    }
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let mut printed = HashMap::<String, Vec<ScalarRange>>::new();
    let call = CALL.get_or_init(|| Regex::new(r"\[(\d{1,3})\]").unwrap());
    for capture in call.captures_iter(&structure.text) {
        let whole = capture.get(0).unwrap();
        let unit = coordinates.utf16_of_byte(whole.start());
        let opening = kinds.paragraphs.iter().any(|paragraph| paragraph.range.start == unit);
        if !opening && !kinds.notes.iter().any(|note| note.ranges.iter().any(|range| range.start <= unit && unit < range.end)) {
            printed.entry(capture[1].to_owned()).or_default().push(ScalarRange { start: unit, end: unit + utf16_len(whole.as_str()) });
        }
    }
    let used = kinds.notes.iter().flat_map(|note| note.calls.iter().map(|call| call.start)).collect::<HashSet<_>>();
    for note in kinds.notes.iter_mut().filter(|note| note.calls.is_empty()) {
        if let Some(calls) = printed.get_mut(note.label.as_deref().unwrap_or("")) {
            calls.retain(|call| !used.contains(&call.start));
            if !calls.is_empty() {
                note.calls.push(calls.remove(0));
            }
        }
    }
}
