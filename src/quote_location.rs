//! Where a quotation is in its source: the cited paragraph, page or provision first, then anywhere else, then the
//! document as a whole; the block that holds it, its score, the source's own words for it. Ported from Beaver's
//! quoteCheck.ts (itself ported from the ALR Quote Verifier's _apply_quote_checks and resolve_quote), with its
//! JavaScript string semantics kept where they decide a result: lengths and slices count UTF-16 units, and trimming
//! and lower-casing are ECMAScript's. A source's quotations are located in one call, the source's text read once.

use crate::quote_verification::QuoteSupport;
use legal_structure_model::{ScalarRange, ScalarText};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const LONG_SOURCE_CHARS: usize = 80_000;
const LONG_SOURCE_TOKENS: usize = 8_000;
const MIN_MATCH: f64 = 0.6;
const STRONG_MATCH: f64 = 0.98;
const MAX_SEQUENCE_WORK: usize = 4_000_000;
const NON_CONTENT: [&str; 23] = ["a", "an", "and", "are", "as", "at", "be", "been", "being", "by", "for", "from", "in",
    "is", "it", "its", "of", "on", "or", "that", "the", "their", "to"];
const NON_CONTENT_MORE: [&str; 3] = ["was", "were", "with"];
const DOUBLE_QUOTES: [&str; 6] = ["\"", "\u{201c}", "\u{201d}", "\u{ab}", "\u{bb}", "\u{201e}"];
const SINGLE_QUOTES: [&str; 4] = ["'", "\u{2018}", "\u{2019}", "\u{201a}"];
const TRAILING: [&str; 8] = [".", ",", ";", ":", "!", "?", "\u{2026}", "..."];

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(concat!(r"\.\.\.|\[[^\]\[\n]{1,80}\]|",
    r"[\p{L}\p{N}]+(?:['\u{2019}][\p{L}\p{N}]+)*|[\x22\u{201c}\u{201d}\u{2018}\u{2019}]|[^\p{L}\p{N}_\s]")).unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+(?:['\u{2019}][\p{L}\p{N}]+)*").unwrap());
static SINGLE_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}]+(?:['\u{2019}][\p{L}\p{N}]+)*$").unwrap());
static PLAIN_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}]+(?:'[\p{L}\p{N}]+)*$").unwrap());
static INITIAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[([A-Za-z])\]([A-Za-z]+)$").unwrap());
static ALIASED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([A-Za-z]+)((?:\[[A-Za-z]+\])+)$").unwrap());
static BRACKETED_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]+(?:\[[A-Za-z]+\])+$").unwrap());
static BRACKET_GROUP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([A-Za-z]+)\]").unwrap());
static ONLY_BRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[[^\]]+\]$").unwrap());
static LEADING_NUMBER: LazyLock<Regex> = LazyLock::new(||
    Regex::new(r"^(?:\[[\s]*[0-9]{1,4}[\s]*\]|[0-9]{1,4}[\s]*\([0-9]{1,4}\)|[0-9]{1,4})[\s]+").unwrap());
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static SECTION_TAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([^)]+\)$").unwrap());

fn is_dash(c: char) -> bool {
    matches!(c, '-' | '\u{ad}' | '\u{2010}'..='\u{2015}' | '\u{2212}')
}
fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric()
}
/// ECMAScript's String.prototype.trim: its whitespace and line terminators.
fn js_trim(text: &str) -> &str {
    text.trim_matches(js_space)
}
fn js_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
        | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}
/// `text.slice(start, end)` in UTF-16 units.
fn slice16(text: &[u16], start: usize, end: usize) -> String {
    let end = end.min(text.len());
    if start >= end { return String::new(); }
    String::from_utf16_lossy(&text[start..end])
}
/// `haystack.indexOf(needle, from)` over UTF-16 units.
fn index_of16(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() { return Some(from.min(haystack.len())); }
    if needle.len() > haystack.len() { return None; }
    (from..=haystack.len() - needle.len()).find(|&at| haystack[at..at + needle.len()] == *needle)
}
fn byte_to_utf16(text: &str, byte: usize) -> usize {
    utf16_len(&text[..byte])
}

/// Each text's tokens, words and keys, read once per call.
#[derive(Default)]
struct Reader {
    tokens: HashMap<String, std::rc::Rc<Vec<String>>>,
    exact: HashMap<String, String>,
    /// The source document's text (trimmed) by where it is, and what is read of it once for every quotation.
    whole: Option<(usize, usize)>,
    whole_words: Option<std::rc::Rc<LongWords>>,
    whole_positions: Option<std::rc::Rc<Vec<usize>>>,
    /// The document's text in UTF-16, as written and lower-cased.
    whole_units: Option<std::rc::Rc<(Vec<u16>, Vec<u16>)>>,
}

/// A long source's lower-cased words and the set of them.
type LongWords = (Vec<String>, HashSet<String>);

#[derive(Clone)]
struct Token {
    text: String,
    start: usize,
    end: usize,
}

fn mergeable(token: &str) -> bool {
    token.chars().any(is_word_char) || (token.starts_with('[') && token.ends_with(']') && token.len() >= 2)
}
fn wordlike(token: &str) -> bool {
    token.chars().any(is_word_char)
}

fn internal_omission(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(at, token)| (matches!(token.as_str(), "..." | "\u{2026}")
        || token == "." && tokens.get(at + 1).is_some_and(|token| token == ".")
            && tokens.get(at + 2).is_some_and(|token| token == "."))
        && tokens[..at].iter().any(|token| wordlike(token))
        && tokens[at + 1..].iter().any(|token| wordlike(token)))
}

/// Words, bracketed insertions, ellipses, quotation marks and punctuation; adjoining words merge.
fn quote_tokens(text: &str) -> Vec<Token> {
    let raw = TOKEN.find_iter(text).map(|found| Token { text: found.as_str().to_owned(), start: found.start(),
        end: found.end() }).collect::<Vec<_>>();
    let mut merged = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let mut token = raw[i].clone();
        let mut next = i + 1;
        while next < raw.len() && raw[next - 1].end == raw[next].start && mergeable(&raw[next - 1].text)
            && mergeable(&raw[next].text) {
            token.text.push_str(&raw[next].text);
            token.end = raw[next].end;
            next += 1;
        }
        merged.push(token);
        i = next;
    }
    merged
}

impl Reader {
    fn tokens(&mut self, text: &str) -> std::rc::Rc<Vec<String>> {
        if let Some(found) = self.tokens.get(text) { return found.clone(); }
        // Pure readings, so forgetting them changes nothing: a region search reads a new window at every step.
        if self.tokens.len() >= 8192 { self.tokens.clear(); }
        let value = std::rc::Rc::new(quote_tokens(text).into_iter().map(|token| token.text).collect::<Vec<_>>());
        self.tokens.insert(text.to_owned(), value.clone());
        value
    }
    fn exact_key(&mut self, text: &str) -> String {
        if let Some(found) = self.exact.get(text) { return found.clone(); }
        if self.exact.len() >= 8192 { self.exact.clear(); }
        let value = quote_exact_key(text).to_lowercase();
        self.exact.insert(text.to_owned(), value.clone());
        value
    }
}

/// Lower-cased words, as the plausibility and region checks count them.
fn quote_words(text: &str) -> Vec<String> {
    WORD.find_iter(text).map(|found| found.as_str().to_lowercase()).collect()
}

fn strip_trailing(values: &[String]) -> Vec<String> {
    let mut end = values.len();
    while end > 0 && TRAILING.contains(&values[end - 1].as_str()) { end -= 1; }
    values[..end].to_vec()
}

/// "[T]he" read as "The".
fn collapse_initial_case(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() >= 4 && chars[0] == '[' && chars[1].is_ascii_alphabetic() && chars[2] == ']' && chars[3].is_ascii_alphabetic() {
        let mut out = String::new();
        out.push(chars[1]);
        out.extend(&chars[3..]);
        return out;
    }
    text.to_owned()
}

/// Smart quotes, dashes, joined words, outer quotation marks and trailing punctuation are not wording.
pub fn normalize_quote_for_compare(text: &str) -> String {
    let value = js_trim(text);
    if value.is_empty() { return String::new(); }
    let value = value.chars().map(|c| match c {
        '\u{201c}' | '\u{201d}' => '"',
        '\u{2019}' | '\u{2018}' => '\'',
        c if is_dash(c) => '-',
        c => c,
    }).collect::<Vec<char>>();
    // A hyphen joining two words, spaced or not, is a space: (?<=[\p{L}\p{N}])\s*-\s*(?=[\p{L}\p{N}]).
    let mut joined = String::new();
    let mut i = 0;
    while i < value.len() {
        if i > 0 && is_word_char(value[i - 1]) {
            let mut j = i;
            while j < value.len() && js_space(value[j]) { j += 1; }
            if j < value.len() && value[j] == '-' {
                let mut k = j + 1;
                while k < value.len() && js_space(value[k]) { k += 1; }
                if k < value.len() && is_word_char(value[k]) {
                    joined.push(' ');
                    i = k;
                    continue;
                }
            }
        }
        joined.push(value[i]);
        i += 1;
    }
    let collapsed = WHITESPACE.replace_all(&joined, " ").into_owned();
    let mut value = js_trim(&collapsed).to_owned();
    let chars = value.chars().collect::<Vec<_>>();
    let quote = |c: char| DOUBLE_QUOTES.contains(&c.to_string().as_str()) || SINGLE_QUOTES.contains(&c.to_string().as_str());
    if chars.len() >= 2 && quote(chars[0]) && quote(chars[chars.len() - 1])
        && utf16_len(&chars[0].to_string()) == 1 && utf16_len(&chars[chars.len() - 1].to_string()) == 1 {
        value = js_trim(&chars[1..chars.len() - 1].iter().collect::<String>()).to_owned();
    }
    // A closing quotation mark before the sentence's punctuation: (?<=[A-Za-z0-9])"\s*(?=[.,;:!?…]).
    let chars = value.chars().collect::<Vec<_>>();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' && i > 0 && chars[i - 1].is_ascii_alphanumeric() {
            let mut j = i + 1;
            while j < chars.len() && js_space(chars[j]) { j += 1; }
            if j < chars.len() && ".,;:!?\u{2026}".contains(chars[j]) { i = j; continue; }
        }
        out.push(chars[i]);
        i += 1;
    }
    let out = out.trim_end_matches(|c: char| ".,;:!?\u{2026}".contains(c));
    js_trim(out).to_owned()
}

pub fn quote_exact_key(text: &str) -> String {
    collapse_initial_case(&normalize_quote_for_compare(text))
}

/// Where `needle` first stands in `haystack`, in UTF-16 units: a single word as a whole word, case aside.
fn find_exact(needle: &str, haystack: &str) -> Option<usize> {
    let needle = js_trim(needle);
    if needle.is_empty() || haystack.is_empty() { return None; }
    if SINGLE_WORD.is_match(needle) {
        // A word is searched for at every step of a region search: its pattern is compiled once.
        thread_local!(static WORDS: std::cell::RefCell<HashMap<String, Regex>> = Default::default());
        return WORDS.with(|words| {
            let mut words = words.borrow_mut();
            if words.len() >= 4096 { words.clear(); }
            let pattern = words.entry(needle.to_owned()).or_insert_with(||
                Regex::new(&format!("(?i){}", regex::escape(needle))).expect("an escaped word is a pattern"));
            let mut from = 0;
            while let Some(found) = pattern.find_at(haystack, from) {
                let before = haystack[..found.start()].chars().next_back();
                let after = haystack[found.end()..].chars().next();
                if !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char) {
                    return Some(byte_to_utf16(haystack, found.start()));
                }
                from = found.start() + haystack[found.start()..].chars().next().map_or(1, char::len_utf8);
            }
            None
        });
    }
    let lower = haystack.to_lowercase().encode_utf16().collect::<Vec<_>>();
    let needle = needle.to_lowercase().encode_utf16().collect::<Vec<_>>();
    index_of16(&lower, &needle, 0)
}

