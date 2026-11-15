//! A2AJ asked for what the store lacks: the document a citation names, a statute's provision, the decision a CanLII
//! number names, a search, and A2AJ's courts and jurisdictions.

use super::{citation_lookup, document, named_case, named_record, record, record_listing, record_matches, record_provision,
    section, CoreResult, Found, Lookup, LookupForm, Make, Named, ProvisionLookup, Search};
use crate::a2aj::{self, Excerpt, Language, Listing, Query};
use crate::http::{self, Error, DAY};
use legal_structure::{citation_lookup_key, trim_javascript_whitespace as js_trim, windows_1252_punctuation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, LazyLock};

const BASE_URL: &str = "https://api.a2aj.ca";

/// A request's records (`results`): a decision's text is kept for good, anything else a day.
fn results(endpoint: &str, params: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
    // As URLSearchParams writes a query, a value left empty left out.
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in params.iter().filter(|(_, value)| !value.is_empty()) {
        query.append_pair(key, value);
    }
    let kept = if endpoint == "/fetch" && params.contains(&("doc_type", "cases")) { None } else { Some(DAY) };
    let answer = http::json(&format!("{BASE_URL}{endpoint}?{}", query.finish()), kept, "A2AJ")?;
    Ok(answer.get("results").and_then(Value::as_array).cloned().unwrap_or_default())
}

/// A citation searched as the words it is. A2AJ's full-text search reads brackets, parentheses, colons and the like as
/// query syntax, and refuses "[2016] OJ No 1" as a query it cannot parse.
fn literal(text: &str) -> String {
    text.chars().fold(String::new(), |mut query, character| {
        if "+-=&|<>!(){}[]^\"~*?:\\/".contains(character) {
            query.push('\\');
        }
        query.push(character);
        query
    })
}

/// A document looked up: found, or the citation names several documents in the store's inventory (asked without an
/// address), or no document was found.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "A2ajLooked"))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Looked<D> {
    Found { document: D },
    Ambiguous,
    Missing,
}

impl<D> Looked<D> {
    pub fn map<E>(self, make: impl FnOnce(D) -> E) -> Looked<E> {
        match self {
            Looked::Found { document } => Looked::Found { document: make(document) },
            Looked::Ambiguous => Looked::Ambiguous,
            Looked::Missing => Looked::Missing,
        }
    }
}

/// A document asked for by its citation, in a collection and at an address where given; `remote`: whether A2AJ may be
/// asked (lookups leave the computer).
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "A2ajRequest"))]
#[cfg_attr(feature = "binding-types", ts(optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub citation: String,
    pub doc_type: Option<String>,
    pub language: Option<Language>,
    pub dataset: Option<String>,
    pub source_url: Option<String>,
    pub remote: bool,
}

impl Request {
    fn doc_type(&self) -> &'static str {
        a2aj::doc_type(self.doc_type.as_deref().unwrap_or("cases"))
    }
    fn language(&self) -> Language {
        self.language.unwrap_or(Language::En)
    }
}

