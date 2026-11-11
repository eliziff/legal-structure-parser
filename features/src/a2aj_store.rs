//! A2AJ's documents, read by the engine: Beaver's local A2AJ store (a2aj.sqlite, which a2ajInstall.ts installs), found
//! by citation key, searched and inventoried, and the records A2AJ's API answers with. Each document is read by
//! legal_structure_features::a2aj and derived here into the host's document (`make`); JavaScript is given its metadata
//! and documents, never its text.

use self::lookup::{self as a2aj_lookup, datasets, sql};
pub use self::lookup::{name_key, Named};
use crate::AliasGroup;
type CoreResult<T> = Result<T, String>;
use crate::a2aj::{
    self, Answering, Columns, Document, Excerpt, Language, LazyRow, Listing, Metadata, Query, Read, Source, StoreRow, CITATIONS,
};
use legal_structure::{trim_javascript_whitespace as js_trim, DocumentStructure, ProviderTextInput};
use regex::Regex;
use rusqlite::{types::ValueRef, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::any::Any;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::time::SystemTime;

pub mod lookup;

/// What the host makes a derived structure into: its own document, which the store keeps while the host holds it.
pub trait Make<D>: Fn(DocumentStructure) -> D {}
impl<D, F: Fn(DocumentStructure) -> D> Make<D> for F {}

/// Which copy of a store a connection or a document came from: a store replaced in its place is another copy.
pub type Stamp = (u64, Option<SystemTime>);

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()))
}

/// Read-only connections, kept while their copy of the store is: the installer writes the store in place, through its
/// write-ahead log, which readers may hold open. Opening and closing one costs more than most reads.
static CONNECTIONS: Mutex<Vec<(PathBuf, Stamp, Connection)>> = Mutex::new(Vec::new());

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runs `work` on a read-only connection to a store's file (the store, or a file beside it); None where there is no file.
pub fn with_store<T>(path: &Path, work: impl FnOnce(&Connection, Stamp) -> CoreResult<T>) -> CoreResult<Option<T>> {
    let Some(stamp) = stamp(path) else { return Ok(None) };
    let kept = {
        let mut connections = lock(&CONNECTIONS);
        connections.retain(|(other, held, _)| other != path || *held == stamp);
        connections.iter().position(|(other, _, _)| other == path).map(|index| connections.swap_remove(index).2)
    };
    let connection = match kept {
        Some(connection) => connection,
        None => {
            let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            let connection = Connection::open_with_flags(path, flags).map_err(sql)?;
            // A decision's text runs over many pages: they are read from a mapping of the file, not a call each.
            connection.execute_batch("PRAGMA mmap_size=2147418112").map_err(sql)?;
            connection
        }
    };
    let result = work(&connection, stamp);
    lock(&CONNECTIONS).push((path.to_owned(), stamp, connection));
    result.map(Some)
}

/// The documents derived from a store, kept while JavaScript holds them: an operation's stages ask for the same
/// decision in each of its forms, and each asks the store again.
type Kept = Weak<dyn Any + Send + Sync>;
struct Derived {
    metadata: Metadata,
    native: Kept,
    search: Option<Kept>,
}
static DERIVED: Mutex<Vec<((PathBuf, Stamp, String), Derived)>> = Mutex::new(Vec::new());

fn held<D: Send + Sync + 'static>(kept: &Kept) -> Option<Arc<D>> {
    kept.upgrade()?.downcast().ok()
}

fn derived<D: Send + Sync + 'static>(key: &(PathBuf, Stamp, String)) -> Option<Found<D>> {
    let cache = lock(&DERIVED);
    let (_, kept) = cache.iter().find(|(other, _)| other == key)?;
    let search = match &kept.search {
        Some(search) => Some(held(search)?),
        None => None,
    };
    Some(Found { metadata: kept.metadata.clone(), native: held(&kept.native)?, search })
}