fn exact_in_source(quote: &str, source: &str) -> bool {
    if find_exact(quote, source).is_some() { return true; }
    let (q, s) = (collapse_initial_case(quote), collapse_initial_case(source));
    (q != quote || s != source) && find_exact(&q, &s).is_some()
}

fn equivalent_token(token: &str) -> String {
    if DOUBLE_QUOTES.contains(&token) { return "\"".into(); }
    if SINGLE_QUOTES.contains(&token) { return "'".into(); }
    let mut chars = token.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if is_dash(c) { return "-".into(); }
    }
    let mut value = token.chars().map(|c| match c {
        '\u{2019}' | '\u{2018}' => '\'',
        c if is_dash(c) => '-',
        c => c,
    }).collect::<String>();
    if let Some(found) = INITIAL.captures(&value) { value = format!("{}{}", &found[1], &found[2]); }
    if PLAIN_WORD.is_match(&value) { value.to_lowercase() } else { value }
}
fn alignment_aliases(values: &[String]) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for token in values {
        let equivalent = equivalent_token(token);
        let Some(found) = ALIASED.captures(&equivalent) else { continue };
        let base = found[1].to_lowercase();
        let expanded = format!("{}{}", base, found[2].replace(['[', ']'], "").to_lowercase());
        let joined = format!("{base}|{expanded}");
        aliases.insert(base, joined.clone());
        aliases.insert(expanded, joined);
    }
    aliases
}
fn alignment_token(token: &str, aliases: &HashMap<String, String>) -> String {
    let mut value = equivalent_token(token);
    if BRACKETED_SUFFIX.is_match(&value) { value = BRACKET_GROUP.replace_all(&value, "").into_owned(); }
    let lowered = value.to_lowercase();
    aliases.get(&lowered).cloned().unwrap_or(lowered)
}

impl Reader {
    fn bracket_suffix_exact(&mut self, quote: &str, source: &str) -> bool {
        let quoted = strip_trailing(&self.tokens(quote));
        let aliases = alignment_aliases(&quoted);
        if aliases.is_empty() { return false; }
        bracket_suffix_in(&quoted, &aliases, &self.tokens(source))
    }
}
/// The quotation's words, a word with a bracketed suffix read either way, run in order in the source's tokens.
fn bracket_suffix_in(quoted: &[String], aliases: &HashMap<String, String>, source: &[String]) -> bool {
    if aliases.is_empty() { return false; }
    let needle = quoted.iter().filter(|token| wordlike(token)).map(|token| alignment_token(token, aliases))
        .collect::<Vec<_>>();
    let words = source.iter().filter(|token| wordlike(token)).map(|token| alignment_token(token, aliases))
        .collect::<Vec<_>>();
    if needle.is_empty() || needle.len() > words.len() { return false; }
    (0..=words.len() - needle.len()).any(|at| words[at..at + needle.len()] == needle[..])
}

/// Python difflib's matching blocks without junk, as sequence-diff.mjs computes them: the longest common run first,
/// then each side of it, as (tag, a0, a1, b0, b1).
pub fn sequence_opcodes<T: PartialEq>(left: &[T], right: &[T]) -> Vec<(&'static str, usize, usize, usize, usize)> {
    if left.len().saturating_mul(right.len()) > MAX_SEQUENCE_WORK {
        return vec![("replace", 0, left.len(), 0, right.len())];
    }
    let mut blocks: Vec<(usize, usize, usize)> = Vec::new();
    let mut pending = vec![(0, left.len(), 0, right.len())];
    let mut prior = vec![0usize; right.len() + 1];
    let mut current = vec![0usize; right.len() + 1];
    while let Some((a0, a1, b0, b1)) = pending.pop() {
        let mut best = (a0, b0, 0usize);
        for slot in &mut prior[b0..=b1] { *slot = 0; }
        for i in a0..a1 {
            for j in b0..b1 {
                current[j + 1] = if left[i] == right[j] {
                    let size = prior[j] + 1;
                    if size > best.2 { best = (i + 1 - size, j + 1 - size, size); }
                    size
                } else { 0 };
            }
            std::mem::swap(&mut prior, &mut current);
            prior[b0] = 0;
        }
        let (i, j, size) = best;
        if size == 0 { continue; }
        blocks.push(best);
        if a0 < i && b0 < j { pending.push((a0, i, b0, j)); }
        if i + size < a1 && j + size < b1 { pending.push((i + size, a1, j + size, b1)); }
    }
    blocks.sort();
    let mut merged: Vec<(usize, usize, usize)> = Vec::new();
    for block in blocks {
        match merged.last_mut() {
            Some(last) if last.0 + last.2 == block.0 && last.1 + last.2 == block.1 => last.2 += block.2,
            _ => merged.push(block),
        }
    }
    let mut result = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (next_i, next_j, size) in merged.into_iter().chain([(left.len(), right.len(), 0)]) {
        if i < next_i || j < next_j {
            let tag = if i < next_i && j < next_j { "replace" } else if i < next_i { "delete" } else { "insert" };
            result.push((tag, i, next_i, j, next_j));
        }
        if size > 0 { result.push(("equal", next_i, next_i + size, next_j, next_j + size)); }
        i = next_i + size;
        j = next_j + size;
    }
    result
}
fn matched_size<T: PartialEq>(left: &[T], right: &[T]) -> usize {
    sequence_opcodes(left, right).into_iter().filter(|(tag, ..)| *tag == "equal").map(|(_, a0, a1, ..)| a1 - a0).sum()
}

fn anchor_phrases(words: &[String]) -> Vec<Vec<String>> {
    let mut phrases = Vec::new();
    let mut seen = HashSet::new();
    for size in [8usize, 6, 5, 4, 3] {
        if words.len() < size { continue; }
        for start in [0, words.len() / 4, words.len() / 2, words.len().saturating_sub(size)] {
            let start = start.min(words.len() - size);
            let phrase = words[start..start + size].to_vec();
            if !phrase.is_empty() && seen.insert(phrase.join("\u{0}")) { phrases.push(phrase); }
        }
    }
    phrases
}
fn find_phrase(words: &[String], phrase: &[String]) -> Option<usize> {
    let mut at = words.iter().position(|word| *word == phrase[0])?;
    loop {
        if at + phrase.len() > words.len() { return None; }
        if words[at..at + phrase.len()] == *phrase { return Some(at); }
        at += 1 + words[at + 1..].iter().position(|word| *word == phrase[0])?;
    }
}
fn long_words(source: &str) -> LongWords {
    let words = quote_words(source);
    let present = words.iter().cloned().collect();
    (words, present)
}
fn long_source_score(quoted: &[String], (words, present): &LongWords) -> f64 {
    let needle = quoted.iter().filter(|token| wordlike(token)).map(|token| token.to_lowercase()).collect::<Vec<_>>();
    if needle.is_empty() { return 0.0; }
    if words.is_empty() { return 0.0; }
    let overlap = needle.iter().filter(|word| present.contains(word.as_str())).count() as f64 / needle.len() as f64;
    if overlap < 0.6 { return overlap; }
    let mut best = 0.0f64;
    let mut seen = HashSet::new();
    for phrase in anchor_phrases(&needle) {
        let Some(at) = find_phrase(&words, &phrase) else { continue };
        let start = at.saturating_sub(needle.len());
        let end = words.len().min(at + phrase.len() + needle.len() * 3);
        if !seen.insert((start, end)) { continue; }
        best = best.max(matched_size(&words[start..end], &needle) as f64 / needle.len() as f64);
        if best >= 0.98 { break; }
    }
    if best == 0.0 { overlap.min(0.59) } else { best }
}

impl Reader {
    /// Fraction of the quotation's tokens the source contains in order; 1 when it contains it exactly. Given
    /// `floor`, a score that cannot reach it is reported as 0 without aligning.
    fn quote_match_score(&mut self, quote: &str, source: &str, floor: f64) -> f64 {
        let (q, s) = (js_trim(quote), js_trim(source));
        if q.is_empty() || s.is_empty() { return 0.0; }
        let quoted = strip_trailing(&self.tokens(q));
        if quoted.is_empty() { return 0.0; }
        let key = self.exact_key(q);
        if !key.is_empty() && exact_in_source(&key, &self.exact_key(s)) { return 1.0; }
        if self.bracket_suffix_exact(q, s) { return 1.0; }
        if utf16_len(s) > LONG_SOURCE_CHARS { let words = self.long_words(s); return long_source_score(&quoted, &words); }
        let source_tokens = self.tokens(s);
        if source_tokens.is_empty() { return 0.0; }
        if source_tokens.len() > LONG_SOURCE_TOKENS { let words = self.long_words(s); return long_source_score(&quoted, &words); }
        if floor > 0.0 {
            let mut left = HashMap::<&str, usize>::new();
            for token in source_tokens.iter() { *left.entry(token.as_str()).or_default() += 1; }
            let mut shared = 0usize;
            for token in &quoted {
                if let Some(count) = left.get_mut(token.as_str()).filter(|count| **count > 0) { shared += 1; *count -= 1; }
            }
            if (shared as f64) / (quoted.len() as f64) < floor { return 0.0; }
        }
        matched_size(&source_tokens, &quoted) as f64 / quoted.len() as f64
    }

    /// quote_match_score of a window of the source's tokens joined by spaces, scored from the tokens themselves (the
    /// joined text tokenizes back to them, short of a lone "[" a later "]" could now close), or any score no more than
    /// `best` where it cannot exceed it: the tokens a window shares with the quotation bound its alignment.
    fn window_score(&mut self, q: &str, windows: &Windows, start: usize, end: usize, best: f64) -> f64 {
        let window = &windows.source[start..end];
        // The joined text trims to itself unless its first or last token begins or ends with a blank.
        if window.first().is_none_or(|token| token.starts_with(js_space)) || window.last().is_none_or(|token| token.ends_with(js_space))
            || window.iter().any(|token| token == "[") {
            return self.quote_match_score(q, &window.join(" "), 0.0);
        }
        if windows.core.is_empty() { return 0.0; }
        if windows.may_hold_exactly(start, end) && exact_in_source(&windows.key, &quote_exact_key(&window.join(" ")).to_lowercase()) {
            return 1.0;
        }
        if let Some((needle, aligned)) = &windows.aligned {
            let words = aligned[start..end].iter().flatten().collect::<Vec<_>>();
            if !needle.is_empty() && needle.len() <= words.len()
                && (0..=words.len() - needle.len()).any(|at| words[at..at + needle.len()].iter().zip(needle).all(|(a, b)| *a == b)) {
                return 1.0;
            }
        }
        if window.iter().map(|token| utf16_len(token)).sum::<usize>() + window.len() - 1 > LONG_SOURCE_CHARS
            || window.len() > LONG_SOURCE_TOKENS {
            return self.quote_match_score(q, &window.join(" "), 0.0);
        }
        let core = &windows.core;
        // The tokens the window shares with the quotation, each counted as often as both hold it.
        let mut seen = vec![0usize; windows.wanted.len()];
        let mut shared = 0usize;
        for &slot in &windows.slots[start..end] {
            if slot != u32::MAX && seen[slot as usize] < windows.wanted[slot as usize] { seen[slot as usize] += 1; shared += 1; }
        }
        if shared as f64 / core.len() as f64 <= best { return 0.0; }
        matched_size(window, core) as f64 / core.len() as f64
    }
}

