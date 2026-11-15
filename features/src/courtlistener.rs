//! CourtListener's US decisions: the case a reporter citation names, a case with its opinions, and a search. The bulk
//! store on this computer (`courtlistener.sqlite`) answers first; CourtListener's API answers what it does not, with the
//! API's key. Each opinion comes as the record the opinion reader reads whole (`legal_structure::opinion`).

use crate::http::{self, Error, DAY};
use crate::sqlite::{self, sql};
use legal_citations::reference_names::read_citations;
use legal_structure::opinion::{OpinionCitation, OpinionRecord};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;

const API: &str = "https://www.courtlistener.com/api/rest/v4";
const WEB: &str = "https://www.courtlistener.com";
const STORAGE: &str = "https://storage.courtlistener.com";

/// Where CourtListener is read: its bulk store's path, the API's key, and whether lookups may leave the computer.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "CourtListenerAccess", optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct Access {
    pub store: PathBuf,
    pub token: Option<String>,
    pub remote: bool,
}

/// One opinion of a case: its id, type, author, page, PDF, and the record the opinion reader reads (none where
/// CourtListener holds no rendition of it).
pub struct Opinion {
    pub id: Option<u64>,
    pub kind: Option<String>,
    pub author: Option<String>,
    pub url: Option<String>,
    pub pdf_url: Option<String>,
    pub record: Option<OpinionRecord>,
}

/// A case: its cluster's id, name and date, its page, its one PDF, its citations as printed, and its opinions.
pub struct Case {
    pub id: u64,
    pub case_name: Option<String>,
    pub date_filed: Option<String>,
    pub url: String,
    pub pdf_url: Option<String>,
    pub citations: Vec<String>,
    pub opinions: Vec<Opinion>,
}

/// A case a search lists.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "CourtListenerHit"))]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub cluster_id: Option<u64>,
    pub case_name: Option<String>,
    pub citation: Option<String>,
    pub court: Option<String>,
    pub date_filed: Option<String>,
    pub snippet: Option<String>,
    pub url: Option<String>,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "CourtListenerSearch", optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct Search {
    pub query: String,
    pub court: Option<String>,
    pub filed_after: Option<String>,
    pub filed_before: Option<String>,
    pub limit: Option<f64>,
}

/// Each rendition CourtListener stores, by the file name the opinion reader reads it under.
const RENDITIONS: [(&str, &str); 7] = [("html_with_citations.html", "html_with_citations"), ("xml_harvard.xml", "xml_harvard"),
    ("html_columbia.html", "html_columbia"), ("html_lawbox.html", "html_lawbox"), ("html_anon_2020.html", "html_anon_2020"),
    ("html.html", "html"), ("plain_text.txt", "plain_text")];

/// A field's text as written, the first of `keys` that is not blank.
fn text(record: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| record.get(*key)?.as_str().filter(|text| !text.trim().is_empty()).map(str::to_owned))
}

fn web(path: &str) -> String {
    if path.starts_with("http") { path.to_owned() } else { format!("{WEB}{path}") }
}

fn storage(path: &str) -> String {
    if path.starts_with("http") { path.to_owned() } else { format!("{STORAGE}/{}", path.trim_start_matches('/')) }
}

/// The one id among `ids`; none where there are none, or several.
fn unique(mut ids: Vec<u64>) -> Option<u64> {
    ids.sort_unstable();
    ids.dedup();
    match ids[..] { [id] => Some(id), _ => None }
}

fn unique_pdf(opinions: &[Opinion]) -> Option<String> {
    let mut pdfs = opinions.iter().filter_map(|opinion| opinion.pdf_url.clone()).collect::<Vec<_>>();
    pdfs.sort();
    pdfs.dedup();
    (pdfs.len() == 1).then(|| pdfs.remove(0))
}