fn keep<D: Send + Sync + 'static>(key: (PathBuf, Stamp, String), found: &Found<D>) {
    let kept = |document: &Arc<D>| -> Kept { Arc::downgrade(&(document.clone() as Arc<dyn Any + Send + Sync>)) };
    let mut cache = lock(&DERIVED);
    cache.retain(|(other, kept)| *other != key && kept.native.strong_count() > 0);
    let derived = Derived { metadata: found.metadata.clone(), native: kept(&found.native), search: found.search.as_ref().map(kept) };
    cache.push((key, derived));
}

/// A document's small fields, read with its row: every one but its texts and provisions.
const FIELDS: &[&str] = a2aj::COLUMNS.as_slice().split_at(13).0;

/// A document: its metadata, its provisions' document (or its text's, where it has no provisions), and its full text's
/// document where that is another.
pub struct Found<D> {
    pub metadata: Metadata,
    pub native: Arc<D>,
    pub search: Option<Arc<D>>,
}

fn derive<D>(input: ProviderTextInput, make: &impl Make<D>) -> CoreResult<Arc<D>> {
    legal_structure::provider_text_document_structure(input).map(|structure| Arc::new(make(structure))).map_err(|error| error.to_string())
}

fn readings<D>(read: Read, make: &impl Make<D>) -> CoreResult<Found<D>> {
    let metadata = read.metadata.clone();
    let (native, search) = read.inputs();
    Ok(Found { native: derive(native, make)?, search: search.map(|search| derive(search, make)).transpose()?, metadata })
}

#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Lookup {
    pub citation: String,
    pub doc_type: Option<String>,
    pub language: Option<Language>,
    pub dataset: Option<String>,
    pub source_url: Option<String>,
}

pub fn find(connection: &Connection, lookup: &Lookup) -> CoreResult<Option<i64>> {
    a2aj_lookup::find(connection, &lookup.citation, lookup.doc_type.as_deref(), lookup.dataset.as_deref(), lookup.source_url.as_deref())
}

/// The citation of the one decision the store holds under a style of cause, dated in the year asked for, of one of
/// the courts asked for (of any where none is): how a decision cited by an identifier A2AJ does not carry ("1991
/// CanLII 45 (SCC)") is found, as the Python app's identity_hints does. None where no decision answers, or several.
pub fn named_case(path: &Path, named: &Named) -> CoreResult<Option<String>> {
    Ok(with_store(path, |connection, _| {
        let mut citations = BTreeSet::new();
        for (_, row) in a2aj_lookup::named_decisions(connection, named)? {
            citations.extend(Document::new(&row, Source::Store).language_field("citation", Language::En).map_err(sql)?);
        }
        Ok(one(citations))
    })?
    .flatten())
}

/// The same of the records A2AJ's API answered a search of the name with.
pub fn named_record(records: &[Value], named: &Named, language: Language) -> Option<String> {
    let mut citations = BTreeSet::new();
    for record in records.iter().filter_map(Value::as_object) {
        let document = Document::new(record, Source::Api);
        let Ok(name) = document.language_field("name", language);
        let Ok(answers) = named.answers(&document, Some(name.as_deref().unwrap_or_default()));
        if answers {
            let Ok(citation) = document.citation(language);
            citations.extend(citation);
        }
    }
    one(citations)
}

fn one(citations: BTreeSet<String>) -> Option<String> {
    (citations.len() == 1).then(|| citations.into_iter().next()).flatten()
}

/// The store's document with an id, derived once while JavaScript holds it.
fn by_id<D: Send + Sync + 'static>(connection: &Connection, path: &Path, stamp: Stamp, id: i64, language: Language,
    make: &impl Make<D>) -> CoreResult<Option<Found<D>>> {
    let key = (path.to_owned(), stamp, format!("{id}\0{}", language.code()));
    if let Some(found) = derived(&key) {
        return Ok(Some(found));
    }
    let Some(row) = LazyRow::read(connection, id, FIELDS).map_err(sql)? else { return Ok(None) };
    let document = Document::new(&row, Source::Store);
    let Some(mut read) = document.read(language, document.doc_type().map_err(sql)?).map_err(sql)? else { return Ok(None) };
    read.printed_pages = a2aj::reporter_pages(path, id, read.metadata.language);
    let found = readings(read, make)?;
    keep(key, &found);
    Ok(Some(found))
}

