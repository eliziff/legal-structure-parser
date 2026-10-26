//! A filing's cover and a judgment's front matter, read from their lines: the parties a cover's
//! BETWEEN block names in the roles it prints, the party a document is filed by ("BOOK OF
//! AUTHORITIES OF THE APPLICANT"), and what a front-matter line of a judgment is.
use crate::trim_javascript_whitespace as trim;
use regex::Regex;
use serde::Serialize;
use std::sync::LazyLock;

/// The parties a cover prints under one role.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PartyGroup {
    /// "Applicant", "Moving Party", "Intervener".
    pub role: String,
    pub parties: Vec<String>,
}

/// Whose document it is: "of the Applicant" is the role `Applicant` with its article; "of Jane
/// Smith" is the party as printed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FiledBy {
    pub party: String,
    pub article: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilingCover {
    pub party_groups: Vec<PartyGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filed_by: Option<FiledBy>,
}

/// A line that is a party role alone, as a cover prints it ("Applicant", "RESPONDENTS:").
static COVER_ROLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    &format!(r"(?i)^(?:{})\s*:?$", legal_structure_model::PARTY_ROLES),
).unwrap());


/// The cover of a filing, from the text of its opening page; `title`, where the cover's record title
/// is already known, names whose document it is in place of its "DOCUMENT:" line.
pub fn filing_cover(opening: &str, title: Option<&str>) -> FilingCover {
    static DOCUMENT_LABEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^DOCUMENT\s*:?$").unwrap());
    static DOCUMENT_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^DOCUMENT\s*:?\s+(.+)$").unwrap());
    static FILED_BY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(?-u:\b)of\s+(the\s+)?(.{2,120}?)\s*$").unwrap());
    let lines = opening.split('\n').map(trim).collect::<Vec<_>>();
    let labelled = lines.iter().position(|line| DOCUMENT_LABEL.is_match(line));
    let named = title.filter(|title| !title.is_empty())
        .or_else(|| labelled.and_then(|at| lines.get(at + 1).copied()).filter(|line| !line.is_empty()))
        .or_else(|| lines.iter().find_map(|line| DOCUMENT_LINE.captures(line).map(|found| found.get(1).unwrap().as_str())))
        .unwrap_or("");
    let filed_by = FILED_BY.captures(named).map(|found| {
        let party = &found[2];
        FiledBy {
            party: if COVER_ROLE.is_match(party) {
                let mut characters = party.chars();
                characters.next().map(|first| first.to_uppercase().chain(characters.as_str().to_lowercase().chars()).collect())
                    .unwrap_or_default()
            } else {
                party.to_owned()
            },
            article: found.get(1).is_some(),
        }
    });
    FilingCover { party_groups: cover_parties(&lines), filed_by }
}

/// The parties named in a cover's BETWEEN block, in the order and roles it prints them: each role's
/// line follows the names it applies to.
fn cover_parties(lines: &[&str]) -> Vec<PartyGroup> {
    static BETWEEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^between\s*:?$").unwrap());
    static JOINER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:and|-\s*and\s*-|v\.?)$").unwrap());
    static CAPITALS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][A-Z\s]+$").unwrap());
    static COLON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*:$").unwrap());
    let lines = lines.iter().copied().filter(|line| !line.is_empty()).collect::<Vec<_>>();
    let Some(start) = lines.iter().position(|line| BETWEEN.is_match(line)) else { return Vec::new() };
    let mut groups = Vec::new();
    let mut names = Vec::new();
    for line in &lines[start + 1..] {
        if COVER_ROLE.is_match(line) {
            let word = COLON.replace(line, "").to_lowercase();
            let role = if word.starts_with("interven") {
                "Intervener".to_owned()
            } else {
                let singular = word.strip_suffix("ies").map(|stem| format!("{stem}y"))
                    .unwrap_or_else(|| word.strip_suffix('s').unwrap_or(&word).to_owned());
                // Each word capitalized.
                let mut after_space = true;
                singular.chars().map(|character| {
                    let upper = after_space && (character.is_ascii_alphanumeric() || character == '_');
                    after_space = character.is_whitespace();
                    if upper { character.to_ascii_uppercase() } else { character }
                }).collect()
            };
            if !names.is_empty() {
                groups.push(PartyGroup { role, parties: std::mem::take(&mut names) });
            }
        } else if JOINER.is_match(line) {
            continue;
        } else if CAPITALS.is_match(line) && line.encode_utf16().count() > 12 && names.is_empty() && !groups.is_empty() {
            break;
        } else {
            names.push(line.to_string());
        }
    }
    groups
}

/// What a line of a judgment's front matter is: a party's role ("Respondent", "The Appellants (Plaintiffs)
/// and Respondent by cross-appeal"), a labelled line ("DOCKET: C69420"), counsel's line ("A. Lawyer,
/// for the appellant"), or another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FrontMatterLine {
    Party,
    Labelled,
    Counsel,
    Other,
}

pub(crate) fn front_matter_line(text: &str) -> FrontMatterLine {
    static PARTY: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(
        r"(?i)^(?:(?:the\s+)?(?:{})(?:\s*\([^()]*\))?(?:\s+and\s+\S[^\n\r\u{{2028}}\u{{2029}}]*)?)$",
        legal_structure_model::PARTY_ROLES,
    )).unwrap());
    static LABELLED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\s]+:\s").unwrap());
    static COUNSEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i),\s+for\s+the\s").unwrap());
    if PARTY.is_match(trim(text)) {
        FrontMatterLine::Party
    } else if LABELLED.is_match(text) {
        FrontMatterLine::Labelled
    } else if COUNSEL.is_match(text) {
        FrontMatterLine::Counsel
    } else {
        FrontMatterLine::Other
    }
}
