#[cfg(feature = "structure-inference")]
use crate::{
    definitions::{derive_definitions_bytes, DefinitionHit},
    javascript_whitespace,
    locator::{compact_provision_label, normalize_compact_numbered_section_locator},
    node_depths,
    text::{normalize_javascript_whitespace, trim_javascript_start},
    AuthoritativeTableCell, AuthoritativeTables, Block, CoverageState, DefinedTerm,
    DefinitionOccurrence, DetectionProfile, DocumentInput, DocumentStructure, EngineError,
    NodeKind, Origin, ParagraphBreak, ScalarRange, ScalarText, Scope, EVIDENCE_SCHEMA,
};
#[cfg(feature = "structure-inference")]
use legal_grammar_tables::{
    compile_ecmascript_pattern, compile_ecmascript_table_entry, expand_pattern, load_tables,
    CompiledEcmascriptGrammar,
};
#[cfg(feature = "structure-inference")]
use regex::Regex;
use serde::{Deserialize, Serialize};
#[cfg(feature = "structure-inference")]
use sha2::{Digest, Sha256};
#[cfg(feature = "structure-inference")]
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::OnceLock,
};

#[cfg(feature = "structure-inference")]
fn split_instrument_space_runs(text: &str) -> Option<String> {
    let mut recovered: Option<String> = None;
    let mut characters = text.char_indices().peekable();
    while let Some((start, character)) = characters.next() {
        if !matches!(character, ' ' | '\t') {
            if let Some(recovered) = &mut recovered {
                recovered.push(character);
            }
            continue;
        }
        let mut count = 1;
        while characters
            .next_if(|(_, character)| matches!(character, ' ' | '\t'))
            .is_some()
        {
            count += 1;
        }
        let end = characters.peek().map_or(text.len(), |(byte, _)| *byte);
        let internal_run = count >= 2
            && start > 0
            && end < text.len()
            && !javascript_whitespace(text[..start].chars().next_back().unwrap())
            && !javascript_whitespace(text[end..].chars().next().unwrap());
        if internal_run {
            let recovered = recovered.get_or_insert_with(|| text[..start].to_owned());
            recovered.push('\n');
            recovered.push_str(&text[start + 1..end]);
        } else if let Some(recovered) = &mut recovered {
            recovered.push_str(&text[start..end]);
        }
    }
    recovered
}

#[cfg(feature = "structure-inference")]
fn split_instrument_sentence_joins(text: &str) -> Option<String> {
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let head = HEAD.get_or_init(|| {
        Regex::new(
            r"^(?:(?:ARTICLE|Article|PART|Part|DIVISION|Division|Section|SECTION|SCHEDULE|Schedule|EXHIBIT|Exhibit|ANNEX|Annex|APPENDIX|Appendix)[\s\u{feff}]+[IVXLCDM0-9]|[0-9]{1,3}\.[0-9]{1,3}(?:\.[0-9]{1,3})*[\s\u{feff}]+\S|\([A-Za-z0-9_]{1,3}\)[\s\u{feff}])",
        )
        .expect("valid instrument sentence-join grammar")
    });
    let mut recovered: Option<String> = None;
    let mut previous = None;
    let mut previous_previous = None;
    for (byte, character) in text.char_indices() {
        let preceded_by_terminator = previous.is_some()
            && (matches!(previous, Some('.' | ';' | ':'))
                || (matches!(
                    previous,
                    Some(')' | ']' | '"' | '\'' | '\u{201d}' | '\u{2019}' | '\u{00bb}')
                ) && matches!(previous_previous, Some('.' | ';' | ':'))));
        let after = byte + character.len_utf8();
        if matches!(character, ' ' | '\t')
            && preceded_by_terminator
            && head.is_match(&text[after..])
        {
            let recovered = recovered.get_or_insert_with(|| text[..byte].to_owned());
            recovered.push('\n');
        } else if let Some(recovered) = &mut recovered {
            recovered.push(character);
        }
        previous_previous = previous;
        previous = Some(character);
    }
    recovered
}

/// Offset-preserving lineation hypotheses used by the instrument structure profile.
/// The source lineation is first, so downstream selection keeps it on a tie.
#[cfg(feature = "structure-inference")]
fn instrument_lineation_hypotheses_iter(text: &str) -> impl Iterator<Item = String> + '_ {
    (0..4)
        .scan(None, move |joined, hypothesis| {
            Some(match hypothesis {
                0 => Some(text.to_owned()),
                1 => split_instrument_space_runs(text),
                2 => split_instrument_sentence_joins(text)
                    .inspect(|recovered| *joined = Some(recovered.clone())),
                3 => joined
                    .take()
                    .and_then(|joined| split_instrument_space_runs(&joined)),
                _ => None,
            })
        })
        .flatten()
}

#[cfg(feature = "structure-inference")]
struct ProvisionGrammars {
    reference: CompiledEcmascriptGrammar,
    leading_subdivision: CompiledEcmascriptGrammar,
    external_following: CompiledEcmascriptGrammar,
    instrument_lead: CompiledEcmascriptGrammar,
    list_continuation: CompiledEcmascriptGrammar,
    hyphenated_number: CompiledEcmascriptGrammar,
    thereof: CompiledEcmascriptGrammar,
    list_owner: CompiledEcmascriptGrammar,
    continuation: CompiledEcmascriptGrammar,
}

#[cfg(feature = "structure-inference")]
fn provision_grammars() -> &'static ProvisionGrammars {
    static GRAMMARS: OnceLock<ProvisionGrammars> = OnceLock::new();
    GRAMMARS.get_or_init(|| {
        let tables = load_tables().expect("valid legal grammar corpus");
        let numeric = &tables["provision.reference.numeric"];
        let roman = &tables["provision.reference.roman"];
        let defs = &numeric.defs;
        let continuation = format!(
            r"^\s*(,|and\b|or\b)\s*({}|{})",
            defs.get("numeric_label").expect("numeric label grammar"),
            defs.get("sub_only_label").expect("sub-only label grammar")
        );
        let table = |id| compile_ecmascript_table_entry(id).expect("valid provision grammar");
        ProvisionGrammars {
            reference: compile_ecmascript_pattern(
                "provision.reference",
                &format!(
                    "(?:{})|(?:{})",
                    expand_pattern(&numeric.entry.pattern, &numeric.defs).unwrap(),
                    expand_pattern(&roman.entry.pattern, &roman.defs).unwrap()
                ),
                "i",
            )
            .expect("valid provision grammar"),
            leading_subdivision: table("provision.external.leading-subdivision"),
            external_following: table("provision.external.following"),
            instrument_lead: table("provision.external.instrument-lead"),
            list_continuation: table("provision.external.list-continuation"),
            hyphenated_number: table("provision.external.hyphenated-number"),
            thereof: table("provision.external.thereof"),
            list_owner: table("provision.external.list-owner"),
            continuation: compile_ecmascript_pattern(
                "provision.reference.continuation",
                &continuation,
                "i",
            )
            .expect("valid provision continuation grammar"),
        }
    })
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvisionReferenceShape {
    Numeric,
    SubOnly,
    Roman,
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionReference {
    pub start: usize,
    pub end: usize,
    pub raw: String,
    pub word: String,
    pub plural: bool,
    pub label: String,
    pub shape: ProvisionReferenceShape,
    pub locator: String,
    pub alias_key: String,
    pub external: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_of: Option<usize>,
}

#[cfg(feature = "structure-inference")]
#[derive(Default)]
struct FindProvisionReferencesOptions<'a> {
    words: Option<&'a [&'a str]>,
    window: Option<usize>,
}

#[cfg(feature = "structure-inference")]
fn replace_first_match<'a>(regex: &CompiledEcmascriptGrammar, value: &'a str) -> &'a str {
    regex
        .find(value)
        .map_or(value, |matched| &value[matched.end()..])
}

#[cfg(feature = "structure-inference")]
fn is_external_reference(following: &str) -> bool {
    let grammars = provision_grammars();
    let trimmed = trim_javascript_start(replace_first_match(
        &grammars.leading_subdivision,
        following,
    ));
    let Some(captures) = grammars.external_following.captures(trimmed) else {
        return false;
    };
    captures
        .get(1)
        .is_some_and(|owner| !owner.as_str().eq_ignore_ascii_case("this"))
}

#[cfg(feature = "structure-inference")]
fn is_external_reference_in_context(before: &str, after: &str) -> bool {
    let grammars = provision_grammars();
    if grammars.instrument_lead.is_match(before)
        || grammars.hyphenated_number.is_match(after)
        || grammars.thereof.is_match(after)
        || is_external_reference(after)
    {
        return true;
    }
    let skipped = replace_first_match(&grammars.list_continuation, after);
    if skipped.len() == after.len() {
        return false;
    }
    grammars
        .list_owner
        .captures(skipped)
        .and_then(|captures| captures.get(1))
        .is_some_and(|owner| !owner.as_str().eq_ignore_ascii_case("this"))
}

#[cfg(feature = "structure-inference")]
fn provision_flanks<'a>(
    text: &'a str,
    start_byte: usize,
    end_byte: usize,
    window: usize,
) -> (&'a str, &'a str) {
    let mut before_byte = start_byte;
    let mut units = 0;
    for (byte, character) in text[..start_byte].char_indices().rev() {
        if units >= window {
            break;
        }
        before_byte = byte;
        units += character.len_utf16();
    }
    let mut after_byte = end_byte;
    units = 0;
    for (byte, character) in text[end_byte..].char_indices() {
        if units >= window {
            break;
        }
        after_byte = end_byte + byte + character.len_utf8();
        units += character.len_utf16();
    }
    (&text[before_byte..start_byte], &text[end_byte..after_byte])
}

