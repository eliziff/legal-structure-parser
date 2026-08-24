use fancy_regex::{Match as FancyMatch, Regex as FancyRegex, RegexBuilder, RegexInput};
use regex::{Regex, RegexBuilder as LinearRegexBuilder};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::{Arc, OnceLock};

mod grammar_word;

use grammar_word::SOURCE_WORD;

#[derive(Clone, Debug)]
pub enum Error {
    Message(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
pub type CompiledGrammar = FancyRegex;
pub type CompiledEcmascriptGrammar = Regex;

pub struct AsciiBoundedGrammar {
    regex: Regex,
}

impl AsciiBoundedGrammar {
    pub fn find_spans(&self, text: &str) -> Vec<Range<usize>> {
        let mut spans = Vec::new();
        let mut cursor = 0;
        while cursor <= text.len() {
            let Some(captures) = self.regex.captures_at(text, cursor) else {
                break;
            };
            let matched = captures
                .name("__legal_grammar_span")
                .expect("bounded grammar span");
            spans.push(matched.start()..matched.end());
            cursor = matched.end();
        }
        spans
    }
}

pub const GRAMMAR_CORPUS_FORMAT: &str = "legal-grammar-corpus:v1";
const GRAMMAR_CORPUS_JSON: &str = include_str!("../../data/grammar-corpus.json");
pub const SOURCE_WHITESPACE: &str = concat!(
    r" \t\n\r\f\v\x1c-\x1f\x85\u00a0\u1680",
    r"\u2000-\u200a\u2028\u2029\u202f\u205f\u3000"
);
pub const ECMASCRIPT_WHITESPACE: &str = concat!(
    r" \t\n\r\f\v\u00a0\u1680\u2000-\u200a",
    r"\u2028\u2029\u202f\u205f\u3000\ufeff"
);
pub const ECMASCRIPT_WORD: &str = "0-9A-Z_a-z";

#[derive(Debug, Deserialize)]
struct GrammarEvidenceVector {
    input: String,
    groups: Value,
    #[serde(default)]
    canonical: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrammarEntry {
    pub id: String,
    pub pattern: String,
    #[serde(default)]
    pub flags: String,
}

#[derive(Debug, Deserialize)]
struct GrammarEvidenceEntry {
    id: String,
    #[serde(default)]
    canonical: Value,
    #[serde(default)]
    vectors: Vec<GrammarEvidenceVector>,
}

#[derive(Debug, Deserialize)]
struct GrammarEvidenceTable {
    #[serde(default)]
    entries: Vec<GrammarEvidenceEntry>,
}

#[derive(Debug, Deserialize)]
struct GrammarEvidenceCorpus {
    format: String,
    tables: BTreeMap<String, GrammarEvidenceTable>,
}

#[derive(Debug, Deserialize)]
struct GrammarTable {
    #[serde(default)]
    defs: HashMap<String, String>,
    #[serde(default)]
    entries: Vec<GrammarEntry>,
}

#[derive(Debug, Deserialize)]
struct GrammarCorpus {
    format: String,
    tables: BTreeMap<String, GrammarTable>,
}

#[derive(Debug, Clone)]
pub struct TableEntry {
    pub entry: GrammarEntry,
    pub defs: Arc<HashMap<String, String>>,
}

fn load_static_tables() -> std::result::Result<BTreeMap<String, TableEntry>, String> {
    let mut result = BTreeMap::new();
    let corpus: GrammarCorpus = serde_json::from_str(GRAMMAR_CORPUS_JSON)
        .map_err(|error| format!("grammar-corpus.json: {error}"))?;
    if corpus.format != GRAMMAR_CORPUS_FORMAT {
        return Err(format!(
            "unexpected grammar corpus format {:?}",
            corpus.format
        ));
    }
    for (name, table) in corpus.tables {
        let defs = Arc::new(table.defs);
        for entry in table.entries {
            let id = entry.id.clone();
            if result
                .insert(
                    id.clone(),
                    TableEntry {
                        entry,
                        defs: Arc::clone(&defs),
                    },
                )
                .is_some()
            {
                return Err(format!("{name}: duplicate grammar entry id {id:?}"));
            }
        }
    }
    Ok(result)
}

fn load_static_evidence() -> std::result::Result<BTreeMap<String, GrammarEvidenceEntry>, String> {
    let mut result = BTreeMap::new();
    let corpus: GrammarEvidenceCorpus = serde_json::from_str(GRAMMAR_CORPUS_JSON)
        .map_err(|error| format!("grammar-corpus.json: {error}"))?;
    if corpus.format != GRAMMAR_CORPUS_FORMAT {
        return Err(format!(
            "unexpected grammar corpus format {:?}",
            corpus.format
        ));
    }
    for (name, table) in corpus.tables {
        for entry in table.entries {
            let id = entry.id.clone();
            if result.insert(id.clone(), entry).is_some() {
                return Err(format!("{name}: duplicate grammar entry id {id:?}"));
            }
        }
    }
    Ok(result)
}

pub fn load_tables() -> Result<&'static BTreeMap<String, TableEntry>> {
    static TABLES: OnceLock<std::result::Result<BTreeMap<String, TableEntry>, String>> =
        OnceLock::new();
    TABLES
        .get_or_init(load_static_tables)
        .as_ref()
        .map_err(|message| Error::Message(message.clone()))
}

fn def_reference() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"\{\{([A-Za-z_][A-Za-z0-9_]*)\}\}").unwrap())
}

