//! Where a quotation is in its source: the cited paragraph, page or provision first, then anywhere else, then the
//! document as a whole; the block that holds it, its score, the source's own words for it. Ported from Beaver's
//! quoteCheck.ts (itself ported from the ALR Quote Verifier's _apply_quote_checks and resolve_quote), with its
//! JavaScript string semantics kept where they decide a result: lengths and slices count UTF-16 units, and trimming
//! and lower-casing are ECMAScript's. A source's quotations are located in one call, the source's text read once.

use crate::quote_verification::QuoteSupport;
use legal_structure::{javascript_whitespace as js_space, trim_javascript_whitespace as js_trim, utf16_len, JS_WHITESPACE_CLASS};
use legal_document_query::{lowercase_words, word_regex, DocumentQuery};
use legal_structure::parse_ordinal;
use legal_structure_model::{provision_mark, text::{normalize_blank_whitespace, ScalarCoordinates}, DocumentStructure, NodeKind, ScalarRange, ScalarText, StructureNode};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
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

fn is_dash(c: char) -> bool {
    matches!(c, '-' | '\u{ad}' | '\u{2010}'..='\u{2015}' | '\u{2212}')
}
fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric()
}
/// `text.slice(start, end)` in UTF-16 units, its end clamped to the text and a character either end splits left out.
fn slice_utf16<'t>(text: &ScalarText<'t>, start: usize, end: usize) -> &'t str {
    let end = end.min(text.utf16_len());
    match (text.byte_at_utf16_ceil(start), text.byte_at_utf16_floor(end)) {
        (Some(from), Some(to)) if from < to => &text.value[from..to],
        _ => "",
    }
}
/// `haystack.indexOf(needle, from)`, in UTF-16 units.
fn index_of(haystack: &ScalarText, needle: &str, from: usize) -> Option<usize> {
    let byte = haystack.byte_at_utf16_ceil(from)?;
    haystack.value[byte..].find(needle).and_then(|at| haystack.utf16_at_byte(byte + at))
}

/// Each text's tokens, words and keys, read once per call.
#[derive(Default)]
struct Reader {
    tokens: HashMap<String, std::sync::Arc<Vec<String>>>,
    exact: HashMap<String, String>,
    /// The source document's text (trimmed) by where it is, and what is read of it once for every quotation.
    whole: Option<(usize, usize)>,
    whole_words: Option<std::sync::Arc<LongWords>>,
    whole_positions: Option<std::sync::Arc<Vec<usize>>>,
    /// The document's text's coordinates, and its lower-cased text with its coordinates.
    whole_text: Option<std::sync::Arc<(ScalarCoordinates, ScalarCoordinates, String)>>,
    /// A long document's runs of letters and digits, where its anchor phrases are looked for.
    whole_runs: std::sync::OnceLock<Option<std::sync::Arc<TextRuns>>>,
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
    let raw = TOKEN.find_iter(text).map(|found| (found.start(), found.end())).collect::<Vec<_>>();
    let joins = |&(start, end): &(usize, usize)| mergeable(&text[start..end]);
    let mut merged = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let mut next = i + 1;
        while next < raw.len() && raw[next - 1].1 == raw[next].0 && joins(&raw[next - 1]) && joins(&raw[next]) { next += 1; }
        let (start, end) = (raw[i].0, raw[next - 1].1);
        merged.push(Token { text: text[start..end].to_owned(), start, end });
        i = next;
    }
    merged
}

impl Reader {
    fn tokens(&mut self, text: &str) -> std::sync::Arc<Vec<String>> {
        if let Some(found) = self.tokens.get(text) { return found.clone(); }
        // Pure readings, so forgetting them changes nothing: a region search reads a new window at every step.
        if self.tokens.len() >= 8192 { self.tokens.clear(); }
        let value = std::sync::Arc::new(quote_tokens(text).into_iter().map(|token| token.text).collect::<Vec<_>>());
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
    // A hyphen joining two words, spaced or not, is a space: (?<=[\p{L}\p{N}])\s*-\s*(?=[\p{L}\p{N}]); then every run of
    // blanks (\s+) one space.
    let mut collapsed = String::with_capacity(text.len());
    let mut blank = false;
    let mut push = |c: char| {
        if !c.is_whitespace() { collapsed.push(c); } else if !blank { collapsed.push(' '); }
        blank = c.is_whitespace();
    };
    let mut i = 0;
    while i < value.len() {
        if i > 0 && is_word_char(value[i - 1]) {
            let mut j = i;
            while j < value.len() && js_space(value[j]) { j += 1; }
            if j < value.len() && value[j] == '-' {
                let mut k = j + 1;
                while k < value.len() && js_space(value[k]) { k += 1; }
                if k < value.len() && is_word_char(value[k]) {
                    push(' ');
                    i = k;
                    continue;
                }
            }
        }
        push(value[i]);
        i += 1;
    }
    let mut value = js_trim(&collapsed);
    let quote = |c: char| matches!(c, '"' | '\u{201c}' | '\u{201d}' | '\u{ab}' | '\u{bb}' | '\u{201e}' | '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}');
    let mut ends = value.chars();
    if let (Some(first), Some(last)) = (ends.next(), ends.next_back()) {
        if quote(first) && quote(last) { value = js_trim(&value[first.len_utf8()..value.len() - last.len_utf8()]); }
    }
    // A closing quotation mark before the sentence's punctuation: (?<=[A-Za-z0-9])"\s*(?=[.,;:!?…]).
    let chars = value.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
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
                    return Some(utf16_len(&haystack[..found.start()]));
                }
                from = found.start() + haystack[found.start()..].chars().next().map_or(1, char::len_utf8);
            }
            None
        });
    }
    let lower = haystack.to_lowercase();
    index_of(&ScalarText::new(&lower), &needle.to_lowercase(), 0)
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

/// Python difflib's matching blocks without junk: the longest common run first, then each side of it, as (tag, a0, a1,
/// b0, b1). The one sequence diff: the quotation diff, Word's tracked corrections and a quotation's character plan read it.
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
/// The fewest inserted and deleted UTF-16 units that turn `text` into `target`, as (start, length, replacement) edits in
/// ascending order: a unit LCS, each gap between matched units one edit. A side longer than 200 units is replaced whole;
/// equal texts plan nothing. The ALR macro's plan (ALR_Rules.bas ALR_PlanTextEdits, Option Compare Binary): its
/// tie-breaks, and its units as VBA's Len and Mid$ count them.
fn minimal_edit_plan(text: &[u16], target: &[u16]) -> Vec<(usize, usize, String)> {
    if text == target {
        return Vec::new();
    }
    let (a, b) = (text.len(), target.len());
    if a > 200 || b > 200 {
        return vec![(0, a, String::from_utf16_lossy(target))];
    }
    let width = b + 1;
    let mut longest = vec![0u32; (a + 1) * width];
    for i in (0..a).rev() {
        for j in (0..b).rev() {
            longest[i * width + j] = if text[i] == target[j] {
                longest[(i + 1) * width + j + 1] + 1
            } else {
                longest[(i + 1) * width + j].max(longest[i * width + j + 1])
            };
        }
    }
    let mut plan = Vec::new();
    let (mut i, mut j, mut gap) = (0, 0, None::<(usize, usize)>);
    while i < a || j < b {
        if i < a && j < b && text[i] == target[j] {
            if let Some((from, at)) = gap.take() {
                plan.push((from, i - from, String::from_utf16_lossy(&target[at..j])));
            }
            i += 1;
            j += 1;
            continue;
        }
        gap.get_or_insert((i, j));
        if i < a && j < b {
            if longest[(i + 1) * width + j] >= longest[i * width + j + 1] { i += 1 } else { j += 1 }
        } else if i < a {
            i += 1;
        } else {
            j += 1;
        }
    }
    if let Some((from, at)) = gap {
        plan.push((from, a - from, String::from_utf16_lossy(&target[at..])));
    }
    plan
}

