use crate::text::{
    javascript_whitespace, normalize_javascript_whitespace, trim_javascript_whitespace as trim_js,
};
use crate::{
    derive::derive_trusted, utf16_len, CitedAuthority, CoverageState, Derivation, DetectionProfile,
    DocumentInput, DocumentStructure, EngineError, EvidenceKind, Exclusion, MarkupAncestor,
    MarkupElement, NativeClaim, NodeKind, Note, NoteKindV2, NoteReference, ScalarRange, ScalarText,
    Scope, ScopeKind,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

#[cfg(all(test, feature = "document-query"))]
use crate::DocumentKind;

const ORIGIN: &str = "provider-adapter";

#[derive(Clone, Copy)]
enum DocumentProvider {
    A2aj,
    CourtListener,
    Tna,
    GovInfo,
    GovUkEt,
    Hansard,
    Journal,
    LocalPdf,
}

impl DocumentProvider {
    fn as_str(self) -> &'static str {
        match self {
            Self::A2aj => "a2aj",
            Self::CourtListener => "courtlistener",
            Self::Tna => "tna",
            Self::GovInfo => "govinfo",
            Self::GovUkEt => "govuk-et",
            Self::Hansard => "hansard",
            Self::Journal => "journal",
            Self::LocalPdf => "local-pdf",
        }
    }

    fn from_name(value: &str) -> Option<Self> {
        Some(match value {
            "a2aj" => Self::A2aj,
            "courtlistener" => Self::CourtListener,
            "tna" => Self::Tna,
            "govinfo" => Self::GovInfo,
            "govuk-et" => Self::GovUkEt,
            "hansard" => Self::Hansard,
            "journal" => Self::Journal,
            "local-pdf" => Self::LocalPdf,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum MarkupSerialization {
    #[default]
    HtmlSource,
    BrowserHtml,
    XmlSource,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NativeMarkupScope {
    kind: String,
    excerpt_of: Option<String>,
    #[serde(default)]
    markup_serialization: MarkupSerialization,
}

fn default_scope() -> NativeMarkupScope {
    NativeMarkupScope {
        kind: "complete".to_owned(),
        excerpt_of: None,
        markup_serialization: MarkupSerialization::HtmlSource,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NativeMarkupInput {
    provider: String,
    id: String,
    url: Option<String>,
    text: String,
    markup: Option<String>,
    citation: Option<String>,
    #[serde(default)]
    page_citations: Vec<String>,
    #[serde(default)]
    markup_ancestors: Vec<MarkupAncestor>,
    #[serde(default = "default_scope")]
    scope: NativeMarkupScope,
}

/// Exact markup insertions over a retained source rendition. Offsets are UTF-8
/// bytes in the source after HTML escaping, when it is plain text.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CourtListenerMarkupRecipe {
    pub source_is_plain_text: bool,
    pub insertions: Vec<(usize, String)>,
}

fn courtlistener_source(source: &str, plain_text: bool) -> Cow<'_, str> {
    if !plain_text {
        return Cow::Borrowed(source);
    }
    let mut escaped = String::with_capacity(source.len());
    for character in source.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            _ => escaped.push(character),
        }
    }
    Cow::Owned(escaped)
}

/// Find CourtListener's citation/pre wrappers without reserializing source
/// markup. Admit a recipe only when it rebuilds the independently supplied
/// annotated rendition exactly; unfamiliar transformations yield None.
pub fn courtlistener_markup_recipe(
    source: &str,
    annotated: &str,
    source_is_plain_text: bool,
) -> Option<CourtListenerMarkupRecipe> {
    let mut stack = Vec::<(String, bool, bool)>::new();
    let mut spans = Vec::new();
    let mut attributes = Attributes::new();
    let mut at = 0;
    while let Some(relative) = annotated[at..].find('<') {
        at += relative;
        if annotated[at..].starts_with("<!--") {
            at += 4 + annotated[at + 4..].find("-->")? + 3;
            continue;
        }
        let end = at + annotated[at..].find('>')? + 1;
        let raw = &annotated[at..end];
        if let Some((tag, _)) = parse_tag(raw, true) {
            if let Some(index) = stack.iter().rposition(|entry| entry.0 == tag) {
                if stack[index].1 {
                    spans.push(at..end);
                }
                stack.truncate(index);
            }
        } else if let Some((tag, attrs)) = parse_tag(raw, false) {
            if !void_tag(&tag) && !raw[..raw.len() - 1].trim_end().ends_with('/') {
                parse_attributes(attrs, &mut attributes);
                let classes = attribute(&attributes, "class");
                let citation = tag == "span" && contains_ascii_word(classes, "citation", false);
                let remove = citation
                    || (tag == "a" && stack.last().is_some_and(|entry| entry.2))
                    || (source_is_plain_text
                        && tag == "pre"
                        && classes
                            .split_ascii_whitespace()
                            .eq(std::iter::once("inline")));
                if remove {
                    spans.push(at..end);
                }
                stack.push((tag, remove, citation));
            }
        }
        at = end;
    }
    let mut base = String::with_capacity(annotated.len());
    let mut insertions = Vec::with_capacity(spans.len());
    let mut cursor = 0;
    for span in spans {
        base.push_str(annotated.get(cursor..span.start)?);
        insertions.push((base.len(), annotated.get(span.clone())?.to_owned()));
        cursor = span.end;
    }
    base.push_str(&annotated[cursor..]);
    if base != courtlistener_source(source, source_is_plain_text) {
        return None;
    }
    let recipe = CourtListenerMarkupRecipe {
        source_is_plain_text,
        insertions,
    };
    (restore_courtlistener_markup(source, &recipe).ok()?.as_str() == annotated).then_some(recipe)
}

/// Replay retained annotation bytes without HTML parsing or citation matching.
pub fn restore_courtlistener_markup(
    source: &str,
    recipe: &CourtListenerMarkupRecipe,
) -> Result<String, EngineError> {
    let base = courtlistener_source(source, recipe.source_is_plain_text);
    let mut result = String::with_capacity(base.len());
    let mut cursor = 0;
    for (at, insertion) in &recipe.insertions {
        result
            .push_str(base.get(cursor..*at).ok_or_else(|| {
                EngineError::source("invalid CourtListener insertion byte range")
            })?);
        result.push_str(insertion);
        cursor = *at;
    }
    result.push_str(
        base.get(cursor..)
            .ok_or_else(|| EngineError::source("invalid CourtListener source byte range"))?,
    );
    Ok(result)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Paragraph,
    Prose,
    Page,
    Section,
    Heading,
    Footnote,
    List,
    Table,
    Row,
    Cell,
}

impl Kind {
    fn evidence(self) -> EvidenceKind {
        match self {
            Self::Paragraph => EvidenceKind::Paragraph,
            Self::Prose => EvidenceKind::Prose,
            Self::Page => EvidenceKind::Page,
            Self::Section => EvidenceKind::Section,
            Self::Heading => EvidenceKind::Heading,
            Self::Footnote => EvidenceKind::Footnote,
            Self::List => EvidenceKind::List,
            Self::Table => EvidenceKind::Table,
            Self::Row => EvidenceKind::Row,
            Self::Cell => EvidenceKind::Cell,
        }
    }
}

#[derive(Clone, Default)]
struct MarkupMetadata {
    start: Option<usize>,
    attributes: BTreeMap<String, String>,
    note_marker: Option<ScalarRange>,
}

struct PendingBlock {
    metadata: MarkupMetadata,
    tag: String,
    kind: Kind,
    label: String,
    start: usize,
    anchor: Option<String>,
    aliases: Vec<String>,
    parent_label: Option<String>,
    inline: bool,
    page_label: Option<String>,
    citation_index: Option<usize>,
    page_scheme: Option<String>,
}

struct RawBlock {
    metadata: MarkupMetadata,
    kind: Kind,
    label: String,
    start: usize,
    end: usize,
    anchor: Option<String>,
    markup_tag: Option<String>,
    aliases: Vec<String>,
    parent_label: Option<String>,
}

#[derive(Clone)]
struct OpenRange {
    tag: String,
    depth: usize,
    start: usize,
}

struct TextPage {
    metadata: MarkupMetadata,
    start: usize,
    anchor: Option<String>,
    citation_index: Option<usize>,
    page_scheme: Option<String>,
}

struct PageStart {
    metadata: MarkupMetadata,
    label: String,
    start: usize,
    anchor: Option<String>,
    markup_tag: Option<String>,
    aliases: Vec<String>,
    scope: Option<(usize, String)>,
}

fn courtlistener_opinion_page_scope(
    elements: &[MarkupElement],
    element: &MarkupElement,
) -> Option<(usize, String)> {
    let mut ancestor = element.parent;
    while let Some(index) = ancestor {
        let owner = &elements[index];
        if owner.tag == "div"
            && owner.attributes.get("class").is_some_and(|classes| {
                classes
                    .split_ascii_whitespace()
                    .any(|class| class == "co_opinionBlock")
            })
        {
            return (owner.range.end >= element.range.end && owner.range.end > element.range.start)
                .then(|| {
                    (
                        owner.range.end,
                        format!("native-markup-{}", owner.markup_start),
                    )
                });
        }
        ancestor = owner.parent;
    }
    None
}

struct RenderedMarkup {
    text: String,
    blocks: Vec<RawBlock>,
    recovered_body_blocks: Vec<RawBlock>,
    elements: Vec<MarkupElement>,
    matched_role_starts: HashSet<usize>,
    exclusions: Vec<ScalarRange>,
    cited_authorities: Vec<CitedAuthority>,
    note_references: Vec<(String, Option<String>, ScalarRange)>,
    source_hash: String,
    harvard_casebody: bool,
}

fn contains_ascii_word(value: &str, word: &str, insensitive: bool) -> bool {
    let value = value.as_bytes();
    let word = word.as_bytes();
    if word.len() > value.len() {
        return false;
    }
    (0..=value.len().saturating_sub(word.len())).any(|start| {
        let found = &value[start..start + word.len()];
        if !(if insensitive {
            found.eq_ignore_ascii_case(word)
        } else {
            found == word
        }) {
            return false;
        }
        let ascii_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        value
            .get(start.wrapping_sub(1))
            .is_none_or(|byte| !ascii_word(*byte))
            && value
                .get(start + found.len())
                .is_none_or(|byte| !ascii_word(*byte))
    })
}

fn decode_entities(value: &str) -> Cow<'_, str> {
    decode_attribute(value)
}

type Attributes<'a> = Vec<(&'a str, Cow<'a, str>)>;

fn decode_attribute(value: &str) -> Cow<'_, str> {
    if !value.contains('&') {
        return Cow::Borrowed(value);
    }
    static ENTITIES: OnceLock<Regex> = OnceLock::new();
    ENTITIES
        .get_or_init(|| Regex::new(r"(?i)&(?:nbsp|amp|lt|gt|quot|apos|#\d+|#x[0-9a-f]+);").unwrap())
        .replace_all(value, |capture: &regex::Captures<'_>| {
            let body = &capture[0][1..capture[0].len() - 1];
            let character = match body.to_ascii_lowercase().as_str() {
                "nbsp" => Some('\u{a0}'),
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => body
                    .strip_prefix("#x")
                    .or_else(|| body.strip_prefix("#X"))
                    .map(|digits| (digits, 16))
                    .or_else(|| body.strip_prefix('#').map(|digits| (digits, 10)))
                    .and_then(|(digits, radix)| u32::from_str_radix(digits, radix).ok())
                    .and_then(char::from_u32),
            };
            character.map_or_else(|| capture[0].to_owned(), |value| value.to_string())
        })
}

fn parse_attributes<'a>(raw: &'a str, attributes: &mut Attributes<'a>) {
    attributes.clear();
    let mut at = 0;
    while at < raw.len() {
        while at < raw.len() {
            let character = raw[at..].chars().next().unwrap();
            if !javascript_whitespace(character) {
                break;
            }
            at += character.len_utf8();
        }
        if at == raw.len() {
            break;
        }
        let name_start = at;
        while at < raw.len() {
            let character = raw[at..].chars().next().unwrap();
            if javascript_whitespace(character) || matches!(character, '=' | '"' | '\'' | '<' | '>')
            {
                break;
            }
            at += character.len_utf8();
        }
        if at == name_start {
            at += raw[at..].chars().next().unwrap().len_utf8();
            continue;
        }
        let name = &raw[name_start..at];
        while at < raw.len() && raw[at..].chars().next().is_some_and(javascript_whitespace) {
            at += raw[at..].chars().next().unwrap().len_utf8();
        }
        if raw.as_bytes().get(at) != Some(&b'=') {
            attributes.push((name, Cow::Borrowed("")));
            continue;
        }
        at += 1;
        while at < raw.len() && raw[at..].chars().next().is_some_and(javascript_whitespace) {
            at += raw[at..].chars().next().unwrap().len_utf8();
        }
        let Some(first) = raw[at..].chars().next() else {
            attributes.push((name, Cow::Borrowed("")));
            break;
        };
        let (start, end) = if matches!(first, '"' | '\'') {
            at += first.len_utf8();
            let start = at;
            while at < raw.len() && raw[at..].chars().next() != Some(first) {
                at += raw[at..].chars().next().unwrap().len_utf8();
            }
            let end = at;
            if at < raw.len() {
                at += first.len_utf8();
            }
            (start, end)
        } else {
            let start = at;
            while at < raw.len() {
                let character = raw[at..].chars().next().unwrap();
                if javascript_whitespace(character)
                    || matches!(character, '"' | '\'' | '=' | '<' | '>')
                {
                    break;
                }
                at += character.len_utf8();
            }
            (start, at)
        };
        let value = decode_attribute(&raw[start..end]);
        attributes.push((name, value));
    }
}

fn attribute<'a>(attributes: &'a Attributes<'_>, name: &str) -> &'a str {
    attributes
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| trim_js(value))
        .unwrap_or_default()
}

fn clean_section_id(raw: &str) -> String {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    static NAMED: OnceLock<Regex> = OnceLock::new();
    let prefix = PREFIX.get_or_init(|| {
        Regex::new(r"(?i)^(?:section|sec|article|part|chapter|subsection|level|lvl)[_-]*").unwrap()
    });
    let named = NAMED
        .get_or_init(|| Regex::new(r"(?i)__(?:subsection|paragraph|subparagraph)[_-]*").unwrap());
    let without_prefix = prefix.replace(raw, "");
    let value = named.replace_all(&without_prefix, "(");
    let mut result = String::with_capacity(value.len());
    let mut at = 0;
    while at < value.len() {
        let rest = &value[at..];
        let first = rest.chars().next().unwrap();
        if matches!(first, '_' | '-') {
            let after = &rest[first.len_utf8()..];
            let length = after
                .find(|value| matches!(value, '_' | '-'))
                .unwrap_or(after.len());
            let token = &after[..length];
            let admissible = !token.is_empty()
                && (token.bytes().all(|byte| byte.is_ascii_digit())
                    || (token.len() == 1 && token.as_bytes()[0].is_ascii_alphabetic())
                    || token.bytes().all(|byte| b"ivxlcdmIVXLCDM".contains(&byte)));
            if admissible {
                if !result.ends_with('(') {
                    result.push('(');
                }
                result.push_str(token);
                result.push(')');
                at += first.len_utf8() + length;
                continue;
            }
        }
        if first != '(' || !result.ends_with('(') {
            result.push(first);
        }
        at += first.len_utf8();
    }
    let open = result.matches('(').count();
    let close = result.matches(')').count();
    result.extend(std::iter::repeat(')').take(open.saturating_sub(close)));
    result
}

fn page_value(raw: &str) -> String {
    let without_stars = raw.trim_start_matches('*');
    let mut value = if without_stars.len() == raw.len() {
        raw
    } else {
        without_stars.trim_start_matches(javascript_whitespace)
    };
    let rest = value
        .get(..4)
        .filter(|prefix| prefix.eq_ignore_ascii_case("page"))
        .map(|_| &value[4..])
        .or_else(|| {
            value
                .get(..2)
                .filter(|prefix| prefix.eq_ignore_ascii_case("p."))
                .map(|_| &value[2..])
        });
    if let Some(rest) = rest {
        if rest.chars().next().is_some_and(javascript_whitespace) {
            value = rest.trim_start_matches(javascript_whitespace);
        }
    }
    trim_js(value).to_owned()
}

fn page_identity(
    raw: &str,
    attributes: &Attributes<'_>,
    anchor: Option<String>,
    inline: bool,
) -> Option<PendingBlock> {
    let page_label = page_value(raw);
    if !page_label.bytes().any(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let numeric = page_label.len() <= 5 && page_label.bytes().all(|byte| byte.is_ascii_digit());
    let label = if numeric {
        format!("page{}", page_label.parse::<u32>().ok()?)
    } else {
        format!(
            "page{}",
            page_label
                .chars()
                .filter(|value| !javascript_whitespace(*value))
                .collect::<String>()
        )
    };
    let (citation_index, page_scheme) = page_citation_identity(attributes);
    Some(PendingBlock {
        metadata: MarkupMetadata::default(),
        tag: String::new(),
        kind: Kind::Page,
        label,
        start: 0,
        anchor,
        aliases: Vec::new(),
        parent_label: None,
        inline,
        page_label: Some(page_label),
        citation_index,
        page_scheme,
    })
}

fn page_citation_identity(attributes: &Attributes<'_>) -> (Option<usize>, Option<String>) {
    let citation_index = ["citation-index", "data-citation-index"]
        .into_iter()
        .find_map(|name| attribute(attributes, name).parse::<usize>().ok())
        .filter(|value| *value != 0);
    (
        citation_index,
        (!attribute(attributes, "pagescheme").is_empty())
            .then(|| attribute(attributes, "pagescheme").to_owned()),
    )
}

fn numbered_id(id: &str) -> Option<String> {
    static STANDARD: OnceLock<Regex> = OnceLock::new();
    static NOTE: OnceLock<Regex> = OnceLock::new();
    static FTN: OnceLock<Regex> = OnceLock::new();
    STANDARD
        .get_or_init(|| Regex::new(r"(?i)^(?:fn|footnote)[_-]?(\d{1,5})(?:[-_]+\d+)*$").unwrap())
        .captures(id)
        .or_else(|| {
            NOTE.get_or_init(|| {
                Regex::new(r"(?i)^fn_(?:fn|fnote|refnote)(\d{1,5})(?:_\d+)*$").unwrap()
            })
            .captures(id)
        })
        .or_else(|| {
            FTN.get_or_init(|| Regex::new(r"(?i)^ftn(\d{1,5})$").unwrap())
                .captures(id)
        })
        .map(|capture| capture[1].to_owned())
}

fn footnote_identity(raw: &str, id: &str, anchor: Option<String>) -> Option<PendingBlock> {
    static NUMBERED: OnceLock<Regex> = OnceLock::new();
    let marker = trim_js(raw);
    let numbered = NUMBERED
        .get_or_init(|| Regex::new(r"^(?:\[(\d{1,5})\]|\((\d{1,5})\)|(\d{1,5}))$").unwrap())
        .captures(marker)
        .and_then(|capture| {
            (1..=3).find_map(|index| capture.get(index).map(|value| value.as_str().to_owned()))
        });
    if let Some(number) = numbered.or_else(|| marker.is_empty().then(|| numbered_id(id)).flatten())
    {
        let aliases = (marker != number && !marker.is_empty())
            .then(|| vec![marker.to_owned(), format!("footnote {marker}")])
            .unwrap_or_default();
        return Some(PendingBlock {
            metadata: MarkupMetadata::default(),
            tag: String::new(),
            kind: Kind::Footnote,
            label: format!("fn{}", number.parse::<u32>().ok()?),
            start: 0,
            anchor,
            aliases,
            parent_label: None,
            inline: false,
            page_label: None,
            citation_index: None,
            page_scheme: None,
        });
    }
    let compact = id
        .chars()
        .filter(|value| !javascript_whitespace(*value))
        .collect::<String>();
    let compact_lower = compact.to_ascii_lowercase();
    let id_symbol = compact_lower.strip_prefix("fn").is_some_and(|rest| {
        rest.chars()
            .next()
            .is_some_and(|value| matches!(value, '-' | '*' | '†' | '['))
    });
    let symbol = if !marker.is_empty() {
        marker
    } else if id_symbol {
        compact.as_str()
    } else {
        return None;
    };
    Some(PendingBlock {
        metadata: MarkupMetadata::default(),
        tag: String::new(),
        kind: Kind::Footnote,
        label: if symbol
            .as_bytes()
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"fn"))
        {
            symbol.to_owned()
        } else {
            format!("fn{symbol}")
        },
        start: 0,
        anchor,
        aliases: (!marker.is_empty())
            .then(|| vec![marker.to_owned(), format!("footnote {marker}")])
            .unwrap_or_default(),
        parent_label: None,
        inline: false,
        page_label: None,
        citation_index: None,
        page_scheme: None,
    })
}

fn courtlistener_footnote_body(
    provider: DocumentProvider,
    tag: &str,
    attributes: &Attributes<'_>,
) -> bool {
    if !matches!(provider, DocumentProvider::CourtListener) {
        return false;
    }
    static ID: OnceLock<Regex> = OnceLock::new();
    matches!(tag, "footnote" | "footnote_body")
        || (matches!(tag, "aside" | "div" | "li" | "section")
            && (contains_ascii_word(attribute(attributes, "class"), "footnote", true)
                || attribute(attributes, "class")
                    .split_ascii_whitespace()
                    .any(|class| class == "co_footnoteBody")
                || ID
                    .get_or_init(|| Regex::new(r"(?i)^(?:(?:fn|footnote)[_-]|fn\d|ftn\d)").unwrap())
                    .is_match(attribute(attributes, "id"))))
}

fn courtlistener_sibling_note_markers(
    elements: &[MarkupElement],
    text: &str,
) -> HashMap<usize, (String, ScalarRange)> {
    let mut coordinates = None;
    let mut numbers = HashMap::<usize, Option<(String, ScalarRange)>>::new();
    let mut bodies = HashMap::<usize, Option<usize>>::new();
    for element in elements.iter().filter(|element| element.tag == "div") {
        let Some(parent) = element.parent else {
            continue;
        };
        let classes = element.attributes.get("class").map_or("", String::as_str);
        if classes
            .split_ascii_whitespace()
            .any(|class| class == "co_footnoteBody")
        {
            bodies
                .entry(parent)
                .and_modify(|body| *body = None)
                .or_insert(Some(element.markup_start));
        }
        if !classes
            .split_ascii_whitespace()
            .any(|class| class == "co_footnoteNumber")
        {
            continue;
        }
        let coordinates = coordinates.get_or_insert_with(|| ScalarText::new(text));
        let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end) else {
            continue;
        };
        let label = trim_js(raw);
        if label.is_empty() || label.len() > 5 || !label.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
        let start = element.range.start + utf16_len(&raw[..leading]);
        let marker = (
            format!("fn{}", label.parse::<u32>().unwrap()),
            ScalarRange {
                start,
                end: start + utf16_len(label),
            },
        );
        numbers
            .entry(parent)
            .and_modify(|number| *number = None)
            .or_insert(Some(marker));
    }
    bodies
        .into_iter()
        .filter_map(|(parent, body)| Some((body?, numbers.remove(&parent)??)))
        .collect()
}

