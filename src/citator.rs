use crate::{javascript_whitespace, text::trim_javascript_whitespace, utf16_len, ScalarText};
use legal_grammar_tables::{AsciiBoundedGrammar, CompiledEcmascriptGrammar};
use regex::Regex;
use serde::Serialize;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

const EXTENDED_US_CITATION_IDS: [&str; 4] = [
    "cite.us.reporter.custom.full",
    "cite.us.reporter.custom.short",
    "cite.us.law.full",
    "cite.us.law.short",
];
const FUNCTION_WORDS: &str = "the of to that in a an and is was were be as for on with by it this not or which would may must court judge held found stated";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcerptClassification {
    pub kind: &'static str,
    pub cite_tokens: usize,
    pub cite_runs: usize,
    pub cite_char_coverage: f64,
    pub function_words: usize,
    pub prose_window: Option<String>,
    pub rule: &'static str,
}

type Hit = Range<usize>;

#[derive(Serialize)]
pub struct CitationMatch<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

#[derive(Serialize)]
pub struct ProviderCitationMatch<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
    pub family: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub court: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reporter: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<&'a str>,
}

fn ecmascript(pattern: &str, flags: &str) -> CompiledEcmascriptGrammar {
    legal_grammar_tables::compile_ecmascript_pattern("citator", pattern, flags)
        .expect("frozen citator regex")
}

static CITATION_PATTERN: LazyLock<CompiledEcmascriptGrammar> =
    LazyLock::new(|| legal_grammar_tables::compile_ecmascript_table_entry("cite.in-text").unwrap());
static CASE_NAME: LazyLock<CompiledEcmascriptGrammar> = LazyLock::new(|| {
    ecmascript(
        r"(?:^|[^\p{L}])(?:R\.|[A-Z][\p{L}\p{M}'\u2019.&-]*(?:\s+(?:of|the|and|&|[A-Z][\p{L}\p{M}'\u2019.&-]*)){0,6})\s+v(?:\.|ersus)?\s+[A-Z][\p{L}\p{M}'\u2019.&-]*",
        "m",
    )
});
static ROUTING_PATTERN: LazyLock<CompiledEcmascriptGrammar> = LazyLock::new(|| {
    legal_grammar_tables::compile_ecmascript_table_entry("cite.provider-routing").unwrap()
});
static RESIDUAL_CUE: LazyLock<CompiledEcmascriptGrammar> = LazyLock::new(|| {
    legal_grammar_tables::compile_ecmascript_table_entry("cite.us.fallback-cue").unwrap()
});
static COMMON_US_LAW: LazyLock<AsciiBoundedGrammar> = LazyLock::new(|| {
    legal_grammar_tables::compile_ascii_bounded_table_entry("cite.us.law.common")
        .expect("common US law grammar")
});
static STANDARD_CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:\A|[^A-Za-z0-9])(?<citation>(?<volume>[1-9][0-9]*) (?<reporter>[^;\r\n]{1,180}?),? (?:at\s?(?:p(?:\.|age)?)? )?(?<page>[0-9]+|_+))(?:\z|[^A-Za-z0-9])",
    )
    .unwrap()
});
static EXTENDED_CANDIDATE: LazyLock<Regex> = LazyLock::new(extended_standard_candidate);
static STANDARD_SURFACES: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let tables = legal_grammar_tables::load_tables().expect("shared US citation grammar");
    let entry = tables
        .get("cite.us.reporter.standard.full")
        .expect("shared reporter grammar");
    ["us_reporters", "us_journals"]
        .into_iter()
        .flat_map(|name| split_literal_alternation(&entry.defs[name]))
        .map(decode_surface)
        .collect()
});
static EXTENDED_US_PATTERNS: LazyLock<[AsciiBoundedGrammar; 4]> = LazyLock::new(|| {
    EXTENDED_US_CITATION_IDS.map(|id| {
        legal_grammar_tables::compile_ascii_bounded_table_entry(id).expect("extended US grammar")
    })
});

fn extended_standard_candidate() -> Regex {
    let tables = legal_grammar_tables::load_tables().expect("shared US citation grammar");
    let page = &tables["cite.us.reporter.full"].defs["us_page"];
    Regex::new(&format!(
        r"(?:\A|[^A-Za-z0-9])(?<citation>(?<volume>[1-9][0-9]*) (?<reporter>[^;\r\n]{{1,180}}?),? (?:at\s?(?:p(?:\.|age)?)? )?(?<page>{page}))(?:\z|[^A-Za-z0-9])"
    ))
    .unwrap()
}