/// An API record or a bulk row, as the opinion reader reads it with its case's fields and citations.
fn opinion(record: &Map<String, Value>, cluster: &[(String, String)], citations: &[OpinionCitation]) -> Opinion {
    let id = record.get("id").and_then(Value::as_u64);
    let kind = text(record, &["type"]);
    let author = text(record, &["author_str", "author"])
        .or_else(|| text(record.get("author")?.as_object()?, &["name"]));
    let renditions = RENDITIONS.iter().filter_map(|(file, field)| Some(((*file).to_owned(), text(record, &[field])?))).collect::<Vec<_>>();
    let page_count = record.get("page_count").and_then(Value::as_u64).and_then(|count| u32::try_from(count).ok());
    Opinion {
        url: text(record, &["absolute_url", "url"]).map(|path| web(&path)),
        pdf_url: text(record, &["local_path"]).map(|path| storage(&path)),
        record: (!renditions.is_empty()).then(|| OpinionRecord { id: id.map(|id| id.to_string()).unwrap_or_default(),
            opinion_type: kind.clone(), author: author.clone(), page_count, renditions, citations: citations.to_vec(),
            cluster: cluster.to_vec() }),
        id, kind, author,
    }
}

/// Every row `query` returns, each as its columns by name.
fn rows(database: &Connection, query: &str, params: impl Params) -> Result<Vec<Map<String, Value>>, String> {
    let mut statement = database.prepare_cached(query).map_err(sql)?;
    let names = statement.column_names().into_iter().map(str::to_owned).collect::<Vec<_>>();
    let mut found = statement.query(params).map_err(sql)?;
    let mut rows = Vec::new();
    while let Some(row) = found.next().map_err(sql)? {
        let mut columns = Map::new();
        for (index, name) in names.iter().enumerate() {
            columns.insert(name.clone(), match row.get_ref(index).map_err(sql)? {
                ValueRef::Integer(number) => number.into(),
                ValueRef::Real(number) => number.into(),
                ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned().into(),
                ValueRef::Null | ValueRef::Blob(_) => Value::Null,
            });
        }
        rows.push(columns);
    }
    Ok(rows)
}

/// CourtListener's API's answer at `address` (a form posted where `form` is given, never kept; else kept a day); none
/// where nothing is published there. An address the API hands back is followed only on CourtListener, which the key goes to.
fn api(access: &Access, address: &str, form: Option<&str>) -> Result<Option<Value>, Error> {
    let url = if address.starts_with("http") {
        let url = url::Url::parse(address).map_err(|error| error.to_string())?;
        if url.scheme() != "https" || url.host_str() != Some("www.courtlistener.com") || url.port().is_some() {
            return Err(format!("CourtListener handed back an address off its own site: {address}").into());
        }
        url.into()
    } else {
        format!("{API}{address}")
    };
    let key = access.token.as_deref().map(|token| format!("Token {token}"));
    let reply = http::send(&url, "application/json", form, key.as_deref(), Some(if form.is_some() { 0 } else { DAY }))?;
    match reply.status {
        404 => Ok(None),
        200..=299 => serde_json::from_str(&reply.body).map(Some).map_err(|error| format!("CourtListener answered unreadable JSON: {error}").into()),
        401 | 403 => Err("CourtListener authentication failed.".to_owned().into()),
        status => Err(format!("CourtListener request failed ({status}).").into()),
    }
}

/// The text cut to `units` UTF-16 code units, as JavaScript's slice cuts it.
fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut counted = 0;
    text.char_indices().find(|(_, character)| { counted += character.len_utf16(); counted > units }).map_or(text, |(end, _)| &text[..end])
}