fn courtlistener_footnote_number(raw: &str) -> Option<PendingBlock> {
    let marker = trim_js(raw);
    let number = marker.strip_prefix("[fn")?.strip_suffix(']')?;
    if number.is_empty() || number.len() > 5 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut identity = footnote_identity(number, "", None)?;
    identity.aliases = vec![marker.to_owned(), format!("footnote {marker}")];
    Some(identity)
}

fn courtlistener_numbered_note_markers(
    elements: &[MarkupElement],
    matched_role_starts: &HashSet<usize>,
    text: &str,
) -> HashMap<usize, (String, Vec<String>, ScalarRange)> {
    if !elements
        .iter()
        .any(|element| element.tag == "footnote_number")
    {
        return HashMap::new();
    }
    let coordinates = ScalarText::new(text);
    let mut markers = HashMap::<usize, Option<(String, Vec<String>, ScalarRange)>>::new();
    for element in elements
        .iter()
        .filter(|element| element.tag == "footnote_number")
    {
        let Some(body) = element
            .parent
            .map(|parent| &elements[parent])
            .filter(|body| {
                body.tag == "footnote_body" && matched_role_starts.contains(&body.markup_start)
            })
        else {
            continue;
        };
        let marker = (|| {
            if !matched_role_starts.contains(&element.markup_start) {
                return None;
            }
            let raw = coordinates.slice_utf16(element.range.start..element.range.end)?;
            let identity = courtlistener_footnote_number(raw)?;
            let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
            let start = element.range.start + utf16_len(&raw[..leading]);
            let range = ScalarRange {
                start,
                end: start + utf16_len(trim_js(raw)),
            };
            if body.range.end < range.end
                || !trim_js(coordinates.slice_utf16(body.range.start..start)?).is_empty()
            {
                return None;
            }
            Some((identity.label, identity.aliases, range))
        })();
        // A merged body with multiple declared numbers supplies no unique marker.
        markers
            .entry(body.markup_start)
            .and_modify(|marker| *marker = None)
            .or_insert(marker);
    }
    markers
        .into_iter()
        .filter_map(|(start, marker)| Some((start, marker?)))
        .collect()
}

fn supplement_courtlistener_declared_note_references(
    structure: &mut DocumentStructure,
    matched_role_starts: &HashSet<usize>,
) {
    let updates = {
        let elements = &structure.markup_elements;
        if structure.notes.is_empty() {
            return;
        }
        let has_class = |element: &MarkupElement, class: &str| {
            element
                .attributes
                .get("class")
                .is_some_and(|classes| classes.split_ascii_whitespace().any(|value| value == class))
        };
        let ldml_reference = |element: &MarkupElement| {
            element.tag == "sup"
                && has_class(element, "ldml-superscript")
                && matched_role_starts.contains(&element.markup_start)
        };
        let has_ldml_references = elements.iter().any(ldml_reference);
        if !has_ldml_references
            && !elements.iter().any(|element| {
                element.tag == "footnotemark"
                    || element.tag == "footnote_reference"
                    || element.tag.eq_ignore_ascii_case("footnotereference")
            })
        {
            return;
        }
        let opinion_owner = |index: usize| {
            let mut parent = elements[index].parent;
            while let Some(index) = parent {
                if elements[index].tag == "opinion" {
                    return Some(index);
                }
                parent = elements[index].parent;
            }
            None
        };
        let mut owners = HashMap::<(usize, &str), Option<usize>>::new();
        for (index, element) in elements.iter().enumerate() {
            if element.tag != "footnote" {
                continue;
            }
            let Some(opinion) = opinion_owner(index) else {
                continue;
            };
            let Some(label) = element.attributes.get("label").map(|label| trim_js(label)) else {
                continue;
            };
            if !label.is_empty() {
                owners
                    .entry((opinion, label))
                    .and_modify(|owner| *owner = None)
                    .or_insert(Some(index));
            }
        }
        let nodes = structure
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Footnote)
            .filter_map(|node| Some((node.markup_start?, node.id.as_str())))
            .collect::<HashMap<_, _>>();
        let notes = structure
            .notes
            .iter()
            .enumerate()
            .filter_map(|(index, note)| note.node_id.as_deref().map(|id| (id, index)))
            .collect::<HashMap<_, _>>();
        let mut identities = HashMap::<&str, Option<usize>>::new();
        let mut direct_targets = HashMap::<&str, Option<usize>>::new();
        if elements.iter().any(|element| {
            element.tag == "footnote_reference"
                || element.tag.eq_ignore_ascii_case("footnotereference")
        }) {
            for (index, element) in elements.iter().enumerate() {
                if let Some(id) = element.attributes.get("id").filter(|id| !id.is_empty()) {
                    identities
                        .entry(id.as_str())
                        .and_modify(|identity| *identity = None)
                        .or_insert(Some(index));
                }
            }
            for node in structure
                .nodes
                .iter()
                .filter(|node| node.source == Derivation::Native && node.kind == NodeKind::Footnote)
            {
                let Some(start) = node.markup_start else {
                    continue;
                };
                let Ok(index) =
                    elements.binary_search_by_key(&start, |element| element.markup_start)
                else {
                    continue;
                };
                let element = &elements[index];
                let Some(id) = element.attributes.get("id") else {
                    continue;
                };
                if identities.get(id.as_str()).copied().flatten() != Some(index)
                    || !node
                        .markup_tag
                        .as_deref()
                        .is_some_and(|tag| tag.eq_ignore_ascii_case(&element.tag))
                {
                    continue;
                }
                let Some(&note) = notes.get(node.id.as_str()) else {
                    continue;
                };
                direct_targets
                    .entry(id.as_str())
                    .and_modify(|target| *target = None)
                    .or_insert(Some(note));
            }
        }
        let coordinates = ScalarText::new(structure.query_text());
        let mut numbered_targets = HashMap::<&str, Option<usize>>::new();
        if elements
            .iter()
            .any(|element| element.tag == "footnote_reference")
        {
            for element in elements
                .iter()
                .filter(|element| element.tag == "footnote_number")
            {
                let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end)
                else {
                    continue;
                };
                if courtlistener_footnote_number(raw).is_none() {
                    continue;
                }
                let label = trim_js(raw);
                let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
                let start = element.range.start + utf16_len(&raw[..leading]);
                let range = ScalarRange {
                    start,
                    end: start + utf16_len(label),
                };
                let target = element
                    .parent
                    .filter(|parent| {
                        elements[*parent].tag == "footnote_body"
                            && matched_role_starts.contains(&elements[*parent].markup_start)
                            && matched_role_starts.contains(&element.markup_start)
                    })
                    .and_then(|parent| nodes.get(&elements[parent].markup_start))
                    .and_then(|node| notes.get(node))
                    .copied()
                    .filter(|note| structure.notes[*note].label_range == range);
                // Repeated declarations, including an unqualified second body,
                // cannot establish a unique whole-field reference relationship.
                numbered_targets
                    .entry(label)
                    .and_modify(|target| *target = None)
                    .or_insert(target);
            }
        }
        let is_ldml = |element: &MarkupElement, class: &str| {
            element.tag == "div" && has_class(element, class)
        };
        let mut decision_owners = Vec::<Option<usize>>::new();
        let mut writing_owners = Vec::<Option<usize>>::new();
        let mut writings = HashMap::<usize, Option<usize>>::new();
        let mut footers = HashMap::<usize, Option<usize>>::new();
        if has_ldml_references {
            for (index, element) in elements.iter().enumerate() {
                let decision = element.parent.and_then(|parent| {
                    if is_ldml(&elements[parent], "ldml-decision") {
                        Some(parent)
                    } else {
                        decision_owners[parent]
                    }
                });
                let writing = element.parent.and_then(|parent| {
                    if is_ldml(&elements[parent], "ldml-opinion") {
                        Some(parent)
                    } else {
                        writing_owners[parent]
                    }
                });
                decision_owners.push(decision);
                writing_owners.push(writing);
                if let Some(decision) = decision {
                    let owners = if is_ldml(element, "ldml-opinion") {
                        Some(&mut writings)
                    } else if is_ldml(element, "ldml-notes") {
                        Some(&mut footers)
                    } else {
                        None
                    };
                    if let Some(owners) = owners {
                        // Even an unmatched or indirect second declaration is ambiguous.
                        owners
                            .entry(decision)
                            .and_modify(|owner| *owner = None)
                            .or_insert(Some(index));
                    }
                }
            }
        }
        let ldml_scope = |decision: usize| {
            let writing = writings.get(&decision).copied().flatten()?;
            let footer = footers.get(&decision).copied().flatten()?;
            (elements[writing].parent == Some(decision)
                && elements[footer].parent == Some(decision)
                && elements[writing].markup_end <= elements[footer].markup_start
                && [decision, writing, footer]
                    .iter()
                    .all(|index| matched_role_starts.contains(&elements[*index].markup_start)))
            .then_some((writing, footer))
        };
        let mut ldml_targets = HashMap::<(usize, &str), Option<usize>>::new();
        if has_ldml_references {
            let native_owners = structure
                .nodes
                .iter()
                .filter(|node| node.source == Derivation::Native && node.kind == NodeKind::Footnote)
                .filter_map(|node| {
                    Some((
                        node.markup_start?,
                        (Kind::Footnote, node.range.start, node.range.end),
                    ))
                })
                .collect::<HashMap<_, _>>();
            for (index, element) in elements.iter().enumerate() {
                if !courtlistener_ldml_note(element) {
                    continue;
                }
                let Some(decision) = decision_owners[index] else {
                    continue;
                };
                let Some((_, marker)) = courtlistener_declared_note_marker(
                    elements,
                    index,
                    &coordinates,
                    &native_owners,
                ) else {
                    continue;
                };
                let Some(label) = coordinates
                    .slice_utf16(marker.start..marker.end)
                    .filter(|label| courtlistener_ldml_bracketed_marker(label))
                else {
                    continue;
                };
                let target = ldml_scope(decision).and_then(|(_, footer)| {
                    let note = nodes
                        .get(&element.markup_start)
                        .and_then(|node| notes.get(node))
                        .copied()?;
                    let body = &structure.notes[note];
                    (element.parent == Some(footer)
                        && writing_owners[index].is_none()
                        && matched_role_starts.contains(&element.markup_start)
                        && native_owners.contains_key(&element.markup_start)
                        && body.label_range == marker
                        && body.body_range.start == marker.end
                        && body.body_range.end == element.range.end
                        && body.references.is_empty()
                        && body.primary_reference.is_none()
                        && elements[index + 1..]
                            .iter()
                            .take_while(|child| child.markup_start < element.markup_end)
                            .any(|child| ldml_reference(child) && child.range == marker))
                    .then_some(note)
                });
                // Count literal labels even when a body cannot supply a qualified Note.
                ldml_targets
                    .entry((decision, label))
                    .and_modify(|target| *target = None)
                    .or_insert(target);
            }
        }
        let mut updates = Vec::new();
        for (index, element) in elements.iter().enumerate() {
            if ldml_reference(element) {
                if ["id", "name", "href", "anchoridref"]
                    .iter()
                    .any(|key| element.attributes.contains_key(*key))
                    || elements
                        .get(index + 1)
                        .is_some_and(|child| child.markup_start < element.markup_end)
                {
                    continue;
                }
                let Some(decision) = decision_owners[index] else {
                    continue;
                };
                let Some((writing, _)) = ldml_scope(decision) else {
                    continue;
                };
                if writing_owners[index] != Some(writing) {
                    continue;
                }
                let mut parent = element.parent;
                let mut blocked = false;
                while let Some(index) = parent {
                    let ancestor = &elements[index];
                    blocked |= matches!(
                        ancestor.tag.as_str(),
                        "blockquote" | "table" | "nav" | "script" | "style" | "a"
                    ) || [
                        "ldml-note",
                        "ldml-notes",
                        "ldml-referencenote",
                        "ldml-referencechain",
                        "ldml-refname",
                        "ldml-cite",
                        "ldml-headmatter",
                        "ldml-header",
                        "ldml-quotation",
                    ]
                    .iter()
                    .any(|class| has_class(ancestor, class));
                    parent = ancestor.parent;
                }
                if blocked {
                    continue;
                }
                let Some(label) = coordinates
                    .slice_utf16(element.range.start..element.range.end)
                    .filter(|label| courtlistener_ldml_bracketed_marker(label))
                else {
                    continue;
                };
                if let Some(note) = ldml_targets.get(&(decision, label)).copied().flatten() {
                    // This is an inferred printed-marker pair, not an explicit source link.
                    updates.push((note, element.range, None));
                }
                continue;
            }
            if element.tag == "footnote_reference"
                || element.tag.eq_ignore_ascii_case("footnotereference")
            {
                let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end)
                else {
                    continue;
                };
                let label = trim_js(raw);
                if label.is_empty() {
                    continue;
                }
                let target = if element.tag == "footnote_reference" {
                    matched_role_starts
                        .contains(&element.markup_start)
                        .then(|| numbered_targets.get(label).copied().flatten())
                        .flatten()
                } else {
                    element
                        .attributes
                        .get("anchoridref")
                        .and_then(|target| direct_targets.get(target.as_str()))
                        .copied()
                        .flatten()
                };
                let Some(note) = target else {
                    continue;
                };
                let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
                let start = element.range.start + utf16_len(&raw[..leading]);
                let anchor = element
                    .attributes
                    .get("id")
                    .filter(|id| identities.get(id.as_str()).copied().flatten() == Some(index))
                    .cloned();
                // IDREF and unique printed-marker relationships retain only
                // this reference's own actual unique ID as a fragment anchor.
                updates.push((
                    note,
                    ScalarRange {
                        start,
                        end: start + utf16_len(label),
                    },
                    anchor,
                ));
                continue;
            }
            if element.tag != "footnotemark" {
                continue;
            }
            let mut parent = element.parent;
            let mut in_note = false;
            while let Some(index) = parent {
                in_note |= elements[index].tag == "footnote";
                parent = elements[index].parent;
            }
            if in_note {
                continue;
            }
            let Some(opinion) = opinion_owner(index) else {
                continue;
            };
            let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end) else {
                continue;
            };
            let label = trim_js(raw);
            let Some(owner) = owners.get(&(opinion, label)).copied().flatten() else {
                continue;
            };
            let Some(note) = nodes
                .get(&elements[owner].markup_start)
                .and_then(|node| notes.get(node))
            else {
                continue;
            };
            let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
            let start = element.range.start + utf16_len(&raw[..leading]);
            updates.push((
                *note,
                ScalarRange {
                    start,
                    end: start + utf16_len(label),
                },
                None,
            ));
        }
        updates
    };
    for (index, range, anchor) in updates {
        let note = &mut structure.notes[index];
        if !note
            .references
            .iter()
            .any(|reference| reference.range == range)
        {
            // IDREF markers retain only their own actual unique ID, if any.
            // Label-only same-opinion relationships supply no source fragment.
            note.references.push(NoteReference {
                range,
                anchor,
                page_indexes: Vec::new(),
                line_ids: Vec::new(),
            });
        }
        if note.primary_reference.is_none() {
            note.primary_reference = note.references.first().map(|reference| reference.range);
        }
    }
}

fn clip_courtlistener_pre_note_bodies(structure: &mut DocumentStructure) {
    let ends = {
        let coordinates = ScalarText::new(structure.query_text());
        let pre = structure
            .markup_elements
            .iter()
            .filter(|element| element.tag == "pre")
            .collect::<Vec<_>>();
        let mut boundaries = Vec::new();
        for element in &pre {
            let Some(raw) = coordinates.slice_utf16(element.range.start..element.range.end) else {
                continue;
            };
            let mut start = element.range.start;
            let mut lines = raw.split_inclusive('\n').peekable();
            while let Some(line) = lines.next() {
                let value = line.trim_matches([' ', '\t', '\r', '\n']);
                let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
                if (1..=3).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    let following = lines
                        .clone()
                        .find(|line| !line.trim_matches([' ', '\t', '\r', '\n']).is_empty());
                    if following.is_some_and(|line| {
                        !line.contains('\u{c}')
                            && line.len() - line.trim_start_matches([' ', '\t']).len() > indent
                            && line.chars().any(char::is_alphabetic)
                    }) {
                        boundaries.push((value.parse::<u32>().unwrap(), start, indent));
                    }
                }
                start += utf16_len(line);
            }
        }
        structure
            .notes
            .iter()
            .filter_map(|note| {
                let node = structure
                    .nodes
                    .iter()
                    .find(|node| note.node_id.as_deref() == Some(node.id.as_str()))?;
                if node.kind != NodeKind::Footnote
                    || node.source != Derivation::Heuristic
                    || note.primary_reference.is_none()
                {
                    return None;
                }
                let label =
                    coordinates.slice_utf16(note.label_range.start..note.label_range.end)?;
                let number = label.parse::<u32>().ok()?;
                let owner = pre.iter().find(|element| {
                    element.range.start <= note.label_range.start
                        && note.label_range.end <= element.range.end
                })?;
                let prefix = coordinates.slice_utf16(owner.range.start..note.label_range.start)?;
                let prefix = prefix.rsplit('\n').next()?;
                if !prefix.bytes().all(|byte| matches!(byte, b' ' | b'\t')) {
                    return None;
                }
                let end = boundaries
                    .iter()
                    .filter(|(next, start, indent)| {
                        *next == number + 1
                            && *indent == prefix.len()
                            && note.body_range.start < *start
                            && *start < note.body_range.end
                    })
                    .map(|(_, start, _)| *start)
                    .min()?;
                Some((note.node_id.clone()?, end))
            })
            .collect::<HashMap<_, _>>()
    };
    // PRE container starts can expose a printed next label even when HTML's
    // opening-LF removal joins it to the preceding rendered line. Bound only an
    // existing corroborated fragment; the next label supplies no new note/ref.
    for node in &mut structure.nodes {
        if let Some(end) = ends.get(&node.id) {
            node.range.end = *end;
        }
    }
    for note in &mut structure.notes {
        if let Some(end) = note.node_id.as_ref().and_then(|id| ends.get(id)) {
            note.body_range.end = *end;
            note.body_ranges.retain_mut(|range| {
                range.end = range.end.min(*end);
                range.start < range.end
            });
        }
    }
}