/// The id of the one document the store holds under a citation.
pub fn find_id(path: &Path, lookup: &Lookup) -> CoreResult<Option<i64>> {
    Ok(with_store(path, |connection, _| find(connection, lookup))?.flatten())
}

/// A document the store holds under a citation.
pub fn document<D: Send + Sync + 'static>(path: &Path, lookup: &Lookup, make: impl Make<D>) -> CoreResult<Option<Found<D>>> {
    let language = lookup.language.unwrap_or(Language::En);
    Ok(with_store(path, |connection, stamp| {
        let Some(id) = find(connection, lookup)? else { return Ok(None) };
        by_id(connection, path, stamp, id, language, &make)
    })?
    .flatten())
}

/// The store's documents with these ids, of a kind: what experiments sample the store by.
pub fn documents<D: Send + Sync + 'static>(path: &Path, ids: &[i64], doc_type: &str, language: Language, make: impl Make<D>)
    -> CoreResult<Option<Vec<(i64, Found<D>)>>> {
    with_store(path, |connection, stamp| {
        let mut found = Vec::new();
        for &id in ids {
            let document = by_id(connection, path, stamp, id, language, &make)?;
            found.extend(document.filter(|found| found.metadata.doc_type == doc_type).map(|document| (id, document)));
        }
        Ok(found)
    })
}

/// One provision of a statute the store holds: None where the store lacks the statute, Some(None) where it lacks the
/// provision.
pub fn section<D: Send + Sync + 'static>(path: &Path, lookup: &Lookup, section: &str, excerpt: Excerpt, make: impl Make<D>)
    -> CoreResult<Option<Option<Arc<D>>>> {
    let language = lookup.language.unwrap_or(Language::En);
    Ok(with_store(path, |connection, stamp| {
        let Some(id) = find(connection, lookup)? else { return Ok(None) };
        let Some(row) = LazyRow::read(connection, id, FIELDS).map_err(sql)? else { return Ok(None) };
        // The store holds the statute where it holds its text, in the language asked for or the other, and citation.
        let holds = |language: Language| row.holds(&format!("unofficial_text_{}", language.code())).map_err(sql);
        let actual = if holds(language)? {
            language
        } else if holds(language.other())? {
            language.other()
        } else {
            return Ok(None);
        };
        let document = Document::new(&row, Source::Store);
        let Some(metadata) = document.metadata(actual, document.doc_type().map_err(sql)?).map_err(sql)? else { return Ok(None) };
        let provisions = document.provisions(actual).map_err(sql)?.unwrap_or_default();
        let text = provisions.into_iter().find(|(key, _)| key == section).map(|(_, text)| text).unwrap_or_default();
        if js_trim(section).is_empty() || js_trim(&text).is_empty() {
            return Ok(Some(None));
        }
        let identity = (id, language.code(), section, &excerpt.citation, &excerpt.url, &excerpt.alternate_citation,
            &excerpt.dataset, &excerpt.name);
        let key = (path.to_owned(), stamp, serde_json::to_string(&identity).map_err(|error| error.to_string())?);
        if let Some(found) = derived::<D>(&key) {
            return Ok(Some(Some(found.native)));
        }
        let native = derive(a2aj::provision_input(excerpt, metadata.language, section, text), &make)?;
        keep(key, &Found { metadata, native: native.clone(), search: None });
        Ok(Some(Some(native)))
    })?
    .flatten())
}

/// What a search asks of the store. The full-text index's query comes built (searchQuery.ts builds every search's).
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Search {
    query: String,
    doc_type: Option<String>,
    search_type: Option<String>,
    language: Option<Language>,
    /// How many are wanted: ten unless asked, at most fifty.
    size: Option<f64>,
    dataset: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    sort_results: Option<String>,
    /// The query in the full-text index's syntax.
    #[serde(rename = "match")]
    expression: String,
    /// Whether the query uses the index's operators.
    operators: bool,
}

