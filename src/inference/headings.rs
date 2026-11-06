//! Formal and sentence headings, as a judgment's text prints them.

use super::*;

pub(crate) fn heading_enumerator(value: &str) -> bool {
    // Every enumerator the grammar admits ends in a point or a closing parenthesis, but a section's own figures: "5.2.2".
    value.ends_with(['.', ')']) && cached_regex!(VALUE, r"^(?:\([\p{L}\p{N}]{1,5}\)|\p{L}[.)]|[IVXLCDM]{1,4}[.)]|[ivxlcdm]{1,4}[.)]|\d{1,3}(?:\.\d{1,3})*[.)])$").is_match(value)
        || cached_regex!(SECTION, r"^\d{1,3}(?:\.\d{1,3}){2,}$").is_match(value)
}

pub(super) fn level_opens(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|character| character.is_uppercase() || character.is_numeric())
}

pub(super) fn heading_level(words: &[&str], enumerated: bool) -> bool {
    if words.is_empty() || (!enumerated && words.len() > 12) {
        return false;
    }
    if words.len() == 1 && heading_enumerator(words[0]) {
        return true;
    }
    let last = words.last().unwrap();
    const CLOSERS: [char; 4] = ['"', '\u{201d}', '\u{bb}', '\u{2019}'];
    // (A sentence may close on its quotation: "(3d) 88, Gillese J.A. said, ... is still developing.”".)
    // (Not on a name's initials, under an enumerator: "I. The Sexual Activity with A.G.".)
    let initials = enumerated && words.len() <= 12 && last.trim_end_matches('.').contains('.') && last.chars().all(|c| c.is_uppercase() || c == '.');
    let closed = last.trim_end_matches(CLOSERS).ends_with(['.', ',', ';']) && !initials;
    // (Or on a note's call after its stop: "... as an adult.14", "... costs awards.”60".)
    let bare = last.trim_end_matches(|c: char| c.is_ascii_digit());
    let called = bare != *last && bare.trim_end_matches(CLOSERS).strip_suffix(['.', '?', '!']).is_some_and(|word| word.ends_with(char::is_alphabetic));
    // (A heading closes no bracket it never opened, "Legal Probs. 23)", "... was English.)"; a clause's letter does, "de l'al.
    // 11b)", "... Conspiracy?a)".)
    let clause = |word: &str| {
        let mut letters = word.trim_end_matches(')').chars().rev();
        word.ends_with(')') && letters.next().is_some_and(char::is_lowercase) && !letters.next().is_some_and(char::is_alphabetic)
    };
    let count = |bracket: char| words.iter().filter(|word| !clause(word)).map(|word| word.matches(bracket).count()).sum::<usize>();
    if !level_opens(words[0]) || closed || called || count(')') > count('(') {
        return false;
    }
    if last.ends_with(['?', ':']) {
        return true;
    }
    heading_title_case(words) || enumerated || words.len() <= 6
}

pub(super) fn heading_title_case(words: &[&str]) -> bool {
    words.iter().all(|word| {
        let (length, first) = word.chars().filter(|value| value.is_alphabetic()).fold(
            (0, None),
            |(length, first), character| {
                (length + character.len_utf16(), first.or(Some(character)))
            },
        );
        length < 4 || first.is_some_and(char::is_uppercase)
    })
}