pub fn expand_pattern(source: &str, defs: &HashMap<String, String>) -> Result<String> {
    let mut output = source.to_owned();
    for _ in 0..11 {
        let mut next = String::with_capacity(output.len());
        let mut cursor = 0;
        let mut found = false;
        for captures in def_reference().captures_iter(&output) {
            let matched = captures.get(0).expect("whole def reference");
            let name = captures.get(1).expect("def name").as_str();
            let replacement = defs.get(name).ok_or_else(|| {
                Error::Message(format!("grammar def {{{{{name}}}}} is not defined"))
            })?;
            next.push_str(&output[cursor..matched.start()]);
            next.push_str(replacement);
            cursor = matched.end();
            found = true;
        }
        if !found {
            return Ok(output);
        }
        next.push_str(&output[cursor..]);
        if next == output {
            return Ok(output);
        }
        output = next;
    }
    Err(Error::Message(
        "grammar defs reference each other in a cycle".to_owned(),
    ))
}

fn word_boundary_for(word: &str) -> String {
    format!("(?:(?<![{word}])(?=[{word}])|(?<=[{word}])(?![{word}]))")
}

#[cfg(test)]
fn word_boundary() -> &'static str {
    static VALUE: OnceLock<String> = OnceLock::new();
    VALUE.get_or_init(|| word_boundary_for(SOURCE_WORD))
}

fn word_nonboundary_for(word: &str) -> String {
    format!("(?:(?<=[{word}])(?=[{word}])|(?<![{word}])(?![{word}]))")
}

fn expand_portable_with(source: &str, whitespace: &str, word: &str) -> Result<String> {
    let characters = source.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(source.len());
    let mut in_class = false;
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if character == '\\' && index + 1 < characters.len() {
            let next = characters[index + 1];
            match next {
                's' => {
                    if in_class {
                        output.push_str(whitespace);
                    } else {
                        output.push('[');
                        output.push_str(whitespace);
                        output.push(']');
                    }
                }
                'w' => {
                    if in_class {
                        output.push_str(word);
                    } else {
                        output.push('[');
                        output.push_str(word);
                        output.push(']');
                    }
                }
                'd' => {
                    output.push_str(if in_class { "0-9" } else { "[0-9]" });
                }
                'S' | 'W' | 'D' => {
                    if in_class {
                        return Err(Error::Message(format!(
                            "\\{next} inside a character class cannot be expanded portably"
                        )));
                    }
                    let fragment = match next {
                        'S' => whitespace,
                        'W' => word,
                        _ => "0-9",
                    };
                    output.push_str("[^");
                    output.push_str(fragment);
                    output.push(']');
                }
                'b' if !in_class => output.push_str(&word_boundary_for(word)),
                'B' => {
                    if in_class {
                        return Err(Error::Message(
                            "\\B inside a character class cannot be expanded portably".to_owned(),
                        ));
                    }
                    output.push_str(&word_nonboundary_for(word));
                }
                _ => {
                    output.push(character);
                    output.push(next);
                }
            }
            index += 2;
            continue;
        }
        if character == '[' && !in_class {
            in_class = true;
        } else if character == ']' && in_class {
            in_class = false;
        }
        output.push(character);
        index += 1;
    }
    Ok(output)
}