/// The one case the text's US reporter citation names: the bulk store's, where it holds the citation (an ambiguous hit
/// is its answer too: the API is not asked to choose), else CourtListener's citation lookup, with its key.
pub fn cluster_of(access: &Access, text: &str) -> Result<Option<u64>, Error> {
    let mut asked = text.trim().to_owned();
    if asked.is_empty() {
        return Ok(None);
    }
    if access.store.exists() {
        let read = read_citations(text);
        let reporter = legal_structure::provider_citations(text, &read).into_iter().find(|cited| cited.family == "reporter")
            .and_then(|cited| Some((cited.volume?, cited.reporter?.split_whitespace().collect::<Vec<_>>().join(" "), cited.page?)));
        if let Some((volume, reporter, page)) = reporter {
            asked = format!("{volume} {reporter} {page}");
            let key = reporter.to_lowercase().chars().filter(char::is_ascii_alphanumeric).collect::<String>();
            let ids = sqlite::read(&access.store, |database, _| rows(database,
                "SELECT DISTINCT cluster.id FROM citation JOIN cluster ON cluster.id = citation.cluster_id
                 WHERE citation.volume = ?1 AND citation.reporter_key = ?2 AND citation.page = ?3
                 ORDER BY cluster.date_filed DESC, cluster.id LIMIT 20", (volume.trim(), key, page.trim())))?.unwrap_or_default();
            if !ids.is_empty() {
                return Ok(unique(ids.iter().filter_map(|row| row.get("id")?.as_u64()).collect()));
            }
        }
        if access.token.is_none() {
            return Ok(None);
        }
    }
    if !access.remote {
        return Ok(None);
    }
    let form = url::form_urlencoded::Serializer::new(String::new()).append_pair("text", utf16_prefix(&asked, 64_000)).finish();
    let Some(found) = api(access, "/citation-lookup/", Some(&form))? else { return Ok(None) };
    Ok(unique(found.as_array().into_iter().flatten()
        .flat_map(|row| row.get("clusters").and_then(Value::as_array).into_iter().flatten())
        .filter(|cluster| cluster.get("absolute_url").and_then(Value::as_str).is_some_and(|url| !url.trim().is_empty()))
        .filter_map(|cluster| cluster.get("id")?.as_u64()).collect()))
}

/// A case and its opinions (`opinion` alone where given): the bulk store's, else CourtListener's API's.
pub fn case(access: &Access, id: u64, opinion_id: Option<u64>) -> Result<Option<Case>, Error> {
    if let Some(case) = sqlite::read(&access.store, |database, _| stored_case(database, id, opinion_id))?.flatten() {
        return Ok(Some(case));
    }
    if !access.remote {
        return Ok(None);
    }
    api_case(access, id, opinion_id).map(Some)
}

fn stored_case(database: &Connection, id: u64, opinion_id: Option<u64>) -> Result<Option<Case>, String> {
    let id_value = id as i64;
    let Some(cluster) = rows(database, "SELECT * FROM cluster WHERE id = ?1", [id_value])?.into_iter().next() else { return Ok(None) };
    let printed = rows(database, "SELECT volume, reporter, page FROM citation WHERE cluster_id = ?1 ORDER BY id LIMIT 100", [id_value])?;
    let part = |row: &Map<String, Value>, field: &str| match row.get(field) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    };
    let citations = printed.iter().map(|row| OpinionCitation { volume: part(row, "volume"),
        reporter: part(row, "reporter").split_whitespace().collect::<Vec<_>>().join(" "), page: part(row, "page"), kind: String::new() })
        .filter(|cite| !cite.volume.is_empty() && !cite.reporter.is_empty() && !cite.page.is_empty()).collect::<Vec<_>>();
    // The store keeps the case's names and date under CourtListener's own field names.
    let fields = ["case_name", "case_name_full", "case_name_short", "date_filed"].iter()
        .filter_map(|field| Some(((*field).to_owned(), text(&cluster, &[field])?))).collect::<Vec<_>>();
    let opinions = rows(database, "SELECT * FROM opinion WHERE cluster_id = ?1 ORDER BY id", [id_value])?.iter()
        .filter(|row| opinion_id.is_none() || row.get("id").and_then(Value::as_u64) == opinion_id)
        .map(|row| opinion(row, &fields, &citations)).collect::<Vec<_>>();
    let url = match text(&cluster, &["slug"]) {
        Some(slug) => format!("{WEB}/opinion/{id}/{slug}/"),
        None => format!("{WEB}/opinion/{id}/"),
    };
    Ok(Some(Case { id, case_name: text(&cluster, &["case_name", "case_name_full", "case_name_short"]),
        date_filed: text(&cluster, &["date_filed"]), url,
        pdf_url: text(&cluster, &["filepath_pdf_harvard"]).map(|path| storage(&path)).or_else(|| unique_pdf(&opinions)),
        citations: printed.iter().map(|row| format!("{} {} {}", part(row, "volume"), part(row, "reporter"), part(row, "page"))).collect(),
        opinions }))
}

fn api_case(access: &Access, id: u64, opinion_id: Option<u64>) -> Result<Case, Error> {
    let cluster = api(access, &format!("/clusters/{id}/"), None)?.and_then(|cluster| cluster.as_object().cloned())
        .ok_or_else(|| format!("CourtListener has no cluster {id}."))?;
    let field = |row: &Value, name: &str| match row.get(name) {
        Some(Value::String(text)) => text.trim().to_owned(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    };
    let citations = cluster.get("citations").and_then(Value::as_array).into_iter().flatten()
        .map(|row| OpinionCitation { volume: field(row, "volume"), reporter: field(row, "reporter"), page: field(row, "page"), kind: field(row, "type") })
        .filter(|cite| !cite.volume.is_empty() && !cite.reporter.is_empty() && !cite.page.is_empty()).collect::<Vec<_>>();
    let fields = cluster.iter().filter_map(|(name, value)| Some((name.clone(), value.as_str().filter(|text| !text.is_empty())?.to_owned())))
        .collect::<Vec<_>>();
    let mut opinions = Vec::new();
    let mut next = Some(format!("/opinions/?cluster={id}"));
    for _ in 0..10 {
        let Some(address) = next.take() else { break };
        let page = api(access, &address, None)?.unwrap_or_default();
        opinions.extend(page.get("results").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_object)
            .filter(|record| opinion_id.is_none() || record.get("id").and_then(Value::as_u64) == opinion_id)
            .map(|record| opinion(record, &fields, &citations)));
        if opinion_id.is_some() && !opinions.is_empty() {
            break;
        }
        next = text(page.as_object().unwrap_or(&Map::new()), &["next"]);
    }
    let pdf_url = text(&cluster, &["filepath_pdf_harvard"]).or_else(|| text(&cluster, &["filepath_pdf_scan"])).map(|path| storage(&path))
        .or_else(|| unique_pdf(&opinions));
    Ok(Case { id, case_name: text(&cluster, &["case_name", "case_name_full", "case_name_short"]),
        date_filed: text(&cluster, &["date_filed"]),
        url: text(&cluster, &["absolute_url"]).map(|path| web(&path)).or_else(|| opinions.first().and_then(|opinion| opinion.url.clone()))
            .unwrap_or_else(|| format!("{WEB}/opinion/{id}/")),
        pdf_url, citations: citations.iter().map(|cite| format!("{} {} {}", cite.volume, cite.reporter, cite.page)).collect(), opinions })
}

/// The cases a search finds: the bulk store's full-text indexes where no court is named (its cases first, then those
/// of its opinions), unless they find none and the API can be asked; else CourtListener's search.
pub fn search(access: &Access, search: &Search) -> Result<Vec<Hit>, Error> {
    let query = search.query.trim();
    if query.is_empty() {
        return Err("query is required.".to_owned().into());
    }
    let limit = search.limit.map_or(10.0, f64::floor).clamp(1.0, 20.0) as usize;
    let court = search.court.as_deref().map(str::trim).filter(|court| !court.is_empty());
    let dated = |field: &Option<String>| field.as_deref().map(str::trim).filter(|date| !date.is_empty()).map(str::to_owned);
    let (after, before) = (dated(&search.filed_after), dated(&search.filed_before));
    let local = match court {
        Some(_) => None,
        None => sqlite::read(&access.store, |database, _| stored_search(database, query, limit, &after, &before))?,
    };
    if let Some(local) = local.filter(|local| !local.is_empty() || access.token.is_none()) {
        return Ok(local);
    }
    if !access.remote {
        return Ok(Vec::new());
    }
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    params.append_pair("type", "o").append_pair("q", &crate::search_query::compile(query, None).lucene);
    for (name, value) in [("court", court.map(str::to_owned)), ("filed_after", after), ("filed_before", before)] {
        if let Some(value) = value { params.append_pair(name, &value); }
    }
    let found = api(access, &format!("/search/?{}", params.finish()), None)?.unwrap_or_default();
    let label = |citation: &Value| match citation {
        Value::String(text) => Some(text.clone()),
        Value::Object(record) => {
            let part = |name: &str| record.get(name).map(|value| value.as_str().map_or_else(|| value.to_string(), |text| text.trim().to_owned()))
                .unwrap_or_default();
            Some([part("volume"), part("reporter"), part("page")].into_iter().filter(|part| !part.is_empty() && part != "null")
                .collect::<Vec<_>>().join(" ")).filter(|label| !label.is_empty())
        }
        _ => None,
    };
    Ok(found.get("results").and_then(Value::as_array).into_iter().flatten().take(limit).filter_map(Value::as_object).map(|result| Hit {
        cluster_id: result.get("cluster_id").and_then(Value::as_u64).or_else(|| result.get("cluster")?.get("id")?.as_u64()),
        case_name: text(result, &["caseName", "case_name", "caseNameFull"]),
        citation: text(result, &["citation"]).or_else(|| result.get("citation")?.as_array()
            .map(|citations| citations.iter().filter_map(label).collect::<Vec<_>>().join("; "))),
        court: text(result, &["court", "court_id", "court_citation_string"]),
        date_filed: text(result, &["dateFiled", "date_filed"]),
        snippet: text(result, &["snippet"]),
        url: text(result, &["absolute_url"]).map(|path| web(&path)),
    }).collect())
}

fn stored_search(database: &Connection, query: &str, limit: usize, after: &Option<String>, before: &Option<String>)
    -> Result<Vec<Hit>, String> {
    if !query.chars().any(char::is_alphanumeric) {
        return Ok(Vec::new());
    }
    let fts = crate::search_query::compile(query, None).fts5;
    let dates = [after.as_ref().map(|_| "cluster.date_filed >= ?3"), before.as_ref().map(|_| if after.is_some() { "cluster.date_filed <= ?4" } else { "cluster.date_filed <= ?3" })]
        .into_iter().flatten().map(|filter| format!(" AND {filter}")).collect::<String>();
    let ranked = |index: &str, join: &str, size: usize| -> Result<Vec<Map<String, Value>>, String> {
        let query = format!("SELECT cluster.id, cluster.case_name, cluster.case_name_short, cluster.case_name_full, cluster.slug,
            cluster.date_filed FROM {index} JOIN cluster ON {join} WHERE {index} MATCH ?1{dates} ORDER BY rank LIMIT ?2");
        let values = [Some(fts.clone()), Some(size.to_string()), after.clone(), before.clone()].into_iter().flatten().collect::<Vec<_>>();
        rows(database, &query, rusqlite::params_from_iter(values))
    };
    let mut found = ranked("cluster_search", "cluster.id = cluster_search.rowid", limit)?;
    let opinions = rows(database, "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'opinion_search'", [])?;
    if found.len() < limit && !opinions.is_empty() {
        for row in ranked("opinion_search", "cluster.id = CAST(opinion_search.cluster_id AS INTEGER)", (limit * 4).max(50))? {
            if !found.iter().any(|seen| seen.get("id") == row.get("id")) {
                found.push(row);
            }
        }
        found.truncate(limit);
    }
    Ok(found.iter().map(|cluster| {
        let id = cluster.get("id").and_then(Value::as_u64);
        Hit { cluster_id: id, case_name: text(cluster, &["case_name", "case_name_full", "case_name_short"]), citation: None, court: None,
            date_filed: text(cluster, &["date_filed"]), snippet: None,
            url: id.map(|id| match text(cluster, &["slug"]) { Some(slug) => format!("{WEB}/opinion/{id}/{slug}/"), None => format!("{WEB}/opinion/{id}/") }) }
    }).collect())
}
