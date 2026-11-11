//! Where a numbered paragraph's own lines end: the lines closing the reasons, the headings its lines end with after its
//! last sentence, the list it goes on with, and what the reasons' last paragraph holds. A line's evidence is read once
//! (`LineEvidence`); each rule below is named, and `paragraph_tail`, `trailing_headings` and `own_matter` ask them in
//! order, so a rule's meaning and the order of its vetoes read in one place.
//!
//! How a line ends is one concept with one answer for each question it is asked, all of them here but the page reader's
//! `sentence_ended` (a stop before any closing marks: `legal_layout::ends_on`, as `heading_level` and the case readers
//! read theirs): `ends_sentence` (the closing marks stand alone too, and a semicolon; a note's call after them; an
//! editorial line; a colon where the reader asks), `completed` (it ends, no abbreviation, initials or reporter's series
//! going on), `closes_sentence` (it ends, no capitals, question or bracket it does not open) and `sentence_closed` (it
//! ends on no one- or two-letter word).

use super::headings::{
    formal_heading, heading_between, heading_enumerator, heading_title_case, level_opens, may_head, utf16_longer,
};
use super::instrument::{enumerated, Levels};
use super::paragraphs::running_prose;
use crate::judgment::{author_line, date_line, disposition_line, end_matter, figure_caption, signature_line};
use crate::numbered_paragraphs::editorial_line;
use std::borrow::Cow;

/// How many of a line's words open in lower case.
pub(super) fn lower_words(line: &str) -> usize {
    line.split_whitespace().filter(|word| word.starts_with(char::is_lowercase)).count()
}

/// Whether a line ends as a sentence does: on a stop (".", ";", "?", "!", "…", and a colon where `colon` says a colon ends
/// one), or a closing quotation mark, bracket or guillemet after one or standing alone; past a note's call printed after
/// it ("… costs awards.”60", "… (Vernon Supp. 2002).\n 5"); or an editorial line ("[Emphasis added]"). The one reader
/// of a sentence's end: the case readers ask it without the colon, the opinion's layout with it (a block introducing a
/// list is finished as a block).
pub(crate) fn ends_sentence(line: &str, colon: bool) -> bool {
    const CLOSERS: [char; 7] = ['"', '\u{201d}', '\u{2019}', '\'', ')', ']', '\u{bb}'];
    let stop = |line: &str| line.ends_with(['.', ';', '?', '!', '\u{2026}']) || (colon && line.ends_with(':'));
    let ends = |line: &str| stop(line) || line.ends_with(CLOSERS);
    // (A recognizer reads a stop after a note's call as a bullet: "... injonctive3\u{2022}".)
    let line = line.trim_end();
    if line.ends_with('\u{2022}') {
        return true;
    }
    // (A colon's note call ends nothing: "... the following two issues on appeal:[8]".)
    if cached_regex!(COLON_CALL, r":\[\d{1,3}\]$").is_match(line) {
        return false;
    }
    let called = line.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end();
    // (A stop inside a section's figures is none: "Section 732.1". A year's is: "... in 1974.14".)
    let decimal = called.strip_suffix('.').is_some_and(|rest| (1..=3).contains(&(rest.len() - rest.trim_end_matches(|c: char| c.is_ascii_digit()).len())));
    let line = if called.len() < line.len() && ends(called) && !decimal { called } else { line };
    ends(line) || editorial_line(line)
}

/// A line stops on an abbreviation its next words complete ("… at para." / "340.", "… c. F-" / "23.", "OJ NO." / "1189 (C.A.)."), or
/// on a citation's year, its volume on the next line ("…, [1992]" / "6 W.W.R. 701 (B.C.C.A.)."), or on the Crown, its case on the
/// next line ("… aux pp. 473 et 474; R." / "c. Biddle, [1995] 1 R.C.S. 761").
fn shortened(words: &[&str]) -> bool {
    const SHORTENED: [&str; 26] = ["para", "paras", "par", "al", "man", "s", "ss", "c", "p", "pp", "art", "arts", "no", "nos", "vol", "at", "v", "mr", "mrs", "ms", "dr", "me", "mme", "hon", "prof", "r"];
    let bare = words.last().copied().unwrap_or_default().trim_end_matches('.');
    let word = bare.to_lowercase();
    // (Its article elided: "l'art.", "d'al.".)
    let word = word.rsplit(['\'', '\u{2019}']).next().unwrap_or_default();
    // (Words all the same: "[60] No.", "... B, and C.".)
    let answer = bare == "No" && words.iter().filter(|word| !word.starts_with('[')).count() == 1;
    SHORTENED.contains(&word) && bare != "C" && !answer || cached_regex!(YEAR, r"^\[\d{4}\]$").is_match(bare)
}

/// A sentence's stop, a remark in brackets after it: "... after pleadings close. (Underlining mine)". A judge's initials are no
/// stop: "Brossard J.A. (dissenting in part)". Nor is a numbered question's gloss a remark: "F. Did SJSL and Raychem Owe BVI and
/// HOOL a Duty to Warn? (Recovery of Contractual Relational Economic Loss)".
fn remarked(line: &str) -> bool {
    let asked = line.contains('?') && line.split_whitespace().next().is_some_and(heading_enumerator);
    !asked && cached_regex!(REMARK, r"\p{Ll}{2}[.?!]\s+\([^()]*\)$").is_match(line.trim_end())
}

