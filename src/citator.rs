//! Output-shape adapters over the shared citation engine. No parsing lives here.
use crate::ScalarText;
use legal_citations::{Authority, Citation, Form, Format, PinpointKind, Options};
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
    pub key: Option<String>,
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
    pub kind: PinpointKind,
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
    pub pinpoints: Vec<CitationPinpoint>,
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
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_number: Option<usize>,
}

fn span(text: &str, coordinates: &ScalarText<'_>, start: usize, end: usize) -> CitationTextSpan {
    CitationTextSpan { text: text[start..end].to_owned(),
        start: coordinates.utf16_at_byte(start).unwrap(), end: coordinates.utf16_at_byte(end).unwrap() }
}

fn pinpoints(text: &str, coordinates: &ScalarText<'_>, citation: &Citation) -> Vec<CitationPinpoint> {
    citation.pinpoints.iter().map(|pin| {
        let mapped = span(text, coordinates, pin.span.start, pin.span.end);
        CitationPinpoint { text: mapped.text, start: mapped.start, end: mapped.end,
            kind: pin.kind, first: pin.first.clone(), last: pin.last.clone() }
    }).collect()
}

fn occurrences(text: &str) -> Vec<Citation> {
    legal_citations::extract(text, &Options { resolve: false, parallel: false, ..Default::default() })
}