/// Where each token starts in the text (UTF-16), found in order in its lower-cased form.
fn token_positions(text: &str, source: &[String]) -> Vec<usize> {
    let lower = text.to_lowercase().encode_utf16().collect::<Vec<_>>();
    let mut positions = Vec::with_capacity(source.len());
    let mut from = 0usize;
    for token in source.iter() {
        let needle = token.to_lowercase().encode_utf16().collect::<Vec<_>>();
        let at = index_of16(&lower, &needle, from).unwrap_or(from);
        positions.push(at);
        from = at + 1.max(utf16_len(token));
    }
    positions
}

/// What every window of a source's tokens is checked against for one quotation, prepared once.
struct Windows {
    source: std::rc::Rc<Vec<String>>,
    /// The quotation's tokens short of trailing punctuation, and its exact key.
    core: Vec<String>,
    key: String,
    /// How many of the source's first n tokens could hold the key's anchor word (see exact_anchor); None where the key
    /// has none, and every window is searched.
    anchored: Option<Vec<usize>>,
    /// The quotation's words and each source token's word as a bracketed suffix aligns them, where it has such a suffix.
    aligned: Option<(Vec<String>, Vec<Option<String>>)>,
    /// How often the quotation holds each of its distinct tokens, and which of them each source token is (u32::MAX: none).
    wanted: Vec<usize>,
    slots: Vec<u32>,
}

impl Windows {
    fn new(source: std::rc::Rc<Vec<String>>, quoted: &[String], key: String) -> Self {
        let core = strip_trailing(quoted);
        let anchored = exact_anchor(&key).map(|anchor| {
            let mut counts = Vec::with_capacity(source.len() + 1);
            counts.push(0);
            for token in source.iter() {
                let holds = !token.is_ascii() || token.split(|c: char| !c.is_ascii_alphanumeric()).any(|run| run.eq_ignore_ascii_case(&anchor));
                counts.push(counts.last().unwrap() + usize::from(holds));
            }
            counts
        });
        let aliases = alignment_aliases(&core);
        let aligned = (!aliases.is_empty()).then(|| (
            core.iter().filter(|token| wordlike(token)).map(|token| alignment_token(token, &aliases)).collect(),
            source.iter().map(|token| wordlike(token).then(|| alignment_token(token, &aliases))).collect()));
        let mut distinct = HashMap::<&str, u32>::new();
        let mut wanted = Vec::new();
        for token in &core {
            let slot = *distinct.entry(token.as_str()).or_insert_with(|| { wanted.push(0); (wanted.len() - 1) as u32 });
            wanted[slot as usize] += 1;
        }
        let slots = source.iter().map(|token| distinct.get(token.as_str()).copied().unwrap_or(u32::MAX)).collect();
        Windows { source, core, key, anchored, aligned, wanted, slots }
    }

    /// Whether the window's text could hold the quotation's key word for word: it holds the key's anchor word, or it
    /// begins where a "[T]he" read as "The" could change what it holds.
    fn may_hold_exactly(&self, start: usize, end: usize) -> bool {
        if self.key.is_empty() { return false; }
        let Some(counts) = &self.anchored else { return true };
        counts[end] > counts[start] || self.source[start..end.min(start + 3)].iter().any(|token| token.contains('['))
    }
}

/// A word any text holding `key` word for word (exact_in_source) holds whole, as one of its tokens' runs of letters and
/// digits: the key's longest run with a letter or digit on neither side (any run of a single word, which is matched
/// whole), where that run is ASCII. None where there is none, or the key begins with "[", which "[T]he" read as "The"
/// joins to the run after it.
fn exact_anchor(key: &str) -> Option<String> {
    if key.is_empty() || key.starts_with('[') { return None; }
    let single = SINGLE_WORD.is_match(js_trim(key));
    let chars = key.chars().collect::<Vec<_>>();
    let mut best: Option<String> = None;
    let mut i = 0;
    while i < chars.len() {
        if !is_word_char(chars[i]) { i += 1; continue; }
        let start = i;
        while i < chars.len() && is_word_char(chars[i]) { i += 1; }
        let run = chars[start..i].iter().collect::<String>();
        if (single || (start > 0 && i < chars.len())) && run.is_ascii() && best.as_ref().is_none_or(|kept| run.len() > kept.len()) {
            best = Some(run);
        }
    }
    best
}

fn content_words(text: &str) -> Vec<String> {
    quote_words(text).into_iter().filter(|word| !NON_CONTENT.contains(&word.as_str()) && !NON_CONTENT_MORE.contains(&word.as_str())
        && !(utf16_len(word) == 1 && word.chars().all(char::is_alphabetic))).collect()
}
/// A partial match is plausible only when enough of the quotation's content words occur in the source.
fn plausible_content_overlap(quote: &str, source: &str) -> bool {
    let mut needle = Vec::new();
    for word in content_words(quote) { if !needle.contains(&word) { needle.push(word); } }
    if needle.is_empty() { return true; }
    let present = content_words(source).into_iter().collect::<HashSet<_>>();
    if present.is_empty() { return false; }
    let overlap = needle.iter().filter(|word| present.contains(*word)).count();
    if overlap == 0 { return false; }
    if needle.len() <= 2 { return true; }
    if needle.len() <= 4 { return overlap >= 2; }
    overlap as f64 / needle.len() as f64 >= 0.35
}

fn long_source_region(quote: &str, text: &str, window: usize) -> String {
    let words = quote_words(quote);
    let units = text.encode_utf16().collect::<Vec<_>>();
    for phrase in anchor_phrases(&words) {
        let pattern = format!("(?i){}", phrase.iter().map(|word| regex::escape(word)).collect::<Vec<_>>()
            .join(r"[^\p{L}\p{N}_]+"));
        let Ok(pattern) = Regex::new(&pattern) else { continue };
        let mut from = 0;
        while let Some(found) = pattern.find_at(text, from) {
            let before = text[..found.start()].chars().next_back();
            let after = text[found.end()..].chars().next();
            let edge = |c: Option<char>| c.is_some_and(|c| is_word_char(c) || c == '_');
            if !edge(before) && !edge(after) {
                let at = byte_to_utf16(text, found.start());
                let length = utf16_len(found.as_str());
                return slice16(&units, at.saturating_sub(window), units.len().min(at + length + utf16_len(quote) + window));
            }
            from = found.start() + text[found.start()..].chars().next().map_or(1, char::len_utf8);
        }
    }
    String::new()
}

impl Reader {
    /// The window of the source where the quotation best matches, so alignment never spans distant text.
    fn quote_region(&mut self, quote: &str, full_text: &str, window: usize) -> String {
        let (q, text) = (js_trim(quote), js_trim(full_text));
        if q.is_empty() || text.is_empty() { return full_text.to_owned(); }
        // An explicit omission can put valid quoted words beyond any fixed local window.
        let quoted = self.tokens(q);
        if utf16_len(text) <= LONG_SOURCE_CHARS && internal_omission(&quoted)
            && self.tokens(text).len().saturating_mul(quoted.len()) <= MAX_SEQUENCE_WORK { return text.to_owned(); }
        let q_units = q.encode_utf16().collect::<Vec<_>>();
        let head = slice16(&q_units, 0, 40);
        let (local, whole);
        // The whole quotation where it stands as written, else its opening: an opening several passages share
        // finds only the first of them.
        let needles = if q_units.len() > 40 { vec![q.to_owned(), head] } else { vec![head] };
        let (units, found): (&[u16], Option<usize>) = if self.is_whole(text) {
            whole = self.whole_units.get_or_insert_with(|| std::rc::Rc::new((text.encode_utf16().collect(),
                text.to_lowercase().encode_utf16().collect()))).clone();
            // find_exact, the document's lower-cased text read once.
            let found = needles.iter().find_map(|needle| {
                let needle = js_trim(needle);
                if needle.is_empty() { None } else if SINGLE_WORD.is_match(needle) { find_exact(needle, text) }
                    else { index_of16(&whole.1, &needle.to_lowercase().encode_utf16().collect::<Vec<_>>(), 0) }
            });
            (&whole.0, found)
        } else {
            local = text.encode_utf16().collect::<Vec<_>>();
            (&local, needles.iter().find_map(|needle| find_exact(needle, text)))
        };
        if let Some(at) = found {
            return slice16(&units, at.saturating_sub(window / 2), units.len().min(at + q_units.len() + window / 2));
        }
        if units.len() > LONG_SOURCE_CHARS { return long_source_region(q, text, window); }
        if quoted.is_empty() { return text.to_owned(); }
        let step = 1.max(quoted.len() / 2);
        let source = self.tokens(text);
        let positions = match (&self.whole_positions, self.is_whole(text)) {
            (Some(positions), true) => positions.clone(),
            _ => {
                let positions = std::rc::Rc::new(token_positions(text, &source));
                if self.is_whole(text) { self.whole_positions = Some(positions.clone()); }
                positions
            }
        };
        let (mut best, mut best_at) = (0.0f64, 0usize);
        let windows = Windows::new(source.clone(), &quoted, self.exact_key(q));
        let mut i = 0;
        // A window can score no more than 1, so the first that does is the best.
        while i + quoted.len() <= source.len() && best < 1.0 {
            let score = self.window_score(q, &windows, i, source.len().min(i + quoted.len() * 2), best);
            if score > best { best = score; best_at = i; }
            i += step;
        }
        if source.is_empty() { return text.to_owned(); }
        let first = best_at.saturating_sub(quoted.len());
        let last = (source.len() - 1).min(best_at + quoted.len() * 3);
        slice16(&units, positions[first], positions[last] + utf16_len(&source[last]))
    }

    fn only_bracketed(&mut self, text: &str) -> bool {
        let meaningful = strip_trailing(&self.tokens(text)).into_iter().filter(|token| !DOUBLE_QUOTES.contains(&token.as_str())
            && !SINGLE_QUOTES.contains(&token.as_str()) && !TRAILING.contains(&token.as_str()) && !js_trim(token).is_empty())
            .collect::<Vec<_>>();
        meaningful.len() == 1 && ONLY_BRACKET.is_match(&meaningful[0])
    }