/// A sentence's tail is no heading: "… is the norm.18 [emphasis added]", "… in his car.”", "…, 2013 BCCA 34 at para.
/// 7)", a parenthesis it does not open. A question is one, as a heading ("…, what is the quantum of damages?") is.
fn closes_sentence(line: &str) -> bool {
    // A heading across a parallel report's columns prints its enumerator again at its end ("B. Was It Fraud?B.");
    // capitals are no sentence ("[SUBMISSIONS RE VICTIM FINE SURCHARGE]"), a citation's are ("1189 (C.A.).").
    let line = line.split_whitespace().next().and_then(|first| line.trim_end().strip_suffix(first)).filter(|rest| !rest.is_empty()).unwrap_or(line);
    let opened = line.matches('(').count() >= line.matches(')').count();
    let capitals = !line.contains(char::is_lowercase) && line.split_whitespace().filter(|word| word.chars().filter(|c| c.is_alphabetic()).count() >= 3).count() >= 2;
    ends_sentence(line, false) && !capitals && !line.ends_with('?') && !(line.ends_with(')') && opened && !remarked(line))
}

/// Whether text ends a sentence, not a citation ("…; 2 Baudry", "([22]) 1 Masse") or an abbreviation ("p. 3",
/// "Q. R. 6 Q. B. 237"); a pinpoint ends one ("… at para. 23. 24 The …").
pub(super) fn sentence_closed(value: &str) -> bool {
    ends_sentence(value, false)
        && value.split_whitespace().last().is_some_and(|word| {
            let word = word.trim_matches(|c: char| !c.is_alphanumeric());
            !(word.chars().all(char::is_alphabetic) && word.chars().count() <= 2)
        })
}

/// What a witness or counsel says heads nothing: a heading asks in the third person, an answer opens on its speaker.
fn spoken(line: &str) -> bool {
    let line = line.trim();
    // (A heading's numeral speaks for no one: "I Is there a relationship between the donor and ultimate beneficiary?".)
    let words = line.strip_prefix("I ").filter(|rest| rest.starts_with(char::is_uppercase)).unwrap_or(line);
    let asked = line.ends_with('?') && words.split_whitespace().any(|word| ["I", "I'm", "I\u{2019}m", "you", "You", "je", "Je", "vous", "Vous"].contains(&word));
    // (French sets a space before its colon: "Homme : O.K.")
    // (A speaker's words end as a sentence does: "Canada: the Standard under s. 686(1)(a)(iii)" is a heading.)
    let speaking = line.ends_with(['.', '?', '!']) && (line.split_whitespace().next().is_some_and(|word| word.ends_with(':')) || line.split_whitespace().nth(1) == Some(":"));
    asked || line.starts_with("A. ") || line.starts_with("Q. ") || speaking
}

/// Whether a line heads what follows by its shortness, whatever else it shows.
fn short(line: &str) -> bool {
    // (A heading may stand on a bullet: "\u{2022} Principes applicables".)
    // (Or its first letters on the other column's line, its clause's bracket none it left open: "'alinéa 10b)", "’est-ce qui peut ...".)
    let elided = line.trim().starts_with(['\'', '\u{2019}']) && !line.trim().ends_with(['\'', '\u{2019}']);
    let line = line.trim().trim_start_matches(['\u{2022}', '\u{25cf}', '\u{25aa}', ' ', '\'', '\u{2019}']);
    // Capitals run on as words of a sentence do not ("LA DEMANDERESSE A-T-ELLE FAIT LA DÉMONSTRATION …").
    let capitals = !line.contains(char::is_lowercase);
    // (A term in quotation marks stands alone with no stop of its own, the other column's mark on it or not: "\u{201c}Lands reserved for the Indians\u{201d}\u{ab}".)
    let term = cached_regex!(QUOTED_TERM, r#"^[\u{201c}"\u{ab}][^.?!]*[\u{201d}"\u{bb}][\u{201c}\u{ab}]?$"#).is_match(line);
    // (A citing line, "See also, Canada Square Corporation, supra", is the quotation's.)
    // (So is a source's date: "Letter from the Registrar, April 27, 1998".)
    !utf16_longer(line, if capitals { 300 } else { 120 }) && (1..=if capitals { 50 } else { 18 }).contains(&line.split_whitespace().count())
        && (level_opens(line) || elided || term || line.starts_with(['"', '\u{201c}']) && heading_title_case(&line.split_whitespace().collect::<Vec<_>>()))
        && (!closes_sentence(line) || elided && line.ends_with(')') || term) && !line.ends_with(',') && may_head(line) && !line.starts_with("See ") && !line.starts_with("Voir ")
        && !cached_regex!(DATED_SOURCE, r",\s+(?:January|February|March|April|May|June|July|August|September|October|November|December)\s+\d{1,2},\s+\d{4}\.?$").is_match(line)
        // (Nor is a sentence's last line one for its lost stop: "There were also no specific firefighter issues during ...".)
        // (Long words: a French heading's little ones are no sentence's.)
        && (line.ends_with('?') || line.split_whitespace().filter(|word| word.starts_with(char::is_lowercase) && word.chars().count() >= 4).count() < 7) && !spoken(line)
        // (Nor a question going on from the last sentence: "Ces motifs étaient-ils suffisants ...?")
        && !(line.ends_with('?') && ["These ", "This ", "Those ", "Ces ", "Cette ", "Ce "].iter().any(|opening| line.starts_with(opening)))
}

/// A court opening a line: its history, under a heading's enumerator or not. The court's name runs a few words to its
/// bracket, a comma before its citation or division, or the line's end ("La Cour suprême de la Colombie-Britannique (1994),
/// 91 B.C.L.R. (2d) 365", "Cour d'appel, [1995] R.J.Q. 2015", "Federal Court, Trial Division", "La Section de première instance
/// de la Cour fédérale, [1996] 3 C.F. 259"): words going on from it are a sentence's ("The Court is now asked to ...", "La Cour
/// d'appel a ramené la période ...", "Notre Cour a exigé ...").
fn court_opens(line: &str) -> bool {
    !line.contains(" v. ") && cached_regex!(COURT_LINE, r"^(?:(?:The|La|Le)\s+)?(?:\p{Lu}\p{L}+\s+){0,3}(?:Court|Cour|Tribunal|Section\s+de\s+premi.re\s+instance)\b(?:\s+[^\s,(.?!]+){0,8}?(?:\s*\(|,\s*[\[\d\p{Lu}]|\s*$)").is_match(line.trim())
}

/// What a paragraph's line shows, read once: the rules ask this, not the text.
struct LineEvidence<'a> {
    /// The line as given, and without the space around it.
    line: &'a str,
    value: &'a str,
    words: Vec<&'a str>,
    /// `ended`, and `shortened`.
    ended: bool,
    shortened: bool,
    /// It ends a sentence for what follows: `ended`, and nothing it stops on goes on.
    completed: bool,
    /// It opens on a heading's enumerator.
    enumerated: bool,
    /// It ends on a question mark.
    asks: bool,
    /// `spoken`.
    spoken: bool,
    /// It opens on a provision's or a page's abbreviation and its number ("s. 59 of the Act?"), or on a report's series, the
    /// citation over the line going on: "(3d) 14 (Ont. C.A.), at para. 47, per Doherty J.A.".
    provision: bool,
    /// It is in title case (`heading_title_case`).
    title_case: bool,
    /// A transcript's note in capitals and brackets heads what follows: "[SUBMISSIONS RE VICTIM FINE SURCHARGE]".
    noted: bool,
}