fn split_literal_alternation(source: &str) -> Vec<&str> {
    let inner = source
        .strip_prefix("(?:")
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(source);
    let mut values = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (index, character) in inner.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '|' {
            values.push(&inner[start..index]);
            start = index + 1;
        }
    }
    values.push(&inner[start..]);
    values
}

fn decode_surface(source: &str) -> String {
    let source = source.replace(r"\s*", "").replace(' ', "");
    let mut decoded = String::with_capacity(source.len());
    let mut characters = source.chars();
    while let Some(character) = characters.next() {
        decoded.push(if character == '\\' {
            characters.next().unwrap_or(character)
        } else {
            character
        });
    }
    decoded
}

fn standard_us_matches(value: &str, pattern: &Regex) -> Vec<Hit> {
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(captures) = pattern.captures_at(value, cursor) {
        let citation = captures.name("citation").expect("standard citation");
        let reporter = captures.name("reporter").expect("standard reporter");
        let key = reporter
            .as_str()
            .chars()
            .filter(|character| !javascript_whitespace(*character))
            .collect::<String>();
        if STANDARD_SURFACES.contains(&key) {
            found.push(citation.start()..citation.end());
        }
        cursor = citation.start() + 1;
    }
    found
}

fn us_fallback_ranges(value: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut seen = HashSet::new();
    for cue in RESIDUAL_CUE.find_iter(value) {
        let start = value[..cue.start()].rfind('\n').map_or(0, |at| at + 1);
        let end = value[cue.end()..]
            .find('\n')
            .map_or(value.len(), |at| cue.end() + at);
        if seen.insert((start, end)) {
            ranges.push((start, end));
        }
    }
    ranges
}

fn citation_hits(value: &str, extended_us_fallback: bool) -> Vec<Hit> {
    let mut found = CITATION_PATTERN
        .find_iter(value)
        .map(|matched| matched.start()..matched.end())
        .collect::<Vec<_>>();
    found.extend(standard_us_matches(value, &STANDARD_CANDIDATE));
    found.extend(COMMON_US_LAW.find_spans(value));
    if extended_us_fallback {
        for (start, end) in us_fallback_ranges(value) {
            let candidate = &value[start..end];
            found.extend(
                standard_us_matches(candidate, &EXTENDED_CANDIDATE)
                    .into_iter()
                    .map(|matched| start + matched.start..start + matched.end),
            );
            for (id, pattern) in EXTENDED_US_CITATION_IDS
                .iter()
                .zip(EXTENDED_US_PATTERNS.iter())
            {
                if id.ends_with(".short") && !candidate.contains(" at") {
                    continue;
                }
                found.extend(
                    pattern
                        .find_spans(candidate)
                        .into_iter()
                        .map(|matched| start + matched.start..start + matched.end),
                );
            }
        }
    }
    found.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.end.cmp(&left.end))
    });
    let mut resolved: Vec<Hit> = Vec::new();
    for hit in found {
        if resolved
            .last()
            .is_some_and(|previous| hit.start < previous.end)
        {
            continue;
        }
        resolved.push(hit);
    }
    resolved
}

pub fn provider_citations_in_text(text: &str) -> Vec<ProviderCitationMatch<'_>> {
    let document = ScalarText::new(text);
    ROUTING_PATTERN
        .captures_iter(text)
        .map(|captures| {
            let matched = captures.get(0).unwrap();
            let has = |name| captures.name(name).is_some();
            let (family, jurisdiction) = if has("uk_neutral") {
                ("neutral", Some("uk"))
            } else if has("ca_statute") {
                ("statute", Some("ca"))
            } else if has("ca_reporter") {
                ("reporter", Some("ca"))
            } else if has("ca_neutral") {
                ("neutral", Some("ca"))
            } else if has("us_reporter") {
                ("reporter", Some("us"))
            } else if has("neutral") {
                ("neutral", None)
            } else {
                ("reporter", None)
            };
            let group = |names: &[&str]| {
                names
                    .iter()
                    .find_map(|name| captures.name(name).map(|capture| capture.as_str()))
            };
            ProviderCitationMatch {
                text: matched.as_str(),
                start: document.utf16_at_byte(matched.start()).unwrap(),
                end: document.utf16_at_byte(matched.end()).unwrap(),
                family,
                jurisdiction,
                year: group(&["uk_year", "ca_year", "year"]),
                court: group(&["uk_court", "ca_court", "court"]),
                number: group(&["uk_num", "ca_num", "num"]),
                volume: group(&["us_volume", "volume"]),
                reporter: group(&["us_reporter_name", "reporter"]),
                page: group(&["us_page", "page"]),
            }
        })
        .collect()
}