/// The characters that differ between `text` and `target`, as (start, length, replacement) edits in UTF-16: the ALR
/// macro's plan, and where that plan replaces a long text whole (its 200-unit cutoff), the character alignment of the
/// sequence diff, so a highlight still shows which letters differ.
pub fn character_plan(text: &str, target: &str) -> Vec<(usize, usize, String)> {
    let units = text.encode_utf16().collect::<Vec<_>>();
    let plan = minimal_edit_plan(&units, &target.encode_utf16().collect::<Vec<_>>());
    if !(plan.len() == 1 && plan[0].0 == 0 && plan[0].1 == units.len() && !text.is_empty() && !target.is_empty()) {
        return plan;
    }
    let (left, right) = (text.chars().collect::<Vec<_>>(), target.chars().collect::<Vec<_>>());
    let offset = |index: usize| left[..index].iter().map(|c| c.len_utf16()).sum::<usize>();
    sequence_opcodes(&left, &right)
        .into_iter()
        .filter(|(tag, ..)| *tag != "equal")
        .map(|(_, a0, a1, b0, b1)| (offset(a0), offset(a1) - offset(a0), right[b0..b1].iter().collect()))
        .collect()
}

/// `text` (at `base`, in UTF-16) as runs (start, end, edited), marking what `plan` changes: the characters an edit removes
/// or replaces; the characters an edit brackets (an editorial target writes an authored word the source does not have
/// as "[word]"); and for any other edit that only inserts, the space at that point (a missing word), else the character
/// after it, or the last character at the text's end. Runs cover `text` in order.
pub fn highlight_runs(text: &str, plan: &[(usize, usize, String)], base: usize) -> Vec<(usize, usize, bool)> {
    let units = text.encode_utf16().collect::<Vec<_>>();
    let length = units.len();
    let mut edited = vec![false; length];
    let mut fill = |from: usize, to: usize| {
        for slot in &mut edited[from.min(length)..to.min(length).max(from.min(length))] {
            *slot = true;
        }
    };
    let mut edits = plan.iter().collect::<Vec<_>>();
    edits.sort_by_key(|edit| edit.0);
    let mut marks = Vec::new();
    for (index, (start, edit_length, replacement)) in edits.iter().enumerate() {
        let at = start.saturating_sub(base);
        if *edit_length > 0 {
            fill(at, at + edit_length);
            continue;
        }
        let close = replacement.ends_with('[')
            .then(|| edits[index + 1..].iter().find(|(_, _, text)| text.starts_with(']')))
            .flatten();
        if let Some((close, _, _)) = close {
            fill(at, close.saturating_sub(base));
            continue;
        }
        if replacement.starts_with(']') && edits[..index].iter().any(|(_, _, text)| text.ends_with('[')) {
            continue;
        }
        if length > 0 {
            let space = at > 0 && at <= length && char::from_u32(u32::from(units[at - 1])).is_some_and(js_space);
            marks.push(if space { at - 1 } else if at < length { at } else { length - 1 });
        }
    }
    for at in marks {
        edited[at] = true;
    }
    let mut runs = Vec::new();
    let mut i = 0;
    while i < length {
        let mut j = i + 1;
        while j < length && edited[j] == edited[i] {
            j += 1;
        }
        runs.push((base + i, base + j, edited[i]));
        i = j;
    }
    runs
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
    let words = lowercase_words(source);
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
    let lower = text.to_lowercase();
    let lower = ScalarText::new(&lower);
    let mut positions = Vec::with_capacity(source.len());
    let mut from = 0usize;
    for token in source.iter() {
        let at = index_of(&lower, &token.to_lowercase(), from).unwrap_or(from);
        positions.push(at);
        from = at + 1.max(utf16_len(token));
    }
    positions
}

/// What every window of a source's tokens is checked against for one quotation, prepared once.
struct Windows {
    source: std::sync::Arc<Vec<String>>,
    /// The quotation's tokens short of trailing punctuation, and its exact key.
    core: Vec<String>,
    key: String,
    /// For each of the key's anchor words (see exact_anchors), how many of the source's first n tokens could hold it;
    /// None where the key begins with "[", and every window is searched.
    anchored: Option<Vec<Vec<usize>>>,
    /// The quotation's words and each source token's word as a bracketed suffix aligns them, where it has such a suffix.
    aligned: Option<(Vec<String>, Vec<Option<String>>)>,
    /// How often the quotation holds each of its distinct tokens, and which of them each source token is (u32::MAX: none).
    wanted: Vec<usize>,
    slots: Vec<u32>,
}

