use crate::{
    javascript_whitespace, normalize_javascript_whitespace, text::JS_WHITESPACE_CLASS as JS_WS,
    tokenize_source_text, utf16_len, DocumentWordSpan, ScalarText,
};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

const MIN_COPY_TOKENS: usize = 8;
const MIN_COPY_CHARS: usize = 51;
const MIN_COPY_DISTINCT_CONTENT_TOKENS: usize = 4;
const MAX_MARKED_QUOTE_CHARS: usize = 4_000;
const MAX_MARKED_QUOTE_EDITS: usize = 4;
const MAX_FUZZY_SOURCE_CHARS: usize = 50_000;
const COPY_STOP_WORDS: &str = "a an and are as at be but by for from has have if in into is it its of on or that the their there these this to was were will with which would when who whom whose";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibleEvidenceText {
    pub evidence_id: String,
    pub text: String,
    #[serde(default)]
    pub labels: Vec<String>,
}

type Repair<'a> = (f64, Option<&'a str>);

struct MarkedQuote {
    text: String,
    marked: [usize; 2],
    start: usize,
    end: usize,
}

#[derive(Serialize)]
pub struct MarkedQuoteSpan {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

fn regex(pattern: &'static str, cell: &'static OnceLock<Regex>) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("quote regex must compile"))
}

fn js_non_ws() -> &'static str {
    static VALUE: OnceLock<String> = OnceLock::new();
    VALUE.get_or_init(|| format!("[^{}]", &JS_WS[1..JS_WS.len() - 1]))
}

fn normalize_repair_word(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    for character in value.chars() {
        let character = match character {
            '‘' | '’' | '‚' | '′' => '\'',
            '–' | '—' | '−' | '\u{2010}'..='\u{2015}' => '-',
            character => character,
        };
        normalized.extend(character.to_lowercase());
    }
    normalized
}

fn repair_tokens(value: &str, coordinates: &ScalarText<'_>, limit: usize) -> Vec<DocumentWordSpan> {
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    regex(r"\p{L}[\p{L}\p{N}'’\u{2010}-\u{2015}-]*|\p{N}+", &TOKEN)
        .find_iter(value)
        .take(limit)
        .map(|token| DocumentWordSpan {
            word: normalize_repair_word(token.as_str()),
            start: coordinates.utf16_at_byte(token.start()).unwrap(),
            end: coordinates.utf16_at_byte(token.end()).unwrap(),
        })
        .collect()
}

fn nearest_verbatim_excerpt<'a>(claim_body: &str, span_text: &'a str) -> Repair<'a> {
    let claim_coordinates = ScalarText::new(claim_body);
    let span_coordinates = ScalarText::new(span_text);
    let claim = repair_tokens(claim_body, &claim_coordinates, 400);
    let span = repair_tokens(span_text, &span_coordinates, 4_000);
    if claim.is_empty() || span.is_empty() {
        return (0.0, None);
    }
    let mut best = 0;
    let mut best_span_end = 0;
    let mut runs = vec![0; claim.len() + 1];
    for (span_index, span_token) in span.iter().enumerate() {
        for claim_index in (0..claim.len()).rev() {
            runs[claim_index + 1] = if span_token.word == claim[claim_index].word {
                runs[claim_index] + 1
            } else {
                0
            };
            if runs[claim_index + 1] > best {
                best = runs[claim_index + 1];
                best_span_end = span_index + 1;
            }
        }
    }
    let excerpt = (best >= 6)
        .then(|| {
            let start = span_coordinates
                .byte_at_utf16(span[best_span_end - best].start)
                .unwrap();
            let end = span_coordinates
                .byte_at_utf16(span[best_span_end - 1].end)
                .unwrap();
            &span_text[start..end]
        })
        .filter(|excerpt| utf16_len(excerpt) >= 25);
    (best as f64 / claim.len() as f64, excerpt)
}

fn truncate_suggestion(excerpt: &str) -> &str {
    if utf16_len(excerpt) <= 600 {
        return excerpt;
    }
    let coordinates = ScalarText::new(excerpt);
    let searched = coordinates.byte_at_utf16_floor(601).unwrap();
    let end = excerpt[..searched]
        .rfind(' ')
        .unwrap_or_else(|| excerpt.char_indices().next_back().unwrap().0);
    &excerpt[..end]
}

