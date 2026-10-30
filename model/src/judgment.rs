//! A judgment's author line, which opens its reasons, and the signature lines that close them.
use regex::Regex;
use std::sync::OnceLock;

/// A judge's or member's signature, or a release line, as a decision prints them after its reasons
/// ("“K. Feldman J.A.”", "“I agree. L.B. Roberts J.A.”", "Released: November 28, 2022").
pub fn signature_line(value: &str) -> bool {
    static SIGNATURE: OnceLock<Regex> = OnceLock::new();
    static RELEASE: OnceLock<Regex> = OnceLock::new();
    let value = value.trim().trim_matches(['"', '“', '”', '\'', '‘', '’']).trim();
    !value.is_empty()
        && value.chars().count() <= 90
        && (SIGNATURE
            .get_or_init(|| {
                Regex::new(r"\b(?:J|JJ|C\.?J|C\.?J\.?C|C\.?J\.?O|J\.?A|JJ\.?A|J\.?C\.?A|Q\.?C|K\.?C)\.(?:\s*\((?:ad hoc|dissenting|concurring)\))?$|\b(?:Member|Adjudicator|Arbitrator|Chairperson|Vice-Chairperson|Commissioner|Associate Judge|Master|Prothonotary|Referee)$")
                    .expect("signature regex")
            })
            .is_match(value)
            || RELEASE
                .get_or_init(|| Regex::new(r"^(?i:released|date of release|dated|i agree)\b").expect("release regex"))
                .is_match(value))
}

/// The line a decision's reasons open with, naming who gives them: a judge's name and title before
/// a dash ("MARTLAND J.—…"), "The judgment of the Court was delivered by", "The reasons for judgment
/// were delivered by".
pub fn author_line(line: &str) -> bool {
    static AUTHOR: OnceLock<Regex> = OnceLock::new();
    AUTHOR
        .get_or_init(|| {
            Regex::new(r"^[A-Z][A-Za-zÀ-ÿ'’. -]{1,60}?(?:C\.J\.[A-Z.]*|J\.A\.|JJ\.|J\.)\s*[—–-]\s*\S").expect("author regex")
        })
        .is_match(line)
        || line.contains("judgment of the Court was delivered by")
        || line.contains("reasons for judgment were delivered by")
}
