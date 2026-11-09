//! The hierarchy rule: each unit's parent from its region, its first line's text, its heading level, its
//! depth and whether it starts a document (hierarchy.py's `assemble`, CONVENTIONS section 2): the one rule the
//! gold, the readers' graphs and the tagger's labels share, supplied to the parsers by `StructureAnalysis`.

use regex::Regex;
use std::sync::OnceLock;

/// The units the hierarchy orders (CONVENTIONS section 2); notes, marginal notes, contents and
/// furniture stay outside it.
const RANKED: [&str; 8] = ["text", "provision", "list_item", "heading", "quote", "table", "caption", "headnote"];
const WORD_OUTER: [&str; 7] = ["PART", "PARTIE", "BOOK", "LIVRE", "TITLE", "TITRE", "VOLUME"];
const WORD_INNER: [&str; 16] = ["ARTICLE", "CHAPTER", "CHAPITRE", "DIVISION", "SECTION", "SCHEDULE", "ANNEX", "ANNEXE",
    "APPENDIX", "TAB", "RULE", "RÈGLE", "REGLE", "CLAUSE", "FORM", "FORMULE"];

struct Patterns {
    sec: Regex,
    dotted: Regex,
    num: Regex,
    roman: Regex,
    alpha: Regex,
    bullet: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        sec: Regex::new(r"^\d+(?:\.\d+)?\s*\(\d+\)").unwrap(),
        dotted: Regex::new(r"^(\d+(?:\.\d+)+)\.?").unwrap(),
        num: Regex::new(r"^[(\[]?\d+([.)\]:])?").unwrap(),
        roman: Regex::new(r"^\(?([IVXLCivxlc]{1,6})[.)\]:]").unwrap(),
        alpha: Regex::new(r"^\(?([A-Za-z]{1,2})[.)\]]").unwrap(),
        bullet: Regex::new(r"^[•·▪–—\-*●○■□►]\s").unwrap(),
    })
}

/// Whether `text` opens with one of `words` as a whole word, case apart.
fn opens_with_word(text: &str, words: &[&str]) -> bool {
    let upper: String = text.chars().take(12).collect::<String>().to_uppercase();
    words.iter().any(|w| upper.starts_with(w) && !upper[w.len()..].chars().next().is_some_and(char::is_alphanumeric))
}

/// Whether the token ending at `end` is followed by whitespace or the end of the text.
fn followed(text: &str, end: usize) -> bool {
    text[end..].chars().next().map_or(true, char::is_whitespace)
}

/// The lower-case letter a letter list item opens with, for reading "i" after "h" as a letter.
fn alpha_token(text: &str) -> Option<char> {
    let t = text.trim_start();
    let m = patterns().alpha.captures(t).filter(|m| followed(t, m.get(0).unwrap().end()))?;
    let mut letters = m[1].chars();
    match (letters.next(), letters.next()) {
        (Some(c), None) => c.to_lowercase().next(),
        _ => None,
    }
}

/// The numbering family a unit opens with ("" for none), hierarchy.py's `numbering`: `open_letter`
/// is the last letter of an open lower-case letter list, so "i" after "h" reads as a letter.
pub fn numbering(text: &str, open_letter: Option<char>) -> String {
    let t = text.trim_start();
    let p = patterns();
    if opens_with_word(t, &WORD_OUTER) {
        return "word_outer".into();
    }
    if opens_with_word(t, &WORD_INNER) {
        return "word_inner".into();
    }
    if p.sec.is_match(t) {
        return "sec".into();
    }
    if let Some(m) = p.dotted.captures(t).filter(|m| followed(t, m.get(0).unwrap().end())) {
        return format!("dec{}", (m[1].matches('.').count() + 1).min(4));
    }
    if let Some(m) = p.num.captures(t) {
        let end = m.get(0).unwrap().end();
        if m.get(1).map_or_else(|| t[end..].starts_with(char::is_whitespace), |_| followed(t, end)) {
            return "num".into();
        }
    }
    if let Some(m) = p.roman.captures(t).filter(|m| followed(t, m.get(0).unwrap().end())) {
        let token = &m[1];
        let lower = token.chars().all(char::is_lowercase);
        let low = token.to_lowercase();
        let (letter, numeral) = if lower { ("al", "rl") } else { ("AU", "RU") };
        if token.chars().count() == 1 && "ivxlc".contains(low.as_str()) {
            let first = low.chars().next().unwrap() as u32;
            if open_letter.is_some_and(|o| o as u32 + 1 == first) {
                return letter.into();
            }
            return numeral.into();
        }
        // (A numeral counts to 25, or is 30, 40 or 50.)
        if legal_structure_model::layout::roman_value(&low).is_some_and(|value| value <= 25 || matches!(value, 30 | 40 | 50)) {
            return numeral.into();
        }
    }
    if let Some(m) = p.alpha.captures(t).filter(|m| followed(t, m.get(0).unwrap().end())) {
        let letters: Vec<char> = m[1].chars().collect();
        if letters.len() == 2 && letters[0] != letters[1] {
            return String::new();
        }
        return if letters.iter().all(|c| c.is_lowercase()) { "al" } else { "AU" }.into();
    }
    if p.bullet.is_match(t) {
        return "bullet".into();
    }
    String::new()
}