pub fn quote_repair_suggestion(claim_body: &str, spans: &[String]) -> Option<String> {
    let mut best: Option<Repair<'_>> = None;
    for span in spans {
        let repair = nearest_verbatim_excerpt(claim_body, span);
        if repair.1.is_some() && best.as_ref().is_none_or(|best| repair.0 > best.0) {
            best = Some(repair);
        }
    }
    let repair = best.filter(|repair| repair.0 >= 0.5)?;
    Some(format!(
        "closest verbatim excerpt of its cited span: “{}” — if this serves, resubmit quoting it exactly as shown",
        truncate_suggestion(repair.1.unwrap())
    ))
}

fn representation(value: &str) -> String {
    normalize_javascript_whitespace(
        &value
            .nfc()
            .map(|character| match character {
                '“' | '”' | '„' | '‟' => '"',
                '‘' | '’' | '‚' | '‛' => '\'',
                character => character,
            })
            .collect::<String>(),
    )
}

fn marked_quotes(text: &str) -> Vec<MarkedQuote> {
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static INLINE: OnceLock<Regex> = OnceLock::new();
    let coordinates = ScalarText::new(text);
    let make_quote = |marked: regex::Match<'_>, body: &str| {
        let body_start = marked.start() + marked.as_str().find(body).unwrap();
        MarkedQuote {
            text: body.into(),
            marked: [marked.start(), marked.end()],
            start: coordinates.utf16_at_byte(body_start).unwrap(),
            end: coordinates.utf16_at_byte(body_start + body.len()).unwrap(),
        }
    };
    let mut quotes = regex(
        r"(?:^|[\r\n\u{2028}\u{2029}])([ \t]*>+[ \t]?([^\r\n\u{2028}\u{2029}]*))",
        &BLOCK,
    )
    .captures_iter(if text.contains('>') { text } else { "" })
    .filter_map(|captures| {
        let marked = captures.get(1).unwrap();
        let body = captures
            .get(2)
            .unwrap()
            .as_str()
            .trim_matches(javascript_whitespace);
        (!body.is_empty()).then(|| make_quote(marked, body))
    })
    .collect::<Vec<_>>();
    let inline = regex(
        r#""([^"\r\n]+)"|\u{201c}([^\u{201d}\r\n]+)\u{201d}|\u{2018}([^\u{2019}\r\n]+)\u{2019}|\u{00ab}([^\u{00bb}\r\n]+)\u{00bb}"#,
        &INLINE,
    )
    .captures_iter(if text.find(&['"', '“', '‘', '«'][..]).is_some() {
        text
    } else {
        ""
    })
    .filter_map(|captures| {
        let marked = captures.get(0).unwrap();
        if quotes.iter().any(|quote| {
            marked.start() >= quote.marked[0] && marked.end() <= quote.marked[1]
        }) {
            return None;
        }
        let body = captures.iter().skip(1).flatten().next().unwrap().as_str();
        Some(make_quote(marked, body))
    })
    .collect::<Vec<_>>();
    quotes.extend(inline);
    quotes.sort_unstable_by_key(|quote| (quote.start, quote.end));
    quotes
}

pub fn marked_quote_spans(text: &str) -> Vec<MarkedQuoteSpan> {
    marked_quotes(text)
        .into_iter()
        .map(|quote| MarkedQuoteSpan {
            text: quote.text,
            start: quote.start,
            end: quote.end,
        })
        .collect()
}

fn letter_or_number(character: char) -> bool {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    let mut buffer = [0; 4];
    regex(r"^[\p{L}\p{N}]$", &VALUE).is_match(character.encode_utf8(&mut buffer))
}

fn flexible_spaces(value: &str) -> String {
    regex::escape(value).replace(' ', &format!("{JS_WS}+"))
}