/// The best matches the index ranks on its own before citations count, and how much each order of magnitude of
/// citations counts against the index's relevance.
const RANKED_POOL: i64 = 200;
const CITED_WEIGHT: f64 = 0.1;

/// What a search reads of a document: never its text.
const SEARCH_COLUMNS: &[&str] = FIELDS.split_at(12).0;

/// The store's documents answering a search, as a search lists them; None where there is no store or it has no
/// full-text indexes. A name search reads the names and citations' own index, every word of a plain query in any
/// order; a search of the text reads every column's, and, ranked by relevance, lists the sources the query names before
/// those whose text holds it, as a search engine ranks a title match first: "Hryniak v Mauldin" or "2014 SCC 7" finds
/// that case, not its citers. Relevance counts how often a decision is cited: among the many decisions named "R v Jordan", the
/// Supreme Court's comes first. The index ranks its best matches on its own (fast); they are then ordered with the
/// citations counted.
pub fn search(path: &Path, search: &Search) -> CoreResult<Option<Vec<Listing>>> {
    use rusqlite::types::Value as Sql;
    static WORDED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}]").expect("worded"));
    if !WORDED.is_match(&search.query) {
        return Ok(Some(Vec::new()));
    }
    let wanted = search.size.map_or(10.0, f64::trunc).clamp(1.0, 50.0) as i64;
    let language = search.language.unwrap_or(Language::En);
    let name_search = search.search_type.as_deref() == Some("name");
    let names = if search.operators {
        search.expression.clone()
    } else {
        a2aj::name_words(&search.query).iter().map(|word| format!("\"{word}\"")).collect::<Vec<_>>().join(" ")
    };
    Ok(with_store(path, |connection, _| {
        let mut indexes = connection.prepare_cached(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('document_search', 'name_search')").map_err(sql)?;
        if indexes.query_row([], |row| row.get::<_, i64>(0)).map_err(sql)? < 2 {
            return Ok(None);
        }
        let mut filters = vec!["document.doc_type = ?".to_owned()];
        let mut values: Vec<Sql> = vec![search.doc_type.clone().unwrap_or_else(|| "cases".to_owned()).into()];
        let datasets = datasets(search.dataset.as_deref());
        if !datasets.is_empty() {
            filters.push(format!("LOWER(document.dataset) IN ({})", vec!["LOWER(?)"; datasets.len()].join(", ")));
            values.extend(datasets.into_iter().map(Sql::from));
        }
        let date = format!("substr(COALESCE(NULLIF(document.document_date_{}, ''), document.document_date_{}, ''), 1, 10)",
            language.code(), language.other().code());
        for (bound, value) in [(">=", &search.start_date), ("<=", &search.end_date)] {
            if let Some(value) = value.as_deref().map(js_trim).filter(|value| !value.is_empty()) {
                filters.push(format!("{date} {bound} ?"));
                values.push(value.to_owned().into());
            }
        }
        let ranked = search.sort_results.as_deref().is_none_or(|sort| sort == "default");
        // How often each match is cited: the higher of A2AJ's count and the citator's, where its graph is built beside the
        // store (through the store's keys for the document). Each misses citers the other finds: A2AJ has no count for most
        // of the Supreme Court's older decisions.
        let counted = {
            #[cfg(feature = "citator-graph")] { let graph = crate::citator_graph::graph_path(path); ranked && graph.exists() && connection.execute("ATTACH DATABASE ? AS graph", [graph.to_string_lossy()]).is_ok() }
            #[cfg(not(feature = "citator-graph"))] { false }
        };
        let cited = if counted {
            "MAX(COALESCE(document.citing_cases_count, 0), COALESCE((SELECT MAX(metric.citing_cases) FROM citation_lookup AS lookup
                JOIN graph.authority_metric AS metric ON metric.cited_key = lookup.citation_key WHERE lookup.document_id = document.id), 0))"
        } else {
            "document.citing_cases_count"
        };
        let order = |index: &str| match search.sort_results.as_deref() {
            Some("newest_first") => format!("{date} DESC, document.id"),
            Some("oldest_first") => format!("{date} ASC, document.id"),
            _ => format!("{index}.rank"),
        };
        let matching = |index: &str, expression: &str| -> CoreResult<Vec<(i64, StoreRow)>> {
            let query = format!("SELECT document.id, {index}.rank, {cited}, {} FROM {index} JOIN document ON document.id = {index}.rowid
                WHERE {index} MATCH ? AND {} ORDER BY {} LIMIT ?",
                SEARCH_COLUMNS.iter().map(|column| format!("document.{column}")).collect::<Vec<_>>().join(", "),
                filters.join(" AND "), order(index));
            let pool = if ranked { RANKED_POOL.max(wanted) } else { wanted };
            let bound: Vec<Sql> = std::iter::once(Sql::from(expression.to_owned())).chain(values.iter().cloned())
                .chain(std::iter::once(Sql::from(pool))).collect();
            let mut statement = connection.prepare_cached(&query).map_err(sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(bound.iter()), |row| Ok((row.get::<_, i64>(0)?,
                row.get::<_, f64>(1)?, row.get::<_, Option<i64>>(2)?, StoreRow::from_query(row, 3, SEARCH_COLUMNS)?))).map_err(sql)?;
            let mut rows = rows.collect::<Result<Vec<_>, _>>().map_err(sql)?;
            if ranked {
                // The index's rank is negative, more negative the better: each citation's log scales it.
                let score = |rank: f64, cited: Option<i64>| rank * (1.0 + CITED_WEIGHT * (1.0 + cited.unwrap_or(0).max(0) as f64).ln());
                rows.sort_by(|a, b| score(a.1, a.2).total_cmp(&score(b.1, b.2)));
            }
            Ok(rows.into_iter().take(wanted as usize).map(|(id, _, _, row)| (id, row)).collect())
        };
        let found = (|| -> CoreResult<Vec<(i64, StoreRow)>> {
            let named = if name_search || ranked { matching("name_search", &names)? } else { Vec::new() };
            if name_search {
                return Ok(named);
            }
            let mut seen = HashSet::new();
            Ok(named.into_iter().chain(matching("document_search", &search.expression)?).filter(|(id, _)| seen.insert(*id))
                .take(wanted as usize).collect())
        })();
        // Held only for the search: the citator replaces its graph's file when it builds it again.
        if counted {
            connection.execute_batch("DETACH DATABASE graph").map_err(sql)?;
        }
        let mut listed = Vec::new();
        for (_, row) in &found? {
            listed.extend(Document::new(row, Source::Store).listing(language).map_err(sql)?);
        }
        Ok(Some(listed))
    })?.flatten())
}

