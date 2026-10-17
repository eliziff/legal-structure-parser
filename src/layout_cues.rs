//! Whether a laid-out line reads as a heading's title, by the citation grammar: shared by the PDF
//! parser's heading reading and the engine's note pairing.

use legal_citations::cues;

const TITLECASE_MIN_RATIO: f64 = 0.6;

/// Whether a short line is plausibly a heading's title: capitalized, in title case or
/// capitals, and not a citation. A title can name a decision or a provision ("Court of
/// Appeal of Alberta, 2021 ABCA 273, ..." or "Section 33.1 Infringes Sections 7 and
/// 11(d)"); what it says around its citations and provision numbers must then read as a
/// title, without a citation's style of cause, pinpoint or signal.
pub fn heading_text_plausible(value: &str) -> bool {
    // A title ending in a comma is a sentence's opening ("In these Regulations,").
    !value.trim_end().ends_with(',')
        && (cues::layout_heading_text_plausible(value)
            || titled_around_references_once(value.trim()))
}

/// [`titled_around_references`], each text read once: a document repeats its running heads and
/// titles on every page, and each reading runs the citation grammar.
fn titled_around_references_once(text: &str) -> bool {
    thread_local! {
        static READ: std::cell::RefCell<std::collections::HashMap<String, bool>> = Default::default();
    }
    if let Some(known) = READ.with(|read| read.borrow().get(text).copied()) {
        return known;
    }
    let titled = titled_around_references(text);
    READ.with(|read| {
        let mut read = read.borrow_mut();
        if read.len() >= 4096 {
            read.clear();
        }
        read.insert(text.to_owned(), titled);
    });
    titled
}

/// A title set apart in bold can also name a case by its parties ("R. v. Stone and
/// Automatons"), when it carries no citation the citation grammar finds and does not end
/// in a page number, as a contents entry or a cited page does.
pub fn styled_heading_text_plausible(value: &str) -> bool {
    let text = value.trim();
    heading_text_plausible(text)
        || (text
            .starts_with(|character: char| character.is_alphabetic() && character.is_uppercase())
            && !text.ends_with(',')
            && !text
                .rsplit(char::is_whitespace)
                .next()
                .is_some_and(|word| word.chars().all(|character| character.is_ascii_digit()))
            && title_cased(text.split_whitespace())
            && crate::analysis::citation_spans(text).is_empty())
}

fn title_cased<'a>(words: impl Iterator<Item = &'a str>) -> bool {
    let lettered = words
        .filter_map(|word| word.chars().find(|character| character.is_alphabetic()))
        .collect::<Vec<_>>();
    let capitalized = lettered
        .iter()
        .filter(|character| character.is_uppercase())
        .count();
    capitalized >= 2 && capitalized as f64 / lettered.len() as f64 >= TITLECASE_MIN_RATIO
}

fn dated(citation: &str) -> bool {
    citation
        .split(|character: char| !character.is_ascii_digit())
        .any(|digits| {
            digits.len() == 4
                && digits
                    .parse::<u32>()
                    .is_ok_and(|year| (1700..2100).contains(&year))
        })
}

fn titled_around_references(text: &str) -> bool {
    if !text.starts_with(|character: char| character.is_alphabetic() && character.is_uppercase())
        || text.chars().count() > 200
    {
        return false;
    }
    // A title is read around references only by a dated citation it holds or a provision it
    // counts with no citation at all: a text with no run of four digits a year could sit in and
    // no count is none, citations or not.
    if !text
        .split(|character: char| !character.is_ascii_digit())
        .any(|digits| digits.len() >= 4)
        && !counts_provision(text)
    {
        return false;
    }
    // Each citation, its style of cause, pinpoints and parentheticals included, is cut out.
    let mut uncited = String::new();
    let mut from = 0;
    let mut dated_citation = false;
    for span in crate::analysis::citation_spans(text) {
        let (start, end) = (span.range.start, span.range.end);
        if start < from || text.get(start..end) != Some(span.text.as_str()) {
            continue;
        }
        if text[..start].trim().is_empty() {
            return false;
        }
        dated_citation |= dated(&span.text);
        uncited.push_str(&text[from..start]);
        uncited.push(' ');
        from = end;
    }
    uncited.push_str(&text[from..]);
    // A counted provision ("Section 33.1", "Sections 7 and 11(d)") keeps its noun as a
    // title word; its numbers, and the noun, are no citation cue.
    let (words, prose, counted) = counted_words(&uncited);
    let prose = prose.join(" ");
    // A decision a title names is dated ("2021 ABCA 273"); a bare "1 TO 10" names none.
    ((from > 0 && dated_citation) || (from == 0 && counted))
        && !cues::layout_has_citation_cue(&prose)
        && !cues::layout_has_citation_signal(&prose)
        && title_cased(words.into_iter())
}

/// Whether a text counts a provision ("Section 33.1", "Sections 7 and 11(d)").
fn counts_provision(text: &str) -> bool {
    counted_words(text).2
}

/// A text's title words and prose words, a counted provision's noun kept as a title word and
/// its numbers, and the noun, as no citation cue; and whether it counts a provision.
fn counted_words(text: &str) -> (Vec<&str>, Vec<&str>, bool) {
    let mut words = Vec::new();
    let mut prose = Vec::new();
    let (mut counting, mut counted) = (false, false);
    for word in text.split_whitespace() {
        let numbered = word.chars().any(|character| character.is_ascii_digit());
        if numbered || (counting && matches!(word, "and" | "or" | "to" | "&")) {
            if counting {
                counted = true;
            } else {
                prose.push(word);
            }
            continue;
        }
        counting = cues::is_counter_noun(
            word.trim_matches(|character: char| !character.is_alphanumeric()),
        );
        if !counting {
            prose.push(word);
        }
        words.push(word);
    }
    (words, prose, counted)
}