impl Windows {
    fn new(source: std::sync::Arc<Vec<String>>, quoted: &[String], key: String) -> Self {
        let core = strip_trailing(quoted);
        let anchored = exact_anchors(&key).map(|anchors| {
            // Each token as the window's key reads it: lower-cased (ASCII is compared case aside), and the long s as a
            // case-insensitive single word matches it.
            let runs = source.iter().map(|token| if token.is_ascii() { std::borrow::Cow::Borrowed(token.as_str()) }
                else { token.to_lowercase().replace('\u{17f}', "s").into() }).collect::<Vec<_>>();
            anchors.iter().map(|anchor| {
                let mut counts = Vec::with_capacity(source.len() + 1);
                counts.push(0);
                for token in &runs {
                    let holds = token.split(|c: char| !is_word_char(c)).any(|run| anchor.fits(run));
                    counts.push(counts.last().unwrap() + usize::from(holds));
                }
                counts
            }).collect()
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

    /// Whether the window's text could hold the quotation's key word for word: it holds each of the key's anchor words,
    /// or it begins where a "[T]he" read as "The" could change what it holds.
    fn may_hold_exactly(&self, start: usize, end: usize) -> bool {
        if self.key.is_empty() { return false; }
        let Some(anchored) = &self.anchored else { return true };
        anchored.iter().all(|counts| counts[end] > counts[start])
            || self.source[start..end.min(start + 3)].iter().any(|token| token.contains('['))
    }
}

/// A run of letters and digits a text holding a key word for word holds: as one of its own runs where the key has no
/// letter or digit on either side of it, else as the start or end of one, or inside one.
struct Anchor {
    run: String,
    left: bool,
    right: bool,
}

impl Anchor {
    /// Whether a run, lower-cased but for its ASCII, holds this one.
    fn fits(&self, run: &str) -> bool {
        let (run, anchor) = (run.as_bytes(), self.run.as_bytes());
        let at = |start: usize| run[start..start + anchor.len()].eq_ignore_ascii_case(anchor);
        if run.len() < anchor.len() { return false; }
        match (self.left, self.right) {
            (true, true) => run.len() == anchor.len() && at(0),
            (true, false) => at(0),
            (false, true) => at(run.len() - anchor.len()),
            (false, false) => (0..=run.len() - anchor.len()).any(at),
        }
    }
}

/// The runs of letters and digits any text holding `key` word for word (exact_in_source) holds, each within one of its
/// tokens' runs as the key's own neighbours bound it (a single word's whole, as it is matched): the key's ASCII runs.
/// None where the key begins with "[", which "[T]he" read as "The" joins to the run after it.
fn exact_anchors(key: &str) -> Option<Vec<Anchor>> {
    if key.is_empty() || key.starts_with('[') { return None; }
    let single = SINGLE_WORD.is_match(js_trim(key));
    let chars = key.chars().collect::<Vec<_>>();
    let mut anchors: Vec<Anchor> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_word_char(chars[i]) { i += 1; continue; }
        let start = i;
        while i < chars.len() && is_word_char(chars[i]) { i += 1; }
        let run = chars[start..i].iter().collect::<String>();
        if run.is_ascii() { anchors.push(Anchor { run, left: single || start > 0, right: single || i < chars.len() }); }
    }
    Some(anchors)
}

fn content_words(text: &str) -> Vec<String> {
    lowercase_words(text).into_iter().filter(|word| !NON_CONTENT.contains(&word.as_str()) && !NON_CONTENT_MORE.contains(&word.as_str())
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

/// A long text's runs of letters and digits in order: where each begins, and a key every casing of it shares.
struct TextRuns {
    starts: Vec<usize>,
    keys: Vec<u64>,
    by_key: HashMap<u64, Vec<usize>>,
}

static RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").unwrap());

/// What a run shares with every run a case-insensitive pattern matches it to: its ASCII lower-cased, the long s and
/// the Kelvin sign with them, every other character alike.
fn run_key(run: &str) -> u64 {
    run.chars().fold(0u64, |key, c| {
        let c = match c { '\u{17f}' => 's', '\u{212a}' => 'k', c if c.is_ascii() => c.to_ascii_lowercase(), _ => '\u{fffd}' };
        (key.rotate_left(5) ^ c as u64).wrapping_mul(0x517c_c1b7_2722_0a95)
    })
}

impl TextRuns {
    /// None where the text holds U+0345, which is no letter yet matches iota in any case.
    fn new(text: &str) -> Option<Self> {
        if text.contains('\u{345}') { return None; }
        let (mut starts, mut keys, mut by_key) = (Vec::new(), Vec::new(), HashMap::<u64, Vec<usize>>::new());
        for run in RUN.find_iter(text) {
            let key = run_key(run.as_str());
            by_key.entry(key).or_default().push(starts.len());
            starts.push(run.start());
            keys.push(key);
        }
        Some(TextRuns { starts, keys, by_key })
    }

    /// Where a match of `phrase` beginning and ending at a word's edge can begin: at a run, the phrase's runs standing
    /// in a row from it, since what joins them in the phrase or between its words is no letter or digit in any case.
    fn starts(&self, phrase: &[String]) -> Vec<usize> {
        let wanted = phrase.iter().flat_map(|word| RUN.find_iter(word).map(|run| run_key(run.as_str()))).collect::<Vec<_>>();
        self.by_key.get(&wanted[0]).into_iter().flatten().copied()
            .filter(|&at| self.keys.get(at..at + wanted.len()).is_some_and(|keys| *keys == wanted[..]))
            .map(|at| self.starts[at]).collect()
    }
}

/// The text around the quotation's first anchor phrase found whole, its words separated by anything but letters and
/// digits, case aside.
fn long_source_region(quote: &str, source: &ScalarText, runs: Option<&TextRuns>, window: usize) -> String {
    let text = source.value;
    let words = lowercase_words(quote);
    let edge = |c: Option<char>| c.is_some_and(|c| is_word_char(c) || c == '_');
    let edges = |start: usize, end: usize| !edge(text[..start].chars().next_back()) && !edge(text[end..].chars().next());
    for phrase in anchor_phrases(&words) {
        let pattern = phrase.iter().map(|word| regex::escape(word)).collect::<Vec<_>>().join(r"[^\p{L}\p{N}_]+");
        // A match of a phrase beginning with a letter or digit begins a run; the text is searched throughout otherwise.
        let found = match runs.filter(|_| RUN.find(&phrase[0]).is_some_and(|run| run.start() == 0)) {
            Some(runs) => {
                let starts = runs.starts(&phrase);
                if starts.is_empty() { continue; }
                let Ok(pattern) = Regex::new(&format!("^(?i:{pattern})")) else { continue };
                starts.into_iter().find_map(|start| pattern.find(&text[start..]).map(|found| (start, start + found.end()))
                    .filter(|&(start, end)| edges(start, end)))
            }
            None => {
                let Ok(pattern) = Regex::new(&format!("(?i){pattern}")) else { continue };
                let (mut from, mut found) = (0, None);
                while let Some(next) = pattern.find_at(text, from) {
                    if edges(next.start(), next.end()) { found = Some((next.start(), next.end())); break; }
                    from = next.start() + text[next.start()..].chars().next().map_or(1, char::len_utf8);
                }
                found
            }
        };
        if let Some((start, end)) = found {
            let at = source.utf16_at_byte(start).unwrap_or_default();
            let length = utf16_len(&text[start..end]);
            return slice_utf16(source, at.saturating_sub(window), at + length + utf16_len(quote) + window).to_owned();
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
        let q_length = utf16_len(q);
        let head = slice_utf16(&ScalarText::new(q), 0, 40).to_owned();
        let whole;
        // The whole quotation where it stands as written, else its opening: an opening several passages share
        // finds only the first of them.
        let needles = if q_length > 40 { vec![q.to_owned(), head] } else { vec![head] };
        let (source, found) = if self.is_whole(text) {
            whole = self.whole_text.get_or_insert_with(|| {
                let lower = text.to_lowercase();
                std::sync::Arc::new((ScalarCoordinates::new(text), ScalarCoordinates::new(&lower), lower))
            }).clone();
            // find_exact, the document's lower-cased text read once.
            let lower = ScalarText::with_coordinates(&whole.2, &whole.1);
            let found = needles.iter().find_map(|needle| {
                let needle = js_trim(needle);
                if needle.is_empty() { None } else if SINGLE_WORD.is_match(needle) { find_exact(needle, text) }
                    else { index_of(&lower, &needle.to_lowercase(), 0) }
            });
            (ScalarText::with_coordinates(text, &whole.0), found)
        } else {
            (ScalarText::new(text), needles.iter().find_map(|needle| find_exact(needle, text)))
        };
        if let Some(at) = found {
            return slice_utf16(&source, at.saturating_sub(window / 2), at + q_length + window / 2).to_owned();
        }
        if source.utf16_len() > LONG_SOURCE_CHARS {
            let runs = if self.is_whole(text) { self.whole_runs.get_or_init(|| TextRuns::new(text).map(std::sync::Arc::new)).clone() }
                else { TextRuns::new(text).map(std::sync::Arc::new) };
            return long_source_region(q, &source, runs.as_deref(), window);
        }
        if quoted.is_empty() { return text.to_owned(); }
        let step = 1.max(quoted.len() / 2);
        let source = self.tokens(text);
        let positions = match (&self.whole_positions, self.is_whole(text)) {
            (Some(positions), true) => positions.clone(),
            _ => {
                let positions = std::sync::Arc::new(token_positions(text, &source));
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
        slice_utf16(&ScalarText::new(text), positions[first], positions[last] + utf16_len(&source[last])).to_owned()
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
            let region = ScalarText::new(source_region);
            // A source's bracket the words begin or end inside is kept whole.
            let count = |bracket: char, start: usize, end: usize| slice_utf16(&region, start, end).matches(bracket).count();
            if count(']', start, end) > count('[', start, end) && start > 0 && slice_utf16(&region, start - 1, start) == "[" { start -= 1; }
            if count('[', start, end) > count(']', start, end) && slice_utf16(&region, end, end + 1) == "]" { end += 1; }
            // The source's punctuation after the words only as far as the quotation carries punctuation there.
            let ending = js_trim(quote).chars().rev().take_while(|c| !is_word_char(*c)).collect::<String>();
            let tail = slice_utf16(&region, end, region.utf16_len()).chars().take_while(|c| ending.contains(*c)).collect::<String>();
            let exact = format!("{}{tail}", slice_utf16(&region, start, end));
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
        let first_word = lowercase_words(quote).into_iter().next();
        if !first_word.is_some_and(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_digit())) {
            let stripped = js_trim(&LEADING_NUMBER.replace(&fragment, "")).to_owned();
            if !stripped.is_empty() { fragment = stripped; }
        }
        if fragment.is_empty() || self.only_bracketed(&fragment) || utf16_len(&fragment) > max_chars { return String::new(); }
        fragment
    }
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum QuoteBlockKind {
    Paragraph,
    /// A page by the number it prints.
    Page,
    /// A page by its place in a PDF that prints no page numbers.
    PdfPage,
    Section,
}

impl QuoteBlockKind {
    fn is_page(self) -> bool {
        matches!(self, Self::Page | Self::PdfPage)
    }
}

/// Where a quotation was found: a block's kind and the label it prints ("12" of a paragraph or a page, "3–5" of a run
/// of them, "5(2)" of a provision).
#[derive(Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct QuoteBlockLabel {
    pub kind: QuoteBlockKind,
    pub label: String,
    /// As a pinpoint is written for a reader: a paragraph or provision by its place ("par12", "sec5(2)"), "page 5",
    /// "pages 5–6", "PDF page 3".
    pub shown: String,
}

impl QuoteBlockLabel {
    fn new(block: &Block) -> Self {
        let label = &block.label;
        let shown = match block.kind {
            QuoteBlockKind::Paragraph | QuoteBlockKind::Section => {
                block.node.and_then(StructureNode::fragment).unwrap_or_else(|| format!("par{label}"))
            }
            QuoteBlockKind::Page if label.contains('\u{2013}') => format!("pages {label}"),
            QuoteBlockKind::Page => format!("page {label}"),
            QuoteBlockKind::PdfPage => format!("PDF page {label}"),
        };
        Self { shown, kind: block.kind, label: label.to_string() }
    }
}

/// How a source's locators are read: a provider's text by every block its structure labels; a PDF by what it prints,
/// its numbered paragraphs, provisions and page numbers; a statute's PDF likewise, its provisions the parse's sections.
#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum QuoteSource {
    Text,
    Pdf,
    Legislation,
}

/// A paragraph, page or provision a quotation is located in: a node the query serves, read as `kind` under `label`
/// (a paragraph's or page's number as printed, a provision's label), or a cited run of them joined (no node).
#[derive(Clone)]
struct Block<'a> {
    node: Option<&'a StructureNode>,
    kind: QuoteBlockKind,
    label: Cow<'a, str>,
    ranges: Cow<'a, [ScalarRange]>,
    text: String,
}

impl Block<'_> {
    fn start(&self) -> usize {
        self.ranges.first().map_or(0, |range| range.start)
    }

    /// The number its label prints, where it is a whole number.
    fn number(&self) -> Option<u64> {
        self.label.parse().ok()
    }
}

/// The document's blocks a quotation is located among, from the nodes the query serves. A PDF uses the paragraph
/// numbers it prints, its sections and the page numbers it prints, front matter's and every sequence's as printed; a
/// PDF that prints no page numbers, its PDF pages.
fn document_quote_blocks<'a>(document: &'a DocumentStructure, query: &'a DocumentQuery, source: QuoteSource) -> Vec<Block<'a>> {
    let pdf = source != QuoteSource::Text;
    let blocks = query.blocks(document, None).collect::<Vec<_>>();
    let block = |node: &'a StructureNode, kind: QuoteBlockKind, label: Cow<'a, str>| {
        let mut ranges = document.query_ranges(node);
        if !ranges.windows(2).all(|pair| pair[0].end < pair[1].start) {
            ScalarRange::normalize_union(ranges.to_mut());
        }
        Block { node: Some(node), kind, label, ranges, text: String::new() }
    };
    let mut printed = Vec::new();
    if pdf {
        // A paragraph's number is a locator only where the PDF prints it.
        printed = blocks.iter().filter(|node| node.kind == NodeKind::Paragraph && node.prints_label()).filter_map(|node| {
            let number = parse_ordinal("paragraph", node.label.as_deref()?)?;
            Some(block(node, QuoteBlockKind::Paragraph, number.to_string().into()))
        }).collect::<Vec<_>>();
        printed.sort_by_key(|block| (block.start(), block.ranges.last().map_or(0, |range| range.end)));
    }
    // A PDF prints its pages' numbers where a page holds the label it prints (`aliases`).
    let printed_page = |node: &'a StructureNode| node.aliases.as_deref().and_then(<[String]>::first);
    let numbered_pages = pdf && blocks.iter().any(|node| node.kind == NodeKind::Page && printed_page(node).is_some());
    printed.extend(blocks.into_iter().filter_map(|node| {
        let kind = match node.kind {
            NodeKind::Paragraph | NodeKind::Prose if !pdf => QuoteBlockKind::Paragraph,
            NodeKind::Page if pdf && !numbered_pages => QuoteBlockKind::PdfPage,
            NodeKind::Page => QuoteBlockKind::Page,
            NodeKind::Section => QuoteBlockKind::Section,
            _ => return None,
        };
        // A page is the number it prints, where the PDF prints them; a page printing none is no locator then.
        let value = if kind == QuoteBlockKind::Page && numbered_pages { printed_page(node)?.as_str() } else { js_trim(node.label.as_deref()?) };
        let value = value.strip_prefix('[').and_then(|value| value.strip_suffix(']')).unwrap_or(value);
        Some(block(node, kind, value.into()))
    }));
    printed
}

/// Every quotation located in the document's text among its blocks, read from `source`. A PDF's quotation is located
/// by one scheme of its locators: the kind its citation names where the PDF prints it, else the PDF's numbered
/// paragraphs, provisions or pages, the first it prints. What is read of the document is kept in `locator` for every
/// quotation located in it later.
pub fn locate_document_quotes(locator: &mut QuoteLocator, document: &DocumentStructure, query: &DocumentQuery,
    source: QuoteSource, quotes: &[QuoteToLocate], compare: bool) -> Vec<LocatedQuote> {
    use QuoteBlockKind::{Page, Paragraph, Section};
    let text = document.query_text();
    let blocks = document_quote_blocks(document, query, source);
    let mut locate = |kind: Option<QuoteBlockKind>, blocks: Vec<Block>, quotes: &[QuoteToLocate]| {
        let kept = locator.kept.remove(&(source, kind));
        let (located, state) = locate_quotes_kept(kept, document, text, blocks, quotes, compare);
        locator.kept.insert((source, kind), state);
        located
    };
    if source == QuoteSource::Text { return locate(None, blocks, quotes); }
    let printed = |kind: QuoteBlockKind| blocks.iter().any(|block| block.kind == kind);
    let scheme = |scopes: &Scopes| [(Paragraph, &scopes.paragraphs), (Page, &scopes.pages)].into_iter()
        .filter(|(_, asked)| !asked.is_empty()).map(|(kind, _)| kind)
        .chain((!scopes.sections.is_empty()).then_some(Section))
        .find(|kind| printed(*kind)).or_else(|| [Paragraph, Section, Page].into_iter().find(|kind| printed(*kind)));
    let schemes = quotes.iter().map(|quote| scheme(&Scopes::of(&quote.pinpoints))).collect::<Vec<_>>();
    let mut located = quotes.iter().map(|_| None).collect::<Vec<_>>();
    for kind in [Some(Paragraph), Some(Section), Some(Page), None] {
        let members = (0..quotes.len()).filter(|&at| schemes[at] == kind).collect::<Vec<_>>();
        if members.is_empty() { continue; }
        let kept = blocks.iter().filter(|block| Some(block.kind) == kind).cloned().collect::<Vec<_>>();
        let group = members.iter().map(|&at| quotes[at].clone()).collect::<Vec<_>>();
        for (at, found) in members.into_iter().zip(locate(kind, kept, &group)) { located[at] = Some(found); }
    }
    located.into_iter().map(|found| found.expect("every quotation is located")).collect()
}

/// What locating quotations reads of a document's text and blocks, kept by the document for every quotation located
/// in it later: each block's text and readings, the whole text's, and the reader's indexes of it.
#[derive(Default)]
pub struct QuoteLocator {
    kept: HashMap<(QuoteSource, Option<QuoteBlockKind>), LocatorState>,
}

/// What one scheme of a document's blocks read, by block in the order `document_quote_blocks` gives them.
struct LocatorState {
    reader: Reader,
    texts: Vec<String>,
    prepared: Vec<Option<std::sync::Arc<Prepared>>>,
    whole_prepared: Option<std::sync::Arc<Prepared>>,
    /// The whole text as a quotation found nowhere in it is compared with it.
    whole_compared: Option<ComparedText>,
}

/// The paragraphs, pages and provisions a citation's pinpoints name: whole numbers of paragraphs and pages, first to
/// last, and provisions by their place ("sec5(2)").
#[derive(Default)]
struct Scopes {
    paragraphs: Vec<(u64, u64)>,
    pages: Vec<(u64, u64)>,
    sections: Vec<String>,
}

impl Scopes {
    fn of(pinpoints: &[CitedPinpoint]) -> Self {
        let mut scopes = Self::default();
        for pinpoint in pinpoints {
            let Some(first) = pinpoint.first.as_deref().filter(|first| !first.is_empty()) else { continue };
            let mark = provision_mark(Some(&pinpoint.kind));
            let ranges = match pinpoint.kind.as_str() {
                // A provision by its place as its node's fragment names it: its kind's mark and its label.
                _ if !mark.is_empty() => {
                    scopes.sections.push(format!("{mark}{}", first.chars().filter(|c| !js_space(*c)).collect::<String>()));
                    continue;
                }
                "paragraph" => &mut scopes.paragraphs,
                "page" => &mut scopes.pages,
                _ => continue,
            };
            let number = |value: &str| js_trim(value).parse::<u64>().ok();
            if let (Some(first), Some(last)) = (number(first), number(pinpoint.last.as_deref().unwrap_or(first))) {
                ranges.push((first, first.max(last)));
            }
        }
        scopes
    }

    fn any(&self) -> bool {
        !self.paragraphs.is_empty() || !self.pages.is_empty() || !self.sections.is_empty()
    }
}

/// A pinpoint of the citation a quotation is for, as the citation engine read it: its kind ("paragraph", "page",
/// "section") and its first locator and the last of a range.
#[derive(Clone, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct CitedPinpoint {
    pub kind: String,
    #[serde(default)]
    pub first: Option<String>,
    #[serde(default)]
    pub last: Option<String>,
}

#[derive(Clone, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct QuoteToLocate {
    pub quote: String,
    #[serde(default)]
    pub pinpoints: Vec<CitedPinpoint>,
    /// The quotation's id, which a host's checks of it name the evidence by.
    #[serde(default)]
    pub id: Option<String>,
}

/// A located quotation's part in the passage it was found in: the region of the passage, the source's words for the
/// quotation, and the quotation corrected to them.
#[derive(Serialize, Default)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QuoteCorrection {
    pub region: String,
    pub fragment: String,
    pub corrected: String,
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CitedPartialMatch {
    pub score: f64,
    pub labels: Vec<QuoteBlockLabel>,
    pub pinpoint: String,
    #[serde(flatten)]
    pub correction: QuoteCorrection,
}

/// Where a quotation is in its source: `location` is "unmatched" where it is not found. `perfect`: found word for
/// word where the citation sends a reader.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct QuoteLocation {
    pub location: String,
    pub score: f64,
    pub labels: Vec<QuoteBlockLabel>,
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
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct LocatedQuote {
    #[serde(rename = "match")]
    pub found: QuoteLocation,
    /// None where the caller asked for the location alone.
    pub comparison: Option<QuoteComparison>,
}

fn in_ranges(value: Option<u64>, ranges: &[(u64, u64)]) -> bool {
    value.is_some_and(|value| ranges.iter().any(|&(first, last)| first <= value && value <= last))
}
/// A cited provision's place and the places of those holding it, nearest first: "sec5(2)(a)", "sec5(2)", "sec5".
fn holders(place: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(place), |place| {
        let rest = place.strip_suffix(')')?;
        let at = rest.rfind('(')?;
        (at + 1 < rest.len() && !rest[at + 1..].contains(')')).then(|| &rest[..at])
    })
}
fn word_count(quote: &str, text: &str) -> usize {
    let (needle, words) = (lowercase_words(quote), lowercase_words(text));
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

/// A block a quotation is scored against: one of the source's, or the cited range of them joined.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Candidate {
    Block(usize),
    Range(usize),
}