fn source_supports_marked_quote(quote: &str, source: &str) -> bool {
    static EDIT: OnceLock<Regex> = OnceLock::new();
    static CONTENT: OnceLock<Regex> = OnceLock::new();
    let expected = representation(quote);
    let available = representation(source);
    if expected.is_empty() {
        return false;
    }
    if available.contains(&expected) {
        return true;
    }
    if utf16_len(&available) > MAX_FUZZY_SOURCE_CHARS
        || utf16_len(&expected) > MAX_MARKED_QUOTE_CHARS
    {
        return false;
    }
    let edits = regex(r"\[[^\]\r\n]+\]|…|\.{3}", &EDIT)
        .find_iter(&expected)
        .collect::<Vec<_>>();
    if edits.is_empty() || edits.len() > MAX_MARKED_QUOTE_EDITS {
        return false;
    }
    let mut cursor = 0;
    let mut pattern = String::new();
    let mut literal = String::new();
    for edit in edits {
        let before = expected[cursor..edit.start()].trim_end_matches(javascript_whitespace);
        let after = &expected[edit.end()..];
        let adjacent = before.chars().next_back().is_some_and(|character| {
            letter_or_number(character) || matches!(character, '\'' | '’')
        }) || after.chars().next().is_some_and(|character| {
            letter_or_number(character) || matches!(character, '\'' | '’')
        });
        pattern.push_str(&flexible_spaces(before));
        literal.push_str(before);
        if edit.as_str().starts_with('[') {
            if adjacent {
                pattern.push_str(js_non_ws());
                pattern.push('*');
            } else {
                pattern.push_str(&format!("{JS_WS}+(?:{}+{JS_WS}+)?", js_non_ws()));
            }
        } else {
            pattern.push_str("(?s:.*?)");
        }
        cursor = edit.end();
        if !adjacent || !edit.as_str().starts_with('[') {
            while expected[cursor..].starts_with(' ') {
                cursor += 1;
            }
        }
    }
    let tail = &expected[cursor..];
    pattern.push_str(&flexible_spaces(tail));
    literal.push_str(tail);
    if !regex(r"[\p{L}\p{N}]", &CONTENT).is_match(&literal) {
        return false;
    }
    Regex::new(&format!(
        r"(?:^|[^\p{{L}}\p{{N}}])(?:{pattern})(?:$|[^\p{{L}}\p{{N}}])"
    ))
    .expect("altered quote regex must compile")
    .is_match(&available)
}

fn unmarked_chunks(text: &str, quotes: &[MarkedQuote], labels: &[&str]) -> Vec<String> {
    static ARTIFACT: OnceLock<Regex> = OnceLock::new();
    let mut clean = text.to_owned();
    let mut ranges = quotes
        .iter()
        .map(|quote| quote.marked[0]..quote.marked[1])
        .collect::<Vec<_>>();
    ranges.sort_unstable_by(|left, right| right.start.cmp(&left.start));
    for range in ranges {
        clean.replace_range(range, "\0");
    }
    let artifact = ARTIFACT.get_or_init(|| {
        Regex::new(&format!(
            r"\[@[^\]\r\n]+\]|\[\d+\]|\[\[[^\]\r\n]+\]\]|\[[^\]\r\n]+\]\([^\)\r\n]+\)|https?://{}+",
            js_non_ws()
        ))
        .expect("prose artifact regex must compile")
    });
    clean = artifact.replace_all(&clean, "\0").into_owned();
    let mut seen = HashSet::new();
    for &label in labels {
        if utf16_len(label) < 4 || !seen.insert(label) {
            continue;
        }
        clean = RegexBuilder::new(&regex::escape(label))
            .case_insensitive(true)
            .build()
            .expect("evidence label regex must compile")
            .replace_all(&clean, "\0")
            .into_owned();
    }
    clean
        .split('\0')
        .filter(|chunk| !chunk.is_empty())
        .map(str::to_owned)
        .collect()
}

fn copy_content_word(word: &str) -> bool {
    utf16_len(word) > 2 && !COPY_STOP_WORDS.split(' ').any(|stop| stop == word)
}

