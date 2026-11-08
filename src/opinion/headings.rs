//! What is a heading: titles by their words, set apart, run into a paragraph, or printed with a page.

use super::*;

/// A title in strong or underlined type at a paragraph's head, or in italics where it reads as one: where it ends.
pub(super) fn run_in(structure: &DocumentStructure, node: &StructureNode, range: ScalarRange, bare: &dyn Fn(ScalarRange) -> String) -> Option<usize> {
    if node.source != Derivation::Native {
        return None;
    }
    let (at, element) = element_of(structure, node)?;
    let child = structure.markup_elements[at + 1..].iter().take_while(|child| child.markup_start < element.markup_end)
        .find(|child| child.range.end > child.range.start && matches!(child.tag.as_str(), "strong" | "b" | "u" | "underline" | "center" | "em" | "i"))?;
    if child.range.start.saturating_sub(range.start) > 8 || child.range.end >= range.end {
        return None;
    }
    let title = bare(child.range);
    let title = trim_js(&title);
    let firm = !matches!(child.tag.as_str(), "em" | "i");
    let after = bare(ScalarRange { start: child.range.end, end: range.end });
    if trim_js(&after).starts_with(|c: char| c.is_lowercase() || matches!(c, ',' | ';')) {
        return None;
    }
    let lead = bare(ScalarRange { start: range.start, end: child.range.start });
    let enumerated = enumerator_end(trim_js(&lead)).is_some_and(|end| end == trim_js(&lead).len());
    let whole = format!("{} {title}", trim_js(&lead));
    (heading_line(&whole, true) && (firm || (title.ends_with(['.', ':']) && (enumerated || section_keyword(title))))).then_some(child.range.end)
}