#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Court {
    doc_type: &'static str,
    court: String,
    documents: i64,
    /// The Parquet file the court was installed from, where the installer recorded it.
    sha256: Option<String>,
    size: Option<i64>,
    revision: Option<String>,
}

/// Every court and jurisdiction in the store, with how many documents each has.
pub fn courts(path: &Path) -> CoreResult<Option<Vec<Court>>> {
    with_store(path, |connection, _| {
        let recorded = connection.query_row("SELECT 1 FROM sqlite_master WHERE name = 'source_file'", [], |_| Ok(()))
            .optional().map_err(sql)?.is_some();
        let mut files = HashMap::new();
        if recorded {
            let mut statement = connection.prepare_cached("SELECT doc_type, dataset, sha256, size, revision FROM source_file").map_err(sql)?;
            let rows = statement.query_map([], |row| Ok(((row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                (row.get::<_, String>(2)?, row.get::<_, i64>(3)?, row.get::<_, String>(4)?)))).map_err(sql)?;
            files = rows.collect::<Result<HashMap<_, _>, _>>().map_err(sql)?;
        }
        let mut statement = connection.prepare_cached(
            "SELECT doc_type, dataset, COUNT(*) AS documents FROM document GROUP BY doc_type, dataset").map_err(sql)?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)))
            .map_err(sql)?;
        rows.map(|row| {
            let (doc_type, court, documents) = row.map_err(sql)?;
            let file = files.get(&(doc_type.clone(), court.clone()));
            Ok(Court { doc_type: a2aj::doc_type(&doc_type), court, documents, sha256: file.map(|file| file.0.clone()),
                size: file.map(|file| file.1), revision: file.map(|file| file.2.clone()) })
        })
        .collect()
    })
}