pub fn expand_portable(source: &str) -> Result<String> {
    expand_portable_with(source, SOURCE_WHITESPACE, SOURCE_WORD)
}

pub fn expand_ecmascript_portable(source: &str) -> Result<String> {
    let expanded = expand_portable_with(source, ECMASCRIPT_WHITESPACE, ECMASCRIPT_WORD)?;
    Ok(expanded
        .replace(&word_boundary_for(ECMASCRIPT_WORD), r"(?-u:\b)")
        .replace(&word_nonboundary_for(ECMASCRIPT_WORD), r"(?-u:\B)"))
}

fn opposite_ascii_case(character: char) -> char {
    if character.is_ascii_lowercase() {
        character.to_ascii_uppercase()
    } else {
        character.to_ascii_lowercase()
    }
}

fn casefold_class(characters: &[char], index: &mut usize, output: &mut String) {
    output.push('[');
    *index += 1;
    while *index < characters.len() {
        let character = characters[*index];
        if character == ']' {
            output.push(character);
            *index += 1;
            return;
        }
        if character == '\\' && *index + 1 < characters.len() {
            let escape_len = match characters[*index + 1] {
                'u' if *index + 5 < characters.len() => 6,
                'x' if *index + 3 < characters.len() => 4,
                _ => 2,
            };
            for value in &characters[*index..(*index + escape_len).min(characters.len())] {
                output.push(*value);
            }
            *index += escape_len;
            continue;
        }
        if character.is_ascii_alphabetic()
            && *index + 2 < characters.len()
            && characters[*index + 1] == '-'
            && characters[*index + 2].is_ascii_alphabetic()
        {
            let end = characters[*index + 2];
            output.push(character);
            output.push('-');
            output.push(end);
            output.push(opposite_ascii_case(character));
            output.push('-');
            output.push(opposite_ascii_case(end));
            *index += 3;
            continue;
        }
        output.push(character);
        if character.is_ascii_alphabetic() {
            output.push(opposite_ascii_case(character));
        }
        *index += 1;
    }
}

fn expand_ascii_case_insensitive(source: &str) -> String {
    let characters = source.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(source.len() * 2);
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if character == '[' {
            casefold_class(&characters, &mut index, &mut output);
            continue;
        }
        if character == '\\' && index + 1 < characters.len() {
            let escape_len = match characters[index + 1] {
                'u' if index + 5 < characters.len() => 6,
                'x' if index + 3 < characters.len() => 4,
                _ => 2,
            };
            for value in &characters[index..(index + escape_len).min(characters.len())] {
                output.push(*value);
            }
            index += escape_len;
            continue;
        }
        if character == '('
            && characters.get(index + 1) == Some(&'?')
            && characters.get(index + 2) == Some(&'<')
            && characters
                .get(index + 3)
                .is_some_and(|value| value.is_ascii_alphabetic() || *value == '_')
        {
            while index < characters.len() {
                let value = characters[index];
                output.push(value);
                index += 1;
                if value == '>' {
                    break;
                }
            }
            continue;
        }
        if character.is_ascii_alphabetic() {
            output.push('[');
            output.push(character);
            output.push(opposite_ascii_case(character));
            output.push(']');
        } else {
            output.push(character);
        }
        index += 1;
    }
    output
}

