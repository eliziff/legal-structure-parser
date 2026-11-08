//! What a lawyer searches inside the text: short names, quotations and the documents reproduced.

use super::*;

/// Short names the opinion gives: a quoted name in parentheses, an acronym after its full name, "X" means.
pub(super) fn defined_terms(text: &str, ranges: &[(usize, usize)]) -> Vec<DefinedTerm> {
    static QUOTED: OnceLock<Regex> = OnceLock::new();
    static ACRONYM: OnceLock<Regex> = OnceLock::new();
    static MEANS: OnceLock<Regex> = OnceLock::new();
    let quoted = QUOTED.get_or_init(|| Regex::new(r#"\((?:(?:the|collectively|together|jointly|hereinafter|hereafter|referred to as|the term|and|,|\s)+)?[“"‘']([^”"’'()]{1,50})[”"’'](?:\s*(?:or|and|,)\s*(?:the\s+)?[“"‘']([^”"’'()]{1,50})[”"’'])?\)"#).unwrap());
    let acronym = ACRONYM.get_or_init(|| Regex::new(r"((?:\p{Lu}[\p{L}'’&-]*\s+(?:(?:of|for|and|the|on|in|to)\s+)*){2,})\(([A-Z]{2,8})\)").unwrap());
    static SHORT: OnceLock<Regex> = OnceLock::new();
    let short = SHORT.get_or_init(|| Regex::new(r"\([A-Z]{2,8}\)").unwrap());
    let means = MEANS.get_or_init(|| Regex::new(r#"[“"]([^”"]{1,50})[,.]?[”"]\s+(?:shall mean|means|as used herein|shall refer|refers to)\b|\bthe term\s+[“"]([^”"]{1,50})[”"]\s+(?:as used|shall|means|refers)"#).unwrap());
    let coordinates = ScalarText::new(text);
    let mut found = Vec::<DefinedTerm>::new();
    let mut seen = HashSet::new();
    for &(start, end) in ranges {
        let Some(raw) = coordinates.slice_utf16(start..end) else { continue };
        let mut add = |term: &str, form: &str, from: usize, to: usize| {
            let term = squeezed(term);
            if !term.is_empty() && seen.insert(term.to_lowercase()) {
                let at = start + utf16_len(&raw[..from]);
                found.push(DefinedTerm { term, form: form.to_owned(), start: at, end: at + utf16_len(&raw[from..to]), page: None });
            }
        };
        let framed = raw.contains('(');
        for capture in quoted.captures_iter(if framed { raw } else { "" }) {
            let whole = capture.get(0).unwrap();
            add(&capture[1], "parenthetical", whole.start(), whole.end());
            if let Some(second) = capture.get(2) {
                add(second.as_str(), "parenthetical", whole.start(), whole.end());
            }
        }
        let defining = ["mean", "term", "refer", "as used"].iter().any(|word| raw.contains(word));
        for capture in means.captures_iter(if defining { raw } else { "" }) {
            let whole = capture.get(0).unwrap();
            if let Some(term) = capture.get(1).or_else(|| capture.get(2)) {
                add(term.as_str(), "means", whole.start(), whole.end());
            }
        }
        let initials_in = framed && short.is_match(raw);
        for capture in acronym.captures_iter(if initials_in { raw } else { "" }) {
            let whole = capture.get(0).unwrap();
            let initials = capture[1].split_whitespace().filter(|word| word.starts_with(char::is_uppercase)).filter_map(|word| word.chars().next()).collect::<String>();
            // The letters must be the name's: "Administrative Procedure Act (APA)", not "United States (CIA)".
            let mut letters = capture[2].chars();
            let mut wanted = letters.next();
            for initial in initials.chars() {
                if wanted == Some(initial) { wanted = letters.next(); }
            }
            if wanted.is_none() {
                add(&capture[2], "acronym", whole.start(), whole.end());
            }
        }
    }
    found.sort_by_key(|term| term.start);
    found
}

/// Each quotation, inline in double marks of two words or more or set as a block, with the marks an editor made in it.
pub(super) fn quotations(text: &str, paragraphs: &[StructureNode]) -> Vec<OpinionQuote> {
    static INLINE: OnceLock<Regex> = OnceLock::new();
    static MARK: OnceLock<Regex> = OnceLock::new();
    let inline = INLINE.get_or_init(|| Regex::new(r#"“[^“”]{3,}”|"[^"\n]{3,}""#).unwrap());
    let mark = MARK.get_or_init(|| Regex::new(r"(?i)(\[sic\]|\(sic\))|(\[(?:footnotes? |citations? |internal )[^\]]{0,40}omitted\.?\]|\((?:footnotes?|citations?)[^)]{0,40}omitted\.?\))|(\* \* \*|\. ?\. ?\.(?: ?\.)?|…)|(\[[^\[\]]{1,40}\])|(\((?:emphasis|italics)[^)]{0,30}(?:added|supplied|ours|original)\.?\))").unwrap());
    let coordinates = ScalarText::new(text);
    let marked = |raw: &str| raw.contains('[') || raw.contains("* *") || raw.contains(". .") || raw.contains("...") || raw.contains('…') || raw.contains("(emphasis") || raw.contains("(italics") || raw.contains("(sic") || raw.contains("omitted");
    let marks_of = |raw: &str, base: usize| mark.captures_iter(if marked(raw) { raw } else { "" }).filter_map(|capture| {
        let whole = capture.get(0)?;
        let kind = (1..=5).find(|&group| capture.get(group).is_some())?;
        let kind = ["sic", "omitted-note", "omission", "insertion", "emphasis"][kind - 1];
        let start = base + utf16_len(&raw[..whole.start()]);
        Some(QuoteMark { kind: kind.to_owned(), start, end: start + utf16_len(whole.as_str()) })
    }).collect::<Vec<_>>();
    let mut quotes = Vec::new();
    for paragraph in paragraphs {
        let Some(raw) = coordinates.slice_utf16(paragraph.range.start..paragraph.range.end) else { continue };
        if (paragraph.kind == NodeKind::Quote) {
            quotes.push(OpinionQuote { block: true, start: paragraph.range.start, end: paragraph.range.end, marks: marks_of(raw, paragraph.range.start), page: None });
            continue;
        }
        for found in inline.find_iter(raw) {
            if found.as_str().split_whitespace().count() < 2 {
                continue;
            }
            let start = paragraph.range.start + utf16_len(&raw[..found.start()]);
            // The words after a quotation that say whose emphasis it is.
            let after = &raw[found.end()..];
            let tail = after.char_indices().nth(40).map_or(after.len(), |(at, _)| at);
            let mut marks = marks_of(found.as_str(), start);
            let tail_base = start + utf16_len(found.as_str());
            marks.extend(marks_of(&after[..tail], tail_base).into_iter().filter(|mark| mark.kind == "emphasis"));
            quotes.push(OpinionQuote { block: false, start, end: start + utf16_len(found.as_str()), marks, page: None });
        }
    }
    quotes
}

/// Documents reproduced in the opinion: a lead-in that names one and the quoted passages after it.
pub(super) fn instruments(text: &str, paragraphs: &[StructureNode]) -> Vec<OpinionInstrument> {
    static LEAD: OnceLock<Regex> = OnceLock::new();
    static KINDS: OnceLock<[(Regex, &'static str); 12]> = OnceLock::new();
    let lead = LEAD.get_or_init(|| Regex::new(r"(?i)(?:as follows|the following|in (?:relevant |pertinent )?part|reads?|provides?|provided|states?|stated|stating|exchange occurred|said|recites?|charged the jury)\b[^.]{0,60}:[”\x22]?$").unwrap());
    let kinds = KINDS.get_or_init(|| [
        (Regex::new(r"(?i)charged the jury|\bjury (?:charge|instruction)|\binstructions?\b|\bcharge\b").unwrap(), "jury-charge"),
        (Regex::new(r"(?i)\bverdict\b|\bspecial findings?\b").unwrap(), "verdict"),
        (Regex::new(r"(?i)\btestif|\btestimony\b|\bexchange\b|\bcolloquy\b|\bdeposition\b|\bcross-examination\b|\bconfession\b|\bstatement\b").unwrap(), "testimony"),
        (Regex::new(r"(?i)\bwill\b|\btestament\b|\bcodicil\b").unwrap(), "will"),
        (Regex::new(r"(?i)\bcontract\b|\bagreement\b|\bpolicy\b|\bcovenant\b|\bstipulation\b|\bbond\b").unwrap(), "contract"),
        (Regex::new(r"(?i)\blease\b|\bdeed\b|\bmortgage\b|\bnote\b|\bconveyance\b").unwrap(), "instrument"),
        (Regex::new(r"(?i)\bletter\b|\bmemo(?:randum)?\b|\btelegram\b").unwrap(), "letter"),
        (Regex::new(r"(?i)\bcomplaint\b|\bpetition\b|\bindictment\b|\baffidavit\b|\bmotion\b|\banswer\b").unwrap(), "pleading"),
        (Regex::new(r"(?i)\border\b|\bdecree\b|\bjudgment\b|\bresolution\b|\bwrit\b").unwrap(), "order"),
        (Regex::new(r"(?i)\bstatute\b|\bsection\b|\bact\b|\bcode\b|\brule\b|\bregulation\b|\bordinance\b|\bconstitution\b|§").unwrap(), "statute"),
        (Regex::new(r"(?i)\bopinion\b|\bcourt (?:said|stated|held)\b").unwrap(), "opinion-passage"),
        (Regex::new(r"(?i)\bfindings?\b|\breport\b").unwrap(), "findings"),
    ]);
    let coordinates = ScalarText::new(text);
    let mut found = Vec::new();
    for (index, paragraph) in paragraphs.iter().enumerate() {
        if (paragraph.kind == NodeKind::Quote) || !paragraphs.get(index + 1).is_some_and(|next| (next.kind == NodeKind::Quote)) {
            continue;
        }
        let printed = coordinates.slice_utf16(paragraph.range.start..paragraph.range.end).map(trim_js).unwrap_or("");
        if !lead.is_match(printed) {
            continue;
        }
        let sentence = printed.rfind(". ").map_or(printed, |stop| &printed[stop + 2..]);
        let last = paragraphs[index + 1..].iter().take_while(|next| (next.kind == NodeKind::Quote)).last().unwrap();
        // "For the following reasons:" lists; only a lead-in that names a document, or says one speaks, brings one.
        let Some(kind) = kinds.iter().find(|(pattern, _)| pattern.is_match(sentence)).map(|(_, kind)| *kind)
            .or_else(|| sentence.contains(" provides").then_some("passage")).or_else(|| sentence.contains(" reads").then_some("passage")) else { continue };
        found.push(OpinionInstrument { kind: kind.to_owned(), lead_in: squeezed(sentence), start: paragraphs[index + 1].range.start, end: last.range.end });
    }
    found
}
