use crate::javascript_whitespace;
use regex::Regex;
use std::sync::OnceLock;

pub(crate) fn compact_provision_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !javascript_whitespace(*character))
        .collect()
}

pub(crate) fn normalize_numbered_section_locator(value: &str) -> String {
    normalize_compact_numbered_section_locator(&compact_provision_label(value))
}

pub(crate) fn normalize_compact_numbered_section_locator(compact: &str) -> String {
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
