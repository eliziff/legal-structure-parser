//! A decision a publisher abroad holds, found by the citation a text gives: a UK neutral citation at the National
//! Archives' Find Case Law (its page worked out from the citation), an Employment Tribunal case number at GOV.UK, a US
//! federal docket at GovInfo. Each is read as its publisher's markup, with the PDFs it publishes.

use crate::http::{self, Error, DAY};
use legal_citations::reference_names::read_citations;
use legal_citations::{Citation, Format};
use legal_structure::NativeMarkupInput;
use serde::Serialize;
use serde_json::Value;
use std::sync::LazyLock;

/// A file a publisher offers with a decision.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub url: String,
    pub title: Option<String>,
    pub content_type: Option<String>,
    pub filename: Option<String>,
    pub page_count: Option<f64>,
}

/// A decision found: its publisher, the id the publisher files it under, its citation and title, its page, the one PDF
/// the publisher gives of it, the PDFs it offers, and its markup to be read.
pub struct Found {
    pub provider: &'static str,
    pub id: String,
    pub citation: String,
    pub title: Option<String>,
    pub url: String,
    pub pdf_url: Option<String>,
    pub attachments: Vec<Attachment>,
    pub input: NativeMarkupInput,
}

enum Asked {
    Tna { url: String, citation: String },
    GovUkEt { number: String },
    GovInfo { docket: String },
}

/// What the text's citations ask each publisher for, in order.
fn asked(text: &str) -> Vec<(&'static str, Asked)> {
    let read = read_citations(text);
    let mut asked = Vec::new();
    let neutral = legal_structure::provider_citations(text, &read).into_iter()
        .find(|cited| cited.family == "neutral" && cited.jurisdiction.as_deref() == Some("uk"));
    if let (Some(url), Some(cited)) = (read.iter().find_map(legal_citations::url::uk_caselaw), neutral) {
        asked.push(("tna", Asked::Tna { url, citation: cited.text.split_whitespace().collect::<Vec<_>>().join(" ") }));
    }
    let dockets = |court: fn(&Citation) -> bool| {
        let mut dockets = read.iter().filter(|cite| cite.format == Some(Format::Docket) && court(cite))
            .filter_map(|cite| cite.fields.docket.clone()).collect::<Vec<_>>();
        // One docket cited twice is one; the first stays first.
        let mut seen = std::collections::HashSet::new();
        dockets.retain(|docket| seen.insert(docket.clone()));
        dockets
    };
    // A text naming two tribunal cases names neither.
    if let [number] = &dockets(|cite| cite.reasons.iter().any(|reason| reason == "employment_tribunal_grammar"))[..] {
        asked.push(("govuk-et", Asked::GovUkEt { number: number.clone() }));
    }
    let federal = |cite: &Citation| cite.jurisdiction.as_deref().is_some_and(|place| place == "us" || place.starts_with("us-"));
    if let Some(docket) = dockets(federal).first() {
        asked.push(("govinfo", Asked::GovInfo { docket: docket.to_lowercase() }));
    }
    asked
}

/// The publishers abroad the text's citations name, in the order they are asked.
pub fn claimed(text: &str) -> Vec<&'static str> {
    asked(text).into_iter().map(|(provider, _)| provider).collect()
}