    /// The source's own words that correspond to the quotation, for links and display.
    fn source_side_quote_fragment(&mut self, source_region: &str, quote: &str, max_chars: usize) -> String {
        let spans = quote_tokens(source_region);
        let quoted = self.tokens(quote);
        if spans.is_empty() || quoted.is_empty() { return String::new(); }
        // The quoted words where the source has them all, as the comparison finds them; the closest otherwise.
        let plan = legal_document_query::text_fragment_plan(source_region, None, &[quote.to_owned()], false, false, false);
        if plan.source_safe_complete && !plan.source_word_intervals.is_empty() {
            let mut start = plan.source_word_intervals.iter().map(|span| span.start).min().unwrap();
            let mut end = plan.source_word_intervals.iter().map(|span| span.end).max().unwrap();
            let units = source_region.encode_utf16().collect::<Vec<_>>();
            // A source's bracket the words begin or end inside is kept whole.
            let (open, close) = (u16::from(b'['), u16::from(b']'));
            let count = |unit: u16, start: usize, end: usize| units[start..end].iter().filter(|&&found| found == unit).count();
            if count(close, start, end) > count(open, start, end) && start > 0 && units[start - 1] == open { start -= 1; }
            if count(open, start, end) > count(close, start, end) && units.get(end) == Some(&close) { end += 1; }
            // The source's punctuation after the words only as far as the quotation carries punctuation there.
            let ending = js_trim(quote).chars().rev().take_while(|c| !is_word_char(*c)).collect::<String>();
            let tail = slice16(&units, end, units.len()).chars().take_while(|c| ending.contains(*c)).collect::<String>();
            let exact = format!("{}{tail}", slice16(&units, start, end));
            return if utf16_len(&exact) <= max_chars { exact } else { String::new() };
        }
        let omission = internal_omission(&quoted);
        let source = spans.iter().map(|span| span.text.clone()).collect::<Vec<_>>();
        let aliases = alignment_aliases(&quoted);
        let left = source.iter().map(|token| alignment_token(token, &aliases)).collect::<Vec<_>>();
        let right = quoted.iter().map(|token| alignment_token(token, &aliases)).collect::<Vec<_>>();
        let mut ranges = Vec::new();
        for (tag, i1, i2, j1, j2) in sequence_opcodes(&left, &right) {
            if (tag != "equal" && tag != "replace") || i2 <= i1 || j2 <= j1 { continue; }
            let source_words = source[i1..i2].iter().filter(|token| wordlike(token)).count();
            let quote_words_count = quoted[j1..j2].iter().filter(|token| wordlike(token)).count();
            if tag == "replace" && (source_words > quote_words_count + 2 || quote_words_count > source_words + 2) { continue; }
            if source_words == 0 || quote_words_count == 0 { continue; }
            ranges.push((i1, i2));
        }
        if ranges.is_empty() { return String::new(); }
        let mut clusters: Vec<Vec<(usize, usize)>> = Vec::new();
        for range in ranges {
            match clusters.last_mut() {
                Some(cluster) if omission || range.0 as i64 - cluster.last().unwrap().1 as i64 <= 8 => cluster.push(range),
                _ => clusters.push(vec![range]),
            }
        }
        let score = |cluster: &Vec<(usize, usize)>| {
            let (start, end) = (cluster[0].0, cluster.last().unwrap().1);
            (source[start..end].iter().filter(|token| wordlike(token)).count() as i64, -((end - start) as i64))
        };
        let mut best = &clusters[0];
        for cluster in &clusters[1..] {
            let ((a, b), (c, d)) = (score(cluster), score(best));
            if a > c || (a == c && b > d) { best = cluster; }
        }
        let (start, end) = (best[0].0, best.last().unwrap().1);
        if end <= start { return String::new(); }
        let raw = &source_region[spans[start].start..spans[end - 1].end];
        let mut fragment = js_trim(&WHITESPACE.replace_all(raw, " ")).to_owned();
        let first_word = quote_words(quote).into_iter().next();
        if !first_word.is_some_and(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_digit())) {
            let stripped = js_trim(&LEADING_NUMBER.replace(&fragment, "")).to_owned();
            if !stripped.is_empty() { fragment = stripped; }
        }
        if fragment.is_empty() || self.only_bracketed(&fragment) || utf16_len(&fragment) > max_chars { return String::new(); }
        fragment
    }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum QuoteBlockKind {
    Paragraph,
    Page,
    Section,
}

/// A source's paragraph, page or provision, by its offsets (UTF-16) in the source's text.
#[derive(Clone, Deserialize)]
pub struct QuoteBlockInput {
    /// Actual reflected identity, absent for raw membership-only inputs.
    pub node_id: Option<String>,
    #[serde(default)]
    pub ancestor_ids: Vec<String>,
    pub ranges: Vec<legal_structure_model::ScalarRange>,
    pub kind: QuoteBlockKind,
    pub label: String,
    pub start: usize,
    pub end: usize,
}


/// Prepare quotation locators from the document's own reflected structure. A judgment PDF
/// uses actual printed paragraph numbers, or page/provision locators when absent.
/// Repeated locators retain their source candidates but do not resolve unqualified citations.
pub fn document_quote_blocks(
    document: &legal_structure_model::DocumentStructure,
    query: &legal_document_query::DocumentQuery,
    judgment: bool,
) -> Vec<QuoteBlockInput> {
    use legal_document_query::DocumentKind;
    use legal_structure_model::{parse_ordinal, NodeKind};
    let blocks = query.blocks(document, None).collect::<Vec<_>>();
    let nodes = document.nodes.iter().map(|node| (node.id.as_str(), node)).collect::<HashMap<_, _>>();
    let ancestors = |id: Option<&str>| {
        let mut current = id.and_then(|id| nodes.get(id)).and_then(|node| node.parent_id.as_deref());
        let mut seen = id.into_iter().collect::<HashSet<_>>();
        let mut ancestors = Vec::new();
        while let Some(id) = current {
            if !seen.insert(id) { break; }
            ancestors.push(id.to_owned());
            current = nodes.get(id).and_then(|node| node.parent_id.as_deref());
        }
        ancestors
    };
    let mut printed = Vec::new();
    if judgment {
        let mut counts = HashMap::new();
        let numbered = blocks.iter().filter_map(|block| {
            let node = nodes.get(block.node_id.as_deref()?)?;
            if node.kind != NodeKind::Paragraph { return None; }
            let number = parse_ordinal("paragraph", node.label.as_deref()?)?;
            *counts.entry(number).or_insert(0usize) += 1;
            Some((number, block))
        }).collect::<Vec<_>>();
        let has_unique = counts.values().any(|count| *count == 1);
        printed = numbered.into_iter()
            .map(|(number, block)| QuoteBlockInput { node_id: block.node_id.clone(),
                ancestor_ids: ancestors(block.node_id.as_deref()), kind: QuoteBlockKind::Paragraph,
                label: format!("par{number}"), start: block.start, end: block.end, ranges: block.ranges.clone() })
            .collect::<Vec<_>>();
        printed.sort_by_key(|block| (block.start, block.end));
        if has_unique { return printed; }
    }
    static PARAGRAPH: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?i)^(?:paras?\.?|par|¶)[{JS_SPACE}]*([0-9])")).unwrap());
    static PAGE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?i)^(?:pages?|pp?\.?)[{JS_SPACE}]*([0-9])")).unwrap());
    static SECTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r"(?i)^(?:sections?|ss?\.|sec)[{JS_SPACE}]*([0-9])")).unwrap());
    printed.extend(blocks.into_iter().filter_map(|block| {
        let (kind, prefix) = match block.kind {
            DocumentKind::Paragraph if !judgment => (QuoteBlockKind::Paragraph, &*PARAGRAPH),
            DocumentKind::Page => (QuoteBlockKind::Page, &*PAGE),
            DocumentKind::Section => (QuoteBlockKind::Section, &*SECTION),
            _ => return None,
        };
        let normalized = prefix.replace(js_trim(&block.label), "$1");
        let mut value = normalized.as_ref();
        if value.starts_with('[') && value.ends_with(']') { value = &value[1..value.len()-1]; }
        if kind == QuoteBlockKind::Section && value.strip_prefix("sec")
            .and_then(|rest| rest.chars().next()).is_some_and(|c| c.is_ascii_uppercase()) {
            value = &value[3..];
        }
        let label = match kind {
            QuoteBlockKind::Paragraph => format!("par{value}"),
            QuoteBlockKind::Page => format!("page {value}"),
            QuoteBlockKind::Section => format!("sec{value}"),
        };
        Some(QuoteBlockInput { ancestor_ids: ancestors(block.node_id.as_deref()), node_id: block.node_id,
            kind, label, start: block.start, end: block.end, ranges: block.ranges })
    }));
    printed
}

#[derive(Clone)]
struct Block {
    node_id: Option<String>,
    ancestor_ids: Vec<String>,
    ranges: Vec<legal_structure_model::ScalarRange>,
    kind: QuoteBlockKind,
    label: String,
    start: usize,
    end: usize,
    text: String,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct QuoteScopes {
    pub paragraphs: Vec<(f64, f64)>,
    pub pages: Vec<(f64, f64)>,
    pub sections: Vec<String>,
}

#[derive(Deserialize)]
pub struct QuoteToLocate {
    pub quote: String,
    #[serde(default)]
    pub scopes: QuoteScopes,
    /// The quotation's id, which a host's checks of it name the evidence by.
    #[serde(default)]
    pub id: Option<String>,
}

/// A located quotation's part in the passage it was found in: the region of the passage, the source's words for the
/// quotation, and the quotation corrected to them.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QuoteCorrection {
    pub region: String,
    pub fragment: String,
    pub corrected: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CitedPartialMatch {
    pub score: f64,
    pub labels: Vec<String>,
    pub pinpoint: String,
    #[serde(flatten)]
    pub correction: QuoteCorrection,
}

/// Where a quotation is in its source: `location` is "unmatched" where it is not found. `perfect`: found word for
/// word where the citation sends a reader.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteLocation {
    pub location: String,
    pub score: f64,
    pub labels: Vec<String>,
    pub pinpoint: String,
    pub text: String,
    #[serde(flatten)]
    pub correction: QuoteCorrection,
    pub perfect: bool,
    /// A short quotation in a long source is only ever searched for word for word.
    pub short_in_long: bool,
    pub cited: Option<CitedPartialMatch>,
}

/// A quotation's place in its source and how its words differ from the source's there.
#[derive(Serialize)]
pub struct LocatedQuote {
    #[serde(rename = "match")]
    pub found: QuoteLocation,
    /// None where the caller asked for the location alone.
    pub comparison: Option<QuoteComparison>,
}

fn js_number(text: &str) -> f64 {
    let value = js_trim(text);
    if value.is_empty() { return 0.0; }
    value.parse::<f64>().ok().filter(|_| !value.chars().any(|c| c.is_alphabetic() && c != 'e' && c != 'E'))
        .unwrap_or(f64::NAN)
}
fn block_number(block: &Block) -> f64 {
    match block.kind {
        QuoteBlockKind::Paragraph => js_number(block.label.get(3..).unwrap_or("")),
        _ => js_number(block.label.split(' ').next_back().unwrap_or("")),
    }
}
fn in_ranges(value: f64, ranges: &[(f64, f64)]) -> bool {
    ranges.iter().any(|&(a, b)| a <= value && value <= b)
}
fn has_scopes(scopes: &QuoteScopes) -> bool {
    !scopes.paragraphs.is_empty() || !scopes.pages.is_empty() || !scopes.sections.is_empty()
}
fn section_ancestors(label: &str) -> Vec<String> {
    let mut out = vec![label.to_owned()];
    let mut current = label.to_owned();
    while SECTION_TAIL.is_match(&current) {
        current = SECTION_TAIL.replace(&current, "").into_owned();
        out.push(current.clone());
    }
    out
}
fn word_count(quote: &str, text: &str) -> usize {
    let (needle, words) = (quote_words(quote), quote_words(text));
    if needle.is_empty() || needle.len() > words.len() { return 0; }
    (0..=words.len() - needle.len()).filter(|&i| words[i..i + needle.len()] == needle[..]).count()
}
/// Up to `limit` locations, then how many more.
fn pinpoint_summary(pinpoints: &[String], limit: usize) -> String {
    let values = pinpoints.iter().map(|value| js_trim(value)).filter(|value| !value.is_empty()).collect::<Vec<_>>();
    let mut out = values.iter().take(limit).cloned().collect::<Vec<_>>().join(", ");
    if values.len() > limit { out.push_str(&format!(" [+{} more instances]", values.len() - limit)); }
    out
}

/// Whether `inner` is a smaller block of the same kind inside `outer`.
fn within(outer: &Block, inner: &Block) -> bool {
    inner.kind == outer.kind
        && inner.ranges.iter().all(|part| outer.ranges.iter().any(|range| range.start <= part.start && part.end <= range.end))
        && match outer.node_id.as_ref().zip(inner.node_id.as_ref()) {
            Some((outer_id, _)) => inner.ancestor_ids.contains(outer_id),
            None => outer.ranges.iter().map(|range| range.end - range.start).sum::<usize>()
                > inner.ranges.iter().map(|range| range.end - range.start).sum::<usize>(),
        }
}