fn supplement_courtlistener_bookmarked_notes(structure: &mut DocumentStructure) {
    if structure.notes.is_empty() {
        return;
    }
    let (updates, printed_updates) = {
        let elements = &structure.markup_elements;
        let has_class = |element: &MarkupElement, class: &str| {
            element
                .attributes
                .get("class")
                .is_some_and(|classes| classes.split_ascii_whitespace().any(|value| value == class))
        };
        let has_fn_notes = elements
            .iter()
            .any(|element| element.tag == "div" && has_class(element, "fn-footnote"));
        if !elements.iter().any(|element| {
            (element.tag.eq_ignore_ascii_case("a")
                && element
                    .attributes
                    .get("name")
                    .is_some_and(|name| name.starts_with("_ftn")))
                || (element.tag.eq_ignore_ascii_case("sup")
                    && element.attributes.get("id").is_some_and(|id| {
                        id.starts_with("fn") || id.starts_with("op") || id.starts_with("co_fnRef_")
                    }))
                || (element.tag.eq_ignore_ascii_case("span")
                    && has_class(element, "MsoFootnoteReference"))
                || (element.tag.eq_ignore_ascii_case("div")
                    && element
                        .attributes
                        .get("id")
                        .is_some_and(|id| id.starts_with("ftn")))
        }) && !has_fn_notes
        {
            return;
        }
        let reference_spans = elements
            .iter()
            .enumerate()
            .filter(|(_, element)| {
                element.tag.eq_ignore_ascii_case("sup")
                    || (element.tag.eq_ignore_ascii_case("span")
                        && has_class(element, "MsoFootnoteReference"))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let mut identities = HashMap::<&str, Option<usize>>::new();
        for (index, element) in elements.iter().enumerate() {
            for name in [
                element.attributes.get("id"),
                element
                    .tag
                    .eq_ignore_ascii_case("a")
                    .then(|| element.attributes.get("name"))
                    .flatten(),
            ]
            .into_iter()
            .flatten()
            .filter(|name| !name.is_empty())
            {
                identities
                    .entry(name.as_str())
                    .and_modify(|owner| {
                        if *owner != Some(index) {
                            *owner = None;
                        }
                    })
                    .or_insert(Some(index));
            }
        }
        let note_starts = structure
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Footnote)
            .filter_map(|node| node.markup_start)
            .collect::<HashSet<_>>();
        let note_owner = |index: usize| {
            let mut parent = elements[index].parent;
            while let Some(index) = parent {
                let element = &elements[index];
                if note_starts.contains(&element.markup_start) {
                    return Some(element.markup_start);
                }
                parent = element.parent;
            }
            None
        };
        let parent_tag = |index: usize, tag: &str| {
            let mut parent = elements[index].parent;
            while let Some(index) = parent {
                if elements[index].tag.eq_ignore_ascii_case(tag) {
                    return Some(index);
                }
                parent = elements[index].parent;
            }
            None
        };
        let coordinates = ScalarText::new(structure.query_text());
        let printed_range = |index: usize, expected: &str| {
            let element = &elements[index];
            let raw = coordinates.slice_utf16(element.range.start..element.range.end)?;
            let printed = trim_js(raw);
            if printed != expected && printed != format!("[{expected}]") {
                return None;
            }
            let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
            let start = element.range.start + utf16_len(&raw[..leading]);
            Some(ScalarRange {
                start,
                end: start + utf16_len(printed),
            })
        };
        // Source ownership, rather than Word's formatting sections, bounds an
        // anonymous pair. A nested native opinion starts a distinct owner.
        let opinion_starts = structure
            .nodes
            .iter()
            .filter(|node| {
                node.source == Derivation::Native
                    && node.kind == NodeKind::Section
                    && (node.markup_tag.as_deref() == Some("opinion")
                        || node.markup_attributes.get("class").is_some_and(|classes| {
                            classes
                                .split_ascii_whitespace()
                                .any(|class| matches!(class, "co_opinionBlock" | "opinion"))
                        }))
            })
            .filter_map(|node| node.markup_start)
            .collect::<HashSet<_>>();
        let mut document_owners = Vec::with_capacity(elements.len());
        for (index, element) in elements.iter().enumerate() {
            document_owners.push(if opinion_starts.contains(&element.markup_start) {
                index
            } else {
                element
                    .parent
                    .map_or(index, |parent| document_owners[parent])
            });
        }
        let anonymous_markers = reference_spans
            .iter()
            .copied()
            .filter(|&index| {
                let element = &elements[index];
                let superscript = element.tag.eq_ignore_ascii_case("sup");
                if !(superscript
                    || (element.tag.eq_ignore_ascii_case("span")
                        && has_class(element, "MsoFootnoteReference")))
                    || parent_tag(index, "a").is_some()
                {
                    return false;
                }
                let mut ancestor = element.parent;
                while let Some(index) = ancestor {
                    if has_class(&elements[index], "MsoFootnoteReference")
                        || (superscript && elements[index].tag.eq_ignore_ascii_case("sup"))
                    {
                        return false;
                    }
                    ancestor = elements[index].parent;
                }
                std::iter::once(element)
                    .chain(
                        elements[index + 1..]
                            .iter()
                            .take_while(|child| child.markup_start < element.markup_end),
                    )
                    .enumerate()
                    .all(|(position, child)| {
                        !child.tag.eq_ignore_ascii_case("a")
                            && !["id", "href", "name"]
                                .iter()
                                .any(|key| child.attributes.contains_key(*key))
                            && (!superscript
                                || position == 0
                                || (!child.tag.eq_ignore_ascii_case("sup")
                                    && !has_class(child, "MsoFootnoteReference")))
                    })
            })
            .collect::<Vec<_>>();
        let mut native_body_counts = HashMap::<(usize, &str), usize>::new();
        for node in structure
            .nodes
            .iter()
            .filter(|node| node.source == Derivation::Native && node.kind == NodeKind::Footnote)
        {
            if let Some((index, label)) = node.markup_start.and_then(|start| {
                Some((
                    elements
                        .binary_search_by_key(&start, |element| element.markup_start)
                        .ok()?,
                    node.label.as_deref()?,
                ))
            }) {
                *native_body_counts
                    .entry((document_owners[index], label))
                    .or_default() += 1;
            }
        }
        let mut updates = Vec::new();
        let mut printed_updates = Vec::new();
        let sibling_markers = courtlistener_sibling_note_markers(elements, structure.query_text());
        let note_nodes = structure
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Footnote)
            .map(|node| (node.id.as_str(), node))
            .collect::<HashMap<_, _>>();
        let fn_writing_starts = if has_fn_notes {
            structure
                .nodes
                .iter()
                .filter(|node| {
                    node.source == Derivation::Native
                        && node.kind == NodeKind::Section
                        && (node
                            .markup_start
                            .is_some_and(|start| opinion_starts.contains(&start))
                            || matches!(
                                node.markup_tag.as_deref(),
                                Some("opinion_text" | "concurrence_text" | "dissent_text")
                            )
                            || node.markup_attributes.get("class").is_some_and(|classes| {
                                classes
                                    .split_ascii_whitespace()
                                    .any(|class| matches!(class, "ldml-opinion" | "de-writing"))
                            }))
                })
                .filter_map(|node| node.markup_start)
                .collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        let mut fn_writings = Vec::new();
        if has_fn_notes {
            for (index, element) in elements.iter().enumerate() {
                fn_writings.push(if fn_writing_starts.contains(&element.markup_start) {
                    Some(index)
                } else {
                    element.parent.and_then(|parent| fn_writings[parent])
                });
            }
        }
        let numeric_marker = |index: usize| {
            let element = &elements[index];
            if element.tag != "sup" {
                return None;
            }
            let raw = coordinates.slice_utf16(element.range.start..element.range.end)?;
            let printed = trim_js(raw);
            if printed.is_empty()
                || printed.len() > 5
                || !printed.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            Some((
                footnote_identity(printed, "", None)?,
                printed_range(index, printed)?,
                printed,
            ))
        };
        let fn_nodes = note_nodes
            .values()
            .filter(|node| {
                has_fn_notes
                    && node.source == Derivation::Native
                    && node.markup_tag.as_deref() == Some("div")
                    && node.markup_attributes.get("class").is_some_and(|classes| {
                        classes
                            .split_ascii_whitespace()
                            .any(|class| class == "fn-footnote")
                    })
            })
            .filter_map(|node| Some((node.markup_start?, *node)))
            .collect::<HashMap<_, _>>();
        let mut fn_markers = HashMap::new();
        let fn_marker_sources = if has_fn_notes {
            anonymous_markers.as_slice()
        } else {
            &[]
        };
        for index in fn_marker_sources.iter().copied() {
            let Some(owner) = note_owner(index) else {
                continue;
            };
            let Some(node) = fn_nodes.get(&owner) else {
                continue;
            };
            let Some((identity, range, printed)) = numeric_marker(index) else {
                continue;
            };
            if !coordinates
                .slice_utf16(node.range.start..range.start)
                .is_some_and(|prefix| trim_js(prefix).is_empty())
                || (node.marker_range.is_some() && node.marker_range != Some(range))
                || ["data-label", "label", "n"].iter().any(|key| {
                    node.markup_attributes
                        .get(*key)
                        .filter(|raw| !trim_js(raw).is_empty())
                        .is_some_and(|raw| {
                            footnote_identity(raw, "", None)
                                .is_none_or(|declared| declared.label != identity.label)
                        })
                })
            {
                continue;
            }
            fn_markers
                .entry(owner)
                .and_modify(|marker| *marker = None)
                .or_insert(Some((identity, range, printed)));
        }
        let mut fn_body_counts = HashMap::new();
        for node in note_nodes
            .values()
            .filter(|node| has_fn_notes && node.source == Derivation::Native)
        {
            if let Some((index, label)) = node.markup_start.and_then(|start| {
                Some((
                    elements
                        .binary_search_by_key(&start, |element| element.markup_start)
                        .ok()?,
                    fn_markers
                        .get(&start)
                        .and_then(Option::as_ref)
                        .map_or_else(
                            || node.label.as_deref(),
                            |(identity, _, _)| Some(identity.label.as_str()),
                        )?,
                ))
            }) {
                *fn_body_counts
                    .entry((fn_writings[index], label))
                    .or_insert(0usize) += 1;
            }
        }
        for (note_index, note) in structure.notes.iter().enumerate() {
            let Some(&node) = note.node_id.as_deref().and_then(|id| note_nodes.get(id)) else {
                continue;
            };
            if let Some((identity, marker, printed)) = node
                .markup_start
                .and_then(|start| fn_markers.get(&start))
                .and_then(Option::as_ref)
            {
                let Some(body_index) = node.markup_start.and_then(|start| {
                    elements
                        .binary_search_by_key(&start, |element| element.markup_start)
                        .ok()
                }) else {
                    continue;
                };
                let label = identity.label.as_str();
                if note.label_range.start != note.label_range.end && note.label_range != *marker {
                    continue;
                }
                let mut references = Vec::new();
                if note.references.is_empty()
                    && note.primary_reference.is_none()
                    && fn_body_counts.get(&(fn_writings[body_index], label)) == Some(&1)
                {
                    let mut container = elements[body_index].parent;
                    while container
                        .is_some_and(|index| !has_class(&elements[index], "fn-container"))
                    {
                        container = elements[container.unwrap()].parent;
                    }
                    if let Some(container) = container {
                        for index in anonymous_markers.iter().copied() {
                            if elements[index].markup_end > elements[container].markup_start
                                || note_owner(index).is_some()
                                || fn_writings[index] != fn_writings[body_index]
                            {
                                continue;
                            }
                            // Without a supplied writing, only its actual enclosing
                            // source container corroborates an anonymous reference.
                            if fn_writings[body_index].is_none() {
                                let Some(parent) = elements[container].parent else {
                                    continue;
                                };
                                let mut ancestor = elements[index].parent;
                                while ancestor.is_some_and(|index| index != parent) {
                                    ancestor = elements[ancestor.unwrap()].parent;
                                }
                                if ancestor != Some(parent) {
                                    continue;
                                }
                            }
                            if let Some((_, range, _)) = numeric_marker(index)
                                .filter(|(reference, _, _)| reference.label == label)
                            {
                                references.push(NoteReference {
                                    range,
                                    anchor: None,
                                    page_indexes: Vec::new(),
                                    line_ids: Vec::new(),
                                });
                            }
                        }
                    }
                }
                printed_updates.push((
                    note_index,
                    *marker,
                    label.to_owned(),
                    (*printed).to_owned(),
                    references,
                ));
                continue;
            }
            if let Some((label, range)) = node
                .markup_start
                .and_then(|start| sibling_markers.get(&start))
            {
                if node.label.as_ref() != Some(label)
                    || note.label_range != *range
                    || !note.references.is_empty()
                    || note.primary_reference.is_some()
                {
                    continue;
                }
                let Some(body_index) = elements
                    .binary_search_by_key(&node.markup_start.unwrap(), |element| {
                        element.markup_start
                    })
                    .ok()
                else {
                    continue;
                };
                let Some(parent) = elements[body_index].parent else {
                    continue;
                };
                let number = label.strip_prefix("fn").unwrap();
                let mut markers = elements.iter().enumerate().filter_map(|(index, element)| {
                    let number_index = element.parent?;
                    let number_element = &elements[number_index];
                    let id = element.attributes.get("id")?;
                    let opaque = id.strip_prefix("co_footnote_")?;
                    (element.tag == "span"
                        && !opaque.is_empty()
                        && number_element.tag == "div"
                        && has_class(number_element, "co_footnoteNumber")
                        && number_element.parent == Some(parent)
                        && identities.get(id.as_str()).copied().flatten() == Some(index)
                        && printed_range(index, number) == Some(*range))
                    .then_some(opaque)
                });
                let Some(opaque) = markers.next() else {
                    continue;
                };
                if markers.next().is_some() {
                    continue;
                }
                let prefix = format!("co_fnRef_{opaque}_ID");
                let mut references = reference_spans.iter().filter_map(|&index| {
                    let reference = &elements[index];
                    let id = reference.attributes.get("id")?;
                    let suffix = id.strip_prefix(&prefix)?;
                    let range = printed_range(index, number)?;
                    (reference.tag == "sup"
                        && !suffix.is_empty()
                        && identities.get(id.as_str()).copied().flatten() == Some(index)
                        && note_owner(index).is_none()
                        && reference.markup_end <= node.markup_start?)
                        .then_some((range, Some(id.clone())))
                });
                let reference = references.next();
                if references.next().is_none() {
                    if let Some(reference) = reference {
                        // The literal opaque identity links the superscript to
                        // its sibling label, without inventing an href or body ID.
                        updates.push((note_index, None, Some(reference)));
                    }
                }
                continue;
            }
            if note.label_range.start != note.label_range.end {
                continue;
            }
            if node.anchor.is_none()
                && (node.markup_tag.as_deref() == Some("footnote_body")
                    || (node.markup_tag.as_deref() == Some("div")
                        && node.markup_attributes.get("class").is_some_and(|classes| {
                            classes
                                .split_ascii_whitespace()
                                .any(|class| class == "footnote")
                        })))
            {
                let Some(label_id) = node.label.as_deref() else {
                    continue;
                };
                let Some(number) = label_id.strip_prefix("fn").filter(|number| {
                    *number == "*"
                        || (!number.is_empty()
                            && number.len() <= 5
                            && number.bytes().all(|byte| byte.is_ascii_digit()))
                }) else {
                    continue;
                };
                let mut markers = elements.iter().enumerate().filter_map(|(index, element)| {
                    let id = element.attributes.get("id")?;
                    let scoped_label = id
                        .strip_prefix("op")
                        .and_then(|value| value.split_once('-'));
                    (element.tag == "sup"
                        && (id == label_id
                            || scoped_label.is_some_and(|(opinion, label)| {
                                node.markup_tag.as_deref() == Some("div")
                                    && !opinion.is_empty()
                                    && opinion.bytes().all(|byte| byte.is_ascii_digit())
                                    && label == label_id
                            }))
                        && identities.get(id.as_str()).copied().flatten() == Some(index)
                        && note_owner(index) == node.markup_start
                        && printed_range(index, number)
                            .is_some_and(|range| range.start == note.body_range.start))
                    .then_some(index)
                });
                let Some(marker_index) = markers.next() else {
                    continue;
                };
                if markers.next().is_some() {
                    continue;
                }
                let marker = &elements[marker_index];
                let Some(label_range) = printed_range(marker_index, number) else {
                    continue;
                };
                if !marker.tag.eq_ignore_ascii_case("sup")
                    || node.markup_start.is_none()
                    || note_owner(marker_index) != node.markup_start
                    || label_range.start != note.body_range.start
                    || !coordinates
                        .slice_utf16(label_range.end..note.body_range.end)
                        .is_some_and(|body| body.chars().any(char::is_alphabetic))
                {
                    continue;
                }
                updates.push((note_index, Some(label_range), None));
                if !note.references.is_empty() || note.primary_reference.is_some() {
                    continue;
                }
                let forward_href = format!("#{}", marker.attributes["id"]);
                if elements
                    .iter()
                    .filter(|element| {
                        element.tag.eq_ignore_ascii_case("a")
                            && element.attributes.get("href") == Some(&forward_href)
                    })
                    .count()
                    != 1
                {
                    continue;
                }
                let mut pairs = elements.iter().enumerate().filter_map(|(index, backlink)| {
                    if !backlink.tag.eq_ignore_ascii_case("a")
                        || backlink.parent != Some(marker_index)
                        || printed_range(index, number) != Some(label_range)
                    {
                        return None;
                    }
                    let reference_id = backlink.attributes.get("href")?.strip_prefix('#')?;
                    let reference_index = identities.get(reference_id).copied().flatten()?;
                    let reference = &elements[reference_index];
                    let range = printed_range(reference_index, number)?;
                    if !reference.tag.eq_ignore_ascii_case("sup")
                        || note_owner(reference_index).is_some()
                        || reference.markup_end > node.markup_start?
                        || elements
                            .iter()
                            .enumerate()
                            .filter(|(index, element)| {
                                element.tag.eq_ignore_ascii_case("a")
                                    && element.parent == Some(reference_index)
                                    && element.attributes.get("href") == Some(&forward_href)
                                    && printed_range(*index, number) == Some(range)
                            })
                            .count()
                            != 1
                    {
                        return None;
                    }
                    Some((range, Some(reference_id.to_owned())))
                });
                let pair = pairs.next();
                if pairs.next().is_none() {
                    // IDs belong to the actual superscripts, not the idless
                    // note container. Preserve that source ownership while
                    // adding the reciprocal semantic reference.
                    updates.last_mut().unwrap().2 = pair;
                }
                continue;
            }
            let Some(body_id) = node.anchor.as_deref() else {
                continue;
            };
            let Some(number) = body_id.strip_prefix("ftn").filter(|number| {
                !number.is_empty()
                    && number.len() <= 5
                    && number.bytes().all(|byte| byte.is_ascii_digit())
            }) else {
                continue;
            };
            let Some(owner_index) = identities.get(body_id).copied().flatten() else {
                continue;
            };
            let owner = &elements[owner_index];
            if !owner.tag.eq_ignore_ascii_case("div")
                || node.markup_start != Some(owner.markup_start)
            {
                continue;
            }
            let body_name = format!("_{body_id}");
            let reference_name = format!("_ftnref{number}");
            let owner_scope = document_owners[owner_index];
            if node.source == Derivation::Native
                && node
                    .label
                    .as_deref()
                    .and_then(|label| label.strip_prefix("fn"))
                    == Some(number)
                && owner.attributes.get("id").map(String::as_str) == Some(body_id)
                && !identities.contains_key(body_name.as_str())
                && !identities.contains_key(reference_name.as_str())
                && node
                    .label
                    .as_deref()
                    .is_some_and(|label| native_body_counts.get(&(owner_scope, label)) == Some(&1))
            {
                let printed = format!("[{number}]");
                let marker_range = |index| {
                    let range = printed_range(index, number)?;
                    (coordinates.slice_utf16(range.start..range.end) == Some(printed.as_str()))
                        .then_some(range)
                };
                let mut labels = anonymous_markers.iter().filter_map(|&index| {
                    let range = printed_range(index, number)?;
                    let printed_label = if elements[index].tag.eq_ignore_ascii_case("sup") {
                        number
                    } else {
                        printed.as_str()
                    };
                    (coordinates.slice_utf16(range.start..range.end) == Some(printed_label)
                        && note_owner(index) == Some(owner.markup_start)
                        && document_owners[index] == owner_scope
                        && range.start == note.body_range.start)
                        .then_some(range)
                });
                if let Some(label_range) = labels.next().filter(|range| {
                    coordinates
                        .slice_utf16(range.end..note.body_range.end)
                        .is_some_and(|body| body.chars().any(char::is_alphabetic))
                }) {
                    if labels.next().is_some() {
                        continue;
                    }
                    // A printed label is independent of whether the
                    // corresponding reference is absent or already known.
                    updates.push((note_index, Some(label_range), None));
                    if !note.references.is_empty() || note.primary_reference.is_some() {
                        continue;
                    }
                    let mut references = anonymous_markers.iter().filter_map(|&index| {
                        if !elements[index].tag.eq_ignore_ascii_case("span")
                            || !has_class(&elements[index], "MsoFootnoteReference")
                        {
                            return None;
                        }
                        let range = marker_range(index)?;
                        (note_owner(index).is_none() && document_owners[index] == owner_scope)
                            .then_some((index, range))
                    });
                    if let Some((index, range)) = references.next() {
                        if references.next().is_none()
                            && elements[index].markup_end <= owner.markup_start
                        {
                            // The semantic pair has no source fragment.
                            updates.last_mut().unwrap().2 = Some((range, None));
                        }
                    }
                    continue;
                }
            }
            let Some(body_bookmark_index) = identities.get(body_name.as_str()).copied().flatten()
            else {
                continue;
            };
            let body_bookmark = &elements[body_bookmark_index];
            if !body_bookmark.tag.eq_ignore_ascii_case("a")
                || body_bookmark.range.start != body_bookmark.range.end
                || body_bookmark.attributes.contains_key("href")
                || body_bookmark.attributes.get("name") != Some(&body_name)
                || note_owner(body_bookmark_index) != Some(owner.markup_start)
            {
                continue;
            }
            let Some(paragraph) = body_bookmark.parent else {
                continue;
            };
            if !elements[paragraph].tag.eq_ignore_ascii_case("p") {
                continue;
            }
            let printed = number;
            let mut labels = elements.iter().enumerate().filter_map(|(index, element)| {
                if !element.tag.eq_ignore_ascii_case("a")
                    || element.parent != Some(paragraph)
                    || note_owner(index) != Some(owner.markup_start)
                {
                    return None;
                }
                let mut superscripts =
                    elements.iter().enumerate().filter_map(|(sup_index, sup)| {
                        (sup.tag.eq_ignore_ascii_case("sup")
                            && parent_tag(sup_index, "a") == Some(index))
                        .then(|| printed_range(sup_index, printed))
                        .flatten()
                        .filter(|range| range.start == note.body_range.start)
                    });
                let superscript = superscripts.next();
                if superscripts.next().is_some() {
                    return None;
                }
                let range = superscript.or_else(|| {
                    has_class(&elements[paragraph], "MsoFootnoteText")
                        .then(|| printed_range(index, printed))
                        .flatten()
                })?;
                Some((index, element, range))
            });
            let Some((label_index, label, label_range)) = labels.next() else {
                continue;
            };
            if labels.next().is_some() {
                continue;
            }
            if body_bookmark.markup_end > label.markup_start
                || body_bookmark.range.start != label_range.start
                || label_range.start != note.body_range.start
                || !coordinates
                    .slice_utf16(label_range.end..note.body_range.end)
                    .is_some_and(|body| body.chars().any(char::is_alphabetic))
            {
                continue;
            }
            // The leading declared label is independent of a reference's
            // printed text or href. Keep a conflict without losing the label.
            updates.push((note_index, Some(label_range), None));
            if !note.references.is_empty() || note.primary_reference.is_some() {
                continue;
            }
            let Some(reference_bookmark_index) =
                identities.get(reference_name.as_str()).copied().flatten()
            else {
                continue;
            };
            let reference_bookmark = &elements[reference_bookmark_index];
            if !reference_bookmark.tag.eq_ignore_ascii_case("a")
                || reference_bookmark.range.start != reference_bookmark.range.end
                || reference_bookmark.attributes.contains_key("href")
                || reference_bookmark.attributes.get("name") != Some(&reference_name)
                || note_owner(reference_bookmark_index).is_some()
            {
                continue;
            }
            let Some(reference_paragraph) = parent_tag(reference_bookmark_index, "p") else {
                continue;
            };
            let Some((prefix, target)) = label
                .attributes
                .get("href")
                .and_then(|href| href.rsplit_once('#'))
            else {
                continue;
            };
            if prefix.is_empty() || target != reference_name {
                continue;
            }
            if printed_range(label_index, printed) != Some(label_range) {
                continue;
            }
            let mut matches = reference_spans
                .iter()
                .filter_map(|&span_index| {
                    if note_owner(span_index).is_some() {
                        return None;
                    }
                    let reference_index = parent_tag(span_index, "a")?;
                    let reference = &elements[reference_index];
                    let range = printed_range(span_index, &printed)?;
                    (reference
                        .attributes
                        .get("href")
                        .and_then(|href| href.rsplit_once('#'))
                        == Some((prefix, body_name.as_str()))
                        && printed_range(reference_index, &printed) == Some(range)
                        && reference_bookmark.markup_end <= reference.markup_start
                        && reference.markup_end <= owner.markup_start
                        && reference_bookmark.range.start == range.start
                        && parent_tag(reference_index, "p") == Some(reference_paragraph))
                    .then_some((reference_index, range))
                })
                .collect::<Vec<_>>();
            matches.sort_unstable_by_key(|(index, range)| (*index, range.start, range.end));
            matches.dedup();
            if matches.len() != 1 {
                continue;
            }
            // Reciprocal bookmarks support the semantic pair without claiming
            // that the preserved absolute href is the current source URI.
            updates.last_mut().unwrap().2 = Some((matches[0].1, Some(reference_name)));
        }
        (updates, printed_updates)
    };
    for (index, label_range, reference) in updates {
        let note = &mut structure.notes[index];
        if let Some(label_range) = label_range {
            note.label_range = label_range;
            note.label_ranges = vec![label_range];
            note.body_range.start = label_range.end;
            note.body_ranges.retain_mut(|range| {
                range.start = range.start.max(label_range.end);
                range.start < range.end
            });
        }
        if let Some((reference_range, reference_name)) = reference {
            note.primary_reference = Some(reference_range);
            note.references.push(NoteReference {
                range: reference_range,
                anchor: reference_name,
                page_indexes: Vec::new(),
                line_ids: Vec::new(),
            });
        }
    }
    let mut markers = HashMap::new();
    for (index, marker, label, printed, references) in printed_updates {
        let note = &mut structure.notes[index];
        note.label_range = marker;
        note.label_ranges = vec![marker];
        if let Some(id) = &note.node_id { markers.insert(id.clone(), (marker, label, printed)); }
        if !references.is_empty() {
            note.primary_reference = references.first().map(|reference| reference.range);
            note.references = references;
        }
    }
    // The body is already supplied. Attach its printed marker/reference
    // evidence without moving its range, identity or existing stronger links.
    if !markers.is_empty() {
        for node in &mut structure.nodes {
            if let Some((marker, label, printed)) = markers.get(&node.id) {
                node.marker_range = Some(*marker);
                node.label = Some(label.clone());
                let aliases = node.aliases.get_or_insert_with(Vec::new);
                for alias in [printed.clone(), format!("footnote {printed}")] {
                    if !aliases.contains(&alias) {
                        aliases.push(alias);
                    }
                }
            }
        }
    }
}

fn courtlistener_ldml_bracketed_marker(raw: &str) -> bool {
    raw.strip_prefix('[')
        .and_then(|number| number.strip_suffix(']'))
        .is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn courtlistener_ldml_note(element: &MarkupElement) -> bool {
    element.tag.eq_ignore_ascii_case("div")
        && element.attributes.get("class").is_some_and(|classes| {
            classes
                .split_ascii_whitespace()
                .any(|class| class == "ldml-note")
        })
}

fn courtlistener_declared_note_marker(
    elements: &[MarkupElement],
    index: usize,
    text: &ScalarText,
    owners: &HashMap<usize, (Kind, usize, usize)>,
) -> Option<(PendingBlock, ScalarRange)> {
    let note = &elements[index];
    let superscript = elements[index + 1..]
        .iter()
        .take_while(|element| element.markup_start < note.markup_end)
        .find(|element| {
            if !element.tag.eq_ignore_ascii_case("sup") {
                return false;
            }
            let mut parent = element.parent;
            while let Some(ancestor) = parent {
                if ancestor == index {
                    return true;
                }
                let element = &elements[ancestor];
                if courtlistener_ldml_note(element)
                    || owners
                        .get(&element.markup_start)
                        .is_some_and(|(kind, _, _)| *kind == Kind::Footnote)
                {
                    return false;
                }
                parent = element.parent;
            }
            false
        })?;
    let printed = text.slice_utf16(superscript.range.start..superscript.range.end)?;
    let printed = trim_js(printed);
    let (marker, start) = if printed.is_empty() {
        let raw = text.slice_utf16(note.range.start..note.range.end)?;
        let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
        let start = note.range.start + utf16_len(&raw[..leading]);
        if superscript.range.end > start {
            return None;
        }
        let raw = &raw[leading..];
        let digits = raw.bytes().take(6).take_while(u8::is_ascii_digit).count();
        let marker = if (1..=5).contains(&digits)
            && raw[digits..].starts_with('.')
            && raw[digits + 1..]
                .chars()
                .next()
                .is_none_or(|character| !character.is_ascii_digit())
        {
            // LDML also prints N. immediately after an empty SUP, sometimes
            // without separator whitespace. Keep the printed punctuation.
            &raw[..digits + 1]
        } else {
            leading_footnote_marker(raw)?
        };
        (marker, start)
    } else {
        if leading_footnote_marker(printed) != Some(printed) {
            return None;
        }
        let raw = text.slice_utf16(superscript.range.start..superscript.range.end)?;
        let leading = raw.len() - raw.trim_start_matches(javascript_whitespace).len();
        (
            printed,
            superscript.range.start + utf16_len(&raw[..leading]),
        )
    };
    if !trim_js(text.slice_utf16(note.range.start..start)?).is_empty() {
        return None;
    }
    let marker_range = ScalarRange {
        start,
        end: start + utf16_len(marker),
    };
    if elements[index + 1..]
        .iter()
        .take_while(|element| element.markup_start < note.markup_end)
        .any(|element| {
            (courtlistener_ldml_note(element)
                || owners
                    .get(&element.markup_start)
                    .is_some_and(|(kind, _, _)| *kind == Kind::Footnote))
                && element.range.start < marker_range.end
                && marker_range.start < element.range.end
        })
    {
        return None;
    }
    let mut identity = footnote_identity(marker.strip_suffix('.').unwrap_or(marker), "", None)?;
    if marker.ends_with('.') {
        identity.aliases = vec![marker.to_owned(), format!("footnote {marker}")];
    }
    Some((identity, marker_range))
}

fn courtlistener_declared_blocks(
    elements: &[MarkupElement],
    existing: &[RawBlock],
    recovered_bodies: &[RawBlock],
    matched_role_starts: &HashSet<usize>,
    text: &str,
) -> Vec<RawBlock> {
    let declared_role = |element: &MarkupElement| {
        let class = |name: &str| {
            element
                .attributes
                .get("class")
                .is_some_and(|classes| classes.split_ascii_whitespace().any(|value| value == name))
        };
        if courtlistener_ldml_note(element) && matched_role_starts.contains(&element.markup_start) {
            Some(Kind::Footnote)
        } else if (element.tag.eq_ignore_ascii_case("div")
            && (class("ldml-opinion") || class("ldml-section")))
            || (element.tag.eq_ignore_ascii_case("span") && class("ldml-appendix"))
            || (element.tag.eq_ignore_ascii_case("div")
                && class("de-writing")
                && matched_role_starts.contains(&element.markup_start))
            || (element.tag.eq_ignore_ascii_case("summary")
                && matched_role_starts.contains(&element.markup_start)
                && element.range.start < element.range.end
                && element
                    .parent
                    .is_some_and(|parent| elements[parent].tag.eq_ignore_ascii_case("opinion")))
            || (matches!(
                element.tag.as_str(),
                "opinion_text"
                    | "concurrence_text"
                    | "dissent_text"
                    | "appendix"
                    | "correction"
                    | "syllabus"
            ) && matched_role_starts.contains(&element.markup_start))
        {
            Some(Kind::Section)
        } else if (element.tag.eq_ignore_ascii_case("section") && class("ldml-heading"))
            || (element.tag == "p"
                && element
                    .attributes
                    .get("data-type")
                    .is_some_and(|role| role == "header")
                && matched_role_starts.contains(&element.markup_start))
        {
            Some(Kind::Heading)
        } else {
            None
        }
    };
    if !elements
        .iter()
        .any(|element| element.tag.eq_ignore_ascii_case("dl") || declared_role(element).is_some())
    {
        return Vec::new();
    }
    let note_text = elements
        .iter()
        .any(|element| declared_role(element) == Some(Kind::Footnote))
        .then(|| ScalarText::new(text));
    let mut owners = existing
        .iter()
        .chain(recovered_bodies)
        .filter_map(|block| Some((block.metadata.start?, (block.kind, block.start, block.end))))
        .collect::<HashMap<_, _>>();
    let mut recovered = Vec::new();
    for (index, element) in elements.iter().enumerate() {
        let list = element.tag.eq_ignore_ascii_case("dl");
        let item = (element.tag.eq_ignore_ascii_case("dt")
            || element.tag.eq_ignore_ascii_case("dd"))
            && element.parent.is_some_and(|parent| {
                let owner = &elements[parent];
                owner.tag.eq_ignore_ascii_case("dl")
                    && owners
                        .get(&owner.markup_start)
                        .is_some_and(|&(kind, _, _)| kind == Kind::List)
            });
        let kind = if let Some(kind) = declared_role(element) {
            kind
        } else if list || item {
            Kind::List
        } else {
            continue;
        };
        if owners.contains_key(&element.markup_start) {
            continue;
        }
        let mut parent = element.parent;
        let mut parent_label = None;
        while let Some(index) = parent {
            let ancestor = &elements[index];
            if owners
                .get(&ancestor.markup_start)
                .is_some_and(|&(kind, start, end)| {
                    kind != Kind::Page && start <= element.range.start && end >= element.range.end
                })
            {
                parent_label = Some(format!("native-markup-{}", ancestor.markup_start));
                break;
            }
            parent = ancestor.parent;
        }
        let attributes = element
            .attributes
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let anchor = attributes.get("id").filter(|id| !id.is_empty()).cloned();
        let note = (kind == Kind::Footnote)
            .then(|| {
                courtlistener_declared_note_marker(elements, index, note_text.as_ref()?, &owners)
            })
            .flatten();
        let note_marker = note.as_ref().map(|(_, range)| *range);
        let (label, aliases) = note.map_or_else(
            || {
                (
                    if kind == Kind::Footnote {
                        String::new()
                    } else {
                        anchor.clone().unwrap_or_default()
                    },
                    Vec::new(),
                )
            },
            |(identity, _)| (identity.label, identity.aliases),
        );
        recovered.push(RawBlock {
            metadata: MarkupMetadata {
                start: Some(element.markup_start),
                attributes,
                note_marker,
            },
            kind,
            label,
            start: note_marker.map_or(element.range.start, |marker| marker.end),
            end: element.range.end,
            anchor,
            markup_tag: Some(element.tag.to_ascii_lowercase()),
            aliases,
            parent_label,
        });
        owners.insert(
            element.markup_start,
            (kind, element.range.start, element.range.end),
        );
    }
    recovered
}

fn courtlistener_printed_note_blocks(
    markup: &str,
    elements: &[MarkupElement],
    text: &ScalarText<'_>,
    existing: &[RawBlock],
) -> Vec<(RawBlock, Vec<NoteReference>)> {
    if !elements.iter().any(|element| {
        element.tag == "h2"
            && text
                .slice_utf16(element.range.start..element.range.end)
                .map(trim_js)
                == Some("NOTES")
    }) {
        return Vec::new();
    }
    let existing_notes = existing
        .iter()
        .filter(|block| block.kind == Kind::Footnote)
        .map(|block| ScalarRange {
            start: block.start,
            end: block.end,
        })
        .collect::<Vec<_>>();
    let matched = |element: &MarkupElement| {
        markup
            .get(element.markup_start..element.markup_end)
            .is_some_and(|raw| {
                raw.rfind("</")
                    .and_then(|end| parse_tag(&raw[end..], true))
                    .is_some_and(|(tag, _)| tag == element.tag)
            })
    };
    let mut writings = Vec::with_capacity(elements.len());
    for (index, element) in elements.iter().enumerate() {
        let writing = matches!(
            element.tag.as_str(),
            "opinion" | "opinion_text" | "concurrence_text" | "dissent_text"
        ) || (matches!(element.tag.as_str(), "article" | "section" | "div")
            && element.attributes.get("class").is_some_and(|classes| {
                classes.split_ascii_whitespace().any(|class| {
                    class == "opinion"
                        || (element.tag == "div"
                            && matches!(class, "ldml-opinion" | "de-writing" | "co_opinionBlock"))
                })
            }));
        writings.push(if writing {
            Some(index)
        } else {
            element.parent.and_then(|parent| writings[parent])
        });
    }
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| Regex::new(r"^(\[(?:[0-9]{1,5}|\*{1,2})\])(?:\s|$)").unwrap());
    let printed = |element: &MarkupElement| {
        let raw = text.slice_utf16(element.range.start..element.range.end)?;
        let value = raw.trim_start_matches(javascript_whitespace);
        let marker = label.captures(value)?.get(1)?;
        let start = element.range.start + utf16_len(&raw[..raw.len() - value.len()]);
        Some((
            marker.as_str(),
            ScalarRange {
                start,
                end: start + utf16_len(marker.as_str()),
            },
        ))
    };
    let mut result = Vec::new();
    let mut previous_ends = HashMap::new();
    for (heading_index, heading) in elements.iter().enumerate().filter(|(_, element)| {
        element.tag == "h2"
            && matched(element)
            && text
                .slice_utf16(element.range.start..element.range.end)
                .map(trim_js)
                == Some("NOTES")
    }) {
        let Some(parent) = heading.parent.filter(|&parent| {
            matches!(elements[parent].tag.as_str(), "div" | "pre") && matched(&elements[parent])
        }) else {
            continue;
        };
        let writing = writings[heading_index];
        let boundary = elements[heading_index + 1..]
            .iter()
            .enumerate()
            .take_while(|(_, element)| element.markup_start < elements[parent].markup_end)
            .find(|(index, element)| {
                if element.parent == Some(parent) && matches!(element.tag.as_str(), "h1" | "h2") {
                    return true;
                }
                let index = heading_index + 1 + *index;
                if writings[index] != Some(index) || writings[index] == writing {
                    return false;
                }
                // An embedded quoted writing is continuation evidence, not a
                // new sibling writing after the notes region.
                let mut ancestor = element.parent;
                while let Some(index) = ancestor {
                    if index == parent {
                        return true;
                    }
                    if matches!(elements[index].tag.as_str(), "p" | "blockquote" | "table") {
                        return false;
                    }
                    ancestor = elements[index].parent;
                }
                false
            })
            .map(|(_, element)| element);
        let source_end =
            boundary.map_or(elements[parent].markup_end, |element| element.markup_start);
        let rendered_end =
            boundary.map_or(elements[parent].range.end, |element| element.range.start);
        let lower_bound = previous_ends
            .insert((parent, writing), source_end)
            .unwrap_or(elements[parent].markup_start);
        let bodies = elements[heading_index + 1..]
            .iter()
            .enumerate()
            .take_while(|(_, element)| element.markup_start < source_end)
            .filter(|(index, element)| {
                element.tag == "p"
                    && element.parent == Some(parent)
                    && writings[heading_index + 1 + *index] == writing
                    && matched(element)
            })
            .filter_map(|(_, element)| {
                printed(element).map(|(label, marker)| (element, label, marker))
            })
            .collect::<Vec<_>>();
        if bodies.is_empty() {
            continue;
        }
        let mut references = HashMap::<&str, Vec<NoteReference>>::new();
        for (index, element) in
            elements[..heading_index]
                .iter()
                .enumerate()
                .filter(|(_, element)| {
                    element.tag == "sup" && element.markup_start >= lower_bound && matched(element)
                })
        {
            if writings[index] != writing {
                continue;
            }
            let mut ancestor = element.parent;
            while let Some(index) = ancestor {
                if index == parent {
                    break;
                }
                ancestor = elements[index].parent;
            }
            if ancestor != Some(parent) {
                continue;
            }
            let Some((marker, range)) = printed(element) else {
                continue;
            };
            if text
                .slice_utf16(element.range.start..element.range.end)
                .map(trim_js)
                != Some(marker)
                || existing_notes
                    .iter()
                    .any(|note| note.start <= range.start && range.end <= note.end)
            {
                continue;
            }
            references.entry(marker).or_default().push(NoteReference {
                range,
                anchor: None,
                page_indexes: Vec::new(),
                line_ids: Vec::new(),
            });
        }
        let mut counts = HashMap::new();
        for (_, marker, _) in &bodies {
            *counts.entry(*marker).or_insert(0usize) += 1;
        }
        for (index, &(element, marker, marker_range)) in bodies.iter().enumerate() {
            let Some(supported) = references.get(marker) else {
                continue;
            };
            let end = bodies
                .get(index + 1)
                .map_or(rendered_end, |(element, _, _)| element.range.start);
            if marker_range.end >= end
                || existing_notes
                    .iter()
                    .any(|note| note.start <= marker_range.start && marker_range.end <= note.end)
            {
                continue;
            }
            let Some(identity) = footnote_identity(marker, "", None) else {
                continue;
            };
            result.push((
                RawBlock {
                    metadata: MarkupMetadata {
                        start: Some(element.markup_start),
                        attributes: element.attributes.clone(),
                        note_marker: Some(marker_range),
                    },
                    kind: Kind::Footnote,
                    label: identity.label,
                    start: marker_range.end,
                    end,
                    anchor: None,
                    markup_tag: Some(element.tag.clone()),
                    aliases: identity.aliases,
                    parent_label: None,
                },
                if counts.get(marker) == Some(&1) {
                    supported.clone()
                } else {
                    Vec::new()
                },
            ));
        }
    }
    result
}

fn supplement_courtlistener_printed_notes(
    structure: &mut DocumentStructure,
    notes: Vec<(RawBlock, Vec<NoteReference>)>,
) {
    for (raw, references) in notes {
        let Some(marker) = raw.metadata.note_marker else {
            continue;
        };
        if structure.notes.iter().any(|note| {
            note.label_range == marker
                || (note.body_range.start <= marker.start && marker.end <= note.body_range.end)
        }) {
            continue;
        }
        // NOTES, a printed label and SUP corroborate a recovered role; the
        // ordinary source P remains Native and is not an explicit footnote tag.
        let mut node = crate::StructureNode::new(
            format!("heuristic-footnote-markup-{}", raw.metadata.start.unwrap()),
            NodeKind::Footnote,
            ScalarRange {
                start: raw.start,
                end: raw.end,
            },
            crate::ENGINE_ORIGIN,
            Derivation::Heuristic,
            None,
        );
        node.label = Some(raw.label);
        node.aliases = Some(raw.aliases);
        node.markup_tag = raw.markup_tag;
        node.markup_start = raw.metadata.start;
        node.markup_attributes = raw.metadata.attributes;
        node.marker_range = Some(marker);
        node.grammar = Some("courtlistener-notes-sup".to_owned());
        structure.notes.push(Note {
            id: format!("note-{}", node.id),
            node_id: Some(node.id.clone()),
            ownership: node.ownership.clone(),
            kind: NoteKindV2::Footnote,
            label_range: marker,
            body_range: node.range,
            label_ranges: vec![marker],
            body_ranges: vec![node.range],
            primary_reference: references.first().map(|reference| reference.range),
            references,
        });
        structure.nodes.push(node);
    }
}

fn courtlistener_uncontained_note_blocks(
    markup: &str,
    elements: &[MarkupElement],
    text: &str,
    existing: &[RawBlock],
) -> Vec<RawBlock> {
    let candidates = elements
        .iter()
        .enumerate()
        .filter(|(_, element)| {
            element.tag.eq_ignore_ascii_case("a")
                && element
                    .attributes
                    .get("name")
                    .is_some_and(|name| !name.is_empty())
                && ((element
                    .attributes
                    .get("href")
                    .is_some_and(|href| href.starts_with('#'))
                    && markup
                        .get(element.markup_start..element.markup_end)
                        .is_some_and(|raw| raw.contains("Footnote ")))
                    || (element
                        .attributes
                        .get("name")
                        .is_some_and(|name| name.starts_with("footnote"))
                        && !element.attributes.contains_key("href")))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Vec::new();
    }
    let coordinates = ScalarText::new(text);
    let mut anchors = HashMap::<&str, Option<usize>>::new();
    for (index, element) in elements.iter().enumerate() {
        for name in [
            element.attributes.get("id"),
            element
                .tag
                .eq_ignore_ascii_case("a")
                .then(|| element.attributes.get("name"))
                .flatten(),
        ]
        .into_iter()
        .flatten()
        .filter(|name| !name.is_empty())
        {
            anchors
                .entry(name.as_str())
                .and_modify(|owner| {
                    if *owner != Some(index) {
                        *owner = None;
                    }
                })
                .or_insert(Some(index));
        }
    }
    let gap = |start, end| {
        markup
            .get(start..end)
            .is_some_and(|raw| trim_js(raw).is_empty())
    };
    let inner = |element: &MarkupElement| {
        let raw = markup.get(element.markup_start..element.markup_end)?;
        let start = markup_tag_end(raw)? + 1;
        let end = raw.rfind("</")?;
        let (tag, _) = parse_tag(&raw[end..], true)?;
        (tag.eq_ignore_ascii_case(&element.tag))
            .then(|| raw.get(start..end))
            .flatten()
    };
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label_pattern = LABEL.get_or_init(|| Regex::new(r"^Footnote ([0-9]{1,5}):$").unwrap());
    let mut result = Vec::new();
    let descendant = |index: usize, ancestor: usize| {
        let mut parent = elements[index].parent;
        while let Some(index) = parent {
            if index == ancestor {
                return true;
            }
            parent = elements[index].parent;
        }
        false
    };
    let decoded_gap = |start, end| {
        markup
            .get(start..end)
            .is_some_and(|raw| trim_js(&decode_entities(raw)).is_empty())
    };
    let mut paragraph_notes = Vec::new();
    for &(note_index, note) in &candidates {
        let Some(name) = note.attributes.get("name") else {
            continue;
        };
        let Some(number) = name.strip_prefix("footnote").filter(|number| {
            !number.is_empty()
                && number.len() <= 5
                && number.bytes().all(|byte| byte.is_ascii_digit())
        }) else {
            continue;
        };
        if note.attributes.contains_key("href")
            || anchors.get(name.as_str()).copied().flatten() != Some(note_index)
        {
            continue;
        }
        let Some(paragraph_index) = note.parent else {
            continue;
        };
        let paragraph = &elements[paragraph_index];
        if !paragraph.tag.eq_ignore_ascii_case("p")
            || inner(paragraph).is_none()
            || elements.iter().enumerate().any(|(index, element)| {
                element.tag.eq_ignore_ascii_case("p") && descendant(index, paragraph_index)
            })
            || existing.iter().any(|block| {
                block.metadata.start == Some(note.markup_start)
                    || block.kind == Kind::Footnote
                        && block.start < paragraph.range.end
                        && paragraph.range.start < block.end
            })
        {
            continue;
        }
        let Some((_, marker)) = elements
            .iter()
            .enumerate()
            .find(|(_, element)| element.parent == Some(note_index))
        else {
            continue;
        };
        if !marker.tag.eq_ignore_ascii_case("span")
            || marker.range.start != paragraph.range.start
            || note.range.start != marker.range.start
            || coordinates
                .slice_utf16(marker.range.start..marker.range.end)
                .map(trim_js)
                != Some(number)
            || !coordinates
                .slice_utf16(marker.range.end..paragraph.range.end)
                .is_some_and(|body| body.chars().any(char::is_alphabetic))
        {
            continue;
        }
        let mut incoming = elements.iter().filter(|element| {
            element.tag.eq_ignore_ascii_case("a")
                && element
                    .attributes
                    .get("href")
                    .and_then(|href| href.rsplit_once('#'))
                    .is_some_and(|(_, target)| target == name.as_str())
        });
        let Some(reference) = incoming.next() else {
            continue;
        };
        if incoming.next().is_some()
            || reference.markup_end >= paragraph.markup_start
            || !reference
                .attributes
                .get("href")
                .is_some_and(|href| href.starts_with("file:"))
            || coordinates
                .slice_utf16(reference.range.start..reference.range.end)
                .map(trim_js)
                != Some(number)
        {
            continue;
        }
        // A bookmark can close mid-note. Admit the rest only where explicit
        // same-name bookmark spans cover it inside this complete paragraph.
        let mut covered = note.range.end;
        for (_, element) in elements.iter().enumerate().filter(|(index, element)| {
            element.tag.eq_ignore_ascii_case("span")
                && element.markup_start >= note.markup_end
                && descendant(*index, paragraph_index)
                && element.attributes.get("style").is_some_and(|style| {
                    style.split(';').any(|declaration| {
                        declaration
                            .split_once(':')
                            .is_some_and(|(property, value)| {
                                property.trim().eq_ignore_ascii_case("mso-bookmark")
                                    && value.trim() == name.as_str()
                            })
                    })
                })
        }) {
            if element.range.start > covered
                && !coordinates
                    .slice_utf16(covered..element.range.start)
                    .is_some_and(|gap| trim_js(gap).is_empty())
            {
                break;
            }
            covered = covered.max(element.range.end);
        }
        if !coordinates
            .slice_utf16(covered..paragraph.range.end)
            .is_some_and(|tail| trim_js(tail).is_empty())
        {
            continue;
        }
        paragraph_notes.push((note, paragraph, number, marker.range));
    }
    if let (Some(first), Some(last)) = (paragraph_notes.first(), paragraph_notes.last()) {
        let first_paragraph = first.1;
        let last_paragraph = last.1;
        let mut ancestor_tags = Vec::new();
        let mut parent = last_paragraph.parent;
        while let Some(index) = parent {
            ancestor_tags.push(elements[index].tag.as_str());
            parent = elements[index].parent;
        }
        let literal_empty_tail = markup
            .get(last_paragraph.markup_end..)
            .is_some_and(|mut tail| {
                while let Some(at) = tail.find('<') {
                    if !trim_js(&decode_entities(&tail[..at])).is_empty() {
                        return false;
                    }
                    let raw = &tail[at..];
                    let Some(end) = markup_tag_end(raw) else {
                        return false;
                    };
                    let closing = raw.starts_with("</");
                    let Some((tag, _)) = parse_tag(&raw[..end + 1], closing) else {
                        return false;
                    };
                    if !tag.eq_ignore_ascii_case("p")
                        && !tag.eq_ignore_ascii_case("span")
                        && !(closing
                            && ancestor_tags
                                .iter()
                                .any(|ancestor| tag.eq_ignore_ascii_case(ancestor)))
                    {
                        return false;
                    }
                    tail = &raw[end + 1..];
                }
                trim_js(&decode_entities(tail)).is_empty()
            });
        let owners = paragraph_notes
            .iter()
            .map(|(_, paragraph, _, _)| paragraph.markup_start)
            .collect::<HashSet<_>>();
        let siblings = elements
            .iter()
            .filter(|element| {
                element.parent == first_paragraph.parent
                    && element.markup_start >= first_paragraph.markup_start
            })
            .collect::<Vec<_>>();
        let coherent = paragraph_notes
            .iter()
            .all(|(_, paragraph, _, _)| paragraph.parent == first_paragraph.parent)
            && siblings.iter().all(|element| {
                element.tag.eq_ignore_ascii_case("p")
                    && (owners.contains(&element.markup_start)
                        || coordinates
                            .slice_utf16(element.range.start..element.range.end)
                            .is_some_and(|text| trim_js(text).is_empty()))
            })
            && siblings
                .windows(2)
                .all(|pair| decoded_gap(pair[0].markup_end, pair[1].markup_start))
            && coordinates
                .slice_utf16(last_paragraph.range.end..coordinates.utf16_len())
                .is_some_and(|tail| trim_js(tail).is_empty());
        let separated = elements
            .iter()
            .rev()
            .find(|element| {
                element.tag.eq_ignore_ascii_case("hr")
                    && element.markup_end <= first_paragraph.markup_start
            })
            .is_some_and(|separator| {
                let mut parent = separator.parent;
                let mut end = separator.markup_end;
                while parent != first_paragraph.parent {
                    let Some(index) = parent else { return false };
                    let owner = &elements[index];
                    let Some(raw) = markup.get(end..owner.markup_end) else {
                        return false;
                    };
                    let decoded = decode_entities(raw);
                    let raw = trim_js(&decoded);
                    if !raw.starts_with("</")
                        || markup_tag_end(raw).map(|at| at + 1) != Some(raw.len())
                        || !parse_tag(raw, true).is_some_and(|(tag, attributes)| {
                            tag.eq_ignore_ascii_case(&owner.tag) && attributes.is_empty()
                        })
                    {
                        return false;
                    }
                    end = owner.markup_end;
                    parent = owner.parent;
                }
                decoded_gap(end, first_paragraph.markup_start)
            });
        if coherent && separated && literal_empty_tail {
            for (note, paragraph, number, marker) in paragraph_notes {
                let Some(identity) =
                    footnote_identity(number, "", note.attributes.get("name").cloned())
                else {
                    continue;
                };
                result.push(RawBlock {
                    metadata: MarkupMetadata {
                        start: Some(note.markup_start),
                        attributes: note.attributes.clone(),
                        note_marker: Some(marker),
                    },
                    kind: Kind::Footnote,
                    label: identity.label,
                    start: marker.end,
                    end: paragraph.range.end,
                    anchor: identity.anchor,
                    markup_tag: Some(note.tag.clone()),
                    aliases: identity.aliases,
                    parent_label: None,
                });
            }
        }
    }
    for (note_index, note) in candidates {
        let Some(raw_label) = coordinates.slice_utf16(note.range.start..note.range.end) else {
            continue;
        };
        let label = trim_js(raw_label);
        let Some(capture) = label_pattern.captures(label) else {
            continue;
        };
        let Some(name) = note.attributes.get("name") else {
            continue;
        };
        if anchors.get(name.as_str()).copied().flatten() != Some(note_index)
            || existing.iter().any(|block| {
                block.metadata.start == Some(note.markup_start)
                    || block.kind == Kind::Footnote
                        && block.start <= note.range.start
                        && note.range.end <= block.end
            })
        {
            continue;
        }
        let Some(reference_name) = note
            .attributes
            .get("href")
            .and_then(|href| href.strip_prefix('#'))
        else {
            continue;
        };
        let Some(reference_index) = anchors.get(reference_name).copied().flatten() else {
            continue;
        };
        let reference = &elements[reference_index];
        if !reference.tag.eq_ignore_ascii_case("a")
            || reference.markup_end >= note.markup_start
            || reference.attributes.get("name").map(String::as_str) != Some(reference_name)
            || reference
                .attributes
                .get("href")
                .and_then(|href| href.strip_prefix('#'))
                != Some(name.as_str())
            || coordinates
                .slice_utf16(reference.range.start..reference.range.end)
                .map(trim_js)
                != Some(format!("[FN{}]", &capture[1]).as_str())
        {
            continue;
        }
        let Some(parent) = note.parent else {
            continue;
        };
        let siblings = elements
            .iter()
            .filter(|element| element.parent == Some(parent))
            .collect::<Vec<_>>();
        let Some(position) = siblings
            .iter()
            .position(|element| element.markup_start == note.markup_start)
        else {
            continue;
        };
        if position < 3 {
            continue;
        }
        let preceding = &siblings[position - 3..position];
        let heading = preceding[0];
        if !heading.tag.eq_ignore_ascii_case("div")
            || reference.markup_end >= heading.markup_start
            || coordinates
                .slice_utf16(heading.range.start..heading.range.end)
                .map(trim_js)
                != Some("Footnotes")
            || preceding[1..]
                .iter()
                .any(|element| !element.tag.eq_ignore_ascii_case("br"))
            || !gap(heading.markup_end, preceding[1].markup_start)
            || !gap(preceding[1].markup_end, preceding[2].markup_start)
            || !gap(preceding[2].markup_end, note.markup_start)
        {
            continue;
        }
        let following = &siblings[position + 1..];
        let Some(separator_at) = following
            .iter()
            .position(|element| element.tag.eq_ignore_ascii_case("br"))
        else {
            continue;
        };
        // ponytail: only the witnessed terminal single fragment; broader inline
        // layouts and continuations need their own independent source evidence.
        if following[..separator_at]
            .iter()
            .any(|element| !element.tag.eq_ignore_ascii_case("i"))
            || following.len() != separator_at + 5
        {
            continue;
        }
        let separators = &following[separator_at..separator_at + 4];
        let navigation = following[separator_at + 4];
        if separators
            .iter()
            .any(|element| !element.tag.eq_ignore_ascii_case("br"))
            || separators
                .windows(2)
                .any(|pair| !gap(pair[0].markup_end, pair[1].markup_start))
            || !gap(separators[3].markup_end, navigation.markup_start)
            || !navigation.tag.eq_ignore_ascii_case("div")
        {
            continue;
        }
        let Some(navigation_index) = elements
            .binary_search_by_key(&navigation.markup_start, |element| element.markup_start)
            .ok()
        else {
            continue;
        };
        let mut children = elements
            .iter()
            .enumerate()
            .filter(|(_, element)| element.parent == Some(navigation_index));
        let Some((form_index, form)) = children.next() else {
            continue;
        };
        if children.next().is_some()
            || !form.tag.eq_ignore_ascii_case("form")
            || !form
                .attributes
                .get("method")
                .is_some_and(|method| method.eq_ignore_ascii_case("LINK"))
            || !form
                .attributes
                .get("action")
                .is_some_and(|action| !action.is_empty())
            || elements
                .iter()
                .any(|element| element.parent == Some(form_index))
            || !inner(form).is_some_and(|raw| trim_js(raw).is_empty())
            || inner(navigation).map(trim_js)
                != markup.get(form.markup_start..form.markup_end).map(trim_js)
        {
            continue;
        }
        let end = separators[0].range.start;
        if !coordinates
            .slice_utf16(end..coordinates.utf16_len())
            .is_some_and(|tail| trim_js(tail).is_empty())
        {
            continue;
        }
        let Some(raw_body) = coordinates.slice_utf16(note.range.end..end) else {
            continue;
        };
        let body = trim_js(raw_body);
        if body.is_empty() || !body.chars().any(char::is_alphabetic) {
            continue;
        }
        let body_leading =
            raw_body.len() - raw_body.trim_start_matches(javascript_whitespace).len();
        let start = note.range.end + utf16_len(&raw_body[..body_leading]);
        let label_leading =
            raw_label.len() - raw_label.trim_start_matches(javascript_whitespace).len();
        let marker_start = note.range.start + utf16_len(&raw_label[..label_leading]);
        let Some(mut identity) = footnote_identity(&capture[1], "", Some(name.clone())) else {
            continue;
        };
        identity.aliases.push(label.to_owned());
        result.push(RawBlock {
            metadata: MarkupMetadata {
                start: Some(note.markup_start),
                attributes: note.attributes.clone(),
                note_marker: Some(ScalarRange {
                    start: marker_start,
                    end: marker_start + utf16_len(label),
                }),
            },
            kind: Kind::Footnote,
            label: identity.label,
            start,
            end: start + utf16_len(body),
            anchor: identity.anchor,
            markup_tag: Some(note.tag.clone()),
            aliases: identity.aliases,
            parent_label: None,
        });
    }
    result
}

fn native_identity(
    provider: DocumentProvider,
    tag: &str,
    attributes: &Attributes<'_>,
    courtlistener_footnote: bool,
) -> Option<PendingBlock> {
    let id = ["eid", "id", "name"]
        .into_iter()
        .map(|name| attribute(attributes, name))
        .find(|value| !value.is_empty())
        .unwrap_or_default();
    let anchor = (!id.is_empty()).then(|| id.to_owned());
    if tag == "a" && matches!(provider, DocumentProvider::CourtListener) {
        if contains_ascii_word(attribute(attributes, "class"), "page-label", false) {
            static PAGE_ID: OnceLock<Regex> = OnceLock::new();
            let preferred = attribute(attributes, "data-label");
            let label = if preferred.is_empty() {
                PAGE_ID
                    .get_or_init(|| Regex::new(r"(?i)^p(\d{1,5})$").unwrap())
                    .captures(id)
                    .map(|capture| capture[1].to_owned())
                    .map_or(Cow::Borrowed(""), Cow::Owned)
            } else {
                Cow::Borrowed(preferred)
            };
            return page_identity(&label, attributes, anchor, true);
        }
        return None;
    }
    if matches!(provider, DocumentProvider::CourtListener) {
        if tag == "span"
            && contains_ascii_word(attribute(attributes, "class"), "star-pagination", false)
        {
            let raw = ["label", "data-label"]
                .into_iter()
                .map(|name| attribute(attributes, name))
                .find(|value| !value.is_empty())
                .unwrap_or_default()
                .to_owned();
            return page_identity(&raw, attributes, anchor, true);
        }
        if courtlistener_footnote {
            let raw = ["data-label", "label", "n"]
                .into_iter()
                .map(|name| attribute(attributes, name))
                .find(|value| !value.is_empty())
                .unwrap_or_default()
                .to_owned();
            return Some(
                footnote_identity(&raw, &id, anchor.clone()).unwrap_or_else(|| PendingBlock {
                    metadata: MarkupMetadata::default(),
                    tag: String::new(),
                    kind: Kind::Footnote,
                    label: String::new(),
                    start: 0,
                    anchor,
                    aliases: Vec::new(),
                    parent_label: None,
                    inline: false,
                    page_label: None,
                    citation_index: None,
                    page_scheme: None,
                }),
            );
        }
    }
    if tag == "page-number" {
        static PAGE_ID: OnceLock<Regex> = OnceLock::new();
        let preferred = ["label", "page"]
            .into_iter()
            .map(|name| attribute(attributes, name))
            .find(|value| !value.is_empty());
        let raw = preferred.map_or_else(
            || {
                PAGE_ID
                    .get_or_init(|| Regex::new(r"(?i)(?:page|p)[_-]?(\d{1,5})$").unwrap())
                    .captures(id)
                    .map(|capture| capture[1].to_owned())
                    .map_or(Cow::Borrowed(""), Cow::Owned)
            },
            Cow::Borrowed,
        );
        return page_identity(&raw, attributes, anchor, false);
    }
    static PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    static DIV_PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    let paragraph = PARAGRAPH
        .get_or_init(|| Regex::new(r"(?i)^(?:para(?:graph)?)[_-]?(\d{1,5})$").unwrap())
        .captures(id)
        .map(|capture| capture[1].to_owned())
        .or_else(|| {
            (matches!(provider, DocumentProvider::CourtListener)
                && tag == "div"
                && contains_ascii_word(attribute(attributes, "class"), "num", false))
            .then(|| {
                DIV_PARAGRAPH
                    .get_or_init(|| Regex::new(r"(?i)^p(\d{1,5})$").unwrap())
                    .captures(id)
                    .map(|capture| capture[1].to_owned())
            })
            .flatten()
        });
    if let Some(paragraph) = paragraph.filter(|_| {
        matches!(
            provider,
            DocumentProvider::Tna | DocumentProvider::CourtListener
        )
    }) {
        return Some(PendingBlock {
            metadata: MarkupMetadata::default(),
            tag: String::new(),
            kind: Kind::Paragraph,
            label: format!("par{}", paragraph.parse::<u32>().ok()?),
            start: 0,
            anchor,
            aliases: Vec::new(),
            parent_label: None,
            inline: false,
            page_label: None,
            citation_index: None,
            page_scheme: None,
        });
    }
    // Source positions identify unanchored structure without inventing browser IDs.
    let source_kind = match tag {
        "p" | "blockquote" | "author" | "judges" | "attorneys" => Some(Kind::Prose),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "heading" | "h" => Some(Kind::Heading),
        "opinion" | "headnotes" | "casebody" => Some(Kind::Section),
        "div"
            if attribute(attributes, "class")
                .split_ascii_whitespace()
                .any(|class| class == "co_headtext") =>
        {
            Some(Kind::Heading)
        }
        "div"
            if attribute(attributes, "class")
                .split_ascii_whitespace()
                .any(|class| class == "co_paragraph") =>
        {
            Some(Kind::Prose)
        }
        "div"
            if attribute(attributes, "class")
                .split_ascii_whitespace()
                .any(|class| matches!(class, "co_opinionBlock" | "co_section")) =>
        {
            Some(Kind::Section)
        }
        "div" if id == "headmatter" => Some(Kind::Section),
        "article" | "section" | "div"
            if ["opinion", "casebody", "head-matter"].iter().any(|class| {
                attribute(attributes, "class")
                    .split_ascii_whitespace()
                    .any(|value| value == *class)
            }) =>
        {
            Some(Kind::Section)
        }
        "ul" | "ol" | "li" => Some(Kind::List),
        "table" => Some(Kind::Table),
        "tr" => Some(Kind::Row),
        "td" | "th" => Some(Kind::Cell),
        _ => None,
    };
    if let Some(kind) = source_kind.filter(|_| matches!(provider, DocumentProvider::CourtListener))
    {
        return Some(PendingBlock {
            metadata: MarkupMetadata::default(),
            tag: String::new(),
            kind,
            label: id.to_owned(),
            start: 0,
            anchor,
            aliases: Vec::new(),
            parent_label: None,
            inline: false,
            page_label: None,
            citation_index: None,
            page_scheme: None,
        });
    }
    if matches!(
        tag,
        "article" | "chapter" | "level" | "part" | "section" | "subsection"
    ) && !id.is_empty()
    {
        static SECTION: OnceLock<Regex> = OnceLock::new();
        let section = clean_section_id(id);
        if SECTION
            .get_or_init(|| Regex::new(r"^\d{1,8}(?:[.-]\d{1,8}){0,3}(?:\([^)]+\))*$").unwrap())
            .is_match(&section)
        {
            return Some(PendingBlock {
                metadata: MarkupMetadata::default(),
                tag: String::new(),
                kind: Kind::Section,
                label: format!("sec{section}"),
                start: 0,
                anchor,
                aliases: Vec::new(),
                parent_label: None,
                inline: false,
                page_label: None,
                citation_index: None,
                page_scheme: None,
            });
        }
    }
    static SECTION_ID: OnceLock<Regex> = OnceLock::new();
    SECTION_ID
        .get_or_init(|| {
            Regex::new(r"(?i)^sec(\d{1,8}(?:[.-]\d{1,8}){0,3}(?:\([^)]+\))*)$").unwrap()
        })
        .captures(id)
        .map(|capture| PendingBlock {
            metadata: MarkupMetadata::default(),
            tag: String::new(),
            kind: Kind::Section,
            label: format!("sec{}", &capture[1]),
            start: 0,
            anchor,
            aliases: Vec::new(),
            parent_label: None,
            inline: false,
            page_label: None,
            citation_index: None,
            page_scheme: None,
        })
}

fn citation_page_alias(citation: &str, label: &str) -> String {
    let trimmed = citation.trim_end_matches(javascript_whitespace);
    let start = trimmed
        .char_indices()
        .rev()
        .find(|(_, value)| javascript_whitespace(*value))
        .map_or(0, |(at, value)| at + value.len_utf8());
    if start == trimmed.len() {
        citation.to_owned()
    } else {
        format!("{}{label}", &citation[..start])
    }
}

fn page_aliases(
    label: &str,
    citation_index: Option<usize>,
    page_citations: &[String],
    page_scheme: Option<&str>,
) -> Vec<String> {
    let mut aliases = Vec::new();
    if !(label.len() <= 5 && label.bytes().all(|byte| byte.is_ascii_digit())) {
        aliases.push(label.to_owned());
    }
    if let Some(citation) = citation_index
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| page_citations.get(index))
    {
        aliases.push(citation_page_alias(citation, label));
    }
    if let Some(page_scheme) = page_scheme {
        aliases.push(format!("{page_scheme}, at *{label}"));
    }
    aliases
}

fn leading_footnote_marker(value: &str) -> Option<&str> {
    let end = if value.starts_with('[') {
        let close = value.find(']')?;
        (close <= 6 && value[1..close].bytes().all(|byte| byte.is_ascii_digit()))
            .then_some(close + 1)?
    } else if value.starts_with('(') {
        let close = value.find(')')?;
        (close <= 6 && value[1..close].bytes().all(|byte| byte.is_ascii_digit()))
            .then_some(close + 1)?
    } else if value
        .chars()
        .next()
        .is_some_and(|value| matches!(value, '*' | '†'))
    {
        value
            .char_indices()
            .take_while(|(_, character)| matches!(character, '*' | '†'))
            .last()
            .map(|(at, character)| at + character.len_utf8())?
    } else {
        let end = value
            .bytes()
            .take(6)
            .position(|byte| !byte.is_ascii_digit())
            .unwrap_or(value.len().min(6));
        if end > 5 {
            return None;
        }
        end
    };
    (end > 0
        && value[..end].chars().count() <= 7
        && value[end..]
            .chars()
            .next()
            .is_none_or(javascript_whitespace))
    .then(|| &value[..end])
}

fn break_tag(tag: &str) -> bool {
    matches!(
        tag,
        "article"
            | "blockquote"
            | "br"
            | "chapter"
            | "conclusion"
            | "content"
            | "decision"
            | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "heading"
            | "item"
            | "level"
            | "li"
            | "opinion"
            | "p"
            | "paragraph"
            | "part"
            | "preface"
            | "section"
            | "subsection"
            | "tr"
    )
}

fn void_tag(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn parse_tag(raw: &str, closing: bool) -> Option<(String, &str)> {
    let mut value = raw.strip_prefix('<')?;
    value = value.trim_start_matches(javascript_whitespace);
    if closing {
        value = value
            .strip_prefix('/')?
            .trim_start_matches(javascript_whitespace);
    } else if value
        .chars()
        .next()
        .is_some_and(|value| matches!(value, '/' | '!'))
    {
        return None;
    }
    let end = value
        .char_indices()
        .take_while(|(_, character)| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '-')
        })
        .last()
        .map(|(at, character)| at + character.len_utf8())?;
    let qualified = &value[..end];
    let tag = qualified.rsplit(':').next()?.to_ascii_lowercase();
    let attrs = value[end..].strip_suffix('>')?;
    Some((tag, attrs))
}