pub(super) fn trim_leading_parenthetical(value: &str) -> &str {
    let value = value.trim();
    let Some(rest) = value.strip_prefix('(') else {
        return value;
    };
    let Some(close) = rest.find(')') else {
        return value;
    };
    if !rest[..close].is_empty()
        && rest[..close].chars().all(char::is_alphanumeric)
        && rest[close + 1..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    {
        rest[close + 1..].trim_start()
    } else {
        value
    }
}

pub(crate) fn formal_heading(value: &str) -> bool {
    heading_levels(value).is_some()
}

/// A formal heading's path, a level at a time, each with its enumerator ("IV. Analysis A. The
/// Right ..." is "IV. Analysis" then "A. The Right ..."), as A2AJ runs a decision's heading path
/// onto one line; None where the value is no formal heading.
pub(crate) fn heading_levels(value: &str) -> Option<Vec<String>> {
    heading_levels_within(value, false).or_else(|| cited_heading(value).then(|| vec![value.trim().to_owned()]))
}

/// A heading naming a decision by its citation ("(1) British Columbia Supreme Court (Ehrcke J.), 2008 BCSC 1726, 90
/// B.C.L.R. (4th) 177", a judgment's history): read with each citation one word, its points and brackets no level's.
fn cited_heading(value: &str) -> bool {
    if value.len() > 240 || !value.bytes().any(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let mut spans = crate::analysis::citation_spans(value);
    if spans.is_empty() {
        return false;
    }
    spans.sort_by_key(|span| std::cmp::Reverse(span.range.start));
    let mut masked = value.to_owned();
    for span in spans {
        if masked.is_char_boundary(span.range.start) && masked.is_char_boundary(span.range.end) && span.range.start < span.range.end {
            masked.replace_range(span.range.clone(), "Citation");
        }
    }
    heading_levels_within(&masked, false).is_some()
}

/// [`heading_levels`] for a path the paragraph number after it on its line vouches for: each
/// level may run as long as a heading's line, and a level may hold a semicolon ("V. Analysis A.
/// Was there abuse of authority in the assessment of the merit criteria, particularly at the
/// interview and informal discussion stages? 45 Subsection ...").
pub(crate) fn joined_heading_levels(value: &str) -> Option<Vec<String>> {
    heading_levels_within(value, true)
}

pub(super) fn heading_levels_within(value: &str, joined: bool) -> Option<Vec<String>> {
    let heading = trim_leading_parenthetical(value);
    let excluded = if joined { "![]{}" } else { ";![]{}" };
    if heading.is_empty()
        || !joined && utf16_longer(heading, 120)
        || heading.chars().any(|value| excluded.contains(value))
    {
        return None;
    }
    let words = heading.split_whitespace().collect::<Vec<_>>();
    let mut start = 0;
    let mut enumerated = heading != value.trim();
    let mut levels = Vec::new();
    let mut level = 0;
    for (index, word) in words.iter().enumerate() {
        let opener = words
            .get(index + 1)
            .is_some_and(|next| !heading_enumerator(next) && level_opens(next));
        if heading_enumerator(word) && opener {
            if start < index && !heading_level(&words[start..index], enumerated) {
                return None;
            }
            if index > level {
                levels.push(words[level..index].join(" "));
            }
            level = index;
            start = index + 1;
            enumerated = true;
        }
    }
    if !heading_level(&words[start..], enumerated) {
        return None;
    }
    levels.push(words[level..].join(" "));
    if joined && levels.iter().any(|level| utf16_longer(level, 160)) {
        return None;
    }
    // A leading parenthesized enumerator ("(2) Remedy") is its first level's.
    let prefix = value
        .trim()
        .strip_suffix(heading)
        .unwrap_or_default()
        .trim();
    if !prefix.is_empty() {
        levels[0] = format!("{prefix} {}", levels[0]);
    }
    Some(levels)
}

/// Whether a value runs past `limit` UTF-16 code units, read no further than that: a char
/// is never fewer UTF-8 bytes than UTF-16 units, so a value no longer in bytes is not read.
pub(super) fn utf16_longer(value: &str, limit: usize) -> bool {
    let mut length = 0;
    value.len() > limit
        && value.chars().any(|character| {
            length += character.len_utf16();
            length > limit
        })
}

/// Whether a line may head what follows it rather than being matter the judgment quotes: a table's
/// figure ("1,000.00") has no words, and a quoted enactment's provision opens its first subsection
/// on its number's line ("399. (1) On motion, the Court may …", "6 (1) Except where …").
pub(crate) fn may_head(line: &str) -> bool {
    // (A table's row: "David E. Aiken, George W. Matthews \u{2192} G. Bradley Simon (1995)".)
    if line.contains('\u{2192}') {
        return false;
    }
    let mut words = line.split_whitespace();
    let provision = words.next().is_some_and(|first| first.trim_end_matches('.').bytes().all(|byte| byte.is_ascii_digit() || byte == b'.'))
        && words.next().is_some_and(|second| {
            second.len() >= 3 && second.starts_with('(') && second.ends_with(')')
                && second[1..second.len() - 1].bytes().all(|byte| byte.is_ascii_alphanumeric())
        });
    line.chars().any(char::is_alphabetic) && !provision
}

/// What follows a line, read for whether the line heads it: without the number of the paragraph it
/// opens ("[11] The court ...").
pub(crate) fn after_marker(following: &str) -> &str {
    let following = following.trim_start();
    following
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .filter(|(number, _)| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        .map_or(following, |(_, rest)| rest)
}

/// A line standing between paragraphs read as the heading of what follows it (`following`): its path
/// a level at a time ("IV. Analysis A. The Right ..."), or a sentence heading as one level.
pub(crate) fn heading_between(line: &str, following: &str) -> Option<Vec<String>> {
    if !may_head(line) {
        return None;
    }
    heading_levels(line).or_else(|| sentence_heading(line, after_marker(following)).then(|| vec![line.to_owned()]))
}

/// Where the headings a numbered paragraph's lines end with begin (`lines`: the paragraph's lines, the first the one
/// its number opens; `after`: what follows them, the next paragraph's opening): after its last sentence, a run of
/// lines each the heading of what follows it ("B. Proceedings" / "(1) British Columbia Supreme Court (Ehrcke J.),
/// 2008 BCSC 1726"), or short as a heading is (a numbered judgment numbers its reasons' prose, so a short line
/// after a paragraph's last sentence that ends none of its own, "Cogency", "Event 2: Denial of a second computer",
/// heads what follows), opening under a line that ends a sentence, on a heading's line no list item's "(a)" opens.
/// A sentence ends on its stop, a closing quotation mark or parenthesis, or an editorial line ("[Emphasis added]"),
/// a note's call after it ("… costs awards.”60"); a line that ends on a colon opens a quotation or a list, no
/// heading. A formal heading ("B. Analysis", "IV. Standard of Review") opens what follows under a line that ends no
/// sentence too ("…, 2008 BCSC 1726"), as any heading does set apart by a blank line (`separated`: whether a blank
/// line stands before each line). Headings may stand one above another; the run is theirs and opens what follows.
/// `lines.len()` where the paragraph ends in none; never 0, the paragraph's own first line.
pub fn trailing_headings(lines: &[&str], separated: &[bool], after: &str) -> usize {
    let ended = |line: &str| {
        // (A recognizer reads a stop after a note's call as a bullet: "... injonctive3\u{2022}".)
        let line = line.trim_end();
        if line.ends_with('\u{2022}') {
            return true;
        }
        let called = line.trim_end_matches(|c: char| c.is_ascii_digit());
        let line = if called.ends_with(['.', '?', '!', '"', '\u{201d}', ')']) { called } else { line };
        line.ends_with(['.', ';', '?', '!', '"', '\u{201d}', ')', ']', '\u{2026}', '\u{bb}']) || crate::numbered_paragraphs::editorial_line(line)
    };
    // A line stops on an abbreviation its next words complete ("… at para." / "340.", "… c. F-" / "23.", "OJ NO." / "1189 (C.A.).").
    const SHORTENED: [&str; 25] = ["para", "paras", "par", "al", "man", "s", "ss", "c", "p", "pp", "art", "arts", "no", "nos", "vol", "at", "v", "mr", "mrs", "ms", "dr", "me", "mme", "hon", "prof"];
    let shortened = |line: &str| {
        let bare = line.split_whitespace().last().unwrap_or_default().trim_end_matches('.');
        let word = bare.to_lowercase();
        // (Its article elided: "l'art.", "d'al.".)
        let word = word.rsplit(['\'', '\u{2019}']).next().unwrap_or_default();
        // (Words all the same: "[60] No.", "... B, and C.".)
        let answer = bare == "No" && line.split_whitespace().filter(|word| !word.starts_with('[')).count() == 1;
        SHORTENED.contains(&word) && bare != "C" && !answer
    };
    let completed = |line: &str| {
        let letters = line.split_whitespace().last().unwrap_or_default().trim_end_matches('.');
        // (A reporter's series goes on to its page: "(2d)" / "321.".)
        ended(line) && !shortened(line) && !(letters.contains('.') && letters.chars().all(|c| c.is_alphabetic() || c == '.')) && !cached_regex!(SERIES, r"\(\d*(?:d|th|nd|rd|e)\)$").is_match(line.trim_end())
    };
    // A wrapped citation's tail ends on its figures ("2 S.C.R. 9.", "H., [1999] 2 R.C.S. 3."), no heading's words.
    let citation_tail = |line: &str| {
        let line = line.trim();
        (line.ends_with('.') && line[..line.len() - 1].ends_with(|c: char| c.is_ascii_digit())
            || cached_regex!(PINPOINT, r"(?: at (?:paras? |pp?\.? )?| par\.\s?| p\.\s?)\d+[.)]*$").is_match(line))
            // ("R." and "c." open citations, not headings.)
            && (!line.split_whitespace().next().is_some_and(heading_enumerator) || line.starts_with("R. ") || line.starts_with("c. "))
    };
    // A sentence's tail is no heading: "… is the norm.18 [emphasis added]", "… in his car.”", "…, 2013 BCCA 34 at para.
    // 7)", a parenthesis it does not open. A question is one, as a heading ("…, what is the quantum of damages?") is.
    let closes_sentence = |line: &str| {
        // A heading across a parallel report's columns prints its enumerator again at its end ("B. Was It Fraud?B.");
        // capitals are no sentence ("[SUBMISSIONS RE VICTIM FINE SURCHARGE]"), a citation's are ("1189 (C.A.).").
        let line = line.split_whitespace().next().and_then(|first| line.trim_end().strip_suffix(first)).filter(|rest| !rest.is_empty()).unwrap_or(line);
        let opened = line.matches('(').count() >= line.matches(')').count();
        let capitals = !line.contains(char::is_lowercase) && line.split_whitespace().filter(|word| word.chars().filter(|c| c.is_alphabetic()).count() >= 3).count() >= 2;
        ended(line) && !capitals && !line.ends_with('?') && !(line.ends_with(')') && opened)
    };
    // What a witness or counsel says heads nothing: a heading asks in the third person, an answer opens on its speaker.
    let spoken = |line: &str| {
        let line = line.trim();
        let asked = line.ends_with('?') && line.split_whitespace().any(|word| ["I", "I'm", "I\u{2019}m", "you", "You", "je", "Je", "vous", "Vous"].contains(&word));
        // (French sets a space before its colon: "Homme : O.K.")
        // (A speaker's words end as a sentence does: "Canada: the Standard under s. 686(1)(a)(iii)" is a heading.)
        let speaking = line.ends_with(['.', '?', '!']) && (line.split_whitespace().next().is_some_and(|word| word.ends_with(':')) || line.split_whitespace().nth(1) == Some(":"));
        asked || line.starts_with("A. ") || line.starts_with("Q. ") || speaking
    };
    let short = |line: &str| {
        // (A heading may stand on a bullet: "\u{2022} Principes applicables".)
        // (Or its first letters on the other column's line: "'alinéa 10b)".)
        let line = line.trim().trim_start_matches(['\u{2022}', '\u{25cf}', '\u{25aa}', ' ', '\'', '\u{2019}']);
        // Capitals run on as words of a sentence do not ("LA DEMANDERESSE A-T-ELLE FAIT LA DÉMONSTRATION …").
        let capitals = !line.contains(char::is_lowercase);
        // (A citing line, "See also, Canada Square Corporation, supra", is the quotation's.)
        !utf16_longer(line, if capitals { 300 } else { 120 }) && (1..=if capitals { 50 } else { 18 }).contains(&line.split_whitespace().count())
            && (level_opens(line) || line.starts_with(['"', '\u{201c}']) && heading_title_case(&line.split_whitespace().collect::<Vec<_>>()))
            && !closes_sentence(line) && !line.ends_with(',') && may_head(line) && !line.starts_with("See ") && !line.starts_with("Voir ")
            // (Nor is a sentence's last line one for its lost stop: "There were also no specific firefighter issues during ...".)
            // (Long words: a French heading's little ones are no sentence's.)
            && (line.ends_with('?') || line.split_whitespace().filter(|word| word.starts_with(char::is_lowercase) && word.chars().count() >= 4).count() < 7) && !spoken(line)
            // (Nor a question going on from the last sentence: "Ces motifs étaient-ils suffisants ...?")
            && !(line.ends_with('?') && ["These ", "This ", "Those ", "Ces ", "Cette ", "Ce "].iter().any(|opening| line.starts_with(opening)))
    };
    // A line of its own in title case, a stop after it, is a heading some courts stop: "Impact on the Community.",
    // "LSOG Inc. and LSM Inc."
    // So is a question of its own, however long: "If the April 6 Agreement is valid and binding, is it enforceable in view of
    // s. 59 of the Law and Equity Act, R.S.B.C. 1996, c. 253?"
    let titled = |line: &str| {
        let words = line.split_whitespace().collect::<Vec<_>>();
        let asked = line.trim_end().ends_with('?') && words.len() <= 40;
        // (A word to carry it: "R. (2d) 308 (C.A.).", "456 (H.C.)." are a citation's tail.)
        let worded = words.iter().any(|word| word.chars().count() >= 4 && word.starts_with(char::is_uppercase) && word.chars().all(char::is_alphabetic));
        level_opens(line.trim()) && !spoken(line) && (asked || line.trim_end().ends_with('.') && (2..=8).contains(&words.len()) && !line.contains(',') && worded && heading_title_case(&words))
    };
    let provision = |value: &str| cached_regex!(CITED, r"^(?:ss?|c|pp?|art|no)\.\s+\d").is_match(value);
    // A formal heading stands where its form says so: enumerated, set apart by a blank line, or in title case (a
    // question's wording alone is none); never under a line that introduces it with a colon.
    let formal = |at: usize| {
        let (value, words) = (lines[at].trim(), lines[at].split_whitespace().collect::<Vec<_>>());
        let apart = separated.get(at).copied().unwrap_or(false);
        // (Nor a provision's or a case's next words, "s. 59 of the Act?", "... Limited v." / "Companhia ... 2007 BCSC 610", nor
        // figures, "94, 103-4.1".)
        let cited = provision(value);
        // (A line going on, or a list's bullet, stands over none: "... R. v. Hunter (1997)," / "59 O.R. (2d) 364 (C.A.)".)
        let above = lines[at - 1].trim_end();
        !above.ends_with([':', ',']) && above.chars().any(char::is_alphanumeric) && !shortened(above) && !cited && may_head(value) && formal_heading(value)
            && (words.first().is_some_and(|word| heading_enumerator(word)) || apart || !value.ends_with('?') && heading_title_case(&words))
    };
    let court_opens = |line: &str| !line.contains(" v. ") && cached_regex!(COURT_LINE, r"^(?:(?:The|La|Le)\s+)?(?:\p{Lu}\p{L}+\s+){0,3}(?:Court|Cour|Tribunal)\b").is_match(line.trim());
    // A citation closing a line, with no enumerator before it, is what the paragraph cites: "Premakumar v. Air Canada, 2002
    // CanLII 23561 (CHRT)".
    let cited_line = |line: &str| {
        // (A court's history is a heading on its citation: "The Supreme Court of British Columbia (1994), 91 B.C.L.R. (2d) 365".)
        let court = court_opens(line);
        // (A decision's citation: a statute named as a heading, "Section 35(1) of the Constitution Act, 1982", cites nothing here.)
        let decision = line.contains(" v. ") || cached_regex!(REPORTER, r"\d{4} [A-Z]{2,}|\[\d{4}\]").is_match(line);
        !court && decision && !line.split_whitespace().next().is_some_and(heading_enumerator) && crate::analysis::citation_spans(line).iter().any(|span| span.range.end + 8 >= line.trim_end().len())
    };
    // (A heading on a decision's citation closes its stop: "R. v. Lloyd, 2019 BCCA 128.")
    let case_head = |at: usize| {
        let line = lines[at].trim();
        // (Its paragraph names the case: a source closing a paragraph, "Hinds v. Canada 1988 CarswellNat 993.", heads nothing.)
        let party = line.split(" v. ").next().and_then(|name| name.split_whitespace().last()).unwrap_or_default();
        line.ends_with('.') && line.contains(" v. ") && line.split_whitespace().count() <= 10 && completed(lines[at - 1]) && citation_tail(line)
            && after.chars().take(200).collect::<String>().contains(party)
    };
    // (Words "as follows." introduce, a wrapped "as" / "follows." with them; "what follows." introduces none.)
    let sets_out = |at: usize| {
        let above = lines[at - 1].trim_end();
        above.ends_with("as follows.") || above == "follows." && at >= 2 && lines[at - 2].trim_end().ends_with(" as")
    };
    let (mut at, mut cut, mut stood, mut unformal) = (lines.len(), lines.len(), lines.len(), 0);
    while at > 1 {
        // (A heading's note call is none of its words: "(1) The Jordan Framework[4]".)
        let uncalled = cached_regex!(HEADING_CALL, r"\[\d{1,3}\]\s*$").replace(lines[at - 1], "");
        let line: &str = &uncalled;
        // A lone number under a line going on is the sentence's last word ("… c. F-" / "23."), no heading's enumerator.
        // (Nor does a table's row head anything: "2010: 50 samples".)
        let row = cached_regex!(YEAR_ROW, r"^\d{4}:\s|\$\d").is_match(line.trim());
        // (Nor what a line closing "... as follows." sets out.)
        let set_out = sets_out(at - 1);
        // (Nor does a question under a question, the next of those a paragraph asks.)
        // (Nor a list's next item under its bullet: "-" / "Can the MUHC invoke ...?".)
        // (But a heading in question form under a list's last question is no next question.)
        let opens_item = |line: &str| line.split_whitespace().next().is_some_and(heading_enumerator);
        let asked_on = line.trim_end().ends_with('?') && lines[at - 2].trim_end().ends_with('?') && !(opens_item(lines[at - 2]) && !opens_item(line)) || !lines[at - 2].chars().any(char::is_alphanumeric);
        // A transcript's note in capitals and brackets heads what follows: "[SUBMISSIONS RE VICTIM FINE SURCHARGE]".
        let noted = line.trim().starts_with('[') && line.trim().ends_with(']') && !line.contains(char::is_lowercase) && line.split_whitespace().count() >= 2;
        if heading_enumerator(line.trim()) && !completed(lines[at - 2]) || citation_tail(line) && !case_head(at - 1) || row && !noted || set_out || asked_on || cited_line(line) && !case_head(at - 1) || crate::numbered_paragraphs::editorial_line(line) && !noted {
            break;
        }
        let stands = noted || formal(at - 1) || case_head(at - 1);
        // (What heads nothing for `short` heads nothing here: a stop read as a bullet, a sentence's lost stop, spoken words, a provision, a citing line.)
        let value = line.trim();
        let lowers = value.split_whitespace().filter(|word| word.starts_with(char::is_lowercase)).count();
        let barred = value.ends_with('\u{2022}') || lowers >= 9 && !value.ends_with('?') || spoken(value) || provision(value) || value.starts_with("See ") || value.starts_with("Voir ");
        // (A heading may be set in lower case: "the sentence", under a finished one.)
        let small = value.split_whitespace().count() <= 3 && !ended(value) && completed(lines[at - 2]);
        if !stands && (barred || heading_between(line, lines.get(at).copied().unwrap_or(after)).is_none()) && !short(line) && !(titled(line) && (completed(lines[at - 2]) || at >= 3 && formal(at - 2))) && !small {
            break;
        }
        // What a colon sets out runs on to the paragraph's end, however like headings its lines read: a quoted order's
        // "APPROVED AS TO THE ORDER MADE:" over its signatures. Nor do headings run to more than a few lines: those are a table's.
        unformal += usize::from(!stands);
        // (A heading on a colon introduces nothing: "Analysis:" / "1. Does the applicant ...?".)
        let colon_heading = at >= 3 && completed(lines[at - 3]) && lines[at - 2].split_whitespace().count() <= 5 && lines[at - 2].split_whitespace().all(|word| word.starts_with(char::is_uppercase) || word.chars().count() <= 3);
        if !stands && (lines[at - 2].trim_end().ends_with(':') && !colon_heading || unformal > 4) {
            cut = stood;
            break;
        }
        at -= 1;
        if stands {
            stood = at;
        }
        // A list item's "(a)" opens no heading's line; a formal heading's "(3) Legal proceedings" does.
        if stands || (at > 0 && completed(lines[at - 1]) || separated.get(at).copied().unwrap_or(false)) && !line.trim_start().starts_with('(') {
            cut = at;
        }
    }
    // (Headings run to a few lines, however formal: rows in title case are a table's, "Presentation 1" / "1 to 27".)
    if lines.len() - cut > 8 {
        cut = lines.len();
    }
    // An enumerated heading may run longer than a heading's line, or wrap ("A. Provincial Court of Newfoundland and
    // Labrador," / "2011 NLPC 1709"): from a line opening on an enumerator under a sentence's end, its lines to the
    // paragraph's end closing no sentence.
    // (A court's history whose citation wraps: "Cour d'appel de l'Ontario (1996), 29 O.R. (3d)" / "321".)
    if let Some(at) = (1..cut).find(|&at| completed(lines[at - 1]) && court_opens(lines[at]) && lines.len() - at <= 3) {
        cut = at;
    }
    let enumerated = (1..cut).find(|&at| {
        let (first, rest) = (lines[at].split_whitespace().next(), &lines[at..cut]);
        // The enumerator may run on a dash ("4 - Le Conseil devrait-il ...?", "B - Le bien-fonde"), or stand under a word
        // ("Issue 1:", "Event 4:").
        // (Or be glued to its first word: "(c)School Health Support Services".)
        let dashed = cached_regex!(DASHED, r"^(?:\S{1,4}\s*[\u{2014}\u{2013}-]+\s*|\([\p{L}\p{N}]{1,3}\))\p{Lu}").is_match(lines[at].trim());
        let named = cached_regex!(NAMED, r"^(?:Issue|Event|Item|Question|Ground)\s+\S{1,6}\s*[:.)]?\s").is_match(lines[at].trim());
        // (A heading's words open in capitals, a provision's clause in its sentence: "(e) in the case of ...".)
        // (Of a bare enumerator: "2—L'APAC a-t-elle manqué ...?" is a heading on a dash.)
        let lower = first.is_some_and(heading_enumerator) && lines[at].split_whitespace().nth(1).is_some_and(|word| word.starts_with(char::is_lowercase));
        // (An enumerator split in two, "I V. Analysis", and not a citation's opening: "c. M. (H.)", "R. v. J.-L.J.", "s. 18(1).".)
        let spaced = cached_regex!(SPACED_NUMERAL, r"^[IVX]\s+[IVX]+\.\s").is_match(lines[at].trim());
        let citing = lines[at].split_whitespace().nth(1).is_some_and(|word| word == "v." || cached_regex!(CITING_WORD, r"^(?:\p{Lu}\.|\(\d|\d)").is_match(word));
        // (A question set apart and wrapped: "Should the court address ..." / "than those directly in issue in this case?")
        let asked = separated.get(at).copied().unwrap_or(false) && rest.len() >= 2 && rest.len() <= 4 && lines[at].trim_start().starts_with(char::is_uppercase)
            && rest[rest.len() - 1].trim_end().ends_with('?') && !spoken(lines[at]);
        // (A question under a question is the next of those a paragraph asks, numbered or not: "2. With respect to Jennifer ...?",
        // "3. What order should be made ...?")
        let asked_on = lines[at - 1].trim_end().ends_with('?') && rest[rest.len() - 1].trim_end().ends_with('?');
        // (A section's two levels before a capital, "2.3 The Trial Judge's Ruling": "2.3 million" is none.)
        let section = cached_regex!(SECTION_NUMBER, r"^\d{1,2}\.\d{1,2}\s+\p{Lu}").is_match(lines[at].trim());
        let finished = completed(lines[at - 1]) || dashed && ended(lines[at - 1]) && !shortened(lines[at - 1]);
        finished && !sets_out(at) && !asked_on && (!citing || spaced || named || section) && (!lower && (first.is_some_and(heading_enumerator) || dashed || spaced || section) || named || asked)
            && rest.iter().map(|line| line.chars().count()).sum::<usize>() <= 360
            // (A citation's brackets may close on its last line: "B. Ontario Court of Appeal ... (2008 ONCA 796, 92 O.R." / "(3d) 151)".)
            && rest.iter().enumerate().all(|(k, line)| {
                let last = k + 1 == rest.len();
                // (A term in quotation marks closes no sentence: "... and \u{201c}Pith and Substance\u{201d}".)
                // (A heading's wrapped citation closes nothing on its line: "... (Division générale), [1998]", "... 85 Man. R.", "(Rivard J.".)
                let wraps = cached_regex!(WRAPS, r"(?:\[\d{4}\]|\b\p{Lu}\.)$").is_match(line.trim());
                k == 0 && heading_enumerator(line.trim()) || if last { !closes_sentence(rest.join(" ").trim()) || cached_regex!(TERM, r"\p{L}\s?[\u{201d}\x22\u{bb}]$").is_match(line.trim()) } else { !closes_sentence(line.trim()) || !completed(line) || wraps }
            }) && !rest[rest.len() - 1].trim_end().ends_with(',')
    });
    enumerated.unwrap_or(cut)
}

pub(crate) fn sentence_heading(value: &str, following: &str) -> bool {
    let heading = trim_leading_parenthetical(value);
    if utf16_longer(heading, 120) {
        return false;
    }
    let mut words = heading.split_whitespace();
    let word_count = words.clone().count();
    (4..=18).contains(&word_count)
        && heading.chars().next().is_some_and(char::is_uppercase)
        && words.any(|word| word.chars().next().is_some_and(char::is_lowercase))
        && !heading.chars().any(|value| "[].,;:!?".contains(value))
        && following
            .trim_start()
            .chars()
            .next()
            .is_some_and(char::is_uppercase)
}