/// Parents of units in reading order from each unit's region, first-line text, heading level (0 off
/// headings) and document start: hierarchy.py's `assemble`, CONVENTIONS section 2. A heading falls
/// under the nearest earlier heading of a smaller level; text, tables, captions and headnotes
/// under the nearest earlier heading; a list item continues its numbering family's open list or
/// falls under the unit before it; a quote under the unit before it; a document start resets all.
pub fn ranked(region: &str) -> bool {
    RANKED.contains(&region)
}

/// A body unit with a depth (`depths`, 0 where none) sits inside the nearest earlier body unit of a
/// smaller depth under the same heading, else under the heading.
pub fn assemble(regions: &[&str], texts: &[String], levels: &[usize], depths: &[usize], starts_document: &[bool]) -> Vec<Option<usize>> {
    let mut parents = vec![None; regions.len()];
    let mut heading_stack: Vec<(usize, usize)> = Vec::new();
    let mut list_stack: Vec<(String, usize, Option<usize>)> = Vec::new();
    let mut body_stack: Vec<(usize, usize)> = Vec::new();
    let (mut prev, mut open_letter): (Option<usize>, Option<char>) = (None, None);
    for u in 0..regions.len() {
        let region = regions[u];
        if !RANKED.contains(&region) {
            continue;
        }
        if starts_document[u] {
            heading_stack.clear();
            list_stack.clear();
            body_stack.clear();
            prev = None;
            open_letter = None;
        }
        let top = heading_stack.last().map(|&(_, h)| h);
        if region != "heading" && depths[u] > 0 {
            while body_stack.last().is_some_and(|&(d, _)| d >= depths[u]) {
                body_stack.pop();
            }
            parents[u] = body_stack.last().map(|&(_, b)| b).or(top);
            body_stack.push((depths[u], u));
            prev = Some(u);
            continue;
        }
        if region == "heading" {
            body_stack.clear();
        }
        let parent = match region {
            "heading" => {
                let level = levels[u].max(1);
                while heading_stack.last().is_some_and(|&(l, _)| l >= level) {
                    heading_stack.pop();
                }
                let parent = heading_stack.last().map(|&(_, h)| h);
                heading_stack.push((level, u));
                list_stack.clear();
                open_letter = None;
                parent
            }
            "text" | "provision" | "table" | "caption" | "headnote" => {
                if matches!(region, "text" | "provision") {
                    list_stack.clear();
                    open_letter = None;
                }
                top
            }
            "list_item" => {
                let fam = numbering(&texts[u], open_letter);
                let found = if fam.is_empty() { None } else { list_stack.iter().rposition(|(f, _, _)| *f == fam) };
                let parent = if let Some(k) = found {
                    let parent = list_stack[k].2;
                    list_stack.truncate(k + 1);
                    list_stack[k] = (fam.clone(), u, parent);
                    parent
                } else if fam.is_empty() && !list_stack.is_empty() {
                    let last = list_stack.len() - 1;
                    let parent = list_stack[last].2;
                    list_stack[last].1 = u;
                    parent
                } else {
                    let parent = prev.or(top);
                    list_stack.push((fam.clone(), u, parent));
                    parent
                };
                if fam == "al" {
                    open_letter = alpha_token(&texts[u]);
                }
                parent
            }
            _ => match prev {
                Some(p) if regions[p] == "quote" => parents[p],
                other => other,
            },
        };
        parents[u] = parent;
        prev = Some(u);
    }
    parents
}
