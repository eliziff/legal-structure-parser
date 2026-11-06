//! A transcript's speaker labels.
use regex::Regex;
use std::sync::OnceLock;

/// Every speaker label opening a transcript's line, as byte ranges of `text`: the first, after the question number it
/// may carry (with or without its stop: "1   Q.") and a margin's code ("U/A"): "Q.", "A.", a numbered speaker
/// ("Speaker 1:"), or a name before a colon ("MR. SMITH:", "THE COURT:", "MS JONES (Counsel):", "Me BRISTOW :"); and a
/// name's after a "Q." or "A." ("Q.   MR. PHILLIPS:"). A caption's, a memo's or a form's own labels ("BETWEEN:", "RE:",
/// "APPEARANCES:", "BY MR. X:", "REPORTED BY:", "Email:") name no speaker.
pub fn speaker_labels(text: &str) -> Vec<(usize, usize)> {
    static LABEL: OnceLock<Regex> = OnceLock::new();
    static NAME: OnceLock<Regex> = OnceLock::new();
    static CAPTION: OnceLock<Regex> = OnceLock::new();
    let label = LABEL.get_or_init(|| {
        Regex::new(r"^\s*(?:\d{1,4}\.?\s+)?(?:[A-Z]/[A-Z]\s+)?((?:Q|A)\.|Speaker\s+\d{1,2}\s?:|(?:[A-Z][A-Za-z'\-]*\.?\s+){0,3}[A-Z][A-Za-z'\-]*\.?(?:\s*\([^)]{1,40}\))?\s?:)(?:\s|$)")
            .expect("speaker label regex")
    });
    let name = NAME.get_or_init(|| {
        Regex::new(r"^\s+((?:[A-Z][A-Za-z'\-]*\.?\s+){0,3}[A-Z][A-Za-z'\-]*\.?(?:\s*\([^)]{1,40}\))?\s?:)(?:\s|$)").expect("speaker name regex")
    });
    let caption = CAPTION.get_or_init(|| {
        Regex::new(r"(?i)^(?:between|and|and\s+to|and\s+between|re|from|to|cc|date|before|present|also\s+present|appearances|appearing|index|file\s+no\.?|court\s+file\s+no\.?|et\s+entre|entre|by\s+.+|reported\s+by|witness(?:es)?|email|e-mail|example|cost|expires|tel|fax|phone|address)\s?:$")
            .expect("caption label regex")
    });
    let mut found = Vec::new();
    let Some(first) = label.captures(text).and_then(|found| found.get(1)).filter(|found| !caption.is_match(found.as_str())) else { return found };
    found.push((first.start(), first.end()));
    if matches!(first.as_str(), "Q." | "A.") {
        if let Some(second) = name.captures(&text[first.end()..]).and_then(|found| found.get(1)).filter(|found| !caption.is_match(found.as_str())) {
            found.push((first.end() + second.start(), first.end() + second.end()));
        }
    }
    found
}
