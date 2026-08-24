use crate::text::{
    javascript_whitespace, normalize_javascript_whitespace, trim_javascript_whitespace as trim_js,
};
use crate::{
    derive::derive_trusted, utf16_len, CitedAuthority, CoverageState, DetectionProfile,
    DocumentInput, DocumentStructure, EngineError, EvidenceKind, Exclusion, NativeClaim, Origin,
    ScalarRange, ScalarText, Scope, ScopeKind,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::HashSet;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NativeMarkupScope {
    kind: String,
    excerpt_of: Option<String>,
}

fn default_scope() -> NativeMarkupScope {
    NativeMarkupScope {
        kind: "complete".to_owned(),
        excerpt_of: None,
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
    #[serde(default = "default_scope")]
    scope: NativeMarkupScope,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Paragraph,
    Page,
    Section,
    Footnote,
}

impl Kind {
    fn evidence(self) -> EvidenceKind {
        match self {
            Self::Paragraph => EvidenceKind::Paragraph,
            Self::Page => EvidenceKind::Page,
            Self::Section => EvidenceKind::Section,
            Self::Footnote => EvidenceKind::Footnote,
        }
    }
}

struct PendingBlock {
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
    kind: Kind,
    label: String,
    start: usize,
    end: usize,
    anchor: Option<String>,
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
    start: usize,
    anchor: Option<String>,
    citation_index: Option<usize>,
    page_scheme: Option<String>,
}

struct PageStart {
    label: String,
    start: usize,
    anchor: Option<String>,
    aliases: Vec<String>,
}

struct RenderedMarkup {
    text: String,
    blocks: Vec<RawBlock>,
    exclusions: Vec<ScalarRange>,
    cited_authorities: Vec<CitedAuthority>,
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
    if !value.contains('&') {
        return Cow::Borrowed(value);
    }
    static EARLY: OnceLock<Regex> = OnceLock::new();
    static LATE: OnceLock<Regex> = OnceLock::new();
    let value = EARLY
        .get_or_init(|| Regex::new(r"(?i)&(?:nbsp|#160|amp);").unwrap())
        .replace_all(value, |captures: &regex::Captures<'_>| {
            if captures[0].eq_ignore_ascii_case("&amp;") {
                "&"
            } else {
                " "
            }
        });
    let late =
        LATE.get_or_init(|| Regex::new(r"(?i)&(?:lt|gt|quot|apos|#\d+|#x[0-9a-f]+);").unwrap());
    if !late.is_match(&value) {
        return value;
    }
    late.replace_all(&value, |captures: &regex::Captures<'_>| {
        let entity = &captures[0];
        let body = &entity[1..entity.len() - 1];
        let character = if body.eq_ignore_ascii_case("lt") {
            Some('<')
        } else if body.eq_ignore_ascii_case("gt") {
            Some('>')
        } else if body.eq_ignore_ascii_case("quot") {
            Some('"')
        } else if body.eq_ignore_ascii_case("apos") {
            Some('\'')
        } else {
            body.strip_prefix("#x")
                .or_else(|| body.strip_prefix("#X"))
                .map(|digits| (digits, 16))
                .or_else(|| body.strip_prefix('#').map(|digits| (digits, 10)))
                .and_then(|(digits, radix)| u32::from_str_radix(digits, radix).ok())
                .and_then(char::from_u32)
        };
        character.map_or_else(|| entity.to_owned(), |value| value.to_string())
    })
    .into_owned()
    .into()
}

type Attributes<'a> = Vec<(&'a str, Cow<'a, str>)>;

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
        let value = match decode_entities(&raw[start..end]) {
            Cow::Borrowed(value) => Cow::Borrowed(trim_js(value)),
            Cow::Owned(value) => Cow::Owned(trim_js(&value).to_owned()),
        };
        attributes.push((name, value));
    }
}

