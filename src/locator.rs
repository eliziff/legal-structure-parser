use crate::{
    javascript_whitespace,
    text::{trim_javascript_whitespace as js_trim, JS_WHITESPACE_CLASS as JS_WS},
};
use regex::Regex;
use std::sync::OnceLock;

pub(crate) fn literal_page_marker(line: &str, insensitive: bool) -> Option<&str> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let marker = line.strip_suffix(']')?;
    let prefix = marker.get(..6)?;
    ((insensitive && prefix.eq_ignore_ascii_case("[page ")) || prefix == "[page ")
        .then(|| &marker[6..])
}

pub(crate) fn compact_provision_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !javascript_whitespace(*character))
        .collect()
}

pub(crate) fn normalize_numbered_section_locator(value: &str) -> String {
    normalize_compact_numbered_section_locator(&compact_provision_label(value))
}

pub fn normalize_section_locator(locator: &str) -> String {
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
    let numbered = normalize_numbered_section_locator(&without_prefix);
    if !numbered.is_empty() {
        return numbered;
    }
    let heading = without_prefix
        .trim_end_matches(|character| character == '.' || javascript_whitespace(character));
    if HEADING
        .get_or_init(|| Regex::new(r"^(?:[IVXLCDM]+|[A-Z])$").expect("valid heading grammar"))
        .is_match(heading)
    {
        return format!("sec{heading}");
    }
    let lowercase = heading.to_lowercase();
    let normalized = NON_TITLE
        .get_or_init(|| Regex::new(r"[^\p{L}\p{N}]+").expect("valid section title grammar"))
        .replace_all(&lowercase, " ");
    let title = js_trim(&normalized);
    if title.is_empty() {
        String::new()
    } else {
        format!("sectitle:{title}")
    }
}

pub fn normalize_compact_numbered_section_locator(compact: &str) -> String {
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
    if numeric.is_match(compact) || alphanumeric.is_match(compact) {
        format!("sec{compact}")
    } else {
        String::new()
    }
}