fn copied_run<'a>(
    chunks: &[String],
    sources: &'a [VisibleEvidenceText],
) -> Option<(&'a str, String)> {
    let prose = chunks
        .iter()
        .map(|chunk| tokenize_source_text(chunk))
        .collect::<Vec<_>>();
    for source in sources {
        let source_tokens = tokenize_source_text(&source.text);
        if source_tokens.len() < MIN_COPY_TOKENS {
            continue;
        }
        let mut windows = HashMap::<&str, Vec<usize>>::new();
        for index in 0..=source_tokens.len() - MIN_COPY_TOKENS {
            windows
                .entry(source_tokens[index].word.as_str())
                .or_default()
                .push(index);
        }
        for (chunk_index, prose) in prose.iter().enumerate() {
            if prose.len() < MIN_COPY_TOKENS {
                continue;
            }
            for index in 0..=prose.len() - MIN_COPY_TOKENS {
                for &source_index in windows
                    .get(prose[index].word.as_str())
                    .into_iter()
                    .flatten()
                {
                    if prose[index..index + MIN_COPY_TOKENS]
                        .iter()
                        .zip(&source_tokens[source_index..source_index + MIN_COPY_TOKENS])
                        .any(|(left, right)| left.word != right.word)
                    {
                        continue;
                    }
                    let mut left = 0;
                    while left < index
                        && left < source_index
                        && prose[index - left - 1].word
                            == source_tokens[source_index - left - 1].word
                    {
                        left += 1;
                    }
                    let mut right = MIN_COPY_TOKENS;
                    while index + right < prose.len()
                        && source_index + right < source_tokens.len()
                        && prose[index + right].word == source_tokens[source_index + right].word
                    {
                        right += 1;
                    }
                    let run = &prose[index - left..index + right];
                    let normalized_length = run
                        .iter()
                        .map(|token| utf16_len(&token.word))
                        .sum::<usize>()
                        + run.len().saturating_sub(1);
                    if normalized_length < MIN_COPY_CHARS {
                        continue;
                    }
                    if run
                        .iter()
                        .filter(|token| copy_content_word(&token.word))
                        .map(|token| token.word.as_str())
                        .collect::<HashSet<_>>()
                        .len()
                        < MIN_COPY_DISTINCT_CONTENT_TOKENS
                    {
                        continue;
                    }
                    let chunk = &chunks[chunk_index];
                    let coordinates = ScalarText::new(chunk);
                    let start = coordinates.byte_at_utf16(run[0].start).unwrap();
                    let end = coordinates.byte_at_utf16(run.last().unwrap().end).unwrap();
                    return Some((&source.evidence_id, chunk[start..end].to_owned()));
                }
            }
        }
    }
    None
}

pub fn grounded_prose_errors(
    text: &str,
    cited_evidence_ids: &[String],
    visible_evidence: &[VisibleEvidenceText],
) -> Vec<String> {
    let quotes = marked_quotes(text);
    let cited = visible_evidence
        .iter()
        .filter(|source| cited_evidence_ids.contains(&source.evidence_id))
        .collect::<Vec<_>>();
    let mut errors = Vec::new();
    for quote in &quotes {
        if cited
            .iter()
            .any(|source| source_supports_marked_quote(&quote.text, &source.text))
        {
            continue;
        }
        let mut repaired = None;
        for &source in &cited {
            if let Some(suggestion) =
                quote_repair_suggestion(&quote.text, std::slice::from_ref(&source.text))
            {
                repaired = Some((source, suggestion));
                break;
            }
        }
        let source = repaired
            .as_ref()
            .map(|(source, _)| *source)
            .or_else(|| cited.first().copied());
        let mut error = format!(
            "quoted text {} does not match its cited evidence",
            serde_json::to_string(&quote.text).unwrap()
        );
        if let Some(source) = source {
            error.push_str(&format!(
                "; {} source window: {}",
                source.evidence_id,
                serde_json::to_string(&source.text).unwrap()
            ));
        }
        if let Some((_, suggestion)) = repaired {
            error.push_str("; ");
            error.push_str(&suggestion);
        }
        errors.push(error);
    }
    let labels = visible_evidence
        .iter()
        .flat_map(|source| source.labels.iter().map(String::as_str))
        .collect::<Vec<_>>();
    if let Some((evidence_id, copied)) =
        copied_run(&unmarked_chunks(text, &quotes, &labels), visible_evidence)
    {
        errors.push(format!(
            "unmarked copied passage {} matches visible evidence {}; quote it explicitly or write a genuine paraphrase",
            serde_json::to_string(&copied).unwrap(),
            evidence_id
        ));
    }
    errors
}