pub fn validate_pattern(source: &str) -> Vec<String> {
    let mut violations = Vec::new();
    if source.contains("(?P") {
        violations.push("(?P named-group syntax: author JS-style (?<name>...)".to_owned());
    }
    if Regex::new(r"\\[pP]\{").unwrap().is_match(source) {
        violations.push("\\p{...} classes: unsupported in re".to_owned());
    }
    for captures in Regex::new(r"\(\?([a-zA-Z-]+)[:)]")
        .unwrap()
        .captures_iter(source)
    {
        violations.push(format!(
            "inline flags (?{}...: banned",
            captures.get(1).expect("flags").as_str()
        ));
    }
    if Regex::new(r"(?:^|[^\\])\(\?\(").unwrap().is_match(source) {
        violations.push("conditional groups: not portable".to_owned());
    }
    if source.contains(r"\u{") {
        violations.push("braced \\u{...} escape: JS-only; use \\uXXXX or the literal".to_owned());
    }
    violations
}

fn expanded_entry(entry: &GrammarEntry, defs: &HashMap<String, String>) -> Result<String> {
    if !entry
        .flags
        .chars()
        .all(|flag| matches!(flag, 'i' | 'm' | 's'))
    {
        return Err(Error::Message(format!(
            "{}: flags must be a subset of \"ims\"",
            entry.id
        )));
    }
    let expanded = expand_pattern(&entry.pattern, defs)?;
    if let Some(violation) = validate_pattern(&expanded).into_iter().next() {
        return Err(Error::Message(format!("{}: {violation}", entry.id)));
    }
    Ok(expanded)
}

pub fn compile_entry(entry: &GrammarEntry, defs: &HashMap<String, String>) -> Result<FancyRegex> {
    let expanded = expanded_entry(entry, defs)?;
    let source = if entry.flags.contains('i') {
        expand_ascii_case_insensitive(&expanded)
    } else {
        expanded
    };
    let portable = expand_portable(&source)?;
    let mut builder = RegexBuilder::new(&portable);
    builder
        .unicode_mode(true)
        .case_insensitive(false)
        .multi_line(entry.flags.contains('m'))
        .dot_matches_new_line(entry.flags.contains('s'))
        .backtrack_limit(10_000_000);
    builder
        .build()
        .map_err(|error| Error::Message(format!("{}: does not compile in Rust: {error}", entry.id)))
}

pub fn compile_ecmascript_entry(
    entry: &GrammarEntry,
    defs: &HashMap<String, String>,
) -> Result<CompiledEcmascriptGrammar> {
    let portable = expand_ecmascript_portable(&expanded_entry(entry, defs)?)?;
    let mut builder = LinearRegexBuilder::new(&portable);
    builder
        .case_insensitive(entry.flags.contains('i'))
        .multi_line(entry.flags.contains('m'))
        .dot_matches_new_line(entry.flags.contains('s'));
    builder
        .build()
        .map_err(|error| Error::Message(format!("{}: does not compile in Rust: {error}", entry.id)))
}

pub fn compile_pattern(id: &str, pattern: &str, flags: &str) -> Result<FancyRegex> {
    compile_entry(
        &GrammarEntry {
            id: id.to_owned(),
            pattern: pattern.to_owned(),
            flags: flags.to_owned(),
        },
        &HashMap::new(),
    )
}

pub fn compile_ecmascript_pattern(
    id: &str,
    pattern: &str,
    flags: &str,
) -> Result<CompiledEcmascriptGrammar> {
    let entry = GrammarEntry {
        id: id.to_owned(),
        pattern: pattern.to_owned(),
        flags: flags.to_owned(),
    };
    compile_ecmascript_entry(&entry, &HashMap::new())
}

pub fn compile_table_entry(entry_id: &str) -> Result<FancyRegex> {
    let tables = load_tables()?;
    let value = tables
        .get(entry_id)
        .ok_or_else(|| Error::Message(format!("unknown grammar entry: {entry_id}")))?;
    compile_entry(&value.entry, &value.defs)
}

pub fn compile_ecmascript_table_entry(entry_id: &str) -> Result<CompiledEcmascriptGrammar> {
    let tables = load_tables()?;
    let value = tables
        .get(entry_id)
        .ok_or_else(|| Error::Message(format!("unknown grammar entry: {entry_id}")))?;
    compile_ecmascript_entry(&value.entry, &value.defs)
}