#[cfg(feature = "structure-inference")]
#[allow(clippy::too_many_arguments)]
fn push_provision_reference(
    found: &mut Vec<ProvisionReference>,
    text: &str,
    allowed: Option<&HashSet<&str>>,
    window: usize,
    start_byte: usize,
    raw: &str,
    word: &str,
    plural: bool,
    raw_label: &str,
    shape: ProvisionReferenceShape,
    external_override: Option<bool>,
    continuation_of: Option<usize>,
) {
    if found
        .last()
        .is_some_and(|reference| reference.start == start_byte)
    {
        return;
    }
    let mut singular = word.to_lowercase();
    singular.truncate(singular.len() - usize::from(singular.ends_with('s')));
    if allowed.is_some_and(|allowed| !allowed.contains(singular.as_str())) {
        return;
    }
    let label = compact_provision_label(raw_label);
    let end_byte = start_byte + raw.len();
    let external = external_override.unwrap_or_else(|| {
        let (before, after) = provision_flanks(text, start_byte, end_byte, window);
        is_external_reference_in_context(before, after)
    });
    let locator = if shape == ProvisionReferenceShape::Roman {
        String::new()
    } else {
        normalize_compact_numbered_section_locator(&label)
    };
    let alias_key = format!("{singular} {label}").to_lowercase();
    found.push(ProvisionReference {
        start: start_byte,
        end: end_byte,
        raw: raw.to_owned(),
        word: singular,
        plural,
        label,
        shape,
        locator,
        alias_key,
        external,
        continuation_of,
    });
}