/// A document of the store as the engine reads it, by its id.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct StoreRead {
    id: i64,
    #[serde(flatten)]
    metadata: Metadata,
    text: String,
    provisions: Option<Vec<(String, String)>>,
}

/// The documents with these ids, of a kind, each in the language asked for where it has text in it, else in the
/// other: what experiments read the store's documents by.
pub fn reads(path: &Path, ids: &[i64], doc_type: &str, language: Language) -> CoreResult<Option<Vec<StoreRead>>> {
    // Its small fields, texts and provisions.
    const COLUMNS: &[&str] = a2aj::COLUMNS.as_slice().split_at(17).0;
    with_store(path, |connection, _| {
        let mut statement = connection.prepare_cached(&format!("SELECT {} FROM document WHERE id = ? AND doc_type = ?", COLUMNS.join(", ")))
            .map_err(sql)?;
        let mut found = Vec::new();
        for &id in ids {
            let row = statement.query_row(rusqlite::params![id, doc_type], |row| StoreRow::from_query(row, 0, COLUMNS));
            let Some(row) = row.optional().map_err(sql)? else { continue };
            let document = Document::new(&row, Source::Store);
            if let Some(read) = document.read(language, document.doc_type().map_err(sql)?).map_err(sql)? {
                found.push(StoreRead { id, metadata: read.metadata, text: read.text, provisions: read.provisions });
            }
        }
        Ok(found)
    })
}

/// The document an A2AJ API record holds: its metadata and readings, derived once while JavaScript holds them.
pub fn record<D: Send + Sync + 'static>(record: &Value, language: Language, doc_type: &str, make: impl Make<D>)
    -> CoreResult<Option<Found<D>>> {
    use sha2::{Digest, Sha256};
    let Some(record) = record.as_object() else { return Ok(None) };
    let Ok(read) = Document::new(record, Source::Api).read(language, a2aj::doc_type(doc_type));
    let Some(read) = read else { return Ok(None) };
    let identity = serde_json::to_string(&(&read.metadata, &read.text, &read.provisions)).map_err(|error| error.to_string())?;
    let key = (PathBuf::new(), (0, None), format!("record\0{:x}", Sha256::digest(identity.as_bytes())));
    if let Some(found) = derived(&key) {
        return Ok(Some(found));
    }
    let found = readings(read, &make)?;
    keep(key, &found);
    Ok(Some(found))
}

/// The records answering a lookup of a statute's provision that one is read from: of the collection asked for, at the
/// address asked for, else the first.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ProvisionLookup {
    language: Option<Language>,
    doc_type: Option<String>,
    dataset: Option<String>,
    source_url: Option<String>,
    section: String,
}

/// One provision of a statute, read from the A2AJ API records answering a lookup of it as an excerpt of the statute;
/// None where no record holds text for it.
pub fn record_provision<D>(records: &[Value], lookup: &ProvisionLookup, excerpt: Excerpt, make: impl Make<D>)
    -> CoreResult<Option<Arc<D>>> {
    let language = lookup.language.unwrap_or(Language::En);
    let doc_type = a2aj::doc_type(lookup.doc_type.as_deref().unwrap_or("cases"));
    let present = |value: &Option<String>| value.as_deref().map(js_trim).filter(|value| !value.is_empty()).map(str::to_owned);
    let dataset = present(&lookup.dataset).map(|dataset| dataset.to_lowercase());
    let mut candidates = records.iter().filter_map(Value::as_object)
        .filter_map(|record| {
            let Ok(read) = Document::new(record, Source::Api).read(language, doc_type);
            read
        })
        .filter(|read| dataset.as_ref().is_none_or(|dataset| read.metadata.dataset.to_lowercase() == *dataset));
    let chosen = match present(&lookup.source_url) {
        Some(address) => candidates.find(|read| [&read.metadata.url, &read.metadata.publisher_url].into_iter()
            .any(|url| url.as_deref() == Some(address.as_str()))),
        None => candidates.next(),
    };
    let section = js_trim(&lookup.section);
    match chosen {
        Some(read) if !section.is_empty() => Ok(Some(derive(a2aj::provision_input(excerpt, language, section, read.text), &make)?)),
        _ => Ok(None),
    }
}