impl<'a> LineEvidence<'a> {
    fn read(line: &'a str) -> Self {
        let (value, words) = (line.trim(), line.split_whitespace().collect::<Vec<_>>());
        let (ended, shortened) = (ends_sentence(line, false), shortened(&words));
        let letters = words.last().copied().unwrap_or_default().trim_end_matches('.');
        // (A reporter's series goes on to its page: "(2d)" / "321.".)
        let completed = ended && !shortened && !(letters.contains('.') && letters.chars().all(|c| c.is_alphabetic() || c == '.')) && !cached_regex!(SERIES, r"\(\d*(?:d|th|nd|rd|e)\)$").is_match(value);
        Self {
            line,
            value,
            ended,
            shortened,
            completed,
            enumerated: words.first().is_some_and(|word| heading_enumerator(word)),
            asks: value.ends_with('?'),
            spoken: spoken(line),
            provision: cached_regex!(CITED, r"^(?:(?:ss?|c|pp?|art|no)\.\s+\d|\(\d+(?:d|th|nd|rd|e)\)\s)").is_match(value),
            title_case: heading_title_case(&words),
            noted: value.starts_with('[') && value.ends_with(']') && !value.contains(char::is_lowercase) && words.len() >= 2,
            words,
        }
    }

    /// A wrapped citation's tail ends on its figures ("2 S.C.R. 9.", "H., [1999] 2 R.C.S. 3."), no heading's words; a report's
    /// initials or series and its page stand alone ("N.W.T.R. 201", "L.R. (6th) 1", "(2d) 40", "R. 241").
    fn citation_tail(&self) -> bool {
        let line = self.value;
        cached_regex!(REPORT_PAGE, r"^(?:(?:\p{Lu}\.)+\s*(?:\(\d+(?:d|th|nd|rd|e)\)\s*)?|\(\d+(?:d|th|nd|rd|e)\)\s*)\d+\p{L}?$").is_match(line)
            || (line.ends_with('.') && line[..line.len() - 1].ends_with(|c: char| c.is_ascii_digit())
            || cached_regex!(PINPOINT, r"(?: at (?:paras? |pp?\.? )?| par\.\s?| p\.\s?)\d+[.)]*$").is_match(line))
            // ("R." and "c." open citations, not headings.)
            && (!self.enumerated || line.starts_with("R. ") || line.starts_with("c. "))
    }

    /// A citation closing a line, with no enumerator before it, is what the paragraph cites: "Premakumar v. Air Canada, 2002
    /// CanLII 23561 (CHRT)".
    fn cited(&self) -> bool {
        let line = self.line;
        // (A court's history is a heading on its citation: "The Supreme Court of British Columbia (1994), 91 B.C.L.R. (2d) 365".)
        let court = court_opens(line);
        // (A decision's citation: a statute named as a heading, "Section 35(1) of the Constitution Act, 1982", cites nothing here.)
        let decision = line.contains(" v. ") || cached_regex!(REPORTER, r"\d{4} [A-Z]{2,}|\[\d{4}\]").is_match(line);
        !court && decision && !self.enumerated && crate::analysis::citation_spans(line).iter().any(|span| span.range.end + 8 >= line.trim_end().len())
    }

    /// A line of its own in title case, a stop after it, is a heading some courts stop: "Impact on the Community.",
    /// "LSOG Inc. and LSM Inc."
    /// So is a question of its own, however long: "If the April 6 Agreement is valid and binding, is it enforceable in view of
    /// s. 59 of the Law and Equity Act, R.S.B.C. 1996, c. 253?", its mark lost or not: "Could this hazard ... exposed to it".
    fn titled(&self) -> bool {
        let unmarked = !self.ended && cached_regex!(ASKING, r"^(?:Could|Did|Does|Is|Was|Will|Should|What)\s").is_match(self.value);
        let asked = (self.asks || unmarked) && self.words.len() <= 40;
        // (A word to carry it: "R. (2d) 308 (C.A.).", "456 (H.C.)." are a citation's tail.)
        let worded = self.words.iter().any(|word| word.chars().count() >= 4 && word.starts_with(char::is_uppercase) && word.chars().all(char::is_alphabetic));
        level_opens(self.value) && !self.spoken && (asked || self.value.ends_with('.') && (2..=8).contains(&self.words.len()) && !self.line.contains(',') && worded && self.title_case)
    }