/// The decision the text's citations name, from the first publisher that holds it (`only`: that publisher alone); a
/// source kept from an earlier lookup (`saved`, its page) is read there, not searched for again.
pub fn foreign_case(text: &str, only: Option<&str>, saved: Option<&str>) -> Result<Option<Found>, Error> {
    for (provider, asked) in asked(text) {
        if only.is_some_and(|only| only != provider) {
            continue;
        }
        let found = match asked {
            Asked::Tna { url, citation } => tna(url, citation)?,
            Asked::GovUkEt { number } => gov_uk_et(number, saved)?,
            Asked::GovInfo { docket } => gov_info(docket, saved)?,
        };
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

/// A federal law no consolidated act of A2AJ's holds, as Justice Laws publishes it: its id (the citation's key), the
/// citation it is filed under, its title, its page and its official PDF.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct JusticeLaws {
    pub id: String,
    pub citation: String,
    pub title: String,
    pub url: String,
    pub pdf_url: String,
}

/// The Constitution Act or annual statute of Canada the text cites, from Justice Laws: a Constitution Act at once, an
/// annual statute where Justice Laws' page names it (its title), which is asked only where lookups may leave the
/// computer (`remote`).
pub fn justice_laws(text: &str, remote: bool) -> Result<Option<JusticeLaws>, Error> {
    // Claimed by the citation's key, never its wording ("the Canadian Charter of Rights and Freedoms" is the Canada Act
    // 1982's), the key without its version being the id the provider has always filed it under.
    let key = legal_structure::citation_lookup_key(text);
    let Some(law) = legal_citations::url::justice_laws_unconsolidated(&key, legal_citations::format::Language::En) else { return Ok(None) };
    let id = key.split_once(':').map_or(key.as_str(), |(_, key)| key).to_owned();
    if let Some(title) = law.constitution {
        // The brief's own citation stays the authority's, so the index prints the Act once.
        return Ok(Some(JusticeLaws { id, citation: text.trim().to_owned(), title: title.to_owned(), url: law.url, pdf_url: law.pdf_url }));
    }
    if !remote {
        return Ok(None);
    }
    let Some(page) = published(&law.url, "text/html", None, Some(30 * DAY), "Justice Laws")? else { return Ok(None) };
    static TITLE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)<title>([^<]{1,300})</title>").expect("title"));
    let Some(title) = TITLE.captures(&page).map(|found| legal_structure::decode_entities(&found[1]).split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|title| !title.to_lowercase().starts_with("error") && !title.to_lowercase().starts_with("page not found"))
    else { return Ok(None) };
    Ok(Some(JusticeLaws { id, citation: law.citation, title, url: law.url, pdf_url: law.pdf_url }))
}

/// What `url` publishes (a POST of `body` where given), kept `kept`; none where it publishes nothing there (404).
fn published(url: &str, accept: &str, body: Option<&str>, kept: Option<u64>, label: &str) -> Result<Option<String>, Error> {
    let reply = http::send(url, accept, body, None, kept)?;
    match reply.status {
        404 => Ok(None),
        200..=299 => Ok(Some(reply.body)),
        status => Err(format!("{label} request failed ({status}).").into()),
    }
}

/// A JSON answer as an object (any other value an empty one); none where nothing is published.
fn object(url: &str, body: Option<&str>, label: &str) -> Result<Option<Value>, Error> {
    Ok(published(url, "application/json", body, Some(DAY), label)?
        .map(|text| serde_json::from_str::<Value>(&text).ok().filter(Value::is_object).unwrap_or_else(|| Value::Object(Default::default()))))
}

/// A field's text, trimmed (an XML-shaped `{"#text": …}` read as its text); none where empty.
fn text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value.as_str().or_else(|| value.get("#text")?.as_str()).map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}

/// A publisher's address read against `base`, only on `hosts` over https on the default port, its API key dropped.
fn on_host(value: Option<&Value>, base: &str, hosts: &[&str]) -> Option<String> {
    let mut url = url::Url::parse(base).ok()?.join(&text(value)?).ok()?;
    if url.query().is_some() {
        let kept = url.query_pairs().filter(|(key, _)| key != "api_key").map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        if kept.is_empty() { url.set_query(None) } else { url.query_pairs_mut().clear().extend_pairs(kept); }
    }
    (url.scheme() == "https" && url.port().is_none() && url.username().is_empty() && url.password().is_none()
        && url.host_str().is_some_and(|host| hosts.contains(&host))).then(|| url.into())
}

fn count(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value.as_f64().or_else(|| value.as_str().map(str::trim).filter(|text| !text.is_empty())?.parse().ok()).filter(|count| *count >= 0.0)
}

fn query(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish()
}