pub fn citation_occurrences_in_text(text: &str) -> Vec<CitationOccurrence> {
    let coordinates = ScalarText::new(text);
    occurrences(text).into_iter().filter(|cite| cite.form == Form::Full).map(|cite| {
        let start = cite.style.as_ref().map_or(cite.span.start, |style| style.start);
        let full = span(text, &coordinates, start, cite.full_span.end);
        let kind = match cite.authority {
            Authority::Case => "case", Authority::Journal => "journal",
            Authority::Book | Authority::BookChapter => "book",
            Authority::ParliamentaryPaper | Authority::Debate => "parliamentary",
            authority if authority.is_legislation() => "statute", _ => "other",
        };
        // Keep this adapter's original reason vocabulary and borrowed-string
        // contract. The full engine record exposes its additional diagnostics.
        let reasons = cite.reasons.iter().filter_map(|reason| {
            let reason = match reason.as_str() {
                "neutral_grammar" | "canlii_grammar" | "database_grammar" => "provider_routing",
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
            pinpoints: pinpoints(text, &coordinates, &cite), kind,
            short_form: cite.short_name, explicit_short_form: cite.explicit_short_name, reasons }
    }).collect()
}

pub fn authority_references_in_text(text: &str) -> Vec<AuthorityReferenceOccurrence> {
    let coordinates = ScalarText::new(text);
    occurrences(text).into_iter().filter(|cite| matches!(cite.form, Form::Short | Form::Ibid | Form::Supra)).map(|cite| {
        let full = span(text, &coordinates, cite.full_span.start, cite.full_span.end);
        AuthorityReferenceOccurrence { text: full.text, start: full.start, end: full.end,
            token: span(text, &coordinates, cite.span.start, cite.span.end),
            pinpoints: pinpoints(text, &coordinates, &cite), kind: match cite.form {
                Form::Short => "short", Form::Ibid => "ibid", Form::Supra => "supra",
                _ => unreachable!(),
            },
            note_number: cite.fields.note.map(|number| number as usize) }
    }).collect()
}

pub fn provider_citations_in_text(text: &str) -> Vec<ProviderCitationMatch> {
    let coordinates = ScalarText::new(text);
    occurrences(text).into_iter().filter(|cite| cite.form == Form::Full).filter_map(|cite| {
        let family = match cite.format? {
            Format::Neutral => "neutral", Format::Reporter => "reporter", Format::CanLii => "canlii", Format::Database => "database",
            Format::StatuteVolume | Format::RegulationSeries | Format::Code => "statute", _ => return None,
        };
        let core = span(text, &coordinates, cite.span.start, cite.span.end);
        Some(ProviderCitationMatch { text: core.text, start: core.start, end: core.end, family, key: cite.key,
            jurisdiction: cite.jurisdiction, year: cite.fields.year,
            court: cite.court.map(|court| court.text), number: cite.fields.number,
            volume: cite.fields.volume, reporter: cite.fields.reporter, page: cite.fields.page })
    }).collect()
}

pub fn citation_lookup_key(text: &str) -> String {
    legal_citations::key::key_for_text(text).unwrap_or_default()
}

pub fn caselaw_citation_lookup_key(text: &str) -> Result<String, &'static str> {
    legal_citations::key::key_for_text(text).map_err(|error| match error {
        legal_citations::key::KeyError::NoCitation => "no citation was found",
        legal_citations::key::KeyError::Multiple(_) => "citation must identify one citation form; multiple citations were found",
        legal_citations::key::KeyError::NoIdentity => "the citation has no unambiguous authority identity",
    })
}

pub fn has_citation_in_text(text: &str) -> Result<bool, String> { Ok(legal_citations::has_citation(text)) }

#[cfg(test)]
mod tests {
    use super::{authority_references_in_text, citation_occurrences_in_text, has_citation_in_text};

    #[test]
    fn citation_presence_accepts_plain_text_and_unicode_case_names() {
        assert!(!has_citation_in_text("no citation here at all").unwrap());
        assert!(has_citation_in_text("R. v. Jordan, 2016 SCC 27").unwrap());
        assert!(has_citation_in_text("Éditions Écosociété Inc. v. Banro Corp.").unwrap());
    }

    #[test]
    fn citation_occurrence_separates_style_core_and_multiple_pinpoints() {
        let text = "See R. v. Jordan, 2016 SCC 27 at paras. 20, 23 and 25.";
        let occurrences = citation_occurrences_in_text(text);
        assert_eq!(occurrences.len(), 1);
        let occurrence = &occurrences[0];
        assert_eq!(
            occurrence.text,
            "R. v. Jordan, 2016 SCC 27 at paras. 20, 23 and 25"
        );
        assert_eq!(occurrence.styled_citation.text, "R. v. Jordan, 2016 SCC 27");
        assert_eq!(occurrence.core_citation.text, "2016 SCC 27");
        assert_eq!(occurrence.kind, "case");
        assert_eq!(occurrence.short_form.as_deref(), Some("R. v. Jordan"));
        assert_eq!(
            occurrence
                .pinpoints
                .iter()
                .map(|pinpoint| (pinpoint.text.as_str(), pinpoint.kind))
                .collect::<Vec<_>>(),
            [
                ("20", "paragraph"),
                ("23", "paragraph"),
                ("25", "paragraph")
            ]
        );
        assert!(occurrence.styled_citation.end <= occurrence.pinpoints[0].start);
        assert!(occurrence
            .pinpoints
            .windows(2)
            .all(|pair| pair[0].end <= pair[1].start));
    }

    #[test]
    fn citation_occurrences_keep_repeated_matches_distinct_and_short_forms_local() {
        let text = "Hansman v Neufeld, 2023 SCC 14 [Hansman]. Then 2023 SCC 14 at para 9.";
        let occurrences = citation_occurrences_in_text(text);
        assert_eq!(occurrences.len(), 2);
        assert_eq!(
            occurrences[0].short_form.as_deref(),
            Some("Hansman v Neufeld")
        );
        assert_eq!(
            occurrences[0].explicit_short_form.as_deref(),
            Some("Hansman")
        );
        assert_eq!(
            occurrences[0].text,
            "Hansman v Neufeld, 2023 SCC 14 [Hansman]."
        );
        assert_eq!(occurrences[1].core_citation.text, "2023 SCC 14");
        assert_eq!(occurrences[1].pinpoints[0].text, "9");
        assert!(occurrences[0].end <= occurrences[1].start);
    }

    #[test]
    fn citation_occurrence_offsets_are_javascript_utf16() {
        let text = "🦫 Éditions Écosociété Inc. v. Banro Corp., 2012 SCC 18 at para 7";
        let occurrence = citation_occurrences_in_text(text).pop().unwrap();
        assert_eq!(occurrence.start, "🦫 ".encode_utf16().count());
        assert_eq!(
            occurrence.core_citation.start,
            "🦫 Éditions Écosociété Inc. v. Banro Corp., "
                .encode_utf16()
                .count()
        );
        assert_eq!(
            occurrence.styled_citation.text,
            "Éditions Écosociété Inc. v. Banro Corp., 2012 SCC 18"
        );
        assert_eq!(occurrence.pinpoints[0].text, "7");
    }

    #[test]
    fn authority_references_reuse_frozen_reference_and_pinpoint_grammars() {
        let text = "🦫 Ibid at para 7; Smith, supra note 4 at pp. 10-11.";
        let references = authority_references_in_text(text);
        assert_eq!(references.len(), 2);
        assert_eq!(references[0].kind, "ibid");
        assert_eq!(references[0].start, "🦫 ".encode_utf16().count());
        assert_eq!(references[0].pinpoints[0].text, "7");
        assert_eq!(references[1].kind, "supra");
        assert_eq!(references[1].note_number, Some(4));
        assert_eq!(
            references[1]
                .pinpoints
                .iter()
                .map(|pinpoint| pinpoint.text.as_str())
                .collect::<Vec<_>>(),
            ["10", "11"]
        );
    }
}

#[cfg(test)]
mod authorities_style_regressions {
    use super::citation_occurrences_in_text;
    #[test]
    fn reporter_citations_retain_case_identity_and_full_style() {
        for citation in [
            "[2015] 1 S.C.R. 331",
            "[2015] 1 SCR 331",
            "[2015] 1 R.C.S. 331",
            "(1994) 117 DLR (4th) 577",
            "(2003), 227 DLR (4th) 282",
            "(1895), 24 SCR 650",
        ] {
            let text = format!("See Carter v. Canada (Attorney General), {citation} at para 7.");
            let occurrences = citation_occurrences_in_text(&text);
            assert_eq!(occurrences.len(), 1, "{citation}");
            let item = &occurrences[0];
            assert_eq!(item.kind, "case", "{citation}");
            assert_eq!(item.core_citation.text, citation);
            assert_eq!(
                item.short_form.as_deref(),
                Some("Carter v. Canada (Attorney General)")
            );
            assert_eq!(item.pinpoints[0].text, "7");
        }
    }
    #[test]
    fn article_dates_are_not_authorities() {
        for month in [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ] {
            assert!(citation_occurrences_in_text(&format!("Vice, 21 {month} 2016.")).is_empty());
        }
        assert_eq!(citation_occurrences_in_text("123 Mass 456").len(), 1);
    }

    #[test]
    fn numbered_case_names_do_not_lose_their_first_words() {
        let text = "See 40 Days for Life v. Dietrich, 2024 ONCA 599 at para 2.";
        let occurrences = citation_occurrences_in_text(text);
        assert_eq!(
            occurrences[0].short_form.as_deref(),
            Some("40 Days for Life v. Dietrich")
        );
        assert_eq!(
            &text[occurrences[0].start..occurrences[0].end],
            occurrences[0].text
        );
    }

    #[test]
    fn citation_occurrence_expands_parenthesized_left_party() {
        let text = "Quebec (Attorney General) v. Blaikie, [1979] 2 SCR 1016";
        let occurrence = citation_occurrences_in_text(text).pop().unwrap();
        assert_eq!(occurrence.kind, "case");
        assert_eq!(
            occurrence.styled_citation.text,
            "Quebec (Attorney General) v. Blaikie, [1979] 2 SCR 1016"
        );
        assert_eq!(occurrence.core_citation.text, "[1979] 2 SCR 1016");
        assert_eq!(
            occurrence.short_form.as_deref(),
            Some("Quebec (Attorney General) v. Blaikie")
        );
        assert!(occurrence.reasons.contains(&"same_text_style"));
    }

    #[test]
    fn citation_occurrence_keeps_non_party_parentheticals_out_of_style() {
        for text in [
            "X (1998) v. Smith, 2020 SCC 1",
            "X (2d) v. Smith, 2012 SCC 1",
            "X (see below) v. Smith, 2015 SCC 1",
        ] {
            let occurrence = citation_occurrences_in_text(text).pop().unwrap();
            assert_eq!(occurrence.kind, "case");
            assert_eq!(
                occurrence.styled_citation.text,
                occurrence.core_citation.text
            );
            assert!(!occurrence.reasons.contains(&"same_text_style"));
        }
    }
}