    /// What heads nothing for `short` heads nothing here: a stop read as a bullet, a sentence's lost stop, spoken words, a
    /// provision, a citing line.
    fn barred(&self) -> bool {
        let value = self.value;
        value.ends_with('\u{2022}') || lower_words(value) >= 9 && !self.asks || self.spoken || self.provision || value.starts_with("See ") || value.starts_with("Voir ")
    }

    /// A note's call set apart from its sentence's stop: "... give effect to the intent of this Agreement. 6". An abbreviation's
    /// figure is none: "at para. 6".
    fn call_apart(&self) -> bool {
        cached_regex!(CALL_APART, r"\p{Ll}{3}[.?!]\s+\d{1,3}$").is_match(self.value) && !shortened(&self.words[..self.words.len() - 1])
    }
}

/// A paragraph's lines, read, and what stands around them.
struct Tail<'a> {
    lines: Vec<LineEvidence<'a>>,
    /// Whether a blank line stands before each line.
    separated: &'a [bool],
    /// What follows the lines.
    after: &'a str,
}

impl Tail<'_> {
    fn apart(&self, at: usize) -> bool {
        self.separated.get(at).copied().unwrap_or(false)
    }

    /// A formal heading stands where its form says so: enumerated, set apart by a blank line, or in title case (a
    /// question's wording alone is none); never under a line that introduces it with a colon, but for a list of one.
    fn formal(&self, at: usize, line: &LineEvidence) -> bool {
        let above = &self.lines[at - 1];
        // (A list of one is none: "The sentence I impose is therefore:" / "A. First indictment (no. 169433-2)", the paragraph's last line,
        // heads what follows; "... in turn:" / "1. Is the applicant motivated by commercial or other private interests?".)
        let introduced = above.value.ends_with(':') && !(line.enumerated && at + 1 == self.lines.len() && Levels::opens(line.words[0]));
        // (Nor a provision's or a case's next words, "s. 59 of the Act?", "... Limited v." / "Companhia ... 2007 BCSC 610", nor
        // figures, "94, 103-4.1".)
        // (A line going on, or a list's bullet, stands over none: "... R. v. Hunter (1997)," / "59 O.R. (2d) 364 (C.A.)".)
        !introduced && !above.value.ends_with(',') && above.value.chars().any(char::is_alphanumeric) && !above.shortened && !line.provision && may_head(line.value) && formal_heading(line.value)
            && (line.enumerated || self.apart(at) || !line.asks && line.title_case)
    }

    /// (A heading on a decision's citation closes its stop: "R. v. Lloyd, 2019 BCCA 128.")
    fn case_head(&self, at: usize) -> bool {
        let line = &self.lines[at];
        // (Its paragraph names the case: a source closing a paragraph, "Hinds v. Canada 1988 CarswellNat 993.", heads nothing.)
        let party = line.value.split(" v. ").next().and_then(|name| name.split_whitespace().last()).unwrap_or_default();
        line.value.ends_with('.') && line.value.contains(" v. ") && line.words.len() <= 10 && self.lines[at - 1].completed && line.citation_tail()
            && self.after.chars().take(200).collect::<String>().contains(party)
    }

    /// (Words "as follows." introduce, a wrapped "as" / "follows." with them; "what follows." introduces none.)
    fn sets_out(&self, at: usize) -> bool {
        let above = self.lines[at - 1].line.trim_end();
        above.ends_with("as follows.") || above == "follows." && at >= 2 && self.lines[at - 2].line.trim_end().ends_with(" as")
    }

    /// Whether a line opens on an enumerator past a list's first that goes on with none of the lists open above it.
    fn off_list(&self, at: usize, line: &LineEvidence) -> bool {
        let (mut items, mut listed) = (Levels::default(), false);
        for (token, _) in self.lines[..at].iter().filter_map(|before| enumerated(before.value)) {
            items.read(token);
            listed = true;
        }
        listed && line.enumerated && !Levels::opens(line.words[0]) && !items.continues(line.words[0])
    }

    /// Whether the line at `at` (`line`, its note's call aside) stands in no run of headings, in the order the vetoes are
    /// asked.
    fn stops_run(&self, at: usize, line: &LineEvidence) -> bool {
        let above = &self.lines[at - 1];
        // A lone number under a line going on is the sentence's last word ("… c. F-" / "23.", "..., D.T.E." / "92T-517"), no heading's
        // enumerator.
        (heading_enumerator(line.value) || line.words.len() == 1 && line.value.starts_with(|c: char| c.is_ascii_digit())) && !above.completed
            || line.citation_tail() && !self.case_head(at)
            // (Nor does a table's row head anything: "2010: 50 samples".)
            || cached_regex!(YEAR_ROW, r"^\d{4}:\s|\$\d").is_match(line.value) && !line.noted
            // (Nor what a line closing "... as follows." sets out.)
            || self.sets_out(at)
            // (Nor does a question under a question, the next of those a paragraph asks.)
            // (Nor a list's next item under its bullet: "-" / "Can the MUHC invoke ...?".)
            // (But a heading in question form under a list's last question is no next question, nor is an outline's next
            // heading under a sentence's question: "... immune from review by the courts?" / "(iii) What Constitutes a Policy Decision ...?".)
            // (A list that sets out a question with no label goes on without them: "What is the alleged hazard ...?" / "a) Could ...?" / "Or" / "b) Could ...?" / "Will the threat ...?".)
            // (Nor is a question on an enumerator no open list goes on with: "(1) ... Et (2) ... santé?" / "VI. La juge de première instance était-elle liée ...?".)
            || line.asks && above.asks && !(above.enumerated && !line.enumerated && !self.lines[1.min(at - 1)..at - 1].iter().any(|before| before.asks && !before.enumerated)) && !(line.enumerated && line.title_case && !above.words.iter().take(2).any(|word| heading_enumerator(word)) && !Levels::opens(line.words[0])) && !self.off_list(at, line)
            || !above.value.chars().any(char::is_alphanumeric)
            || line.cited() && !self.case_head(at)
            || editorial_line(line.line) && !line.noted
            || remarked(line.line)
    }

    /// Whether the line at `at` (`line`, its note's call aside), standing on no form of its own, reads as a heading all the
    /// same.
    fn reads_as_heading(&self, at: usize, line: &LineEvidence) -> bool {
        let above = &self.lines[at - 1];
        // (A heading may be set in lower case: "the sentence", under a finished one.)
        let small = line.words.len() <= 3 && !line.ended && above.completed;
        // (A title may wrap onto its provision: "The Admissibility of a Videotaped Statement Under" / "s. 715.1".)
        let wrapped = line.provision && line.words.len() <= 3 && !above.ended && above.title_case;
        let next = self.lines.get(at + 1).map_or(self.after, |next| next.line);
        !line.barred() && heading_between(line.line, next).is_some() || short(line.line) || line.titled() && (above.completed || at >= 2 && self.formal(at - 1, above)) || small || wrapped
    }

    /// (A heading on a colon introduces nothing: "Analysis:" / "1. Does the applicant ...?".)
    fn colon_heading(&self, at: usize) -> bool {
        let line = &self.lines[at];
        at >= 1 && self.lines[at - 1].completed && line.words.len() <= 5 && line.words.iter().all(|word| word.starts_with(char::is_uppercase) || word.chars().count() <= 3)
    }

    /// Where the list a colon's line sets out ends, its numbers bare: "... in the following order:" / "I   Directors" / "II Directors
    /// (metteurs en scène)" / "III   Choreographers" / "IV  Fight directors", the next "I DIRECTORS (RÉALISATEURS)" no item of it.
    /// A level under the list's own is the list's where the list comes back over it: "The outline is as follows:" / "A. General
    /// Principles" / "(1) Nature ..." / "B. Application" / "(1) Reasonable ...", the next "IV. Analysis" no item of it; "... as
    /// follows:" / "1. ...?" / "2. ...?" / "(1) The Second Amendment is a Policy Decision" heads what follows.
    fn counted_list(&self, colon: usize) -> usize {
        let mut items = Levels::default();
        let token = |at: usize| self.lines.get(at).and_then(|line| line.words.first()).map(|word| if heading_enumerator(word) { (*word).to_owned() } else { format!("{word}.") });
        let (mut at, mut end, mut under, mut returned) = (colon + 1, colon + 1, false, false);
        while let Some(token) = token(at).filter(|token| if at == colon + 1 { Levels::opens(token) } else { items.continues(token) || items.nests(token) }) {
            let outermost = items.read(&token) == Some(0);
            at += 1;
            under |= !outermost;
            returned |= outermost && under;
            if outermost || returned {
                end = at;
            }
        }
        end
    }

    /// Where the run of headings the lines end with opens: `lines.len()` where they end in none.
    fn heading_run(&self) -> usize {
        let lines = &self.lines;
        let (mut at, mut cut, mut stood, mut unformal) = (lines.len(), lines.len(), lines.len(), 0);
        while at > 1 {
            // (A heading's note call is none of its words: "(1) The Jordan Framework[4]".)
            let uncalled = cached_regex!(HEADING_CALL, r"\[\d{1,3}\]\s*$").replace(lines[at - 1].line, "");
            let reread = matches!(uncalled, Cow::Owned(_)).then(|| LineEvidence::read(&uncalled));
            let line = reread.as_ref().unwrap_or(&lines[at - 1]);
            if self.stops_run(at - 1, line) {
                break;
            }
            let stands = line.noted || self.formal(at - 1, line) || self.case_head(at - 1);
            if !stands && !self.reads_as_heading(at - 1, line) {
                break;
            }
            // What a colon sets out runs on to the paragraph's end, however like headings its lines read: a quoted order's
            // "APPROVED AS TO THE ORDER MADE:" over its signatures. Nor do headings run to more than a few lines: those are a table's.
            unformal += usize::from(!stands);
            let colon = lines[at - 2].value.ends_with(':') && !self.colon_heading(at - 2);
            if !stands && (colon || unformal > 4) {
                cut = if colon { stood.max(self.counted_list(at - 2)) } else { stood };
                break;
            }
            at -= 1;
            if stands {
                stood = at;
            }
            // A list item's "(a)" opens no heading's line; a formal heading's "(3) Legal proceedings" does.
            if stands || (lines[at - 1].completed || self.apart(at)) && !line.value.starts_with('(') {
                cut = at;
            }
        }
        // (Headings run to a few lines, however formal: rows in title case are a table's, "Presentation 1" / "1 to 27".)
        if lines.len() - cut > 8 {
            cut = lines.len();
        }
        // (A court's history whose citation wraps: "Cour d'appel de l'Ontario (1996), 29 O.R. (3d)" / "321".)
        (1..cut).find(|&at| lines.len() - at <= 3 && lines[at - 1].completed && court_opens(lines[at].line)).unwrap_or(cut)
    }

    /// An enumerated heading may run longer than a heading's line, or wrap ("A. Provincial Court of Newfoundland and
    /// Labrador," / "2011 NLPC 1709"): from a line opening on an enumerator under a sentence's end, its lines to the
    /// paragraph's end closing no sentence.
    fn enumerated_heading(&self, at: usize, cut: usize) -> bool {
        let (line, above, rest) = (&self.lines[at], &self.lines[at - 1], &self.lines[at..cut]);
        // (Nothing opens under a line going on, and the readings below are the dearest here.)
        // (Nor is a remark in brackets an enumerator: "(sic)" / "(c'est nous qui soulignons)".)
        if !above.ended || above.shortened || editorial_line(line.value) {
            return false;
        }
        let (value, last) = (line.value, &rest[rest.len() - 1]);
        // The enumerator may run on a dash ("4 - Le Conseil devrait-il ...?", "B - Le bien-fonde"), or stand under a word
        // ("Issue 1:", "Event 4:").
        // (Or be glued to its first word: "(c)School Health Support Services".)
        // (A provider's dash may hold a hyphen that does not break: "II -\u{2011} Decision of the Court of Appeal".)
        let dashed = cached_regex!(DASHED, r"^(?:\S{1,4}\s*[\u{2014}\u{2013}\u{2011}-]+\s*|\([\p{L}\p{N}]{1,3}\))\p{Lu}").is_match(value);
        let named = cached_regex!(NAMED, r"^(?:Issue|Event|Item|Question|Ground)\s+\S{1,6}\s*[:.)]?\s").is_match(value);
        // (A heading's words open in capitals, a provision's clause in its sentence: "(e) in the case of ...".)
        // (Of a bare enumerator: "2—L'APAC a-t-elle manqué ...?" is a heading on a dash.)
        let lower = line.enumerated && line.words.get(1).is_some_and(|word| word.starts_with(char::is_lowercase));
        // (An enumerator split in two, "I V. Analysis", and not a citation's opening: "c. M. (H.)", "R. v. J.-L.J.", "s. 18(1).".)
        let spaced = cached_regex!(SPACED_NUMERAL, r"^[IVX]\s+[IVX]+\.\s").is_match(value);
        let citing = line.words.get(1).is_some_and(|word| *word == "v." || cached_regex!(CITING_WORD, r"^(?:\p{Lu}\.|\(\d|\d)").is_match(word));
        // (A question set apart and wrapped: "Should the court address ..." / "than those directly in issue in this case?")
        let asked = self.apart(at) && rest.len() >= 2 && rest.len() <= 4 && value.starts_with(char::is_uppercase) && last.asks && !line.spoken;
        // (A question under a question is the next of those a paragraph asks, numbered or not: "2. With respect to Jennifer ...?",
        // "3. What order should be made ...?")
        let asked_on = above.asks && last.asks;
        // (A section's two levels before a capital, "2.3 The Trial Judge's Ruling": "2.3 million" is none.)
        let section = cached_regex!(SECTION_NUMBER, r"^\d{1,2}\.\d{1,2}\s+\p{Lu}").is_match(value);
        let finished = above.completed || dashed;
        finished && !self.sets_out(at) && !asked_on && (!citing || spaced || named || section) && (!lower && (line.enumerated || dashed || spaced || section) || named || asked)
            && rest.iter().map(|line| line.line.chars().count()).sum::<usize>() <= 360
            // (A citation's brackets may close on its last line: "B. Ontario Court of Appeal ... (2008 ONCA 796, 92 O.R." / "(3d) 151)".)
            && rest.iter().enumerate().all(|(k, line)| {
                // (A heading's wrapped citation closes nothing on its line: "... (Division générale), [1998]", "... 85 Man. R.", "(Rivard J.".)
                let wraps = !line.completed || cached_regex!(WRAPS, r"(?:\[\d{4}\]|\b\p{Lu}\.)$").is_match(line.value);
                // (A term in quotation marks closes no sentence: "... and \u{201c}Pith and Substance\u{201d}".)
                k == 0 && heading_enumerator(line.value) || if k + 1 == rest.len() {
                    // (A heading's last word, a line read as a heading already: "(3) Les erreurs de droit dans la défense de M." / "Barton".)
                    !closes_sentence(rest.iter().map(|line| line.line).collect::<Vec<_>>().join(" ").trim()) || cached_regex!(TERM, r"\p{L}\s?[\u{201d}\x22\u{bb}]$").is_match(line.value) || cut < self.lines.len() && wraps
                } else {
                    !closes_sentence(line.value) || wraps
                }
            }) && !last.value.ends_with(',') && !last.call_apart()
    }
}

