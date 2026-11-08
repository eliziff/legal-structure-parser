//! What a finished structure shows is wrong or missing, class by class, for a run over a corpus to report.

use super::*;

/// Each failure the structure shows: its class, and the detail to read it by.
pub fn parse_failures(structure: &DocumentStructure) -> Vec<(&'static str, String)> {
    let pages: Vec<&StructureNode> = nodes(structure, NodeKind::Page).collect();
    let paragraphs: Vec<&StructureNode> = super::paragraphs(structure).collect();
    let notes: Vec<&StructureNode> = nodes(structure, NodeKind::Footnote).collect();
    let parts: Vec<&StructureNode> = nodes(structure, NodeKind::Section).collect();
    let mut failures = Vec::new();
    let opinion_pages = || pages.iter().filter(|page| in_flow(page));
    if opinion_pages().next().is_none() {
        failures.push(("no pages", String::new()));
    }
    if paragraphs.is_empty() {
        failures.push(("no paragraphs", format!("{} chars", structure.text.len())));
    }
    if super::reading(structure).outcomes.is_empty() && paragraphs.len() > 3 && super::reading(structure).document_kind == "court-opinion" {
        failures.push(("no outcome", String::new()));
    }
    let mut series = HashMap::<&str, Vec<&str>>::new();
    for page in opinion_pages() {
        series.entry(&page.series).or_default().push(page.label.as_deref().unwrap_or(""));
    }
    for (name, labels) in &series {
        let printed = name.starts_with("reporter") || name.starts_with("html");
        if printed && !name.contains('/') && !super::reading(structure).page_series.iter().any(|known| known.series == *name) {
            failures.push(("numbering without a citation", format!("{name} {:?}", &labels[..labels.len().min(3)])));
        }
        let numbers = labels.iter().filter_map(|label| page_number(label)).collect::<Vec<_>>();
        if numbers.windows(2).any(|pair| pair[1] < pair[0]) {
            failures.push(("pages out of order", format!("{name} {:?}", &labels[..labels.len().min(12)])));
        }
        if numbers.windows(2).any(|pair| pair[1] > pair[0] + 3) {
            failures.push(("pages jump", format!("{name} {:?}", &labels[..labels.len().min(12)])));
        }
    }
    let uncalled = notes.iter().filter(|note| note.calls.is_empty() && !note.properties.contains_key("called_outside")).count();
    if uncalled > 0 {
        failures.push(("notes without a call", format!("{uncalled} of {}", notes.len())));
    }
    if !super::reading(structure).unmatched_calls.is_empty() {
        failures.push(("calls without a note", super::reading(structure).unmatched_calls.iter().take(8).map(|call| call.label.as_str()).collect::<Vec<_>>().join(" ")));
    }
    // An opinion names its own notes: "footnote 7", "note 7, supra". One the structure does not hold was missed.
    static NAMED: OnceLock<Regex> = OnceLock::new();
    let named = NAMED.get_or_init(|| Regex::new(r"(?i)\b(?:footnote|fn\.|n\.|note) (\d{1,3}),? (?:supra|infra|above|below)\b|\bsee (?:also )?(?:footnote|note) (\d{1,3})[,.;)]? *(?:$|\p{Lu})").unwrap());
    let held = notes.iter().filter_map(|note| plain_label(note.label.as_deref().unwrap_or("")).parse::<u32>().ok()).collect::<HashSet<_>>();
    let unheld = named.captures_iter(&structure.text).filter_map(|capture| capture.get(1).or_else(|| capture.get(2))?.as_str().parse::<u32>().ok()).filter(|number| !held.contains(number)).collect::<HashSet<_>>();
    if !unheld.is_empty() {
        let mut numbers = unheld.into_iter().collect::<Vec<_>>();
        numbers.sort_unstable();
        failures.push(("note named, not held", format!("{:?}", &numbers[..numbers.len().min(8)])));
    }
    let mut labels = notes.iter().filter_map(|note| note.label.as_deref().unwrap_or("").parse::<u32>().ok()).collect::<Vec<_>>();
    labels.sort_unstable();
    // A separate opinion carries on its case's numbering.
    let separate = parts.first().is_some_and(|part| part.display_value.as_deref().unwrap_or("").contains("concur") || part.display_value.as_deref().unwrap_or("").contains("dissent") || part.display_value.as_deref().unwrap_or("").contains("rehearing"));
    if !separate && labels.first().is_some_and(|first| *first > 1) {
        failures.push(("notes not from 1", format!("{:?}", &labels[..labels.len().min(8)])));
    }
    if labels.windows(2).any(|pair| pair[1] > pair[0] + 1) {
        failures.push(("note numbers skip", format!("{:?}", &labels[..labels.len().min(16)])));
    }
    let numbers = paragraphs.iter().filter_map(|paragraph| paragraph.label.as_deref().as_deref()?.parse::<u32>().ok()).collect::<Vec<_>>();
    // An opinion names its own paragraphs too: "¶ 12, supra", "paragraph 12 above".
    static OWN_PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    let own = OWN_PARAGRAPH.get_or_init(|| Regex::new(r"(?i)(?:¶¶?|\bparagraph) ?(\d{1,4}),? (?:supra|infra|above|below)\b").unwrap());
    let unheld = own.captures_iter(&structure.text).filter_map(|capture| capture[1].parse::<u32>().ok()).filter(|number| !numbers.contains(number)).collect::<HashSet<_>>();
    if !unheld.is_empty() {
        let mut named = unheld.into_iter().collect::<Vec<_>>();
        named.sort_unstable();
        failures.push(("paragraph named, not held", format!("{:?}", &named[..named.len().min(8)])));
    }
    if numbers.len() >= 3 {
        if !separate && numbers[0] > 1 {
            failures.push(("paragraph numbers not from 1", format!("{:?}", &numbers[..numbers.len().min(8)])));
        }
        if numbers.windows(2).any(|pair| pair[1] != pair[0] + 1 && pair[1] != 1) {
            failures.push(("paragraph numbers broken", format!("{:?}", &numbers[..numbers.len().min(16)])));
        }
    }
    failures
}