fn markup_tag_end(markup: &str) -> Option<usize> {
    let mut quote = None;
    for (at, character) in markup.char_indices() {
        if let Some(open) = quote {
            if character == open {
                quote = None;
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '>' {
            return Some(at);
        }
    }
    None
}

fn append_text(text: &mut String, position: &mut usize, value: &str) {
    if value.is_empty() {
        return;
    }
    let separated = text.chars().next_back().is_some_and(|value| {
        javascript_whitespace(value) || matches!(value, '(' | '[' | '{' | '/' | '-')
    }) || value.chars().next().is_some_and(|value| {
        javascript_whitespace(value)
            || matches!(
                value,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | '}' | ']' | '/' | '-'
            )
    });
    if !text.is_empty() && !separated {
        text.push(' ');
        *position += 1;
    }
    *position += utf16_len(value);
    text.push_str(value);
}

// Inline HTML boundaries do not themselves supply whitespace. Collapse source
// whitespace across text nodes while retaining adjacent citation/page text.
fn flush_inline_whitespace(text: &mut String, position: &mut usize, whitespace_pending: &mut bool) {
    if *whitespace_pending
        && !text.is_empty()
        && !text.chars().next_back().is_some_and(javascript_whitespace)
    {
        text.push(' ');
        *position += 1;
    }
    *whitespace_pending = false;
}

fn append_inline_text(
    text: &mut String,
    position: &mut usize,
    value: &str,
    whitespace_pending: &mut bool,
) {
    for character in value.chars() {
        if javascript_whitespace(character) {
            *whitespace_pending = true;
        } else {
            flush_inline_whitespace(text, position, whitespace_pending);
            text.push(character);
            *position += character.len_utf16();
        }
    }
}

fn append_preformatted_text(
    text: &mut String,
    position: &mut usize,
    value: &str,
    bounds: &mut Option<(usize, usize)>,
) {
    if value.is_empty() {
        return;
    }
    let start = text.len();
    text.push_str(value);
    *position += utf16_len(value);
    bounds.get_or_insert((start, 0)).1 = text.len();
}

fn append_break(text: &mut String, position: &mut usize) {
    if text.is_empty() || text.ends_with('\n') {
        return;
    }
    text.push('\n');
    *position += 1;
}

fn normalized_text(text: String) -> String {
    let mut result = String::with_capacity(text.len());
    for character in text.chars() {
        if character == '\n' {
            while result.ends_with(' ') || result.ends_with('\t') {
                result.pop();
            }
            if !result.ends_with("\n\n") {
                result.push(character);
            }
        } else {
            result.push(character);
        }
    }
    result
}

fn observe_footnote_marker(open: &mut [PendingBlock], text: &str, seen: &mut HashSet<usize>) {
    let Some(note) = open
        .iter_mut()
        .rev()
        .find(|entry| entry.kind == Kind::Footnote)
    else {
        return;
    };
    let Some(start) = note.metadata.start else {
        return;
    };
    if note.label.is_empty() && !trim_js(text).is_empty() && seen.insert(start) {
        if let Some(identity) =
            leading_footnote_marker(text).and_then(|marker| footnote_identity(marker, "", None))
        {
            note.label = identity.label;
            note.aliases = identity.aliases;
        }
    }
}

fn render_markup<'a>(
    provider: DocumentProvider,
    markup: &'a str,
    page_citations: &[String],
) -> Result<RenderedMarkup, EngineError> {
    render_markup_in_context(
        provider,
        markup,
        page_citations,
        false,
        MarkupSerialization::HtmlSource,
    )
}

fn render_markup_in_context<'a>(
    provider: DocumentProvider,
    markup: &'a str,
    page_citations: &[String],
    resource_org_context: bool,
    markup_serialization: MarkupSerialization,
) -> Result<RenderedMarkup, EngineError> {
    let mut text = String::new();
    let mut blocks = Vec::new();
    let mut recovered_body_blocks = Vec::new();
    let mut open = Vec::<PendingBlock>::new();
    let mut observed_note_markers = HashSet::new();
    let mut tag_stack = Vec::<String>::new();
    let mut elements = Vec::<MarkupElement>::new();
    let mut element_stack = Vec::<usize>::new();
    let mut declared_body_stack = Vec::<usize>::new();
    let mut matched_role_starts = HashSet::new();
    let mut resource_org_depths = Vec::<usize>::new();
    let mut hidden_jumpback_depth = None::<usize>;
    let mut open_excluded = Vec::<OpenRange>::new();
    let mut exclusions = Vec::<ScalarRange>::new();
    let mut open_cited_authorities = Vec::<(usize, CitedAuthority)>::new();
    let mut cited_authorities = Vec::<CitedAuthority>::new();
    let mut cited_authority_keys = HashSet::<String>::new();
    let mut open_note_references = Vec::<(usize, String, Option<String>, usize)>::new();
    let mut note_references = Vec::<(String, Option<String>, ScalarRange)>::new();
    let mut text_page = None::<TextPage>;
    let mut position = 0;
    let mut inline_whitespace = false;
    let mut pre_text_bounds = None::<(usize, usize)>;
    let mut ignore_pre_linefeed = false;
    let mut harvard_casebody = false;
    let mut page_starts = Vec::<PageStart>::new();
    let mut attributes = Attributes::new();
    let mut at = 0;
    let mut recover_as_text = false;
    while at < markup.len() {
        if markup.as_bytes()[at] != b'<' || recover_as_text {
            let search_start = at + usize::from(std::mem::take(&mut recover_as_text));
            let end = markup[search_start..]
                .find('<')
                .map_or(markup.len(), |end| search_start + end);
            let raw = &markup[at..end];
            if hidden_jumpback_depth.is_some() {
                at = end;
                continue;
            }
            // HTML input preprocessing happens before character references;
            // browser serialization has already undergone that parsing step.
            let raw = if matches!(markup_serialization, MarkupSerialization::HtmlSource)
                && raw.contains('\r')
            {
                Cow::Owned(raw.replace("\r\n", "\n").replace('\r', "\n"))
            } else {
                Cow::Borrowed(raw)
            };
            let decoded = decode_entities(&raw);
            let decoded = if std::mem::take(&mut ignore_pre_linefeed) {
                decoded.strip_prefix('\n').unwrap_or(&decoded)
            } else {
                &decoded
            };
            for (_, authority) in &mut open_cited_authorities {
                authority.citation.push_str(decoded);
            }
            let rendered = normalize_javascript_whitespace(decoded);
            observe_footnote_marker(&mut open, &rendered, &mut observed_note_markers);
            let label = page_value(&rendered);
            if text_page.is_some() && label.bytes().any(|byte| byte.is_ascii_digit()) {
                let pending = text_page.take().unwrap();
                let numeric = label.len() <= 5 && label.bytes().all(|byte| byte.is_ascii_digit());
                page_starts.push(PageStart {
                    metadata: pending.metadata,
                    label: if numeric {
                        format!("page{}", label.parse::<u32>().map_err(EngineError::source)?)
                    } else {
                        format!(
                            "page{}",
                            label
                                .chars()
                                .filter(|value| !javascript_whitespace(*value))
                                .collect::<String>()
                        )
                    },
                    start: pending.start,
                    anchor: pending.anchor,
                    markup_tag: Some("span".to_owned()),
                    aliases: page_aliases(
                        &label,
                        pending.citation_index,
                        page_citations,
                        pending.page_scheme.as_deref(),
                    ),
                    scope: None,
                });
            }
            if matches!(provider, DocumentProvider::CourtListener) {
                if tag_stack.iter().any(|tag| tag == "pre") {
                    append_preformatted_text(
                        &mut text,
                        &mut position,
                        decoded,
                        &mut pre_text_bounds,
                    );
                } else {
                    append_inline_text(&mut text, &mut position, decoded, &mut inline_whitespace);
                }
            } else {
                append_text(&mut text, &mut position, &rendered);
            }
            at = end;
            continue;
        }
        // Only the next token immediately after an HTML PRE start can lose a
        // leading LF. An intervening comment or tag ends that convenience.
        ignore_pre_linefeed = false;
        if markup[at..].starts_with("<!--") {
            if let Some(end) = markup[at + 4..].find("-->") {
                at += 4 + end + 3;
                continue;
            }
        }
        if markup[at..].starts_with("<![CDATA[") {
            if let Some(end) = markup[at + 9..].find("]]>") {
                let value = &markup[at + 9..at + 9 + end];
                if hidden_jumpback_depth.is_some() {
                    at += 9 + end + 3;
                    continue;
                }
                observe_footnote_marker(
                    &mut open,
                    &normalize_javascript_whitespace(value),
                    &mut observed_note_markers,
                );
                if matches!(provider, DocumentProvider::CourtListener) {
                    if tag_stack.iter().any(|tag| tag == "pre") {
                        append_preformatted_text(
                            &mut text,
                            &mut position,
                            value,
                            &mut pre_text_bounds,
                        );
                    } else {
                        append_inline_text(&mut text, &mut position, value, &mut inline_whitespace);
                    }
                } else {
                    append_text(&mut text, &mut position, value);
                }
                at += 9 + end + 3;
                continue;
            }
        }
        let Some(relative_end) = markup_tag_end(&markup[at..]) else {
            // An unterminated token remains literal source text.
            recover_as_text = true;
            continue;
        };
        let end = at + relative_end + 1;
        let raw = &markup[at..end];
        if let Some((tag, _)) = parse_tag(raw, true) {
            let mut closing_source_start = None;
            if let Some(depth) = tag_stack.iter().rposition(|value| value == &tag) {
                if matches!(provider, DocumentProvider::CourtListener) {
                    closing_source_start = element_stack
                        .get(depth)
                        .map(|index| elements[*index].markup_start);
                    if matches!(
                        tag.as_str(),
                        "opinion"
                            | "opinion_text"
                            | "concurrence_text"
                            | "dissent_text"
                            | "appendix"
                            | "correction"
                            | "summary"
                            | "syllabus"
                            | "page_number"
                            | "footnote_body"
                            | "footnote_number"
                            | "footnote_reference"
                    ) || (tag == "p"
                        && element_stack.get(depth).is_some_and(|index| {
                            elements[*index]
                                .attributes
                                .get("data-type")
                                .is_some_and(|role| role == "header")
                        }))
                        || (tag == "div"
                            && element_stack.get(depth).is_some_and(|index| {
                                let element = &elements[*index];
                                courtlistener_ldml_note(element)
                                    || element.attributes.get("class").is_some_and(|classes| {
                                        classes.split_ascii_whitespace().any(|class| {
                                            matches!(
                                                class,
                                                "de-writing"
                                                    | "co_opinionBlock"
                                                    | "opinion"
                                                    | "ldml-decision"
                                                    | "ldml-opinion"
                                                    | "ldml-notes"
                                            )
                                        })
                                    })
                            }))
                        || (matches!(tag.as_str(), "article" | "section")
                            && element_stack.get(depth).is_some_and(|index| {
                                elements[*index]
                                    .attributes
                                    .get("class")
                                    .is_some_and(|classes| {
                                        classes
                                            .split_ascii_whitespace()
                                            .any(|class| class == "opinion")
                                    })
                            }))
                        || (tag == "sup"
                            && element_stack.get(depth).is_some_and(|index| {
                                let element = &elements[*index];
                                element.attributes.get("class").is_some_and(|classes| {
                                    classes
                                        .split_ascii_whitespace()
                                        .any(|class| class == "ldml-superscript")
                                }) && markup_tag_end(&markup[element.markup_start..])
                                    .and_then(|end| markup.get(element.markup_start + end + 1..at))
                                    .is_some_and(courtlistener_ldml_bracketed_marker)
                            }))
                    {
                        // An enclosing close or EOF does not establish a complete role.
                        if let Some(start) = closing_source_start {
                            matched_role_starts.insert(start);
                        }
                    }
                    for index in element_stack.drain(depth..) {
                        elements[index].markup_end = end;
                        elements[index].range.end = position;
                    }
                }
                resource_org_depths.retain(|open| *open < depth);
                if hidden_jumpback_depth.is_some_and(|open| open >= depth) {
                    hidden_jumpback_depth = None;
                }
                if tag == "a" {
                    if let Some(index) = open_note_references
                        .iter()
                        .rposition(|reference| reference.0 == depth)
                    {
                        let (_, target, anchor, start) = open_note_references.remove(index);
                        note_references.push((
                            target,
                            anchor,
                            ScalarRange {
                                start,
                                end: position,
                            },
                        ));
                    }
                }
                if tag == "ref" {
                    if let Some(index) = open_cited_authorities
                        .iter()
                        .rposition(|authority| authority.0 == depth)
                    {
                        let (_, mut authority) = open_cited_authorities.remove(index);
                        authority.citation = normalize_javascript_whitespace(&authority.citation);
                        if authority.citation.is_empty() {
                            authority.citation = authority.canonical.clone().unwrap_or_default();
                        }
                        let key = authority
                            .canonical
                            .as_ref()
                            .unwrap_or(&authority.citation)
                            .to_lowercase();
                        if !key.is_empty() && cited_authority_keys.insert(key) {
                            cited_authorities.push(authority);
                        }
                    }
                }
                if let Some(index) = open_excluded
                    .iter()
                    .rposition(|entry| entry.tag == tag && entry.depth == depth)
                {
                    let pending = open_excluded.remove(index);
                    if position > pending.start {
                        exclusions.push(ScalarRange {
                            start: pending.start,
                            end: position,
                        });
                    }
                }
                tag_stack.truncate(depth);
            }
            if matches!(provider, DocumentProvider::CourtListener) {
                if let Some(depth) = declared_body_stack
                    .iter()
                    .rposition(|index| elements[*index].tag.eq_ignore_ascii_case(&tag))
                {
                    let element = &elements[declared_body_stack[depth]];
                    declared_body_stack.truncate(depth);
                    if closing_source_start != Some(element.markup_start) {
                        // An enclosing P close can end the physical element before
                        // its supplied container close. Keep that literal graph and
                        // recover the declared body from its own matching tokens.
                        matched_role_starts.insert(element.markup_start);
                        let anchor = element
                            .attributes
                            .get("id")
                            .filter(|id| !id.is_empty())
                            .cloned();
                        recovered_body_blocks.push(RawBlock {
                            metadata: MarkupMetadata {
                                start: Some(element.markup_start),
                                attributes: element.attributes.clone(),
                                note_marker: None,
                            },
                            kind: Kind::Section,
                            label: anchor.clone().unwrap_or_default(),
                            start: element.range.start,
                            end: position,
                            anchor,
                            markup_tag: Some(tag.clone()),
                            aliases: Vec::new(),
                            parent_label: None,
                        });
                    }
                }
            }
            if tag == "span" {
                text_page = None;
            }
            // Typed owners close with their actual source element, including
            // active descendants when the source closes an enclosing element.
            let closing = if matches!(provider, DocumentProvider::CourtListener) {
                closing_source_start.and_then(|start| {
                    open.iter().position(|entry| {
                        entry
                            .metadata
                            .start
                            .is_some_and(|position| position >= start)
                    })
                })
            } else {
                open.iter().rposition(|entry| entry.tag == tag)
            };
            if let Some(index) = closing {
                let stop = if matches!(provider, DocumentProvider::CourtListener) {
                    open.len()
                } else {
                    index + 1
                };
                for pending in open.drain(index..stop).rev() {
                    if position > pending.start
                        || (matches!(provider, DocumentProvider::CourtListener)
                            && pending.kind != Kind::Page)
                    {
                        blocks.push(RawBlock {
                            metadata: pending.metadata,
                            kind: pending.kind,
                            label: pending.label,
                            start: pending.start,
                            end: position,
                            anchor: pending.anchor,
                            markup_tag: Some(pending.tag),
                            aliases: pending.aliases,
                            parent_label: pending.parent_label,
                        });
                    }
                }
            }
            if break_tag(&tag)
                && !(matches!(provider, DocumentProvider::CourtListener) && tag == "content")
            {
                append_break(&mut text, &mut position);
            }
            at = end;
            continue;
        }
        let Some((tag, attrs)) = parse_tag(raw, false) else {
            at = end;
            continue;
        };
        let syntactic_self_closing = raw[..raw.len() - 1]
            .trim_end_matches(javascript_whitespace)
            .ends_with('/');
        let attrs = if syntactic_self_closing {
            attrs
                .trim_end_matches(javascript_whitespace)
                .strip_suffix('/')
                .unwrap_or(attrs)
        } else {
            attrs
        };
        parse_attributes(attrs, &mut attributes);
        if !harvard_casebody
            && matches!(tag.as_str(), "section" | "article")
            && contains_ascii_word(raw, "casebody", true)
        {
            harvard_casebody = true;
        }
        let self_closing = syntactic_self_closing || void_tag(&tag);
        if matches!(provider, DocumentProvider::CourtListener) && tag == "pre" && !self_closing {
            // Preserve the inline source seam without creating a PRE wrapper
            // break or charging preceding collapsed whitespace to its range.
            flush_inline_whitespace(&mut text, &mut position, &mut inline_whitespace);
            ignore_pre_linefeed = matches!(markup_serialization, MarkupSerialization::HtmlSource);
        }
        let depth = tag_stack.len();
        if matches!(provider, DocumentProvider::CourtListener) && !self_closing {
            if attribute(&attributes, "id") == "resource-org-text" {
                resource_org_depths.push(depth);
            }
            // The retained publisher stylesheet hides jumpback UI only within
            // this actual source container. Its markup and link stay preserved
            // in the source graph; it contributes no visible opinion text.
            if (resource_org_context || !resource_org_depths.is_empty())
                && attribute(&attributes, "class")
                    .split_ascii_whitespace()
                    .any(|class| class == "jumpback")
            {
                hidden_jumpback_depth.get_or_insert(depth);
            }
        }
        if matches!(provider, DocumentProvider::CourtListener) && tag == "a" && !self_closing {
            if let Some(target) = attribute(&attributes, "href")
                .strip_prefix('#')
                .filter(|target| !target.is_empty())
            {
                let anchor = ["id", "name"]
                    .into_iter()
                    .map(|name| attribute(&attributes, name))
                    .find(|value| !value.is_empty())
                    .map(str::to_owned);
                open_note_references.push((depth, target.to_owned(), anchor, position));
            }
        }
        if tag == "ref" && !self_closing {
            let canonical = attribute(&attributes, "uk:canonical");
            let kind = attribute(&attributes, "uk:type");
            open_cited_authorities.push((
                depth,
                CitedAuthority {
                    citation: String::new(),
                    canonical: (!canonical.is_empty()).then(|| canonical.to_owned()),
                    kind: (!kind.is_empty()).then(|| kind.to_owned()),
                },
            ));
        }
        let footnote_body = courtlistener_footnote_body(provider, &tag, &attributes);
        let mut identity = native_identity(provider, &tag, &attributes, footnote_body);
        if matches!(provider, DocumentProvider::CourtListener) {
            if let Some(identity) = &mut identity {
                identity.metadata = MarkupMetadata {
                    start: Some(at),
                    attributes: attributes
                        .iter()
                        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
                        .collect(),
                    note_marker: None,
                };
            }
        }
        if !self_closing {
            tag_stack.push(tag.clone());
            static FOOTNOTE_CONTAINER_ID: OnceLock<Regex> = OnceLock::new();
            if matches!(provider, DocumentProvider::CourtListener)
                && (footnote_body
                    || contains_ascii_word(attribute(&attributes, "class"), "footnotes", true)
                    || FOOTNOTE_CONTAINER_ID
                        .get_or_init(|| Regex::new(r"(?i)^(?:fn|footnote)[_-]").unwrap())
                        .is_match(attribute(&attributes, "id")))
            {
                open_excluded.push(OpenRange {
                    tag: tag.clone(),
                    depth,
                    start: position,
                });
            }
        }
        if matches!(provider, DocumentProvider::CourtListener)
            && !self_closing
            && tag == "span"
            && contains_ascii_word(attribute(&attributes, "class"), "star-pagination", false)
            && identity
                .as_ref()
                .is_none_or(|value| value.kind != Kind::Page)
        {
            let (citation_index, page_scheme) = page_citation_identity(&attributes);
            text_page = Some(TextPage {
                metadata: MarkupMetadata {
                    start: Some(at),
                    attributes: attributes
                        .iter()
                        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
                        .collect(),
                    note_marker: None,
                },
                start: position,
                anchor: (!attribute(&attributes, "id").is_empty())
                    .then(|| attribute(&attributes, "id").to_owned()),
                citation_index,
                page_scheme,
            });
        }
        if identity
            .as_ref()
            .is_some_and(|value| value.kind == Kind::Page)
        {
            let identity = identity.take().unwrap();
            if !identity.inline {
                append_break(&mut text, &mut position);
            }
            let page_label = identity.page_label.as_deref().unwrap_or_default();
            page_starts.push(PageStart {
                metadata: identity.metadata,
                label: identity.label,
                start: position,
                anchor: identity.anchor,
                markup_tag: Some(tag.clone()),
                aliases: page_aliases(
                    page_label,
                    identity.citation_index,
                    page_citations,
                    identity.page_scheme.as_deref(),
                ),
                scope: None,
            });
        } else if let Some(mut identity) = identity.filter(|value| value.kind != Kind::Page) {
            append_break(&mut text, &mut position);
            identity.tag = tag.clone();
            identity.start = position;
            identity.parent_label = open
                .iter()
                .rev()
                .find(|entry| {
                    if matches!(provider, DocumentProvider::CourtListener) {
                        entry.kind != Kind::Page
                    } else if matches!(identity.kind, Kind::Prose | Kind::Heading) {
                        matches!(entry.kind, Kind::Prose | Kind::Footnote | Kind::Section)
                    } else {
                        entry.kind == identity.kind
                    }
                })
                .map(|entry| {
                    entry.metadata.start.map_or_else(
                        || entry.label.clone(),
                        |start| format!("native-markup-{start}"),
                    )
                });
            if self_closing
                && matches!(provider, DocumentProvider::CourtListener)
                && identity.kind != Kind::Page
            {
                blocks.push(RawBlock {
                    metadata: identity.metadata,
                    kind: identity.kind,
                    label: identity.label,
                    start: position,
                    end: position,
                    anchor: identity.anchor,
                    markup_tag: Some(identity.tag),
                    aliases: identity.aliases,
                    parent_label: identity.parent_label,
                });
            } else {
                open.push(identity);
            }
        }
        if tag == "br" {
            append_break(&mut text, &mut position);
        }
        if matches!(provider, DocumentProvider::CourtListener) {
            let tag = raw[1..]
                .trim_start_matches(javascript_whitespace)
                .split(|c: char| javascript_whitespace(c) || matches!(c, '/' | '>'))
                .next()
                .unwrap_or_default()
                .to_owned();
            let index = elements.len();
            elements.push(MarkupElement {
                markup_start: at,
                markup_end: end,
                tag,
                attributes: attributes
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
                parent: element_stack.last().copied(),
                range: ScalarRange {
                    start: position,
                    end: position,
                },
            });
            if !self_closing {
                element_stack.push(index);
                if [
                    "opinion_text",
                    "concurrence_text",
                    "dissent_text",
                    "syllabus",
                ]
                .iter()
                .any(|tag| elements[index].tag.eq_ignore_ascii_case(tag))
                {
                    declared_body_stack.push(index);
                }
            }
        }
        at = if matches!(tag.as_str(), "script" | "style") && !self_closing {
            // Raw-text contents are source bytes, not visible opinion prose or
            // nested markup. Preserve the element, then visit its closing tag.
            markup[end..]
                .match_indices("</")
                .find_map(|(offset, _)| {
                    let closing = &markup[end + offset + 2..];
                    let name = closing.get(..tag.len())?;
                    (name.eq_ignore_ascii_case(&tag)
                        && closing[tag.len()..]
                            .chars()
                            .next()
                            .is_some_and(|value| javascript_whitespace(value) || value == '>'))
                    .then_some(end + offset)
                })
                .unwrap_or(markup.len())
        } else {
            end
        };
    }
    // CourtListener normalizes ordinary inline text while appending it. A
    // later sweep would alter PRE whitespace after its UTF-16 ranges were set.
    let mut normalized = if matches!(provider, DocumentProvider::CourtListener) {
        text
    } else {
        normalized_text(text)
    };
    if matches!(provider, DocumentProvider::CourtListener) {
        // The printed star label is the EM child; the accessibility label is
        // separate source text and must not become part of the page number.
        let mut coordinates = None;
        let original_page_starts = page_starts.len();
        for (index, element) in elements.iter().enumerate() {
            let Some(parent_index) = element.parent else {
                continue;
            };
            let parent = &elements[parent_index];
            let accessible_span = element.tag == "span"
                && index == parent_index + 2
                && elements[parent_index + 1].tag == "span"
                && elements[parent_index + 1].parent == Some(parent_index)
                && elements[parent_index + 1]
                    .attributes
                    .get("class")
                    .is_some_and(|classes| {
                        classes
                            .split_ascii_whitespace()
                            .any(|class| class == "co_accessibilityLabel")
                    })
                && !elements.get(index + 1).is_some_and(|next| {
                    next.parent == Some(index) || next.parent == Some(parent_index)
                });
            if (element.tag != "em" && !accessible_span)
                || parent.tag != "span"
                || !parent.attributes.get("class").is_some_and(|classes| {
                    classes
                        .split_ascii_whitespace()
                        .any(|class| class == "co_starPage")
                })
            {
                continue;
            }
            let coordinates = coordinates.get_or_insert_with(|| ScalarText::new(&normalized));
            let Some(label) = coordinates
                .slice_utf16(element.range.start..element.range.end)
                .map(trim_js)
                .and_then(|text| text.strip_prefix('*'))
                .map(trim_js)
                .filter(|label| {
                    !label.is_empty()
                        && label.len() <= 5
                        && label.bytes().all(|byte| byte.is_ascii_digit())
                })
            else {
                continue;
            };
            let scope = if accessible_span {
                let accessibility = &elements[parent_index + 1];
                if coordinates
                    .slice_utf16(accessibility.range.start..accessibility.range.end)
                    .map(trim_js)
                    != Some(format!("Star page {label}").as_str())
                    || coordinates
                        .slice_utf16(element.range.start..element.range.end)
                        .map(trim_js)
                        != Some(format!("*{label}").as_str())
                    || page_starts
                        .iter()
                        .any(|page| page.metadata.start == Some(parent.markup_start))
                {
                    continue;
                }
                let Some(scope) = courtlistener_opinion_page_scope(&elements, parent) else {
                    continue;
                };
                Some(scope)
            } else {
                None
            };
            page_starts.push(PageStart {
                metadata: MarkupMetadata {
                    start: Some(parent.markup_start),
                    attributes: parent.attributes.clone(),
                    note_marker: None,
                },
                label: format!("page{}", label.parse::<u32>().map_err(EngineError::source)?),
                start: parent.range.start,
                anchor: parent
                    .attributes
                    .get("id")
                    .filter(|id| !id.is_empty())
                    .cloned(),
                markup_tag: Some(parent.tag.clone()),
                aliases: if accessible_span {
                    vec![format!("*{label}")]
                } else {
                    Vec::new()
                },
                scope,
            });
        }
        for (index, element) in elements.iter().enumerate() {
            if element.tag != "span"
                || elements
                    .get(index + 1)
                    .is_some_and(|next| next.parent == Some(index))
                || page_starts
                    .iter()
                    .any(|page| page.metadata.start == Some(element.markup_start))
                || !element.attributes.get("class").is_some_and(|classes| {
                    classes
                        .split_ascii_whitespace()
                        .any(|class| class == "co_starPage")
                })
            {
                continue;
            }
            let coordinates = coordinates.get_or_insert_with(|| ScalarText::new(&normalized));
            let Some(printed) = coordinates
                .slice_utf16(element.range.start..element.range.end)
                .map(trim_js)
            else {
                continue;
            };
            let Some(label) = printed.strip_prefix('*').filter(|label| {
                !label.is_empty()
                    && label.len() <= 5
                    && label.bytes().all(|byte| byte.is_ascii_digit())
            }) else {
                continue;
            };
            let Some(scope) = courtlistener_opinion_page_scope(&elements, element) else {
                continue;
            };
            // This literal marker has no accessibility-label child. Its page
            // ends within the actual opinion, before separate notes or a footer.
            page_starts.push(PageStart {
                metadata: MarkupMetadata {
                    start: Some(element.markup_start),
                    attributes: element.attributes.clone(),
                    note_marker: None,
                },
                label: format!("page{}", label.parse::<u32>().map_err(EngineError::source)?),
                start: element.range.start,
                anchor: element
                    .attributes
                    .get("id")
                    .filter(|id| !id.is_empty())
                    .cloned(),
                markup_tag: Some(element.tag.clone()),
                aliases: vec![printed.to_owned()],
                scope: Some(scope),
            });
        }
        for element in elements
            .iter()
            .filter(|element| element.tag == "page_number")
        {
            if !matched_role_starts.contains(&element.markup_start)
                || page_starts
                    .iter()
                    .any(|page| page.metadata.start == Some(element.markup_start))
            {
                continue;
            }
            let coordinates = coordinates.get_or_insert_with(|| ScalarText::new(&normalized));
            let Some(printed) = coordinates
                .slice_utf16(element.range.start..element.range.end)
                .map(trim_js)
            else {
                continue;
            };
            let label = page_value(printed);
            if label.is_empty()
                || label.len() > 5
                || !label.bytes().all(|byte| byte.is_ascii_digit())
            {
                continue;
            }
            page_starts.push(PageStart {
                metadata: MarkupMetadata {
                    start: Some(element.markup_start),
                    attributes: element.attributes.clone(),
                    note_marker: None,
                },
                label: format!("page{}", label.parse::<u32>().map_err(EngineError::source)?),
                start: element.range.start,
                anchor: element
                    .attributes
                    .get("id")
                    .filter(|id| !id.is_empty())
                    .cloned(),
                markup_tag: Some(element.tag.clone()),
                aliases: Vec::new(),
                scope: courtlistener_opinion_page_scope(&elements, element),
            });
        }
        if page_starts.len() != original_page_starts {
            page_starts.sort_by_key(|page| page.start);
        }
    }
    let leading = (normalized.len() - normalized.trim_start_matches(javascript_whitespace).len())
        .min(pre_text_bounds.map_or(usize::MAX, |(start, _)| start));
    let leading_trim = utf16_len(&normalized[..leading]);
    let trailing = normalized
        .trim_end_matches(javascript_whitespace)
        .len()
        .max(leading)
        .max(pre_text_bounds.map_or(0, |(_, end)| end));
    let trimmed = &normalized[leading..trailing];
    let text_utf16 = utf16_len(trimmed);
    let raw_end = leading_trim + text_utf16;
    for index in element_stack {
        elements[index].markup_end = markup.len();
        elements[index].range.end = raw_end;
    }
    for element in &mut elements {
        element.range.start = element.range.start.clamp(leading_trim, raw_end) - leading_trim;
        element.range.end = element.range.end.clamp(leading_trim, raw_end) - leading_trim;
    }
    for (_, target, anchor, start) in open_note_references {
        note_references.push((
            target,
            anchor,
            ScalarRange {
                start,
                end: raw_end.max(start),
            },
        ));
    }
    for (_, _, range) in &mut note_references {
        range.start = range.start.clamp(leading_trim, raw_end) - leading_trim;
        range.end = range.end.clamp(leading_trim, raw_end) - leading_trim;
    }
    for pending in open_excluded {
        if raw_end > pending.start {
            exclusions.push(ScalarRange {
                start: pending.start,
                end: raw_end,
            });
        }
    }
    for pending in open {
        if raw_end > pending.start
            || (matches!(provider, DocumentProvider::CourtListener) && pending.kind != Kind::Page)
        {
            blocks.push(RawBlock {
                metadata: pending.metadata,
                kind: pending.kind,
                label: pending.label,
                start: pending.start,
                end: raw_end.max(pending.start),
                anchor: pending.anchor,
                markup_tag: Some(pending.tag),
                aliases: pending.aliases,
                parent_label: pending.parent_label,
            });
        }
    }
    let note_ends = blocks
        .iter()
        .filter(|block| block.kind == Kind::Footnote)
        .filter_map(|block| Some((block.metadata.start?, block.end)))
        .collect::<HashMap<_, _>>();
    let page_ends = {
        let mut next_by_track = HashMap::new();
        let mut ends = vec![raw_end; page_starts.len()];
        for (index, page) in page_starts.iter().enumerate().rev() {
            let scheme = page
                .metadata
                .attributes
                .get("pagescheme")
                .filter(|scheme| !scheme.is_empty())
                .map(String::as_str);
            let citation_index = ["citation-index", "data-citation-index"]
                .into_iter()
                .find_map(|name| page.metadata.attributes.get(name)?.parse::<usize>().ok())
                .filter(|value| *value != 0);
            // Parallel reporters paginate the same opinion independently.
            // An unqualified marker still has only source-order evidence.
            ends[index] = if scheme.is_some() || citation_index.is_some() {
                next_by_track
                    .insert(
                        (
                            scheme,
                            if scheme.is_none() {
                                citation_index
                            } else {
                                None
                            },
                        ),
                        page.start,
                    )
                    .unwrap_or(raw_end)
            } else {
                page_starts
                    .get(index + 1)
                    .map_or(raw_end, |next| next.start)
            };
        }
        ends
    };
    for (page, end) in page_starts.into_iter().zip(page_ends) {
        let (end, scope_parent) = page.scope.map_or((end, None), |(scope_end, id)| {
            (end.min(scope_end), Some(id))
        });
        if end > page.start {
            // Literal elements retain source-byte order. A note-contained page
            // belongs to its nearest declared Footnote, including repeated
            // reporter labels; its body cannot continue into a sibling note.
            let mut parent = page
                .metadata
                .start
                .and_then(|start| {
                    elements
                        .binary_search_by_key(&start, |element| element.markup_start)
                        .ok()
                })
                .and_then(|index| elements[index].parent);
            let mut note_owner = None;
            while let Some(index) = parent {
                let element = &elements[index];
                if let Some(&note_end) = note_ends.get(&element.markup_start) {
                    note_owner =
                        Some((note_end, format!("native-markup-{}", element.markup_start)));
                    break;
                }
                parent = element.parent;
            }
            let (end, parent_label) = note_owner.map_or((end, scope_parent), |(note_end, id)| {
                (end.min(note_end), Some(id))
            });
            blocks.push(RawBlock {
                metadata: page.metadata,
                kind: Kind::Page,
                label: page.label,
                start: page.start,
                end,
                anchor: page.anchor,
                markup_tag: page.markup_tag,
                aliases: page.aliases,
                parent_label,
            });
        }
    }
    // Pre-trim normalized-provider UTF-16 becomes rendered-document UTF-16;
    // leading trim is subtracted and split-surrogate positions stay invalid.
    let normalized_coordinates = ScalarText::new(&normalized);
    let normalized_utf16 = normalized_coordinates.utf16_len();
    for block in blocks.iter_mut().chain(&mut recovered_body_blocks) {
        // Empty source structure remains a point in the trimmed document.
        if matches!(provider, DocumentProvider::CourtListener) && block.kind != Kind::Page {
            block.start = block.start.clamp(leading_trim, raw_end);
            block.end = block.end.clamp(block.start, raw_end);
        }
        let start = block.start.checked_sub(leading_trim).ok_or_else(|| {
            EngineError::source("native block starts before trimmed provider text")
        })?;
        let end = block
            .end
            .checked_sub(leading_trim)
            .ok_or_else(|| EngineError::source("native block ends before trimmed provider text"))?;
        if start > text_utf16 || start > end {
            return Err(EngineError::source(
                "native block has an invalid provider UTF-16 range",
            ));
        }
        if end > text_utf16 {
            if block.end <= raw_end || block.end > normalized_utf16 {
                return Err(EngineError::source(
                    "native block has an invalid trim overhang",
                ));
            }
            let from = normalized_coordinates
                .byte_at_utf16(raw_end)
                .ok_or_else(|| EngineError::source("UTF-16 range splits a Unicode scalar"))?;
            let until = normalized_coordinates
                .byte_at_utf16(block.end)
                .ok_or_else(|| EngineError::source("UTF-16 range splits a Unicode scalar"))?;
            if normalized[from..until]
                .chars()
                .any(|value| !javascript_whitespace(value))
            {
                return Err(EngineError::source(
                    "native block trim overhang contains text",
                ));
            }
        }
        block.start = start;
        block.end = end;
    }
    let mut projected_exclusions = Vec::with_capacity(exclusions.len());
    for range in exclusions {
        let start = range
            .start
            .checked_sub(leading_trim)
            .ok_or_else(|| EngineError::source("exclusion starts before trimmed provider text"))?;
        let end = range.end.saturating_sub(leading_trim).min(text_utf16);
        if start > end || end > text_utf16 {
            return Err(EngineError::source(
                "exclusion has an invalid provider UTF-16 range",
            ));
        }
        projected_exclusions.push(ScalarRange { start, end });
    }
    normalized.truncate(trailing);
    normalized.drain(..leading);
    let coordinates = ScalarText::new(&normalized);
    for (_, _, range) in &mut note_references {
        let text = coordinates
            .slice_utf16(range.start..range.end)
            .ok_or_else(|| EngineError::source("invalid native note-reference range"))?;
        let trimmed = trim_js(text);
        let leading = text.len() - text.trim_start_matches(javascript_whitespace).len();
        range.start += utf16_len(&text[..leading]);
        range.end = range.start + utf16_len(trimmed);
    }
    Ok(RenderedMarkup {
        text: normalized,
        blocks,
        recovered_body_blocks,
        elements,
        matched_role_starts,
        exclusions: projected_exclusions,
        cited_authorities,
        note_references,
        source_hash: format!("{:x}", Sha256::digest(markup.as_bytes())),
        harvard_casebody,
    })
}