/// The document a citation names: the store's under each of its forms, else A2AJ's, asked as the Python app asks it
/// (a2aj_client.py, alr_quote_verifier.py) by each form of a court or jurisdiction A2AJ holds, then searched once for
/// the first form, and fetched again by the citation a search's one record gives where that record has no text.
pub fn document_asking<D: Send + Sync + 'static>(store: &Path, request: &Request, make: impl Make<D>)
    -> Result<Looked<Found<D>>, Error> {
    let citation = js_trim(&request.citation);
    if citation.is_empty() {
        return Err("citation is required".to_owned().into());
    }
    let (doc_type, language) = (request.doc_type(), request.language());
    let lookup = citation_lookup(store, citation, doc_type == "laws")?;
    if lookup.ambiguous && request.source_url.as_deref().map(js_trim).unwrap_or_default().is_empty() {
        return Ok(Looked::Ambiguous);
    }
    for form in &lookup.forms {
        let asked = Lookup { citation: form.text.clone(), doc_type: Some(doc_type.to_owned()), language: Some(language),
            dataset: request.dataset.clone(), source_url: request.source_url.clone() };
        if let Some(found) = document(store, &asked, &make)? {
            return Ok(Looked::Found { document: found });
        }
    }
    if !request.remote {
        return Ok(Looked::Missing);
    }
    let read = |found: Option<&Value>| -> CoreResult<Option<Found<D>>> {
        found.map_or(Ok(None), |found| record(found, language, doc_type, &make))
    };
    let asked = covered_forms(&lookup.forms, doc_type);
    for form in &asked {
        let records = &results("/fetch", &[("citation", form.as_str()), ("doc_type", doc_type), ("output_language", language.code())])?;
        if let Some(found) = read(accept(records, form, request, lookup.aliases.clone()))? {
            return Ok(Looked::Found { document: found });
        }
    }
    let Some(form) = asked.first() else { return Ok(Looked::Missing) };
    let dataset = request.dataset.as_deref().map(js_trim).unwrap_or_default();
    let records = &results("/search", &[("query", literal(form).as_str()), ("doc_type", doc_type), ("search_type", "full_text"),
        ("search_language", language.code()), ("size", "10"), ("dataset", dataset)])?;
    let Some(accepted) = accept(records, form, request, lookup.aliases.clone()) else { return Ok(Looked::Missing) };
    if let Some(found) = read(Some(accepted))? {
        return Ok(Looked::Found { document: found });
    }
    let Some(canonical) = record_listing(accepted, language).map(|listing| listing.citation).filter(|citation| !citation.is_empty())
    else { return Ok(Looked::Missing) };
    let records = &results("/fetch", &[("citation", canonical.as_str()), ("doc_type", doc_type), ("output_language", language.code())])?;
    let aliases = lookup.aliases.iter().cloned().chain([citation_lookup_key(&canonical)]).collect();
    Ok(read(accept(records, &canonical, request, aliases))?.map_or(Looked::Missing, |document| Looked::Found { document }))
}

/// The one record answering a citation (by its key or `aliases`), of the collection and at the address asked for:
/// distinct records with the same citation require an explicit address.
fn accept<'a>(records: &'a [Value], citation: &str, request: &Request, aliases: Vec<String>) -> Option<&'a Value> {
    let query = Query { citation: citation.to_owned(), dataset: request.dataset.clone(), source_url: request.source_url.clone(), aliases };
    match record_matches(records, &query)[..] {
        [at] => records.get(at),
        _ => None,
    }
}

/// One provision of a statute found under `request` (`excerpt` its document's fields): the store's where it holds the
/// statute, else A2AJ's answer to a lookup of the provision; None where neither holds its text.
pub fn provision_asking<D>(store: &Path, request: &Request, section_label: &str, excerpt: Excerpt,
    make: impl Make<D>) -> Result<Option<Arc<D>>, Error>
where D: Send + Sync + 'static {
    let (doc_type, language) = (request.doc_type(), request.language());
    let statute = Lookup { citation: excerpt.citation.clone(), doc_type: Some(doc_type.to_owned()), language: Some(language),
        dataset: excerpt.dataset.clone(), source_url: None };
    if let Some(provision) = section(store, &statute, section_label, excerpt.clone(), &make)? {
        return Ok(provision);
    }
    if !request.remote {
        return Ok(None);
    }
    let records = &results("/fetch", &[("citation", js_trim(&request.citation)), ("doc_type", doc_type),
        ("output_language", language.code()), ("section", section_label)])?;
    let lookup = ProvisionLookup { language: Some(language), doc_type: Some(doc_type.to_owned()), dataset: request.dataset.clone(),
        source_url: request.source_url.clone(), section: section_label.to_owned() };
    Ok(record_provision(records, &lookup, excerpt, &make)?)
}

