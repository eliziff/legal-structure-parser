//! A transcript's speaker labels.
use regex::Regex;
use std::sync::OnceLock;

/// The speaker label opening a transcript's line, after the question number it may carry: "Q.",
/// "A.", or a name before a colon ("MR. SMITH:", "THE COURT:", "MS JONES (Counsel):"). Returns
/// the label's byte range in `text`. A caption's or a memo's own labels ("BETWEEN:", "RE:", "FROM:",
/// "APPEARANCES:") name no speaker.
pub fn speaker_label(text: &str) -> Option<(usize, usize)> {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    static CAPTION: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| {
        Regex::new(r"^\s*(?:\d{1,4}\.\s+)?((?:Q|A)\.|(?:[A-Z][A-Za-z'\-]*\.?\s+){0,3}[A-Z][A-Za-z'\-]*\.?(?:\s*\([^)]{1,40}\))?:)(?:\s|$)")
            .expect("speaker label regex")
    });
    let caption = CAPTION.get_or_init(|| {
        Regex::new(r"(?i)^(?:between|and|and\s+to|re|from|to|cc|date|before|present|also\s+present|appearances|appearing|index|file\s+no\.?|court\s+file\s+no\.?|et\s+entre|entre):$")
            .expect("caption label regex")
    });
    label.captures(text).and_then(|found| found.get(1))
        .filter(|found| !caption.is_match(found.as_str()))
        .map(|found| (found.start(), found.end()))
}