/// A block a quotation is scored against: one of the source's, or the cited range of them joined.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Candidate {
    Block(usize),
    Range(usize),
}

struct Located<'a> {
    reader: Reader,
    text: &'a str,
    source_text: ScalarText<'a>,
    blocks: Vec<Block>,
    /// Each block's readings, and the whole text's, prepared once for every quotation.
    prepared: Vec<Option<std::rc::Rc<Prepared>>>,
    whole: Option<std::rc::Rc<Prepared>>,
}

/// A text's readings a quotation is scored against (quote_match_score's of `source`), prepared once.
struct Prepared {
    text: String,
    length: usize,
    key: String,
    lower: Vec<u16>,
    tokens: std::rc::Rc<Vec<String>>,
    counts: HashMap<String, usize>,
    /// A long source's words, which a long source is scored by.
    words: Option<std::rc::Rc<LongWords>>,
}

/// A quotation's readings (quote_match_score's of `quote`), prepared once for every block.
struct Quoted {
    empty: bool,
    core: Vec<String>,
    key: String,
    single: bool,
    needle: Vec<u16>,
    aliases: HashMap<String, String>,
    counts: HashMap<String, usize>,
}

impl Reader {
    fn prepare(&mut self, source: &str) -> Prepared {
        let trimmed = js_trim(source);
        let key = self.exact_key(trimmed);
        let tokens = self.tokens(trimmed);
        let mut counts = HashMap::new();
        for token in tokens.iter() { *counts.entry(token.clone()).or_default() += 1; }
        let length = utf16_len(trimmed);
        let words = (length > LONG_SOURCE_CHARS || tokens.len() > LONG_SOURCE_TOKENS).then(|| self.long_words(trimmed));
        Prepared { length, lower: key.to_lowercase().encode_utf16().collect(), key, tokens, counts, words, text: trimmed.to_owned() }
    }
    fn is_whole(&self, text: &str) -> bool {
        self.whole == Some((text.as_ptr() as usize, text.len()))
    }
    fn long_words(&mut self, text: &str) -> std::rc::Rc<LongWords> {
        if !self.is_whole(text) { return std::rc::Rc::new(long_words(text)); }
        self.whole_words.get_or_insert_with(|| std::rc::Rc::new(long_words(text))).clone()
    }
    fn quoted(&mut self, quote: &str) -> Quoted {
        let q = js_trim(quote);
        let core = strip_trailing(&self.tokens(q));
        let key = self.exact_key(q);
        let mut counts = HashMap::new();
        for token in &core { *counts.entry(token.clone()).or_default() += 1; }
        Quoted { empty: q.is_empty(), single: SINGLE_WORD.is_match(js_trim(&key)), needle: js_trim(&key).to_lowercase().encode_utf16().collect(),
            aliases: alignment_aliases(&core), key, core, counts }
    }
}

/// quote_match_score from prepared readings.
fn prepared_score(quoted: &Quoted, source: &Prepared, floor: f64) -> f64 {
    if quoted.empty || source.text.is_empty() || quoted.core.is_empty() { return 0.0; }
    if !quoted.key.is_empty() && prepared_exact(quoted, source) { return 1.0; }
    if bracket_suffix_in(&quoted.core, &quoted.aliases, &source.tokens) { return 1.0; }
    if source.length > LONG_SOURCE_CHARS { return long_source_score(&quoted.core, source.words.as_ref().expect("long words")); }
    if source.tokens.is_empty() { return 0.0; }
    if source.tokens.len() > LONG_SOURCE_TOKENS { return long_source_score(&quoted.core, source.words.as_ref().expect("long words")); }
    if floor > 0.0 {
        let shared: usize = quoted.counts.iter().map(|(token, count)| (*count).min(source.counts.get(token).copied().unwrap_or(0))).sum();
        if (shared as f64) / (quoted.core.len() as f64) < floor { return 0.0; }
    }
    matched_size(&source.tokens, &quoted.core) as f64 / quoted.core.len() as f64
}
/// exact_in_source of the quotation's key in the source's, from prepared readings.
fn prepared_exact(quoted: &Quoted, source: &Prepared) -> bool {
    let found = if quoted.needle.is_empty() || source.key.is_empty() { false }
        else if quoted.single { find_exact(&quoted.key, &source.key).is_some() }
        else { index_of16(&source.lower, &quoted.needle, 0).is_some() };
    if found { return true; }
    let (q, s) = (collapse_initial_case(&quoted.key), collapse_initial_case(&source.key));
    (q != quoted.key || s != source.key) && find_exact(&q, &s).is_some()
}

struct Resolution {
    scope_ambiguous: bool,
    location: &'static str,
    score: f64,
    labels: Vec<String>,
    text: String,
    cited: Option<(f64, Vec<String>, String)>,
}