/// A screen flags a record to read, not an error: each asks a question only reading settles, and a class is worth reading where
/// it fills up. All cheap enough for every record of a run.
pub fn parse_screens(structure: &DocumentStructure, record: &OpinionRecord) -> Vec<(&'static str, String)> {
    let pages: Vec<&StructureNode> = nodes(structure, NodeKind::Page).collect();
    let paragraphs: Vec<&StructureNode> = super::paragraphs(structure).collect();
    let notes: Vec<&StructureNode> = nodes(structure, NodeKind::Footnote).collect();
    let headings: Vec<&StructureNode> = nodes(structure, NodeKind::Heading).collect();
    const SHORT_TITLE: usize = 60;
    const LONG_PARAGRAPH: usize = 12_000;
    const LONG_NOTE: usize = 8_000;
    const LONG_TITLE: usize = 200;
    let mut found = Vec::new();
    let coordinates = ScalarText::new(&structure.text);
    let quoted = |start: usize, end: usize| coordinates.slice_utf16(start..end).map(|text| squeezed(text).chars().take(120).collect::<String>()).unwrap_or_default();
    let printed = |start: usize, end: usize| coordinates.slice_utf16(start..end).map(trim_js).unwrap_or("");
    let size = utf16_len(&structure.text);

    // Text no structure holds.
    let mut held = paragraphs.iter().map(|paragraph| (paragraph.range.start, paragraph.range.end))
        .chain(headings.iter().map(|heading| (heading.range.start, heading.range.end)))
        .chain(notes.iter().flat_map(|note| note.ranges.iter().map(|range| (range.start, range.end))))
        .chain(super::reading(structure).front.iter().filter_map(|line| line.range).map(|range| (range.start, range.end)))
        .chain(super::reading(structure).headnotes.iter().filter_map(|headnote| headnote.range).map(|range| (range.start, range.end)))
        .chain(nodes(structure, NodeKind::Table).map(|table| (table.range.start, table.range.end)))
        .collect::<Vec<_>>();
    held.sort_unstable();
    let (mut reached, mut loose, mut widest) = (0, 0, (0, 0));
    for (start, end) in held.into_iter().chain([(size, size)]) {
        if start > reached {
            let gap = printed(reached, start).len();
            loose += gap;
            if gap > widest.1 - widest.0 { widest = (reached, reached + gap); }
        }
        reached = reached.max(end);
    }
    if loose > 500 && loose * 7 > size {
        found.push(("screen: text in no structure", format!("{loose} of {size}: {}", quoted(widest.0, widest.1))));
    }

    // Text the reading lost or left dirty.
    static TAGS: OnceLock<Regex> = OnceLock::new();
    let tags = TAGS.get_or_init(|| Regex::new(r"<[^>]*>").unwrap());
    if let Some((_, source)) = record.renditions.iter().find(|(name, _)| *name == super::reading(structure).rendition) {
        let words = |text: &str| text.split_whitespace().count();
        let (ours, theirs) = (words(&structure.text), words(&tags.replace_all(source, " ")));
        if theirs > 300 && ours * 10 < theirs * 7 {
            found.push(("screen: text lost", format!("{ours} words of {theirs}")));
        }
    }
    static DIRT: OnceLock<Regex> = OnceLock::new();
    let dirt = DIRT.get_or_init(|| Regex::new(r"&(?:amp|nbsp|lt|gt|quot|#\d{2,5});|</?[a-z]{1,8}(?: [^>]{0,40})?>|â€|Â |Ã[\x{80}-\x{bf}]|[\x{1}-\x{8}\x{e}-\x{1f}]").unwrap());
    let marks = dirt.find_iter(&structure.text).take(4).map(|found| found.as_str().to_owned()).collect::<Vec<_>>();
    if marks.len() >= 4 {
        found.push(("screen: markup or bad bytes in text", marks.join(" ")));
    }

    // Pages.
    static LEFT: OnceLock<Regex> = OnceLock::new();
    let left = LEFT.get_or_init(|| Regex::new(r"[a-z,;] \*\d{2,5} [a-zA-Z]|\[\*+ ?\d{1,5}\]|\[Page \d{1,5}\]|(?m)^\s*Page \d{1,4} of \d{1,4}\s*$").unwrap());
    let left_marks = left.find_iter(&structure.text).take(3).map(|found| found.as_str().to_owned()).collect::<Vec<_>>();
    if left_marks.len() >= 3 {
        found.push(("screen: page marks left in text", left_marks.join(" | ")));
    }
    let longest_run = pages.iter().filter(|page| !page.properties.contains_key("in_note")).fold(HashMap::<&str, usize>::new(), |mut runs, page| { *runs.entry(&page.series).or_default() += 1; runs }).into_values().max().unwrap_or(0);
    if let Some(count) = record.page_count.filter(|count| *count >= 5 && longest_run * 2 < *count as usize) {
        found.push(("screen: far fewer pages than the record counts", format!("{longest_run} of {count}")));
    }
    let printed_report = record.citations.iter().any(|citation| matches!(citation.kind.as_str(), "1" | "2" | "3" | "4" | "5"));
    if printed_report && size > 12_000 && !pages.iter().any(|page| !page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("citation")) {
        found.push(("screen: reported opinion, no page marks", format!("{size} long")));
    }

    // Paragraphs.
    if size > 20_000 && paragraphs.len() <= 3 {
        found.push(("screen: long text, few paragraphs", format!("{} in {size}", paragraphs.len())));
    }
    if let Some(paragraph) = paragraphs.iter().find(|paragraph| !(paragraph.kind == NodeKind::Quote) && paragraph.range.end - paragraph.range.start > LONG_PARAGRAPH) {
        found.push(("screen: very long paragraph", format!("{}: {}", paragraph.range.end - paragraph.range.start, quoted(paragraph.range.start, paragraph.range.end))));
    }
    let body = paragraphs.iter().filter(|paragraph| !(paragraph.kind == NodeKind::Quote)).collect::<Vec<_>>();
    let low = body.iter().filter(|paragraph| !paragraph.properties.contains_key("continues") && printed(paragraph.range.start, paragraph.range.end).starts_with(char::is_lowercase)).collect::<Vec<_>>();
    if low.len() >= 8 && low.len() * 6 >= body.len() {
        found.push(("screen: paragraphs open lower case", format!("{} of {}: {}", low.len(), body.len(), quoted(low[0].range.start, low[0].range.end))));
    }
    let open = body.windows(2).filter(|pair| !pair[1].properties.contains_key("continues") && !text::ended_sentence(printed(pair[0].range.start, pair[0].range.end)) && printed(pair[1].range.start, pair[1].range.end).starts_with(char::is_lowercase)).collect::<Vec<_>>();
    if open.len() >= 8 && open.len() * 6 >= body.len() {
        found.push(("screen: sentences split across paragraphs", format!("{} of {}: {}", open.len(), body.len(), quoted(open[0][1].range.start, open[0][1].range.end))));
    }
    static MARKED: OnceLock<Regex> = OnceLock::new();
    let marked = MARKED.get_or_init(|| Regex::new(r"(?m)^[ \t]*(?:¶ ?\d{1,4}|\[¶ ?\d{1,4}\.?\]|\{¶ ?\d{1,4}\}) +\p{Lu}").unwrap());
    let opens = paragraphs.iter().map(|paragraph| paragraph.range.start).collect::<HashSet<_>>();
    let buried = marked.find_iter(&structure.text).filter(|mark| !opens.contains(&utf16_len(&structure.text[..mark.start()])) && !opens.contains(&utf16_len(&structure.text[..mark.start() + (mark.len() - mark.as_str().trim_start().len())]))).take(3).map(|mark| mark.as_str().trim().to_owned()).collect::<Vec<_>>();
    if buried.len() >= 3 {
        found.push(("screen: paragraph marks no paragraph opens on", buried.join(" | ")));
    }

    // Notes.
    if let Some(note) = notes.iter().find(|note| note.ranges.iter().map(|range| range.end - range.start).sum::<usize>() > LONG_NOTE) {
        found.push(("screen: very long note", format!("note {}: {}", note.label.as_deref().unwrap_or(""), quoted(note.range.start, note.range.end))));
    }
    let in_notes = notes.iter().flat_map(|note| note.ranges.iter()).map(|range| range.end - range.start).sum::<usize>();
    if size > 5_000 && in_notes * 2 > size {
        found.push(("screen: notes hold most of the text", format!("{in_notes} of {size}")));
    }
    if notes.is_empty() {
        static CALL: OnceLock<Regex> = OnceLock::new();
        let call = CALL.get_or_init(|| Regex::new(r#"[a-z.,;)”"](\d{1,2})\s+\p{Lu}"#).unwrap());
        let calls = call.captures_iter(&structure.text).filter_map(|capture| capture[1].parse::<u32>().ok()).collect::<HashSet<_>>();
        if (1..=4).all(|number| calls.contains(&number)) {
            found.push(("screen: calls in the text, no notes", format!("{} numbers", calls.len())));
        }
    }
    static NEXT: OnceLock<Regex> = OnceLock::new();
    // A closed "2." is more often a list the note quotes.
    let next = NEXT.get_or_init(|| Regex::new(r"(?m)^[ \t]*(\d{1,3}) +\p{Lu}").unwrap());
    let swallowed = notes.iter().filter(|note| note.ranges.len() == 1).filter_map(|note| {
        let number = note.label.as_deref().unwrap_or("").parse::<u32>().ok()?;
        let body = coordinates.slice_utf16(note.range.start..note.range.end)?;
        next.captures_iter(body).skip_while(|capture| capture.get(0).is_some_and(|whole| whole.start() == 0)).any(|capture| capture[1].parse::<u32>().is_ok_and(|inner| inner == number + 1)).then_some(number)
    }).take(3).collect::<Vec<_>>();
    if !swallowed.is_empty() {
        found.push(("screen: note runs into the next note", format!("{swallowed:?}")));
    }

    // Calls that climb past the last note held: the notes after it are missing.
    if let Some(last) = notes.iter().filter_map(|note| note.label.as_deref().unwrap_or("").parse::<u32>().ok()).max() {
        static TIGHT: OnceLock<Regex> = OnceLock::new();
        let tight = TIGHT.get_or_init(|| Regex::new(r#"[a-z)”"][.,;]?["”]?(\d{1,3})\s+\p{Lu}"#).unwrap());
        let outside = |at: usize| !notes.iter().flat_map(|note| note.ranges.iter()).any(|range| range.start <= at && at < range.end);
        let mut above = tight.captures_iter(&structure.text).filter_map(|capture| Some((capture.get(1)?.start(), capture[1].parse::<u32>().ok()?))).filter(|(_, number)| *number > last && *number <= last + 12)
            .filter(|(at, _)| outside(utf16_len(&structure.text[..*at]))).map(|(_, number)| number).collect::<Vec<_>>();
        above.sort_unstable();
        above.dedup();
        if above.len() >= 3 && above[..3] == [last + 1, last + 2, last + 3] {
            found.push(("screen: calls climb past the last note", format!("last {last}, calls {:?}", &above[..above.len().min(8)])));
        }
    }

    // Paragraphs each a line of the page.
    let lengths = body.iter().map(|paragraph| paragraph.range.end - paragraph.range.start).collect::<Vec<_>>();
    if lengths.len() >= 30 {
        let mut sorted = lengths.clone();
        sorted.sort_unstable();
        if sorted[sorted.len() / 2] < 110 {
            found.push(("screen: paragraphs the length of a line", format!("median {} of {}", sorted[sorted.len() / 2], sorted.len())));
        }
    }
    // Paragraphs with no pin where the text has pages.
    let paged = pages.iter().filter(|page| in_flow(page)).count();
    let table = super::pages::pin_table(pages.iter().copied(), &super::reading(structure).page_series);
    let unpinned = paragraphs.iter().filter(|paragraph| super::pages::pins_of(&table, paragraph.range.start, paragraph.range.end).is_empty()).count();
    if paged >= 2 && unpinned * 5 > paragraphs.len() {
        found.push(("screen: paragraphs without a pin", format!("{unpinned} of {}", paragraphs.len())));
    }

    // Headings.
    static ROMAN: OnceLock<Regex> = OnceLock::new();
    let roman = ROMAN.get_or_init(|| Regex::new(r"(?m)^[ \t]*(I|II|III|IV|V|VI)\.?(?:[ \t]+\p{Lu}[\p{Lu}\p{Ll} ,'’-]{3,60})?[ \t]*$").unwrap());
    let titles = headings.iter().map(|heading| (heading.range.start, heading.range.end)).collect::<Vec<_>>();
    let bare_sections = roman.find_iter(&structure.text).filter(|line| { let at = utf16_len(&structure.text[..line.start() + (line.len() - line.as_str().trim_start().len())]); !titles.iter().any(|(start, end)| *start <= at + 1 && at < *end) }).take(3).map(|line| line.as_str().trim().to_owned()).collect::<Vec<_>>();
    if bare_sections.len() >= 2 {
        found.push(("screen: sections not held as headings", bare_sections.join(" | ")));
    }
    if let Some(heading) = headings.iter().find(|heading| heading.display_value.as_deref().unwrap_or("").chars().count() > LONG_TITLE || (heading.display_value.as_deref().unwrap_or("").split_whitespace().count() >= 15 && text::ended_sentence(&heading.display_value.as_deref().unwrap_or("")) && heading.grammar.as_deref() != Some("other-rendition"))) {
        found.push(("screen: heading reads as a sentence", heading.display_value.as_deref().unwrap_or("").chars().take(120).collect()));
    }
    // "a. Background" is an outline's letter.
    let lettered = |title: &str| { let mut letters = title.chars(); letters.next().is_some() && matches!(letters.next(), Some('.' | ')')) };
    if let Some(heading) = headings.iter().find(|heading| heading.display_value.as_deref().unwrap_or("").starts_with(char::is_lowercase) && !lettered(&heading.display_value.as_deref().unwrap_or(""))) {
        found.push(("screen: heading opens lower case", heading.display_value.as_deref().unwrap_or("").chars().take(120).collect()));
    }
    if headings.len() >= 12 && headings.len() * 2 > paragraphs.len() {
        found.push(("screen: nearly as many headings as paragraphs", format!("{} and {}", headings.len(), paragraphs.len())));
    }
    static LOOSE_TITLE: OnceLock<Regex> = OnceLock::new();
    let loose_title = LOOSE_TITLE.get_or_init(|| Regex::new(r"^(?:[IVX]{1,6}\.?|[A-Z]\.|[A-Z][A-Z ,'’&-]{5,})$").unwrap());
    let untitled = paragraphs.iter().filter(|paragraph| paragraph.label.is_none() && paragraph.range.end - paragraph.range.start < SHORT_TITLE && loose_title.is_match(printed(paragraph.range.start, paragraph.range.end))).collect::<Vec<_>>();
    if untitled.len() >= 2 {
        found.push(("screen: title left as a paragraph", untitled.iter().take(3).map(|paragraph| printed(paragraph.range.start, paragraph.range.end).to_owned()).collect::<Vec<_>>().join(" | ")));
    }

    // Headnotes.
    if super::reading(structure).headnotes.is_empty() {
        if let Some((name, _)) = record.cluster.iter().find(|(name, value)| matches!(name.as_str(), "syllabus" | "headnotes" | "summary") && value.trim().len() > 40) {
            found.push(("screen: the cluster has a headnote, the structure none", name.clone()));
        }
        static TITLE: OnceLock<Regex> = OnceLock::new();
        let title = TITLE.get_or_init(|| Regex::new(r"(?m)^[ \t]*(?:SYLLABUS|Syllabus|HEADNOTES?|Headnotes?|SYNOPSIS|Synopsis)[ \t.:]*$").unwrap());
        if title.is_match(&structure.text) {
            found.push(("screen: a headnote title, the structure none", String::new()));
        }
    }
    found
}

/// What a reader's finding says the record holds, and whether the structure has it: `None` where it does or where the family
/// is none of the targets, else the target it lacks. The quote is the reader's, verified in the record's text.
pub fn receipt_miss(structure: &DocumentStructure, family: &str, quote: &str) -> Option<&'static str> {
    let target = match family {
        "cap-page-number-el" | "star-page-span" | "page-label-anchor" | "westlaw-star-page" | "page-marker-paragraph" | "ny-slip-op-star-pages" | "ny-slip-star-page-text"
        | "ny-brace-page-locator" | "pt-page-number" | "cap-pgmap" | "lexis-pagescheme" => "page",
        "fn-body" | "cap-footnote-elements" | "harvard-footnote" | "lawbox-bracket-footnotes" | "westlaw-co-footnotes" | "pt-footnote-text" | "inline-note" | "appended-note" => "note",
        "heading-as-plain-paragraph" | "heading-elements" | "pt-outline-heading" => "heading",
        "pt-pilcrow-number" | "numbered-paragraph-div" => "paragraph number",
        "headnote-number-markers" | "syllabus-headnote-division" | "held-lead-in" | "pt-synopsis" | "mass-library-sections" | "lexis-summary-counsel" | "reporter-digest-entry" | "editorial-digest" => "headnote",
        "disposition-lines" => "disposition",
        _ => return None,
    };
    quote_miss(structure, target, quote)
}

/// Whether the structure holds what a quote shows of a target: `page`, `note`, `heading`, `paragraph number`, `headnote`, `disposition`.
pub fn quote_miss(structure: &DocumentStructure, target: &'static str, quote: &str) -> Option<&'static str> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    let bare = TAG.get_or_init(|| Regex::new(r"<[^>]*>").unwrap()).replace_all(quote, " ");
    static ENTITY: OnceLock<Regex> = OnceLock::new();
    let bare = ENTITY.get_or_init(|| Regex::new(r"&#?\w+;").unwrap()).replace_all(&bare, " ");
    let key = |text: &str| word_starts(text).into_iter().map(|word| word.0).collect::<Vec<_>>().join(" ");
    let wanted = key(&bare);
    if wanted.is_empty() {
        return None;
    }
    // A note's quote may open on its label, or be the list's title.
    let wanted = match target {
        "note" if notes_title(wanted.as_str()) => return None,
        "note" => wanted.split_once(' ').filter(|(first, _)| nodes(structure, NodeKind::Footnote).any(|note| plain_label(note.label.as_deref().unwrap_or("")) == *first)).map_or(wanted.clone(), |(_, rest)| rest.to_owned()),
        _ => wanted,
    };
    let coordinates = ScalarText::new(&structure.text);
    let printed = |start: usize, end: usize| coordinates.slice_utf16(start..end).map(key).unwrap_or_default();
    let found = match target {
        // A page mark's number, in any numbering.
        "page" => {
            let labels = nodes(structure, NodeKind::Page).filter_map(|page| page_number(page.label.as_deref().unwrap_or("")).map(|number| number.to_string())).collect::<HashSet<_>>();
            let mut numbers = wanted.split(' ').filter(|word| word.bytes().all(|byte| byte.is_ascii_digit())).peekable();
            numbers.peek().is_none() || numbers.any(|number| labels.contains(number))
        }
        "note" => nodes(structure, NodeKind::Footnote).any(|note| {
            let body = note.ranges.iter().map(|range| printed(range.start, range.end)).collect::<Vec<_>>().join(" ");
            body.contains(&wanted) || wanted.contains(&body) && !body.is_empty()
        }) || reading(structure).headnotes.iter().any(|headnote| key(&headnote.text).contains(&wanted))
            || nodes(structure, NodeKind::Footnote).any(|note| plain_label(note.label.as_deref().unwrap_or("")) == wanted.trim_start_matches("footnote ")),
        "heading" => nodes(structure, NodeKind::Heading).any(|heading| {
            let title = key(&heading.display_value.as_deref().unwrap_or(""));
            !title.is_empty() && (title.contains(&wanted) || wanted.contains(&title))
        }) || reading(structure).front.iter().any(|line| line.role == "author" && key(&line.text) == wanted),
        "paragraph number" => wanted.split(' ').find(|word| word.bytes().all(|byte| byte.is_ascii_digit())).is_none_or(|number| paragraphs(structure).any(|paragraph| paragraph.label.as_deref().as_deref().is_some_and(|printed| printed.trim_matches(['(', ')']) == number))),
        "headnote" => reading(structure).headnotes.iter().any(|headnote| {
            let text = key(&headnote.text);
            text.contains(&wanted) || wanted.contains(&text) && !text.is_empty()
        }) || !reading(structure).headnote_marks.is_empty(),
        _ => !reading(structure).outcomes.is_empty() || reading(structure).front.iter().any(|line| line.role == "disposition" && (key(&line.text).contains(&wanted) || wanted.contains(&key(&line.text)))),
    };
    (!found).then_some(target)
}
