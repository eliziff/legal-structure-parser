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
pub(super) fn element_pages(structure: &DocumentStructure, coordinates: &ScalarText, notes: &[ScalarRange], pages: &mut Vec<OpinionPage>) {
    let mut known = pages.iter().filter(|page| page.series.starts_with("reporter")).map(|page| page.label.clone()).collect::<HashSet<_>>();
    let page_at = |label: String, series: &str, start: usize, marker: Option<ScalarRange>, source: &'static str| {
        OpinionPage { label, series: series.to_owned(), start, end: 0, marker, source, in_note: false }
    };
    let (mut broken, mut blank) = (Vec::new(), Vec::new());
    let mut marked = pages.iter().map(|page| page.start).collect::<Vec<_>>();
    marked.sort_unstable();
    for element in &structure.markup_elements {
        let class = element.attributes.get("class").map_or("", String::as_str);
        if let Some(map) = element.attributes.get("pgmap") {
            if notes.iter().any(|note| note.start <= element.range.start && element.range.start < note.end) {
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
    let leaves = layer.pages.iter().enumerate().filter(|(_, page)| page.source == "pgmap").filter_map(|(at, page)| Some((at, page_number(&page.label)?))).collect::<Vec<_>>();
    let Some(first) = leaves.iter().map(|leaf| leaf.1).min() else { return };
    let head = record.cluster.iter().find(|(field, _)| field == "headmatter").and_then(|(_, matter)| {
        BLOCK.get_or_init(|| Regex::new(r#"\bid="b(\d+)-"#).unwrap()).captures(matter)?[1].parse::<u32>().ok()
    });
    let anchor = head.unwrap_or(first);
    let printed = |kinds: &[&str]| record.citations.iter().filter(|citation| kinds.contains(&citation.kind.as_str()) && !citation.reporter.contains("LEXIS"))
        .filter_map(|citation| page_number(&citation.page)).filter(|page| *page <= anchor).max();
    let Some(opening) = printed(&["1", "2", "4", "5"]).or_else(|| printed(&["3"])) else {
        for (at, _) in leaves {
            layer.pages[at].series = "leaf".to_owned();
        }
        return;
    };
    for (at, leaf) in leaves {
        layer.pages[at].label = (leaf - (anchor - opening)).to_string();
        if head.is_none() {
            layer.pages[at].source = "reckoned";
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
    let mut existing = layer.pages.iter().filter_map(|page| page.marker).collect::<Vec<_>>();
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
            pages.push(OpinionPage {
                label: number.to_string(), series: series.clone(), start, end: longest.get(index + 1).map_or(utf16_len(&layer.text), |next| next.0),
                marker: Some(ScalarRange { start, end }), source: "printed", in_note: false,
            });
        }
    }
    layer.pages.extend(pages);
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
        let mut members = layer.pages.iter().enumerate().filter(|(_, page)| page.series == series && !page.in_note && page.source != "stray")
            .filter_map(|(at, page)| Some((page.start, at, page_number(&page.label)?))).collect::<Vec<_>>();
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
            layer.pages[at].source = "stray";
        }
    }
}

/// The most text a reporter's page holds, in UTF-16 units.
const ONE_PAGE: usize = 7000;

/// Which printing each numbering is. A numbering that names its reporter (a Lexis page scheme) is that citation; any
/// other is the citation that opens nearest below its first page, Lexis's and Westlaw's star numbers aside, which count
/// from 1. Pages before the first mark open on that citation's page. Without the table, the head's own citation lines.
pub(super) fn page_series(layer: &mut OpinionLayer) {
    static OWN: OnceLock<Regex> = OnceLock::new();
    if layer.citations.is_empty() {
        let own = OWN.get_or_init(|| Regex::new(r"^(\d{1,4})\s+(\D.*?)\s(\d{1,5})\b").unwrap());
        layer.citations = layer.front.iter().filter(|line| line.role == "citation").flat_map(|line| line.text.split(';')).filter_map(|citation| {
            let capture = own.captures(citation.trim())?;
            Some(OpinionCitation { volume: capture[1].to_owned(), reporter: capture[2].to_owned(), page: capture[3].to_owned(), kind: String::new() })
        }).collect();
    }
    separate_printings(layer);
    // The first mark in the text's order: one stray low label is no numbering's start.
    let mut firsts = HashMap::<String, (u32, usize)>::new();
    for page in layer.pages.iter().filter(|page| !page.in_note && page.source != "stray" && !own_count(&page.series)) {
        if let Some(number) = page_number(&page.label) {
            let entry = firsts.entry(page.series.clone()).or_insert((number, page.start));
            if page.start < entry.1 { *entry = (number, page.start); }
        }
    }
    let marked = !firsts.is_empty();
    let mut named = Vec::new();
    let mut opening = Vec::new();
    let mut nearest_to = HashMap::<String, u32>::new();
    let mut distances = HashMap::<String, u32>::new();
    for (series, (first, first_at)) in firsts {
        let squeezed_series = squeezed(&series);
        // "T.C. Memo 2015-88" is the table's "2015 T.C. Memo. 88".
        let memo = squeezed_series.strip_prefix("T.C. Memo ").and_then(|rest| rest.split_once('-')).map(|(year, number)| format!("{year} T.C. Memo. {number}"));
        // A vendor's name on a reporter's pages: "1911 N.C. LEXIS 201" over 359, 360 is 157 N.C. 359, printed under the vendor's scheme.
        let vendor_scheme_only = |citation: &&OpinionCitation| (matches!(citation.kind.as_str(), "6" | "7") || citation.reporter.contains("LEXIS")) && first > 3 && page_number(&citation.page).is_some_and(|page| first.abs_diff(page) > 3);
        let by_name = layer.citations.iter().find(|citation| citation.printed() == squeezed_series || squeezed_series.starts_with(&citation.printed()) || memo.as_deref() == Some(citation.printed().as_str())).filter(|citation| !vendor_scheme_only(citation));
        let chosen = by_name.map(|citation| vec![citation]).unwrap_or_else(|| {
            // A case's first mark can stand a page before its citation's: the printed page it shares with the case before.
            let opens = |citation: &&OpinionCitation| page_number(&citation.page).filter(|page| *page <= first + 1 && first + 1 - page <= PAGE_REACH).map(|page| first + 1 - page);
            let eligible = layer.citations.iter().filter(|citation| !matches!(citation.kind.as_str(), "6" | "7")).collect::<Vec<_>>();
            let nearest = eligible.iter().filter(|citation| citation.kind != "8").filter_map(|citation| opens(citation)).min();
            let printed = eligible.iter().copied().filter(|citation| citation.kind != "8" && opens(citation) == nearest && nearest.is_some()).collect::<Vec<_>>();
            // A neutral or a vendor's citation numbers no printed pages: the opinion's own, counted from 1, are all it has.
            if printed.is_empty() && first <= 3 { layer.citations.iter().filter(|citation| matches!(citation.kind.as_str(), "6" | "7" | "8")).collect() } else { printed }
        });
        let Some(citation) = chosen.first() else { continue };
        let distance = page_number(&citation.page).map_or(u32::MAX, |page| first.abs_diff(page));
        let nearest = nearest_to.entry(citation.printed()).or_insert(distance);
        *nearest = distance.min(*nearest);
        distances.insert(series.clone(), distance);
        if by_name.is_none() {
            if let Some(page) = page_number(&citation.page).filter(|_| !matches!(citation.kind.as_str(), "6" | "7" | "8")) {
                if page < first && first_at > 0 && !layer.pages.iter().any(|known| known.series == series && known.start == 0) {
                    if first - page <= 3 {
                        opening.push(OpinionPage { label: citation.page.clone(), series: series.clone(), start: 0, end: 0, marker: None, source: "citation", in_note: false });
                    } else if first_at <= ONE_PAGE {
                        // An opinion that opens a few pages into its case: the words before the first mark are on the page before it.
                        opening.push(OpinionPage { label: (first - 1).to_string(), series: series.clone(), start: 0, end: 0, marker: None, source: "reckoned", in_note: false });
                    }
                }
            }
        }
        named.push(PageSeries {
            series, citation: citation.printed(), reporter: format!("{} {}", citation.volume, citation.reporter),
            also: chosen[1..].iter().map(|other| other.printed()).collect(),
        });
    }
    // One citation names one printing: a numbering a page or so off the nearest is that printing marked another way.
    named.retain(|known: &PageSeries| nearest_to.get(&known.citation).zip(distances.get(&known.series)).is_none_or(|(nearest, own)| *own <= nearest + 3));
    opening.retain(|page: &OpinionPage| page.source != "citation" || named.iter().any(|known| known.series == page.series));
    // A short opinion prints no page mark: it still opens on each page its citations give.
    if !marked {
        for (index, citation) in layer.citations.iter().filter(|citation| !matches!(citation.kind.as_str(), "6" | "7" | "8") && page_number(&citation.page).is_some()).enumerate() {
            let series = format!("citation-{}", index + 1);
            opening.push(OpinionPage { label: citation.page.clone(), series: series.clone(), start: 0, end: 0, marker: None, source: "citation", in_note: false });
            named.push(PageSeries { series, citation: citation.printed(), reporter: format!("{} {}", citation.volume, citation.reporter), also: Vec::new() });
        }
    }
    named.sort_by(|left, right| left.series.cmp(&right.series));
    layer.pages.extend(opening);
    layer.page_series = named;
}

/// Each numbering's pages, ready to say where a passage falls in all of them.
pub(super) fn pin_table(layer: &OpinionLayer) -> Vec<(String, Option<String>, Vec<(usize, String)>)> {
    let mut table = HashMap::<&str, Vec<(usize, String)>>::new();
    for page in layer.pages.iter().filter(|page| !page.in_note && page.source != "stray") {
        table.entry(&page.series).or_default().push((page.start, page.label.clone()));
    }
    let mut rows = table.into_iter().map(|(series, mut pages)| {
        pages.sort_by_key(|page| page.0);
        let reporter = layer.page_series.iter().find(|known| known.series == series).map(|known| known.reporter.clone());
        (series.to_owned(), reporter, pages)
    }).collect::<Vec<_>>();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows
}

pub(super) fn pins_of(table: &[(String, Option<String>, Vec<(usize, String)>)], start: usize, end: usize) -> Vec<Pin> {
    table.iter().filter_map(|(series, reporter, pages)| {
        let at = pages.partition_point(|page| page.0 <= start).checked_sub(1)?;
        let last = pages.partition_point(|page| page.0 < end).checked_sub(1)?;
        Some(Pin { series: series.clone(), reporter: reporter.clone(), page: pages[at].1.clone(), through: (last > at).then(|| pages[last].1.clone()) })
    }).collect()
}
