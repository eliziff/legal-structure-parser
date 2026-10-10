//! CanLII pages citations name, from the shared citation engine: a decision's (its neutral or CanLII citation, the
//! court written) and a statute's or regulation's. No parsing lives here.
use legal_citations::{format::Language, url, Authority, Form, Format, Options};

fn full_citations(text: &str) -> Vec<legal_citations::Citation> {
    legal_citations::extract(text, &Options { resolve: false, parallel: false, ..Default::default() })
        .into_iter().filter(|citation| citation.form == Form::Full).collect()
}

/// Whether the engine's registry routes `court` (a neutral citation's court code, "ABQB") to CanLII.
pub fn canlii_court_route(court: &str) -> bool {
    url::source_canlii_routes().contains_key(court)
}

/// The CanLII page of the first decision `citations` name by a neutral or CanLII citation with its court written
/// ("2023 SCC 14", "1999 CanLII 4334 (NWTSC)"), of `court` where it is given and routed.
pub fn canlii_case_url(citations: &[String], language: &str, court: Option<&str>) -> Option<String> {
    let court = court.map(|court| court.trim().to_uppercase());
    if court.as_deref().is_some_and(|court| !canlii_court_route(court)) { return None; }
    let language = if language.starts_with("fr") { Language::Fr } else { Language::En };
    let text = citations.iter().map(|citation| citation.trim()).filter(|citation| !citation.is_empty())
        .collect::<Vec<_>>().join("\n;\n");
    full_citations(&text).into_iter()
        .filter(|citation| matches!(citation.format, Some(Format::Neutral | Format::CanLii)))
        .filter(|citation| court.is_none() || citation.court.as_ref().map(|written| written.text.to_uppercase()) == court)
        .find_map(|citation| url::url(&citation, language).filter(|page| url::is_canlii_url(page)))
}

/// The CanLII page of the first statute or regulation `text` cites.
pub fn canlii_law_url(text: &str) -> Option<String> {
    full_citations(text).into_iter()
        .filter(|citation| matches!(citation.authority, Authority::Statute | Authority::Regulation))
        .find_map(|citation| url::url(&citation, Language::En).filter(|page| url::is_canlii_url(page)))
}
