//! The writings an opinion holds, and who speaks each paragraph of an exchange.

use std::sync::OnceLock;
use legal_structure_model::document::DocumentStructure;
use legal_structure_model::text::ScalarText;
use regex::Regex;
use crate::trim_javascript_whitespace as trim_js;
use super::{read_part, role_of, squeezed, Kinds};

/// Every paragraph that opens on a list's label keeps it as its `item`, numbered run or not, and so does a court's own
/// named paragraph (an advisory opinion's question and answer, a certified question); a note keeps the labels its lines
/// open on as its `items`.
pub(super) fn list_labels(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    // A block's counter may stand ahead of the label: "6\n(A) verify the wages".
    let label = LABEL.get_or_init(|| Regex::new(r"^(?:\d{1,4}\s*\n\s*)?(\(?(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})\)|(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})[.)])\s+\S").unwrap());
    static WITHIN: OnceLock<Regex> = OnceLock::new();
    let within = WITHIN.get_or_init(|| Regex::new(r"(?m)^[ \t]*(\(?(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})\)|(?:\d{1,3}|[a-zA-Z]|[ivxl]{2,5}|[IVXL]{2,5})[.)])[ \t]+\S").unwrap());
    static NAMED: OnceLock<Regex> = OnceLock::new();
    let named = NAMED.get_or_init(|| Regex::new(r"^(QUESTIONS?(?: (?:NO\. )?(?:\d{1,2}|[IVX]{1,4}))?(?: PRESENTED)?|WE ANSWER|ANSWERS?(?: (?:NO\. )?(?:\d{1,2}|[IVX]{1,4}))?|CERTIFIED QUESTIONS?|Certified [Qq]uestions?|Question(?: (?:No\. )?\d{1,2})?(?: [Pp]resented)?|Answer(?: \d{1,2})?)[:.]").unwrap());
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let items = kinds.paragraphs.iter().map(|paragraph| coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map(trim_js).and_then(|opening| Some(label.captures(opening).or_else(|| named.captures(opening))?[1].to_owned()))).collect::<Vec<_>>();
    let lists = kinds.notes.iter().map(|note| note.ranges.iter().filter_map(|range| coordinates.slice_utf16(range.start..range.end)).flat_map(|body| within.captures_iter(body).map(|capture| capture[1].to_owned())).collect::<Vec<_>>()).collect::<Vec<_>>();
    for (paragraph, item) in kinds.paragraphs.iter_mut().zip(items) {
        if let Some(item) = item { paragraph.properties.insert("item".to_owned(), item); }
    }
    for (note, items) in kinds.notes.iter_mut().zip(lists) {
        note.properties.insert("items".to_owned(), items.join("\n"));
    }
}