#[cfg(feature = "structure-inference")]
fn find_provision_references(
    coordinates: &ScalarText<'_>,
    options: FindProvisionReferencesOptions<'_>,
) -> Vec<ProvisionReference> {
    static TRAILING_SUBDIVISIONS: OnceLock<Regex> = OnceLock::new();
    let grammars = provision_grammars();
    let window = options.window.unwrap_or(40);
    let allowed = options
        .words
        .map(|words| words.iter().copied().collect::<HashSet<_>>());
    let text = coordinates.value;
    let mut found = Vec::new();
    let reference_grammar = &grammars.reference;
    let mut capture_locations = reference_grammar.capture_locations();
    let mut search_start = 0;

    while let Some(whole) =
        reference_grammar.captures_read_at(&mut capture_locations, text, search_start)
    {
        search_start = whole.end();
        let capture = |index| {
            capture_locations
                .get(index)
                .map(|(start, end)| &text[start..end])
        };
        if capture(1).is_none() {
            let word = capture(5).expect("roman provision word");
            push_provision_reference(
                &mut found,
                text,
                allowed.as_ref(),
                window,
                whole.start(),
                whole.as_str(),
                word,
                word.ends_with('s') || word.ends_with('S'),
                capture(6).expect("roman provision label"),
                ProvisionReferenceShape::Roman,
                None,
                None,
            );
            continue;
        }
        let raw_label = capture(3).or_else(|| capture(4)).unwrap_or("");
        let start_byte = whole.start();
        let end_byte = whole.end();
        let (before, after) = provision_flanks(text, start_byte, end_byte, window);
        let external = is_external_reference_in_context(before, after);
        let word = capture(1).expect("provision word");
        push_provision_reference(
            &mut found,
            text,
            allowed.as_ref(),
            window,
            start_byte,
            whole.as_str(),
            word,
            capture(2).is_some(),
            raw_label,
            if capture(3).is_some() {
                ProvisionReferenceShape::Numeric
            } else {
                ProvisionReferenceShape::SubOnly
            },
            Some(external),
            None,
        );

        struct Continuation<'a> {
            start_byte: usize,
            raw: &'a str,
            connector: String,
            shape: ProvisionReferenceShape,
        }
        let mut continuations = Vec::new();
        let mut cursor = end_byte;
        for _ in 0..50 {
            let Some(continuation) = grammars.continuation.captures(&text[cursor..]) else {
                break;
            };
            let whole = continuation.get(0).expect("provision continuation match");
            let label = continuation.get(2).expect("provision continuation label");
            let label_at = whole.as_str().rfind(label.as_str()).unwrap();
            let label_start = cursor + label_at;
            continuations.push(Continuation {
                start_byte: label_start,
                raw: &text[label_start..label_start + label.as_str().len()],
                connector: continuation
                    .get(1)
                    .expect("provision continuation connector")
                    .as_str()
                    .to_lowercase(),
                shape: if label.as_str().starts_with('(') {
                    ProvisionReferenceShape::SubOnly
                } else {
                    ProvisionReferenceShape::Numeric
                },
            });
            cursor += whole.end();
        }
        let safe_to_expand = capture(2).is_some()
            || continuations.len() > 1
            || continuations.iter().any(|item| {
                item.connector != "," || item.shape == ProvisionReferenceShape::SubOnly
            });
        if !safe_to_expand {
            continue;
        }
        let numeric_head = TRAILING_SUBDIVISIONS
            .get_or_init(|| Regex::new(r"(?:\([^()]+\))+$").expect("valid subdivision grammar"))
            .replace(&compact_provision_label(raw_label), "")
            .into_owned();
        for continuation in continuations {
            let label = if continuation.shape == ProvisionReferenceShape::SubOnly
                && !numeric_head.is_empty()
            {
                format!(
                    "{}{}",
                    numeric_head,
                    compact_provision_label(continuation.raw)
                )
            } else {
                continuation.raw.to_owned()
            };
            push_provision_reference(
                &mut found,
                text,
                allowed.as_ref(),
                window,
                continuation.start_byte,
                continuation.raw,
                word,
                false,
                &label,
                ProvisionReferenceShape::Numeric,
                Some(external),
                Some(start_byte),
            );
        }
    }
    let mut head = (0, 0);
    found
        .into_iter()
        .map(|mut reference| {
            let start_byte = reference.start;
            let start = coordinates
                .utf16_at_byte(start_byte)
                .expect("reference boundary");
            if let Some(head_byte) = reference.continuation_of {
                assert_eq!(head_byte, head.0, "coordinated-list head precedes member");
                reference.continuation_of = Some(head.1);
            } else {
                head = (start_byte, start);
            }
            reference.start = start;
            reference.end = coordinates
                .utf16_at_byte(reference.end)
                .expect("reference boundary");
            reference
        })
        .collect()
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentContentsEntry {
    pub label: String,
    pub display: String,
    pub heading: String,
    pub depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_label: Option<String>,
    pub page: Option<u32>,
    pub contents_line_start: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentContentsOutline {
    pub entries: Vec<InstrumentContentsEntry>,
    pub region_start: usize,
    pub region_end: usize,
    pub pages_cited: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentContentsRefusal {
    NoContentsMarker,
    NoContentsEntries,
    TooFewContentsEntries,
    ContentsWithoutPageNumbers,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct InstrumentContentsReading {
    pub outline: Option<InstrumentContentsOutline>,
    pub refusal: Option<InstrumentContentsRefusal>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentCrossReferenceStatus {
    Resolved,
    External,
    Unresolved,
    Abstained,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentCrossReferenceReason {
    ExternalInstrument,
    DocumentAbstained,
    NoContainingSection,
    AmbiguousLabel,
    DepthNotNumbered,
    NoSuchProvision,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceEdge {
    pub source_start: usize,
    pub source_end: usize,
    pub source_label: Option<String>,
    pub raw: String,
    pub raw_label: String,
    pub normalized_locator: String,
    pub target_label: Option<String>,
    pub target_start: Option<usize>,
    pub target_end: Option<usize>,
    pub status: InstrumentCrossReferenceStatus,
    pub self_loop: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<InstrumentCrossReferenceReason>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceCounts {
    pub detected: usize,
    pub resolved: usize,
    pub external: usize,
    pub unresolved: usize,
    pub abstained: usize,
    pub self_loops: usize,
    pub integrity: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentCrossReferenceGraph {
    pub edges: Vec<InstrumentCrossReferenceEdge>,
    pub document_abstained: bool,
    pub note: Option<String>,
    pub counts: InstrumentCrossReferenceCounts,
}

// Contents entries advertise provision labels and printed pages; they are not
// provision spans and never enter the detected node inventory.
#[cfg(feature = "structure-inference")]
const CONTENTS_MAX_ENTRY_GAP_UTF16: usize = 400;
// Measured entry gaps were 28-176 UTF-16 units across the accepted corpus;
// 200, 400, and 800 produced identical outlines on all 124 agreement texts.
#[cfg(feature = "structure-inference")]
const CONTENTS_WINDOW_UTF16: usize = 80_000;
#[cfg(feature = "structure-inference")]
const CONTENTS_MAX_ANCHORS: usize = 4;
#[cfg(feature = "structure-inference")]
const MIN_CONTENTS_ENTRIES: usize = 5;
// Accepted contents regions cite pages on 84-100% of their entries.
#[cfg(feature = "structure-inference")]
const MIN_CONTENTS_PAGE_SHARE: f64 = 0.6;
// A short pageless exhibits tail is valid; a continuing body walk is not.
#[cfg(feature = "structure-inference")]
const MAX_PAGELESS_RUN: usize = 3;

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug)]
enum InstrumentContentsHeadKind {
    Container { word: String, value: String },
    Schedule { word: String, value: String },
    Section { number: String },
}

#[cfg(feature = "structure-inference")]
#[derive(Clone, Debug)]
struct InstrumentContentsHead {
    start_byte: usize,
    end_byte: usize,
    start_utf16: usize,
    end_utf16: usize,
    kind: InstrumentContentsHeadKind,
}

#[cfg(feature = "structure-inference")]
fn instrument_contents_anchors(text: &str) -> Vec<usize> {
    static TABLE: OnceLock<Regex> = OnceLock::new();
    static BARE: OnceLock<Regex> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        Regex::new(r"(?i:TABLE[ \t]+OF[ \t]+CONTENTS)")
            .expect("valid instrument contents anchor grammar")
    });
    let bare = BARE.get_or_init(|| {
        Regex::new(r"(?i)^(?:CONTENTS|INDEX)$")
            .expect("valid bare instrument contents anchor grammar")
    });
    let mut anchors = Vec::with_capacity(CONTENTS_MAX_ANCHORS);
    let mut tables = table
        .find_iter(text)
        .filter_map(|found| {
            let before = text[..found.start()].chars().next_back();
            let after = text[found.end()..].chars().next();
            (before.is_none_or(|character| matches!(character, '\r' | '\n' | '\t' | ' '))
                && after.is_none_or(|character| matches!(character, '\r' | '\n' | '\t' | ' ')))
            .then_some((found.start(), found.end()))
        })
        .peekable();
    let mut start = 0;
    loop {
        let end = text[start..]
            .find(['\r', '\n'])
            .map_or(text.len(), |length| start + length);
        let line = &text[start..end];
        while tables
            .peek()
            .is_some_and(|(found_start, _)| *found_start < end)
        {
            anchors.push(tables.next().unwrap().1);
            if anchors.len() == CONTENTS_MAX_ANCHORS {
                break;
            }
        }
        if anchors.len() == CONTENTS_MAX_ANCHORS {
            break;
        }
        let core = line.trim_matches([' ', '\t']);
        if bare.is_match(core) {
            anchors.push(end);
            if anchors.len() == CONTENTS_MAX_ANCHORS {
                break;
            }
        }
        if end == text.len() {
            break;
        }
        start = end + text[end..].chars().next().unwrap().len_utf8();
    }
    anchors
}

#[cfg(feature = "structure-inference")]
fn instrument_contents_heads(
    region: &str,
    text: &ScalarText<'_>,
    region_start_byte: usize,
    region_start_utf16: usize,
) -> Vec<InstrumentContentsHead> {
    // Heads, not line breaks, delimit entries because source formats variously
    // pack entries, preserve spacing, or break them mid-entry. Schedule-like
    // heads require a line boundary or two spaces because their vocabulary
    // also appears inside entry titles.
    static HEAD: OnceLock<Regex> = OnceLock::new();
    let head = HEAD.get_or_init(|| {
        Regex::new(
            r"(?:(?P<container>ARTICLE|Article|PART|Part|DIVISION|Division)[ \t]+(?P<container_value>[IVXLCDM]{1,7}|[0-9]{1,3})[.:]?|(?P<schedule>SCHEDULE|Schedule|EXHIBIT|Exhibit|ANNEX|Annex|APPENDIX|Appendix)[ \t]+(?P<schedule_value>[A-Z0-9][A-Za-z0-9_.-]{0,12}?)[.:]?|(?P<section_word>Section|SECTION)[ \t]+(?P<section>[0-9]{1,3}(?:\.[0-9]{1,3})*[A-Za-z]?)[.)]?|(?P<decimal>[0-9]{1,3}\.[0-9]{1,3}(?:\.[0-9]{1,3})*)[.)]?|(?P<integer>[0-9]{1,3})[.)])(?P<trail>[ \t\r\n]|$)",
        )
        .expect("valid instrument contents head grammar")
    });
    let mut heads = Vec::<InstrumentContentsHead>::new();
    let mut search = 0;
    while search <= region.len() {
        let Some(found) = head.captures_at(region, search) else {
            break;
        };
        let whole = found.get(0).expect("contents head match");
        let trail = found.name("trail").expect("contents head trail");
        let body = found
            .name("container")
            .or_else(|| found.name("schedule"))
            .or_else(|| found.name("section_word"))
            .or_else(|| found.name("decimal"))
            .or_else(|| found.name("integer"))
            .expect("contents head body");
        let before = region[..body.start()].chars().next_back();
        let valid_lead = body.start() == 0 || before.is_some_and(javascript_whitespace);
        let schedule_lead = || {
            let mut before = region[..body.start()].chars().rev();
            match before.next() {
                Some('\r' | '\n') => true,
                Some(' ' | '\t') => before
                    .next()
                    .is_some_and(|value| matches!(value, ' ' | '\t')),
                _ => false,
            }
        };
        let kind = if !valid_lead {
            None
        } else if let (Some(word), Some(value)) =
            (found.name("container"), found.name("container_value"))
        {
            Some(InstrumentContentsHeadKind::Container {
                word: word.as_str().to_owned(),
                value: value.as_str().to_owned(),
            })
        } else if let (Some(word), Some(value)) =
            (found.name("schedule"), found.name("schedule_value"))
        {
            schedule_lead().then(|| InstrumentContentsHeadKind::Schedule {
                word: word.as_str().to_owned(),
                value: value.as_str().to_owned(),
            })
        } else {
            found
                .name("section")
                .or_else(|| found.name("decimal"))
                .or_else(|| found.name("integer"))
                .map(|number| InstrumentContentsHeadKind::Section {
                    number: number.as_str().to_owned(),
                })
        };
        if let Some(kind) = kind {
            let start_byte = body.start();
            let start_utf16 = text
                .utf16_at_byte(region_start_byte + start_byte)
                .expect("regex start is a UTF-8 boundary")
                - region_start_utf16;
            if heads
                .last()
                .map_or(start_utf16 > CONTENTS_MAX_ENTRY_GAP_UTF16, |prior| {
                    start_utf16 - prior.end_utf16 > CONTENTS_MAX_ENTRY_GAP_UTF16
                })
            {
                break;
            }
            heads.push(InstrumentContentsHead {
                start_byte,
                end_byte: trail.start(),
                start_utf16,
                end_utf16: text
                    .utf16_at_byte(region_start_byte + trail.start())
                    .expect("regex end is a UTF-8 boundary")
                    - region_start_utf16,
                kind,
            });
            search = trail.start();
        } else {
            search = body.start() + body.as_str().chars().next().unwrap().len_utf8();
        }
        if search >= region.len() {
            break;
        }
        if search <= whole.start() {
            search = whole.end();
        }
    }
    heads
}

#[cfg(feature = "structure-inference")]
fn instrument_contents_unit_end(value: &str) -> Option<usize> {
    // Printed page footers occur between blank lines inside contents pages;
    // absorbing one as an entry page creates a false page decrease.
    let mut search = 0;
    while let Some(relative) = value[search..].find('\n') {
        let newline = search + relative;
        let mut start = newline;
        for (byte, character) in value[..newline].char_indices().rev() {
            if character == '\n' || !javascript_whitespace(character) {
                break;
            }
            start = byte;
        }

        let after_newline = newline + 1;
        let mut closes_blank_line = false;
        for character in value[after_newline..].chars() {
            if character == '\n' {
                closes_blank_line = true;
                break;
            }
            if !javascript_whitespace(character) {
                break;
            }
        }
        if closes_blank_line {
            return Some(start);
        }
        search = after_newline;
    }
    None
}

#[cfg(feature = "structure-inference")]
fn instrument_roman_value(value: &str) -> Option<u32> {
    let values = |character| match character {
        'I' => Some(1),
        'V' => Some(5),
        'X' => Some(10),
        'L' => Some(50),
        'C' => Some(100),
        'D' => Some(500),
        'M' => Some(1000),
        _ => None,
    };
    let mut characters = value.chars().peekable();
    let mut total = 0i32;
    while let Some(character) = characters.next() {
        let current = values(character)?;
        let next = characters.peek().copied().and_then(values).unwrap_or(0);
        total += if current < next { -current } else { current };
    }
    u32::try_from(total).ok()
}

#[cfg(feature = "structure-inference")]
fn instrument_contents_region(
    text: &ScalarText<'_>,
    from_byte: usize,
    from_utf16: usize,
) -> Option<InstrumentContentsOutline> {
    // Original-text offsets: the window floors a split surrogate and the
    // final-entry lookahead ceils one; exact regex boundaries never round.
    let requested_utf16 = CONTENTS_WINDOW_UTF16.min(text.utf16_len() - from_utf16);
    let region_end = text
        .byte_at_utf16_floor(from_utf16 + requested_utf16)
        .expect("bounded UTF-16 window");
    let region_utf16 = text
        .utf16_at_byte(region_end)
        .expect("contents window ends at a UTF-8 boundary")
        - from_utf16;
    let region = &text.value[from_byte..region_end];
    let heads = instrument_contents_heads(region, text, from_byte, from_utf16);
    if heads.is_empty() || heads[0].start_utf16 > CONTENTS_MAX_ENTRY_GAP_UTF16 {
        return None;
    }

    let mut entries = Vec::new();
    let mut by_label: HashMap<String, usize> = HashMap::new();
    let mut container: Option<String> = None;
    let mut previous_page = 0;
    let mut pageless = 0;
    let mut pageless_from = 0;
    let mut last_head: Option<usize> = None;
    for (index, head) in heads.iter().enumerate() {
        if index > 0 && head.start_utf16 - heads[index - 1].end_utf16 > CONTENTS_MAX_ENTRY_GAP_UTF16
        {
            break;
        }
        let until_byte = if heads
            .get(index + 1)
            .is_some_and(|next| next.start_utf16 - head.end_utf16 <= CONTENTS_MAX_ENTRY_GAP_UTF16)
        {
            heads[index + 1].start_byte
        } else {
            text.byte_at_utf16_ceil(from_utf16 + (head.end_utf16 + 200).min(region_utf16))
                .expect("bounded UTF-16 contents cut")
                - from_byte
        };
        let raw = &region[head.end_byte..until_byte];
        let raw = instrument_contents_unit_end(raw).map_or(raw, |cut| &raw[..cut]);
        let unit = normalize_javascript_whitespace(raw);
        let page_match_start = unit.rfind(' ').map_or(0, |space| space);
        let page_token = if page_match_start == 0 {
            unit.as_str()
        } else {
            &unit[page_match_start + 1..]
        };
        let page = (!page_token.is_empty()
            && page_token.len() <= 4
            && page_token.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| page_token.parse::<u32>().ok())
        .flatten();
        if page.is_some_and(|page| page < previous_page) {
            break;
        }

        let (label, display, depth, parent_label, is_container) = match &head.kind {
            InstrumentContentsHeadKind::Container { word, value } => {
                let number = value
                    .parse::<u32>()
                    .ok()
                    .or_else(|| instrument_roman_value(&value.to_ascii_uppercase()));
                let Some(number) = number else { continue };
                let lower = word.to_ascii_lowercase();
                let prefix = match lower.as_str() {
                    "article" => "art",
                    "part" => "part",
                    _ => "div",
                };
                (
                    format!("{prefix}{number}"),
                    format!("{} {value}", word.to_ascii_uppercase()),
                    0,
                    None,
                    true,
                )
            }
            InstrumentContentsHeadKind::Schedule { word, value } => {
                let prefix = match word.to_ascii_lowercase().as_str() {
                    "schedule" => "sched",
                    "exhibit" => "exh",
                    "annex" => "annex",
                    _ => "app",
                };
                (
                    format!("{prefix}{}", value.to_ascii_lowercase()),
                    format!("{} {value}", word.to_ascii_uppercase()),
                    0,
                    None,
                    true,
                )
            }
            InstrumentContentsHeadKind::Section { number } => {
                let numbered_parent = number.rfind('.').and_then(|dot| {
                    let parent = format!("sec{}", &number[..dot]);
                    by_label
                        .get(&parent)
                        .copied()
                        .map(|depth| (parent, depth + 1))
                });
                let (parent, depth) = numbered_parent
                    .map(|(parent, depth)| (Some(parent), depth))
                    .unwrap_or_else(|| (container.clone(), usize::from(container.is_some())));
                (
                    format!("sec{number}"),
                    format!("Section {number}"),
                    depth,
                    parent,
                    false,
                )
            }
        };
        if by_label.contains_key(&label) {
            break;
        }
        if page.is_none() {
            if pageless == 0 {
                pageless_from = entries.len();
            }
            pageless += 1;
            if pageless > MAX_PAGELESS_RUN {
                entries.truncate(pageless_from);
                break;
            }
        } else {
            pageless = 0;
            previous_page = page.unwrap();
        }
        if is_container {
            container = Some(label.clone());
        }
        let heading_source = page.map_or(unit.as_str(), |_| &unit[..page_match_start]);
        let heading = heading_source
            .trim_end_matches(|character: char| {
                character == '.' || character == '\u{2026}' || javascript_whitespace(character)
            })
            .trim_start_matches(|character: char| {
                javascript_whitespace(character)
                    || matches!(character, '\u{2013}' | '\u{2014}' | '-' | ':' | '.')
            })
            .trim_matches(javascript_whitespace)
            .to_owned();
        let entry = InstrumentContentsEntry {
            label: label.clone(),
            display,
            heading,
            depth,
            parent_label,
            page,
            contents_line_start: from_utf16 + head.start_utf16,
        };
        by_label.insert(label, depth);
        entries.push(entry);
        last_head = Some(index);
    }
    if entries.is_empty() {
        return None;
    }
    let last_head = last_head?;
    Some(InstrumentContentsOutline {
        pages_cited: entries.iter().filter(|entry| entry.page.is_some()).count(),
        region_start: from_utf16 + heads[0].start_utf16,
        region_end: from_utf16 + heads[last_head].end_utf16,
        entries,
    })
}

/// Read a document's own table of contents as a page-addressed outline. The
/// outline never claims provision spans; ambiguous inputs receive a typed refusal.
#[cfg(feature = "structure-inference")]
#[cfg(test)]
fn instrument_contents_outline(text: &str) -> InstrumentContentsReading {
    instrument_contents_outline_indexed(&ScalarText::new(text))
}

#[cfg(feature = "structure-inference")]
fn instrument_contents_outline_from_anchors(
    text: &ScalarText<'_>,
    anchors: Vec<usize>,
) -> InstrumentContentsReading {
    let mut refusal = InstrumentContentsRefusal::NoContentsEntries;
    for from_byte in anchors {
        let from_utf16 = text
            .utf16_at_byte(from_byte)
            .expect("contents anchor end is a UTF-8 boundary");
        let Some(outline) = instrument_contents_region(text, from_byte, from_utf16) else {
            continue;
        };
        if outline.entries.len() < MIN_CONTENTS_ENTRIES {
            refusal = InstrumentContentsRefusal::TooFewContentsEntries;
            continue;
        }
        if outline.pages_cited as f64 / (outline.entries.len() as f64) < MIN_CONTENTS_PAGE_SHARE {
            refusal = InstrumentContentsRefusal::ContentsWithoutPageNumbers;
            continue;
        }
        return InstrumentContentsReading {
            outline: Some(outline),
            refusal: None,
        };
    }
    InstrumentContentsReading {
        outline: None,
        refusal: Some(refusal),
    }
}

#[cfg(feature = "structure-inference")]
fn instrument_contents_outline_indexed(text: &ScalarText<'_>) -> InstrumentContentsReading {
    let anchors = instrument_contents_anchors(text.value);
    if anchors.is_empty() {
        InstrumentContentsReading {
            outline: None,
            refusal: Some(InstrumentContentsRefusal::NoContentsMarker),
        }
    } else {
        instrument_contents_outline_from_anchors(text, anchors)
    }
}

#[cfg(feature = "structure-inference")]
fn instrument_roman(mut value: usize) -> String {
    let mut result = String::new();
    for (amount, numeral) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while value >= amount {
            result.push_str(numeral);
            value -= amount;
        }
    }
    result
}

#[cfg(feature = "structure-inference")]
fn reference_node_kind(label: &str) -> &'static str {
    if label.starts_with("art") {
        "article"
    } else if label.starts_with("part") {
        "part"
    } else if label.starts_with("div") {
        "division"
    } else if ["sched", "exh", "annex", "app"]
        .iter()
        .any(|prefix| label.starts_with(prefix))
    {
        "schedule"
    } else if label.contains('(') {
        "subsection"
    } else {
        "section"
    }
}

#[cfg(feature = "structure-inference")]
fn populate_instrument_node_metadata(nodes: &mut [crate::StructureNode], text: &ScalarText<'_>) {
    for node in nodes
        .iter_mut()
        .filter(|node| node.kind == NodeKind::Section)
    {
        let Some(label) = node.label.as_deref() else {
            continue;
        };
        let kind = reference_node_kind(&label);
        node.locator_kind = Some(kind.to_owned());
        let body = match kind {
            "article" => label.strip_prefix("art"),
            "part" => label.strip_prefix("part"),
            "division" => label.strip_prefix("div"),
            "schedule" if label.starts_with("sched") => label.strip_prefix("sched"),
            "schedule" if label.starts_with("exh") => label.strip_prefix("exh"),
            "schedule" if label.starts_with("annex") => label.strip_prefix("annex"),
            "schedule" => label.strip_prefix("app"),
            _ => label.strip_prefix("sec"),
        }
        .unwrap_or(label);
        let word = match kind {
            "article" => "article",
            "part" => "part",
            "division" => "division",
            "schedule" if label.starts_with("exh") => "exhibit",
            "schedule" if label.starts_with("annex") => "annex",
            "schedule" if label.starts_with("app") => "appendix",
            "schedule" => "schedule",
            _ => "section",
        };
        let raw_head = node
            .content_start
            .and_then(|end| {
                Some((
                    text.byte_at_utf16(node.range.start)?,
                    text.byte_at_utf16(end)?,
                ))
            })
            .and_then(|(start, end)| text.value.get(start..end))
            .map_or("", |value| value.trim_matches(javascript_whitespace));
        let display = if matches!(kind, "section" | "subsection") {
            format!("section {body}").to_lowercase()
        } else if kind == "schedule" {
            let raw_head = raw_head.to_lowercase();
            let mut words = raw_head.split_ascii_whitespace();
            let schedule_word = words.next().unwrap_or(word);
            let raw_value = words.next().unwrap_or(body).trim_end_matches(':');
            let value = raw_value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
                .then_some(raw_value)
                .unwrap_or(body);
            format!("{schedule_word} {value}")
        } else if raw_head.is_empty() {
            format!("{word} {body}")
        } else {
            raw_head
                .trim_end_matches(|character: char| {
                    matches!(character, '\u{2013}' | '\u{2014}' | '-' | '.' | ':')
                })
                .trim_matches(javascript_whitespace)
                .to_lowercase()
        };
        let mut aliases = vec![display];
        if matches!(kind, "article" | "part") {
            if let Ok(value) = body.parse::<usize>() {
                aliases.push(format!("{word} {}", instrument_roman(value)).to_lowercase());
            }
        }
        let label = label.to_lowercase();
        aliases.retain(|alias| alias != &label);
        aliases.dedup();
        node.aliases = (!aliases.is_empty()).then_some(aliases);
        node.marker_range = node.content_start.map(|end| ScalarRange {
            start: node.range.start,
            end,
        });
    }
}

#[cfg(feature = "structure-inference")]
struct ReferenceNode<'a> {
    label: &'a str,
    aliases: &'a [String],
    anchor: Option<&'a str>,
    parent_label: Option<&'a str>,
    kind: &'static str,
    start: usize,
    end: usize,
    depth: usize,
}

#[cfg(feature = "structure-inference")]
fn reference_nodes<'a>(
    graph: &'a DocumentStructure,
    depths: &HashMap<&str, usize>,
) -> Vec<ReferenceNode<'a>> {
    let by_id = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Section && node.label.is_some())
        .map(|node| {
            let label = node.label.as_deref().unwrap();
            let kind = reference_node_kind(&label);
            let parent_label = node
                .parent_id
                .as_deref()
                .and_then(|id| by_id.get(id))
                .and_then(|parent| parent.label.as_deref());
            ReferenceNode {
                label,
                aliases: node.aliases.as_deref().unwrap_or_default(),
                anchor: node.anchor.as_deref(),
                parent_label,
                kind,
                start: node.range.start,
                end: node.range.end,
                depth: depths[node.id.as_str()],
            }
        })
        .collect()
}

