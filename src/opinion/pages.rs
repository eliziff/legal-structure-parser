//! The pages of a record: every form a mark takes, each numbering tied to the printing it belongs to, and where a passage falls in each.

use super::*;

/// The page mark printed at a page's start, in whatever words it carries its label: "*645", "*Page 169", "*West Page 12",
/// "*Supp. 12", "*[12]", "*B5", "**709"; or "Page 382" alone in its block.
pub(super) fn printed_mark(coordinates: &ScalarText, at: usize, end: usize, label: &str) -> Option<ScalarRange> {
    static ALONE: OnceLock<Regex> = OnceLock::new();
    let head = (0..4).find_map(|short| coordinates.slice_utf16(at..(at + 48 - short).min(end)))?;
    let body = head.trim_start_matches(javascript_whitespace);
    let start = at + utf16_len(&head[..head.len() - body.len()]);
    // "[*5]" as "*5", its bracket closing with its label.
    let framed = body.starts_with("[*");
    let inner = if framed { &body[1..] } else { body };
    let stars = inner.len() - inner.trim_start_matches('*').len();
    if stars == 0 {
        let found = ALONE.get_or_init(|| Regex::new(r"^Page\s+\d{1,6}\s*$").unwrap()).find(body)?;
        return Some(ScalarRange { start, end: start + utf16_len(found.as_str().trim_end()) });
    }
    let rest = &inner[stars..];
    let place = if label.is_empty() { 0 } else { rest.find(label)? };
    if place > 24 || !rest[..place].chars().all(|c| c.is_alphabetic() || matches!(c, ' ' | '\t' | '\n' | '\r' | '.' | '[' | '(')) {
        return None;
    }
    let past = place + label.len();
    let closed = past + rest[past..].len() - rest[past..].trim_start_matches([']', ')', '.']).len();
    Some(ScalarRange { start, end: start + usize::from(framed) + utf16_len(&inner[..stars + closed]) })
}

/// A printed run of its own within a reporter: "Supp. 12", "CJP Supp. 12", "B5", "R5" do not count with its pages.
pub(super) fn label_run(label: &str) -> Option<&str> {
    let run = label.split(|c: char| c.is_ascii_digit()).next()?.trim();
    (!run.is_empty() && run.len() < label.len() && run.chars().any(char::is_alphabetic)).then_some(run)
}

/// A page label's number: its digits past any prefix, or a Roman numeral reporters print for a thousand and more.
pub(super) fn page_number(label: &str) -> Option<u32> {
    let digits = label.trim_start_matches(|c: char| !c.is_ascii_digit());
    if let Some(number) = digits.split(|c: char| !c.is_ascii_digit()).next().filter(|part| !part.is_empty()).and_then(|part| part.parse().ok()) {
        return Some(number);
    }
    if label.is_empty() || !label.chars().all(|c| "IVXLCDM".contains(c)) {
        return None;
    }
    let value = |c: char| match c { 'I' => 1, 'V' => 5, 'X' => 10, 'L' => 50, 'C' => 100, 'D' => 500, _ => 1000 };
    let letters = label.chars().map(value).collect::<Vec<u32>>();
    Some(letters.iter().enumerate().fold(0i64, |total, (at, &letter)| {
        if letters.get(at + 1).is_some_and(|next| *next > letter) { total - letter as i64 } else { total + letter as i64 }
    }) as u32)
}

pub(super) fn series_of(node: &StructureNode) -> String {
    if let Some(index) = ["citation-index", "data-citation-index"].iter().find_map(|name| node.markup_attributes.get(*name)) {
        return format!("reporter-{index}");
    }
    node.markup_attributes.get("pagescheme").cloned().unwrap_or_else(|| "reporter".to_owned())
}

pub(super) fn page_source(node: &StructureNode) -> &'static str {
    match node.grammar.as_deref() {
        Some("courtlistener-opening-page") => "opening",
        Some("courtlistener-printed-star-page") => "printed",
        _ => "markup",
    }
}