/// Where the headings a numbered paragraph's lines end with begin (`lines`: the paragraph's lines, the first the one
/// its number opens; `after`: what follows them, the next paragraph's opening): after its last sentence, a run of
/// lines each the heading of what follows it ("B. Proceedings" / "(1) British Columbia Supreme Court (Ehrcke J.),
/// 2008 BCSC 1726"), or short as a heading is (a numbered judgment numbers its reasons' prose, so a short line
/// after a paragraph's last sentence that ends none of its own, "Cogency", "Event 2: Denial of a second computer",
/// heads what follows), opening under a line that ends a sentence, on a heading's line no list item's "(a)" opens.
/// A line that ends on a colon opens a quotation or a list, no
/// heading. A formal heading ("B. Analysis", "IV. Standard of Review") opens what follows under a line that ends no
/// sentence too ("…, 2008 BCSC 1726"), as any heading does set apart by a blank line (`separated`: whether a blank
/// line stands before each line). Headings may stand one above another; the run is theirs and opens what follows.
/// `lines.len()` where the paragraph ends in none; never 0, the paragraph's own first line.
fn trailing_headings(lines: &[&str], separated: &[bool], after: &str) -> usize {
    let tail = Tail { lines: lines.iter().map(|line| LineEvidence::read(line)).collect(), separated, after };
    let cut = tail.heading_run();
    (1..cut).find(|&at| tail.enumerated_heading(at, cut)).unwrap_or(cut)
}