#[cfg(feature = "structure-inference")]
fn node_keys(node: &ReferenceNode, label: String) -> Vec<String> {
    let mut keys = vec![label];
    let roman = match node.kind {
        "article" => node
            .label
            .strip_prefix("art")
            .map(|value| ("article", value)),
        "part" => node.label.strip_prefix("part").map(|value| ("part", value)),
        "division" => node
            .label
            .strip_prefix("div")
            .map(|value| ("division", value)),
        _ => None,
    };
    if let Some((word, value)) =
        roman.and_then(|(word, value)| value.parse().ok().map(|n| (word, n)))
    {
        keys.push(format!("{word} {}", instrument_roman(value)).to_lowercase());
    } else if matches!(node.kind, "section" | "subsection") {
        keys.push(node.label.replacen("sec", "section ", 1).to_lowercase());
    }
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(feature = "structure-inference")]
fn label_parent(locator: &str) -> Option<&str> {
    let body = locator.strip_prefix("sec")?;
    let body = body.split('@').next().unwrap_or(body);
    body.ends_with(')').then_some(())?;
    let open = body.rfind('(')?;
    Some(&locator[..3 + open])
}

#[cfg(feature = "structure-inference")]
fn label_depth(locator: &str) -> usize {
    locator.strip_prefix("sec").map_or(1, |body| {
        1 + body.split('@').next().unwrap_or(body).matches('(').count()
    })
}