/// Who speaks each paragraph of an exchange. "Q." and "A." count only where a record has several of each, since "A." is
/// also an outline's letter.
pub(super) fn speakers(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^(Q\.|A\.|THE COURT:|The Court:|THE WITNESS:|The Witness:|M(?:R|S|RS)\.\s+\p{Lu}[\p{Lu}'’-]+:)\s").unwrap());
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
    let found = kinds.paragraphs.iter().enumerate().filter_map(|(index, paragraph)| {
        let printed = trim_js(coordinates.slice_utf16(paragraph.range.start..paragraph.range.end)?);
        Some((index, label.captures(printed)?[1].trim_end_matches(':').to_owned()))
    }).collect::<Vec<_>>();
    let questions = found.iter().filter(|(_, speaker)| speaker == "Q.").count();
    for (index, speaker) in found {
        if !matches!(speaker.as_str(), "Q." | "A.") || questions >= 2 {
            kinds.paragraphs[index].properties.insert("speaker".to_owned(), speaker);
        }
    }
}

/// A concurrence or dissent that opens inside a flat body, on a line of its own: "SMITH, J., dissenting.".
pub(super) fn separate_parts(structure: &mut DocumentStructure, kinds: &mut Kinds) {
    /// A writing's opening line, its joiners named, runs no longer than this.
    const LINE_LONG: usize = 460;
    /// How much of a line's text the opening's tests read.
    const OPENING_READ: usize = 520;
    static OPENS: OnceLock<Regex> = OnceLock::new();
    if kinds.parts.is_empty() {
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
    let coordinates = ScalarText::with_coordinates(&structure.text, &kinds.coordinates);
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
    let mut breaks = kinds.paragraphs.iter().flat_map(|paragraph| {
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
    }).chain(kinds.headings.iter().filter_map(|heading| {
        // A writing's opening set as a heading.
        let printed = trim_js(coordinates.slice_utf16(heading.range.start..heading.range.end)?);
        opening(&printed[lead.find(printed).map_or(0, |found| found.end())..], false).map(|(kind, author)| (heading.range.start, kind, author))
    })).collect::<Vec<_>>();
    // A second author's line, past the opinion's first text, opens another opinion.
    for line in kinds.spans_of("front").filter(|line| role_of(line) == "author") {
        let (range, words) = (line.range, line.display_value.as_deref().unwrap_or(""));
        let written = kinds.paragraphs.iter().filter(|paragraph| paragraph.range.end <= range.start).map(|paragraph| paragraph.range.end - paragraph.range.start).sum::<usize>();
        if written >= 400 && !breaks.iter().any(|known| known.0.abs_diff(range.start) < 4) {
            let says = { let lower = words.to_lowercase(); lower.contains("dissent") || lower.contains("concur") };
            breaks.push((range.start, if says { kind_of(words) } else { "opinion".to_owned() }, words.to_owned()));
        }
    }
    // A later document stacked in the record, under a title of its own.
    static STACKED: OnceLock<Regex> = OnceLock::new();
    let stacked = STACKED.get_or_init(|| Regex::new(r"(?i)^(appendix\b.{0,40}|(?:opinion |order )?on (?:petition for )?rehearing.{0,30}|supplemental opinion)$").unwrap());
    // An order or judgment under a title of its own, once a good deal has been written: a separate ruling. At the top
    // of a record the same word is its title.
    static RULING: OnceLock<Regex> = OnceLock::new();
    let ruling = RULING.get_or_init(|| Regex::new(r"(?i)^(?:(?:amended |final |supplemental )?order(?: of (?:the )?court)?|(?:final )?judgment|decree|attachment\b.{0,40})\.?$").unwrap());
    for heading in kinds.headings.iter() {
        let printed = coordinates.slice_utf16(heading.range.start..heading.range.end).map(trim_js).unwrap_or("");
        if heading.range.start > 0 && stacked.is_match(printed) {
            let kind = if printed.to_lowercase().starts_with("appendix") { "appendix" } else { "rehearing" };
            breaks.push((heading.range.start, kind.to_owned(), squeezed(printed)));
        } else if printed.starts_with(char::is_uppercase) && ruling.is_match(printed) {
            let written = kinds.paragraphs.iter().filter(|paragraph| paragraph.range.end <= heading.range.start).map(|paragraph| paragraph.range.end - paragraph.range.start).sum::<usize>();
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
        kinds.paragraphs.iter().any(|paragraph| (known.0 < paragraph.range.start && paragraph.range.start < next) || (paragraph.range.start <= known.0 && paragraph.range.end > known.0 + LINE_ALONE))
    });
    // What a source already sets apart stands; a break cuts the part it falls in.
    const SAME_START: usize = 40;
    for (start, kind, author) in breaks {
        if kinds.parts.iter().any(|part| part.range.start.abs_diff(start) < SAME_START) {
            continue;
        }
        let Some(at) = kinds.parts.iter().position(|part| part.range.start < start && start < part.range.end) else { continue };
        let end = std::mem::replace(&mut kinds.parts[at].range.end, start);
        kinds.parts.insert(at + 1, read_part(kind, Some(author), start, end));
    }
    // "Justice BAER files a concurring opinion." says what the part under that author's name is.
    static FILES: OnceLock<Regex> = OnceLock::new();
    let files = FILES.get_or_init(|| Regex::new(r"(?m)^\s*(?:(?:Mr\.|Mrs\.|Ms\.|Chief|Justice|Judge)\s+)*([\p{Lu}][\p{L}'’\-]+)[^.\n]{0,40}?\b(?:files?|filed)\s+an?\s+((?:concurring|dissenting)(?:\s+and\s+(?:concurring|dissenting))?)\s+opinion").unwrap());
    let stated = files.captures_iter(&structure.text).map(|capture| (capture[1].to_lowercase(), kind_of(&capture[2]))).collect::<Vec<_>>();
    for part in kinds.parts.iter_mut().filter(|part| part.display_value.as_deref().unwrap_or("") == "opinion") {
        let author = part.properties.get("author").map(String::as_str).unwrap_or("").to_lowercase();
        if let Some((_, kind)) = stated.iter().find(|(name, _)| author.contains(name.as_str())) {
            part.display_value = Some(kind.clone());
        }
    }
}