/// Pages no mark prints. Harvard's `pgmap` gives the volume's leaves a block stands on, "316", or for a block that turns a
/// page each leaf and how many words of the block fall on it, "316(74) 317(17)": `leaves_to_pages` makes them pages. Colorado's page breaks are a `div.pagebreak` and
/// its number, or an empty `span.ldml-pagenumber`, which says only that a page turns.
pub(super) fn element_pages(structure: &DocumentStructure, coordinates: &ScalarText, notes: &[ScalarRange], pages: &mut Vec<StructureNode>) {
    let mut known = pages.iter().filter(|page| page.series.starts_with("reporter")).map(|page| page.printed_label().to_owned()).collect::<HashSet<_>>();
    let page_at = |label: String, series: &str, start: usize, marker: Option<ScalarRange>, source: &'static str| {
        read_page(label, series.to_owned(), start, 0, marker, source)
    };
    let (mut broken, mut blank) = (Vec::new(), Vec::new());
    let mut marked = pages.iter().map(|page| page.range.start).collect::<Vec<_>>();
    marked.sort_unstable();
    for element in &structure.markup_elements {
        let class = element.attributes.get("class").map_or("", String::as_str);
        if let Some(map) = element.attributes.get("pgmap") {
            if notes.iter().any(|note| note.range.start <= element.range.start && element.range.start < note.range.end) {
                continue;
            }
            let body = coordinates.slice_utf16(element.range.start..element.range.end).unwrap_or("");
            let mut words = 0;
            for entry in map.split_whitespace() {
                let (label, count) = entry.split_once('(').map_or((entry, 0), |(label, count)| (label, count.trim_end_matches(')').parse().unwrap_or(0)));
                if label.bytes().all(|byte| byte.is_ascii_digit()) && !label.is_empty() && known.insert(label.to_owned()) {
                    pages.push(page_at(label.to_owned(), "reporter-1", element.range.start + word_place(body, words), None, "pgmap"));
                }
                words += count;
            }
        } else if class == "pagebreak" {
            let printed = coordinates.slice_utf16(element.range.start..element.range.end).map(trim_js).unwrap_or("");
            if let Ok(number) = printed.parse::<u32>() {
                broken.push((number, element.range));
            }
        } else if class == "ldml-pagenumber" {
            blank.push(element.range);
        } else if class == "star-pagination" && marked.get(marked.partition_point(|start| start + 3 < element.range.start)).is_none_or(|start| *start > element.range.start + 3) {
            let printed = coordinates.slice_utf16(element.range.start..element.range.end).map(trim_js).unwrap_or("");
            let digits = printed.trim_start_matches(['*', ' ']).trim_start_matches("Page").trim();
            if digits.starts_with(|c: char| c.is_ascii_digit()) && digits.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-') {
                let series = pages.first().map_or("reporter".to_owned(), |page| page.series.clone());
                pages.push(page_at(digits.to_owned(), &series, element.range.start, Some(element.range), "markup"));
            }
        }
    }
    if let Some(first) = broken.first().map(|mark| mark.0).filter(|first| *first > 1) {
        pages.push(page_at((first - 1).to_string(), "slip", 0, None, "counted"));
    }
    for (number, range) in broken {
        pages.push(page_at(number.to_string(), "slip", range.start, Some(range), "markup"));
    }
    if pages.is_empty() && !blank.is_empty() {
        pages.push(page_at("1".to_owned(), "sheet", 0, None, "break"));
        for (index, range) in blank.into_iter().enumerate() {
            pages.push(page_at((index + 2).to_string(), "sheet", range.start, None, "break"));
        }
    }
}

/// How far past a citation's first page its case can run.
const PAGE_REACH: u32 = 200;
/// The least text between two pages' numbers, in UTF-16 units: closer, they are a list's or a headnote's.
const PAGE_TEXT: usize = 400;