#[cfg(feature = "structure-inference")]
fn containing_reference_node(
    nodes: &[ReferenceNode],
    ordered: &[usize],
    by_label: &HashMap<&str, usize>,
    position: usize,
) -> Option<usize> {
    let at = ordered.partition_point(|index| nodes[*index].start <= position);
    let mut node = at.checked_sub(1).map(|index| ordered[index]);
    while let Some(index) = node {
        if position < nodes[index].end {
            return Some(index);
        }
        node = nodes[index]
            .parent_label
            .as_deref()
            .and_then(|label| by_label.get(label).copied());
    }
    None
}

#[cfg(feature = "structure-inference")]
fn reference_locator(reference: &ProvisionReference, source: Option<&ReferenceNode>) -> String {
    if !reference.locator.is_empty() {
        return reference.locator.clone();
    }
    if reference.shape == ProvisionReferenceShape::Roman {
        return reference.alias_key.clone();
    }
    if reference.shape != ProvisionReferenceShape::SubOnly {
        return String::new();
    }
    let Some(source) = source else {
        return String::new();
    };
    let Some(body) = source.label.strip_prefix("sec") else {
        return String::new();
    };
    let head = body.split(['(', '@']).next().unwrap_or("");
    (!head.is_empty())
        .then(|| normalize_compact_numbered_section_locator(&format!("{head}{}", reference.label)))
        .unwrap_or_default()
}

#[cfg(feature = "structure-inference")]
fn js_percent(value: f64) -> u64 {
    (value + 0.5).floor() as u64
}