pub fn citations_in_text(text: &str, extended_us_fallback: bool) -> Vec<CitationMatch<'_>> {
    let document = ScalarText::new(text);
    citation_hits(text, extended_us_fallback)
        .into_iter()
        .map(|hit| CitationMatch {
            text: &text[hit.clone()],
            start: document.utf16_at_byte(hit.start).unwrap(),
            end: document.utf16_at_byte(hit.end).unwrap(),
        })
        .collect()
}

pub fn citation_lookup_key(value: &str) -> String {
    let mut characters = value.nfkc().peekable();
    let mut key = String::with_capacity(value.len());
    let mut previous_digit = false;
    while let Some(character) = characters.next() {
        let character = if matches!(character, '\u{2013}' | '\u{2014}') {
            '-'
        } else {
            character
        };
        if previous_digit
            && matches!(character, '.' | '-' | '/')
            && characters.peek().is_some_and(char::is_ascii_digit)
        {
            key.push_str(match character {
                '.' => "dot",
                '-' => "dash",
                _ => "slash",
            });
        } else {
            for character in character.to_lowercase() {
                if character == '\u{df}' {
                    key.push_str("ss");
                } else if character.is_ascii_alphanumeric() {
                    key.push(character);
                }
            }
        }
        previous_digit = character.is_ascii_digit();
    }
    key
}

static NAME_LEADIN: LazyLock<Regex> = LazyLock::new(|| {
    ecmascript(
        r"(?:[A-Z][\w.'’()‑-]*(?:\s+[\w.'’()‑-]+){0,6}\s+(?:v|c)\.\s+[A-Z][\w.'’()‑-]*(?:\s+[\w.'’()‑-]+){0,6},?\s*)$",
        "",
    )
});
static PINPOINT: LazyLock<Regex> = LazyLock::new(|| {
    ecmascript(
        r"^\s*(?:,\s*)?(?:at\s+)?para?s?\.?\s+\d+(?:\s*[-–]\s*\d+)?",
        "",
    )
});
static GLUE: LazyLock<Regex> = LazyLock::new(|| ecmascript(r"^[\s,;]*(?:and\s+)?$", ""));

fn citation_spans(text: &str) -> (Vec<Hit>, usize) {
    let document = ScalarText::new(text);
    let hits = citation_hits(text, true);
    let tokens = hits.len();
    let mut spans = Vec::with_capacity(tokens);
    for hit in hits {
        let mut start = hit.start;
        let mut end = hit.end;
        if let Some(leadin) = NAME_LEADIN.find(&text[..start]) {
            start = leadin.start();
        }
        if let Some(tail) = PINPOINT.find(&text[end..]) {
            end += tail.end();
        }
        spans.push(start..end);
    }
    spans.sort_by_key(|span| span.start);
    let mut merged: Vec<Hit> = Vec::new();
    for span in spans {
        if let Some(last) = merged.last_mut().filter(|last| {
            document.utf16_at_byte(span.start).unwrap()
                <= document.utf16_at_byte(last.end).unwrap() + 6
                && GLUE.is_match(&text[last.end..span.start.max(last.end)])
        }) {
            last.end = last.end.max(span.end);
        } else {
            merged.push(span);
        }
    }
    (merged, tokens)
}

fn refusal(rule: &'static str) -> ExcerptClassification {
    ExcerptClassification {
        kind: "insufficient",
        cite_tokens: 0,
        cite_runs: 0,
        cite_char_coverage: 0.0,
        function_words: 0,
        prose_window: None,
        rule,
    }
}

fn lowercase_words(text: &str) -> usize {
    static WORDS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}'’-]+").unwrap());
    static LOWERCASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{Ll}$").unwrap());
    WORDS
        .find_iter(text)
        .filter(|word| {
            let first = word.as_str().chars().next().unwrap();
            utf16_len(word.as_str()) >= 3
                && first.len_utf16() == 1
                && LOWERCASE.is_match(&first.to_string())
        })
        .count()
}

