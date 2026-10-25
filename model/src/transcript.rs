//! A transcript's speaker labels.
use regex::Regex;
use std::sync::OnceLock;

/// The speaker label opening a transcript's line, after the question number it may carry: "Q.",
/// "A.", or a name before a colon ("MR. SMITH:", "THE COURT:", "MS JONES (Counsel):"). Returns
/// the label's byte range in `text`.
pub fn speaker_label(text: &str) -> Option<(usize, usize)> {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| {
        Regex::new(r"^\s*(?:\d{1,4}\.\s+)?((?:Q|A)\.|(?:[A-Z][A-Za-z'\-]*\.?\s+){0,3}[A-Z][A-Za-z'\-]*\.?(?:\s*\([^)]{1,40}\))?:)(?:\s|$)")
            .expect("speaker label regex")
    });
    label.captures(text).and_then(|found| found.get(1)).map(|found| (found.start(), found.end()))
}
