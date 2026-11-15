//! Output-shape adapters over the shared citation engine. No parsing lives here.
use crate::ScalarText;
use legal_citations::authorities::PinpointValue;
use legal_citations::{Authority, Citation, Form, Format, Options, ParentheticalKind, Span};

/// A text's citations as each reading below takes them, their parallel citations ungrouped: one extraction, which a
/// caller wanting several of the readings of one text makes once and lends to each.
pub fn read_citations(text: &str) -> Vec<Citation> {
    legal_citations::extract(text, &Options { resolve: false, parallel: false, ..Default::default() })
}
use serde::Serialize;
pub use legal_citations::excerpt::{classify_citator_excerpt, ExcerptClassification};
pub use legal_citations as citations;

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct ProviderCitationMatch {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub family: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub jurisdiction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub year: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub court: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub volume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub reporter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub page: Option<String>,
}

/// Where a citation sits in its unit, as a draft records it: its authority's citation (the style its core is written
/// with), its core, its pinpoints first to last, and the words that introduce them ("at para 105").
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OccurrenceSpans {
    pub authority_span: Span,
    pub core_span: Span,
    pub pinpoint_span: Option<Span>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub pinpoint_phrase: Option<Span>,
}

/// A citation read in a text: its whole extent, where its parts sit, and its pinpoints in a draft's form. Every place is
/// in UTF-16 units of the unit the text starts at `offset` in.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CitationOccurrence {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub spans: OccurrenceSpans,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub format: Option<Format>,
    pub pinpoints: Vec<PinpointValue>,
    #[cfg_attr(feature = "binding-types", ts(type = r#""case" | "statute" | "journal" | "book" | "parliamentary" | "other""#))]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub short_form: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub explicit_short_form: Option<String>,
    pub reasons: Vec<&'static str>,
}

/// A supra or ibid read in a text, as [`CitationOccurrence`] places it: its authority's citation is the written short
/// name through the marker, its core the marker ("supra note 6").
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AuthorityReferenceOccurrence {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub spans: OccurrenceSpans,
    pub pinpoints: Vec<PinpointValue>,
    #[cfg_attr(feature = "binding-types", ts(type = r#""ibid" | "supra""#))]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "binding-types", ts(optional))]
    pub note_number: Option<usize>,
}

/// A text's places in UTF-16 units of the unit it starts at `offset` in.
struct Places<'a> { text: &'a str, coordinates: ScalarText<'a>, offset: usize }

impl<'a> Places<'a> {
    fn new(text: &'a str, offset: usize) -> Self { Self { text, coordinates: ScalarText::new(text), offset } }

    fn span(&self, start: usize, end: usize) -> Span {
        Span { text: self.text[start..end].to_owned(), start: self.offset + self.coordinates.utf16_at_byte(start).unwrap(),
            end: self.offset + self.coordinates.utf16_at_byte(end).unwrap() }
    }

    /// The citation's spans from its authority's and core's byte ranges, and its pinpoints.
    fn place(&self, authority: (usize, usize), core: (usize, usize), citation: &Citation) -> (OccurrenceSpans, Vec<PinpointValue>) {
        let (pinpoint, phrase) = pinpoint_extent(citation.pinpoints.iter().map(|pinpoint| (pinpoint.span.start, pinpoint.span.end)),
            citation.fields.pin_cite.as_ref().map(|phrase| (phrase.start, phrase.end)));
        let pinpoints = citation.pinpoints.iter().map(|pinpoint| {
            let at = self.span(pinpoint.span.start, pinpoint.span.end);
            PinpointValue::read(pinpoint, at.start, at.end)
        }).collect();
        (OccurrenceSpans { authority_span: self.span(authority.0, authority.1), core_span: self.span(core.0, core.1),
            pinpoint_span: pinpoint.map(|(start, end)| self.span(start, end)),
            pinpoint_phrase: phrase.map(|(start, end)| self.span(start, end)) }, pinpoints)
    }
}

/// Where a citation writes its pinpoints, first to last, and the words that introduce them ("at para 105") where those
/// hold them all.
pub fn pinpoint_extent(pinpoints: impl IntoIterator<Item = (usize, usize)>, phrase: Option<(usize, usize)>)
    -> (Option<(usize, usize)>, Option<(usize, usize)>) {
    let mut ordered = pinpoints.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|range| range.0);
    let pinpoint = ordered.first().zip(ordered.last()).map(|(first, last)| (first.0, last.1));
    (pinpoint, phrase.filter(|phrase| pinpoint.is_some_and(|(start, end)| phrase.0 <= start && phrase.1 >= end)))
}

pub fn citation_occurrences_in_text(text: &str, offset: usize) -> Vec<CitationOccurrence> {
    citation_occurrences(text, &read_citations(text), offset)
}