fn report_start(citation: Option<&str>) -> Option<u32> {
    citation.and_then(crate::canadian_report_start)
}

pub(super) fn source_heading_exclusion(tag: &str, attributes: &BTreeMap<String, String>) -> bool {
    matches!(
        tag,
        "blockquote" | "head-matter" | "headnotes" | "parties" | "judges" | "attorneys"
    ) || attributes.get("id").is_some_and(|id| id == "headmatter")
        || attributes.get("class").is_some_and(|classes| {
            classes
                .split_ascii_whitespace()
                .any(|class| class == "head-matter")
        })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdapterScope<'a> {
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    excerpt_of: Option<&'a str>,
    markup_serialization: MarkupSerialization,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdapterRevision<'a> {
    provider: &'a str,
    representation_revision: &'a str,
    citation: Option<&'a str>,
    page_citations: &'a [String],
    markup_ancestors: &'a [MarkupAncestor],
    scope: AdapterScope<'a>,
    profile: &'a str,
}

fn native_markup_evidence(
    input: NativeMarkupInput,
) -> Result<
    (
        DocumentInput,
        Vec<CitedAuthority>,
        Vec<(String, Option<String>, ScalarRange)>,
        HashMap<String, (String, MarkupMetadata)>,
        Vec<MarkupElement>,
        String,
        Vec<MarkupAncestor>,
        HashSet<usize>,
        Vec<(RawBlock, Vec<NoteReference>)>,
    ),
    EngineError,