#[cfg(feature = "structure-inference")]
fn resolve_instrument_references(
    text: &ScalarText<'_>,
    graph: &DocumentStructure,
    references: Vec<ProvisionReference>,
    depths: &HashMap<&str, usize>,
) -> Result<InstrumentCrossReferenceGraph, EngineError> {
    const MIN_ADDRESSABLE_NODES: usize = 3;
    const MIN_TARGET_REACH: f64 = 0.05;
    const MIN_TARGETS_FOR_REACH: usize = 3;
    const INTEGRITY_GATE: f64 = 0.5;

    let nodes = reference_nodes(graph, depths);
    let mut by_label = HashMap::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        by_label.entry(node.label).or_insert(index);
    }
    let mut ordered = (0..nodes.len()).collect::<Vec<_>>();
    ordered.sort_by_key(|index| (nodes[*index].start, nodes[*index].depth));

    let mut targets =
        HashMap::<String, (Option<usize>, usize)>::with_capacity(nodes.len().saturating_mul(2));
    let mut child_depths = HashMap::<String, HashSet<usize>>::with_capacity(nodes.len());
    let mut top_level_numeric = 0;
    let mut containers = 0;
    static TOP_LEVEL: OnceLock<Regex> = OnceLock::new();
    let top_level = TOP_LEVEL.get_or_init(|| {
        Regex::new(r"(?i)^sec\d{1,8}[a-z]{0,3}(?:[.-]\d{1,8}[a-z]{0,3}){0,3}$")
            .expect("valid top-level provision grammar")
    });
    for (node_index, node) in nodes.iter().enumerate() {
        let label = node.label.to_lowercase();
        if let Some(parent) = label_parent(&label) {
            child_depths
                .entry(parent.to_owned())
                .or_default()
                .insert(label_depth(&label));
        } else if top_level.is_match(&label) {
            top_level_numeric += 1;
        }
        if matches!(node.kind, "article" | "part" | "division") {
            containers += 1;
        }
        let mut add = |key, ambiguity| {
            let target = targets.entry(key).or_insert((Some(node_index), 0));
            target.1 += usize::from(ambiguity);
            if target.0 != Some(node_index) {
                target.0 = None;
            }
        };
        let mut keys = node_keys(node, label);
        keys.extend(node.aliases.iter().map(|key| key.to_lowercase()));
        keys.sort();
        keys.dedup();
        for key in keys {
            add(key, true);
        }
        if let Some(anchor) = node.anchor {
            add(anchor.to_lowercase(), false);
        }
    }
    let numbers_here = |locator: &str| {
        if let Some(parent) = label_parent(locator) {
            child_depths
                .get(parent)
                .is_some_and(|depths| depths.contains(&label_depth(locator)))
        } else if !locator.starts_with("sec") {
            containers >= MIN_ADDRESSABLE_NODES
        } else {
            top_level.is_match(locator) && top_level_numeric >= MIN_ADDRESSABLE_NODES
        }
    };

    let thin = nodes.len() < MIN_ADDRESSABLE_NODES;
    let mut counts = InstrumentCrossReferenceCounts {
        detected: references.len(),
        resolved: 0,
        external: 0,
        unresolved: 0,
        abstained: 0,
        self_loops: 0,
        integrity: 1.0,
    };
    let mut edges = Vec::with_capacity(references.len());
    for reference in references {
        let source_index = containing_reference_node(&nodes, &ordered, &by_label, reference.start);
        let source = source_index.map(|index| &nodes[index]);
        let locator = reference_locator(&reference, source);
        let mut edge = InstrumentCrossReferenceEdge {
            source_start: reference.start,
            source_end: reference.end,
            source_label: source.map(|node| node.label.to_owned()),
            raw: reference.raw,
            raw_label: reference.label,
            normalized_locator: locator,
            target_label: None,
            target_start: None,
            target_end: None,
            status: InstrumentCrossReferenceStatus::External,
            self_loop: false,
            reason: None,
        };
        if reference.external {
            counts.external += 1;
            edge.reason = Some(InstrumentCrossReferenceReason::ExternalInstrument);
        } else if thin {
            counts.abstained += 1;
            edge.status = InstrumentCrossReferenceStatus::Abstained;
            edge.reason = Some(InstrumentCrossReferenceReason::DocumentAbstained);
        } else if edge.normalized_locator.is_empty() {
            counts.abstained += 1;
            edge.status = InstrumentCrossReferenceStatus::Abstained;
            edge.reason = Some(InstrumentCrossReferenceReason::NoContainingSection);
        } else {
            let lowercase = (!edge.normalized_locator.is_ascii()
                || edge
                    .normalized_locator
                    .bytes()
                    .any(|byte| byte.is_ascii_uppercase()))
            .then(|| edge.normalized_locator.to_lowercase());
            let locator_key = lowercase.as_deref().unwrap_or(&edge.normalized_locator);
            let target = targets.get(locator_key);
            if let Some(target) = target.and_then(|value| value.0) {
                counts.resolved += 1;
                edge.status = InstrumentCrossReferenceStatus::Resolved;
                edge.target_label = Some(nodes[target].label.to_owned());
                edge.target_start = Some(nodes[target].start);
                edge.target_end = Some(nodes[target].end);
                edge.self_loop = source.is_some_and(|source| source.label == nodes[target].label);
                counts.self_loops += usize::from(edge.self_loop);
            } else if target.is_some_and(|value| value.1 > 1) {
                counts.abstained += 1;
                edge.status = InstrumentCrossReferenceStatus::Abstained;
                edge.reason = Some(InstrumentCrossReferenceReason::AmbiguousLabel);
            } else if !numbers_here(locator_key) {
                counts.abstained += 1;
                edge.status = InstrumentCrossReferenceStatus::Abstained;
                edge.reason = Some(InstrumentCrossReferenceReason::DepthNotNumbered);
            } else {
                counts.unresolved += 1;
                edge.status = InstrumentCrossReferenceStatus::Unresolved;
                edge.reason = Some(InstrumentCrossReferenceReason::NoSuchProvision);
            }
        }
        edges.push(edge);
    }

    let accepted = counts.resolved + counts.unresolved;
    counts.integrity = if accepted == 0 {
        1.0
    } else {
        counts.resolved as f64 / accepted as f64
    };
    if thin {
        return Ok(InstrumentCrossReferenceGraph {
            edges,
            document_abstained: true,
            note: Some(format!(
                "Cross-reference resolution abstained: the document compiles to {} addressable provision(s), below the {MIN_ADDRESSABLE_NODES} needed for a numbering scheme to check against.",
                nodes.len()
            )),
            counts,
        });
    }
    let (target_count, furthest_target) = edges
        .iter()
        .filter(|edge| edge.status == InstrumentCrossReferenceStatus::Resolved)
        .filter_map(|edge| edge.target_start)
        .fold((0, 0), |(count, furthest), target| {
            (count + 1, furthest.max(target))
        });
    let reach = if text.utf16_len() == 0 {
        1.0
    } else {
        furthest_target as f64 / text.utf16_len() as f64
    };
    let contents_only = target_count >= MIN_TARGETS_FOR_REACH && reach < MIN_TARGET_REACH;
    if contents_only || (accepted > 0 && counts.integrity < INTEGRITY_GATE) {
        for edge in &mut edges {
            if edge.status != InstrumentCrossReferenceStatus::External {
                edge.status = InstrumentCrossReferenceStatus::Abstained;
                edge.reason = Some(InstrumentCrossReferenceReason::DocumentAbstained);
                edge.target_label = None;
                edge.target_start = None;
                edge.target_end = None;
                edge.self_loop = false;
            }
        }
        let note = if contents_only {
            format!(
                "Cross-reference resolution abstained: every one of {} resolved targets lands in the first {}% of the document, so the only numbering the compiler can see is a table of contents, not the provisions.",
                target_count,
                js_percent(reach * 100.0)
            )
        } else {
            format!(
                "Cross-reference resolution abstained: only {} of {accepted} resolvable references ({}%) landed on a compiled provision, below the {}% needed to trust this document's numbering scheme.",
                counts.resolved,
                js_percent(counts.integrity * 100.0),
                js_percent(INTEGRITY_GATE * 100.0)
            )
        };
        counts.abstained += accepted;
        counts.resolved = 0;
        counts.unresolved = 0;
        counts.self_loops = 0;
        return Ok(InstrumentCrossReferenceGraph {
            edges,
            document_abstained: true,
            note: Some(note),
            counts,
        });
    }
    Ok(InstrumentCrossReferenceGraph {
        edges,
        document_abstained: false,
        note: None,
        counts,
    })
}

#[cfg(feature = "structure-inference")]
pub(crate) fn cross_reference_graph(
    structure: &DocumentStructure,
) -> Result<InstrumentCrossReferenceGraph, EngineError> {
    let text = ScalarText::new(&structure.text);
    let references = find_provision_references(&text, FindProvisionReferencesOptions::default());
    let depths = node_depths(&structure.nodes);
    resolve_instrument_references(&text, structure, references, &depths)
}

#[cfg(feature = "structure-inference")]
fn instrument_reference_index<'a>(
    sections: impl Iterator<Item = (&'a str, usize)>,
) -> HashMap<String, usize> {
    let mut index = HashMap::new();
    let mut duplicates = HashSet::new();
    for (label, start) in sections {
        let aliases = [("art", "article"), ("part", "part"), ("div", "division")];
        let keys = std::iter::once(label.to_ascii_lowercase()).chain(
            aliases.into_iter().filter_map(|(prefix, word)| {
                label
                    .strip_prefix(prefix)
                    .and_then(|value| value.parse().ok())
                    .map(|value| format!("{word} {}", instrument_roman(value)))
            }),
        );
        for key in keys {
            match index.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(start);
                }
                Entry::Occupied(entry) => {
                    duplicates.insert(entry.key().clone());
                }
            }
        }
    }
    for key in duplicates {
        index.remove(&key);
    }
    index
}

#[cfg(feature = "structure-inference")]
fn endorsed_references(references: &[ProvisionReference]) -> Vec<(String, usize, usize)> {
    references
        .iter()
        .filter(|reference| !reference.external)
        .filter_map(|reference| {
            let key = if reference.shape == ProvisionReferenceShape::Roman {
                reference.alias_key.clone()
            } else {
                reference.locator.to_lowercase()
            };
            (!key.is_empty()).then_some((key, reference.start, reference.end))
        })
        .collect()
}

#[cfg(feature = "structure-inference")]
fn instrument_lineation_score_blocks(
    blocks: &[Block],
    endorsed: &[(String, usize, usize)],
) -> usize {
    instrument_lineation_score_sections(
        blocks.iter().filter_map(|block| {
            (block.kind == NodeKind::Section)
                .then_some(block.label.as_deref())
                .flatten()
                .map(|label| (label, block.range.start))
        }),
        endorsed,
    )
}

#[cfg(feature = "structure-inference")]
fn instrument_lineation_score_sections<'a>(
    sections: impl Iterator<Item = (&'a str, usize)>,
    endorsed: &[(String, usize, usize)],
) -> usize {
    let index = instrument_reference_index(sections);
    endorsed
        .iter()
        .filter(|(key, start, end)| {
            index
                .get(key)
                .is_some_and(|target| *target < *start || *target >= *end)
        })
        .count()
}

#[cfg(feature = "structure-inference")]
fn instrument_block_head_span(blocks: &[Block], text_length: usize) -> f64 {
    instrument_head_span_sections(
        blocks.iter().filter_map(|block| {
            let label = block.label.as_deref()?;
            (block.kind == NodeKind::Section).then_some((label, block.range.start))
        }),
        text_length,
    )
}

#[cfg(feature = "structure-inference")]
fn instrument_head_span_sections<'a>(
    sections: impl Iterator<Item = (&'a str, usize)>,
    text_length: usize,
) -> f64 {
    let mut starts = sections.filter_map(|(label, start)| {
        (label.starts_with("sec") && !label.contains('(')).then_some(start)
    });
    let Some(first) = starts.next() else {
        return 0.0;
    };
    let (mut low, mut high) = (first, first);
    for start in starts {
        low = low.min(start);
        high = high.max(start);
    }
    if text_length == 0 {
        0.0
    } else {
        (high - low) as f64 / text_length as f64
    }
}