fn attribute<'a>(attributes: &'a Attributes<'_>, name: &str) -> &'a str {
    attributes
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_ref())
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
    if let Some(number) = numbered.or_else(|| numbered_id(id)) {
        let aliases = (marker != number && !marker.is_empty())
            .then(|| vec![marker.to_owned(), format!("footnote {marker}")])
            .unwrap_or_default();
        return Some(PendingBlock {
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
                || ID
                    .get_or_init(|| Regex::new(r"(?i)^(?:(?:fn|footnote)[_-]|fn\d|ftn\d)").unwrap())
                    .is_match(attribute(attributes, "id"))))
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
            return footnote_identity(&raw, &id, anchor);
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
    static CANLII: OnceLock<Regex> = OnceLock::new();
    CANLII
        .get_or_init(|| {
            Regex::new(r"(?i)^sec(\d{1,8}(?:[.-]\d{1,8}){0,3}(?:\([^)]+\))*)$").unwrap()
        })
        .captures(id)
        .map(|capture| PendingBlock {
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

fn render_markup<'a>(
    provider: DocumentProvider,
    markup: &'a str,
    page_citations: &[String],
) -> Result<RenderedMarkup, EngineError> {
    let mut text = String::new();
    let mut blocks = Vec::new();
    let mut open = Vec::<PendingBlock>::new();
    let mut tag_stack = Vec::<String>::new();
    let mut open_excluded = Vec::<OpenRange>::new();
    let mut unlabelled_footnotes = Vec::<OpenRange>::new();
    let mut exclusions = Vec::<ScalarRange>::new();
    let mut open_cited_authorities = Vec::<(usize, CitedAuthority)>::new();
    let mut cited_authorities = Vec::<CitedAuthority>::new();
    let mut cited_authority_keys = HashSet::<String>::new();
    let mut text_page = None::<TextPage>;
    let mut position = 0;
    let mut harvard_casebody = false;
    let mut page_starts = Vec::<PageStart>::new();
    let mut attributes = Attributes::new();
    let mut at = 0;
    while at < markup.len() {
        if markup.as_bytes()[at] != b'<' {
            let end = markup[at..].find('<').map_or(markup.len(), |end| at + end);
            let raw = &markup[at..end];
            let decoded = decode_entities(raw);
            for (_, authority) in &mut open_cited_authorities {
                authority.citation.push_str(&decoded);
            }
            let rendered = normalize_javascript_whitespace(&decoded);
            if let Some(pending) = unlabelled_footnotes.last().cloned() {
                if !rendered.is_empty() {
                    unlabelled_footnotes.pop();
                    if let Some(marker) = leading_footnote_marker(&rendered) {
                        if let Some(mut identity) = footnote_identity(marker, "", None) {
                            identity.tag = pending.tag;
                            identity.start = pending.start;
                            open.push(identity);
                        }
                    }
                }
            }
            let label = page_value(&rendered);
            if text_page.is_some() && label.bytes().any(|byte| byte.is_ascii_digit()) {
                let pending = text_page.take().unwrap();
                let numeric = label.len() <= 5 && label.bytes().all(|byte| byte.is_ascii_digit());
                page_starts.push(PageStart {
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
                    aliases: page_aliases(
                        &label,
                        pending.citation_index,
                        page_citations,
                        pending.page_scheme.as_deref(),
                    ),
                });
            }
            append_text(&mut text, &mut position, &rendered);
            at = end;
            continue;
        }
        if markup[at..].starts_with("<!--") {
            if let Some(end) = markup[at + 4..].find("-->") {
                at += 4 + end + 3;
                continue;
            }
        }
        if markup[at..].starts_with("<![CDATA[") {
            if let Some(end) = markup[at + 9..].find("]]>") {
                let value = &markup[at + 9..at + 9 + end];
                append_text(&mut text, &mut position, value);
                at += 9 + end + 3;
                continue;
            }
        }
        let Some(relative_end) = markup[at..].find('>') else {
            at += 1;
            continue;
        };
        let end = at + relative_end + 1;
        let raw = &markup[at..end];
        if let Some((tag, _)) = parse_tag(raw, true) {
            if let Some(depth) = tag_stack.iter().rposition(|value| value == &tag) {
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
                if let Some(index) = unlabelled_footnotes
                    .iter()
                    .rposition(|entry| entry.tag == tag && entry.depth == depth)
                {
                    unlabelled_footnotes.remove(index);
                }
                tag_stack.truncate(depth);
            }
            if tag == "span" {
                text_page = None;
            }
            if let Some(index) = open.iter().rposition(|entry| entry.tag == tag) {
                let pending = open.remove(index);
                if position > pending.start {
                    blocks.push(RawBlock {
                        kind: pending.kind,
                        label: pending.label,
                        start: pending.start,
                        end: position,
                        anchor: pending.anchor,
                        aliases: pending.aliases,
                        parent_label: pending.parent_label,
                    });
                }
            }
            if break_tag(&tag) {
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
        let depth = tag_stack.len();
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
            if footnote_body && identity.is_none() {
                unlabelled_footnotes.push(OpenRange {
                    tag: tag.clone(),
                    depth,
                    start: position,
                });
            }
        }
        let in_footnote =
            !open_excluded.is_empty() || open.iter().any(|entry| entry.kind == Kind::Footnote);
        if matches!(provider, DocumentProvider::CourtListener)
            && !self_closing
            && tag == "span"
            && contains_ascii_word(attribute(&attributes, "class"), "star-pagination", false)
            && identity
                .as_ref()
                .is_none_or(|value| value.kind != Kind::Page)
            && !in_footnote
        {
            let (citation_index, page_scheme) = page_citation_identity(&attributes);
            text_page = Some(TextPage {
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
            && !in_footnote
        {
            let identity = identity.take().unwrap();
            if !identity.inline {
                append_break(&mut text, &mut position);
            }
            let page_label = identity.page_label.as_deref().unwrap_or_default();
            page_starts.push(PageStart {
                label: identity.label,
                start: position,
                anchor: identity.anchor,
                aliases: page_aliases(
                    page_label,
                    identity.citation_index,
                    page_citations,
                    identity.page_scheme.as_deref(),
                ),
            });
        } else if let Some(mut identity) = identity.filter(|value| value.kind != Kind::Page) {
            append_break(&mut text, &mut position);
            identity.tag = tag.clone();
            identity.start = position;
            identity.parent_label = open
                .iter()
                .rev()
                .find(|entry| entry.kind == identity.kind)
                .map(|entry| entry.label.clone());
            open.push(identity);
        }
        if tag == "br" {
            append_break(&mut text, &mut position);
        }
        at = end;
    }
    let mut normalized = normalized_text(text);
    let leading = normalized.len() - normalized.trim_start_matches(javascript_whitespace).len();
    let leading_trim = utf16_len(&normalized[..leading]);
    let trimmed = trim_js(&normalized);
    let trailing = leading + trimmed.len();
    let text_utf16 = utf16_len(trimmed);
    let raw_end = leading_trim + text_utf16;
    for pending in open_excluded {
        if raw_end > pending.start {
            exclusions.push(ScalarRange {
                start: pending.start,
                end: raw_end,
            });
        }
    }
    for pending in open {
        if raw_end > pending.start {
            blocks.push(RawBlock {
                kind: pending.kind,
                label: pending.label,
                start: pending.start,
                end: raw_end,
                anchor: pending.anchor,
                aliases: pending.aliases,
                parent_label: pending.parent_label,
            });
        }
    }
    let mut page_starts = page_starts.into_iter().peekable();
    while let Some(page) = page_starts.next() {
        let end = page_starts.peek().map_or(raw_end, |next| next.start);
        if end > page.start {
            blocks.push(RawBlock {
                kind: Kind::Page,
                label: page.label,
                start: page.start,
                end,
                anchor: page.anchor,
                aliases: page.aliases,
                parent_label: None,
            });
        }
    }
    // Pre-trim normalized-provider UTF-16 becomes rendered-document UTF-16;
    // leading trim is subtracted and split-surrogate positions stay invalid.
    let normalized_coordinates = ScalarText::new(&normalized);
    let normalized_utf16 = normalized_coordinates.utf16_len();
    for block in &mut blocks {
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
    Ok(RenderedMarkup {
        text: normalized,
        blocks,
        exclusions: projected_exclusions,
        cited_authorities,
        source_hash: format!("{:x}", Sha256::digest(markup.as_bytes())),
        harvard_casebody,
    })
}

fn report_start(citation: Option<&str>) -> Option<u32> {
    citation.and_then(crate::canadian_report_start)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdapterScope<'a> {
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    excerpt_of: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdapterRevision<'a> {
    provider: &'a str,
    representation_revision: &'a str,
    citation: Option<&'a str>,
    page_citations: &'a [String],
    scope: AdapterScope<'a>,
    profile: &'a str,
}

fn native_markup_evidence(
    input: NativeMarkupInput,
) -> Result<(DocumentInput, Vec<CitedAuthority>), EngineError> {
    let NativeMarkupInput {
        provider: provider_name,
        id,
        url,
        text: fallback_text,
        markup,
        citation,
        page_citations,
        scope,
    } = input;
    let provider = DocumentProvider::from_name(&provider_name)
        .ok_or_else(|| EngineError::source("unsupported native-markup provider"))?;
    let use_markup = markup
        .as_deref()
        .is_some_and(|value| !trim_js(value).is_empty());
    let rendered = use_markup
        .then(|| render_markup(provider, markup.as_deref().unwrap(), &page_citations))
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
            exclusions: Vec::new(),
            cited_authorities: Vec::new(),
            source_hash,
            harvard_casebody: false,
        }
    };
    let RenderedMarkup {
        text,
        blocks: rendered_blocks,
        exclusions: rendered_exclusions,
        cited_authorities,
        source_hash: representation_revision,
        harvard_casebody: harvard,
    } = rendered;
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
        scope: AdapterScope {
            kind: &scope.kind,
            excerpt_of: scope.excerpt_of.as_deref(),
        },
        profile,
    };
    let adapter_revision = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&adapter).map_err(EngineError::source)?)
    );
    // Exact UTF-16 in final rendered/fallback text, never source markup.
    let coordinates = ScalarText::new(&text);
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
    for (index, raw) in rendered_blocks.into_iter().enumerate() {
        let id = format!("native-{:06}", index + 1);
        let claim_end = raw.end.min(text_utf16);
        claims.push(NativeClaim {
            id,
            kind: raw.kind.evidence(),
            label: Some(raw.label),
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
    let exclusions = rendered_exclusions
        .iter()
        .map(|range| {
            Ok(Exclusion {
                range: ScalarRange {
                    start: scalar(range.start)?,
                    end: scalar(range.end)?,
                },
                applies_to: vec!["paragraph".to_owned()],
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    let scope_kind = match scope.kind.as_str() {
        "complete" => ScopeKind::Complete,
        "excerpt" => ScopeKind::Excerpt,
        _ => return Err(EngineError::source("invalid native-markup scope")),
    };
    let evidence = DocumentInput {
        document_id: id,
        provider: provider.as_str().to_owned(),
        url,
        doc_type: Some("cases"),
        profile: match profile {
            "case_contiguous_complete" => DetectionProfile::CaseContiguousComplete,
            _ => DetectionProfile::CaseLossy,
        },
        report_start_page: report_start(citation.as_deref()),
        require_report_start: false,
        allow_hyphenated_sections: false,
        text_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        text,
        source_sha256: Some(adapter_revision),
        scope: Scope {
            kind: scope_kind,
            excerpt_of: scope.excerpt_of,
        },
        origins: vec![Origin {
            id: ORIGIN.to_owned(),
        }],
        native_claims: claims,
        coverage,
        exclusions,
    };
    Ok((evidence, cited_authorities))
}

pub fn analyze_native_markup(input: NativeMarkupInput) -> Result<DocumentStructure, EngineError> {
    let (evidence, cited_authorities) = native_markup_evidence(input)?;
    let mut structure = derive_trusted(evidence)?;
    structure.cited_authorities = cited_authorities;
    Ok(structure)
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