/// The citation of the one decision A2AJ holds under a style of cause, of a year and one of its courts (a CanLII
/// number's, as the host's CanLII index names it): the store's, else A2AJ's search of the name, where `remote`.
pub fn named_asking(store: &Path, named: &Named, language: Language, remote: bool) -> Result<Option<String>, Error> {
    if let Some(citation) = named_case(store, named)? {
        return Ok(Some(citation));
    }
    if !remote {
        return Ok(None);
    }
    let records = &results("/search", &[("query", named.name.as_str()), ("doc_type", "cases"), ("search_type", "name"),
        ("search_language", language.code()), ("size", "10")])?;
    Ok(named_record(records, named, language))
}

/// The documents answering a search, as a search lists them: the store's, else (where the store has no full-text
/// indexes) the first fifty of A2AJ's search where `remote`, each snippet cut to 1,200 characters.
pub fn search_asking(store: &Path, mut search: Search, remote: bool) -> Result<Vec<Listing>, Error> {
    search.query = js_trim(&search.query).to_owned();
    if search.query.is_empty() {
        return Err("query is required".to_owned().into());
    }
    if let Some(found) = super::search(store, &search)? {
        return Ok(found);
    }
    if !remote {
        return Ok(Vec::new());
    }
    let language = search.language.unwrap_or(Language::En);
    let trimmed = |value: &Option<String>| value.as_deref().map(js_trim).unwrap_or_default().to_owned();
    let size = (search.size.map_or(10.0, f64::floor).clamp(1.0, 50.0) as i64).to_string();
    let records = &results("/search", &[("query", crate::search_query::compile(&search.query, None).lucene.as_str()),
        ("doc_type", search.doc_type.as_deref().unwrap_or("cases")), ("search_type", search.search_type.as_deref().unwrap_or("full_text")),
        ("search_language", language.code()), ("size", size.as_str()), ("dataset", trimmed(&search.dataset).as_str()),
        ("start_date", trimmed(&search.start_date).as_str()), ("end_date", trimmed(&search.end_date).as_str()),
        ("sort_results", search.sort_results.as_deref().unwrap_or_default())])?;
    Ok(records.iter().filter_map(|record| record_listing(record, language)).take(50).map(|mut listing| {
        // As JavaScript's slice counts: in UTF-16 code units.
        if let Some(snippet) = &mut listing.snippet {
            let mut units = 0;
            let cut = snippet.char_indices().find(|(_, character)| { units += character.len_utf16(); units > 1200 }).map(|(end, _)| end);
            if let Some(end) = cut {
                snippet.truncate(end);
            }
        }
        listing
    }).collect())
}

/// The forms A2AJ is asked for, as the Python app asks it (alr_quote_verifier.py _a2aj_identity_citation and its
/// coverage check): each whose court or jurisdiction (`covered_by`) A2AJ's coverage holds; where the coverage cannot be
/// read, every form that has one.
fn covered_forms(forms: &[LookupForm], doc_type: &'static str) -> Vec<String> {
    let named = forms.iter().filter_map(|form| Some((&form.text, form.covered_by.as_deref()?))).collect::<Vec<_>>();
    if named.is_empty() {
        return Vec::new();
    }
    let rows = coverage(doc_type).unwrap_or_default();
    let held = rows.iter().map(|row| if doc_type == "laws" { row.jurisdiction_code.as_str() } else { row.dataset.as_str() })
        .collect::<HashSet<_>>();
    named.into_iter().filter(|(_, code)| held.is_empty() || held.contains(code)).map(|(text, _)| text.clone()).collect()
}

/// A court, tribunal or statute collection A2AJ holds, as its coverage lists it.
#[derive(Clone, Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "A2ajCoverage"))]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub dataset: String,
    pub description: String,
    pub description_fr: Option<String>,
    #[cfg_attr(feature = "binding-types", ts(type = r#""cases" | "laws""#))]
    pub doc_type: &'static str,
    pub jurisdiction_code: String,
    pub jurisdiction: String,
    #[cfg_attr(feature = "binding-types", ts(type = r#""court" | "tribunal" | "legislation" | "regulation""#))]
    pub source_kind: &'static str,
    pub earliest_date: Option<String>,
    pub latest_date: Option<String>,
    pub document_count: i64,
}