> {
    let NativeMarkupInput {
        provider: provider_name,
        id,
        url,
        text: fallback_text,
        markup,
        citation,
        page_citations,
        markup_ancestors,
        scope,
    } = input;
    let provider = DocumentProvider::from_name(&provider_name)
        .ok_or_else(|| EngineError::source("unsupported native-markup provider"))?;
    let use_markup = markup
        .as_deref()
        .is_some_and(|value| !trim_js(value).is_empty());
    let rendered = use_markup
        .then(|| {
            render_markup_in_context(
                provider,
                markup.as_deref().unwrap(),
                &page_citations,
                markup_ancestors.iter().any(|ancestor| {
                    ancestor
                        .attributes
                        .get("id")
                        .is_some_and(|id| id == "resource-org-text")
                }),
                scope.markup_serialization,
            )
        })
        .transpose()?;
    let rendered = if let Some(mut rendered) = rendered {
        if rendered.text.is_empty() {
            rendered.text = fallback_text;
        }
        rendered
    } else {
        let source_hash = format!(
            "{:x}",
            Sha256::digest(
                markup
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .unwrap_or(&fallback_text)
                    .as_bytes(),
            ),
        );
        RenderedMarkup {
            text: fallback_text,
            blocks: Vec::new(),
            recovered_body_blocks: Vec::new(),
            elements: Vec::new(),
            matched_role_starts: HashSet::new(),
            exclusions: Vec::new(),
            cited_authorities: Vec::new(),
            note_references: Vec::new(),
            source_hash,
            harvard_casebody: false,
        }
    };
    let RenderedMarkup {
        text,
        blocks: mut rendered_blocks,
        mut recovered_body_blocks,
        elements,
        matched_role_starts,
        exclusions: mut rendered_exclusions,
        cited_authorities,
        note_references,
        source_hash: representation_revision,
        harvard_casebody: harvard,
    } = rendered;
    if matches!(provider, DocumentProvider::CourtListener)
        && scope.kind == "complete"
        && matches!(
            scope.markup_serialization,
            MarkupSerialization::HtmlSource | MarkupSerialization::BrowserHtml
        )
    {
        if let Some(markup) = markup.as_deref() {
            let notes =
                courtlistener_uncontained_note_blocks(markup, &elements, &text, &rendered_blocks);
            for note in &notes {
                if let Some(marker) = note.metadata.note_marker {
                    rendered_exclusions.push(ScalarRange {
                        start: marker.start,
                        end: note.end,
                    });
                }
            }
            rendered_blocks.extend(notes);
        }
    }
    if matches!(provider, DocumentProvider::CourtListener) {
        // Declared roles use the existing rendered source ranges. Creating
        // them while rendering would add breaks to previously retained text.
        for block in &mut rendered_blocks {
            if block.kind == Kind::Prose
                && block.metadata.start.is_some_and(|start| {
                    matched_role_starts.contains(&start)
                        && elements
                            .binary_search_by_key(&start, |element| element.markup_start)
                            .ok()
                            .is_some_and(|index| {
                                let element = &elements[index];
                                element.tag == "p"
                                    && element
                                        .attributes
                                        .get("data-type")
                                        .is_some_and(|role| role == "header")
                            })
                })
            {
                // Preserve this claim's identity, anchor, printed text and children.
                block.kind = Kind::Heading;
            }
        }
        let declared = courtlistener_declared_blocks(
            &elements,
            &rendered_blocks,
            &recovered_body_blocks,
            &matched_role_starts,
            &text,
        );
        rendered_blocks.extend(declared);
        recovered_body_blocks.sort_by_key(|block| block.metadata.start);
        rendered_blocks.extend(recovered_body_blocks);
    }
    let numbered_note_markers = if matches!(provider, DocumentProvider::CourtListener) {
        courtlistener_numbered_note_markers(&elements, &matched_role_starts, &text)
    } else {
        HashMap::new()
    };
    let sibling_note_markers = courtlistener_sibling_note_markers(&elements, &text);
    for block in &mut rendered_blocks {
        if block.kind == Kind::Footnote {
            if let Some((label, aliases, marker)) = block
                .metadata
                .start
                .and_then(|start| numbered_note_markers.get(&start))
            {
                if block.label.is_empty() || block.label == *label {
                    block.label = label.clone();
                    block.aliases = aliases.clone();
                    block.metadata.note_marker = Some(*marker);
                    block.start = marker.end;
                }
            }
            if let Some((label, _)) = block
                .metadata
                .start
                .and_then(|start| sibling_note_markers.get(&start))
            {
                block.label = label.clone();
            }
        }
    }
    let profile = if harvard {
        "case_lossy"
    } else if matches!(provider, DocumentProvider::CourtListener) {
        "case_contiguous_complete"
    } else {
        "case_lossy"
    };
    let adapter = AdapterRevision {
        provider: provider.as_str(),
        representation_revision: &representation_revision,
        citation: citation.as_deref(),
        page_citations: &page_citations,
        markup_ancestors: &markup_ancestors,
        scope: AdapterScope {
            kind: &scope.kind,
            excerpt_of: scope.excerpt_of.as_deref(),
            markup_serialization: scope.markup_serialization,
        },
        profile,
    };
    let adapter_revision = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&adapter).map_err(EngineError::source)?)
    );
    // Exact UTF-16 in final rendered/fallback text, never source markup.
    let coordinates = ScalarText::new(&text);
    let printed_notes = if matches!(provider, DocumentProvider::CourtListener)
        && scope.kind == "complete"
    {
        markup.as_deref().map(|markup| {
            courtlistener_printed_note_blocks(markup, &elements, &coordinates, &rendered_blocks)
        }).unwrap_or_default()
    } else {
        Vec::new()
    };
    let scalar = |offset: usize| {
        coordinates
            .scalar_at_utf16(offset)
            .ok_or_else(|| EngineError::source("provider UTF-16 range splits a Unicode scalar"))
    };
    let text_utf16 = coordinates.utf16_len();
    let native_kinds = rendered_blocks
        .iter()
        .map(|block| block.kind.evidence())
        .collect::<HashSet<_>>();
    let mut claims = Vec::with_capacity(rendered_blocks.len());
    let mut markup_tags = HashMap::with_capacity(rendered_blocks.len());
    for (index, raw) in rendered_blocks.into_iter().enumerate() {
        let id = raw.metadata.start.map_or_else(
            || format!("native-{:06}", index + 1),
            |start| format!("native-markup-{start}"),
        );
        if let Some(tag) = raw.markup_tag {
            markup_tags.insert(id.clone(), (tag, raw.metadata));
        }
        let claim_end = raw.end.min(text_utf16);
        claims.push(NativeClaim {
            id,
            kind: raw.kind.evidence(),
            label: (!raw.label.is_empty()).then_some(raw.label),
            aliases: raw.aliases,
            range: ScalarRange {
                start: scalar(raw.start)?,
                end: scalar(claim_end)?,
            },
            origin_id: ORIGIN,
            parent_label: raw.parent_label,
            anchor: raw.anchor,
        });
    }
    let scalar_end = coordinates.len();
    let coverage = crate::whole_document_coverage(scalar_end, |kind| {
        if native_kinds.contains(&kind) {
            CoverageState::Augment
        } else {
            CoverageState::Absent
        }
    });
    let mut exclusions = rendered_exclusions
        .iter()
        .map(|range| {
            Ok(Exclusion {
                range: ScalarRange {
                    start: scalar(range.start)?,
                    end: scalar(range.end)?,
                },
                applies_to: vec!["paragraph".to_owned(), "heading".to_owned()],
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    if matches!(provider, DocumentProvider::CourtListener) {
        for element in &elements {
            if source_heading_exclusion(&element.tag, &element.attributes) {
                exclusions.push(Exclusion {
                    range: ScalarRange {
                        start: scalar(element.range.start)?,
                        end: scalar(element.range.end)?,
                    },
                    applies_to: vec!["heading".to_owned()],
                });
            }
        }
        if markup_ancestors
            .iter()
            .any(|ancestor| source_heading_exclusion(&ancestor.tag, &ancestor.attributes))
        {
            exclusions.push(Exclusion {
                range: ScalarRange {
                    start: 0,
                    end: scalar_end,
                },
                applies_to: vec!["heading".to_owned()],
            });
        }
    }
    let scope_kind = match scope.kind.as_str() {
        "complete" => ScopeKind::Complete,
        "excerpt" => ScopeKind::Excerpt,
        _ => return Err(EngineError::source("invalid native-markup scope")),
    };
    let evidence = DocumentInput {
        url,
        doc_type: Some("cases"),
        report_start_page: report_start(citation.as_deref()),
        source_sha256: Some(adapter_revision),
        scope: Scope {
            kind: scope_kind,
            excerpt_of: scope.excerpt_of,
        },
        native_claims: claims,
        coverage,
        exclusions,
        ..DocumentInput::new(
            id,
            provider.as_str(),
            match profile {
                "case_contiguous_complete" => DetectionProfile::CaseContiguousComplete,
                _ => DetectionProfile::CaseLossy,
            },
            text,
            ORIGIN,
        )
    };
    Ok((
        evidence,
        cited_authorities,
        note_references,
        markup_tags,
        elements,
        representation_revision,
        markup_ancestors,
        matched_role_starts,
        printed_notes,
    ))
}

fn native_table_coordinates(structure: &mut DocumentStructure) {
    let mut tables = structure
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == NodeKind::Table && node.markup_start.is_some())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    tables.sort_by_key(|&index| structure.nodes[index].markup_start);
    let coordinates = ScalarText::new(structure.query_text());
    let mut replacements = HashMap::<String, String>::new();
    let mut grids = HashMap::<String, (Option<usize>, Option<usize>, Option<usize>)>::new();
    let mut table_headers = HashMap::<String, Vec<usize>>::new();
    for (table_index, &table) in tables.iter().enumerate() {
        let table_id = format!("table:{}", table_index + 1);
        let mut rows = structure
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                node.kind == NodeKind::Row
                    && node.parent_id.as_deref() == Some(&structure.nodes[table].id)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        rows.sort_by_key(|&index| structure.nodes[index].markup_start);
        let mut facts = Vec::new();
        let mut grid = vec![(structure.nodes[table].id.clone(), (Some(table_index + 1), None, None))];
        let mut ids = vec![(structure.nodes[table].id.clone(), table_id.clone())];
        let mut occupied = Vec::<(usize, usize, usize)>::new();
        let mut header_rows = Vec::new();
        let mut valid = true;
        'rows: for (row_index, &row) in rows.iter().enumerate() {
            let row_number = row_index + 1;
            let row_id = format!("{table_id}/row:{row_number}");
            ids.push((structure.nodes[row].id.clone(), row_id.clone()));
            grid.push((structure.nodes[row].id.clone(), (None, Some(row_number), None)));
            let mut cells = structure
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| {
                    node.kind == NodeKind::Cell
                        && node.parent_id.as_deref() == Some(&structure.nodes[row].id)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            cells.sort_by_key(|&index| structure.nodes[index].markup_start);
            if !cells.is_empty() && cells.iter().all(|&index| structure.nodes[index].markup_tag.as_deref() == Some("th")) {
                header_rows.push(row_number);
            }
            let mut column = 1usize;
            for &cell in &cells {
                let node = &structure.nodes[cell];
                while let Some(end) = occupied
                    .iter()
                    .filter(|&&(start, end, last_row)| {
                        start <= column && column <= end && row_number <= last_row
                    })
                    .map(|&(_, end, _)| end)
                    .max()
                {
                    let Some(next) = end.checked_add(1) else {
                        valid = false;
                        break 'rows;
                    };
                    column = next;
                }
                let span = node.column_span.unwrap_or(1);
                let Some(last_column) = span
                    .checked_sub(1)
                    .and_then(|span| column.checked_add(span))
                else {
                    valid = false;
                    break 'rows;
                };
                let Some(last_row) = node
                    .row_span
                    .unwrap_or(1)
                    .checked_sub(1)
                    .and_then(|span| row_number.checked_add(span))
                else {
                    valid = false;
                    break 'rows;
                };
                occupied.push((column, last_column, last_row));
                let range = structure.query_range(node);
                facts.push(crate::AuthoritativeTableCell {
                    ranges: structure.query_ranges(node).into_owned(),
                    table: table_index + 1,
                    table_name: None,
                    row: row_number,
                    column,
                    row_span: node.row_span,
                    column_span: node.column_span,
                    address: node.anchor.clone(),
                    display_value: None,
                    start: range.start,
                    end: range.end,
                });
                ids.push((node.id.clone(), format!("{row_id}/col:{column}")));
                grid.push((node.id.clone(), (None, Some(row_number), Some(column))));
                let Some(next) = last_column.checked_add(1) else {
                    valid = false;
                    break 'rows;
                };
                column = next;
            }
        }
        // The existing table owner validates address/span and text coordinates.
        // Unusable geometry keeps its complete source graph and attributes.
        if valid && crate::tables::AuthoritativeTables::new(&coordinates, &facts).is_ok() {
            table_headers.insert(structure.nodes[table].id.clone(), header_rows);
            replacements.extend(ids);
            grids.extend(grid);
        }
    }
    for node in &mut structure.nodes {
        if let Some(headers) = table_headers.remove(&node.id) { node.header_rows = Some(headers); }
        if let Some(&(table, row, column)) = grids.get(&node.id) {
            node.table_number = table;
            node.row = row;
            node.column = column;
        }
        if let Some(id) = replacements.get(&node.id) {
            node.id = id.clone();
            node.label = Some(id.clone());
        }
        if let Some(parent) = node.parent_id.as_ref() {
            if let Some(id) = replacements.get(parent) {
                node.parent_id = Some(id.clone());
            }
        }
    }
}

fn prefer_source_headings(structure: &mut DocumentStructure) {
    let mut headings = HashMap::new();
    for node in &structure.nodes {
        if node.kind == NodeKind::Heading
            && node.source == Derivation::Native
            && node.markup_start.is_some()
            && node.markup_tag.is_some()
        {
            let range = structure.query_range(node);
            headings
                .entry((range.start, range.end))
                .and_modify(|id| *id = None)
                .or_insert_with(|| Some(node.id.clone()));
        }
    }
    let replacements = structure
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Heading && node.source == Derivation::Heuristic)
        .filter_map(|node| {
            let range = structure.query_range(node);
            let native = headings.get(&(range.start, range.end))?.as_ref()?;
            Some((node.id.clone(), native.clone()))
        })
        .collect::<HashMap<_, _>>();
    // Resolve exact duplicate inference after IDs are assigned so unrelated
    // inferred nodes keep their identities. Partial overlaps remain distinct.
    structure
        .nodes
        .retain(|node| !replacements.contains_key(&node.id));
    for node in &mut structure.nodes {
        if let Some(native) = node.parent_id.as_ref().and_then(|id| replacements.get(id)) {
            node.parent_id = Some(native.clone());
        }
    }
    for diagnostic in &mut structure.diagnostics {
        for id in &mut diagnostic.node_ids {
            if let Some(native) = replacements.get(id) {
                *id = native.clone();
            }
        }
    }
}

fn prefer_source_note_markers(structure: &mut DocumentStructure) {
    let mut markers = HashMap::new();
    for node in &structure.nodes {
        if node.kind == NodeKind::Footnote
            && node.source == Derivation::Native
            && node.markup_start.is_some()
        {
            if let Some(marker) = node.marker_range.filter(|range| range.start < range.end) {
                markers
                    .entry(marker.start)
                    .and_modify(|id| *id = None)
                    .or_insert_with(|| Some(node.id.clone()));
            }
        }
    }
    let replacements = structure
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Paragraph && node.source == Derivation::Heuristic)
        .filter_map(|node| {
            let native = markers.get(&structure.query_range(node).start)?.as_ref()?;
            Some((node.id.clone(), native.clone()))
        })
        .collect::<HashMap<_, _>>();
    // A declared printed note label is not a guessed paragraph number. Resolve
    // the conflict after ID assignment without clipping earlier paragraphs.
    structure
        .nodes
        .retain(|node| !replacements.contains_key(&node.id));
    for node in &mut structure.nodes {
        if let Some(native) = node.parent_id.as_ref().and_then(|id| replacements.get(id)) {
            node.parent_id = Some(native.clone());
        }
    }
    for diagnostic in &mut structure.diagnostics {
        for id in &mut diagnostic.node_ids {
            if let Some(native) = replacements.get(id) {
                *id = native.clone();
            }
        }
    }
}