struct Located<'a> {
    reader: Reader,
    document: &'a DocumentStructure,
    /// The document's nodes by id, read where one block is asked whether it holds another.
    by_id: std::cell::OnceCell<HashMap<&'a str, &'a StructureNode>>,
    text: &'a str,
    source_text: ScalarText<'a>,
    blocks: Vec<Block<'a>>,
    /// Each block's readings, and the whole text's, prepared once for every quotation.
    prepared: Vec<Option<std::sync::Arc<Prepared>>>,
    whole: Option<std::sync::Arc<Prepared>>,
}

/// A text's readings a quotation is scored against (quote_match_score's of `source`), prepared once.
struct Prepared {
    text: String,
    length: usize,
    key: String,
    lower: String,
    /// A long source's tokens are read only where a quotation's bracketed suffix is looked for in them.
    tokens: std::sync::OnceLock<std::sync::Arc<Vec<String>>>,
    counts: HashMap<String, usize>,
    /// A long source's words, which a long source is scored by.
    words: Option<std::sync::Arc<LongWords>>,
}

impl Prepared {
    fn tokens(&self) -> &[String] {
        self.tokens.get_or_init(|| std::sync::Arc::new(token_texts(&self.text)))
    }
}

/// A quotation's readings (quote_match_score's of `quote`), prepared once for every block.
struct Quoted {
    empty: bool,
    core: Vec<String>,
    key: String,
    single: bool,
    needle: String,
    aliases: HashMap<String, String>,
    counts: HashMap<String, usize>,
}