#[cfg(feature = "structure-inference")]
fn derive_instrument_structure(
    text: &ScalarText<'_>,
    document_id: String,
    original_sha256: &str,
    tables: &AuthoritativeTables,
    mut hypotheses: impl Iterator<Item = String>,
) -> Result<(DocumentStructure, bool), EngineError> {
    let references = find_provision_references(text, FindProvisionReferencesOptions::default());
    let endorsed = endorsed_references(&references);
    let first = hypotheses
        .next()
        .ok_or_else(|| EngineError::invalid("instrument lineation selection requires a graph"))?;
    let first_is_original = tables.is_empty();
    let mut selected_text = tables.masked_text(first);
    let mut selected_blocks = if first_is_original {
        crate::inference::detect_instrument(text)
    } else {
        let selected_view = if selected_text.len() == text.value.len() {
            text.with_same_coordinates(&selected_text)
        } else {
            ScalarText::new(&selected_text)
        };
        crate::inference::detect_instrument(&selected_view)
    };
    let mut selected = 0;
    let mut best = if endorsed.is_empty() {
        0
    } else {
        instrument_lineation_score_blocks(&selected_blocks, &endorsed)
    };
    if best < endorsed.len() {
        for (index, hypothesis) in hypotheses.enumerate() {
            let candidate_text = tables.masked_text(hypothesis);
            let candidate_view = if candidate_text.len() == text.value.len() {
                text.with_same_coordinates(&candidate_text)
            } else {
                ScalarText::new(&candidate_text)
            };
            let candidate = crate::inference::detect_instrument(&candidate_view);
            if instrument_block_head_span(&candidate, text.utf16_len()) < 0.05 {
                continue;
            }
            let score = instrument_lineation_score_blocks(&candidate, &endorsed);
            if score > best {
                selected = index + 1;
                best = score;
                selected_text = candidate_text;
                selected_blocks = candidate;
            }
            if best == endorsed.len() {
                break;
            }
        }
    }
    let selected_original = selected == 0 && first_is_original;
    let scalar_end = text.len();
    let input = DocumentInput {
        schema_version: EVIDENCE_SCHEMA.to_owned(),
        document_id,
        provider: "internal".to_owned(),
        #[cfg(feature = "document-query")]
        url: None,
        #[cfg(feature = "document-query")]
        doc_type: None,
        provider_revision: "legal-text-skeleton-v5".to_owned(),
        profile: DetectionProfile::Instrument,
        report_start_page: None,
        require_report_start: false,
        allow_hyphenated_sections: false,
        text: selected_text,
        text_sha256: selected_original
            .then(|| original_sha256.to_owned())
            .unwrap_or_default(),
        source_sha256: None,
        offset_unit: "unicode-scalar".to_owned(),
        scope: Scope::complete(),
        origins: vec![Origin {
            id: "provider-adapter".to_owned(),
        }],
        native_claims: Vec::new(),
        coverage: crate::whole_document_coverage(scalar_end, |_| CoverageState::Absent),
        exclusions: Vec::new(),
        paragraph_breaks: Vec::<ParagraphBreak>::new(),
    };
    let mut structure = crate::derive::derive_trusted_inferred(input, selected_blocks)?;
    structure.selected_hypothesis = Some(selected);
    let selected_coordinates = (!selected_original).then(|| ScalarText::new(&structure.text));
    populate_instrument_node_metadata(
        &mut structure.nodes,
        selected_coordinates.as_ref().unwrap_or(text),
    );
    structure.contents = Some(instrument_contents_outline_indexed(text));
    let table_nodes = tables.nodes(&structure.nodes, "provider-adapter");
    structure.nodes.extend(table_nodes);
    let depths = node_depths(&structure.nodes);
    structure.cross_references = Some(resolve_instrument_references(
        text, &structure, references, &depths,
    )?);
    let mut sections = structure
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == NodeKind::Section)
        .collect::<Vec<_>>();
    sections.sort_by_key(|(index, node)| (node.range.start, *index));
    let lines = text.lines();
    let raw = derive_definitions_bytes(text, lines.len(), |index| {
        (lines[index][0], lines[index][1])
    });
    let mut paragraphs = raw
        .iter()
        .flat_map(|term| term.definitions.iter().chain(&term.uses))
        .map(|hit| hit.paragraph)
        .collect::<Vec<_>>();
    paragraphs.sort_unstable();
    paragraphs.dedup();
    let mut next_section = 0;
    let mut active_sections = Vec::new();
    let mut owners = Vec::with_capacity(paragraphs.len());
    for &paragraph in &paragraphs {
        let start = text.utf16_at_byte(lines[paragraph][0]).unwrap();
        while next_section < sections.len() && sections[next_section].1.range.start <= start {
            active_sections.push(sections[next_section]);
            next_section += 1;
        }
        active_sections.retain(|(_, node)| start < node.range.end);
        owners.push(
            active_sections
                .iter()
                .max_by_key(|(index, node)| (depths[node.id.as_str()], *index))
                .map(|(_, node)| node.id.as_str()),
        );
    }
    let occurrence = |hit: DefinitionHit| DefinitionOccurrence {
        range: ScalarRange {
            start: text.utf16_at_byte(hit.start).unwrap(),
            end: text.utf16_at_byte(hit.end).unwrap(),
        },
        node_id: owners[paragraphs.binary_search(&hit.paragraph).unwrap()].map(str::to_owned),
        source_paragraph_id: hit.paragraph.to_string(),
        source_artifact_id: Some(structure.document_id.clone()),
    };
    let definitions = raw
        .into_iter()
        .map(|term| DefinedTerm {
            term: term.term,
            definitions: term.definitions.into_iter().map(&occurrence).collect(),
            uses: term.uses.into_iter().map(&occurrence).collect(),
        })
        .collect();
    structure.definitions = definitions;
    if !tables.is_empty() {
        let depths = depths
            .into_iter()
            .map(|(id, depth)| (id.to_owned(), depth))
            .collect::<HashMap<_, _>>();
        structure
            .nodes
            .sort_by_key(|node| (node.range.start, depths[node.id.as_str()]));
    }
    Ok((structure, selected_original))
}

#[cfg(feature = "structure-inference")]
pub fn analyze_instrument(
    text: impl Into<String>,
    document_id: String,
    table_cells: &[AuthoritativeTableCell],
    reconstruct_lineation: bool,
) -> Result<DocumentStructure, EngineError> {
    let text = text.into();
    let hypotheses = reconstruct_lineation
        .then(|| instrument_lineation_hypotheses_iter(&text))
        .into_iter()
        .flatten()
        .chain((!reconstruct_lineation).then(|| text.clone()));
    let coordinates = ScalarText::new(&text);
    let tables = AuthoritativeTables::new(&coordinates, table_cells)?;
    let original_sha256 = format!("{:x}", Sha256::digest(text.as_bytes()));
    let (mut structure, selected_original) = derive_instrument_structure(
        &coordinates,
        document_id,
        &original_sha256,
        &tables,
        hypotheses,
    )?;
    if !selected_original {
        structure.text = text;
        structure.text_sha256.clone_from(&original_sha256);
        structure.revision = original_sha256;
    }
    Ok(structure)
}

#[cfg(all(test, feature = "structure-inference"))]
mod tests {
    use super::*;

    #[test]
    fn contents_preserve_packed_entries_and_nesting() {
        let text = "TABLE OF CONTENTS Page ARTICLE I DEFINITIONS 2 Section 1.01 Defined Terms 2 Section 1.02 Interpretation 4 ARTICLE II THE MERGER 5 Section 2.01 The Merger 5 Section 2.02 Closing 6";
        let reading = instrument_contents_outline(text);
        let outline = reading.outline.as_ref().expect("accepted outline");
        assert_eq!(
            outline
                .entries
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            ["art1", "sec1.01", "sec1.02", "art2", "sec2.01", "sec2.02"]
        );
        assert_eq!(outline.entries[0].heading, "DEFINITIONS");
        assert_eq!(outline.entries[1].parent_label.as_deref(), Some("art1"));
        assert_eq!(outline.entries[1].depth, 1);
        assert_eq!(outline.entries[4].parent_label.as_deref(), Some("art2"));
        assert_eq!(reading.refusal, None);
    }

    #[test]
    fn contents_match_all_refusals() {
        let cases = [
            (
                "The contents are discussed in prose.",
                InstrumentContentsRefusal::NoContentsMarker,
            ),
            (
                "CONTENTS\nnot an outline",
                InstrumentContentsRefusal::NoContentsEntries,
            ),
            (
                "CONTENTS\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 4",
                InstrumentContentsRefusal::TooFewContentsEntries,
            ),
            (
                "CONTENTS\nSection 1. First 1\nSection 2. Second\nSection 3. Third 2\nSection 4. Fourth\nSection 5. Fifth",
                InstrumentContentsRefusal::ContentsWithoutPageNumbers,
            ),
        ];
        for (text, expected) in cases {
            let reading = instrument_contents_outline(text);
            assert_eq!(reading.outline, None, "{text}");
            assert_eq!(reading.refusal, Some(expected), "{text}");
        }
        assert_eq!(
            serde_json::to_string(&instrument_contents_outline("plain text")).unwrap(),
            r#"{"outline":null,"refusal":"no_contents_marker"}"#
        );
    }

    #[test]
    fn contents_cut_page_footers_at_blank_lines() {
        let text = "TABLE OF CONTENTS\nARTICLE I DEFINITIONS 60\nSection 1.01 Defined Terms 61\n\n2\n\nSection 1.02 Interpretation 62\nSection 1.03 Currency 63\nSection 1.04 Notices 64\nSection 1.05 Time 65";
        let reading = instrument_contents_outline(text);
        let outline = reading.outline.as_ref().expect("accepted outline");
        assert_eq!(
            outline
                .entries
                .iter()
                .map(|entry| entry.page)
                .collect::<Vec<_>>(),
            [Some(60), Some(61), Some(62), Some(63), Some(64), Some(65)]
        );
    }

    #[test]
    fn contents_resume_inside_a_guarded_schedule() {
        let text = "TABLE OF CONTENTS\nSection 0. First 1\nSection 1. Company Schedule 2. Closing 2\nSection 3. Third 3\nSection 4. Fourth 4\nSection 5. Fifth 5";
        let reading = instrument_contents_outline(text);
        let outline = reading.outline.as_ref().expect("accepted outline");
        assert_eq!(
            outline
                .entries
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            ["sec0", "sec1", "sec2", "sec3", "sec4", "sec5"]
        );
        assert_eq!(outline.entries[1].heading, "Company Schedule");
        assert_eq!(outline.entries[1].page, None);
        assert_eq!(outline.entries[2].heading, "Closing");
        assert_eq!(outline.entries[2].page, Some(2));
    }

