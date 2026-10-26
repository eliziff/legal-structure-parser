//! Output-shape adapters over the shared citation engine. No parsing lives here.
use crate::ScalarText;
use legal_citations::{Authority, Citation, Form, Format, ParentheticalKind, PinpointKind, Options};
use serde::Serialize;
pub use legal_citations::excerpt::{classify_citator_excerpt, ExcerptClassification};
pub use legal_citations as citations;

#[derive(Serialize)]
pub struct ProviderCitationMatch {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub family: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jurisdiction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub court: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationTextSpan {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationPinpoint {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub kind: &'static str,
    pub first: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CitationOccurrence {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub styled_citation: CitationTextSpan,
    pub core_citation: CitationTextSpan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
    pub pinpoints: Vec<CitationPinpoint>,
    /// The pinpoints as written, with the words that introduce them: "at para 105".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinpoint_phrase: Option<CitationTextSpan>,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_form: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit_short_form: Option<String>,
    pub reasons: Vec<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityReferenceOccurrence {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub token: CitationTextSpan,
    pub pinpoints: Vec<CitationPinpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinpoint_phrase: Option<CitationTextSpan>,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_number: Option<usize>,
}

fn span(text: &str, coordinates: &ScalarText<'_>, start: usize, end: usize) -> CitationTextSpan {
    CitationTextSpan { text: text[start..end].to_owned(),
        start: coordinates.utf16_at_byte(start).unwrap(), end: coordinates.utf16_at_byte(end).unwrap() }
}

fn pinpoints(text: &str, coordinates: &ScalarText<'_>, citation: &Citation) -> Vec<CitationPinpoint> {
    citation.pinpoints.iter().map(|pinpoint| {
        let kind = match pinpoint.kind {
            PinpointKind::Paragraph => "paragraph", PinpointKind::Page => "page",
            PinpointKind::Section | PinpointKind::Rule | PinpointKind::Article => "section",
            PinpointKind::Subsection => "subsection",
            PinpointKind::Schedule => "schedule", PinpointKind::Footnote => "footnote",
            PinpointKind::Clause => "clause", _ => "other",
        };
        let mapped = span(text, coordinates, pinpoint.span.start, pinpoint.span.end);
        CitationPinpoint { text: mapped.text, start: mapped.start, end: mapped.end, kind,
            first: pinpoint.first.clone(), last: pinpoint.last.clone() }
    }).collect()
}

/// The engine's pinpoint phrase: the pinpoints as written, with the words that introduce them.
fn pinpoint_phrase(text: &str, coordinates: &ScalarText<'_>, citation: &Citation) -> Option<CitationTextSpan> {
    citation.fields.pin_cite.as_ref().map(|phrase| span(text, coordinates, phrase.start, phrase.end))
}

fn occurrences(text: &str) -> Vec<Citation> {
    legal_citations::extract(text, &Options { resolve: false, parallel: false, ..Default::default() })
}

pub fn citation_occurrences_in_text(text: &str) -> Vec<CitationOccurrence> {
    let coordinates = ScalarText::new(text);
    occurrences(text).into_iter().filter(|cite| matches!(cite.form, Form::Full | Form::Short)).map(|cite| {
        let start = cite.style.as_ref().map_or(cite.span.start, |style| style.start);
        let end = cite.fields.explicit_short_span.as_ref().or(cite.fields.pin_cite.as_ref())
            .map_or(cite.span.end, |suffix| suffix.end.max(cite.span.end));
        let full = span(text, &coordinates, start, end);
        let kind = match cite.reasons.first().map(String::as_str) {
            Some("journal_grammar" | "article_grammar") => "journal",
            Some("book_grammar") => "book",
            Some("parliamentary_grammar") => "parliamentary",
            Some("ca_statute_grammar" | "titled_statute_grammar" | "statute_grammar" | "provider_statute_routing") => "statute",
            Some("provider_routing" | "reporter_grammar") => "case",
            Some("citation_grammar") => if cite.reasons.iter().any(|reason| reason == "kind_unclassified") { "other" } else { "case" },
            Some("online_grammar") => "other",
            _ => match cite.authority {
                Authority::Case => "case", Authority::Journal => "journal",
                Authority::Book | Authority::BookChapter => "book",
                Authority::Bill | Authority::ParliamentaryPaper | Authority::Debate => "parliamentary",
                authority if authority.is_legislation() => "statute", _ => "other",
            },
        };
        // Keep this adapter's original reason vocabulary and borrowed-string
        // contract. The full engine record exposes its additional diagnostics.
        let reasons = cite.reasons.iter().enumerate().filter_map(|(index, reason)| {
            if index > 0 && !matches!(reason.as_str(), "same_text_style" | "pinpoint_grammar" | "short_form_suffix" | "kind_unclassified") { return None; }
            if reason == "pinpoint_grammar" && cite.fields.pin_cite_kind.is_none() { return None; }
            let reason = match reason.as_str() {
                "neutral_grammar" | "canlii_grammar" | "database_grammar" | "provider_statute_routing" => "provider_routing",
                "code_grammar" | "regulation_grammar" => "statute_grammar",
                reason => reason,
            };
            ["article_grammar", "book_grammar", "ca_statute_grammar", "citation_grammar",
                "journal_grammar", "kind_unclassified", "online_grammar", "parliamentary_grammar",
                "pinpoint_grammar", "provider_routing", "reporter_grammar", "same_text_style",
                "short_form_suffix", "statute_grammar", "titled_statute_grammar"]
                .into_iter().find(|known| *known == reason)
        }).collect();
        CitationOccurrence { text: full.text, start: full.start, end: full.end,
            styled_citation: span(text, &coordinates, start, cite.span.end),
            core_citation: span(text, &coordinates, cite.span.start, cite.span.end),
            format: cite.format,
            pinpoints: pinpoints(text, &coordinates, &cite),
            pinpoint_phrase: pinpoint_phrase(text, &coordinates, &cite), kind,
            short_form: cite.short_name, explicit_short_form: cite.explicit_short_name, reasons }
    }).collect()
}

pub fn authority_references_in_text(text: &str) -> Vec<AuthorityReferenceOccurrence> {
    let coordinates = ScalarText::new(text);
    legal_citations::find::find_references(text).into_iter().filter_map(|cite| {
        let reference = cite.fields.inline_reference.as_ref()?;
        let full = span(text, &coordinates, cite.full_span.start, cite.full_span.end);
        Some(AuthorityReferenceOccurrence { text: full.text, start: full.start, end: full.end,
            token: span(text, &coordinates, reference.span.start, reference.span.end),
            pinpoints: pinpoints(text, &coordinates, &cite),
            pinpoint_phrase: pinpoint_phrase(text, &coordinates, &cite), kind: match cite.form {
                Form::Ibid => "ibid", Form::Supra => "supra",
                _ => unreachable!(),
            },
            note_number: reference.note })
    }).collect()
}

pub fn provider_citations_in_text(text: &str) -> Vec<ProviderCitationMatch> {
    let coordinates = ScalarText::new(text);
    legal_citations::extract(text, &Options { resolve: false, parallel: false, ..Default::default() })
        .into_iter().filter_map(|cite| {
        if cite.form != Form::Full { return None; }
        let family = match cite.format {
            Some(Format::Neutral) => "neutral", Some(Format::Reporter) => "reporter",
            Some(Format::CanLii) => "canlii", Some(Format::Database) => "database",
            _ if cite.authority.is_legislation() => "statute", _ => return None,
        };
        let end = cite.parentheticals.iter().filter(|item| item.kind == ParentheticalKind::Court
            && item.span.start >= cite.span.end && text[cite.span.end..item.span.start].trim().is_empty())
            .map(|item| item.span.end).max().unwrap_or(cite.span.end);
        Some(ProviderCitationMatch { text: text[cite.span.start..end].to_owned(),
            start: coordinates.utf16_at_byte(cite.span.start).unwrap(),
            end: coordinates.utf16_at_byte(end).unwrap(), family,
            jurisdiction: cite.jurisdiction, year: cite.fields.year,
            court: cite.court.map(|court| court.text).or(cite.fields.series),
            number: cite.fields.number, volume: cite.fields.volume,
            reporter: cite.fields.reporter, page: cite.fields.page })
    }).collect()
}

pub fn citation_lookup_key(text: &str) -> String {
    legal_citations::key::key_for_text(text).unwrap_or_default()
}

/// The decision a Canadian reporter citation names, for a store that does not index the reporter to find by its style
/// of cause: the style as written, the citation's year, and the neutral codes of the court the citation names
/// (`(Ont CA)`, or the one court its reporter reports). None where the text is not one such citation with its style,
/// year and court. A volume's bracketed year is taken as the decision's: allowing the year before let the 1993 Haig v
/// Canada name the 1992 one.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReporterDecision {
    pub name: String,
    pub year: i32,
    pub courts: Vec<String>,
}

pub fn reporter_decision(text: &str) -> Option<ReporterDecision> {
    let citations = legal_citations::extract(text, &Options { resolve: false, parallel: false, ..Default::default() });
    let [citation] = citations.iter().filter(|citation| citation.form == Form::Full).collect::<Vec<_>>()[..]
        else { return None };
    if citation.format != Some(Format::Reporter) || citation.authority != Authority::Case || citation.is_ambiguous() {
        return None;
    }
    let name = citation.style.as_ref()?.text.trim().to_owned();
    let year = citation.fields.year_number?;
    let court = legal_citations::registry::registry().court(&citation.court.as_ref()?.id)?;
    if !(court.jurisdiction == "ca" || court.jurisdiction.starts_with("ca-")) || court.neutral.is_empty() || name.is_empty() {
        return None;
    }
    Some(ReporterDecision { name, year,
        courts: court.neutral.iter().map(|code| code.to_uppercase()).collect() })
}

pub fn caselaw_citation_lookup_key(text: &str) -> Result<String, &'static str> {
    legal_citations::key::key_for_text(text).map_err(|error| match error {
        legal_citations::key::KeyError::NoCitation => "no citation was found",
        legal_citations::key::KeyError::Multiple(_) => "citation must identify one citation form; multiple citations were found",
        legal_citations::key::KeyError::NoIdentity => "the citation has no unambiguous authority identity",
    })
}

pub fn has_citation_in_text(text: &str) -> Result<bool, String> { Ok(legal_citations::has_citation(text)) }