/// How many of a paragraph's lines stand before the lines closing the reasons: all of them, but for a line closing the
/// reasons (`closing_line`), a figure's caption, and the signatures and dates over them.
fn closing_at(lines: &[&str]) -> usize {
    // (A sentence may wrap onto a caption's words: "... as shown in" / "Figure 1.".)
    // (Under a note's call too: "... at arm's length.11" / "Figure 4.".)
    let caption = |at: usize| figure_caption(lines[at]) && lines[at - 1].trim_end().trim_end_matches(|c: char| c.is_ascii_digit()).ends_with(['.', '?', '!', ':', ')', '"', '\u{201d}']);
    // (A judge's name may be a sentence's last words: "... de la manière proposée par" / "Sopinka J.".)
    let named = |at: usize| signature_line(lines[at].trim()) && !lines[at - 1].trim_end().ends_with(['.', '?', '!', ':', ';', ')', ']', '"', '\u{201d}'])
        && lines[at - 1].split_whitespace().any(|word| word.starts_with(char::is_lowercase));
    // (A provider may set a judge's line in slashes: "//La Forest J.//".)
    let mut closed = (1..lines.len()).find(|&at| closing_line(lines[at].trim().trim_matches('/')) && !named(at) || caption(at)).unwrap_or(lines.len());
    // The signatures and release lines a paragraph ends with are the decision's ("Released: …", "“K. Feldman J.A.”").
    // (A line of prose may end on a judge: "(3d) 14 (Ont. C.A.), at para. 47, per Doherty J.A.".)
    // (Nor is a note in brackets prose: "//Le juge Forget (ad hoc)//".)
    let signed = |line: &str| signature_line(line.trim().trim_matches('/')) && lower_words(line.split('(').next().unwrap_or(line)) <= 1 && !line.contains(|c: char| c.is_ascii_digit());
    // (A date may finish a sentence: "... the parties have until" / "April 3, 2026.".)
    while closed > 1 && (signed(lines[closed - 1]) || date_line(lines[closed - 1]) && ends_sentence(lines[closed - 2], false)) && !named(closed - 1) {
        closed -= 1;
    }
    // (The name over a title goes with it: "Claude Roy" / "Vice-président".)
    let name = |line: &str| {
        let words = line.split_whitespace().collect::<Vec<_>>();
        (2..=4).contains(&words.len()) && words.iter().all(|word| word.starts_with(char::is_uppercase)) && !line.trim_end().ends_with('.')
    };
    if closed > 1 && closed < lines.len() && signature_line(lines[closed].trim()) && name(lines[closed - 1]) {
        closed -= 1;
    }
    // (Whatever else stands above: a title alone closes no paragraph, its signer does.)
    if let Some(at) = (2..lines.len()).find(|&at| signature_line(lines[at].trim()) && name(lines[at - 1])) {
        closed = closed.min(at - 1);
    }
    closed
}