fn ascii_bounded_source(entry_id: &str) -> Result<(String, String)> {
    const LEFT: &str = "(?<![A-Za-z0-9])";
    const RIGHT: &str = "(?![A-Za-z0-9])";

    let tables = load_tables()?;
    let value = tables
        .get(entry_id)
        .ok_or_else(|| Error::Message(format!("unknown grammar entry: {entry_id}")))?;
    let expanded = expanded_entry(&value.entry, &value.defs)?;
    let inner = expanded
        .strip_prefix(LEFT)
        .and_then(|pattern| pattern.strip_suffix(RIGHT))
        .ok_or_else(|| {
            Error::Message(format!(
                "{entry_id}: expected outer ASCII alphanumeric boundaries"
            ))
        })?
        .replace("(?:(?!)|", "(?:");
    let portable = expand_ecmascript_portable(&inner)?;
    let source =
        format!(r"(?:\A|[^A-Za-z0-9])(?<__legal_grammar_span>{portable})(?:\z|[^A-Za-z0-9])");
    Ok((source, value.entry.flags.clone()))
}

fn compile_ascii_bounded_source(entry_id: &str, source: &str, flags: &str) -> Result<Regex> {
    let mut builder = LinearRegexBuilder::new(source);
    builder
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'));
    if entry_id == "cite.us.reporter.custom.short" {
        builder.dfa_size_limit(8 << 20);
    }
    builder.build().map_err(|error| {
        Error::Message(format!(
            "{entry_id}: does not compile as an ASCII-bounded linear grammar: {error}"
        ))
    })
}

pub fn compile_ascii_bounded_table_entry(entry_id: &str) -> Result<AsciiBoundedGrammar> {
    let (source, flags) = ascii_bounded_source(entry_id)?;
    Ok(AsciiBoundedGrammar {
        regex: compile_ascii_bounded_source(entry_id, &source, &flags)?,
    })
}

const MIN_WINDOW_TEXT: usize = 4096;
const COVERAGE_BAILOUT: f64 = 0.6;

fn anchor_metadata(entry_id: &str) -> Option<(&'static [&'static str], usize)> {
    Some(match entry_id {
        "bracket.editorial" => (
            &[
                "citation",
                "citations",
                "ellipsis",
                "emphasis",
                "footnote",
                "footnotes",
                "omitted",
                "sic",
                "translated",
                "translation",
            ],
            12,
        ),
        "cite.canlii" => (&["canlii"], 206),
        "cite.neutral.tribunal" => (
            &[
                "capprt", "ccri", "cirb", "comp", "comp.", "tcrpap", "trib", "trib.",
            ],
            274,
        ),
        "cite.url" => (&["doi:", "http", "perma.cc/", "www."], 147),
        "cite.url.prefix" => (&["http", "www."], 8),
        "marker.inline-fn" => (&["âŸ¦fn:"], 69),
        "pinpoint.para.toa" => (&["para"], 910),
        "ref.cross-reference" => (&["above", "below", "ibid", "note", "supra"], 205),
        "ref.history.toa" => (
            &[
                "aff", "affirmed", "appeal", "rev", "reversed", "varied", "varying",
            ],
            279,
        ),
        "ref.inline.toa" => (&["ibid", "supra"], 206),
        "ref.note-reference" => (&["note"], 1056),
        "ref.pure.splitter" => (&["ibid", "supra"], 2214),
        "ref.pure.toa" => (&["ibid", "supra"], 1140),
        "ref.supra-note.linking" => (&["supra"], 203),
        "ref.token" => (&["ibid", "supra"], 9),
        "title.legal.splitter" | "title.legal.toa" => (
            &["act", "code", "convention", "regulation", "rule", "treaty"],
            313,
        ),
        "title.named-code" => (&["code", "rule"], 307),
        _ => return None,
    })
}

fn full_matches<'a>(
    entry_id: &str,
    regex: &FancyRegex,
    text: &'a str,
) -> Result<Vec<FancyMatch<'a>>> {
    regex
        .find_iter(text)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| Error::Message(format!("{entry_id}: regex search failed: {error}")))
}