impl Located<'_> {
    fn block<'b>(&'b self, ranges: &'b [Block], candidate: Candidate) -> &'b Block {
        match candidate { Candidate::Block(index) => &self.blocks[index], Candidate::Range(index) => &ranges[index] }
    }

    fn label_is_unique(&self, block: &Block) -> bool {
        let label = block.label.to_lowercase();
        let number = (block.kind != QuoteBlockKind::Section).then(|| block_number(block));
        self.blocks.iter().filter(|other| other.kind == block.kind
            && (other.label.to_lowercase() == label
                || number.is_some_and(|number| number.is_finite() && block_number(other) == number)))
            .take(2).count() <= 1
    }

    fn cited_selection(&self, scopes: &QuoteScopes) -> (Vec<(usize, bool)>, bool) {
        let mut selected: Vec<(usize, bool)> = Vec::new();
        let mut ambiguous = false;
        let set = |index: usize, value: bool, selected: &mut Vec<(usize, bool)>| {
            match selected.iter_mut().find(|(at, _)| *at == index) {
                Some(entry) => entry.1 = value,
                None => selected.push((index, value)),
            }
        };
        for (kind, requested) in [(QuoteBlockKind::Paragraph, &scopes.paragraphs),
            (QuoteBlockKind::Page, &scopes.pages)] {
            for &(start, end) in requested {
                let members = self.blocks.iter().enumerate().filter(|(_, block)|
                    block.kind == kind && in_ranges(block_number(block), &[(start, end)]))
                    .collect::<Vec<_>>();
                let mut numbers = HashSet::new();
                if members.iter().any(|(_, block)| !numbers.insert(js_display(block_number(block)))) {
                    ambiguous = true;
                    // A partially ambiguous requested range is not a resolved
                    // citation to the remaining unique members. All candidates
                    // remain available to the full-document quotation search.
                    continue;
                }
                for (index, _) in members { set(index, true, &mut selected); }
            }
        }
        let mut sections = HashMap::<String, Option<usize>>::new();
        for (index, block) in self.blocks.iter().enumerate() {
            if block.kind == QuoteBlockKind::Section {
                sections.entry(block.label.to_lowercase()).and_modify(|owner| *owner = None)
                    .or_insert(Some(index));
            }
        }
        for section in &scopes.sections {
            for candidate in section_ancestors(section) {
                if let Some(owner) = sections.get(&candidate.to_lowercase()) {
                    // A declared ambiguous locator cannot be rescued by a
                    // broader locator. Only an absent child permits fallback.
                    ambiguous |= owner.is_none();
                    if let Some(&index) = owner.as_ref() {
                        let before = selected.iter().find(|(at, _)| *at == index).is_some_and(|(_, value)| *value);
                        set(index, before || candidate.to_lowercase() == section.to_lowercase(), &mut selected);
                    }
                    break;
                }
            }
        }
        (selected, ambiguous)
    }

    fn range_blocks(&self, scopes: &QuoteScopes) -> Vec<Block> {
        let mut combined = Vec::new();
        for (kind, ranges) in [(QuoteBlockKind::Paragraph, &scopes.paragraphs), (QuoteBlockKind::Page, &scopes.pages)] {
            for &(start, end) in ranges.iter() {
                if end <= start { continue; }
                let mut members = self.blocks.iter().filter(|block| block.kind == kind && start <= block_number(block)
                    && block_number(block) <= end).collect::<Vec<_>>();
                members.sort_by(|a, b| block_number(a).partial_cmp(&block_number(b)).unwrap_or(std::cmp::Ordering::Equal));
                let wanted = (end - start + 1.0) as usize;
                if members.len() != wanted || members.iter().enumerate().any(|(i, block)| block_number(block) != start + i as f64) {
                    continue;
                }
                let label = match kind {
                    QuoteBlockKind::Paragraph => format!("par{}\u{2013}{}", js_display(start), js_display(end)),
                    _ => format!("pages {}\u{2013}{}", js_display(start), js_display(end)),
                };
                let mut ranges = members.iter().flat_map(|block| block.ranges.iter().copied()).collect();
                ScalarRange::normalize_union(&mut ranges);
                combined.push(Block { node_id: None, ancestor_ids: Vec::new(), kind, label,
                    start: ranges.first().map_or(members[0].start, |range| range.start),
                    end: ranges.last().map_or(members[members.len() - 1].end, |range| range.end),
                    text: self.source_text.membership_text_utf16(&ranges), ranges });
            }
        }
        combined
    }

    fn plausible_in(&mut self, quote: &str, text: &str) -> bool {
        let region = self.reader.quote_region(quote, text, 400);
        plausible_content_overlap(quote, if region.is_empty() { text } else { &region })
    }

    fn scored(&mut self, quote: &str, candidates: &[Candidate], ranges: &[Block]) -> Vec<(f64, Candidate)> {
        let mut out = Vec::new();
        let quoted = self.reader.quoted(quote);
        for &candidate in candidates {
            let text = self.block(ranges, candidate).text.clone();
            let prepared = match candidate {
                Candidate::Block(index) => match &self.prepared[index] {
                    Some(prepared) => prepared.clone(),
                    None => {
                        let prepared = std::rc::Rc::new(self.reader.prepare(&text));
                        self.prepared[index] = Some(prepared.clone());
                        prepared
                    }
                },
                Candidate::Range(_) => std::rc::Rc::new(self.reader.prepare(&text)),
            };
            let score = prepared_score(&quoted, &prepared, MIN_MATCH);
            if score >= MIN_MATCH && self.plausible_in(quote, &text) { out.push((score, candidate)); }
        }
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            .then(self.block(ranges, a.1).start.cmp(&self.block(ranges, b.1).start)));
        out
    }

    fn specific_labels(&self, matches: &[(f64, Candidate)], ranges: &[Block]) -> Vec<String> {
        let mut labels: Vec<String> = Vec::new();
        for &(score, candidate) in matches {
            let block = self.block(ranges, candidate);
            if labels.contains(&block.label) || !self.label_is_unique(block) { continue; }
            if block.kind == QuoteBlockKind::Section && matches.iter().any(|&(other, other_candidate)| {
                other >= score && within(block, self.block(ranges, other_candidate))
            }) { continue; }
            labels.push(block.label.clone());
        }
        labels
    }

    /// The matches whose block holds no other's of its kind scoring as well: a schedule's provision, not the schedule.
    fn most_specific(&self, matches: Vec<(f64, Candidate)>, ranges: &[Block]) -> Vec<(f64, Candidate)> {
        let keep = matches.iter().map(|&(score, candidate)| !matches.iter().any(|&(other, inner)| other >= score
            && within(self.block(ranges, candidate), self.block(ranges, inner)))).collect::<Vec<_>>();
        matches.into_iter().zip(keep).filter_map(|(found, keep)| keep.then_some(found)).collect()
    }

    /// The matches whose block holds the quotation as written, else case aside: those holding it as written first,
    /// then where its words run closest together, then nearest the cited places (`near`), then in order; a block
    /// giving way to one inside it that holds it.
    fn holding(&self, support: &QuoteSupport, matches: &[(f64, Candidate)], ranges: &[Block], near: &[usize])
        -> Vec<(f64, Candidate)> {
        let found = matches.iter().filter_map(|&(score, candidate)| support.hold(&self.block(ranges, candidate).text)
            .map(|hold| (hold, score, candidate))).collect::<Vec<_>>();
        let mut kept = found.iter().copied().filter(|&(.., candidate)| !found.iter().any(|&(.., inner)|
            within(self.block(ranges, candidate), self.block(ranges, inner)))).collect::<Vec<_>>();
        let start = |candidate: Candidate| self.block(ranges, candidate).start;
        let distance = |candidate: Candidate| near.iter().map(|&at| start(candidate).abs_diff(at)).min().unwrap_or(0);
        kept.sort_by_key(|&(hold, _, candidate)| (hold, distance(candidate), start(candidate)));
        kept.into_iter().map(|(_, score, candidate)| (score, candidate)).collect()
    }

    fn document_score(&mut self, quote: &str, text: &str) -> f64 {
        let prepared = match &self.whole {
            Some(prepared) => prepared.clone(),
            None => {
                let prepared = std::rc::Rc::new(self.reader.prepare(text));
                self.whole = Some(prepared.clone());
                prepared
            }
        };
        let whole = prepared_score(&self.reader.quoted(quote), &prepared, 0.0);
        if whole >= STRONG_MATCH { return whole; }
        let region = self.reader.quote_region(quote, text, 400);
        whole.max(self.reader.quote_match_score(quote, &region, 0.0))
    }

    fn resolve(&mut self, quote: &str, scopes: &QuoteScopes) -> Resolution {
        let text = self.text;
        let (selection, scope_ambiguous) = self.cited_selection(scopes);
        let scoped = selection.iter().map(|(index, _)| Candidate::Block(*index)).collect::<Vec<_>>();
        let ranges = self.range_blocks(scopes);
        let mut scoped_and_ranges = scoped.clone();
        scoped_and_ranges.extend((0..ranges.len()).map(Candidate::Range));
        let scoped_matches = self.scored(quote, &scoped_and_ranges, &ranges);
        let located = |this: &Self, matches: &[(f64, Candidate)]| {
            let best = matches[0].1;
            let cited = matches!(best, Candidate::Range(_)) || match best {
                Candidate::Block(index) => selection.iter().find(|(at, _)| *at == index).is_some_and(|(_, value)| *value),
                Candidate::Range(_) => true,
            };
            Resolution { scope_ambiguous, location: if cited { "cited" } else { "cited_parent" }, score: matches[0].0,
                labels: this.specific_labels(matches, &ranges), text: this.block(&ranges, best).text.clone(), cited: None }
        };
        // A block that holds the quotation as written is where it is: a cited one first, then any other. Scores
        // only rank blocks; one that scores 1 for holding the quoted words with others between them does not hold it.
        let support = QuoteSupport::new(quote);
        let cited_held = self.holding(&support, &scoped_matches, &ranges, &[]);
        if !cited_held.is_empty() { return located(self, &cited_held); }
        let scoped_set = scoped.iter().copied().collect::<HashSet<_>>();
        let all = (0..self.blocks.len()).map(Candidate::Block).collect::<Vec<_>>();
        let alternates = self.scored(quote, &all, &ranges).into_iter().filter(|(_, c)| !scoped_set.contains(c)).collect::<Vec<_>>();
        // Wording found at several places: a pinpoint that misses most often meant the one nearest it.
        let near = scoped.iter().map(|&candidate| self.block(&ranges, candidate).start).collect::<Vec<_>>();
        let mut matches = self.holding(&support, &alternates, &ranges, &near);
        let held = !matches.is_empty();
        // Held nowhere: the closest wording, the cited passage's first.
        let mut strong = scoped_matches.iter().copied().filter(|(score, _)| *score >= STRONG_MATCH).collect::<Vec<_>>();
        if matches.is_empty() && !strong.is_empty() {
            let range_matches = strong.iter().copied().filter(|(_, c)| matches!(c, Candidate::Range(_))).collect::<Vec<_>>();
            let specific = strong.iter().copied().filter(|(_, c)| !matches!(c, Candidate::Range(_))).collect::<Vec<_>>();
            if !range_matches.is_empty() && !specific.is_empty() {
                let any_words = specific.iter().any(|&(_, c)| word_count(quote, &self.block(&ranges, c).text) > 0);
                strong = if any_words || specific[0].0 >= range_matches[0].0 { specific } else { range_matches };
            }
            return located(self, &strong);
        }
        if matches.is_empty() {
            matches = self.most_specific(alternates.iter().copied().filter(|(score, _)| *score >= STRONG_MATCH).collect(), &ranges);
        }
        if matches.is_empty() && !scoped_matches.is_empty() { return located(self, &scoped_matches[..1]); }
        let has_kind = |kind: QuoteBlockKind| self.blocks.iter().any(|block| block.kind == kind);
        let target_kind_available = (!scopes.paragraphs.is_empty() && has_kind(QuoteBlockKind::Paragraph))
            || (!scopes.pages.is_empty() && has_kind(QuoteBlockKind::Page))
            || (!scopes.sections.is_empty() && has_kind(QuoteBlockKind::Section));
        if matches.is_empty() {
            let collapsed = alternates.iter().copied().filter(|&(score, c)| {
                let block = self.block(&ranges, c);
                block.kind != QuoteBlockKind::Section || !alternates.iter().any(|&(other, oc)| other >= score
                    && within(block, self.block(&ranges, oc)))
            }).collect::<Vec<_>>();
            if !collapsed.is_empty() && (collapsed.len() == 1 || collapsed[0].0 - collapsed[1].0 >= 0.1) {
                matches = collapsed[..1].to_vec();
            }
        }
        if !matches.is_empty() {
            let mut labels = Vec::new();
            // A word or two near words almost anywhere is placed only where it is held, or found once.
            if quote_words(quote).len() < 3 && !held {
                let exact = matches.iter().copied().filter(|&(_, c)| word_count(quote, &self.block(&ranges, c).text) > 0)
                    .collect::<Vec<_>>();
                if word_count(quote, text) == 1 && !exact.is_empty() { matches = exact; labels = self.specific_labels(&matches, &ranges); }
            } else {
                labels = self.specific_labels(&matches, &ranges);
            }
            let location = if !scoped.is_empty() || target_kind_available { "alternate" }
                else if has_scopes(scopes) { "scope_unavailable" } else { "uncited" };
            let cited = (location == "alternate" && !scoped_matches.is_empty()).then(|| (scoped_matches[0].0,
                self.specific_labels(&scoped_matches[..1], &ranges), self.block(&ranges, scoped_matches[0].1).text.clone()));
            return Resolution { scope_ambiguous, location, score: matches[0].0, labels, text: self.block(&ranges, matches[0].1).text.clone(), cited };
        }
        let score = self.document_score(quote, text);
        if score >= MIN_MATCH {
            let location = if !scoped.is_empty() || target_kind_available { "alternate_document" }
                else if has_scopes(scopes) { "scope_unavailable_document" } else { "uncited_document" };
            return Resolution { scope_ambiguous, location, score, labels: Vec::new(), text: text.to_owned(), cited: None };
        }
        Resolution { scope_ambiguous, location: "unmatched", score, labels: Vec::new(), text: String::new(), cited: None }
    }

    /// Page labels where the quotation's words run, over a page break too.
    fn page_pinpoint(&mut self, quote: &str) -> String {
        let pages = self.blocks.iter().filter(|block| block.kind == QuoteBlockKind::Page).cloned().collect::<Vec<_>>();
        let mut hits = Vec::new();
        for block in &pages {
            if self.label_is_unique(block) && self.reader.quote_match_score(quote, &block.text, 0.0) >= STRONG_MATCH { hits.push(block.label.clone()); }
        }
        if !hits.is_empty() { return pinpoint_summary(&hits, 2); }
        for pair in pages.windows(2) {
            if pair.iter().any(|block| !self.label_is_unique(block)) { continue; }
            let mut ranges = pair.iter().flat_map(|block| block.ranges.iter().copied()).collect();
            ScalarRange::normalize_union(&mut ranges);
            let text = self.source_text.membership_text_utf16(&ranges);
            if !quote_words(quote).is_empty()
                && self.reader.quote_match_score(quote, &text, 0.0) >= STRONG_MATCH {
                let tail = |label: &str| label.split(' ').next_back().unwrap_or("").to_owned();
                return format!("pages {}\u{2013}{}", tail(&pair[0].label), tail(&pair[1].label));
            }
        }
        String::new()
    }

    /// The source's own words for a quotation, in the text it was found in, and the text it is corrected to.
    fn correction(&mut self, quote: &str, match_text: &str) -> QuoteCorrection {
        let region = self.reader.quote_region(quote, match_text, 400);
        let mut source = if !region.is_empty() { region.clone() }
            else if utf16_len(match_text) > LONG_SOURCE_CHARS { quote.to_owned() } else { match_text.to_owned() };
        let fragment = if region.is_empty() { String::new() } else { self.reader.source_side_quote_fragment(&region, quote, 900) };
        if !fragment.is_empty() && self.reader.quote_match_score(quote, &fragment, 0.0)
            >= MIN_MATCH.max(self.reader.quote_match_score(quote, &source, 0.0)) { source = fragment.clone(); }
        QuoteCorrection { region, fragment, corrected: corrected_quote(quote, &source) }
    }

    fn locate(&mut self, quote: &str, scopes: &QuoteScopes) -> QuoteLocation {
        let text = self.text;
        let mut resolution = if self.blocks.is_empty() {
            Resolution { scope_ambiguous: false, location: "uncited_document", score: self.document_score(quote, text), labels: Vec::new(),
                text: text.to_owned(), cited: None }
        } else { self.resolve(quote, scopes) };
        let short = quote_words(quote).len() < 3;
        if short && resolution.location.starts_with("alternate") && resolution.labels.is_empty() {
            resolution = Resolution { scope_ambiguous: resolution.scope_ambiguous, location: "unmatched", score: 0.0, labels: Vec::new(), text: String::new(), cited: None };
        }
        let unmatched = QuoteLocation { location: "unmatched".into(), score: resolution.score, labels: Vec::new(),
            pinpoint: String::new(), text: String::new(), correction: QuoteCorrection::default(), perfect: false,
            short_in_long: short && utf16_len(text) > LONG_SOURCE_CHARS, cited: None };
        if resolution.score < MIN_MATCH { return unmatched; }
        let match_text = if resolution.text.is_empty() { text.to_owned() } else { resolution.text.clone() };
        let correction = self.correction(quote, &match_text);
        if resolution.score < STRONG_MATCH && !plausible_content_overlap(quote,
            if correction.region.is_empty() { &match_text } else { &correction.region }) { return unmatched; }
        if self.reader.only_bracketed(&correction.corrected) { return unmatched; }
        let labeled = !resolution.labels.is_empty() && !resolution.location.starts_with("scope_unavailable");
        let limited = resolution.scope_ambiguous || resolution.location.starts_with("scope_unavailable")
            || resolution.location == "cited_parent";
        let cited = resolution.cited.take().map(|(score, labels, text)| CitedPartialMatch { score,
            pinpoint: pinpoint_summary(&labels, 2), labels, correction: self.correction(quote, &text) });
        let pinpoint = if labeled { pinpoint_summary(&resolution.labels, 2) } else { self.page_pinpoint(quote) };
        let perfect = resolution.score >= STRONG_MATCH && quote_exact_key(&correction.corrected) == quote_exact_key(quote)
            && !limited;
        QuoteLocation { location: resolution.location.into(), score: resolution.score,
            labels: if labeled { resolution.labels } else { Vec::new() }, pinpoint, text: resolution.text, correction,
            perfect, short_in_long: false, cited }
    }
}