/// The lines the list a paragraph's lines before `cut` read goes on into: a line going on with the list is its next item,
/// however like a heading it reads. (But for a last line closing on nothing under an item that closed: "(h) any other relevant
/// circumstances." / "C. Procedural Issues: Judge and Jury" heads what follows.)
fn list_goes_on(lines: &[&str], mut cut: usize) -> usize {
    let mut items = Levels::default();
    for line in &lines[..cut] {
        if let Some((token, _)) = enumerated(line.trim()) {
            items.read(token);
        }
    }
    let titled = |at: usize| at + 1 == lines.len() && at > 0 && ends_sentence(lines[at - 1], false) && !ends_sentence(lines[at], false);
    while let Some(token) = lines.get(cut).filter(|_| !titled(cut)).and_then(|line| enumerated(line.trim())).map(|(token, _)| token).filter(|token| items.continues(token)) {
        items.read(token);
        cut += 1;
        // (An item's wrapped words are the item's: "C. Should the evidence be excluded under s. 24(2)" / "of the Charter?".)
        while lines.get(cut).is_some_and(|line| line.trim_start().starts_with(char::is_lowercase)) {
            cut += 1;
        }
        // (A parenthesis under an item is the item's: "(page 527)", "(traduction)".)
        if lines.get(cut).is_some_and(|line| line.trim().starts_with('(') && line.trim().ends_with(')')) {
            cut += 1;
        }
    }
    cut
}

/// How many of a numbered paragraph's lines are its own (`lines`: from the one its number opens to the next number;
/// `separated`: whether a blank line stands before each; `after`: the text following them): all of them, but for a
/// line closing the reasons and what follows it
/// (`closing_line`), and the headings its lines end with after its last sentence, which open what follows
/// (`trailing_headings`); a line going on with the paragraph's list is its next item, however like a heading it
/// reads. The count's `last` paragraph ends with its own words and what they introduce (`own_matter`): nothing
/// follows the reasons' last paragraph as its own.
pub(super) fn paragraph_tail(lines: &[&str], separated: &[bool], after: &str, last: bool) -> usize {
    let lines = &lines[..closing_at(lines)];
    let cut = list_goes_on(lines, trailing_headings(lines, separated, after));
    if last { own_matter(&lines[..cut]) } else { cut }
}

/// A line that closes a judge's reasons, in any language the court writes: the disposition set on its own ("Appeal
/// allowed, La Forest J. dissenting."), the judges concurring, or the judge's name alone with a title
/// (`signature_line`: "Bateman, J.A.", "MacDonald, C.J.N.S."), two to four capitalized words; a sentence naming a
/// judge ("This was the view of Lamer C.J.") closes nothing.
pub(crate) fn closing_line(line: &str) -> bool {
    let words = line.split_whitespace().collect::<Vec<_>>();
    disposition_line(line) || line.starts_with("Concurred") || author_line(line) || end_matter(line)
        // (A provider's inline note, "[FN8] There are many ...", and a line naming the next reasons.)
        || cached_regex!(INLINE_NOTE, r"^\[FN\d+\]").is_match(line) || cached_regex!(DELIVERED, r"(?:rendus? par|were delivered by)$").is_match(line)
        || (2..=4).contains(&words.len()) && words.iter().all(|word| word.trim_start_matches(['"', '\u{201c}']).starts_with(char::is_uppercase)) && signature_line(line)
}