/// Frozen-oracle anchor windows for grammar-table scans. The original AST
/// derivation is a build/test concern; production needs only its deterministic
/// literals and pads. Windows use the full haystack through RegexInput so
/// lookaround and boundaries retain their original semantics.
pub fn find_table_matches<'a>(
    entry_id: &str,
    regex: &FancyRegex,
    text: &'a str,
) -> Result<Vec<FancyMatch<'a>>> {
    let Some((anchors, character_pad)) = anchor_metadata(entry_id) else {
        return full_matches(entry_id, regex, text);
    };
    if text.chars().count() < MIN_WINDOW_TEXT {
        return full_matches(entry_id, regex, text);
    }
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        return full_matches(entry_id, regex, text);
    }
    let mut hits = anchors
        .iter()
        .flat_map(|anchor| lower.match_indices(anchor).map(|(index, _)| index))
        .collect::<Vec<_>>();
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    hits.sort_unstable();
    let byte_pad = character_pad.saturating_mul(4);
    let mut windows = Vec::<(usize, usize)>::new();
    for hit in hits {
        let mut low = hit.saturating_sub(byte_pad);
        while low > 0 && !text.is_char_boundary(low) {
            low -= 1;
        }
        let mut high = hit
            .saturating_add(byte_pad)
            .saturating_add(1)
            .min(text.len());
        while high < text.len() && !text.is_char_boundary(high) {
            high += 1;
        }
        if let Some(window) = windows.last_mut().filter(|window| low <= window.1) {
            window.1 = window.1.max(high);
        } else {
            windows.push((low, high));
        }
    }
    if windows.iter().map(|(low, high)| high - low).sum::<usize>() as f64
        > COVERAGE_BAILOUT * text.len() as f64
    {
        return full_matches(entry_id, regex, text);
    }
    let mut found = Vec::new();
    for (low, high) in windows {
        let input = RegexInput::new(text).from_pos(low).range(low..high);
        for matched in regex.find_iter_input(input) {
            let matched = matched.map_err(|error| {
                Error::Message(format!("{entry_id}: regex search failed: {error}"))
            })?;
            if high < text.len() && matched.end() >= high.saturating_sub(1) {
                return full_matches(entry_id, regex, text);
            }
            found.push(matched);
        }
    }
    Ok(found)
}

fn canonicalize(groups: &mut HashMap<String, Option<String>>, rules: &Value) -> Result<()> {
    if let Some(names) = rules.get("lowercase").and_then(Value::as_array) {
        for name in names.iter().filter_map(Value::as_str) {
            if let Some(Some(value)) = groups.get_mut(name) {
                *value = value.to_lowercase();
            }
        }
    }
    if let Some(strip) = rules.get("strip").and_then(Value::as_object) {
        for (name, characters) in strip {
            let Some(characters) = characters.as_str() else {
                continue;
            };
            if let Some(Some(value)) = groups.get_mut(name) {
                value.retain(|character| !characters.contains(character));
            }
        }
    }
    if let Some(maps) = rules.get("map").and_then(Value::as_object) {
        for (name, mapping) in maps {
            let Some(mapping) = mapping.as_object() else {
                continue;
            };
            if let Some(Some(value)) = groups.get_mut(name) {
                if let Some(replacement) = mapping.get(value.as_str()).and_then(Value::as_str) {
                    *value = replacement.to_owned();
                }
            }
        }
    }
    Ok(())
}