fn trim_window_edges(text: &str) -> String {
    static FIRST: LazyLock<Regex> = LazyLock::new(|| ecmascript(r"^\S*\s+", ""));
    static LAST: LazyLock<Regex> = LazyLock::new(|| ecmascript(r"\s+\S*$", ""));
    LAST.replace(&FIRST.replace(text, ""), "").into_owned()
}

pub fn classify_citator_excerpt(excerpt: &str) -> ExcerptClassification {
    let text = trim_javascript_whitespace(excerpt);
    let text_length = utf16_len(text);
    if text_length < 60 {
        return refusal("shorter_than_min_excerpt");
    }
    let (spans, cite_tokens) = citation_spans(text);
    let document = ScalarText::new(text);
    let cite_chars = spans
        .iter()
        .map(|span| {
            document.utf16_at_byte(span.end).unwrap() - document.utf16_at_byte(span.start).unwrap()
        })
        .sum::<usize>();
    let cite_char_coverage = cite_chars as f64 / text_length as f64;
    let cite_runs = text
        .split(';')
        .filter(|segment| has_citation(segment))
        .count();
    let mut segments = Vec::new();
    let mut cursor = 0;
    for span in &spans {
        if span.start > cursor {
            segments.push(&text[cursor..span.start]);
        }
        cursor = cursor.max(span.end);
    }
    if cursor < text.len() {
        segments.push(&text[cursor..]);
    }
    let words = segments.join(" ").to_lowercase();
    let function_words = words
        .split(|character: char| !character.is_ascii_lowercase() && character != '\'')
        .filter(|word| {
            FUNCTION_WORDS
                .split(' ')
                .any(|candidate| candidate == *word)
        })
        .count();
    let best = segments
        .iter()
        .flat_map(|segment| segment.split('\n'))
        .map(|line| trim_javascript_whitespace(line))
        .map(|line| (line, lowercase_words(line), utf16_len(line)))
        .reduce(|best, candidate| {
            if (candidate.1, candidate.2) > (best.1, best.2) {
                candidate
            } else {
                best
            }
        });
    let prose_window = best
        .filter(|(_, score, length)| *score >= 4 && *length >= 40)
        .map(|(line, _, _)| trim_window_edges(line));
    if cite_runs >= 3 && function_words < cite_runs * 4 {
        return ExcerptClassification {
            kind: "authority_list",
            cite_tokens,
            cite_runs,
            cite_char_coverage,
            function_words,
            prose_window: None,
            rule: "cite_runs>=3_low_function_words",
        };
    }
    if cite_char_coverage > 0.5 && function_words < 8 {
        return ExcerptClassification {
            kind: "authority_list",
            cite_tokens,
            cite_runs,
            cite_char_coverage,
            function_words,
            prose_window: None,
            rule: "cite_coverage>0.5_low_function_words",
        };
    }
    let Some(prose_window) = prose_window else {
        return refusal("no_prose_window");
    };
    let prose = cite_char_coverage <= 0.15 && cite_tokens <= 2;
    ExcerptClassification {
        kind: if prose { "prose" } else { "mixed" },
        cite_tokens,
        cite_runs,
        cite_char_coverage,
        function_words,
        prose_window: Some(prose_window),
        rule: if prose {
            "low_cite_coverage"
        } else {
            "prose_window_with_citations"
        },
    }
}

fn has_citation(text: &str) -> bool {
    CITATION_PATTERN.is_match(text) || !citation_hits(text, true).is_empty()
}

pub fn has_citation_in_text(text: &str) -> bool {
    has_citation(text) || CASE_NAME.is_match(text)
}

pub fn caselaw_citation_lookup_key(text: &str) -> Result<String, &'static str> {
    let hits = citation_hits(text, true);
    if hits.len() > 1 {
        return Err("citation must identify one citation form; multiple citations were found");
    }
    let key = hits
        .first()
        .map(|hit| citation_lookup_key(&text[hit.start..hit.end]))
        .unwrap_or_else(|| citation_lookup_key(text));
    if key.is_empty() {
        Err("citation is required (no letters or digits survive normalization)")
    } else {
        Ok(key)
    }
}