/// A2AJ's coverage of a kind, as it lists it (kept a day). A row the citation registry cannot place leaves the coverage
/// unread: every form is then asked, as when A2AJ fails.
pub fn coverage(doc_type: &'static str) -> Result<Vec<Coverage>, Error> {
    Ok(results("/coverage", &[("doc_type", doc_type)])?.iter().map(|record| coverage_row(record, doc_type))
        .collect::<Option<Vec<_>>>().unwrap_or_default().into_iter().flatten().collect())
}

/// A row of A2AJ's coverage; none for one without a collection, and None where the registry cannot place it.
fn coverage_row(record: &Value, doc_type: &'static str) -> Option<Option<Coverage>> {
    let Some(record) = record.as_object() else { return Some(None) };
    let text = |field: &str| record.get(field).and_then(Value::as_str).map(js_trim).filter(|value| !value.is_empty())
        .map(|value| windows_1252_punctuation(value).into_owned());
    let Some(dataset) = text("dataset").map(|dataset| dataset.to_uppercase()) else { return Some(None) };
    // A2AJ names a court by its neutral code ("FCA" is also Australia's Federal Court: A2AJ's courts are Canada's).
    let court = legal_citations::registry::registry().courts_by_surface(&dataset).into_iter()
        .find(|court| court.jurisdiction == "ca" || court.jurisdiction.starts_with("ca-"));
    let (jurisdiction_code, jurisdiction) = if doc_type == "laws" { collection_jurisdiction(&dataset)? } else { place(&court?.jurisdiction)? };
    static NAMED_TRIBUNAL: LazyLock<regex::Regex> = LazyLock::new(||
        regex::Regex::new(r"(?i)(?-u:\b)(?:board|commissioner|division|reporter|tribunal)(?-u:\b)").expect("tribunal"));
    let description_en = text("description_en");
    let source_kind = if doc_type == "laws" {
        if dataset.starts_with("REGULATIONS-") { "regulation" } else { "legislation" }
    } else if court.is_some_and(|court| court.level == legal_citations::registry::CourtLevel::Tribunal)
        || NAMED_TRIBUNAL.is_match(description_en.as_deref().unwrap_or_default()) {
        "tribunal"
    } else {
        "court"
    };
    Some(Some(Coverage {
        description: description_en.or_else(|| text("description_fr")).unwrap_or_else(|| dataset.clone()),
        description_fr: text("description_fr"),
        doc_type,
        jurisdiction_code,
        jurisdiction,
        source_kind,
        earliest_date: text("earliest_document_date"),
        latest_date: text("latest_document_date"),
        document_count: record.get("number_of_documents")
            .and_then(|count| count.as_i64().or_else(|| count.as_str()?.trim().parse().ok())).unwrap_or(0),
        dataset,
    }))
}

/// A Canadian jurisdiction as A2AJ codes it ("FED", "ON") and names it, from its registry id: a province's name is the
/// registry's; Canada's own laws and courts are "Federal" in A2AJ's lists.
fn place(id: &str) -> Option<(String, String)> {
    match id.strip_prefix("ca-") {
        Some(province) => Some((province.to_uppercase(), legal_citations::registry::registry().jurisdiction(id)?.name.en.clone())),
        None => (id == "ca").then(|| ("FED".to_owned(), "Federal".to_owned())),
    }
}

/// The jurisdiction a statute collection's suffix names ("LEGISLATION-ON", "-FED"; A2AJ writes Yukon "YK").
pub fn collection_jurisdiction(collection: &str) -> Option<(String, String)> {
    let suffix = collection.rsplit_once('-')?.1.to_lowercase();
    match suffix.as_str() { "fed" => place("ca"), "yk" => place("ca-yt"), suffix => place(&format!("ca-{suffix}")) }
}