/// How JavaScript writes a whole number held as a double.
fn js_display(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 { format!("{}", value as i64) } else { format!("{value}") }
}

/// Every quotation of `quotes` located in `text`, its blocks given by their UTF-16 offsets, and with `compare`,
/// compared with the source's words where it was found (the whole text where it was not).
pub fn locate_quotes_in(text: &str, blocks: &[QuoteBlockInput], quotes: &[QuoteToLocate], compare: bool) -> Vec<LocatedQuote> {
    let source_text = ScalarText::new(text);
    let blocks: Vec<Block> = blocks.iter().map(|block| {
        let mut ranges = block.ranges.clone();
        ScalarRange::normalize_union(&mut ranges);
        Block { node_id: block.node_id.clone(), ancestor_ids: block.ancestor_ids.clone(),
            kind: block.kind, label: block.label.clone(), start: block.start, end: block.end,
            text: source_text.membership_text_utf16(&ranges), ranges }
    }).collect();
    let prepared = vec![None; blocks.len()];
    let trimmed = js_trim(text);
    let reader = Reader { whole: Some((trimmed.as_ptr() as usize, trimmed.len())), ..Reader::default() };
    let mut located = Located { reader, text, source_text, blocks, prepared, whole: None };
    // A quotation compared with the whole text reads its index and words, built once.
    let mut whole: Option<(legal_document_query::TextFragmentIndex<'_>, SourceWords)> = None;
    let out = quotes.iter().map(|item| {
        let found = located.locate(&item.quote, &item.scopes);
        let passage = [found.correction.region.as_str(), found.text.as_str()].into_iter().find(|value| !value.is_empty())
            .unwrap_or(text);
        let comparison = if !compare { None } else if passage == text {
            let (index, words) = whole.get_or_insert_with(|| (legal_document_query::text_fragment_index(text), SourceWords::new(text)));
            Some(quote_text_comparison_in(&item.quote, text, index, Some(words)))
        } else {
            Some(quote_text_comparison(&item.quote, passage))
        };
        LocatedQuote { comparison, found }
    }).collect::<Vec<_>>();
    out
}

// The quotation rewritten to the source's words, and what differs: Beaver's authoritiesDiscrepancy.ts (editorialQuote,
// verbatimRepair, quoteTextComparison, ported from the pinned Authorities oracle) and quoteCheck.ts (correctedQuote).

const JS_SPACE: &str = r"\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";
const DOUBLE_QUOTE_CHARS: &str = "\"\u{201c}\u{201d}\u{ab}\u{bb}\u{201e}";
static MARKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{M}+").unwrap());
static BLANKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t\r\n\x0C\x0B]+").unwrap());
static LOWER_INITIAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[([a-z])\]([a-z]+)$").unwrap());
static BRACKETED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[^\]]+\]").unwrap());
static CLOSER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[)\]},.;:!?]$").unwrap());
static OPENER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[(\[{]$").unwrap());
static LEADING_QUOTES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^[{DOUBLE_QUOTE_CHARS}{JS_SPACE}]+")).unwrap());
static TRAILING_QUOTES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("[{DOUBLE_QUOTE_CHARS}{JS_SPACE}]+$")).unwrap());
static PUNCTUATION_TAIL: LazyLock<Regex> = LazyLock::new(||
    Regex::new(&format!(r"^(?:[{JS_SPACE}]*(?:\.\.\.|[.,;:!?\u{{2026}}]))+[{JS_SPACE}]*$")).unwrap());

fn token_texts(text: &str) -> Vec<String> {
    quote_tokens(text).into_iter().map(|token| token.text).collect()
}
/// text.ts normalizeWhitespace: trimmed, runs of blanks (not NBSP) one space.
fn normalize_whitespace(text: &str) -> String {
    BLANKS.replace_all(js_trim(text), " ").into_owned()
}
/// Only substantive wording is a difference: case, diacritics, quote and dash variants and compatibility width are
/// noise.
fn fold_for_comparison(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    // ASCII decomposes to itself and has no marks; of it, only blanks and the backtick fold otherwise.
    if value.bytes().all(|byte| byte.is_ascii_graphic() && byte != b'`') { return value.to_ascii_lowercase(); }
    let decomposed = normalize_whitespace(value).nfkd().collect::<String>();
    MARKS.replace_all(&decomposed, "").chars().map(|c| match c {
        '"' | '\u{201c}' | '\u{201d}' | '\u{ab}' | '\u{bb}' | '\u{201e}' => '"',
        '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '`' | '\u{b4}' => '\'',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        c => c,
    }).collect::<String>().to_lowercase()
}
fn editorial_equivalent(value: &str) -> String {
    if DOUBLE_QUOTE_CHARS.contains(value) { return "\"".into(); }
    if "'\u{2018}\u{2019}\u{201a}".contains(value) { return "'".into(); }
    let normalized = fold_for_comparison(value);
    match LOWER_INITIAL.captures(&normalized) {
        Some(found) => format!("{}{}", &found[1], &found[2]),
        None => normalized,
    }
}
/// One UTF-16 unit, lower-cased as JavaScript's `s[0].toLowerCase()` reads it.
fn unit_lower(unit: u16) -> String {
    String::from_utf16_lossy(&[unit]).to_lowercase()
}
/// The same word but for the case of its first letter.
fn first_letter_case(written: &[u16], other: &[u16]) -> bool {
    !written.is_empty() && written.len() == other.len() && written[1..] == other[1..]
        && unit_lower(written[0]) == unit_lower(other[0])
}
/// `[x]` around the first letter of `written`.
fn bracket_first(written: &[u16]) -> String {
    format!("[{}]{}", slice16(written, 0, 1), slice16(written, 1, written.len()))
}

fn join_tokens(values: &[String]) -> String {
    let (mut result, mut previous) = (String::new(), String::new());
    let (mut open_double, mut open_single, mut prior_role) = (false, false, "");
    for value in values {
        let mut chars = value.chars();
        let dash = matches!((chars.next(), chars.next()), (Some('\u{2010}'..='\u{2015}' | '\u{2212}'), None));
        let value = if dash { "-".to_owned() } else { value.clone() };
        let role = if value == "\"" { open_double = !open_double; if open_double { "open" } else { "close" } }
            else if value == "'" { open_single = !open_single; if open_single { "open" } else { "close" } }
            else if "\u{201c}\u{ab}\u{201e}\u{2018}\u{201a}".contains(value.as_str()) { "open" }
            else if "\u{201d}\u{bb}\u{2019}".contains(value.as_str()) { "close" }
            else { "" };
        if result.is_empty() { result = value.clone(); }
        else if value == "-" || previous == "-" { result = format!("{}{}", result.trim_end_matches(js_space), value); }
        else if role == "close" || CLOSER.is_match(&value) { result.push_str(&value); }
        else if OPENER.is_match(&previous) || prior_role == "open" { result.push_str(&value); }
        else { result.push(' '); result.push_str(&value); }
        previous = value;
        prior_role = role;
    }
    normalize_whitespace(&result)
}

/// A word with letters inserted inside it ("employment" written "employ[ee]ment"), the insertion bracketed.
fn internal_insertion(authored: &str, source: &str) -> String {
    if authored.is_empty() || source.is_empty() || authored.contains(['[', ']']) || source.contains(['[', ']']) {
        return String::new();
    }
    // Code points aligned, UTF-16 sliced, as the oracle does.
    let edits = sequence_opcodes(&source.chars().collect::<Vec<_>>(), &authored.chars().collect::<Vec<_>>());
    let inserts = edits.iter().filter(|edit| edit.0 == "insert").collect::<Vec<_>>();
    if inserts.len() != 1 || edits.iter().any(|edit| edit.0 != "equal" && edit.0 != "insert") { return String::new(); }
    let equal: usize = edits.iter().filter(|edit| edit.0 == "equal").map(|edit| edit.2 - edit.1).sum();
    let inserted = inserts[0].4 - inserts[0].3;
    let length = utf16_len(source);
    if equal < 3.min(length) || inserted as f64 > 4f64.max(length as f64 / 2.0 + 1.0) { return String::new(); }
    let (source, authored) = (source.encode_utf16().collect::<Vec<_>>(), authored.encode_utf16().collect::<Vec<_>>());
    edits.iter().map(|&(tag, a, b, c, d)| if tag == "equal" { slice16(&source, a, b) }
        else { format!("[{}]", slice16(&authored, c, d)) }).collect()
}

fn format_authored(authored: &[String], source: Option<&[String]>) -> Vec<String> {
    if authored.is_empty() { return Vec::new(); }
    if let Some([s]) = source {
        let a = &authored[0];
        if authored.len() == 1 && SINGLE_WORD.is_match(a) && SINGLE_WORD.is_match(s) {
            let (written, other) = (a.encode_utf16().collect::<Vec<_>>(), s.encode_utf16().collect::<Vec<_>>());
            if first_letter_case(&written, &other) && written[0] != other[0] { return vec![bracket_first(&written)]; }
            let internal = internal_insertion(a, s);
            if !internal.is_empty() { return vec![internal]; }
        }
    }
    if authored.len() == 1 {
        return if BRACKETED.is_match(&authored[0]) || !SINGLE_WORD.is_match(&authored[0]) { authored.to_vec() }
            else { vec![format!("[{}]", authored[0])] };
    }
    if authored.iter().any(|item| BRACKETED.is_match(item)) { authored.to_vec() }
    else if authored.iter().any(|item| SINGLE_WORD.is_match(item)) { vec![format!("[{}]", join_tokens(authored))] }
    else { authored.to_vec() }
}

/// The source's words as the author quoted them: the author's own changes bracketed, source words left out inside
/// the quotation an ellipsis.
pub fn editorial_quote(authored_quote: &str, source_quote: &str) -> String {
    let (authored, source) = (token_texts(js_trim(authored_quote)), token_texts(js_trim(source_quote)));
    if authored.is_empty() { return js_trim(source_quote).to_owned(); }
    if source.is_empty() { return js_trim(authored_quote).to_owned(); }
    let left = source.iter().map(|token| editorial_equivalent(token)).collect::<Vec<_>>();
    let right = authored.iter().map(|token| editorial_equivalent(token)).collect::<Vec<_>>();
    let mut output: Vec<String> = Vec::new();
    for (tag, i1, i2, j1, j2) in sequence_opcodes(&left, &right) {
        match tag {
            "equal" => for (index, item) in source[i1..i2].iter().enumerate() {
                let written = &authored[j1 + index];
                let units = written.encode_utf16().collect::<Vec<_>>();
                output.push(if item == written || INITIAL.is_match(written)
                        || DOUBLE_QUOTES.contains(&item.as_str()) || SINGLE_QUOTES.contains(&item.as_str()) { written.clone() }
                    else if SINGLE_WORD.is_match(item) && SINGLE_WORD.is_match(written)
                        && first_letter_case(&units, &item.encode_utf16().collect::<Vec<_>>()) { bracket_first(&units) }
                    else { item.clone() });
            },
            // Source words left out inside the quotation are an ellipsis; those before or after it are not quoted.
            "delete" => if source[i1..i2].iter().any(|item| SINGLE_WORD.is_match(item)) {
                if output.iter().any(|item| wordlike(item)) && authored[j1..].iter().any(|item| wordlike(item))
                    && output.last().is_some_and(|last| last != "...") {
                    output.push("...".into());
                }
            } else { output.extend(source[i1..i2].iter().cloned()); },
            _ => {
                // Sentence punctuation must not consume the source's closing inner quotation mark.
                if tag == "replace" && !source[i1..i2].iter().any(|item| wordlike(item))
                    && !authored[j1..j2].iter().any(|item| wordlike(item)
                        || DOUBLE_QUOTES.contains(&item.as_str()) || SINGLE_QUOTES.contains(&item.as_str())) {
                    for item in &source[i1..i2] {
                        let single = match item.as_str() {
                            "'" | "\u{2019}" => true,
                            "\"" | "\u{201d}" | "\u{bb}" => false,
                            _ => continue,
                        };
                        let depth = output.iter().fold(0usize, |depth, token| match (single, token.as_str()) {
                            (true, "'") | (false, "\"") => if depth == 0 { 1 } else { depth - 1 },
                            (true, "\u{2018}" | "\u{201a}") | (false, "\u{201c}" | "\u{ab}" | "\u{201e}") => depth + 1,
                            (true, "\u{2019}") | (false, "\u{201d}" | "\u{bb}") => depth.saturating_sub(1),
                            _ => depth,
                        });
                        if depth > 0 { output.push(item.clone()); }
                    }
                }
                output.extend(format_authored(&authored[j1..j2], (tag == "replace").then(|| &source[i1..i2])));
            },
        }
    }
    join_tokens(&output)
}

/// The quotation rewritten to the source's wording: changes in brackets, omissions as ellipses.
pub fn corrected_quote(quote: &str, excerpt: &str) -> String {
    let authored = js_trim(quote);
    let mut source = js_trim(excerpt).to_owned();
    if authored.is_empty() { return source; }
    if source.is_empty() { return authored.to_owned(); }
    // A source's own quotation marks around the passage are not part of the authored quotation.
    let quoted = token_texts(authored);
    let double = |token: Option<&String>| token.is_some_and(|token| DOUBLE_QUOTES.contains(&token.as_str()));
    if !double(quoted.first()) { source = LEADING_QUOTES.replace(&source, "").into_owned(); }
    if !double(strip_trailing(&quoted).last()) {
        source = TRAILING_QUOTES.replace(&source, "").into_owned();
        // A closing quotation mark followed only by the sentence's punctuation.
        if let Some((at, mark)) = source.char_indices().find(|&(at, c)| DOUBLE_QUOTE_CHARS.contains(c)
            && PUNCTUATION_TAIL.is_match(&source[at + c.len_utf8()..])) {
            source.replace_range(at..at + mark.len_utf8(), "");
        }
    }
    editorial_quote(authored, &source)
}

/// The source's words for an authored quotation that differs from them: the source passage its words align with,
/// where they align closely and the passage occurs once.
pub fn verbatim_repair(authored: &str, source: &str) -> Option<String> {
    verbatim_repair_in(authored, source, &SourceWords::new(source))
}

/// A source's words (byte spans) and their folded forms, read once for every quotation compared with it.
pub struct SourceWords {
    spans: Vec<(usize, usize)>,
    keys: Vec<String>,
}

impl SourceWords {
    pub fn new(source: &str) -> Self {
        let spans = WORD.find_iter(source).map(|word| (word.start(), word.end())).collect::<Vec<_>>();
        SourceWords { keys: spans.iter().map(|&(start, end)| fold_for_comparison(&source[start..end])).collect(), spans }
    }
}

fn verbatim_repair_in(authored: &str, source: &str, words: &SourceWords) -> Option<String> {
    let before = WORD.find_iter(authored).map(|word| fold_for_comparison(word.as_str())).collect::<Vec<_>>();
    let (after, keys) = (&words.spans, &words.keys);
    if before.is_empty() || after.is_empty() { return None; }
    let equal = sequence_opcodes(&before, keys).into_iter().filter(|op| op.0 == "equal").collect::<Vec<_>>();
    let (first_op, last_op) = (equal.first()?, equal.last()?);
    let first = first_op.3.saturating_sub(first_op.1);
    let last = after.len().min(last_op.4 + before.len() - last_op.2);
    let matched: usize = equal.iter().map(|op| op.2 - op.1).sum();
    // Compare both lengths: a tiny common fragment in a long passage is not alignment.
    if matched < 4.min(before.len()) || (matched as f64 / before.len().max(last - first) as f64) < 0.7
        || equal.iter().map(|op| op.2 - op.1).max().unwrap_or(0) < 2 { return None; }
    let (start, end) = (after[first].0, after[last - 1].1);
    let tail = source[end..].chars().take_while(|c| ".,;:!?".contains(*c)).collect::<String>();
    let candidate = format!("{}{tail}", &source[start..end]);
    // Repeated candidate wording is ambiguous, even if the diff happens to prefer the first copy.
    (source.find(&candidate) == source.rfind(&candidate)).then_some(candidate)
}

#[derive(Serialize)]
pub struct QuoteComparisonMatch {
    pub start: usize,
    pub end: usize,
    pub text: String,
}
#[derive(Serialize)]
pub struct QuoteChange {
    pub kind: &'static str,
    pub authored: String,
    pub source: String,
}
/// Where the quotation's words are in the source (UTF-16 offsets), the source passage it is compared with
/// (`candidateOnly`: a close passage, not the quoted words found), the quotation rewritten to it, and what differs.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteComparison {
    pub matches: Vec<QuoteComparisonMatch>,
    pub candidate: Option<String>,
    pub candidate_only: bool,
    pub editorial: Option<String>,
    pub changes: Vec<QuoteChange>,
}

/// Mechanical match locations and the correction and diff Authorities review shows.
pub fn quote_text_comparison(authored: &str, source: &str) -> QuoteComparison {
    quote_text_comparison_in(authored, source, &legal_document_query::text_fragment_index(source), None)
}

/// quote_text_comparison with the source's word index, and its words where read, already built.
fn quote_text_comparison_in(authored: &str, source: &str, index: &legal_document_query::TextFragmentIndex<'_>,
    words: Option<&SourceWords>) -> QuoteComparison {
    let plan = legal_document_query::text_fragment_plan_in(source, index, &[authored.to_owned()], false, false, false);
    let units = source.encode_utf16().collect::<Vec<_>>();
    let matches = if plan.source_safe_complete { plan.source_word_intervals.iter().map(|interval| QuoteComparisonMatch {
        start: interval.start, end: interval.end, text: slice16(&units, interval.start, interval.end) }).collect() }
        else { Vec::new() };
    let candidate = if matches.is_empty() {
        match words { Some(words) => verbatim_repair_in(authored, source, words), None => verbatim_repair(authored, source) }
    } else {
        let start = matches.iter().map(|found: &QuoteComparisonMatch| found.start).min().unwrap_or(0);
        Some(slice16(&units, start, matches.iter().map(|found| found.end).max().unwrap_or(0)))
    };
    let present = candidate.as_deref().filter(|value| !value.is_empty());
    let (editorial, changes) = match present {
        Some(passage) => {
            let (before, after) = (token_texts(authored), token_texts(passage));
            let changes = sequence_opcodes(&before.iter().map(|token| editorial_equivalent(token)).collect::<Vec<_>>(),
                &after.iter().map(|token| editorial_equivalent(token)).collect::<Vec<_>>()).into_iter()
                .filter(|op| op.0 != "equal").map(|(kind, a0, a1, b0, b1)| QuoteChange { kind,
                    authored: join_tokens(&before[a0..a1]), source: join_tokens(&after[b0..b1]) }).collect();
            (Some(editorial_quote(authored, passage)), changes)
        }
        None => (None, Vec::new()),
    };
    QuoteComparison { candidate_only: matches.is_empty(), matches, candidate, editorial, changes }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each line a block of `kind`, labelled by `label` and its number.
    fn lines(text: &str, kind: QuoteBlockKind, label: &str) -> Vec<QuoteBlockInput> {
        let mut at = 0;
        text.split('\n').enumerate().map(|(index, line)| {
            let block = QuoteBlockInput { kind, label: format!("{label}{}", index + 1), start: at, end: at + utf16_len(line) };
            at += utf16_len(line) + 1;
            block
        }).collect()
    }
    fn locate(text: &str, blocks: &[QuoteBlockInput], quote: &str, paragraphs: Vec<(f64, f64)>) -> QuoteLocation {
        let scopes = QuoteScopes { paragraphs, ..QuoteScopes::default() };
        locate_quotes_in(text, blocks, &[QuoteToLocate { quote: quote.into(), scopes, id: None }], false).remove(0).found
    }

    #[test]
    fn a_cited_passage_holding_the_words_apart_yields_to_one_holding_them_as_written() {
        let text = "The tenant listed debts owed to the landlord in the earlier tenancy before the hearing.\n\
            Only debts owed to the tenancy may be set off against the rent.";
        let found = locate(text, &lines(text, QuoteBlockKind::Paragraph, "par"), "debts owed to the tenancy", vec![(1.0, 1.0)]);
        assert_eq!((found.location.as_str(), found.labels.clone()), ("alternate", vec!["par2".to_owned()]));
        assert!(found.perfect);
        assert_eq!(found.cited.map(|cited| cited.labels), Some(vec!["par1".to_owned()]));
    }

    #[test]
    fn words_a_source_brackets_are_still_its_words() {
        let text = "The board described this as the [notice threshold] for all later filings.";
        let found = locate(text, &lines(text, QuoteBlockKind::Paragraph, "par"), "notice threshold", vec![(1.0, 1.0)]);
        assert_eq!(found.location, "cited");
        assert_eq!(found.correction.fragment, "notice threshold");
        assert!(found.perfect);
    }

    #[test]
    fn a_provision_holding_the_quotation_is_named_rather_than_its_schedule() {
        let text = "Schedule\nThe installation and operation of a new wind farm.\n\
            The installation and operation of a new tidal turbine array.";
        let mut blocks = lines(text, QuoteBlockKind::Section, "sec");
        blocks.insert(0, QuoteBlockInput { kind: QuoteBlockKind::Section, label: "secSchedule".into(), start: 0, end: utf16_len(text) });
        let found = locate(text, &blocks, "the installation and operation of a new tidal turbine array", Vec::new());
        assert_eq!(found.labels, vec!["sec3".to_owned()]);
        // Without blocks, the quotation is still found where it stands, not where its opening first does.
        let found = locate(text, &[], "the installation and operation of a new tidal turbine array", Vec::new());
        assert_eq!(found.correction.fragment, "The installation and operation of a new tidal turbine array");
    }

    #[test]
    fn wording_found_in_several_places_names_them_nearest_the_missed_pinpoint_first() {
        let text = "A fair hearing matters.\nThe facts were agreed.\nCosts were reserved.\nA fair hearing was refused.";
        let found = locate(text, &lines(text, QuoteBlockKind::Paragraph, "par"), "fair hearing", vec![(3.0, 3.0)]);
        assert_eq!((found.location.as_str(), found.labels), ("alternate", vec!["par4".to_owned(), "par1".to_owned()]));
    }

    #[test]
    fn the_cited_passage_holding_the_words_in_another_case_is_still_where_they_are() {
        let text = "Reasonable notice was given to every tenant.\nThe landlord says reasonable notice was given.";
        let found = locate(text, &lines(text, QuoteBlockKind::Paragraph, "par"), "reasonable notice was given", vec![(1.0, 1.0)]);
        assert_eq!((found.location.as_str(), found.labels), ("cited", vec!["par1".to_owned()]));
        assert_eq!(found.correction.corrected, "[r]easonable notice was given");
    }
}