/// [`citation_occurrences_in_text`] over the text's citations already read (`reference_names::read_citations`).
pub fn citation_occurrences(text: &str, read: &[Citation], offset: usize) -> Vec<CitationOccurrence> {
    let places = Places::new(text, offset);
    read.iter().filter(|cite| matches!(cite.form, Form::Full | Form::Short)).map(|cite| {
        let start = cite.style.as_ref().map_or(cite.span.start, |style| style.start);
        let end = cite.fields.explicit_short_span.as_ref().or(cite.fields.pin_cite.as_ref())
            .map_or(cite.span.end, |suffix| suffix.end.max(cite.span.end));
        let full = places.span(start, end);
        let kind = match cite.reasons.first().map(String::as_str) {
            Some("journal_grammar" | "article_grammar") => "journal",
            Some("book_grammar") => "book",
            Some("parliamentary_grammar") => "parliamentary",
            Some("ca_statute_grammar" | "titled_statute_grammar" | "statute_grammar" | "provider_statute_routing") => "statute",
            Some("provider_routing" | "reporter_grammar") => "case",
            Some("citation_grammar") => if cite.reasons.iter().any(|reason| reason == "kind_unclassified") { "other" } else { "case" },
            Some("online_grammar") => "other",
            _ => match &cite.authority {
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
        let (spans, pinpoints) = places.place((start, cite.span.end), (cite.span.start, cite.span.end), cite);
        CitationOccurrence { text: full.text, start: full.start, end: full.end, spans, format: cite.format.clone(), pinpoints,
            kind, short_form: cite.short_name.clone(), explicit_short_form: cite.explicit_short_name.clone(), reasons }
    }).collect()
}

pub fn authority_references_in_text(text: &str, offset: usize) -> Vec<AuthorityReferenceOccurrence> {
    let places = Places::new(text, offset);
    legal_citations::find::find_references(text).into_iter().filter_map(|cite| {
        let reference = cite.fields.inline_reference.as_ref()?;
        let full = places.span(cite.full_span.start, cite.full_span.end);
        let (spans, pinpoints) = places.place((cite.full_span.start, reference.span.end),
            (reference.span.start, reference.span.end), &cite);
        Some(AuthorityReferenceOccurrence { text: full.text, start: full.start, end: full.end, spans, pinpoints,
            kind: match cite.form { Form::Ibid => "ibid", Form::Supra => "supra", _ => unreachable!() },
            note_number: reference.note })
    }).collect()
}

pub fn provider_citations_in_text(text: &str) -> Vec<ProviderCitationMatch> {
    provider_citations(text, &read_citations(text))
}

/// [`provider_citations_in_text`] over the text's citations already read (`reference_names::read_citations`).
pub fn provider_citations(text: &str, read: &[Citation]) -> Vec<ProviderCitationMatch> {
    let coordinates = ScalarText::new(text);
    read.iter().filter_map(|cite| {
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
            jurisdiction: cite.jurisdiction.clone(), year: cite.fields.year.clone(),
            court: cite.court.as_ref().map(|court| court.text.clone()).or_else(|| cite.fields.series.clone()),
            number: cite.fields.number.clone(), volume: cite.fields.volume.clone(),
            reporter: cite.fields.reporter.clone(), page: cite.fields.page.clone() })
    }).collect()
}

/// Whether a model's answer links a Canadian decision, which it must not cite by address: a markdown link or a bare
/// address to a page whose paragraphs CanLII's or Decisia's anchors reach, a B.C. court's judgment text or the Supreme
/// Court's case page. A link whose label writes a Canadian neutral citation (of a court CanLII routes, from 1900) is
/// read as that citation, and its address is not counted.
pub fn canadian_decision_link(answer: &str) -> bool {
    use std::sync::LazyLock;
    static LABELLED: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\[([^\]\r\n]+)\]\(([^)\r\n]*)\)").unwrap());
    static LINKED: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\[[^\]\r\n]+\]\((https?://[^\s)]+)\)").unwrap());
    static BARE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"https?://[^\s<>"')\]]+"#).unwrap());
    let cited = |label: &str| provider_citations_in_text(label).into_iter().any(|cite| cite.family == "neutral"
        && cite.year.as_deref().is_some_and(|year| year.starts_with("19") || year.starts_with("20"))
        && legal_citations::url::source_canlii_routes().contains_key(&cite.court.unwrap_or_default().to_uppercase()));
    let decision = |raw: &str| url::Url::parse(raw).is_ok_and(|url| {
        let host = url.host_str().unwrap_or("");
        let (host, path) = (host.strip_prefix("www.").unwrap_or(host), url.path().to_lowercase());
        legal_citations::url::source_anchor_within(url.as_str(), "paragraph", "par1", None).is_some()
            || (host == "bccourts.ca" && path.contains("/jdb-txt/")) || (host == "scc-csc.ca" && path.contains("/case-dossier/"))
    });
    let read = LABELLED.replace_all(answer, |found: &regex::Captures| {
        if cited(&found[1]) { found[1].to_owned() } else { found[0].to_owned() }
    });
    LINKED.captures_iter(&read).any(|found| decision(&found[1]))
        || BARE.find_iter(&read).any(|found| decision(found.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?'])))
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
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct ReporterDecision {
    pub name: String,
    pub year: i32,
    pub courts: Vec<String>,
}

pub fn reporter_decision(text: &str) -> Option<ReporterDecision> {
    let citations = read_citations(text);
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