pub fn run_vectors() -> Result<Vec<String>> {
    let mut failures = Vec::new();
    let tables = load_tables()?;
    let evidence = load_static_evidence().map_err(Error::Message)?;
    for (entry_id, table_entry) in tables {
        let evidence = evidence
            .get(entry_id)
            .ok_or_else(|| Error::Message(format!("missing grammar evidence: {entry_id}")))?;
        let pattern = match compile_entry(&table_entry.entry, &table_entry.defs) {
            Ok(pattern) => pattern,
            Err(error) => {
                failures.push(error.to_string());
                continue;
            }
        };
        for vector in &evidence.vectors {
            let captures = pattern.captures(&vector.input).map_err(|error| {
                Error::Message(format!("{entry_id}: matching vector failed: {error}"))
            })?;
            let Some(expected) = vector.groups.as_object() else {
                if captures.is_some() {
                    failures.push(format!(
                        "{entry_id}: {:?} expected no match, got one",
                        vector.input
                    ));
                }
                continue;
            };
            let Some(captures) = captures else {
                failures.push(format!(
                    "{entry_id}: {:?} expected a match, got none",
                    vector.input
                ));
                continue;
            };
            let mut groups = HashMap::new();
            for name in expected.keys() {
                groups.insert(
                    name.clone(),
                    captures
                        .name(name)
                        .map(|matched| matched.as_str().to_owned()),
                );
            }
            for (name, expected_value) in expected {
                let expected_value = expected_value.as_str().map(str::to_owned);
                if groups.get(name).cloned().flatten() != expected_value {
                    failures.push(format!(
                        "{entry_id}: {:?} group {name}: expected {:?}, got {:?}",
                        vector.input,
                        expected_value,
                        groups.get(name).cloned().flatten()
                    ));
                }
            }
            if let Some(expected_canonical) = vector.canonical.as_ref().and_then(Value::as_object) {
                canonicalize(&mut groups, &evidence.canonical)?;
                for (name, expected_value) in expected_canonical {
                    let expected_value = expected_value.as_str().map(str::to_owned);
                    if groups.get(name).cloned().flatten() != expected_value {
                        failures.push(format!(
                            "{entry_id}: {:?} canonical {name}: expected {:?}, got {:?}",
                            vector.input,
                            expected_value,
                            groups.get(name).cloned().flatten()
                        ));
                    }
                }
            }
        }
    }
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_tables_compile_and_pass_every_oracle_vector() {
        let failures = run_vectors().unwrap();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn portable_expansion_matches_the_frozen_boundary_contract() {
        assert_eq!(
            expand_portable(r"[\s\w]\b\S\W").unwrap(),
            format!(
                "[{SOURCE_WHITESPACE}{SOURCE_WORD}]{}[^{SOURCE_WHITESPACE}][^{SOURCE_WORD}]",
                word_boundary(),
            )
        );
        assert!(expand_portable(r"[\S]").is_err());
    }

    #[test]
    fn ascii_ignorecase_does_not_add_unicode_casefolds() {
        let pattern = compile_table_entry("ref.token").unwrap();
        assert!(pattern.is_match("SUPRA").unwrap());
        assert!(!pattern.is_match("Å¿upra").unwrap());
        assert!(!pattern.is_match("Ä°BID").unwrap());
    }

    #[test]
    fn ecmascript_mode_keeps_javascript_word_and_whitespace_semantics() {
        let roman = compile_ecmascript_table_entry("provision.reference.roman").unwrap();
        assert!(roman.is_match("ARTICLES DÃ‰FINIS"));
        let whitespace = compile_ecmascript_pattern("test.ecmascript-space", r"^\s$", "").unwrap();
        assert!(whitespace.is_match("\u{feff}"));
        assert!(!whitespace.is_match("\u{85}"));
    }

    #[test]
    fn anchor_windows_equal_full_scans_for_every_frozen_grammar() {
        const FILLER: &str = " The tribunal weighed the record before it and reserved judgment on the remaining issues, having heard the parties at length on costs. ";
        let tables = load_tables().unwrap();
        let evidence = load_static_evidence().unwrap();
        let mut core = String::new();
        for id in tables.keys() {
            for vector in &evidence[id].vectors {
                core.push_str(&vector.input);
                core.push_str(FILLER);
            }
        }
        let document = core.repeat(3);
        assert!(document.chars().count() > MIN_WINDOW_TEXT);
        let mut anchored = 0;
        for (id, value) in tables {
            let regex = compile_entry(&value.entry, &value.defs).unwrap();
            if anchor_metadata(id).is_some() {
                anchored += 1;
            }
            let expected = full_matches(id, &regex, &document)
                .unwrap()
                .into_iter()
                .map(|matched| (matched.start(), matched.end()))
                .collect::<Vec<_>>();
            let actual = find_table_matches(id, &regex, &document)
                .unwrap()
                .into_iter()
                .map(|matched| (matched.start(), matched.end()))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{id}");
        }
        assert!(anchored >= 10);
    }
}
