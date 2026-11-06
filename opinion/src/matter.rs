//! What surrounds the opinion's text: the front lines, the reporter's matter, the record's gaps and kind.

use super::*;
use legal_structure::judgment::{JUDGMENT, ORDERED};

/// A long line of the head, not the opinion's first sentence: it opens as a court's or a filing's line does, or closes
/// on a party's role. A sentence may open with one ("Plaintiff Troy H. brings this action ...").
pub(super) fn caption_line(body: &str) -> bool {
    static CAPTION: OnceLock<Regex> = OnceLock::new();
    CAPTION.get_or_init(|| Regex::new(r"(?i)^(?:appeals? from|on appeal|before\b|argued|submitted|filed|in the\b|attorneys? for|on (?:the )?brief)|\b(?:plaintiffs?|defendants?|appell(?:ant|ee)s?|petitioners?|respondents?)[,.]?$").unwrap()).is_match(body)
}

pub(super) fn front_role(body: &str) -> Option<&'static str> {
    static ROLES: OnceLock<[(Regex, &'static str); 11]> = OnceLock::new();
    ROLES.get_or_init(|| [
        (Regex::new(r"^\d{1,4}\s+[A-Z][A-Za-z.\d ]{0,20}\s+\d{1,5}(?:\s*\(\d{4}\))?\.?(?:\s*[;,]\s*\d.*)?$").unwrap(), "citation"),
        (Regex::new(r"(?i)^(?:per curiam\b.*|.{2,60},\s*(?:u\.\s?s\.\s+|united states\s+)?(?:(?:chief|senior|presiding|associate)\s+)?(?:(?:district|circuit|magistrate|bankruptcy)\s+)?(?:judge|justice)(?:\s*\((?:concurring|dissenting)[^)]*\))?[.:]?|.{2,60},\s*(?:[cp]\.\s*)?j\.[,:]?|(?:mr\.\s+(?:chief\s+)?|chief\s+)justice\s+\S.{0,40}[:.]|.{0,60}deliver(?:ing|ed) the opinion of the court\b.*|(?:specially\s+)?(?:concurring|dissenting)[.:,]?|opinion[,:])$").unwrap(), "author"),
        (Regex::new(r"(?i)^(?:appeals? from|on appeal from|on petition for (?:review|writ)|petition for review|trial court\b|lower tribunal\b)").unwrap(), "lower_court"),
        (Regex::new(r"(?i)^(?:re|subject)\s*:").unwrap(), "subject"),
        (Regex::new(r"(?i)^dear\s").unwrap(), "addressee"),
        (Regex::new(r"(?i)\b(?:court of appeals?|supreme court|district court|bankruptcy court|court of|tribunal|circuit)\b").unwrap(), "court"),
        (Regex::new(r"(?i)^(?:(?:case|docket|civil action|cause|appeal|criminal|civil|bankruptcy|adversary|adv\.?|index|c/a|cv|cr|opinion)\s*(?:nos?\.?|numbers?|action)|nos?\.\s*\S*\d)").unwrap(), "docket"),
        (Regex::new(r"(?i)^(?:before|panel|coram|present)\b|\b(?:circuit|district|chief|magistrate|senior) judges?\b").unwrap(), "judges"),
        (Regex::new(r"(?i)^(?:(?:filed|decided|argued|submitted|dated|entered|heard|delivered|rehearing|date of|opinion filed|date filed)\b.*\d{4}|(?:january|february|march|april|may|june|july|august|september|october|november|december)\s+\d{1,2},\s+\d{4}\.?$)").unwrap(), "date"),
        (Regex::new(r"(?i)\b(?:for (?:the )?(?:plaintiff|defendant|appell|petitioner|respondent)|attorneys? for|counsel for|on (?:the )?brief)").unwrap(), "counsel"),
        (Regex::new(r"(?i)\b(?:plaintiffs?|defendants?|appell(?:ant|ee)s?|petitioners?|respondents?)\b|^v\.?$|^vs\.?$").unwrap(), "caption"),
    ]).iter().find(|(pattern, _)| pattern.is_match(body)).map(|(_, role)| *role)
}

/// Head matter a court's own page marks by class or id: the older federal `p.case_cite`, `p.parties`, `p.docket`,
/// `p.date`, `p.court`, and what its `div.prelims` holds; Colorado's `p#Judge`, `p#Attorney`.
pub(super) fn classed_front(structure: &DocumentStructure, coordinates: &ScalarText) -> Vec<FrontLine> {
    static COUNSEL: OnceLock<Regex> = OnceLock::new();
    let counsel = COUNSEL.get_or_init(|| Regex::new(r"(?i)\battorneys?\b|\bcounsel\b|\bfor (?:the )?(?:plaintiff|defendant|appell|petitioner|respondent)").unwrap());
    let prelims = structure.markup_elements.iter().filter(|element| element.attributes.get("class").is_some_and(|class| class == "prelims")).map(|element| element.range).collect::<Vec<_>>();
    structure.markup_elements.iter().filter(|element| element.tag == "p").filter_map(|element| {
        let printed = coordinates.slice_utf16(element.range.start..element.range.end).map(squeezed).filter(|text| !text.is_empty())?;
        let class = element.attributes.get("class").map_or("", String::as_str);
        let id = element.attributes.get("id").map_or("", String::as_str);
        let role = match (class, id) {
            ("case_cite", _) => "citation",
            ("parties", _) => "caption",
            ("docket", _) => "docket",
            ("date", _) => "date",
            ("court", _) => "court",
            (_, "Judge" | "MajorityJudge") => "author",
            (_, "Attorney") if counsel.is_match(&printed) => "counsel",
            _ if prelims.iter().any(|range| range.start <= element.range.start && element.range.end <= range.end) => match front_role(&printed) {
                Some(role) => role,
                None if class == "center" => "caption",
                None => return None,
            },
            _ => return None,
        };
        Some(FrontLine { role: role.to_owned(), text: printed, range: Some(element.range) })
    }).collect()
}

/// The blocks ahead of the opinion's first sentence are its head: a court, a docket, counsel, a date, a title. A block reads as
/// a sentence if it is long, or its words run lower-case; a line that says what it is keeps that role, and a court or a party
/// named in running words is the opinion speaking.
pub(super) fn head_lines(layer: &mut OpinionLayer) {
    const SENTENCE_WORDS: usize = 14;
    // How far down a byline can stand and still close a head.
    const HEAD_BLOCKS: usize = 14;
    const HEAD_WORDS: usize = 300;
    let coordinates = ScalarText::new(&layer.text);
    static TITLE: OnceLock<Regex> = OnceLock::new();
    let title = TITLE.get_or_init(|| Regex::new(r"(?i)^(?:entry )?(?:opinion|memorandum|order)\b.{0,30}$").unwrap());
    static NOTICE: OnceLock<Regex> = OnceLock::new();
    let notice = NOTICE.get_or_init(|| Regex::new(r"(?i)^(?:(?:this (?:opinion|decision|order)|the slip opinion|notice:?)\b.*\b(?:revision|precedent|publication|published|version|binding)\b|published by\b)").unwrap());
    let section = |heading: &OpinionHeading| enumerator_end(&heading.text).is_some() || title_line(&heading.text) || heading.text.starts_with(|c: char| c.is_lowercase());
    let first_heading = layer.headings.iter().filter(|heading| section(heading) && !title.is_match(&heading.text)).map(|heading| heading.start).min().unwrap_or(usize::MAX);
    static FOR_PARTY: OnceLock<Regex> = OnceLock::new();
    let for_party = FOR_PARTY.get_or_init(|| Regex::new(r"(?i)\bfor (?:the )?[\p{L}-]+(?: [\p{L}-]+){0,3}[.,]?$").unwrap());
    // A byline near the top closes the head, whatever stands above it: a court's summary, counsel run into one block.
    let mut above = 0;
    let byline = layer.paragraphs.iter().take(HEAD_BLOCKS).position(|paragraph| {
        let printed = coordinates.slice_utf16(paragraph.start..paragraph.end).map(squeezed).unwrap_or_default();
        above += printed.split_whitespace().count();
        let lower = printed.to_lowercase();
        (printed.len() <= 60 || lower.contains("delivered the opinion")) && front_role(&printed) == Some("author") && !lower.contains("concur") && !lower.contains("dissent")
    }).filter(|at| *at > 0 && above <= HEAD_WORDS && layer.paragraphs[..*at].iter().all(|paragraph| {
        let printed = coordinates.slice_utf16(paragraph.start..paragraph.end).map_or("", trim_js);
        paragraph.number.is_none() && !printed.starts_with("We ") && !printed.starts_with("I ")
    }));
    let mut lines = Vec::new();
    for (index, paragraph) in layer.paragraphs.iter().enumerate().take_while(|(_, paragraph)| paragraph.number.is_none() && (!paragraph.quote || layer.rendition == "plain_text.txt") && paragraph.start < first_heading) {
        let printed = coordinates.slice_utf16(paragraph.start..paragraph.end).map(squeezed).unwrap_or_default();
        if byline.is_some_and(|at| index <= at) {
            lines.push(FrontLine { role: front_role(&printed).unwrap_or("cover").to_owned(), text: printed, range: Some(ScalarRange { start: paragraph.start, end: paragraph.end }) });
            continue;
        }
        let next = layer.paragraphs.get(index + 1).and_then(|next| coordinates.slice_utf16(next.start..next.end)).map_or("", trim_js);
        if next.starts_with(char::is_lowercase) {
            break;
        }
        let titled = title_cased(&printed) || !printed.chars().any(char::is_lowercase);
        let published = notice.is_match(&printed);
        let role = front_role(&printed).filter(|role| titled || !matches!(*role, "court" | "caption")).or(published.then_some("publication"));
        let long = printed.split_whitespace().count() >= SENTENCE_WORDS;
        let counsel = role == Some("counsel") && for_party.is_match(&printed);
        if (long && !counsel && !published && !(titled && role.is_some())) || (role.is_none() && !titled) {
            break;
        }
        lines.push(FrontLine { role: role.unwrap_or("cover").to_owned(), text: printed, range: Some(ScalarRange { start: paragraph.start, end: paragraph.end }) });
    }
    // A record of nothing else has no head: those lines are its text.
    if lines.len() < layer.paragraphs.len() {
        layer.paragraphs.drain(..lines.len());
        let none_taken = lines.is_empty();
        let head_end = layer.paragraphs.first().map_or(0, |first| first.start);
        layer.front.extend(lines);
        let (head, headings): (Vec<_>, Vec<_>) = std::mem::take(&mut layer.headings).into_iter().partition(|heading| heading.start < head_end && !section(heading) && !title.is_match(&heading.text) && (!none_taken || front_role(&heading.text).is_some() || heading.text.contains(|c: char| c.is_ascii_digit())));
        layer.headings = headings;
        layer.front.extend(head.into_iter().map(|heading| FrontLine {
            role: front_role(&heading.text).unwrap_or("cover").to_owned(), range: Some(ScalarRange { start: heading.start, end: heading.end }), text: heading.text,
        }));
    }
}

/// What the reporter added among the paragraphs: "NOTE.—", "Held:", a bracketed statement of the case over the opinion;
/// and the opinion's last words, its disposition and signing.
pub(super) fn reporter_matter(layer: &mut OpinionLayer) {
    static NOTE: OnceLock<Regex> = OnceLock::new();
    static HELD: OnceLock<Regex> = OnceLock::new();
    static SIGNED: OnceLock<Regex> = OnceLock::new();
    static JOINED: OnceLock<Regex> = OnceLock::new();
    let note = NOTE.get_or_init(|| Regex::new(r"^\[Reported by\b|^\[?(?:NOTE|Note|EDITORS?['’]?S? ?NOTE|Editors?['’]?s? ?[Nn]ote|Reporter['’]s [Nn]ote)\b\s?(?:[.:—–\]-]|$)").unwrap());
    let joined = JOINED.get_or_init(|| Regex::new(r"(?i)\b(?:concurs?|concurred|concurring|dissents?|dissenting|joins?|joined)\b[^.]{0,60}\.$").unwrap());
    let held = HELD.get_or_init(|| Regex::new(r"^(?:¶\s*\d+\s+)?Held(?:, further)?\s*[:,.(]").unwrap());
    let signed = SIGNED.get_or_init(|| Regex::new(r"(?i)^dated?\b|^hon\.|^/s/|,\s*j\.s\.c\.?$|^from\s+\p{Lu}.*,\s*\p{Lu}[a-z]{1,3}\.$|^[-*_ ]*end of (?:document|opinion)|digitally signed|^by the court:?$|^(?:chief |deputy )?clerk\b|,\s*(?:chief |associate )?(?:justice|judge|clerk)$|^\S.{0,50},\s*(?:u\.s\. )?district judge\.?$").unwrap());
    static PAPERS: OnceLock<Regex> = OnceLock::new();
    static STATUS: OnceLock<Regex> = OnceLock::new();
    static AFTERWARD: OnceLock<Regex> = OnceLock::new();
    // What became of this case, on a line of its own at the end; "cert. denied" after a cited case is the citation's.
    let afterward = AFTERWARD.get_or_init(|| Regex::new(r"(?i)^\[?(?:(?:petition for |application for |motion for |writ of )?(?:review|certiorari|cert\.?|rehearing|appeal|error|leave to appeal)\b.{0,80}\b(?:denied|granted|dismissed|refused|allowed|overruled|affirmed)\b|this cause was taken to|upon an appeal to|affirmed (?:by|on appeal)|reversed (?:by|on appeal))").unwrap());
    // A digest's running topic: "Same—Burden of Proof."
    static TOPIC: OnceLock<Regex> = OnceLock::new();
    let topic = TOPIC.get_or_init(|| Regex::new(r"^Same\s?[—–]\s?\p{Lu}").unwrap());
    let papers = PAPERS.get_or_init(|| Regex::new(r"(?i)^(?:the following (?:papers|e-?filed documents)|papers (?:considered|numbered|read))").unwrap());
    // What a court says of an opinion's standing, at either end of it: elsewhere the words are a citation's.
    let status = STATUS.get_or_init(|| Regex::new(r"(?im)^.*(?-u:\b)(?:not (?:designated )?for (?:official )?publication|not to be published|do not publish|unpublished (?:opinion|decision|disposition|order)|non-?precedential|subject to (?:formal )?revision|certified for (?:partial )?publication|not recommended for (?:full[- ]text )?publication).*$").unwrap());
    let boundary = |mut at: usize| { while !layer.text.is_char_boundary(at) { at -= 1; } at };
    let (head, foot) = (boundary(3000.min(layer.text.len())), boundary(layer.text.len().saturating_sub(1500)).max(boundary(3000.min(layer.text.len()))));
    let at_ends = status.find_iter(&layer.text[..head]).map(|found| (found.start(), found.as_str()))
        .chain(status.find_iter(&layer.text[foot..]).map(|found| (foot + found.start(), found.as_str())));
    let standing = at_ends.filter(|found| found.1.len() <= 200).map(|found| (utf16_len(&layer.text[..found.0]), found.1)).collect::<Vec<_>>();
    let standing = standing.into_iter().map(|(start, line)| {
        let lead = line.len() - line.trim_start().len();
        let words = line.trim();
        let start = start + utf16_len(&line[..lead]);
        FrontLine { role: "publication".to_owned(), text: squeezed(words), range: Some(ScalarRange { start, end: start + utf16_len(words) }) }
    }).collect::<Vec<_>>();
    layer.front.extend(standing);
    let coordinates = ScalarText::new(&layer.text);
    let count = layer.paragraphs.len();
    let mut kept = Vec::with_capacity(count);
    for (index, paragraph) in std::mem::take(&mut layer.paragraphs).into_iter().enumerate() {
        let printed = coordinates.slice_utf16(paragraph.start..paragraph.end).map(squeezed).unwrap_or_default();
        let range = Some(ScalarRange { start: paragraph.start, end: paragraph.end });
        if index + 8 >= count && printed.len() <= 120 {
            let role = if signed.is_match(&printed) || legal_structure::judgment::signature_line(&printed) {
                Some("closing")
            } else if joined.is_match(&printed) {
                Some("joinder")
            } else {
                None
            };
            if let Some(role) = role {
                layer.front.push(FrontLine { role: role.to_owned(), text: printed.clone(), range });
                // A numbered paragraph is the opinion's, whatever it says; so is a record's only text.
                if paragraph.number.is_none() && !kept.is_empty() {
                    continue;
                }
            }
        }
        if printed.len() <= 80 && front_role(&printed) == Some("author") {
            layer.front.push(FrontLine { role: "author".to_owned(), text: printed.clone(), range });
        }
        // An Appellate Division memorandum opens on its disposition; an Attorney General's opinion on its digest.
        if index < 2 && printed.starts_with("Memorandum.") {
            layer.front.push(FrontLine { role: "disposition".to_owned(), text: printed.clone(), range });
        }
        if index < 2 && printed.starts_with("OPINION") && printed.contains("AG —") {
            layer.headnotes.push(headnote_of("summary", printed.clone(), range));
        }
        if note.is_match(&printed) {
            layer.headnotes.push(headnote_of("note", printed, range));
            continue;
        }
        if index + 8 >= count && printed.len() <= 240 && afterward.is_match(&printed) {
            layer.headnotes.push(headnote_of("history", printed, range));
            continue;
        }
        if index == 0 && printed.len() > 60 && printed.starts_with('[') && printed.ends_with(']') {
            layer.headnotes.push(headnote_of("summary", printed, range));
            continue;
        }
        if topic.is_match(&printed) {
            layer.headnotes.push(headnote_of("digest", printed, range));
            continue;
        }
        if held.is_match(&printed) {
            layer.headnotes.push(headnote_of("held", printed, range));
        } else if papers.is_match(&printed) {
            layer.headnotes.push(headnote_of("papers", printed, range));
        }
        kept.push(paragraph);
    }
    layer.paragraphs = kept;
    let (editorial, notes): (Vec<_>, Vec<_>) = std::mem::take(&mut layer.notes).into_iter().partition(|called| {
        called.references.is_empty() && !called.called_outside && coordinates.slice_utf16(called.start..called.end).is_some_and(|body| note.is_match(trim_js(body)))
    });
    layer.notes = notes;
    for called in editorial {
        let printed = coordinates.slice_utf16(called.start..called.end).map(squeezed).unwrap_or_default();
        layer.headnotes.push(headnote_of("note", printed, Some(ScalarRange { start: called.start, end: called.end })));
    }
    // Capitals past the last text are an index's subjects, no heading.
    let last_text = layer.paragraphs.iter().map(|paragraph| paragraph.end).max().unwrap_or(0);
    let (trailing, headings): (Vec<_>, Vec<_>) = std::mem::take(&mut layer.headings).into_iter().partition(|heading| {
        heading.start >= last_text && !heading.text.chars().any(char::is_lowercase) && !section_keyword(&heading.text) && !notes_title(&heading.text)
            && !["OPINION", "REHEARING", "APPENDIX"].iter().any(|word| heading.text.contains(word))
    });
    layer.headings = headings;
    layer.headnotes.extend(trailing.into_iter().map(|heading| {
        let range = Some(ScalarRange { start: heading.start, end: heading.end });
        headnote_of("topics", heading.text, range)
    }));
}

/// A text's sentences, as ranges of bytes. A stop ends one before a capital or the end, unless it closes an abbreviation
/// or a number: "I.R.S.", "Mr.", "Doc. 6", "$11,000.00".
pub(super) fn sentences(raw: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut start = 0;
    for (at, stop) in raw.char_indices().filter(|(_, c)| matches!(c, '.' | '!' | '?' | ';')) {
        let after = raw[at + stop.len_utf8()..].trim_start_matches(['"', '”', '’', ')', ']']);
        let next = after.trim_start();
        let spaced = after.len() != next.len() || next.is_empty();
        let word = raw[..at].rsplit(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
        let short = stop == '.' && word.len() <= 3 && (word.starts_with(char::is_uppercase) || word.is_empty() || word.bytes().all(|byte| byte.is_ascii_digit()));
        if stop == ';' || spaced && !short && (next.is_empty() || next.starts_with(|c: char| c.is_uppercase() || c == '“' || c == '"')) {
            let end = at + stop.len_utf8();
            found.push((start, end));
            start = end;
        }
    }
    if raw[start..].chars().any(char::is_alphanumeric) {
        found.push((start, raw.len()));
    }
    found
}

/// What the court did, read back from the end of each of its opinions to the block that says so, and through the blocks
/// just before it; and the acts those sentences order. A dissent or concurrence closes on its own view, not on the order.
/// `closing` are the blocks past the paragraphs that may carry it: a title in capitals, a line of the foot.
pub(super) fn dispositions(text: &str, paragraphs: &[OpinionParagraph], parts: &[OpinionPart], closing: &[ScalarRange]) -> (Vec<FrontLine>, Vec<String>) {
    static FIRM: OnceLock<Regex> = OnceLock::new();
    let firm = FIRM.get_or_init(|| Regex::new(r"(?i)^(?:we|i)\b|\b(?:hereby|will be|ordered|adjudged|decreed)\b").unwrap());
    let coordinates = ScalarText::new(text);
    let (mut lines, mut outcomes) = (Vec::new(), Vec::<String>::new());
    let read = |range: ScalarRange| {
        let Some(raw) = coordinates.slice_utf16(range.start..range.end) else { return Vec::new() };
        sentences(raw).into_iter().filter_map(|(from, to)| {
            let words = trim_js(&raw[from..to]);
            let acts = legal_structure::judgment::court_order(words)?;
            let start = range.start + utf16_len(&raw[..from + raw[from..to].find(words).unwrap_or(0)]);
            Some((FrontLine { role: "disposition".to_owned(), text: squeezed(words), range: Some(ScalarRange { start, end: start + utf16_len(words) }) }, acts))
        }).collect::<Vec<_>>()
    };
    for part in parts.iter().filter(|part| !(part.kind.contains("dissent") || part.kind.contains("concur"))) {
        let mut blocks = paragraphs.iter().filter(|paragraph| part.start <= paragraph.start && paragraph.start < part.end)
            .map(|paragraph| (ScalarRange { start: paragraph.start, end: paragraph.end }, paragraph.quote))
            .chain(closing.iter().copied().filter(|range| part.start <= range.start && range.start < part.end).map(|range| (range, false))).collect::<Vec<_>>();
        blocks.sort_by_key(|block| block.0.start);
        let count = blocks.len();
        let mut found = Vec::new();
        // An opinion opens on what is asked and closes on what is done: read early, an order must say so in firm words. A
        // quotation orders nothing unless it is the close, which a signature may follow.
        for (back, (block, quote)) in blocks.into_iter().rev().enumerate() {
            if quote && (back > 2 || !found.is_empty()) {
                continue;
            }
            let mut said = read(block);
            if back >= count.div_ceil(2) {
                said.retain(|(line, _)| firm.is_match(&line.text) || line.text.split_whitespace().count() <= 4);
            }
            // "An appropriate order will be entered." names nothing: what it orders stands further up.
            let named = found.iter().any(|(_, acts): &(FrontLine, Vec<&str>)| acts.iter().any(|act| !matches!(*act, ORDERED | JUDGMENT)));
            if said.is_empty() && named {
                break;
            }
            if !found.is_empty() && !named && said.iter().all(|(_, acts)| acts.iter().all(|act| matches!(*act, ORDERED | JUDGMENT))) {
                continue;
            }
            found.splice(0..0, said);
        }
        for (line, acts) in found {
            lines.push(line);
            for act in acts {
                if !outcomes.iter().any(|known| known == act) {
                    outcomes.push(act.to_owned());
                }
            }
        }
    }
    (lines, outcomes)
}

/// Dates as their lines name them: argued, submitted, decided, rehearing.
pub(super) fn name_dates(front: &mut [FrontLine]) {
    static KINDS: OnceLock<[(Regex, &'static str); 5]> = OnceLock::new();
    let kinds = KINDS.get_or_init(|| [
        (Regex::new(r"(?i)^rehearing|\brehearing (?:denied|granted)").unwrap(), "date_rehearing"),
        (Regex::new(r"(?i)^(?:argued|heard|oral argument)").unwrap(), "date_argued"),
        (Regex::new(r"(?i)^submitted").unwrap(), "date_submitted"),
        (Regex::new(r"(?i)^(?:decided|date of decision|delivered|opinion filed)").unwrap(), "date_decided"),
        (Regex::new(r"(?i)^(?:filed|date filed|entered|dated)").unwrap(), "date_filed_line"),
    ]);
    for line in front.iter_mut().filter(|line| line.role == "date") {
        if let Some((_, kind)) = kinds.iter().find(|(pattern, _)| pattern.is_match(&line.text)) {
            line.role = (*kind).to_owned();
        }
    }
}

/// A cover's summary over a text: what a board's decision prints between "Decided by Board" and the charge, and what an
/// Office of Legal Counsel opinion prints over its memorandum.
pub(super) fn cover_matter(layer: &mut OpinionLayer) {
    static BOARD: OnceLock<Regex> = OnceLock::new();
    static CHARGE: OnceLock<Regex> = OnceLock::new();
    static MEMORANDUM: OnceLock<Regex> = OnceLock::new();
    if layer.rendition != "plain_text.txt" {
        return;
    }
    let units = |byte: usize| utf16_len(&layer.text[..byte]);
    let board = BOARD.get_or_init(|| Regex::new(r"(?m)^[ \t]*Decided by Board\b.*$").unwrap()).find(&layer.text).map(|found| found.end());
    let from_board = board.and_then(|from| {
        let until = CHARGE.get_or_init(|| Regex::new(r"(?m)^[ \t]*(?:CHARGE|Charge|ORDER|ON BEHALF OF)\b").unwrap()).find(&layer.text[from..])?;
        Some((units(from), units(from + until.start()), "syllabus"))
    });
    let window = layer.text.len() * 3 / 20;
    let memorandum = MEMORANDUM.get_or_init(|| Regex::new(r"(?m)^[ \t]*(?:MEMORANDUM OPINION FOR|MEMORANDUM FOR|Dear )").unwrap()).find(&layer.text)
        .filter(|found| found.start() <= window).map(|found| (0, units(found.start()), "summary"));
    let Some((from, until, kind)) = from_board.or(memorandum) else { return };
    let coordinates = ScalarText::new(&layer.text);
    let (taken, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut layer.paragraphs).into_iter().partition(|paragraph| from <= paragraph.start && paragraph.start < until);
    layer.paragraphs = rest;
    let entries = taken.into_iter().map(|paragraph| {
        let printed = coordinates.slice_utf16(paragraph.start..paragraph.end).map(squeezed).unwrap_or_default();
        headnote_of(kind, printed, Some(ScalarRange { start: paragraph.start, end: paragraph.end }))
    }).collect::<Vec<_>>();
    layer.headnotes.extend(entries);
}

/// The topic index New Mexico's texts end on: "Topic Index for ...", a code and its subject to the line.
pub(super) fn topic_index(plain: &str) -> Vec<OpinionHeadnote> {
    static ENTRY: OnceLock<Regex> = OnceLock::new();
    let Some(from) = plain.find("Topic Index for") else { return Vec::new() };
    let entry = ENTRY.get_or_init(|| Regex::new(r"(?m)^[ \t]*([A-Z]{2}(?:-[A-Z]{2,4})?)[ \t]{2,}(\S.*?)[ \t]*$").unwrap());
    entry.captures_iter(&plain[from..]).map(|capture| OpinionHeadnote {
        kind: "topic_index".to_owned(), number: Some(capture[1].to_owned()), topic: None, text: squeezed(&capture[2]), range: None,
    }).collect()
}

/// A Columbia note's list of the case's topics: "Headnotes 1. Juries, 35 C.J., Section 240; 2. Juries, ...".
pub(super) fn embedded_topics(markup: &str) -> Vec<OpinionHeadnote> {
    static LIST: OnceLock<Regex> = OnceLock::new();
    static ITEM: OnceLock<Regex> = OnceLock::new();
    let Some(list) = LIST.get_or_init(|| Regex::new(r"Headnotes\s+1\.\s[^<]{10,}").unwrap()).find(markup) else { return Vec::new() };
    let items = ITEM.get_or_init(|| Regex::new(r"(?:^Headnotes\s+|;\s*)(\d{1,3})\.\s+").unwrap());
    let starts = items.captures_iter(list.as_str()).map(|capture| (capture.get(0).unwrap().start(), capture.get(0).unwrap().end(), capture[1].to_owned())).collect::<Vec<_>>();
    starts.iter().enumerate().map(|(index, (_, from, number))| {
        let to = starts.get(index + 1).map_or(list.as_str().len(), |next| next.0);
        OpinionHeadnote { kind: "topics".to_owned(), number: Some(number.clone()), topic: None, text: squeezed(&list.as_str()[*from..to]), range: None }
    }).collect()
}

/// A syllabus or headnotes the source does not mark: the paragraphs under a heading of that name.
pub(super) fn divisions_of_headnotes(layer: &mut OpinionLayer) {
    static NAME: OnceLock<Regex> = OnceLock::new();
    let name = NAME.get_or_init(|| Regex::new(r"(?i)^(?:syllabus(?: by the court)?|headnotes?|digest|note)\.?$").unwrap());
    let coordinates = ScalarText::new(&layer.text);
    for at in 0..layer.headings.len() {
        let heading = &layer.headings[at];
        let Some(printed) = coordinates.slice_utf16(heading.start..heading.end) else { continue };
        if !name.is_match(trim_js(printed)) { continue; }
        let kind = if trim_js(printed).to_lowercase().starts_with("note") { "note" } else { "syllabus" };
        let until = layer.headings.iter().map(|other| other.start).filter(|start| *start > heading.end).min().unwrap_or(usize::MAX);
        let start = heading.end;
        static OPENS: OnceLock<Regex> = OnceLock::new();
        static AUTHOR: OnceLock<Regex> = OnceLock::new();
        let opens = OPENS.get_or_init(|| Regex::new(r"(?i)^(?:per curiam\b|the opinion of the court|appeal from\b|this is an?\b|this (?:matter|case|appeal|cause)\b|on appeal\b)").unwrap());
        let author = AUTHOR.get_or_init(|| Regex::new(r"^\p{Lu}{2,}[\p{L}'’. -]{0,30}, (?:C\. ?)?J\.?[.:]").unwrap());
        // ... and a reader of the opinion below it must not lose the opinion.
        let until = layer.paragraphs.iter().filter(|paragraph| paragraph.start > start && paragraph.start < until).find(|paragraph| {
            let printed = coordinates.slice_utf16(paragraph.start..paragraph.end).map(trim_js).unwrap_or("");
            opens.is_match(printed) || author.is_match(printed) || legal_structure::judgment::author_line(printed)
        }).map_or(until, |paragraph| paragraph.start);
        let (taken, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut layer.paragraphs).into_iter().partition(|paragraph| paragraph.start >= start && paragraph.start < until);
        layer.paragraphs = rest;
        layer.headnotes.extend(taken.into_iter().map(|paragraph| {
            let printed = coordinates.slice_utf16(paragraph.start..paragraph.end).map(squeezed).unwrap_or_default();
            headnote_of(kind, printed, Some(ScalarRange { start: paragraph.start, end: paragraph.end }))
        }));
    }
}

/// Whether a rendition sets such a title alone, on a line or in a tag of its own: most do not, and reading one is dear.
pub(super) fn names_a_digest(text: &str) -> bool {
    static NAME: OnceLock<Regex> = OnceLock::new();
    NAME.get_or_init(|| Regex::new(r#"(?im)(?:^|>)[ \t*_"]*(?-u:(?:syllabus(?: by the court)?|headnotes?|summary|synopsis|digest|held))[ \t.:*_"]*(?:$|<)"#).unwrap()).is_match(text)
}

/// What a reporter sets over the opinion under a title of its own, in any rendition's words: "Syllabus by the Court", "Synopsis",
/// "Held:". It runs to the opinion's first line; with none in sight it is a section of the opinion, not a digest. A title far
/// into the text is the opinion's too.
pub(super) fn titled_digest(text: &str) -> Option<(&'static str, String)> {
    const HEAD_REACH: usize = 3000;
    const LEAST_WORDS: usize = 8;
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static OPENS: OnceLock<Regex> = OnceLock::new();
    let title = TITLE.get_or_init(|| Regex::new(r#"(?im)^[\s*_"]*(syllabus(?: by the court)?|headnotes?|summary|synopsis|digest|held)[\s.:*_"]*$"#).unwrap());
    let opens = OPENS.get_or_init(|| Regex::new(r"(?im)^\W*(?:opinion\b|per curiam\b|mr\.? (?:chief )?justice\b|.{0,60}delivered the opinion|.{2,60}, (?:c\. ?)?j\.[:.]?\s*$|.{2,60}, (?:chief )?justice:?\s*$|(?:chief )?justice \S.{0,30}:\s*$|this is an?\b|on appeal\b|appeal from\b)").unwrap());
    let found = title.captures(text).filter(|capture| capture.get(0).is_some_and(|whole| whole.start() <= HEAD_REACH))?;
    let rest = &text[found.get(0)?.end()..];
    let opening = opens.find(rest)?.start();
    let body = squeezed(&rest[..opening]);
    let words = body.split_whitespace().count();
    if words < LEAST_WORDS || words >= rest[opening..].split_whitespace().count() {
        return None;
    }
    let kind = match found[1].to_lowercase().split_whitespace().next()? {
        "syllabus" => "syllabus",
        "held" => "held",
        "digest" => "digest",
        "headnote" | "headnotes" => "headnotes",
        _ => "summary",
    };
    Some((kind, body))
}

/// Bracketed headnote numbers opening a paragraph, which no count of paragraphs took: Lawbox's "[1, 2]".
pub(super) fn printed_headnote_marks(layer: &mut OpinionLayer) {
    static MARK: OnceLock<Regex> = OnceLock::new();
    let mark = MARK.get_or_init(|| Regex::new(r"^\[(\d{1,3}(?:\s*[-–,]\s*\d{1,3})*)\]").unwrap());
    let coordinates = ScalarText::new(&layer.text);
    let marks = layer.paragraphs.iter().filter(|paragraph| paragraph.number.is_none()).filter_map(|paragraph| {
        let raw = coordinates.slice_utf16(paragraph.start..paragraph.end)?;
        let printed = raw.trim_start_matches(javascript_whitespace);
        let found = mark.find(printed)?;
        let start = paragraph.start + utf16_len(&raw[..raw.len() - printed.len()]);
        Some(HeadnoteMark { numbers: headnote_numbers(found.as_str())?, start, end: start + utf16_len(found.as_str()), page: None })
    }).collect::<Vec<_>>();
    if marks.len() >= 2 {
        layer.headnote_marks.extend(marks);
    }
}

/// What the record's own words show is not here: another judge's writing, the facts it stands on, an image, a later ruling.
pub(super) fn record_gaps(layer: &OpinionLayer) -> Vec<RecordGap> {
    static CHECKS: OnceLock<[(Regex, &'static str); 4]> = OnceLock::new();
    static SEPARATE: OnceLock<Regex> = OnceLock::new();
    let checks = CHECKS.get_or_init(|| [
        (Regex::new(r"(?i)\(after stating (?:the )?(?:foregoing )?facts[^)]{0,30}\)").unwrap(), "facts-not-here"),
        (Regex::new(r"\[\[Image here\]\]").unwrap(), "image-not-here"),
        (Regex::new(r"(?i)(?-u:\b)a separate (?:memorandum|opinion|order) will (?:address|follow|issue)|(?-u:\b)interlocutory pending full adjudication(?-u:\b)|(?-u:\b)(?:final order|judgment) will (?:be entered|follow) (?:after|upon)(?-u:\b)").unwrap(), "later-ruling-to-come"),
        (Regex::new(r"(?i)(?-u:\b)attached (?:opinion|findings)(?-u:\b)").unwrap(), "attachment-named"),
    ]);
    let separate = SEPARATE.get_or_init(|| Regex::new(r"(?i)(?-u:\b)(?:dissents?|concurs?),? with (?:written )?(?:opinion|writing)(?-u:\b)|(?-u:\b)(?:dissenting|concurring) opinion (?:to follow|filed)(?-u:\b)|(?-u:\b)filed a (?:dissenting|concurring) opinion(?-u:\b)").unwrap());
    let mut gaps = Vec::new();
    for (pattern, kind) in checks.iter() {
        if let Some(found) = pattern.find(&layer.text) {
            gaps.push(RecordGap { kind: (*kind).to_owned(), evidence: found.as_str().to_owned() });
        }
    }
    let separately_here = layer.parts.iter().any(|part| matches!(part.kind.as_str(), "dissent" | "concurrence") || part.kind.contains("dissent") || part.kind.contains("concurr"));
    if let Some(found) = separate.find(&layer.text).filter(|_| !separately_here) {
        gaps.push(RecordGap { kind: "separate-opinion-not-here".to_owned(), evidence: found.as_str().to_owned() });
    }
    // Text that opens on a second section has lost its first.
    let opening = layer.paragraphs.iter().map(|paragraph| paragraph.start).min().unwrap_or(usize::MAX);
    if let Some(first) = layer.headings.first().filter(|heading| heading.start < opening.saturating_add(40) && matches!(heading.text.trim_end_matches('.'), "II" | "B" | "2")) {
        gaps.push(RecordGap { kind: "opens-midway".to_owned(), evidence: first.text.clone() });
    }
    gaps
}

/// Whether the text is a court's at all: an Attorney General's letter is filed among the opinions.
pub(super) fn document_kind(text: &str) -> &'static str {
    static GENERAL: OnceLock<Regex> = OnceLock::new();
    let general = GENERAL.get_or_init(|| Regex::new(r"(?i)(?-u:\b)OPINION\s*—\s*AG(?-u:\b)|(?-u:\b)Attorney General(?-u:\b)[\s\S]{0,400}(?-u:\b)(?:Dear |in response to your|your (?:letter|request|inquiry)|you (?:have )?(?:asked|inquire))|(?-u:\b)(?:Dear |in response to your|your (?:letter|request) for an opinion)[\s\S]{0,400}(?-u:\b)Attorney General(?-u:\b)").unwrap());
    let head = text.char_indices().nth(1500).map_or(text.len(), |(at, _)| at);
    if general.is_match(&text[..head]) { "attorney-general-opinion" } else { "court-opinion" }
}