    #[test]
    fn contents_report_javascript_utf16_offsets() {
        let text = "\u{1f9ab}\nINDEX\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 4\nSection 5. Fifth 5";
        let reading = instrument_contents_outline(text);
        let outline = reading.outline.as_ref().expect("accepted outline");
        assert_eq!(outline.region_start, 9);
        assert_eq!(outline.entries[0].contents_line_start, 9);
    }

    #[test]
    fn contents_keep_astral_combining_text_and_crlf_offsets() {
        let text = "\u{1f9ab}\r\nINDEX\r\nSection 1. First \u{1f9ab} e\u{301} 1\rSection 2. Second 2\nSection 3. Third 3\r\nSection 4. Fourth 4\nSection 5. Fifth 5";
        let outline = instrument_contents_outline(text).outline.unwrap();
        assert_eq!(outline.region_start, 11);
        assert_eq!(outline.entries[0].contents_line_start, 11);
        assert_eq!(outline.entries[0].heading, "First \u{1f9ab} e\u{301}");
        assert_eq!(outline.entries.last().unwrap().label, "sec5");
    }

    #[test]
    fn contents_stop_on_decrease_duplicate_and_pageless_run() {
        let decrease = "CONTENTS\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 2\nSection 5. Fifth 5";
        assert_eq!(
            instrument_contents_outline(decrease).refusal,
            Some(InstrumentContentsRefusal::TooFewContentsEntries)
        );

        let duplicate = "CONTENTS\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 4\nSection 5. Fifth 5\nSection 3. Body 6\nSection 6. Sixth 6";
        let outline = instrument_contents_outline(duplicate).outline.unwrap();
        assert_eq!(outline.entries.len(), 5);
        assert_eq!(outline.entries.last().unwrap().label, "sec5");

        let pageless = "CONTENTS\nSection 1. First 1\nSection 2. Second 2\nSection 3. Third 3\nSection 4. Fourth 4\nSection 5. Fifth 5\nSection 6. Sixth\nSection 7. Seventh\nSection 8. Eighth\nSection 9. Ninth";
        let outline = instrument_contents_outline(pageless).outline.unwrap();
        assert_eq!(outline.entries.len(), 5);
        assert_eq!(outline.entries.last().unwrap().label, "sec5");
    }
}

#[cfg(all(test, feature = "structure-inference"))]
mod provision_reference_tests {
    use super::*;
    use crate::locator::normalize_numbered_section_locator;

    const ACACIA_EXTERNAL: &str =
        "â€œGroupâ€ has the meaning ascribed to such term under Section 13(d) of the Exchange Act.";
    const ACACIA_INTERNAL: &str =
        "in fulfilling its obligations under this Agreement, including under Section 5.3.";
    const ACACIA_LIST: &str =
        "the representations and warranties contained in Section 2.3(b), Section 6.3(a) and Section 7.1(f)), (J) any actions taken";
    const ACACIA_ROMAN: &str =
        "satisfaction or waiver of each of the conditions set forth in Article VI (other than those conditions that by their terms";
    const ACACIA_ORIGINAL_AGREEMENT: &str =
        "B. Pursuant to Section 7.4 of the Original Agreement, Parent, Sub and the Company";

    fn find(text: &str) -> Vec<ProvisionReference> {
        find_provision_references(
            &ScalarText::new(text),
            FindProvisionReferencesOptions::default(),
        )
    }

    #[test]
    fn requires_a_nonempty_label() {
        assert!(find("as provided in this section hereof").is_empty());
        assert!(find("each paragraph of this Agreement").is_empty());
    }

    #[test]
    fn reports_utf16_spans_labels_and_locators() {
        let found = find(ACACIA_INTERNAL);
        assert_eq!(found.len(), 1);
        let reference = &found[0];
        assert_eq!(reference.raw, "Section 5.3");
        assert_eq!(
            &ACACIA_INTERNAL[reference.start..reference.end],
            "Section 5.3"
        );
        assert_eq!(reference.word, "section");
        assert_eq!(reference.label, "5.3");
        assert_eq!(reference.locator, "sec5.3");
        assert_eq!(reference.shape, ProvisionReferenceShape::Numeric);
        assert!(!reference.external);

        let astral = find("\u{1f600} Section 5.3");
        assert_eq!((astral[0].start, astral[0].end), (3, 14));
    }

    #[test]
    fn marks_references_to_another_instrument_external() {
        let reference = &find(ACACIA_EXTERNAL)[0];
        assert_eq!(reference.label, "13(d)");
        assert!(reference.external);
        let original = &find(ACACIA_ORIGINAL_AGREEMENT)[0];
        assert_eq!(original.label, "7.4");
        assert!(original.external);
    }

    #[test]
    fn finds_every_member_of_an_explicitly_repeated_list() {
        assert_eq!(
            find(ACACIA_LIST)
                .into_iter()
                .map(|reference| reference.locator)
                .collect::<Vec<_>>(),
            ["sec2.3(b)", "sec6.3(a)", "sec7.1(f)"]
        );
    }

    #[test]
    fn expands_coordinated_lists_without_collapsing_decimals() {
        let text = "sections 150 and 150.1, subsection 160(2) or (3), and sections 170, 171 or 172";
        let found = find(text);
        assert_eq!(
            found
                .iter()
                .map(|reference| reference.locator.as_str())
                .collect::<Vec<_>>(),
            [
                "sec150",
                "sec150.1",
                "sec160(2)",
                "sec160(3)",
                "sec170",
                "sec171",
                "sec172"
            ]
        );
        for reference in found {
            assert_eq!(&text[reference.start..reference.end], reference.raw);
        }
    }

    #[test]
    fn inherits_external_status_across_a_coordinated_list() {
        assert_eq!(
            find("Sections 302 and 906 of the Sarbanes-Oxley Act")
                .into_iter()
                .map(|reference| (reference.locator, reference.external))
                .collect::<Vec<_>>(),
            [("sec302".to_owned(), true), ("sec906".to_owned(), true)]
        );
    }

    #[test]
    fn does_not_expand_an_ambiguous_singleton_comma() {
        assert_eq!(find("Section 5, 2020 was a difficult year").len(), 1);
    }

    #[test]
    fn reads_only_roman_container_numbering() {
        let reference = &find(ACACIA_ROMAN)[0];
        assert_eq!(reference.raw, "Article VI");
        assert_eq!(reference.shape, ProvisionReferenceShape::Roman);
        assert_eq!(reference.locator, "");
        assert_eq!(reference.alias_key, "article vi");
        assert!(find("Section IV of the deed").is_empty());
    }

    #[test]
    fn carries_sub_only_labels_without_normalizing_them() {
        let reference = &find("as described in paragraph (b) above")[0];
        assert_eq!(reference.shape, ProvisionReferenceShape::SubOnly);
        assert_eq!(reference.label, "(b)");
        assert_eq!(reference.locator, "");
        assert_eq!(normalize_numbered_section_locator("8.01(b)"), "sec8.01(b)");
    }

    #[test]
    fn restricts_the_vocabulary_on_request() {
        let text = "Section 5.3 and Schedule 2.1 and paragraph (b)";
        let found = find_provision_references(
            &ScalarText::new(text),
            FindProvisionReferencesOptions {
                words: Some(&["section"]),
                window: None,
            },
        );
        assert_eq!(
            found
                .into_iter()
                .map(|reference| reference.raw)
                .collect::<Vec<_>>(),
            ["Section 5.3"]
        );
    }

    #[test]
    fn accepts_an_empty_lookaround_window() {
        let found = find_provision_references(
            &ScalarText::new("Act Section 5 thereof"),
            FindProvisionReferencesOptions {
                words: None,
                window: Some(0),
            },
        );
        assert!(!found[0].external);
    }

    #[test]
    fn returns_source_order_without_duplicate_starts() {
        let text = format!("{ACACIA_ROMAN} {ACACIA_LIST}");
        let found = find(&text);
        assert!(found.windows(2).all(|pair| pair[0].start < pair[1].start));
    }

    #[test]
    fn classifies_both_flanks_and_external_numbering_literally() {
        for text in [
            "Code Section 59A applies",
            "Treasury Regulation Section 1.482 applies",
            "Exchange Act Section 13(d) applies",
            "Section 1.6011-4(b)(2) applies",
            "Section 262 thereof applies",
        ] {
            assert!(find(text)[0].external, "{text}");
        }
        assert!(!find("Section 8 of this Agreement")[0].external);
        assert!(!find("Sections 7.2 or 7.3 to be satisfied")[0].external);
    }

    #[test]
    fn serializes_the_complete_reference_contract_in_field_order() {
        let serialized = serde_json::to_string(&find("Sections 160(2) or (3)")).unwrap();
        assert_eq!(
            serialized,
            r#"[{"start":0,"end":15,"raw":"Sections 160(2)","word":"section","plural":true,"label":"160(2)","shape":"numeric","locator":"sec160(2)","aliasKey":"section 160(2)","external":false},{"start":19,"end":22,"raw":"(3)","word":"section","plural":false,"label":"160(3)","shape":"numeric","locator":"sec160(3)","aliasKey":"section 160(3)","external":false,"continuationOf":0}]"#
        );
    }
}