/// What a line of the reasons' last paragraph shows, beside the line above it and the one below (`own_matter`).
struct OwnLine {
    /// The line above ends on a colon.
    introduced: bool,
    /// A passage in quotation marks.
    quoted: bool,
    /// A list's next item.
    item: bool,
    /// An editorial or citing line.
    set_under: bool,
    /// A quotation's next line.
    going_on: bool,
    /// The sentence above goes on over its line.
    unfinished: bool,
    /// A long running line.
    running_line: bool,
    /// A quoted order's heading over its list.
    heads_list: bool,
    /// A sentence, long or short, that closes nothing.
    prose: bool,
}

impl OwnLine {
    fn read(lines: &[&str], at: usize, quoting: bool, items: &Levels) -> Self {
        const QUOTES: [char; 5] = ['"', '\u{201c}', '\u{ab}', '\u{2018}', '\''];
        let (line, previous) = (lines[at].trim(), lines[at - 1].trim_end());
        // (A signature in quotation marks is a signature.)
        // (Or a name alone in them: "David Stratas".)
        let unquoted = line.trim_matches(QUOTES);
        let name = unquoted.len() + 2 <= line.len() && (2..=4).contains(&unquoted.split_whitespace().count()) && unquoted.split_whitespace().all(|word| word.starts_with(char::is_uppercase));
        let introduced = previous.ends_with(':');
        // (A list's marker alone on its line has its words on the next.)
        let marked = previous.split_whitespace().count() == 1 && enumerated(&format!("{} x", previous.trim())).is_some();
        // (A short sentence goes on as a long one does: "I do not reach Kenny's sentence appeal.")
        let lowercase = lower_words(line);
        let sentence = lowercase >= 3 && line.split_whitespace().count() >= 5 && line.ends_with(['.', '?', '!']);
        // (A sentence goes on over its line whatever the next one looks like: "... as proposed by" / "Sopinka J.", "... under s." / "43 of FIPPA:".)
        let above_runs = lower_words(previous) >= 4;
        // (A citation wrapped over its line: "..., 2003 SCC 69, [2003]" / "3 S.C.R. 263, at para. 77.", "... 67 C.C.C. (3d)" / "385 (C.A.C.-B.); R. c. Stockley".)
        let wrapped = previous.ends_with(',') || cached_regex!(YEAR_END, r"\[\d{4}\]$|\(\d*(?:d|th|nd|rd|e)\)$").is_match(previous);
        let closing = closing_line(line);
        Self {
            introduced,
            quoted: line.starts_with(QUOTES) && !closing_line(unquoted) && !name,
            item: enumerated(line).is_some_and(|(token, _)| introduced || quoting || items.continues(token)),
            set_under: editorial_line(line) || (line.starts_with('(') && line.ends_with(')')),
            going_on: quoting && (marked || line.starts_with(|c: char| c.is_lowercase() || matches!(c, '.' | '\u{2026}'))),
            unfinished: wrapped || above_runs && (!previous.ends_with(['.', '?', '!', ':', ';', ')', ']', '"', '\u{201d}']) || cached_regex!(STOPPED, r"(?:\bs|\bss|\bc|\bpara|\bparas|\bp|\bpp|\bat|\bv|\bno)\.$").is_match(previous)),
            // (A long running line ends the paragraph on nothing it ends on, "... the following terms and".)
            running_line: line.split_whitespace().count() >= 8 && lowercase >= 4 && !closing,
            // (A quoted order's second heading over its list: "On the second indictment, ..." / "a)".)
            heads_list: (quoting || enumerated(previous).is_some()) && lines.get(at + 1).is_some_and(|next| enumerated(next.trim()).is_some()),
            // (A line setting out what follows is one too: "Section 683(3) provides:".)
            prose: (running_prose(line) || sentence || lowercase >= 1 && line.ends_with(':')) && !closing,
        }
    }

    /// Whether the line is the paragraph's own: anything else opens matter of its own, and ends it.
    fn own(&self, quoting: bool) -> bool {
        self.introduced || self.quoted || self.item || self.going_on || self.unfinished || self.running_line || self.heads_list || quoting && self.set_under || self.prose
    }

    /// Whether a quotation stands open under the line.
    fn quotes_on(&self, quoting: bool) -> bool {
        self.introduced || self.quoted || self.going_on || self.heads_list || quoting && (self.item || self.set_under)
    }
}

/// How many of the reasons' last paragraph's lines are its own: its first, and after it each line its words go on
/// into, the quotation or list a line ending on a colon introduces, a list's next item, a passage in quotation marks,
/// a quotation's next lines and the editorial or citing lines under it ("[Emphasis added.]", "(at para. 12)"), and
/// running sentences, numbered or not ("I will nominate a date ..."). The first line opening matter of its own ends it:
/// an order, a signature, an appendix, the next reasons.
fn own_matter(lines: &[&str]) -> usize {
    let mut items = Levels::default();
    let mut quoting = false;
    for at in 0..lines.len() {
        if at > 0 {
            let line = OwnLine::read(lines, at, quoting, &items);
            if !line.own(quoting) {
                return at;
            }
            quoting = line.quotes_on(quoting);
        }
        if let Some((token, _)) = enumerated(lines[at].trim()) {
            items.read(token);
        }
    }
    lines.len()
}