impl Reader {
    fn prepare(&mut self, source: &str) -> Prepared {
        let trimmed = js_trim(source);
        let key = self.exact_key(trimmed);
        let lower = key.to_lowercase();
        let length = utf16_len(trimmed);
        if length > LONG_SOURCE_CHARS {
            return Prepared { length, lower, key, tokens: Default::default(), counts: HashMap::new(),
                words: Some(self.long_words(trimmed)), text: trimmed.to_owned() };
        }
        let tokens = self.tokens(trimmed);
        let mut counts = HashMap::new();
        for token in tokens.iter() { *counts.entry(token.clone()).or_default() += 1; }
        let words = (tokens.len() > LONG_SOURCE_TOKENS).then(|| self.long_words(trimmed));
        Prepared { length, lower, key, tokens: tokens.into(), counts, words, text: trimmed.to_owned() }
    }
    fn is_whole(&self, text: &str) -> bool {
        self.whole == Some((text.as_ptr() as usize, text.len()))
    }
    fn long_words(&mut self, text: &str) -> std::sync::Arc<LongWords> {
        if !self.is_whole(text) { return std::sync::Arc::new(long_words(text)); }
        self.whole_words.get_or_insert_with(|| std::sync::Arc::new(long_words(text))).clone()
    }
    fn quoted(&mut self, quote: &str) -> Quoted {
        let q = js_trim(quote);
        let core = strip_trailing(&self.tokens(q));
        let key = self.exact_key(q);
        let mut counts = HashMap::new();
        for token in &core { *counts.entry(token.clone()).or_default() += 1; }
        Quoted { empty: q.is_empty(), single: SINGLE_WORD.is_match(js_trim(&key)), needle: js_trim(&key).to_lowercase(),
            aliases: alignment_aliases(&core), key, core, counts }
    }
}

/// quote_match_score from prepared readings.
fn prepared_score(quoted: &Quoted, source: &Prepared, floor: f64) -> f64 {
    if quoted.empty || source.text.is_empty() || quoted.core.is_empty() { return 0.0; }
    if !quoted.key.is_empty() && prepared_exact(quoted, source) { return 1.0; }
    if !quoted.aliases.is_empty() && bracket_suffix_in(&quoted.core, &quoted.aliases, source.tokens()) { return 1.0; }
    if source.length > LONG_SOURCE_CHARS { return long_source_score(&quoted.core, source.words.as_ref().expect("long words")); }
    if source.tokens().is_empty() { return 0.0; }
    if source.tokens().len() > LONG_SOURCE_TOKENS { return long_source_score(&quoted.core, source.words.as_ref().expect("long words")); }
    if floor > 0.0 {
        let shared: usize = quoted.counts.iter().map(|(token, count)| (*count).min(source.counts.get(token).copied().unwrap_or(0))).sum();
        if (shared as f64) / (quoted.core.len() as f64) < floor { return 0.0; }
    }
    matched_size(source.tokens(), &quoted.core) as f64 / quoted.core.len() as f64
}
/// exact_in_source of the quotation's key in the source's, from prepared readings.
fn prepared_exact(quoted: &Quoted, source: &Prepared) -> bool {
    let found = if quoted.needle.is_empty() || source.key.is_empty() { false }
        else if quoted.single { find_exact(&quoted.key, &source.key).is_some() }
        else { source.lower.contains(quoted.needle.as_str()) };
    if found { return true; }
    let (q, s) = (collapse_initial_case(&quoted.key), collapse_initial_case(&source.key));
    (q != quoted.key || s != source.key) && find_exact(&q, &s).is_some()
}

struct Resolution {
    location: &'static str,
    score: f64,
    labels: Vec<QuoteBlockLabel>,
    text: String,
    cited: Option<(f64, Vec<QuoteBlockLabel>, String)>,
}