fn clip_courtlistener_paragraphs_to_opinions(
    structure: &mut DocumentStructure,
    matched_role_starts: &HashSet<usize>,
) {
    let mut native_scopes = HashMap::new();
    for node in &structure.nodes {
        if node.kind == NodeKind::Section && node.source == Derivation::Native {
            if let Some(start) = node
                .markup_start
                .filter(|start| matched_role_starts.contains(start))
            {
                native_scopes
                    .entry(start)
                    .and_modify(|scope| *scope = None)
                    .or_insert(Some(structure.query_range(node)));
            }
        }
    }
    if native_scopes.is_empty() {
        return;
    }
    let mut scopes = Vec::<Option<ScalarRange>>::with_capacity(structure.markup_elements.len());
    let mut paragraph_scopes = HashMap::new();
    let mut writing_starts = Vec::new();
    for element in &structure.markup_elements {
        let declared_opinion = [
            "opinion",
            "opinion_text",
            "concurrence_text",
            "dissent_text",
        ]
        .iter()
        .any(|tag| element.tag.eq_ignore_ascii_case(tag))
            || (element.tag.eq_ignore_ascii_case("div")
                && element.attributes.get("class").is_some_and(|classes| {
                    classes.split_ascii_whitespace().any(|class| {
                        matches!(class, "ldml-opinion" | "de-writing" | "co_opinionBlock")
                    })
                }))
            || (["article", "section", "div"]
                .iter()
                .any(|tag| element.tag.eq_ignore_ascii_case(tag))
                && element.attributes.get("class").is_some_and(|classes| {
                    classes
                        .split_ascii_whitespace()
                        .any(|class| class == "opinion")
                }));
        let scope = if declared_opinion {
            // An unmatched nearer writing cannot inherit a complete outer scope.
            native_scopes.get(&element.markup_start).copied().flatten()
        } else {
            element.parent.and_then(|parent| scopes[parent])
        };
        scopes.push(scope);
        if declared_opinion {
            if let Some(range) = scope.filter(|range| range.start < range.end) {
                writing_starts.push((element.markup_start, range.start));
            }
        }
        if element.tag.eq_ignore_ascii_case("p") && element.range.start < element.range.end {
            paragraph_scopes
                .entry(element.range.start)
                .and_modify(|scope| *scope = None)
                .or_insert(scope.map(|scope| (scope, element.range.end, element.markup_end)));
        }
    }
    for node in &mut structure.nodes {
        if node.kind == NodeKind::Paragraph && node.source == Derivation::Heuristic {
            if let Some((scope, paragraph_end, markup_end)) =
                paragraph_scopes.get(&node.range.start).copied().flatten()
            {
                node.range.end = node.range.end.min(scope.end);
                // A separately supplied writing after the literal P is also a
                // boundary, including a writing nested inside the same opinion.
                // Keep embedded writing markup inside the P and do not equate
                // the physical P end with the legal paragraph's end.
                let next = writing_starts.partition_point(|&(start, _)| start < markup_end);
                if let Some((_, start)) = writing_starts[next..]
                    .iter()
                    .find(|&&(_, start)| start >= paragraph_end && start > node.range.start)
                {
                    node.range.end = node.range.end.min(*start);
                }
            }
        }
    }
}