fn tna(url: String, citation: String) -> Result<Option<Found>, Error> {
    let label = "The National Archives";
    let Some(xml) = published(&format!("{url}/data.xml"), "application/akn+xml, application/xml, text/xml", None, Some(7 * DAY), label)?
    else { return Ok(None) };
    // A DOCTYPE could declare entities; a judgment's Akoma Ntoso never carries one.
    if xml.contains("<!DOCTYPE") {
        return Err(format!("{label}' XML for {citation} declares a DOCTYPE.").into());
    }
    static NAME: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"<(?:\w+:)?FRBRname\s[^>]*value="([^"]+)""#).expect("FRBRname"));
    let title = NAME.captures(&xml).map(|name| legal_structure::decode_entities(&name[1]).into_owned());
    let input = NativeMarkupInput::published("tna", &citation, &url, String::new(), Some(xml), Some(citation.clone()), false)
        .ok_or("The National Archives' reader is not built in.".to_owned())?;
    Ok(Some(Found { provider: "tna", id: citation.clone(), citation, title, pdf_url: Some(format!("{url}/data.pdf")), url,
        attachments: Vec::new(), input }))
}

fn gov_uk_et(number: String, saved: Option<&str>) -> Result<Option<Found>, Error> {
    const ORIGIN: &str = "https://www.gov.uk";
    let label = "GOV.UK";
    let decision = |link: &Value| on_host(Some(link), ORIGIN, &["www.gov.uk"])
        .filter(|url| url::Url::parse(url).is_ok_and(|url| url.path().starts_with("/employment-tribunal-decisions/")));
    let mut pages = match saved.and_then(|saved| decision(&Value::from(saved))) {
        Some(url) => vec![url],
        None => {
            let search = format!("{ORIGIN}/api/search.json?{}", query(&[("q", &number), ("filter_format", "employment_tribunal_decision"), ("count", "50")]));
            let Some(found) = object(&search, None, label)? else { return Ok(None) };
            found.get("results").and_then(Value::as_array).into_iter().flatten().filter_map(|item| {
                let title = text(item.get("title"))?;
                (text(item.get("format")).as_deref() == Some("employment_tribunal_decision") && title.to_uppercase().contains(&number))
                    .then(|| decision(item.get("link")?)).flatten()
            }).collect::<Vec<_>>()
        }
    };
    // A decision the search lists twice is one.
    pages.sort_unstable();
    pages.dedup();
    let [url] = &pages[..] else { return Ok(None) };
    let path = url::Url::parse(url).map_err(|error| error.to_string())?.path().to_owned();
    let Some(content) = object(&format!("{ORIGIN}/api/content{path}"), None, label)?
    else { return Err(format!("GOV.UK lists {number} but publishes no content for it.").into()) };
    let title = text(content.get("title"));
    let description = text(content.get("description"));
    let shown = [&title, &description].into_iter().flatten().cloned().collect::<Vec<_>>();
    let details = content.get("details");
    let hidden = details.and_then(|details| details.get("hidden_indexable_content")).map(|hidden| match hidden {
        Value::Array(parts) => parts.iter().filter_map(Value::as_str).filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n"),
        other => other.as_str().unwrap_or_default().to_owned(),
    }).unwrap_or_default();
    let escape = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let markup = (!hidden.is_empty()).then(|| shown.iter().map(|part| format!("<p>{}</p>", escape(part))).collect::<String>() + &hidden);
    let attachments = details.and_then(|details| details.get("attachments")).and_then(Value::as_array).into_iter().flatten()
        .filter_map(|attachment| Some(Attachment {
            url: on_host(attachment.get("url"), ORIGIN, &["www.gov.uk", "assets.publishing.service.gov.uk"])?,
            title: text(attachment.get("title")),
            content_type: text(attachment.get("content_type").or_else(|| attachment.get("contentType"))),
            filename: text(attachment.get("filename")),
            page_count: count(attachment.get("number_of_pages").or_else(|| attachment.get("page_count"))),
        })).collect::<Vec<_>>();
    static PDF: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?i)\.pdf(?:$|[?#])").expect("pdf"));
    let mut pdfs = attachments.iter().filter(|attachment| attachment.content_type.as_deref() == Some("application/pdf")
        || PDF.is_match(&attachment.url)).map(|attachment| attachment.url.clone()).collect::<Vec<_>>();
    pdfs.dedup();
    let input = NativeMarkupInput::published("govuk-et", &number, url, shown.join("\n\n"), markup, None, true)
        .ok_or("GOV.UK's reader is not built in.".to_owned())?;
    Ok(Some(Found { provider: "govuk-et", id: number.clone(), citation: number, title, url: url.clone(),
        pdf_url: (pdfs.len() == 1).then(|| pdfs.remove(0)), attachments, input }))
}

fn gov_info(docket: String, saved: Option<&str>) -> Result<Option<Found>, Error> {
    const API: &str = "https://api.govinfo.gov";
    let label = "GovInfo";
    let key = std::env::var("GOVINFO_API_KEY").ok().map(|key| key.trim().to_owned()).filter(|key| !key.is_empty())
        .unwrap_or_else(|| "DEMO_KEY".to_owned());
    let key = query(&[("api_key", &key)]);
    static PACKAGE: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^USCOURTS-[A-Za-z0-9]+-([A-Za-z0-9_-]+)$").expect("package"));
    let expected = docket.replacen(':', "_", 1);
    let matches = |package: &str| PACKAGE.captures(package).is_some_and(|found| found[1].to_lowercase() == expected);
    // A package kept from an earlier lookup is read at its page's package, not searched for again.
    let kept = saved.and_then(|saved| saved.strip_prefix("https://www.govinfo.gov/app/details/"))
        .map(|package| package.trim_end_matches('/')).filter(|package| matches(package));
    let mut packages = match kept {
        Some(package) => vec![(package.to_owned(), None)],
        None => {
            let body = serde_json::json!({ "query": format!("collection:(USCOURTS) and casenumber:({docket})"), "pageSize": "10",
                "offsetMark": "*", "sorts": [{ "field": "score", "sortOrder": "DESC" }] }).to_string();
            let Some(found) = object(&format!("{API}/search?{key}"), Some(&body), label)? else { return Ok(None) };
            found.get("results").and_then(Value::as_array).into_iter().flatten().filter_map(|result| {
                let package = text(result.get("packageId"))?;
                (text(result.get("collectionCode")).as_deref() == Some("USCOURTS") && matches(&package)).then(|| (package, text(result.get("title"))))
            }).collect::<Vec<_>>()
        }
    };
    packages.sort_by(|left, right| left.0.cmp(&right.0));
    packages.dedup_by(|left, right| left.0 == right.0);
    let [(package, listed)] = &packages[..] else { return Ok(None) };
    // The package id is URL-safe: PACKAGE admits only letters, digits, "_" and "-".
    let Some(summary) = object(&format!("{API}/packages/{package}/summary?{key}"), None, label)?
    else { return Err(format!("GovInfo lists {package} but publishes no summary of it.").into()) };
    let title = text(summary.get("title")).or_else(|| listed.clone());
    let text_of = [title.clone(), text(summary.get("caseNumber")).or_else(|| Some(docket.clone())), text(summary.get("courtName")),
        text(summary.get("dateIssued")), text(summary.get("docketText")), text(summary.get("description"))]
        .into_iter().flatten().collect::<Vec<_>>().join("\n");
    let url = format!("https://www.govinfo.gov/app/details/{package}");
    let pdf = summary.get("download").and_then(|download| download.get("pdfLink")).or_else(|| summary.get("pdfLink"));
    let attachments = on_host(pdf, API, &["api.govinfo.gov", "www.govinfo.gov"]).map(|pdf| Attachment { url: pdf,
        title: text(summary.get("title")), content_type: Some("application/pdf".to_owned()),
        filename: text(summary.get("pdfFileName").or_else(|| summary.get("filename"))),
        page_count: count(summary.get("pageCount").or_else(|| summary.get("numberOfPages")).or_else(|| summary.get("pages"))) })
        .into_iter().collect::<Vec<_>>();
    let input = NativeMarkupInput::published("govinfo", package, &url, text_of, None, None, true)
        .ok_or("GovInfo's reader is not built in.".to_owned())?;
    Ok(Some(Found { provider: "govinfo", id: package.clone(), citation: docket, title, url,
        pdf_url: attachments.first().map(|pdf| pdf.url.clone()), attachments, input }))
}