impl<'a> Located<'a> {
    fn block<'b>(&'b self, ranges: &'b [Block<'a>], candidate: Candidate) -> &'b Block<'a> {
        match candidate { Candidate::Block(index) => &self.blocks[index], Candidate::Range(index) => &ranges[index] }
    }

    /// Whether `inner` is a smaller block of the same kind inside `outer`: a node under it, or a joined run inside
    /// another.
    fn within(&self, outer: &Block, inner: &Block) -> bool {
        let extent = |block: &Block| block.ranges.first().zip(block.ranges.last()).map(|(first, last)| (first.start, last.end));
        inner.kind == outer.kind
            // Its members' extent first: most blocks are not inside each other, and this alone says so.
            && extent(inner).zip(extent(outer)).is_none_or(|(inner, outer)| outer.0 <= inner.0 && inner.1 <= outer.1)
            && inner.ranges.iter().all(|part| outer.ranges.iter().any(|range| range.start <= part.start && part.end <= range.end))
            && match outer.node.zip(inner.node) {
                Some((outer, inner)) => {
                    let by_id = self.by_id.get_or_init(|| self.document.nodes.iter().map(|node| (node.id.as_str(), node)).collect());
                    let mut parent = inner.parent_id.as_deref();
                    // (A parent cycle ends where the walk has taken as many steps as there are nodes.)
                    let mut steps = by_id.len();
                    while let (Some(id), true) = (parent, steps > 0) {
                        if id == outer.id { return true; }
                        parent = by_id.get(id).and_then(|node| node.parent_id.as_deref());
                        steps -= 1;
                    }
                    false
                }
                None => outer.ranges.iter().map(|range| range.end - range.start).sum::<usize>()
                    > inner.ranges.iter().map(|range| range.end - range.start).sum::<usize>(),
            }
    }

    /// The cited blocks, each with whether it is the one cited rather than a provision holding it: every paragraph
    /// and page of a cited number, and of a cited provision the last one of its place, else of its nearest holder's
    /// (a2aj_pinpoint_scope._cited_block_selection).
    fn cited_selection(&self, scopes: &Scopes) -> Vec<(usize, bool)> {
        let mut selected: Vec<(usize, bool)> = Vec::new();
        let set = |index: usize, value: bool, selected: &mut Vec<(usize, bool)>| {
            match selected.iter_mut().find(|(at, _)| *at == index) {
                Some(entry) => entry.1 = value,
                None => selected.push((index, value)),
            }
        };
        for (index, block) in self.blocks.iter().enumerate() {
            if block.kind == QuoteBlockKind::Paragraph && in_ranges(block.number(), &scopes.paragraphs) { set(index, true, &mut selected); }
            if block.kind.is_page() && in_ranges(block.number(), &scopes.pages) { set(index, true, &mut selected); }
        }
        let mut sections = HashMap::new();
        for (index, block) in self.blocks.iter().enumerate() {
            // (A citation's section scope names a provision by its place: "sec5(2)".)
            if block.kind == QuoteBlockKind::Section {
                let place = block.node.and_then(StructureNode::fragment).unwrap_or_else(|| block.label.to_string());
                sections.insert(place.to_lowercase(), index);
            }
        }
        for section in &scopes.sections {
            for candidate in holders(section) {
                if let Some(&index) = sections.get(&candidate.to_lowercase()) {
                    let before = selected.iter().find(|(at, _)| *at == index).is_some_and(|(_, value)| *value);
                    set(index, before || candidate.len() == section.len(), &mut selected);
                    break;
                }
            }
        }
        selected
    }

    fn range_blocks(&self, scopes: &Scopes) -> Vec<Block<'a>> {
        let mut combined = Vec::new();
        for (kind, ranges) in [(QuoteBlockKind::Paragraph, &scopes.paragraphs), (QuoteBlockKind::Page, &scopes.pages)] {
            for &(start, end) in ranges.iter() {
                if end <= start { continue; }
                let mut members = self.blocks.iter().filter_map(|block| {
                    let number = block.number().filter(|number| start <= *number && *number <= end)?;
                    (block.kind == kind || kind.is_page() && block.kind.is_page()).then_some((number, block))
                }).collect::<Vec<_>>();
                members.sort_by_key(|(number, _)| *number);
                if members.len() as u64 != end - start + 1
                    || members.iter().zip(start..).any(|((number, _), wanted)| *number != wanted) {
                    continue;
                }
                let mut ranges = members.iter().flat_map(|(_, block)| block.ranges.iter().copied()).collect();
                ScalarRange::normalize_union(&mut ranges);
                combined.push(Block { node: None, kind, label: format!("{start}\u{2013}{end}").into(),
                    text: legal_structure_model::text::membership_text_utf16(&self.source_text, &ranges), ranges: ranges.into() });
            }
        }
        combined
    }

    fn scored(&mut self, quote: &str, candidates: &[Candidate], ranges: &[Block<'a>]) -> Vec<(f64, Candidate)> {
        let mut out = Vec::new();
        let quoted = self.reader.quoted(quote);
        for &candidate in candidates {
            // The block's text is read where it is: each quotation is scored against every block of a document.
            let (text, kept) = match candidate {
                Candidate::Block(index) => (self.blocks[index].text.as_str(), Some(&mut self.prepared[index])),
                Candidate::Range(index) => (ranges[index].text.as_str(), None),
            };
            let prepared = match kept {
                Some(Some(prepared)) => prepared.clone(),
                Some(slot) => slot.insert(std::sync::Arc::new(self.reader.prepare(text))).clone(),
                None => std::sync::Arc::new(self.reader.prepare(text)),
            };
            let score = prepared_score(&quoted, &prepared, MIN_MATCH);
            if score >= MIN_MATCH && {
                let region = self.reader.quote_region(quote, text, 400);
                plausible_content_overlap(quote, if region.is_empty() { text } else { &region })
            } { out.push((score, candidate)); }
        }
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            .then(self.block(ranges, a.1).start().cmp(&self.block(ranges, b.1).start())));
        out
    }

    fn specific_labels(&self, matches: &[(f64, Candidate)], ranges: &[Block<'a>]) -> Vec<QuoteBlockLabel> {
        let mut labels: Vec<QuoteBlockLabel> = Vec::new();
        for &(score, candidate) in matches {
            let block = self.block(ranges, candidate);
            if labels.iter().any(|known| known.kind == block.kind && known.shown == QuoteBlockLabel::new(block).shown) { continue; }
            if block.kind == QuoteBlockKind::Section && matches.iter().any(|&(other, other_candidate)| {
                other >= score && self.within(block, self.block(ranges, other_candidate))
            }) { continue; }
            labels.push(QuoteBlockLabel::new(block));
        }
        labels
    }

    /// The matches whose block holds no other's of its kind scoring as well: a schedule's provision, not the schedule.
    fn most_specific(&self, matches: Vec<(f64, Candidate)>, ranges: &[Block<'a>]) -> Vec<(f64, Candidate)> {
        let keep = matches.iter().map(|&(score, candidate)| !matches.iter().any(|&(other, inner)| other >= score
            && self.within(self.block(ranges, candidate), self.block(ranges, inner)))).collect::<Vec<_>>();
        matches.into_iter().zip(keep).filter_map(|(found, keep)| keep.then_some(found)).collect()
    }

    /// The matches whose block holds the quotation as written, else case aside: those holding it as written first,
    /// then where its words run closest together, then nearest the cited places (`near`), then in order; a block
    /// giving way to one inside it that holds it.
    fn holding(&self, support: &QuoteSupport, matches: &[(f64, Candidate)], ranges: &[Block<'a>], near: &[usize])
        -> Vec<(f64, Candidate)> {
        let found = matches.iter().filter_map(|&(score, candidate)| support.hold(&self.block(ranges, candidate).text)
            .map(|hold| (hold, score, candidate))).collect::<Vec<_>>();
        let mut kept = found.iter().copied().filter(|&(.., candidate)| !found.iter().any(|&(.., inner)|
            self.within(self.block(ranges, candidate), self.block(ranges, inner)))).collect::<Vec<_>>();
        let start = |candidate: Candidate| self.block(ranges, candidate).start();
        let distance = |candidate: Candidate| near.iter().map(|&at| start(candidate).abs_diff(at)).min().unwrap_or(0);
        kept.sort_by_key(|&(hold, _, candidate)| (hold, distance(candidate), start(candidate)));
        kept.into_iter().map(|(_, score, candidate)| (score, candidate)).collect()
    }

    fn document_score(&mut self, quote: &str, text: &str) -> f64 {
        let prepared = match &self.whole {
            Some(prepared) => prepared.clone(),
            None => {
                let prepared = std::sync::Arc::new(self.reader.prepare(text));
                self.whole = Some(prepared.clone());
                prepared
            }
        };
        let whole = prepared_score(&self.reader.quoted(quote), &prepared, 0.0);
        if whole >= STRONG_MATCH { return whole; }
        let region = self.reader.quote_region(quote, text, 400);
        whole.max(self.reader.quote_match_score(quote, &region, 0.0))
    }

    fn resolve(&mut self, quote: &str, scopes: &Scopes) -> Resolution {
        let text = self.text;
        let selection = if scopes.any() { self.cited_selection(scopes) } else { Vec::new() };
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
            Resolution { location: if cited { "cited" } else { "cited_parent" }, score: matches[0].0,
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
        let near = scoped.iter().map(|&candidate| self.block(&ranges, candidate).start()).collect::<Vec<_>>();
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
                    && self.within(block, self.block(&ranges, oc)))
            }).collect::<Vec<_>>();
            if !collapsed.is_empty() && (collapsed.len() == 1 || collapsed[0].0 - collapsed[1].0 >= 0.1) {
                matches = collapsed[..1].to_vec();
            }
        }
        if !matches.is_empty() {
            let mut labels = Vec::new();
            // A word or two near words almost anywhere is placed only where it is held, or found once.
            if lowercase_words(quote).len() < 3 && !held {
                let exact = matches.iter().copied().filter(|&(_, c)| word_count(quote, &self.block(&ranges, c).text) > 0)
                    .collect::<Vec<_>>();
                if word_count(quote, text) == 1 && !exact.is_empty() { matches = exact; labels = self.specific_labels(&matches, &ranges); }
            } else {
                labels = self.specific_labels(&matches, &ranges);
            }
            let location = if !scoped.is_empty() || target_kind_available { "alternate" }
                else if scopes.any() { "scope_unavailable" } else { "uncited" };
            let cited = (location == "alternate" && !scoped_matches.is_empty()).then(|| (scoped_matches[0].0,
                self.specific_labels(&scoped_matches[..1], &ranges), self.block(&ranges, scoped_matches[0].1).text.clone()));
            return Resolution { location, score: matches[0].0, labels, text: self.block(&ranges, matches[0].1).text.clone(), cited };
        }
        let score = self.document_score(quote, text);
        if score >= MIN_MATCH {
            let location = if !scoped.is_empty() || target_kind_available { "alternate_document" }
                else if scopes.any() { "scope_unavailable_document" } else { "uncited_document" };
            return Resolution { location, score, labels: Vec::new(), text: text.to_owned(), cited: None };
        }
        Resolution { location: "unmatched", score, labels: Vec::new(), text: String::new(), cited: None }
    }

    /// Page labels where the quotation's words run, over a page break too.
    fn page_pinpoint(&mut self, quote: &str) -> String {
        let pages = self.blocks.iter().filter(|block| block.kind.is_page()).cloned().collect::<Vec<_>>();
        let mut hits = Vec::new();
        for block in &pages {
            if self.reader.quote_match_score(quote, &block.text, 0.0) >= STRONG_MATCH { hits.push(QuoteBlockLabel::new(block).shown); }
        }
        if !hits.is_empty() { return pinpoint_summary(&hits, 2); }
        for pair in pages.windows(2) {
            let mut ranges = pair.iter().flat_map(|block| block.ranges.iter().copied()).collect();
            ScalarRange::normalize_union(&mut ranges);
            let text = legal_structure_model::text::membership_text_utf16(&self.source_text, &ranges);
            if !lowercase_words(quote).is_empty()
                && self.reader.quote_match_score(quote, &text, 0.0) >= STRONG_MATCH {
                return format!("pages {}\u{2013}{}", pair[0].label, pair[1].label);
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

    fn locate(&mut self, quote: &str, scopes: &Scopes) -> QuoteLocation {
        let text = self.text;
        let mut resolution = if self.blocks.is_empty() {
            Resolution { location: "uncited_document", score: self.document_score(quote, text), labels: Vec::new(),
                text: text.to_owned(), cited: None }
        } else { self.resolve(quote, scopes) };
        let short = lowercase_words(quote).len() < 3;
        if short && resolution.location.starts_with("alternate") && resolution.labels.is_empty() {
            resolution = Resolution { location: "unmatched", score: 0.0, labels: Vec::new(), text: String::new(), cited: None };
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
        let limited = resolution.location.starts_with("scope_unavailable") || resolution.location == "cited_parent";
        let cited = resolution.cited.take().map(|(score, labels, text)| CitedPartialMatch { score,
            pinpoint: pinpoint_summary(&labels.iter().map(|label| label.shown.clone()).collect::<Vec<_>>(), 2), labels, correction: self.correction(quote, &text) });
        let pinpoint = if labeled { pinpoint_summary(&resolution.labels.iter().map(|label| label.shown.clone()).collect::<Vec<_>>(), 2) } else { self.page_pinpoint(quote) };
        let perfect = resolution.score >= STRONG_MATCH && quote_exact_key(&correction.corrected) == quote_exact_key(quote)
            && !limited;
        QuoteLocation { location: resolution.location.into(), score: resolution.score,
            labels: if labeled { resolution.labels } else { Vec::new() }, pinpoint, text: resolution.text, correction,
            perfect, short_in_long: false, cited }
    }
}

/// Every quotation of `quotes` located in `text` (the document's) among `blocks`, and with `compare`, compared with the
/// source's words where it was found (the whole text where it was not); from what `kept` read of the same blocks, and
/// with what it read now.
fn locate_quotes_kept(kept: Option<LocatorState>, document: &DocumentStructure, text: &str, mut blocks: Vec<Block>,
    quotes: &[QuoteToLocate], compare: bool) -> (Vec<LocatedQuote>, LocatorState) {
    let source_text = ScalarText::new(text);
    let trimmed = js_trim(text);
    let whole_at = (trimmed.as_ptr() as usize, trimmed.len());
    let (reader, prepared, whole, mut compared) = match kept {
        Some(state) => {
            for (block, text) in blocks.iter_mut().zip(state.texts) { block.text = text; }
            (state.reader, state.prepared, state.whole_prepared, state.whole_compared)
        }
        None => {
            for block in &mut blocks {
                block.text = legal_structure_model::text::membership_text_utf16(&source_text, &block.ranges);
            }
            (Reader { whole: Some(whole_at), ..Reader::default() }, vec![None; blocks.len()], None, None)
        }
    };
    let mut located = Located { reader, document, by_id: Default::default(), text, source_text, blocks, prepared, whole };
    // A quotation compared with the whole text reads its index and words, built once.
    let out = quotes.iter().map(|item| {
        let found = located.locate(&item.quote, &Scopes::of(&item.pinpoints));
        let passage = [found.correction.region.as_str(), found.text.as_str()].into_iter().find(|value| !value.is_empty())
            .unwrap_or(text);
        let comparison = if !compare { None } else if passage == text {
            Some(quote_text_comparison_in(&item.quote, compared.get_or_insert_with(|| ComparedText::new(text))))
        } else {
            Some(quote_text_comparison(&item.quote, passage))
        };
        LocatedQuote { comparison, found }
    }).collect::<Vec<_>>();
    let Located { reader, blocks, prepared, whole, .. } = located;
    let texts = blocks.into_iter().map(|block| block.text).collect();
    (out, LocatorState { reader, texts, prepared, whole_prepared: whole, whole_compared: compared })
}

// The quotation rewritten to the source's words, and what differs: Beaver's authoritiesDiscrepancy.ts (editorialQuote,
// verbatimRepair, quoteTextComparison, ported from the pinned Authorities oracle) and quoteCheck.ts (correctedQuote).

const DOUBLE_QUOTE_CHARS: &str = "\"\u{201c}\u{201d}\u{ab}\u{bb}\u{201e}";
static MARKS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{M}+").unwrap());
static LOWER_INITIAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[([a-z])\]([a-z]+)$").unwrap());
static BRACKETED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[^\]]+\]").unwrap());
static CLOSER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[)\]},.;:!?]$").unwrap());
static OPENER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[(\[{]$").unwrap());
static LEADING_QUOTES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^[{DOUBLE_QUOTE_CHARS}{JS_WHITESPACE_CLASS}]+")).unwrap());
static TRAILING_QUOTES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("[{DOUBLE_QUOTE_CHARS}{JS_WHITESPACE_CLASS}]+$")).unwrap());
static PUNCTUATION_TAIL: LazyLock<Regex> = LazyLock::new(||
    Regex::new(&format!(r"^(?:{JS_WHITESPACE_CLASS}*(?:\.\.\.|[.,;:!?\u{{2026}}]))+{JS_WHITESPACE_CLASS}*$")).unwrap());

fn token_texts(text: &str) -> Vec<String> {
    quote_tokens(text).into_iter().map(|token| token.text).collect()
}
/// Only substantive wording is a difference: case, diacritics, quote and dash variants and compatibility width are
/// noise.
fn fold_for_comparison(value: &str) -> String {
    // ASCII decomposes to itself and has no marks; of it, only blanks and the backtick fold otherwise.
    if value.bytes().all(|byte| byte.is_ascii_graphic() && byte != b'`') { return value.to_ascii_lowercase(); }
    fold_variants(&normalize_blank_whitespace(value))
}
/// Diacritics, quote and dash variants, compatibility width and case folded away; spacing kept as written.
fn fold_variants(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let decomposed = value.nfkd().collect::<String>();
    MARKS.replace_all(&decomposed, "").chars().map(|c| match c {
        '"' | '\u{201c}' | '\u{201d}' | '\u{ab}' | '\u{bb}' | '\u{201e}' => '"',
        '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '`' | '\u{b4}' => '\'',
        '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
        c => c,
    }).collect::<String>().to_lowercase()
}

/// How often, up to twice, `quote` stands in `text` word for word, both folded as only substantive wording differs
/// (`fold_for_comparison`) and the quotation's own edge punctuation left out: once means verbatim and once.
pub fn quote_occurrences(text: &str, quote: &str) -> usize {
    static EDGES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$").unwrap());
    let text = fold_for_comparison(text);
    let quote = EDGES.replace_all(&fold_for_comparison(quote), "").into_owned();
    if quote.is_empty() { return 0; }
    text.match_indices(&quote).take(2).count()
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct DiffPiece {
    pub text: String,
    pub changed: bool,
}
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct DiffChange {
    pub before: String,
    pub after: String,
}
/// An authored quotation and the source's words set side by side: each in pieces, those the other lacks changed, and
/// each change's words before and after.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct QuotationDiff {
    pub authored: Vec<DiffPiece>,
    pub source: Vec<DiffPiece>,
    pub changes: Vec<DiffChange>,
}

/// The two texts compared by their blanks, words and marks, case, diacritics and quote and dash variants aside.
pub fn quotation_diff(authored: &str, source: &str) -> QuotationDiff {
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(
        r"{ws}+|[\p{{L}}\p{{N}}]+(?:['\u{{2019}}][\p{{L}}\p{{N}}]+)*|[^{inner}]", ws = JS_WHITESPACE_CLASS,
        inner = JS_WHITESPACE_CLASS.trim_start_matches('[').trim_end_matches(']'))).unwrap());
    let tokens = |text: &str| TOKEN.find_iter(text).map(|found| found.as_str().to_owned()).collect::<Vec<_>>();
    let (before, after) = (tokens(authored), tokens(source));
    let opcodes = sequence_opcodes(&before.iter().map(|token| fold_variants(token)).collect::<Vec<_>>(),
        &after.iter().map(|token| fold_variants(token)).collect::<Vec<_>>());
    let pieces = |tokens: &[String], side: fn(&(&str, usize, usize, usize, usize)) -> (usize, usize)| opcodes.iter()
        .filter_map(|op| { let (from, to) = side(op);
            (from < to).then(|| DiffPiece { text: tokens[from..to].concat(), changed: op.0 != "equal" }) }).collect();
    QuotationDiff {
        authored: pieces(&before, |op| (op.1, op.2)),
        source: pieces(&after, |op| (op.3, op.4)),
        changes: opcodes.iter().filter(|op| op.0 != "equal").map(|op| DiffChange {
            before: js_trim(&before[op.1..op.2].concat()).to_owned(), after: js_trim(&after[op.3..op.4].concat()).to_owned() })
            .collect(),
    }
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
/// The same word but for the case of its first letter.
fn first_letter_case(written: &str, other: &str) -> bool {
    let (mut written, mut other) = (written.chars(), other.chars());
    match (written.next(), other.next()) {
        (Some(first), Some(other_first)) => written.as_str() == other.as_str() && first.to_lowercase().eq(other_first.to_lowercase()),
        _ => false,
    }
}
/// `[x]` around the first letter of `written`.
fn bracket_first(written: &str) -> String {
    let mut chars = written.chars();
    chars.next().map_or_else(String::new, |first| format!("[{first}]{}", chars.as_str()))
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
    normalize_blank_whitespace(&result)
}

/// A word with letters inserted inside it ("employment" written "employ[ee]ment"), the insertion bracketed.
fn internal_insertion(authored: &str, source: &str) -> String {
    if authored.is_empty() || source.is_empty() || authored.contains(['[', ']']) || source.contains(['[', ']']) {
        return String::new();
    }
    let (source, authored) = (source.chars().collect::<Vec<_>>(), authored.chars().collect::<Vec<_>>());
    let edits = sequence_opcodes(&source, &authored);
    let inserts = edits.iter().filter(|edit| edit.0 == "insert").collect::<Vec<_>>();
    if inserts.len() != 1 || edits.iter().any(|edit| edit.0 != "equal" && edit.0 != "insert") { return String::new(); }
    let equal: usize = edits.iter().filter(|edit| edit.0 == "equal").map(|edit| edit.2 - edit.1).sum();
    let inserted = inserts[0].4 - inserts[0].3;
    let length = source.len();
    if equal < 3.min(length) || inserted as f64 > 4f64.max(length as f64 / 2.0 + 1.0) { return String::new(); }
    edits.iter().map(|&(tag, a, b, c, d)| if tag == "equal" { source[a..b].iter().collect() }
        else { format!("[{}]", authored[c..d].iter().collect::<String>()) }).collect()
}

fn format_authored(authored: &[String], source: Option<&[String]>) -> Vec<String> {
    if authored.is_empty() { return Vec::new(); }
    if let Some([s]) = source {
        let a = &authored[0];
        if authored.len() == 1 && SINGLE_WORD.is_match(a) && SINGLE_WORD.is_match(s) {
            if first_letter_case(a, s) && a.chars().next() != s.chars().next() { return vec![bracket_first(a)]; }
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
                output.push(if item == written || INITIAL.is_match(written)
                        || DOUBLE_QUOTES.contains(&item.as_str()) || SINGLE_QUOTES.contains(&item.as_str()) { written.clone() }
                    else if SINGLE_WORD.is_match(item) && SINGLE_WORD.is_match(written)
                        && first_letter_case(written, item) { bracket_first(written) }
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

/// A source's words (byte spans) and their folded forms, read once for every quotation compared with it; a word is
/// folded only where a quotation is aligned with them.
pub struct SourceWords {
    spans: Vec<(usize, usize)>,
    keys: std::cell::OnceCell<Vec<String>>,
}

impl SourceWords {
    pub fn new(source: &str) -> Self {
        SourceWords { spans: word_regex().find_iter(source).map(|word| (word.start(), word.end())).collect(), keys: Default::default() }
    }
    fn keys(&self, source: &str) -> &[String] {
        self.keys.get_or_init(|| self.spans.iter().map(|&(start, end)| fold_for_comparison(&source[start..end])).collect())
    }
}

fn verbatim_repair_in(authored: &str, source: &str, words: &SourceWords) -> Option<String> {
    let before = word_regex().find_iter(authored).map(|word| fold_for_comparison(word.as_str())).collect::<Vec<_>>();
    let after = &words.spans;
    if before.is_empty() || after.is_empty() { return None; }
    // Too long to align, sequence_opcodes finds nothing equal.
    if before.len().saturating_mul(after.len()) > MAX_SEQUENCE_WORK { return None; }
    let equal = sequence_opcodes(&before, words.keys(source)).into_iter().filter(|op| op.0 == "equal").collect::<Vec<_>>();
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
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct QuoteComparisonMatch {
    pub start: usize,
    pub end: usize,
    pub text: String,
}
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct QuoteChange {
    pub kind: &'static str,
    pub authored: String,
    pub source: String,
}
/// Where the quotation's words are in the source (UTF-16 offsets), the source passage it is compared with
/// (`candidateOnly`: a close passage, not the quoted words found), the quotation rewritten to it, and what differs.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
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
    quote_text_comparison_in(authored, &ComparedText::new(source))
}

/// What comparing a quotation with a text reads of the text: its word index and UTF-16 units, and its words where a
/// verbatim repair is looked for, read once for every quotation compared with it.
struct ComparedText {
    page: legal_document_query::TextFragmentPage,
    coordinates: ScalarCoordinates,
    words: std::cell::OnceCell<SourceWords>,
}

impl ComparedText {
    fn new(source: &str) -> Self {
        ComparedText { page: legal_document_query::TextFragmentPage::new(source.to_owned()),
            coordinates: ScalarCoordinates::new(source), words: Default::default() }
    }
}

fn quote_text_comparison_in(authored: &str, text: &ComparedText) -> QuoteComparison {
    let source = text.page.text();
    let units = ScalarText::with_coordinates(source, &text.coordinates);
    let plan = legal_document_query::text_fragment_plan_in(source, &text.page, &[authored.to_owned()], false, false, false);
    let matches = if plan.source_safe_complete { plan.source_word_intervals.iter().map(|interval| QuoteComparisonMatch {
        start: interval.start, end: interval.end, text: slice_utf16(&units, interval.start, interval.end).to_owned() }).collect() }
        else { Vec::new() };
    let candidate = if matches.is_empty() {
        verbatim_repair_in(authored, source, text.words.get_or_init(|| SourceWords::new(source)))
    } else {
        let start = matches.iter().map(|found: &QuoteComparisonMatch| found.start).min().unwrap_or(0);
        Some(slice_utf16(&units, start, matches.iter().map(|found| found.end).max().unwrap_or(0)).to_owned())
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