fn courtlistener_source_parents(structure: &mut DocumentStructure) {
    let mut owners = HashMap::<usize, Option<usize>>::new();
    for (index, node) in structure.nodes.iter().enumerate() {
        if node.source == Derivation::Native && node.kind != NodeKind::Page {
            if let Some(start) = node.markup_start {
                owners
                    .entry(start)
                    .and_modify(|owner| *owner = None)
                    .or_insert(Some(index));
            }
        }
    }
    let mut parents = Vec::new();
    for (index, node) in structure.nodes.iter().enumerate() {
        if node.source != Derivation::Native || node.kind == NodeKind::Page {
            continue;
        }
        let Some(element_index) = node.markup_start.and_then(|start| {
            structure
                .markup_elements
                .binary_search_by_key(&start, |element| element.markup_start)
                .ok()
        }) else {
            continue;
        };
        let range = structure.query_range(node);
        let mut parent = structure.markup_elements[element_index].parent;
        while let Some(element_index) = parent {
            let element = &structure.markup_elements[element_index];
            if let Some(owner) = owners.get(&element.markup_start) {
                if let Some(owner) = owner {
                    let owner_range = structure.query_range(&structure.nodes[*owner]);
                    if owner_range.start <= range.start && owner_range.end >= range.end {
                        let id = &structure.nodes[*owner].id;
                        if node.parent_id.as_ref() != Some(id) {
                            parents.push((index, id.clone()));
                        }
                    }
                }
                // A nearer typed owner is a boundary even if its body excludes
                // the child's printed marker or its source identity is ambiguous.
                break;
            }
            parent = element.parent;
        }
    }
    for (index, parent) in parents {
        structure.nodes[index].parent_id = Some(parent);
    }
}

pub struct CourtListenerSourceMarkup<'a> {
    pub kind: &'static str,
    pub markup_start: usize,
    pub markup: &'a str,
}

/// Exact served headmatter and opinion slices with original UTF-8 byte starts. Parsing
/// serialized HTML back through an HTML5 DOM can discard a leading PRE newline;
/// these slices preserve the browser serialization's original text bytes.
pub fn courtlistener_source_markups(
    markup: &str,
) -> Result<Vec<CourtListenerSourceMarkup<'_>>, EngineError> {
    let rendered = render_markup(DocumentProvider::CourtListener, markup, &[])?;
    Ok(rendered
        .elements
        .iter()
        .filter_map(|element| {
            let kind = if element
                .attributes
                .get("id")
                .is_some_and(|id| id == "headmatter")
            {
                "headmatter"
            } else if element.attributes.get("class").is_some_and(|classes| {
                classes
                    .split_ascii_whitespace()
                    .any(|class| class == "subopinion-content")
            }) {
                "opinion"
            } else {
                return None;
            };
            Some(CourtListenerSourceMarkup {
                kind,
                markup_start: element.markup_start,
                markup: &markup[element.markup_start..element.markup_end],
            })
        })
        .collect())
}

pub fn analyze_native_markup(input: NativeMarkupInput) -> Result<DocumentStructure, EngineError> {
    let (
        evidence,
        cited_authorities,
        note_references,
        mut markup_tags,
        elements,
        markup_sha256,
        markup_ancestors,
        matched_role_starts,
        printed_notes,
    ) = native_markup_evidence(input)?;
    // Retain the existing inference exclusions for the source-aware supplement,
    // in the final UTF-16 coordinates used by the retained markup graph.
    let (heading_exclusions, body_exclusions) = if evidence.provider == "courtlistener" {
        let coordinates = ScalarText::new(&evidence.text);
        let mut headings = Vec::new();
        let mut body = Vec::new();
        for exclusion in &evidence.exclusions {
            let range = ScalarRange {
                start: coordinates.utf16(exclusion.range.start),
                end: coordinates.utf16(exclusion.range.end),
            };
            if exclusion.applies_to.iter().any(|kind| kind == "heading") {
                headings.push(range);
            }
            if exclusion.applies_to.iter().any(|kind| kind == "paragraph") {
                body.push(range);
            }
        }
        (headings, body)
    } else {
        (Vec::new(), Vec::new())
    };
    let mut structure = derive_trusted(evidence)?;
    structure.cited_authorities = cited_authorities;
    if structure.provider == "courtlistener" {
        structure.markup_elements = elements;
        structure.markup_ancestors = markup_ancestors;
        structure.markup_sha256 = Some(markup_sha256);
    }
    let sibling_note_markers =
        courtlistener_sibling_note_markers(&structure.markup_elements, structure.query_text());
    for node in &mut structure.nodes {
        if let Some((tag, metadata)) = markup_tags.remove(&node.id) {
            if tag == "li" || (node.kind == NodeKind::List && matches!(tag.as_str(), "dt" | "dd")) {
                node.kind = NodeKind::ListItem;
            }
            node.markup_tag = Some(tag);
            node.markup_start = metadata.start;
            node.markup_attributes = metadata.attributes;
            if node.kind == NodeKind::Footnote {
                node.marker_range = metadata.note_marker.or_else(|| {
                    node.markup_start
                        .and_then(|start| sibling_note_markers.get(&start).map(|(_, range)| *range))
                });
            }
            if node.kind == NodeKind::Cell {
                node.row_span = node
                    .markup_attributes
                    .get("rowspan")
                    .and_then(|value| value.parse().ok());
                node.column_span = node
                    .markup_attributes
                    .get("colspan")
                    .and_then(|value| value.parse().ok());
            }
        }
    }
    if structure.provider == "courtlistener" {
        prefer_source_headings(&mut structure);
        prefer_source_note_markers(&mut structure);
        clip_courtlistener_paragraphs_to_opinions(&mut structure, &matched_role_starts);
        native_table_coordinates(&mut structure);
        let mut references = HashMap::<String, Vec<NoteReference>>::new();
        for (target, anchor, range) in note_references {
            references.entry(target).or_default().push(NoteReference {
                range,
                anchor,
                page_indexes: Vec::new(),
                line_ids: Vec::new(),
            });
        }
        let mut anchor_counts = HashMap::<&str, usize>::new();
        for node in &structure.nodes {
            if let Some(anchor) = node.anchor.as_deref() {
                *anchor_counts.entry(anchor).or_default() += 1;
            }
        }
        let mut derived_notes = std::mem::take(&mut structure.notes)
            .into_iter()
            .filter_map(|note| note.node_id.clone().map(|id| (id, note)))
            .collect::<HashMap<_, _>>();
        structure.notes = structure
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Footnote)
            .map(|node| {
                if let Some(note) = derived_notes.remove(&node.id) {
                    return note;
                }
                let references = node
                    .anchor
                    .as_deref()
                    .filter(|anchor| anchor_counts.get(anchor) == Some(&1))
                    .and_then(|anchor| references.remove(anchor))
                    .unwrap_or_default();
                Note {
                    id: format!("note-{}", node.id),
                    node_id: Some(node.id.clone()),
            ownership: node.ownership.clone(),
                    kind: NoteKindV2::Footnote,
                    // Attribute-only labels have no rendered span; a source
                    // label or sibling number retains its separate range.
                    label_range: node.marker_range.unwrap_or(ScalarRange {
                        start: node.range.start,
                        end: node.range.start,
                    }),
                    body_range: node.range,
                    label_ranges: node.marker_range.into_iter().filter(|range| range.start < range.end).collect(),
                    body_ranges: if node.ranges.is_empty() { vec![node.range] } else { node.ranges.clone() },
                    primary_reference: references.first().map(|reference| reference.range),
                    references,
                }
            })
            .collect();
        supplement_courtlistener_bookmarked_notes(&mut structure);
        supplement_courtlistener_declared_note_references(&mut structure, &matched_role_starts);
        clip_courtlistener_pre_note_bodies(&mut structure);
        crate::inference::supplement_source_formatted_headings(
            &mut structure,
            &heading_exclusions,
            &body_exclusions,
        );
        supplement_courtlistener_printed_notes(&mut structure, printed_notes);
        courtlistener_source_parents(&mut structure);
    }
    Ok(structure)
}

/// A LégisQuébec consolidation page (its HTML) as statute text in the form a provider's statute
/// text takes, for [`crate::statute_outline`]: each heading on a line of its own opened by "#"s for
/// its level ("## PART I HUMAN RIGHTS AND FREEDOMS"), each article and each of its paragraphs on
/// lines of their own ("2. Every human being ..."), without the page's navigation, hidden
/// metadata, history links and amendment notes.
pub fn legisquebec_statute_text(markup: &str) -> String {
    const SKIPPED: [&str; 4] = ["Hidden", "HistoricalNote", "HistoryLink", "modal"];
    struct Open {
        tag: String,
        skipped: bool,
        heading: bool,
        body: bool,
    }
    let (mut text, mut position) = (String::new(), 0usize);
    let mut stack = Vec::<Open>::new();
    let (mut skipping, mut in_body) = (0usize, false);
    // The heading being read: its level (from its label's group) and its words.
    let mut heading: Option<(usize, String)> = None;
    let mut attributes = Attributes::new();
    let mut at = 0;
    while at < markup.len() {
        if markup.as_bytes()[at] != b'<' {
            let end = markup[at..].find('<').map_or(markup.len(), |end| at + end);
            if in_body && skipping == 0 {
                let rendered = normalize_javascript_whitespace(&decode_entities(&markup[at..end]));
                match heading.as_mut() {
                    Some((_, words)) => {
                        if !words.is_empty() && !rendered.is_empty() && !words.ends_with(' ') {
                            words.push(' ');
                        }
                        words.push_str(&rendered);
                    }
                    None => append_text(&mut text, &mut position, &rendered),
                }
            }
            at = end;
            continue;
        }
        if markup[at..].starts_with("<!--") {
            at = markup[at + 4..]
                .find("-->")
                .map_or(markup.len(), |end| at + 4 + end + 3);
            continue;
        }
        let Some(relative_end) = markup[at..].find('>') else {
            at += 1;
            continue;
        };
        let raw = &markup[at..at + relative_end + 1];
        at += relative_end + 1;
        if let Some((tag, _)) = parse_tag(raw, true) {
            let Some(index) = stack.iter().rposition(|open| open.tag == tag) else {
                continue;
            };
            for open in stack.drain(index..).rev() {
                skipping -= usize::from(open.skipped);
                in_body &= !open.body;
                if open.heading {
                    if let Some((level, words)) = heading.take() {
                        let words = normalize_javascript_whitespace(&words);
                        if !words.is_empty() {
                            append_break(&mut text, &mut position);
                            append_text(
                                &mut text,
                                &mut position,
                                &format!("{} {words}", "#".repeat(level + 2)),
                            );
                        }
                    }
                }
            }
            if matches!(tag.as_str(), "div" | "p") && heading.is_none() {
                append_break(&mut text, &mut position);
            }
            continue;
        }
        let Some((tag, attrs)) = parse_tag(raw, false) else {
            continue;
        };
        let closed = attrs.trim_end_matches(javascript_whitespace).ends_with('/');
        parse_attributes(
            attrs
                .trim_end_matches(javascript_whitespace)
                .trim_end_matches('/'),
            &mut attributes,
        );
        let class = attribute(&attributes, "class");
        if tag == "br" && in_body && skipping == 0 && heading.is_none() {
            append_break(&mut text, &mut position);
        }
        if closed || void_tag(&tag) {
            continue;
        }
        let body = !in_body && contains_ascii_word(class, "LegislativeDocument", false);
        in_body |= body;
        let skipped = in_body
            && SKIPPED
                .iter()
                .any(|name| contains_ascii_word(class, name, false));
        skipping += usize::from(skipped);
        // A heading's label names its group ("Label-group4": a chapter); a schedule's is a top one.
        if let Some(group) = class
            .split_whitespace()
            .find_map(|name| name.strip_prefix("Label-group"))
        {
            if let Some((level, _)) = heading.as_mut() {
                *level = group
                    .parse::<usize>()
                    .map_or(0, |group| group.saturating_sub(1).min(3));
            }
        }
        let opens_heading = in_body
            && skipping == 0
            && heading.is_none()
            && (contains_ascii_word(class, "Heading", false)
                || contains_ascii_word(class, "ScheduleHeading", false));
        if opens_heading {
            heading = Some((0, String::new()));
        } else if matches!(tag.as_str(), "div" | "p")
            && in_body
            && skipping == 0
            && heading.is_none()
        {
            append_break(&mut text, &mut position);
        }
        stack.push(Open {
            tag,
            skipped,
            heading: opens_heading,
            body,
        });
    }
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn courtlistener_ignores_page_markers_inside_footnotes() {
        let markup = r#"
            <p><span class="star-pagination" label="200">*200</span>Body</p>
            <p><span class="star-pagination" label="201">*201</span>More</p>
            <div class="footnotes"><div class="footnote" id="fn1" label="1">
              <p><span class="star-pagination" label="200">*200</span>Note</p>
            </div></div>
        "#;
        let rendered = render_markup(DocumentProvider::CourtListener, markup, &[]).unwrap();
        let pages = rendered
            .blocks
            .iter()
            .filter(|block| block.kind == Kind::Page)
            .map(|block| block.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(pages, ["page200", "page201"]);
    }

    #[test]
    fn cited_authorities_preserve_source_order_and_javascript_parity() {
        let rendered = render_markup(
            DocumentProvider::Tna,
            r#"<p><ref uk:canonical="[2016] UKSC 11" uk:type="case">Patel <em>v</em> Mirza &amp;lt;&amp;#160;Co</ref><ref uk:canonical="[2016] uksc 11">duplicate</ref><ref uk:canonical="1996&#32;c.&#32;18" uk:type="legislation"></ref></p>"#,
            &[],
        )
        .unwrap();
        assert_eq!(normalized_text("\n\n\n \n".to_owned()), "\n\n");
        assert_eq!(
            rendered.cited_authorities,
            [
                CitedAuthority {
                    citation: "Patel v Mirza < Co".to_owned(),
                    canonical: Some("[2016] UKSC 11".to_owned()),
                    kind: Some("case".to_owned()),
                },
                CitedAuthority {
                    citation: "1996 c. 18".to_owned(),
                    canonical: Some("1996 c. 18".to_owned()),
                    kind: Some("legislation".to_owned()),
                },
            ]
        );
    }

    #[test]
    #[cfg(feature = "document-query")]
    fn provider_blocks_keep_rendered_utf16_offsets_across_astral_text() {
        let document = analyze_native_markup(NativeMarkupInput {
            provider: "courtlistener".to_owned(),
            id: "astral".to_owned(),
            url: None,
            text: String::new(),
            markup: Some("<p id=\"paragraph-1\">\u{1f9ab}e\u{301}</p>".to_owned()),
            citation: None,
            page_citations: Vec::new(),
            scope: default_scope(),
        })
        .unwrap();
        assert_eq!(document.query_text(), "\u{1f9ab}e\u{301}");
        let paragraph = crate::DocumentQuery::new()
            .blocks(&document, Some(DocumentKind::Paragraph))
            .next()
            .expect("provider paragraph");
        assert_eq!((paragraph.start, paragraph.end), (0, 4));
    }
}