/// A volume's leaves count its front matter, and so stand a constant ahead of its printed pages: "b620-3" and `pgmap="620"`
/// on page 580 of 78 N.M. The head matter's first block is on the case's first page, which its citation names; with no head
/// matter the opinion's first leaf stands for it, the pages reckoned and said to be. With no printed citation they stay leaves.
pub(super) fn leaves_to_pages(layer: &mut OpinionLayer, record: &OpinionRecord) {
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    let leaves = layer.pages.iter().enumerate().filter(|(_, page)| page.grammar.as_deref() == Some("pgmap")).filter_map(|(at, page)| Some((at, page_number(&page.printed_label())?))).collect::<Vec<_>>();
    let Some(first) = leaves.iter().map(|leaf| leaf.1).min() else { return };
    let head = record.cluster.iter().find(|(field, _)| field == "headmatter").and_then(|(_, matter)| {
        BLOCK.get_or_init(|| Regex::new(r#"\bid="b(\d+)-"#).unwrap()).captures(matter)?[1].parse::<u32>().ok()
    });
    let anchor = head.unwrap_or(first);
    let printed = |kinds: &[&str]| record.citations.iter().filter(|citation| kinds.contains(&citation.kind.as_str()) && !citation.reporter.contains("LEXIS"))
        .filter_map(|citation| page_number(&citation.page)).filter(|page| *page <= anchor).max();
    // The leaves are the book Harvard scanned, which it names: "ill-app-3d.306/280" is 306 Ill. App. 3d 280, not 239 Ill. Dec. 283.
    static SCANNED: OnceLock<Regex> = OnceLock::new();
    let scanned = record.cluster.iter().find(|(field, _)| field == "filepath_json_harvard")
        .and_then(|(_, path)| SCANNED.get_or_init(|| Regex::new(r"\.(\d+)/(\d+)\.\d+\.json$").unwrap()).captures(path))
        .and_then(|book| record.citations.iter().find(|citation| citation.volume == book[1] && citation.page == book[2]))
        .and_then(|citation| page_number(&citation.page)).filter(|page| *page <= anchor);
    let Some(opening) = scanned.or_else(|| printed(&["1", "2", "4", "5"])).or_else(|| printed(&["3"])) else {
        for (at, _) in leaves {
            layer.pages[at].series = "leaf".to_owned();
        }
        return;
    };
    for (at, leaf) in leaves {
        layer.pages[at].label = Some(format!("page{}", (leaf - (anchor - opening)).to_string()));
        if head.is_none() {
            layer.pages[at].grammar = Some(("reckoned").to_owned());
        }
    }
}

/// Page numbers printed in the text, each form its own numbering: "{**18 Misc 3d at 418}" the official reporter's,
/// "[*5]" a slip opinion's, "[original page 5]" the source's, and in a text whose source gives none "*517", "**18",
/// "*Page 169". A framed form says what it is; bare stars must run, and a bare "(642)" or "[522]" run from the first pages
/// of one of the case's printed citations.
pub(super) fn printed_page_marks(layer: &mut OpinionLayer, citations: &[OpinionCitation]) {
    static MARK: OnceLock<Regex> = OnceLock::new();
    let held = layer.pages.iter().filter_map(|page| page.series.strip_prefix("reporter-")?.parse::<usize>().ok()).collect::<HashSet<_>>();
    let numbered = |stars: usize| held.contains(&stars);
    let mark = MARK.get_or_init(|| Regex::new(r"\{\*+\d+\s[^{}\n]{1,30}\sat\s(\d{1,5})\}|\[ ?(\*+) ?(\d{1,5}) ?[\]J]|\[(?:(?i:original) )?(?i:page|pg\.?) (\d{1,5})\]|(\*{1,3}) ?(?:Page )?(\d{1,5})(?-u:\b)|\((\d{2,5})\)|\[(\d{2,5})\]").unwrap());
    let mut existing = layer.pages.iter().filter_map(|page| page.marker_range).collect::<Vec<_>>();
    existing.sort_by_key(|mark| mark.start);
    let cited = citations.iter().filter(|citation| !matches!(citation.kind.as_str(), "6" | "7" | "8") && !citation.reporter.contains("LEXIS")).filter_map(|citation| citation.page.parse::<u32>().ok()).collect::<Vec<_>>();
    let within_citation = |number: u32| cited.iter().any(|first| (*first..=first + PAGE_REACH).contains(&number));
    let opens_citation = |number: u32| cited.iter().any(|first| (*first..=first + 3).contains(&number));
    // By form: 1 brace, 3 "[page N]", 10 + n bracketed n stars, 20 + n bare n stars, 31 "(N)", 32 "[N]".
    let mut found = HashMap::<usize, Vec<(usize, usize, u32)>>::new();
    let mut units = Units::new(&layer.text);
    for capture in mark.captures_iter(&layer.text) {
        let whole = capture.get(0).unwrap();
        let form = if capture.get(1).is_some() { 1 } else if let Some(stars) = capture.get(2) { 10 + stars.len() } else if capture.get(4).is_some() { 3 } else if capture.get(7).is_some() { 31 } else if capture.get(8).is_some() { 32 } else { 20 + capture.get(5).map_or(1, |stars| stars.len()) };
        if (21..24).contains(&form) {
            let before = layer.text[..whole.start()].trim_end_matches(' ');
            // "at *3" pins a citation; a star on a word is a note's.
            if numbered(form - 20) || (before.len() == whole.start() && before.chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '*'))
                || before.ends_with(" at") || before.ends_with(',') {
                continue;
            }
        }
        if (12..14).contains(&form) && numbered(form - 10) {
            continue;
        }
        let Some(number) = [1, 3, 4, 6, 7, 8].iter().find_map(|&group| capture.get(group)).and_then(|number| number.as_str().parse().ok()) else { continue };
        if form > 30 && !within_citation(number) {
            continue;
        }
        let unit = units.at(whole.start());
        if existing[..existing.partition_point(|mark| mark.start <= unit)].last().is_some_and(|mark| unit < mark.end) {
            continue;
        }
        found.entry(form).or_default().push((unit, unit + utf16_len(whole.as_str()), number));
    }
    // "[*5]" alone is a slip opinion's; beside "[**709]" each count is a reporter.
    let several = found.keys().any(|form| (12..14).contains(form));
    let slip_held = layer.pages.iter().any(|page| page.series == "slip");
    let mut pages = Vec::new();
    for (form, marks) in found {
        let mut run = Vec::<(usize, usize, u32)>::new();
        let mut longest = Vec::new();
        for mark in marks {
            if run.last().is_some_and(|last| mark.2 <= last.2 || mark.2 - last.2 > if form > 30 { 1 } else { 3 }) {
                if run.len() > longest.len() { longest = std::mem::take(&mut run); }
                run.clear();
            }
            run.push(mark);
        }
        if run.len() > longest.len() { longest = run; }
        if longest.len() < match form { 0..20 => 1, 31.. => 2, _ => 3 } { continue; }
        if form > 30 && !(opens_citation(longest[0].2) && longest.windows(2).all(|pair| pair[1].0 - pair[0].1 >= PAGE_TEXT)) { continue; }
        let series = match form {
            1 => "reporter-official".to_owned(),
            3 => "original".to_owned(),
            11 if !several && slip_held && !numbered(1) => "slip-printed".to_owned(),
            11 if numbered(1) => "reporter-printed".to_owned(),
            11 if !several => "slip".to_owned(),
            31.. => "reporter-printed".to_owned(),
            bracket if bracket < 20 => format!("reporter-{}", bracket - 10),
            stars => format!("reporter-{}", stars - 20),
        };
        for (index, &(start, end, number)) in longest.iter().enumerate() {
            pages.push(read_page(number.to_string(), series.clone(), start, longest.get(index + 1).map_or(utf16_len(&layer.text), |next| next.0), Some(ScalarRange { start, end }), "printed"));
        }
    }
    layer.pages.extend(pages);
}

/// Head matter Harvard sets ahead of the opinion, wherever the book printed it: counsel printed on the case's last page, dates,
/// the syllabus. A mark inside one numbers that block alone.
const MOVED_BLOCKS: [&str; 12] = ["attorneys", "otherdate", "decisiondate", "docketnumber", "parties", "judges", "syllabus", "summary", "headnotes", "court", "seealso", "citation"];

/// Rule one of a printed numbering: read in order, its pages never go down. In the opinion's own text a mark lower than one
/// before it is the source's error ("*1308 controversy ... *1307" inside the block quotation that follows): the heaviest run that
/// never falls keeps its marks and the rest are strays. A mark in a block the source moved ahead of the opinion ("*462" on counsel
/// printed after it) is that block's page and takes no part in the run; it numbers no text after its block.
pub(super) fn never_down(structure: &DocumentStructure, pages: &mut [StructureNode]) {
    let moved = structure.markup_elements.iter().filter(|element| MOVED_BLOCKS.contains(&element.tag.as_str())).map(|element| element.range).collect::<Vec<_>>();
    let in_moved = |start: usize| moved.iter().any(|block| block.start <= start && start < block.end);
    let mut series = pages.iter().filter(|page| page.series.starts_with("reporter") && !page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("stray")).map(|page| page.series.clone()).collect::<Vec<_>>();
    series.sort();
    series.dedup();
    for name in series {
        let mut marks = pages.iter().enumerate().filter(|(_, page)| page.series == name && !page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("stray") && !in_moved(page.range.start))
            .filter_map(|(at, page)| Some((page.range.start, at, page_number(&page.printed_label())?))).collect::<Vec<_>>();
        marks.sort_unstable();
        // The longest run that never falls, by patience: each mark extends the longest run ending at or below its number.
        let (mut best, mut back) = (vec![1usize; marks.len()], vec![usize::MAX; marks.len()]);
        for at in 0..marks.len() {
            for before in 0..at {
                if marks[before].2 <= marks[at].2 && best[before] + 1 > best[at] {
                    best[at] = best[before] + 1;
                    back[at] = before;
                }
            }
        }
        let Some(mut at) = (0..marks.len()).max_by_key(|at| (best[*at], std::cmp::Reverse(*at))) else { continue };
        let mut kept = HashSet::new();
        loop {
            kept.insert(marks[at].1);
            if back[at] == usize::MAX { break; }
            at = back[at];
        }
        for (_, index, _) in &marks {
            if !kept.contains(index) {
                pages[*index].grammar = Some(("stray").to_owned());
            }
        }
        // A moved block's mark: its page holds the block and ends with it.
        for page in pages.iter_mut().filter(|page| page.series == name && !page.properties.contains_key("in_note") && in_moved(page.range.start)) {
            if let Some(block) = moved.iter().find(|block| block.start <= page.range.start && page.range.start < block.end) {
                page.range.end = block.end;
                page.grammar = Some(("moved").to_owned());
            }
        }
    }
}

/// A numbering the document keeps for itself, no reporter's.
fn own_count(series: &str) -> bool {
    matches!(series, "slip" | "ecf" | "sheet" | "original" | "leaf") || series.starts_with("slip-")
}

/// A numbering climbs. Marks of one source's numbering that run apart from its longest run, "364 365 366 697 367", are a
/// second printing if one of the case's citations opens there; otherwise strays, as a rendition's closing "*Page 264" is.
fn separate_printings(layer: &mut OpinionLayer) {
    const STEP: u32 = 3;
    let names = layer.pages.iter().map(|page| page.series.clone()).filter(|series| !own_count(series) && !series.ends_with("-b")).collect::<HashSet<_>>();
    for series in names {
        let mut members = layer.pages.iter().enumerate().filter(|(_, page)| page.series == series && !page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("stray"))
            .filter_map(|(at, page)| Some((page.range.start, at, page_number(&page.printed_label())?))).collect::<Vec<_>>();
        members.sort_by_key(|member| (member.0, member.2));
        let mut runs = Vec::<Vec<(usize, u32)>>::new();
        for &(_, at, number) in &members {
            match runs.iter_mut().find(|run| run.last().is_some_and(|last| last.1 < number && number <= last.1 + STEP)) {
                Some(run) => run.push((at, number)),
                None => runs.push(vec![(at, number)]),
            }
        }
        if runs.len() < 2 {
            continue;
        }
        let longest = (0..runs.len()).max_by_key(|run| runs[*run].len()).unwrap_or(0);
        let (low, high) = (runs[longest][0].1, runs[longest][runs[longest].len() - 1].1);
        let printed = |page: u32| layer.citations.iter().filter(|citation| !matches!(citation.kind.as_str(), "6" | "7" | "8")).filter_map(|citation| page_number(&citation.page)).filter(|first| *first <= page + 1 && page + 1 - first <= PAGE_REACH).collect::<Vec<_>>();
        let held = printed(low);
        let mut second = Vec::new();
        let mut strays = Vec::new();
        for run in runs.iter().enumerate().filter(|(index, _)| *index != longest).map(|(_, run)| run) {
            // Within the longest run's reach a mark is out of its place, not out of the numbering.
            if run.iter().all(|mark| low.saturating_sub(STEP) <= mark.1 && mark.1 <= high + STEP) {
                continue;
            }
            if printed(run[0].1).iter().any(|first| !held.contains(first)) {
                second.extend(run.iter().map(|mark| mark.0));
            } else {
                strays.extend(run.iter().map(|mark| mark.0));
            }
        }
        for at in second {
            layer.pages[at].series = format!("{series}-b");
        }
        for at in strays {
            layer.pages[at].grammar = Some(("stray").to_owned());
        }
    }
}

/// The most text a reporter's page holds, in UTF-16 units.
const ONE_PAGE: usize = 7000;
/// How much worse than the nearest citation the book a case is filed under may fit its marks, in pages: a separate writing's marks
/// open far into its case, by chance beside a parallel reporter's first page.
const BOOK_MARGIN: u32 = 30;

/// Which printing each numbering is. A numbering that names its reporter (a Lexis page scheme) is that citation; any
/// other is the citation that opens nearest below its first page, Lexis's and Westlaw's star numbers aside, which count
/// from 1. Pages before the first mark open on that citation's page. Without the table, the head's own citation lines.
pub(super) fn page_series(layer: &mut OpinionLayer, record: &OpinionRecord) {
    static OWN: OnceLock<Regex> = OnceLock::new();
    if layer.citations.is_empty() {
        let own = OWN.get_or_init(|| Regex::new(r"^(\d{1,4})\s+(\D.*?)\s(\d{1,5})\b").unwrap());
        layer.citations = layer.front.iter().filter(|line| line.role == "citation").flat_map(|line| line.text.split(';')).filter_map(|citation| {
            let capture = own.captures(citation.trim())?;
            Some(OpinionCitation { volume: capture[1].to_owned(), reporter: capture[2].to_owned(), page: capture[3].to_owned(), kind: String::new() })
        }).collect();
    }
    separate_printings(layer);
    // Harvard files a case under the book it scanned, "law.free.cap.us.476/426.12567.json", and its marks are that book's unless
    // another citation fits them far better: "nys.138/212" holds the marks of 78 Misc. 264 from 266.
    static SCANNED: OnceLock<Regex> = OnceLock::new();
    let scanned = record.cluster.iter().find(|(field, _)| field == "filepath_json_harvard")
        .and_then(|(_, path)| SCANNED.get_or_init(|| Regex::new(r"\.(\d+)/(\d+)\.\d+\.json$").unwrap()).captures(path))
        .and_then(|book| layer.citations.iter().find(|citation| citation.volume == book[1] && citation.page == book[2]).cloned());
    // The first mark in the text's order: one stray low label is no numbering's start. Nor is a first mark the next one falls
    // below: a block the source moved ahead of the opinion, "*394" on counsel printed at the case's end, before "*391".
    let mut by_series = HashMap::<String, Vec<(usize, u32)>>::new();
    for page in layer.pages.iter().filter(|page| !page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("stray") && !own_count(&page.series)) {
        if let Some(number) = page_number(&page.printed_label()) {
            by_series.entry(page.series.clone()).or_default().push((page.range.start, number));
        }
    }
    let firsts = by_series.into_iter().map(|(series, mut marks)| {
        marks.sort_unstable();
        let first = if marks.len() > 1 && marks[1].1 < marks[0].1 { marks[1] } else { marks[0] };
        (series, (first.1, first.0))
    }).collect::<HashMap<_, _>>();
    let marked = !firsts.is_empty();
    let mut lowest_of = HashMap::<String, u32>::new();
    for page in layer.pages.iter().filter(|page| !page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("stray") && page.grammar.as_deref() != Some("reckoned")) {
        if let Some(number) = page_number(&page.printed_label()) {
            let entry = lowest_of.entry(page.series.clone()).or_insert(number);
            *entry = number.min(*entry);
        }
    }
    let mut named = Vec::new();
    let mut opening = Vec::new();
    let mut nearest_to = HashMap::<String, u32>::new();
    let mut distances = HashMap::<String, u32>::new();
    // The book Harvard scanned names its own marks; a numbering named only for opening nearest a citation keeps the others it
    // could open on, nearest first, (citation, pages from its first mark), in case the book's marks take that one.
    let mut by_book = HashSet::<String>::new();
    let mut fallbacks = HashMap::<String, Vec<(OpinionCitation, u32)>>::new();
    let mut first_of = HashMap::<String, u32>::new();
    for (series, (first, first_at)) in firsts {
        first_of.insert(series.clone(), first);
        let squeezed_series = squeezed(&series);
        // "T.C. Memo 2015-88" is the table's "2015 T.C. Memo. 88".
        let memo = squeezed_series.strip_prefix("T.C. Memo ").and_then(|rest| rest.split_once('-')).map(|(year, number)| format!("{year} T.C. Memo. {number}"));
        // A vendor's name on a reporter's pages: "1911 N.C. LEXIS 201" over 359, 360 is 157 N.C. 359, printed under the vendor's scheme.
        let vendor_scheme_only = |citation: &&OpinionCitation| (matches!(citation.kind.as_str(), "6" | "7") || citation.reporter.contains("LEXIS")) && first > 3 && page_number(&citation.page).is_some_and(|page| first.abs_diff(page) > 3);
        let closest = layer.citations.iter().filter(|citation| !matches!(citation.kind.as_str(), "6" | "7" | "8")).filter_map(|citation| page_number(&citation.page)).filter(|page| *page <= first + 1).map(|page| first + 1 - page).min();
        // Unless another reporter of record opens right at the marks, a page or two below the first, and the book further off:
        // "643 F.2d 10" is Harvard's book, but marks from 16 at the opinion's first words are 207 U.S. App. D.C. 15's.
        let exact = layer.citations.iter().filter(|citation| !matches!(citation.kind.as_str(), "6" | "7" | "8") && !citation.reporter.contains("LEXIS") && Some(citation.page.as_str()) != scanned.as_ref().map(|book| book.page.as_str()))
            .filter_map(|citation| page_number(&citation.page)).filter(|page| *page <= first + 1).map(|page| first + 1 - page).min();
        let book = scanned.as_ref().filter(|book| series == "reporter-1" && page_number(&book.page).is_some_and(|page| page <= first + 1 && first + 1 - page <= PAGE_REACH
            && closest.is_none_or(|closest| first + 1 - page <= closest + BOOK_MARGIN) && exact.is_none_or(|exact| exact > 2 || first + 1 - page < exact + 3)));
        let by_name = book.or_else(|| layer.citations.iter().find(|citation| citation.printed() == squeezed_series || squeezed_series.starts_with(&citation.printed()) || memo.as_deref() == Some(citation.printed().as_str())).filter(|citation| !vendor_scheme_only(citation)));
        // One printing can go by two names, "23 U.S. 181" and "10 Wheat. 181".
        let twins = |citation: &'_ OpinionCitation| layer.citations.iter().filter(|other| other.page == citation.page && other.printed() != citation.printed() && !matches!(other.kind.as_str(), "6" | "7" | "8") && !other.reporter.contains("LEXIS")).collect::<Vec<_>>();
        let chosen = by_name.map(|citation| std::iter::once(citation).chain(if book.is_some() { twins(citation) } else { Vec::new() }).collect::<Vec<_>>()).unwrap_or_else(|| {
            // A case's first mark can stand a page before its citation's: the printed page it shares with the case before.
            // Nor can it open past the numbering's lowest page.
            let lowest = lowest_of.get(&series).copied().unwrap_or(first).min(first);
            let opens = |citation: &&OpinionCitation| page_number(&citation.page).filter(|page| *page <= lowest + 1 && first + 1 - page <= PAGE_REACH).map(|page| first + 1 - page);
            // A vendor's number is a document's, however the table files it: "1964 N.J. LEXIS 195" is no page near 197.
            let eligible = layer.citations.iter().filter(|citation| !matches!(citation.kind.as_str(), "6" | "7") && !citation.reporter.contains("LEXIS") && citation.reporter != "WL").collect::<Vec<_>>();
            // A separate writing opens far into its case, near some other reporter's first page by chance: "146 L. Ed. 2d 389" over
            // the marks of 529 U.S. 362 from 390. The reporters of record are asked first; the specialty and parallel federal
            // series, "L. Ed.", "S. Ct.", "Ohio Op.", only where none of those fits.
            static SECONDARY: OnceLock<Regex> = OnceLock::new();
            let series_of_record = SECONDARY.get_or_init(|| Regex::new(r"L\. ?Ed|S\. ?Ct|Ohio Op|Law Rep|Weekly|Cas\.|BNA|A\.? ?L\.? ?R|Am\. ?(?:Dec|Rep|St)|U\.? ?S\.? ?L\.? ?W|Lawy\.").unwrap());
            let secondary = |citation: &&OpinionCitation| series_of_record.is_match(&citation.reporter);
            let of_record = eligible.iter().copied().filter(|citation| citation.kind != "8" && !secondary(citation) && opens(citation).is_some()).collect::<Vec<_>>();
            let pool = if of_record.is_empty() { eligible.iter().copied().filter(|citation| citation.kind != "8").collect::<Vec<_>>() } else { of_record };
            let nearest = pool.iter().filter_map(|citation| opens(citation)).min();
            let mut ranked = pool.iter().copied().filter(|citation| opens(citation).is_some()).collect::<Vec<_>>();
            ranked.sort_by_key(|citation| opens(citation));
            fallbacks.insert(series.clone(), ranked.into_iter().map(|citation| (citation.clone(), opens(&citation).unwrap_or(u32::MAX))).collect());
            let mut printed = pool.iter().copied().filter(|citation| opens(citation) == nearest && nearest.is_some()).collect::<Vec<_>>();
            // The table files some reporters as neutral citations, "60 M.J. 178": past page 3, marks that open at one are its pages.
            if printed.is_empty() && first > 3 {
                let filed = eligible.iter().filter(|citation| citation.kind == "8").filter_map(|citation| opens(citation)).min();
                printed = eligible.iter().copied().filter(|citation| citation.kind == "8" && filed.is_some() && opens(citation) == filed).collect();
            }
            // A neutral or a vendor's citation numbers no printed pages: the opinion's own, counted from 1, are all it has.
            if printed.is_empty() && first <= 3 { layer.citations.iter().filter(|citation| matches!(citation.kind.as_str(), "6" | "7" | "8")).collect() } else { printed }
        });
        let Some(citation) = chosen.first() else { continue };
        if book.is_some() {
            by_book.insert(series.clone());
        }
        if by_name.is_some() {
            fallbacks.remove(&series);
        }
        let distance = page_number(&citation.page).map_or(u32::MAX, |page| first.abs_diff(page));
        let nearest = nearest_to.entry(citation.printed()).or_insert(distance);
        *nearest = distance.min(*nearest);
        distances.insert(series.clone(), distance);
        if by_name.is_none() {
            if let Some(page) = page_number(&citation.page).filter(|_| !matches!(citation.kind.as_str(), "6" | "7" | "8")) {
                if page < first && first_at > 0 && !layer.pages.iter().any(|known| known.series == series && known.range.start == 0) {
                    if first - page <= 3 {
                        opening.push(read_page(citation.page.clone(), series.clone(), 0, 0, None, "citation"));
                    } else if first_at <= ONE_PAGE {
                        // An opinion that opens a few pages into its case: the words before the first mark are on the page before it.
                        opening.push(read_page((first - 1).to_string(), series.clone(), 0, 0, None, "reckoned"));
                    }
                }
            }
        }
        named.push(PageSeries {
            series, citation: citation.printed(), reporter: format!("{} {}", citation.volume, citation.reporter),
            also: chosen[1..].iter().map(|other| other.printed()).collect(),
        });
    }
    // The scanned book's marks are its pages, however near another numbering opens to its first page: "259 Ala. 152" is Harvard's
    // marks from 157, not Lawbox's from 153, which are 66 So. 2d 151's. A numbering named only by nearness to the book's citation
    // takes the next citation it could open on that no other numbering holds, or none.
    let books = named.iter().filter(|known| by_book.contains(&known.series)).map(|known| known.citation.clone()).collect::<HashSet<_>>();
    if !books.is_empty() {
        let held = named.iter().map(|known| known.citation.clone()).collect::<HashSet<_>>();
        let renamed = named.iter().filter(|known| !by_book.contains(&known.series) && books.contains(&known.citation)).map(|known| known.series.clone()).collect::<HashSet<_>>();
        // Pages set before a renamed numbering's first mark were set from the book's citation.
        opening.retain(|page: &StructureNode| !renamed.contains(&page.series));
        for known in named.iter_mut().filter(|known| renamed.contains(&known.series)) {
            let next = fallbacks.get(&known.series).and_then(|ranked| ranked.iter().find(|(citation, _)| !held.contains(&citation.printed())));
            match next {
                Some((citation, _)) => {
                    let distance = page_number(&citation.page).zip(first_of.get(&known.series)).map_or(u32::MAX, |(page, first)| first.abs_diff(page));
                    known.citation = citation.printed();
                    known.reporter = format!("{} {}", citation.volume, citation.reporter);
                    known.also = Vec::new();
                    distances.insert(known.series.clone(), distance);
                    let nearest = nearest_to.entry(citation.printed()).or_insert(distance);
                    *nearest = distance.min(*nearest);
                }
                None => known.citation.clear(),
            }
        }
        named.retain(|known| !known.citation.is_empty());
        for citation in &books {
            let best = named.iter().filter(|known| by_book.contains(&known.series) && known.citation == *citation).filter_map(|known| distances.get(&known.series)).min().copied();
            if let Some(best) = best {
                nearest_to.insert(citation.clone(), best);
            }
        }
    }
    // One citation names one printing: a numbering a page or so off the nearest is that printing marked another way.
    named.retain(|known: &PageSeries| nearest_to.get(&known.citation).zip(distances.get(&known.series)).is_none_or(|(nearest, own)| *own <= nearest + 3));
    opening.retain(|page: &StructureNode| page.grammar.as_deref() != Some("citation") || named.iter().any(|known| known.series == page.series));
    // A short opinion prints no page mark: it still opens on each page its citations give.
    if !marked {
        for (index, citation) in layer.citations.iter().filter(|citation| !matches!(citation.kind.as_str(), "6" | "7" | "8") && page_number(&citation.page).is_some()).enumerate() {
            let series = format!("citation-{}", index + 1);
            opening.push(read_page(citation.page.clone(), series.clone(), 0, 0, None, "citation"));
            named.push(PageSeries { series, citation: citation.printed(), reporter: format!("{} {}", citation.volume, citation.reporter), also: Vec::new() });
        }
    }
    named.sort_by(|left, right| left.series.cmp(&right.series));
    layer.pages.extend(opening);
    layer.page_series = named;
}

/// How far past the last citing pin, in UTF-16 units, a passage still takes that page or the next.
const CITERS_REACH: usize = 1500;

/// A reporter's pages as later opinions pin them, where no version marks them: each pin puts a page at the place in the text its
/// quotation stands, `(place, page)`. Citers err, so the numbering is the heaviest run of pins whose pages never fall as the text
/// goes on and climb no faster than text can fill them; a pin's weight is how many opinions agree on it. Returns (place, page, weight).
pub fn citers_numbering(pins: &[(usize, u32)]) -> Vec<(usize, u32, u32)> {
    let mut weights = HashMap::<(usize, u32), u32>::new();
    for pin in pins {
        *weights.entry(*pin).or_default() += 1;
    }
    let mut rows = weights.into_iter().map(|((place, page), weight)| (place, page, weight)).collect::<Vec<_>>();
    rows.sort_unstable();
    let (mut best, mut back) = (vec![0_u32; rows.len()], vec![usize::MAX; rows.len()]);
    for at in 0..rows.len() {
        let (place, page, weight) = rows[at];
        best[at] = weight;
        for before in 0..at {
            let (earlier, low, _) = rows[before];
            if low <= page && page - low <= (((place - earlier) / 300) as u32 + 3) && best[before] + weight > best[at] {
                best[at] = best[before] + weight;
                back[at] = before;
            }
        }
    }
    let Some(mut at) = (0..rows.len()).max_by_key(|at| best[*at]) else { return Vec::new() };
    let mut run = vec![rows[at]];
    while back[at] != usize::MAX {
        at = back[at];
        run.push(rows[at]);
    }
    run.reverse();
    on_the_line(run)
}

/// A case's pages hold a steady amount of text, so its pins lie near one line through place and page. A pin far off the line is wrong,
/// however many opinions repeat it: copying adds votes, not room on the page.
fn on_the_line(run: Vec<(usize, u32, u32)>) -> Vec<(usize, u32, u32)> {
    /// The fewest pins that can say where a line runs.
    const LEAST: usize = 4;
    /// How far off the line a pin may stand, in pages.
    const OFF_THE_LINE: f64 = 6.0;
    let mut slopes = run.windows(2).filter(|pair| pair[1].1 > pair[0].1).map(|pair| (pair[1].0 - pair[0].0) as f64 / f64::from(pair[1].1 - pair[0].1)).collect::<Vec<_>>();
    if run.len() < LEAST || slopes.len() < LEAST - 1 {
        return run;
    }
    slopes.sort_by(f64::total_cmp);
    let per_page = slopes[slopes.len() / 2];
    if per_page <= 0.0 {
        return run;
    }
    let mut offsets = run.iter().map(|pin| f64::from(pin.1) - pin.0 as f64 / per_page).collect::<Vec<_>>();
    offsets.sort_by(f64::total_cmp);
    let offset = offsets[offsets.len() / 2];
    run.into_iter().filter(|pin| (f64::from(pin.1) - pin.0 as f64 / per_page - offset).abs() <= OFF_THE_LINE).collect()
}

/// The page or pair of pages a passage falls on between two citing pins. A gap of up to three pages is reckoned by place.
pub fn citers_page(numbering: &[(usize, u32, u32)], place: usize) -> Option<(u32, u32)> {
    let after = numbering.partition_point(|pin| pin.0 <= place);
    let (low, high) = (after.checked_sub(1).map(|at| numbering[at]), numbering.get(after).copied());
    match (low, high) {
        (Some(low), Some(high)) if high.1 - low.1 <= 1 => Some((low.1, high.1)),
        (Some(low), Some(high)) if high.1 - low.1 <= 3 => {
            let page = low.1 + ((place - low.0) as u64 * u64::from(high.1 - low.1) * 2 / (high.0 - low.0).max(1) as u64).div_ceil(2) as u32;
            Some((page, page))
        }
        (Some(low), None) if place - low.0 < CITERS_REACH => Some((low.1, low.1 + 1)),
        (None, Some(high)) if high.0 - place < CITERS_REACH => Some((high.1.saturating_sub(1), high.1)),
        _ => None,
    }
}

/// Each numbering's pages, ready to say where a passage falls in all of them.
pub(super) fn pin_table<'a>(pages: impl IntoIterator<Item = &'a StructureNode>, page_series: &[PageSeries]) -> Vec<(String, Option<String>, Vec<(usize, String)>)> {
    let mut table = HashMap::<&str, Vec<(usize, String)>>::new();
    // An estimated mark stands for no label: the words past it fall between the marks either side, on no page that can be named.
    let mut ordered = pages.into_iter().filter(|page| !page.properties.contains_key("in_note") && page.grammar.as_deref() != Some("stray")).collect::<Vec<_>>();
    ordered.sort_by_key(|page| page.range.start);
    for (index, page) in ordered.iter().enumerate() {
        let rows = table.entry(&page.series).or_default();
        if page.grammar.as_deref() == Some("moved") {
            // The block's page, then the opinion's page again where the block ends.
            rows.push((page.range.start, page.printed_label().to_owned()));
            let resumed = rows.len().checked_sub(2).map(|before| rows[before].1.clone()).unwrap_or_default();
            rows.push((page.range.end, resumed));
            continue;
        }
        let number = page_number(&page.printed_label());
        // A numbering that skips a page in the opinion's own text lost that page's mark (a block the source moved took it, or
        // the source dropped it): where the turn falls is not known, so the words after the last mark before it have no page.
        let skips = ordered[index + 1..].iter().find(|next| next.series == page.series && next.grammar.as_deref() != Some("moved"))
            .and_then(|next| page_number(next.printed_label())).zip(number).is_some_and(|(next, here)| next > here + 1);
        rows.push((page.range.start, if page.grammar.as_deref() == Some("reckoned") || skips { String::new() } else { page.printed_label().to_owned() }));
    }
    for rows in table.values_mut() {
        rows.sort_by_key(|row| row.0);
    }
    // A numbering of one mark is a citation's first page: it says where the opinion opens, not where anything in it stands.
    table.retain(|_, pages| pages.len() >= 2);
    let mut rows = table.into_iter().map(|(series, mut pages)| {
        pages.sort_by_key(|page| page.0);
        let reporter = page_series.iter().find(|known| known.series == series).map(|known| known.reporter.clone());
        (series.to_owned(), reporter, pages)
    }).collect::<Vec<_>>();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows
}

pub(super) fn pins_of(table: &[(String, Option<String>, Vec<(usize, String)>)], start: usize, end: usize) -> Vec<Pin> {
    table.iter().filter_map(|(series, reporter, pages)| {
        let at = pages.partition_point(|page| page.0 <= start).checked_sub(1)?;
        let last = pages.partition_point(|page| page.0 < end).checked_sub(1)?;
        if pages[at].1.is_empty() || pages[last].1.is_empty() {
            return None;
        }
        Some(Pin { series: series.clone(), reporter: reporter.clone(), page: pages[at].1.clone(), through: (last > at).then(|| pages[last].1.clone()) })
    }).collect()
}