/// An A2AJ API record as a search lists it; None for one without a citation.
pub fn record_listing(record: &Value, language: Language) -> Option<Listing> {
    let Ok(listing) = Document::new(record.as_object()?, Source::Api).listing(language);
    listing
}

/// The places among A2AJ API records of those that answer a query.
pub fn record_matches(records: &[Value], query: &Query) -> Vec<usize> {
    let Some(answering) = Answering::new(query) else { return Vec::new() };
    let answers = |record: &Value| record.as_object().is_some_and(|record| {
        let Ok(answers) = Document::new(record, Source::Api).answers(&answering);
        answers
    });
    records.iter().enumerate().filter(|(_, record)| answers(record)).map(|(at, _)| at).collect()
}

/// A citation's keys for a note-up: every key of the one decision the store holds under it, else its own key.
pub fn alias_keys(path: &Path, citation: &str) -> CoreResult<Vec<String>> {
    let key = legal_structure::caselaw_citation_lookup_key(citation).map_err(str::to_owned)?;
    Ok(match alias_groups(path, &[key.clone()])?.into_iter().next() {
        Some(group) if !group.ambiguous && !group.keys.is_empty() => group.keys,
        _ => vec![key],
    })
}

/// Each parsed key's exact aliases and authored forms (as the store writes them) in the store's inventory; its own key
/// alone where no store is installed.
pub fn alias_groups(path: &Path, keys: &[String]) -> CoreResult<Vec<AliasGroup>> {
    let found = with_store(path, |connection, _| {
        let mut targets = connection.prepare_cached("SELECT DISTINCT document_id FROM citation_lookup WHERE citation_key = ?").map_err(sql)?;
        let mut aliases = connection.prepare_cached("SELECT citation_key FROM citation_lookup WHERE document_id = ?").map_err(sql)?;
        let mut forms = connection.prepare_cached(&format!("SELECT {} FROM document WHERE id = ?", CITATIONS.join(", "))).map_err(sql)?;
        keys.iter().map(|key| {
            let own = AliasGroup::own(key);
            let Some(key) = own.keys.first().cloned() else { return Ok(own) };
            let ids = targets.query_map([&key], |row| row.get::<_, i64>(0)).and_then(|rows| rows.collect::<Result<Vec<_>, _>>()).map_err(sql)?;
            if ids.len() != 1 {
                return Ok(AliasGroup { keys: vec![key], forms: Vec::new(), ambiguous: ids.len() > 1 });
            }
            let text = |value: ValueRef| match value {
                ValueRef::Text(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                _ => String::new(),
            };
            let mut keys = aliases.query_map([ids[0]], |row| row.get_ref(0).map(text))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>()).map_err(sql)?;
            keys.push(key);
            // In JavaScript's order: by UTF-16 code units.
            keys.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
            keys.dedup();
            let row = forms.query_row([ids[0]], |row| StoreRow::from_query(row, 0, &CITATIONS)).optional().map_err(sql)?;
            let forms = match row {
                Some(row) => a2aj_lookup::fields(&Document::new(&row, Source::Store).as_written(), &CITATIONS)?,
                None => Vec::new(),
            };
            Ok(AliasGroup { keys, forms, ambiguous: false })
        })
        .collect()
    })?;
    Ok(found.unwrap_or_else(|| keys.iter().map(|key| AliasGroup::own(key)).collect()))
}
