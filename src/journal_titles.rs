//! The journal article a citation names by the title it quotes, after the ALR Quote Verifier's journal_search.py
//! search_by_title: the searches of a journal index's names to run for it (its quoted titles' words, with the cited
//! journal, volume and first page, among articles of the cited year or the years beside it; a citation without a year
//! takes only names holding every word of the title), and the found name closest to the title, a difflib ratio of at
//! least 0.7 between normalized titles. The host runs the searches against its index.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::LazyLock;

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]+").unwrap());
static NOT_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}_\s]").unwrap());
static BLANKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\t\n\x0B\x0C\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]+").unwrap());

/// A search of the index's names for an article titled `title`, in FTS5's query syntax.
#[derive(Serialize)]
pub struct JournalTitleSearch {
    pub title: String,
    pub query: String,
}

/// A name a title's search found, in the order the searches found them.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalTitleHit {
    pub title: String,
    pub name: String,
    pub article_id: i64,
}

/// A value's words for the index: NFKC, lower-cased, of two letters or digits or more, the first eight.
fn query_tokens(value: &str) -> Vec<String> {
    use unicode_normalization::UnicodeNormalization;
    let folded = value.nfkc().collect::<String>().to_lowercase();
    TOKEN.find_iter(&folded).map(|word| word.as_str().to_owned()).filter(|word| word.encode_utf16().count() > 1)
        .take(8).collect()
}

fn quoted(words: &[String]) -> Vec<String> {
    words.iter().map(|word| format!("\"{word}\"")).collect()
}

/// How JavaScript writes a number.
fn number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 { format!("{}", value as i64) } else { format!("{value}") }
}

/// The searches to run for the articles a citation quotes the titles of.
pub fn journal_title_searches(citation: &str) -> Vec<JournalTitleSearch> {
    let call = |method: &str, request: Value| legal_citations::api::call_value(method, request).unwrap_or(Value::Null);
    let extracted = call("extract", json!({ "text": citation, "offsetUnit": "utf16" }));
    let cited = extracted["citations"].as_array().map(|citations| citations.iter()
        .filter(|citation| citation["authority"] == "journal"
            && citation["fields"]["yearNumber"].as_f64().is_some_and(|year| year != 0.0))
        .collect::<Vec<_>>()).unwrap_or_default();
    let mut years = Vec::new();
    for citation in &cited {
        let year = citation["fields"]["yearNumber"].as_f64().unwrap_or(0.0);
        for offset in [-1.0, 0.0, 1.0] {
            let value = number(year + offset);
            if !years.contains(&value) { years.push(value); }
        }
    }
    let years = quoted(&years);
    let field = |citation: &Value, name: &str| citation["fields"][name].as_str().unwrap_or("").to_owned();
    let published = cited.iter().flat_map(|citation| query_tokens(&format!("{} {} {}", field(citation, "reporter"),
        field(citation, "volume"), field(citation, "page")))).collect::<Vec<_>>();
    let titles = call("quotedTitles", json!({ "text": citation }));
    titles.as_array().into_iter().flatten().filter_map(Value::as_str).filter_map(|title| {
        let words = quoted(&query_tokens(title));
        if words.is_empty() { return None; }
        let query = if years.is_empty() { words.join(" AND ") } else {
            format!("({}) AND ({})", [words, quoted(&published)].concat().join(" OR "), years.join(" OR "))
        };
        Some(JournalTitleSearch { title: title.to_owned(), query })
    }).collect()
}

/// How alike two titles are: difflib's ratio of their letters, case, punctuation and spacing aside.
fn title_ratio(a: &str, b: &str) -> f64 {
    let fold = |value: &str| {
        let lowered = value.to_lowercase();
        let unmarked = NOT_WORD.replace_all(&lowered, "");
        let spaced = BLANKS.replace_all(&unmarked, " ");
        spaced.trim_matches(|c: char| BLANKS.is_match(c.encode_utf8(&mut [0; 4]))).chars().collect::<Vec<_>>()
    };
    let (x, y) = (fold(a), fold(b));
    if x.is_empty() && y.is_empty() { return 1.0; }
    let same: usize = crate::quote_location::sequence_opcodes(&x, &y).into_iter().filter(|op| op.0 == "equal")
        .map(|op| op.2 - op.1).sum();
    (2 * same) as f64 / (x.len() + y.len()) as f64
}

/// The article whose found name is closest to the title it was searched for, when close enough.
pub fn journal_title_choice(hits: &[JournalTitleHit]) -> Option<i64> {
    let mut best: Option<(f64, i64)> = None;
    for hit in hits {
        let score = title_ratio(&hit.title, &hit.name);
        if best.is_none_or(|(kept, _)| score > kept) { best = Some((score, hit.article_id)); }
    }
    best.filter(|(score, _)| *score >= 0.7).map(|(_, article)| article)
}
