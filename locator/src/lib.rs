//! The canonical spellings of a document's printed locators, shared by the structure engine and the queries over it,
//! neither of which can depend on the other.

use legal_text::{javascript_whitespace, trim_javascript_whitespace as js_trim, JS_WHITESPACE_CLASS as JS_WS};
use regex::Regex;
use std::sync::OnceLock;

pub fn literal_page_marker(line: &str, insensitive: bool) -> Option<&str> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let marker = line.strip_suffix(']')?;
    let prefix = marker.get(..6)?;
    ((insensitive && prefix.eq_ignore_ascii_case("[page ")) || prefix == "[page ")
        .then(|| &marker[6..])
}

pub fn compact_provision_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !javascript_whitespace(*character))
        .collect()
}

pub fn normalize_numbered_section_locator(value: &str) -> String {
    normalize_compact_numbered_section_locator(&compact_provision_label(value))
}

/// A section's place as a link names it ("sec5(2)", "secIV"), or a heading's by its title ("sectitle:general
/// provisions"), from a citation's locator ("s. 5 (2)", "section IV", "sec5(2)"); empty where it names neither.
pub fn normalize_section_locator(locator: &str) -> String {
    match section_locator(locator) {
        Some(SectionLocator::Section(label)) => format!("sec{label}"),
        Some(SectionLocator::Title(title)) => format!("sectitle:{title}"),
        None => String::new(),
    }
}

/// What a citation's section locator names: a section by its label as the structure holds it ("5(2)", "IV"), or a
/// heading by its title, its words lowercased and spaced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SectionLocator {
    Section(String),
    Title(String),
}

/// A citation's section locator read once ("s. 5 (2)", "section IV", "sec5(2)", "General Provisions").
pub fn section_locator(locator: &str) -> Option<SectionLocator> {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    static CANONICAL_PREFIX: OnceLock<Regex> = OnceLock::new();
    static HEADING: OnceLock<Regex> = OnceLock::new();
    static NON_TITLE: OnceLock<Regex> = OnceLock::new();
    let value = js_trim(locator);
    let without_prefix = PREFIX
        .get_or_init(|| {
            Regex::new(&[r"(?iu)^(?:sections?|ss?\.?)", JS_WS, "+"].concat())
                .expect("valid section prefix grammar")
        })
        .replace(value, "");
    let without_prefix = CANONICAL_PREFIX
        .get_or_init(|| {
            Regex::new(r"(?iu)^sec([\p{L}\p{N}])").expect("valid canonical section grammar")
        })
        .replace(&without_prefix, "$1");
    if let Some(label) = numbered_section_label(&compact_provision_label(&without_prefix)) {
        return Some(SectionLocator::Section(label));
    }
    let heading = without_prefix
        .trim_end_matches(|character| character == '.' || javascript_whitespace(character));
    if HEADING
        .get_or_init(|| Regex::new(r"^(?:[IVXLCDM]+|[A-Z])$").expect("valid heading grammar"))
        .is_match(heading)
    {
        return Some(SectionLocator::Section(heading.to_owned()));
    }
    let lowercase = heading.to_lowercase();
    let normalized = NON_TITLE
        .get_or_init(|| Regex::new(r"[^\p{L}\p{N}]+").expect("valid section title grammar"))
        .replace_all(&lowercase, " ");
    let title = js_trim(&normalized);
    (!title.is_empty()).then(|| SectionLocator::Title(title.to_owned()))
}

/// A numbered section's place ("sec33.1(2)") from its compact label, whitespace already out; empty where it is none.
pub fn normalize_compact_numbered_section_locator(compact: &str) -> String {
    numbered_section_label(compact).map_or_else(String::new, |label| format!("sec{label}"))
}

/// A numbered section's label ("33.1(2)", "A-1") where a compact label is one.
pub fn numbered_section_label(compact: &str) -> Option<String> {
    static NUMERIC: OnceLock<Regex> = OnceLock::new();
    static ALPHANUMERIC: OnceLock<Regex> = OnceLock::new();
    let numeric = NUMERIC.get_or_init(|| {
        Regex::new(r"^\d{1,8}[A-Za-z]{0,3}(?:[.-]\d{1,8}[A-Za-z]{0,3}){0,3}(?:\([^)]+\))*$")
            .expect("valid numeric section locator grammar")
    });
    let alphanumeric = ALPHANUMERIC.get_or_init(|| {
        Regex::new(r"^[A-Za-z]{1,3}(?:[.-][0-9A-Za-z]{1,8}){1,3}(?:\([^)]+\))*$")
            .expect("valid alphanumeric section locator grammar")
    });
    (numeric.is_match(compact) || alphanumeric.is_match(compact)).then(|| compact.to_owned())
}

/// Parse a numeric printed paragraph, page or transcript line locator using its canonical
/// spelling grammar.
pub fn parse_ordinal(kind: &str, raw: &str) -> Option<usize> {
    static PAGE: OnceLock<Regex> = OnceLock::new();
    static PARAGRAPH: OnceLock<Regex> = OnceLock::new();
    static LINE: OnceLock<Regex> = OnceLock::new();
    use unicode_normalization::UnicodeNormalization;
    let value = raw.nfkc().collect::<String>();
    let captures = if kind == "line" {
        LINE.get_or_init(|| Regex::new(r"(?i)^(?:(?:lines?|ll?\.?)[\s:_=-]*)?0*(\d{1,6})$").unwrap())
            .captures(value.trim())
    } else if kind == "page" {
        PAGE.get_or_init(|| {
            Regex::new(r"(?i)^#?\s*\[?\s*(?:(?:pages?|pp?\.?)[\s:_=-]*)?0*(\d{1,6})\s*\]?$")
                .unwrap()
        })
        .captures(value.trim())
    } else {
        PARAGRAPH.get_or_init(|| Regex::new(r"(?i)^#?\s*(?:(?:paragraphs?|paras?|pars?|¶)\.?\s*)?(?:(?:paragraph|para|par)[\s:_=-]*)?0*(\d{1,6})$" ).unwrap()).captures(value.trim())
    }?;
    captures.get(1)?.as_str().parse().ok()
}

