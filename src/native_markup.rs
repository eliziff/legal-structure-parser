use crate::text::{
    javascript_whitespace, normalize_javascript_whitespace, trim_javascript_whitespace as trim_js,
};
use crate::{
    utf16_len, CitedAuthority, Derivation, DetectionProfile,
    Evidence, DocumentStructure, EngineError, Exclusion, MarkupAncestor,
    MarkupElement, NodeKind, ScalarRange, ScalarText,
    Scope, ScopeKind, StructureNode,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

#[cfg(feature = "courtlistener")]
mod courtlistener;
#[cfg(feature = "courtlistener")]
use courtlistener::*;

const ORIGIN: &str = "provider-adapter";

#[derive(Clone, Copy)]
enum DocumentProvider {
    #[cfg(feature = "a2aj")]
    A2aj,
    #[cfg(feature = "courtlistener")]
    CourtListener,
    #[cfg(feature = "tna")]
    Tna,
    #[cfg(feature = "govinfo")]
    GovInfo,
    #[cfg(feature = "govuk-et")]
    GovUkEt,
    #[cfg(feature = "hansard")]
    Hansard,
    #[cfg(feature = "journal")]
    Journal,
    #[cfg(feature = "local-pdf")]
    LocalPdf,
}

impl DocumentProvider {
    fn as_str(self) -> &'static str {
        match self {
            #[cfg(feature = "a2aj")]
            Self::A2aj => "a2aj",
            #[cfg(feature = "courtlistener")]
            Self::CourtListener => "courtlistener",
            #[cfg(feature = "tna")]
            Self::Tna => "tna",
            #[cfg(feature = "govinfo")]
            Self::GovInfo => "govinfo",
            #[cfg(feature = "govuk-et")]
            Self::GovUkEt => "govuk-et",
            #[cfg(feature = "hansard")]
            Self::Hansard => "hansard",
            #[cfg(feature = "journal")]
            Self::Journal => "journal",
            #[cfg(feature = "local-pdf")]
            Self::LocalPdf => "local-pdf",
        }
    }

    fn from_name(value: &str) -> Option<Self> {
        Some(match value {
            #[cfg(feature = "a2aj")]
            "a2aj" => Self::A2aj,
            #[cfg(feature = "courtlistener")]
            "courtlistener" => Self::CourtListener,
            #[cfg(feature = "tna")]
            "tna" => Self::Tna,
            #[cfg(feature = "govinfo")]
            "govinfo" => Self::GovInfo,
            #[cfg(feature = "govuk-et")]
            "govuk-et" => Self::GovUkEt,
            #[cfg(feature = "hansard")]
            "hansard" => Self::Hansard,
            #[cfg(feature = "journal")]
            "journal" => Self::Journal,
            #[cfg(feature = "local-pdf")]
            "local-pdf" => Self::LocalPdf,
            _ => return None,
        })
    }

    /// CourtListener's renditions, read with its own conventions where its reader is built in.
    fn is_courtlistener(self) -> bool {
        #[cfg(feature = "courtlistener")]
        return matches!(self, Self::CourtListener);
        #[cfg(not(feature = "courtlistener"))]
        false
    }

    fn is_tna(self) -> bool {
        #[cfg(feature = "tna")]
        return matches!(self, Self::Tna);
        #[cfg(not(feature = "tna"))]
        false
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

impl NativeMarkupInput {
    /// A provider's record of one document: its text and its markup, nothing else known of it.
    pub(crate) fn of(provider: &str, id: &str, text: String, markup: String) -> Self {
        Self {
            provider: provider.to_owned(),
            id: id.to_owned(),
            url: None,
            text,
            markup: Some(markup),
            citation: None,
            page_citations: Vec::new(),
            markup_ancestors: Vec::new(),
            scope: default_scope(),
        }
    }
}

/// A node of a block the markup declares, as the reader opens it: its kind, label (none where empty) and anchor, its
/// range in the rendered text's UTF-16 until the reader puts it in scalars; the reader sets its markup on it.
fn markup_node(kind: NodeKind, label: String, anchor: Option<String>) -> StructureNode {
    let mut node = StructureNode::new(String::new(), kind, ScalarRange { start: 0, end: 0 }, ORIGIN, Derivation::Native, None);
    node.label = (!label.is_empty()).then_some(label);
    node.anchor = anchor;
    node
}

/// A node of the block an element declares: the element's anchor, start, attributes, tag and range on it.
fn element_node(kind: NodeKind, label: String, element: &MarkupElement) -> StructureNode {
    let mut node = markup_node(kind, label, element.attributes.get("id").filter(|id| !id.is_empty()).cloned());
    node.markup_start = Some(element.markup_start);
    node.markup_attributes = element.attributes.clone();
    node.markup_tag = Some(element.tag.clone());
    node.range = element.range;
    node
}

#[derive(Clone)]
struct OpenRange {
    tag: String,
    depth: usize,
    start: usize,
}

/// A star page whose label its text, still to come, prints.
struct TextPage {
    page: StructureNode,
    citation_index: Option<usize>,
    page_scheme: Option<String>,
}

#[derive(Default)]
struct RenderedMarkup {
    text: String,
    blocks: Vec<StructureNode>,
    recovered_body_blocks: Vec<StructureNode>,
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
    page_citations: &[String],
) -> Option<StructureNode> {
    let page_label = page_value(raw);
    if !page_label.bytes().any(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let numeric = page_label.len() <= 5 && page_label.bytes().all(|byte| byte.is_ascii_digit());
    let label = if numeric {
        format!("{}", page_label.parse::<u32>().ok()?)
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
    let mut page = markup_node(NodeKind::Page, label, anchor);
    page.aliases = page_aliases(&page_label, citation_index, page_citations, page_scheme.as_deref());
    Some(page)
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

fn footnote_identity(raw: &str, id: &str, anchor: Option<String>) -> Option<StructureNode> {
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
        let mut note = markup_node(NodeKind::Footnote, format!("{}", number.parse::<u32>().ok()?), anchor);
        note.aliases = (marker != number && !marker.is_empty())
            .then(|| vec![marker.to_owned(), format!("footnote {marker}")]);
        return Some(note);
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
    let label = if symbol.as_bytes().get(..2).is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"fn")) { &symbol[2..] } else { symbol }.to_owned();
    let mut note = markup_node(NodeKind::Footnote, label, anchor);
    note.aliases = (!marker.is_empty()).then(|| vec![marker.to_owned(), format!("footnote {marker}")]);
    Some(note)
}

fn native_identity(
    provider: DocumentProvider,
    tag: &str,
    attributes: &Attributes<'_>,
    courtlistener_footnote: bool,
    page_citations: &[String],
) -> Option<StructureNode> {
    let id = ["eid", "id", "name"]
        .into_iter()
        .map(|name| attribute(attributes, name))
        .find(|value| !value.is_empty())
        .unwrap_or_default();
    let anchor = (!id.is_empty()).then(|| id.to_owned());
    if tag == "a" && provider.is_courtlistener() {
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
            return page_identity(&label, attributes, anchor, page_citations);
        }
        return None;
    }
    if provider.is_courtlistener() {
        if tag == "span"
            && contains_ascii_word(attribute(attributes, "class"), "star-pagination", false)
        {
            let raw = ["label", "data-label"]
                .into_iter()
                .map(|name| attribute(attributes, name))
                .find(|value| !value.is_empty())
                .unwrap_or_default()
                .to_owned();
            return page_identity(&raw, attributes, anchor, page_citations);
        }
        if courtlistener_footnote {
            let raw = ["data-label", "label", "n"]
                .into_iter()
                .map(|name| attribute(attributes, name))
                .find(|value| !value.is_empty())
                .unwrap_or_default()
                .to_owned();
            return Some(
                footnote_identity(&raw, &id, anchor.clone())
                    .unwrap_or_else(|| markup_node(NodeKind::Footnote, String::new(), anchor)),
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
        return page_identity(&raw, attributes, anchor, page_citations);
    }
    static PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    static DIV_PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    let paragraph = PARAGRAPH
        .get_or_init(|| Regex::new(r"(?i)^(?:para(?:graph)?)[_-]?(\d{1,5})$").unwrap())
        .captures(id)
        .map(|capture| capture[1].to_owned())
        .or_else(|| {
            (provider.is_courtlistener()
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
        provider.is_tna() || provider.is_courtlistener()
    }) {
        return Some(markup_node(NodeKind::Paragraph, format!("{}", paragraph.parse::<u32>().ok()?), anchor));
    }
    // Source positions identify unanchored structure without inventing browser IDs.
    let source_kind = match tag {
        "p" | "blockquote" | "author" | "judges" | "attorneys" => Some(NodeKind::Prose),
        // CAP's own front-matter blocks, each with its source id, and any block its data-type names as one.
        "syllabus" | "summary" | "parties" | "docketnumber" | "decisiondate" | "otherdate" | "court" | "history"
        | "disposition" | "seealso" | "correction" | "arguments"
            if !id.is_empty() =>
        {
            Some(NodeKind::Prose)
        }
        #[cfg(feature = "courtlistener")]
        _ if !matches!(tag, "opinion" | "headnotes" | "casebody") && cap_front_matter(attribute(attributes, "data-type")).is_some_and(|field| field != "headnotes") => {
            Some(NodeKind::Prose)
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "heading" | "h" => Some(NodeKind::Heading),
        "opinion" | "headnotes" | "casebody" => Some(NodeKind::Section),
        "div"
            if attribute(attributes, "class")
                .split_ascii_whitespace()
                .any(|class| class == "co_headtext") =>
        {
            Some(NodeKind::Heading)
        }
        "div"
            if attribute(attributes, "class")
                .split_ascii_whitespace()
                .any(|class| class == "co_paragraph") =>
        {
            Some(NodeKind::Prose)
        }
        "div"
            if attribute(attributes, "class")
                .split_ascii_whitespace()
                .any(|class| matches!(class, "co_opinionBlock" | "co_section")) =>
        {
            Some(NodeKind::Section)
        }
        "div" if id == "headmatter" => Some(NodeKind::Section),
        "article" | "section" | "div"
            if ["opinion", "casebody", "head-matter"].iter().any(|class| {
                attribute(attributes, "class")
                    .split_ascii_whitespace()
                    .any(|value| value == *class)
            }) =>
        {
            Some(NodeKind::Section)
        }
        "ul" | "ol" | "li" => Some(NodeKind::List),
        "table" => Some(NodeKind::Table),
        "tr" => Some(NodeKind::Row),
        "td" | "th" => Some(NodeKind::Cell),
        _ => None,
    };
    if let Some(kind) = source_kind.filter(|_| provider.is_courtlistener())
    {
        return Some(markup_node(kind, id.to_owned(), anchor));
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
            return Some(markup_node(NodeKind::Section, format!("sec{section}"), anchor));
        }
    }
    static SECTION_ID: OnceLock<Regex> = OnceLock::new();
    SECTION_ID
        .get_or_init(|| {
            Regex::new(r"(?i)^sec(\d{1,8}(?:[.-]\d{1,8}){0,3}(?:\([^)]+\))*)$").unwrap()
        })
        .captures(id)
        .map(|capture| markup_node(NodeKind::Section, format!("sec{}", &capture[1]), anchor))
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
) -> Option<Vec<String>> {
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
    (!aliases.is_empty()).then_some(aliases)
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

fn observe_footnote_marker(open: &mut [StructureNode], text: &str, seen: &mut HashSet<usize>) {
    let Some(note) = open
        .iter_mut()
        .rev()
        .find(|entry| entry.kind == NodeKind::Footnote)
    else {
        return;
    };
    let Some(start) = note.markup_start else {
        return;
    };
    if note.label.is_none() && !trim_js(text).is_empty() && seen.insert(start) {
        if let Some(identity) =
            leading_footnote_marker(text).and_then(|marker| footnote_identity(marker, "", None))
        {
            note.label = identity.label;
            note.aliases = identity.aliases;
        }
    }
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
    let mut open = Vec::<StructureNode>::new();
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
    // Each page's node as it opens, with the end and owner of the opinion it is printed in.
    let mut page_starts = Vec::<(StructureNode, Option<(usize, String)>)>::new();
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
                let TextPage { mut page, citation_index, page_scheme } = text_page.take().unwrap();
                let numeric = label.len() <= 5 && label.bytes().all(|byte| byte.is_ascii_digit());
                page.label = Some(if numeric {
                    format!("{}", label.parse::<u32>().map_err(EngineError::source)?)
                } else {
                    format!("{}", label.chars().filter(|value| !javascript_whitespace(*value)).collect::<String>())
                });
                page.aliases = page_aliases(&label, citation_index, page_citations, page_scheme.as_deref());
                page_starts.push((page, None));
            }
            if provider.is_courtlistener() {
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
                if provider.is_courtlistener() {
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
                if provider.is_courtlistener() {
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
            if provider.is_courtlistener() {
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
                        let label = element.attributes.get("id").cloned().unwrap_or_default();
                        let mut body = element_node(NodeKind::Section, label, element);
                        body.markup_tag = Some(tag.clone());
                        body.range.end = position;
                        recovered_body_blocks.push(body);
                    }
                }
            }
            if tag == "span" {
                text_page = None;
            }
            // Typed owners close with their actual source element, including
            // active descendants when the source closes an enclosing element.
            let closing = if provider.is_courtlistener() {
                closing_source_start.and_then(|start| {
                    open.iter().position(|entry| entry.markup_start.is_some_and(|position| position >= start))
                })
            } else {
                open.iter().rposition(|entry| entry.markup_tag.as_deref() == Some(tag.as_str()))
            };
            if let Some(index) = closing {
                let stop = if provider.is_courtlistener() {
                    open.len()
                } else {
                    index + 1
                };
                for mut pending in open.drain(index..stop).rev() {
                    if position > pending.range.start
                        || (provider.is_courtlistener() && pending.kind != NodeKind::Page)
                    {
                        pending.range.end = position;
                        blocks.push(pending);
                    }
                }
            }
            if break_tag(&tag)
                && !(provider.is_courtlistener() && tag == "content")
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
        if provider.is_courtlistener() && tag == "pre" && !self_closing {
            // Preserve the inline source seam without creating a PRE wrapper
            // break or charging preceding collapsed whitespace to its range.
            flush_inline_whitespace(&mut text, &mut position, &mut inline_whitespace);
            ignore_pre_linefeed = matches!(markup_serialization, MarkupSerialization::HtmlSource);
        }
        let depth = tag_stack.len();
        if provider.is_courtlistener() && !self_closing {
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
        if provider.is_courtlistener() && tag == "a" && !self_closing {
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
        let mut identity = native_identity(provider, &tag, &attributes, footnote_body, page_citations);
        if provider.is_courtlistener() {
            if let Some(identity) = &mut identity {
                identity.markup_start = Some(at);
                identity.markup_attributes = attributes
                    .iter()
                    .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
                    .collect();
            }
        }
        if !self_closing {
            tag_stack.push(tag.clone());
            static FOOTNOTE_CONTAINER_ID: OnceLock<Regex> = OnceLock::new();
            if provider.is_courtlistener()
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
        if provider.is_courtlistener()
            && !self_closing
            && tag == "span"
            && contains_ascii_word(attribute(&attributes, "class"), "star-pagination", false)
            && identity
                .as_ref()
                .is_none_or(|value| value.kind != NodeKind::Page)
        {
            let (citation_index, page_scheme) = page_citation_identity(&attributes);
            let id = attribute(&attributes, "id");
            let mut page = markup_node(NodeKind::Page, String::new(), (!id.is_empty()).then(|| id.to_owned()));
            page.markup_start = Some(at);
            page.markup_attributes = attributes
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.to_string()))
                .collect();
            page.markup_tag = Some("span".to_owned());
            page.range.start = position;
            text_page = Some(TextPage { page, citation_index, page_scheme });
        }
        if identity
            .as_ref()
            .is_some_and(|value| value.kind == NodeKind::Page)
        {
            let mut page = identity.take().unwrap();
            // (A page-number element is a line of its own; CourtListener's page labels stand in the text's line.)
            if tag == "page-number" {
                append_break(&mut text, &mut position);
            }
            page.range.start = position;
            page.markup_tag = Some(tag.clone());
            page_starts.push((page, None));
        } else if let Some(mut identity) = identity.filter(|value| value.kind != NodeKind::Page) {
            append_break(&mut text, &mut position);
            identity.markup_tag = Some(tag.clone());
            identity.range.start = position;
            identity.parent_id = open
                .iter()
                .rev()
                .find(|entry| {
                    if provider.is_courtlistener() {
                        entry.kind != NodeKind::Page
                    } else if matches!(identity.kind, NodeKind::Prose | NodeKind::Heading) {
                        matches!(entry.kind, NodeKind::Prose | NodeKind::Footnote | NodeKind::Section)
                    } else {
                        entry.kind == identity.kind
                    }
                })
                .and_then(|entry| {
                    entry.markup_start.map_or_else(|| entry.label.clone(), |start| Some(format!("native-markup-{start}")))
                });
            if self_closing
                && provider.is_courtlistener()
                && identity.kind != NodeKind::Page
            {
                identity.range.end = position;
                blocks.push(identity);
            } else {
                open.push(identity);
            }
        }
        if tag == "br" {
            append_break(&mut text, &mut position);
        }
        if provider.is_courtlistener() {
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
    let mut normalized = if provider.is_courtlistener() {
        text
    } else {
        normalized_text(text)
    };
    if provider.is_courtlistener() {
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
                    || page_starts.iter().any(|(page, _)| page.markup_start == Some(parent.markup_start))
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
            let mut page = element_node(NodeKind::Page, format!("{}", label.parse::<u32>().map_err(EngineError::source)?), parent);
            page.aliases = accessible_span.then(|| vec![format!("*{label}")]);
            page_starts.push((page, scope));
        }
        for (index, element) in elements.iter().enumerate() {
            if element.tag != "span"
                || elements
                    .get(index + 1)
                    .is_some_and(|next| next.parent == Some(index))
                || page_starts.iter().any(|(page, _)| page.markup_start == Some(element.markup_start))
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
            let mut page = element_node(NodeKind::Page, format!("{}", label.parse::<u32>().map_err(EngineError::source)?), element);
            page.aliases = Some(vec![printed.to_owned()]);
            page_starts.push((page, Some(scope)));
        }
        for element in elements
            .iter()
            .filter(|element| element.tag == "page_number")
        {
            if !matched_role_starts.contains(&element.markup_start)
                || page_starts.iter().any(|(page, _)| page.markup_start == Some(element.markup_start))
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
            let page = element_node(NodeKind::Page, format!("{}", label.parse::<u32>().map_err(EngineError::source)?), element);
            page_starts.push((page, courtlistener_opinion_page_scope(&elements, element)));
        }
        if page_starts.len() != original_page_starts {
            page_starts.sort_by_key(|(page, _)| page.range.start);
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
    for mut pending in open {
        if raw_end > pending.range.start
            || (provider.is_courtlistener() && pending.kind != NodeKind::Page)
        {
            pending.range.end = raw_end.max(pending.range.start);
            blocks.push(pending);
        }
    }
    let note_ends = blocks
        .iter()
        .filter(|block| block.kind == NodeKind::Footnote)
        .filter_map(|block| Some((block.markup_start?, block.range.end)))
        .collect::<HashMap<_, _>>();
    let page_ends = {
        let mut next_by_track = HashMap::new();
        let mut ends = vec![raw_end; page_starts.len()];
        for (index, (page, _)) in page_starts.iter().enumerate().rev() {
            let scheme = page
                .markup_attributes
                .get("pagescheme")
                .filter(|scheme| !scheme.is_empty())
                .map(String::as_str);
            let citation_index = ["citation-index", "data-citation-index"]
                .into_iter()
                .find_map(|name| page.markup_attributes.get(name)?.parse::<usize>().ok())
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
                        page.range.start,
                    )
                    .unwrap_or(raw_end)
            } else {
                page_starts
                    .get(index + 1)
                    .map_or(raw_end, |(next, _)| next.range.start)
            };
        }
        ends
    };
    for ((mut page, scope), end) in page_starts.into_iter().zip(page_ends) {
        let (end, scope_parent) = scope.map_or((end, None), |(scope_end, id)| {
            (end.min(scope_end), Some(id))
        });
        if end > page.range.start {
            // Literal elements retain source-byte order. A note-contained page
            // belongs to its nearest declared Footnote, including repeated
            // reporter labels; its body cannot continue into a sibling note.
            let mut parent = page
                .markup_start
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
            let (end, parent_id) = note_owner.map_or((end, scope_parent), |(note_end, id)| {
                (end.min(note_end), Some(id))
            });
            page.range.end = end;
            page.parent_id = parent_id;
            blocks.push(page);
        }
    }
    // Pre-trim normalized-provider UTF-16 becomes rendered-document UTF-16;
    // leading trim is subtracted and split-surrogate positions stay invalid.
    let normalized_coordinates = ScalarText::new(&normalized);
    let normalized_utf16 = normalized_coordinates.utf16_len();
    for block in blocks.iter_mut().chain(&mut recovered_body_blocks) {
        // Empty source structure remains a point in the trimmed document.
        if provider.is_courtlistener() && block.kind != NodeKind::Page {
            block.range.start = block.range.start.clamp(leading_trim, raw_end);
            block.range.end = block.range.end.clamp(block.range.start, raw_end);
        }
        let start = block.range.start.checked_sub(leading_trim).ok_or_else(|| {
            EngineError::source("native block starts before trimmed provider text")
        })?;
        let end = block
            .range
            .end
            .checked_sub(leading_trim)
            .ok_or_else(|| EngineError::source("native block ends before trimmed provider text"))?;
        if start > text_utf16 || start > end {
            return Err(EngineError::source(
                "native block has an invalid provider UTF-16 range",
            ));
        }
        if end > text_utf16 {
            if block.range.end <= raw_end || block.range.end > normalized_utf16 {
                return Err(EngineError::source(
                    "native block has an invalid trim overhang",
                ));
            }
            let from = normalized_coordinates
                .byte_at_utf16(raw_end)
                .ok_or_else(|| EngineError::source("UTF-16 range splits a Unicode scalar"))?;
            let until = normalized_coordinates
                .byte_at_utf16(block.range.end)
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
        block.range = ScalarRange { start, end };
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
    profile: DetectionProfile,
}

pub fn analyze_native_markup(input: NativeMarkupInput) -> Result<DocumentStructure, EngineError> {
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
    let mut rendered = if let Some(mut rendered) = rendered {
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
        RenderedMarkup { text: fallback_text, source_hash, ..RenderedMarkup::default() }
    };
    #[cfg(feature = "courtlistener")]
    let printed_notes = if provider.is_courtlistener() {
        courtlistener::evidence(
            markup.as_deref(), &scope, &rendered.elements, &rendered.text, &mut rendered.blocks, &mut rendered.exclusions,
            std::mem::take(&mut rendered.recovered_body_blocks), &rendered.matched_role_starts,
        )
    } else {
        courtlistener::sibling_note_labels(&rendered.elements, &rendered.text, &mut rendered.blocks);
        Vec::new()
    };
    #[cfg(not(feature = "courtlistener"))]
    let printed_notes = Vec::<(StructureNode, Vec<ScalarRange>)>::new();
    let profile = if provider.is_courtlistener() && !rendered.harvard_casebody {
        DetectionProfile::CaseContiguousComplete
    } else {
        DetectionProfile::CaseLossy
    };
    let adapter = AdapterRevision {
        provider: provider.as_str(),
        representation_revision: &rendered.source_hash,
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
    let coordinates = ScalarText::new(&rendered.text);
    let scalar = |offset: usize| {
        coordinates
            .scalar_at_utf16(offset)
            .ok_or_else(|| EngineError::source("provider UTF-16 range splits a Unicode scalar"))
    };
    let text_utf16 = coordinates.utf16_len();
    // Each block the markup declares is a node, its element's tag and attributes on it, its ranges now in scalars.
    let mut claims = std::mem::take(&mut rendered.blocks);
    for (index, node) in claims.iter_mut().enumerate() {
        node.id = node.markup_start.map_or_else(
            || format!("native-{:06}", index + 1),
            |start| format!("native-markup-{start}"),
        );
        node.range = ScalarRange { start: scalar(node.range.start)?, end: scalar(node.range.end.min(text_utf16))? };
        // (An "li" is a list item, whatever it declares: `analyze_native_markup` makes it one after derivation.)
        let listed = node.markup_tag.as_deref() == Some("li");
        node.marker_range = node.marker_range.filter(|_| node.kind == NodeKind::Footnote && !listed).and_then(|marker| {
            Some(ScalarRange { start: coordinates.scalar_at_utf16(marker.start)?, end: coordinates.scalar_at_utf16(marker.end)? })
        });
        if node.kind == NodeKind::Cell && !listed {
            let span = |name: &str| node.markup_attributes.get(name).and_then(|value| value.parse().ok());
            node.row_span = span("rowspan");
            node.column_span = span("colspan");
        }
    }
    // Where inference reads nothing, in UTF-16 as the CourtListener passes after derivation read it: in the markup's
    // notes no paragraph or heading, and in matter no heading opens in (head matter, a quotation, the parties, or all
    // of a text whose enclosing markup is such matter).
    let body_exclusions = std::mem::take(&mut rendered.exclusions);
    let mut heading_exclusions = body_exclusions.clone();
    if provider.is_courtlistener() {
        heading_exclusions.extend(
            rendered
                .elements
                .iter()
                .filter(|element| source_heading_exclusion(&element.tag, &element.attributes))
                .map(|element| element.range),
        );
        if markup_ancestors
            .iter()
            .any(|ancestor| source_heading_exclusion(&ancestor.tag, &ancestor.attributes))
        {
            heading_exclusions.push(ScalarRange { start: 0, end: text_utf16 });
        }
    }
    let exclusion = |range: &ScalarRange, applies_to: &[NodeKind]| -> Result<Exclusion, EngineError> {
        Ok(Exclusion {
            range: ScalarRange { start: scalar(range.start)?, end: scalar(range.end)? },
            applies_to: applies_to.to_vec(),
        })
    };
    let exclusions = body_exclusions
        .iter()
        .map(|range| exclusion(range, &[NodeKind::Paragraph, NodeKind::Heading]))
        .chain(heading_exclusions[body_exclusions.len()..].iter().map(|range| exclusion(range, &[NodeKind::Heading])))
        .collect::<Result<Vec<_>, EngineError>>()?;
    let scope_kind = match scope.kind.as_str() {
        "complete" => ScopeKind::Complete,
        "excerpt" => ScopeKind::Excerpt,
        _ => return Err(EngineError::source("invalid native-markup scope")),
    };
    let mut structure = DocumentStructure::new(id, provider.as_str(), rendered.text, ORIGIN);
    structure.url = url;
    structure.doc_type = Some("cases".to_owned());
    structure.source_sha256 = Some(adapter_revision);
    structure.scope = Scope { kind: scope_kind, excerpt_of: scope.excerpt_of };
    structure.nodes = claims;
    structure.cited_authorities = rendered.cited_authorities;
    if provider.is_courtlistener() {
        structure.markup_elements = rendered.elements;
        structure.markup_ancestors = markup_ancestors;
        structure.markup_sha256 = Some(rendered.source_hash);
    }
    let evidence = Evidence {
        report_start_page: report_start(citation.as_deref()),
        exclusions,
        ..Evidence::new(structure, profile)
    };
    let mut structure = crate::derive::derive(evidence, None);
    // A list's item is one, read as the kind its block declares until here.
    for node in &mut structure.nodes {
        let tag = node.markup_tag.as_deref();
        if tag == Some("li") || node.kind == NodeKind::List && matches!(tag, Some("dt" | "dd")) {
            node.kind = NodeKind::ListItem;
        }
    }
    #[cfg(not(feature = "courtlistener"))]
    let _ = (rendered.note_references, rendered.matched_role_starts, printed_notes, heading_exclusions, body_exclusions);
    #[cfg(feature = "courtlistener")]
    if provider.is_courtlistener() {
        courtlistener::finish(&mut structure, rendered.note_references, &rendered.matched_role_starts, printed_notes, &heading_exclusions, &body_exclusions);
        // (Its passes drop and re-parent nodes: the levels are the parents' as they now stand.)
        legal_structure_model::write_levels(&mut structure.nodes).ok();
    }
    structure.refresh_revision().expect("Typed document structure is serializable");
    Ok(structure)
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

fn courtlistener_footnote_body(
    provider: DocumentProvider,
    tag: &str,
    attributes: &Attributes<'_>,
) -> bool {
    if !provider.is_courtlistener() {
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