/// Section titles run into a paragraph with no break: how far one runs from its start, and from its end.
pub(super) fn fused_titles(raw: &str) -> (Option<usize>, Option<usize>) {
    static LEAD: OnceLock<Regex> = OnceLock::new();
    static TAIL: OnceLock<Regex> = OnceLock::new();
    static DASHED: OnceLock<Regex> = OnceLock::new();
    let dashed = DASHED.get_or_init(|| Regex::new(r"^\s*(\p{Lu}[^.—\n]{2,80}\.)—\s?\p{Lu}").unwrap()).captures(raw).and_then(|capture| capture.get(1)).map(|title| utf16_len(&raw[..title.end()]));
    let lead = LEAD.get_or_init(|| Regex::new(r"^\s*([A-Z][A-Z ,&]{5,60}?)\s+[A-Z][a-z .]").unwrap()).captures(raw)
        .filter(|capture| section_word(&capture[1])).and_then(|capture| capture.get(1)).map(|title| utf16_len(&raw[..title.end()])).or(dashed);
    let tail = TAIL.get_or_init(|| Regex::new(r#"[.;:!?”"]\s+([A-Z][A-Z ,&]{5,60})\s*$"#).unwrap()).captures(raw)
        .filter(|capture| section_word(&capture[1])).and_then(|capture| capture.get(1)).map(|title| utf16_len(&raw[title.start()..]));
    (lead, tail)
}

/// The words of a section's title in a text that sets it no apart from a name or an address.
pub(super) fn section_keyword(body: &str) -> bool {
    static KEYWORD: OnceLock<Regex> = OnceLock::new();
    KEYWORD.get_or_init(|| Regex::new(r"(?i)\b(?:background|history|facts?|discussion|analysis|standards?|review|claims?|motions?|arguments?|issues?|jurisdiction|conclusions?|findings?|summary|procedure|proceedings?|evidence|law|liability|damages|fees|relief|statute|statutory|contract|negligence|immunity|appeal|order|judgment|remedy|applicable|legal|introduction)\b").unwrap()).is_match(body)
}

pub(super) fn section_word(body: &str) -> bool {
    static WORDS: OnceLock<Regex> = OnceLock::new();
    WORDS.get_or_init(|| Regex::new(r"(?i)^(?:(?:statement of )?(?:the )?(?:facts?|case)|background|factual (?:and procedural )?background|procedural (?:background|history)|(?:facts?|history) and procedure|facts and proceedings|discussion|analysis|conclusions?|introduction|opinion|order|memorandum(?: opinion| decision)?(?: and order)?|findings of fact(?: and conclusions of law)?|conclusions of law|standard of review|legal standards?|jurisdiction|argument|issues?(?: presented)?|questions? presented|holding|decision|disposition|summary|recommendation|appendix|judgment|decree|remedy|damages|law|applicable law|proceedings(?: below)?|prior proceedings|opinion of the court|memorandum of decision|reasons|order and judgment|syllabus(?: by the court)?|headnotes?|questions?|answers?|short answers?|brief answers?|errata|erratum|corrections?)$").unwrap()).is_match(body)
}

/// A text without the page mark a page's turn fused to its front: "*69 DISCUSSION".
pub(super) fn unfused(printed: &str) -> &str {
    static STAR: OnceLock<Regex> = OnceLock::new();
    STAR.get_or_init(|| Regex::new(r"^(?:\*+\s?\d{1,5}|\{\*+[^{}\n]{1,40}\})\s*").unwrap()).find(printed).filter(|found| found.end() < printed.len())
        .map_or(printed, |found| &printed[found.end()..])
}

/// Small capitals an extractor set apart: "O R D E R", "U N I T E D   S T A T E S".
pub(super) fn letter_spaced(printed: &str) -> Option<String> {
    static SPACED: OnceLock<Regex> = OnceLock::new();
    static GAP: OnceLock<Regex> = OnceLock::new();
    SPACED.get_or_init(|| Regex::new(r"^(?:\p{Lu}[.,]?\s+){3,}\p{Lu}[.,:]?$").unwrap()).is_match(printed).then(|| {
        GAP.get_or_init(|| Regex::new(r"\s{2,}").unwrap()).split(printed).map(|word| word.split_whitespace().collect::<String>()).collect::<Vec<_>>().join(" ")
    })
}

/// A title without the page a text's table of contents or an extractor left on it: "FINDINGS OF FACT ...-— Page 1".
pub(super) fn paged_title(printed: &str) -> &str {
    static PAGED: OnceLock<Regex> = OnceLock::new();
    PAGED.get_or_init(|| Regex::new(r"\s*[.\-—–]{2,}\s*Page \d+$").unwrap()).find(printed).map_or(printed, |found| &printed[..found.start()])
}

/// Where an outline's enumerator ends: "IV. ", "2.", "c) ", "(a) ".
pub(super) fn enumerator_end(body: &str) -> Option<usize> {
    static ENUMERATOR: OnceLock<Regex> = OnceLock::new();
    ENUMERATOR.get_or_init(|| Regex::new(r"^(?:[IVXLC]{1,6}|\d{1,2})[.)]\s*|^[A-Za-z][.)](?:\s+|$)|^[ivxl]{2,6}[.)]\s+|^\([A-Za-z0-9]{1,4}\)\s*").unwrap()).find(body).map(|found| found.end())
}

/// A section's line by its Roman numeral: "I.", "II. BACKGROUND", "III. Claims Against the State". A sentence opening on the
/// pronoun has no stop after it.
pub(super) fn section_line(body: &str) -> bool {
    static SECTION: OnceLock<Regex> = OnceLock::new();
    SECTION.get_or_init(|| Regex::new(r"^[IVX]{1,6}(?:[.)](?:\s+\S.*)?)?$").unwrap()).is_match(body.trim()) && heading_line(body, true)
}

/// Most of a phrase's longer words open on a capital.
pub(super) fn title_cased(phrase: &str) -> bool {
    let significant = phrase.split_whitespace().filter(|word| word.len() > 3 && word.starts_with(char::is_alphabetic)).collect::<Vec<_>>();
    let titled = significant.iter().filter(|word| word.starts_with(char::is_uppercase)).count();
    !significant.is_empty() && titled * 5 >= significant.len() * 4
}

/// A paragraph or line that is a heading by its words: short, no citation, no sentence; in capitals, an enumerator
/// with a title, a section's usual name, or an enumerator alone where the source sets it apart.
pub(super) fn heading_line(body: &str, set_apart: bool) -> bool {
    static CITATION: OnceLock<Regex> = OnceLock::new();
    static NOT_HEADING: OnceLock<Regex> = OnceLock::new();
    let body = paged_title(unfused(trim_js(body)));
    if body.matches('“').count() != body.matches('”').count() {
        return false;
    }
    if letter_spaced(body).is_some() {
        return true;
    }
    let length = body.chars().count();
    if length == 0 || length > 130 || body.chars().any(|c| c.is_control()) || text::contents_line(body) {
        return false;
    }
    if body.starts_with("e.g") || body.starts_with("i.e") || body.starts_with("Q.") {
        return false;
    }
    // "Ill" is III as a scanner reads it.
    if matches!(body.trim_end_matches('.'), "Ill" | "Il") {
        return true;
    }
    if body.chars().all(|c| matches!(c, 'I' | 'V' | 'X' | 'L' | 'C')) && body.len() <= 6 {
        return true;
    }
    let rest = &body[enumerator_end(body).unwrap_or(0)..];
    let numbered = rest.len() != body.len();
    if rest.is_empty() {
        return set_apart || body.len() <= 6 && body.starts_with(['I', 'V', 'X']);
    }
    let citation = CITATION.get_or_init(|| Regex::new(r"\bv\.?(?:\s|$)|\bvs\.?(?:\s|$)|\b\d+\s+[A-Z][A-Za-z.\d ]{0,14}\s+\d+|U\.\s?S\.\s?C|\bF\.\s?(?:2d|3d|4th|Supp)|https?:|\bat \d|\(\d{4}\)$|¶").unwrap());
    let not_heading = NOT_HEADING.get_or_init(|| Regex::new(r"(?i),\s*(?:c\.\s*)?j\.?j?\.?$|\b(?:district|circuit|bankruptcy|chief|presiding|associate|senior|magistrate|administrative law) judge\b|\b(?:justice|judge)[.,:]?$|^per curiam|concur|dissent|^so ordered|^it is (?:so |hereby |further )?ordered|^(?:affirmed|reversed|remanded|vacated|dismissed|denied|granted)\b|^judgment (?:is |for |affirmed|reversed)|\b(?:affirmed|reversed|remanded|vacated|dismissed|denied|granted|sustained|overruled|modified|ordered|affirming|reversing)\.?$|\bjudge advocate\b|\bmilitary judge[.,]?$|\blaw (?:firm|offices?)\b|,\s*(?:llc|inc|pa|pc)\.?$|^(?:\p{Lu}\.){2,4}$|\$\s?\d|\(\d{3}\)\s?\d{3}|\b\d{5}(?:-\d{4})?$|^\d+\s.*\b(?:street|avenue|boulevard|suite|floor)\b|^enter:?$|^as used in\b|^(?:court|county|judge|trial court)\s?:|^opposed\b|^opinion by\b|^ibid\b|\besq\.?$|\b(?:pllc|llp|p\.?c\.?)$|^page \d|^division \S+$|^the people\b|^dated?\b|^signed|^filed|^entered|^/s/|^by the court|^before\b|^ordered\b|attorneys? for|^for (?:the )?(?:plaintiff|defendant|appell|petitioner|respondent)|\b(?:plaintiffs?|defendants?|appell(?:ant|ee)s?|petitioners?|respondents?)[,.]?$|^(?:january|february|march|april|may|june|july|august|september|october|november|december)\s+\d|^\W*$|^mr\.|^mrs\.|^ms\.|^q\.|^a\.\s|\bsee\b|_|\bfor (?:the )?(?:appell|plaintiff|defendant|petitioner|respondent)|\battorney general\b|^(?:the )?hon(?:orable|\.)|\bcourt of (?:appeals|claims|common pleas|errors|criminal appeals)\b|\b(?:district|bankruptcy|supreme|superior|circuit|appellate) court\b$|^in the\b.*\bcourt\b|\bno\.\s*\S*\d|\bjavascript\b").unwrap());
    // A title set apart may cite its statute; a case's name is a citation anywhere.
    if citation.is_match(rest) && (!set_apart || rest.contains(" v. ")) || not_heading.is_match(rest) {
        return false;
    }
    // A table's row is mostly figures; a sentence that wrapped opens as sentences do.
    let tokens = rest.split_whitespace().collect::<Vec<_>>();
    let figures = tokens.iter().filter(|token| token.chars().any(|c| c.is_ascii_digit()) && !token.chars().any(char::is_alphabetic)).count();
    let measured = tokens.iter().any(|token| token.contains('%') || token.contains('$') || (token.contains('.') && token.chars().filter(char::is_ascii_digit).count() >= 2 && !token.chars().any(char::is_alphabetic) && !token.ends_with('.')));
    static OPENS_A_SENTENCE: OnceLock<Regex> = OnceLock::new();
    let sentence = OPENS_A_SENTENCE.get_or_init(|| Regex::new(r"^(?:Moreover|However|Thus|Therefore|Furthermore|Further|Accordingly|Nevertheless|Hence|In addition|The following|It is|IT IS|NOW,)\b|\[\[Image").unwrap());
    if (tokens.len() >= 3 && figures * 2 >= tokens.len()) || (measured && figures >= 2) || sentence.is_match(rest) {
        return false;
    }
    let stem = rest.trim_end_matches(['.', ':', ' ']);
    if section_word(stem) && !rest.starts_with(char::is_lowercase) {
        return true;
    }
    if rest.ends_with([',', ';']) || rest.starts_with(char::is_lowercase) {
        return false;
    }
    // A numbered paragraph's first line, a wrapped line, a name.
    static NAME: OnceLock<Regex> = OnceLock::new();
    let last = rest.split_whitespace().next_back().unwrap_or("").to_lowercase();
    let number = body.split(|c: char| !c.is_ascii_digit()).next().and_then(|digits| digits.parse::<u32>().ok());
    if number.is_some_and(|number| number > 20)
        || matches!(last.as_str(), "to" | "the" | "of" | "and" | "or" | "a" | "an" | "in" | "for" | "with" | "that" | "by" | "on" | "at" | "from" | "as" | "is")
        || (!numbered && NAME.get_or_init(|| Regex::new(r"^\p{Lu}[\p{L}'’-]+(?:\s+\p{Lu}\.)+(?:\s+\p{Lu}[\p{L}'’-]+)+(?:,?\s+(?:JR|SR|II|III)\.?)?$").unwrap()).is_match(rest)) {
        return false;
    }
    let letters = rest.chars().filter(|c| c.is_alphabetic()).count();
    let upper = rest.chars().filter(|c| c.is_uppercase()).count();
    let words = rest.split_whitespace().collect::<Vec<_>>();
    let capitals = letters >= 4 && upper * 10 >= letters * 9;
    if capitals {
        return !rest.ends_with('.') || numbered && words.len() <= 4 || words.len() == 1 || rest.starts_with("ON ") || section_keyword(rest);
    }
    // An enumerator over a few words is a title, whatever its capitals: "K. Other discharge issues", "1. Injunction.". A
    // digit's list runs longer than a letter's outline, a stop or a colon says it is a sentence, and a witness answers.
    let said = words.iter().find(|word| !matches!(word.trim_matches(['"', '“', ':', '.']), "A" | "Q")).map(|word| word.trim_matches(['"', '“', '.', ',']));
    let reply = matches!(said, Some("Yes" | "No" | "I" | "That" | "It" | "We" | "He" | "She" | "They" | "Okay" | "Right" | "Correct" | "Sure" | "Yeah" | "Uh-huh" | "Certainly"));
    let digit = body.starts_with(|c: char| c.is_ascii_digit());
    let stopped = rest.ends_with('.');
    if reply && stopped {
        return false;
    }
    let room = if stopped { 3 } else if digit { 7 } else { 12 };
    if numbered && !reply && !body.starts_with('(') && rest.starts_with(char::is_uppercase) && !rest.ends_with(['?', '!', ':']) && !(stopped && rest.contains(',')) && !rest.contains(". ")
        && words.len() <= room && !(digit && !stopped && words.len() < 2) {
        return true;
    }
    let short_title = stopped && words.len() <= 3 && !rest.contains([':', '“', '"']) && !rest.contains(|c: char| c.is_ascii_digit());
    if rest.ends_with(['.', '?', '!']) && !(set_apart && (numbered || short_title)) {
        return false;
    }
    // Set apart with no stop, a block is a title in sentence case as well: "The claim that Bank breached its duty of good faith".
    const PLAIN_TITLE: usize = 14;
    let title = title_cased(rest) || (set_apart && !rest.ends_with(['.', ':', '?', '!']) && (2..=PLAIN_TITLE).contains(&words.len()));
    (numbered && set_apart && !body.starts_with('(') && words.len() <= 10) || ((numbered || set_apart) && title && words.len() <= 16)
}

/// Sections numbered in the run of the paragraphs, title and text in one block with no markup: "II. The plea of estoppel is
/// not well founded. The fact is ...". Roman numerals running on from block to block say what they are; each title is its
/// block's first sentence.
pub(super) fn enumerated_titles(layer: &mut OpinionLayer) {
    const TITLE_WORDS: usize = 16;
    static OPENING: OnceLock<Regex> = OnceLock::new();
    let opening = OPENING.get_or_init(|| Regex::new(r"^\s*([IVXLC]{1,6})\.\s+\p{Lu}").unwrap());
    let coordinates = ScalarText::new(&layer.text);
    let mut run = Vec::new();
    let mut next = None;
    for (index, paragraph) in layer.paragraphs.iter().enumerate() {
        let Some(raw) = coordinates.slice_utf16(paragraph.range.start..paragraph.range.end) else { continue };
        let Some(number) = opening.captures(raw).and_then(|capture| page_number(&capture[1])) else { continue };
        if next.is_none_or(|next| next == number) {
            next = Some(number + 1);
            if let Some(&(_, end)) = sentences(raw).first().filter(|sentence| sentence.1 < raw.trim_end().len() && raw[..sentence.1].split_whitespace().count() <= TITLE_WORDS) {
                run.push((index, utf16_len(&raw[..end])));
            }
        }
    }
    if run.len() < 2 {
        return;
    }
    for (index, length) in run {
        let paragraph = &mut layer.paragraphs[index];
        layer.headings.push(read_heading(paragraph.range.start, paragraph.range.start + length, "run-in"));
        paragraph.range.start += length;
    }
}

/// Bracketed lines of one form with no text between them are a list's items: a title has a section under it.
pub(super) fn list_items(layer: &mut OpinionLayer) {
    // An outline numbers with a stop, "I." over "A."; a list with a bracket, "(a)", "d)", "6)".
    static ITEM: OnceLock<Regex> = OnceLock::new();
    let item = ITEM.get_or_init(|| Regex::new(r"^(\(?)([a-z]{1,4}|\d{1,2})\)").unwrap());
    let family = |text: &str| item.captures(text).map(|capture| (!capture[1].is_empty(), capture[2].starts_with(|c: char| c.is_ascii_digit())));
    let mut listed = vec![false; layer.headings.len()];
    for at in 1..layer.headings.len() {
        let (before, heading) = (&layer.headings[at - 1], &layer.headings[at]);
        let after = layer.paragraphs.partition_point(|paragraph| paragraph.range.start <= before.start);
        let adjacent = layer.paragraphs.get(after).is_none_or(|paragraph| paragraph.range.start >= heading.range.start);
        if adjacent && family(&before.text).is_some() && family(&before.text) == family(&heading.display_value.as_deref().unwrap_or("")) {
            listed[at - 1] = true;
            listed[at] = true;
        }
    }
    let mut index = 0;
    let mut items = Vec::new();
    layer.headings.retain(|heading| {
        index += 1;
        if listed[index - 1] {
            items.push(read_paragraph(heading.range.start, heading.range.end, None));
        }
        !listed[index - 1]
    });
    if !items.is_empty() {
        layer.paragraphs.append(&mut items);
        layer.paragraphs.sort_by_key(|paragraph| paragraph.range.start);
    }
}

/// The title of a list of notes.
pub(super) fn notes_title(body: &str) -> bool {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    TITLE.get_or_init(|| Regex::new(r"(?i)^(?:foot|end)?notes?:?$").unwrap()).is_match(body)
}

/// A case's name set as a heading: parties either side of a "v.".
pub(super) fn caption_heading(body: &str) -> bool {
    static CAPTION: OnceLock<Regex> = OnceLock::new();
    CAPTION.get_or_init(|| Regex::new(r"\bv\.?\s|\bvs\.?\s|^In re\b|^IN RE\b").unwrap()).is_match(body)
}

/// A title an opinion's head may set among its caption lines.
pub(super) fn title_line(body: &str) -> bool {
    static LEAD: OnceLock<Regex> = OnceLock::new();
    let lead = LEAD.get_or_init(|| Regex::new(r"^(?:(?:[IVXLC]{1,6}|[A-Z]|\d{1,2})[.)]\s+)?(?:(?:THE|The)\s+)?").unwrap()).find(body).map_or(0, |found| found.end());
    section_word(body[lead..].trim_end_matches(['.', ':']))
}
