//! A2AJ's documents, read by the engine: Beaver's local A2AJ store (a2aj.sqlite, which a2ajInstall.ts installs), found
//! by citation key, searched and inventoried, and the records A2AJ's API answers with. Each document is read by
//! legal_structure_features::a2aj and derived here into the host's document (`make`); JavaScript is given its metadata
//! and documents, never its text.

use self::lookup::{self as a2aj_lookup, datasets};
use crate::sqlite::{self, sql, Stamp};
pub use self::lookup::{name_key, Named};
use crate::AliasGroup;
type CoreResult<T> = Result<T, String>;
use crate::a2aj::{
    self, Answering, Columns, Document, Excerpt, Language, LazyRow, Listing, Metadata, Query, Read, Source, StoreRow, CITATIONS,
};
use legal_structure::{trim_javascript_whitespace as js_trim, DocumentStructure, ProviderTextInput};
use regex::Regex;
use rusqlite::{types::ValueRef, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::any::Any;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

pub mod lookup;
pub mod remote;

/// What the host makes a reading into: its own document, which the store keeps while the host holds it, and which
/// derives the reading's structure where it is first read.
pub trait Make<D>: Fn(Reading) -> D {}
impl<D, F: Fn(Reading) -> D> Make<D> for F {}

/// A document as the store reads it, its structure not yet derived: what it is derived from, and its revision, the
/// digest of what it is read from (`role` tells a statute's provisions from its full text) and of the reader, so a
/// document is known without deriving it. The structure derived takes that revision.
pub struct Reading {
    pub revision: String,
    pub input: ProviderTextInput,
    /// The names the document's statute citations give, as the citator read them when it read the decision
    /// (`citator_graph::cited_names`); none where nothing has read its citations.
    pub cited_names: Option<Vec<String>>,
    /// The document's CanLII page, as its metadata read it (`a2aj::Metadata::canlii_url`).
    pub canlii: Option<String>,
}

/// The reader whose readings the revision names: changed whenever what a reading derives changes.
const READER: &str = "a2aj-provider-text-reading-v1";

fn reading(identity: &str, role: &str, input: ProviderTextInput, cited_names: Option<Vec<String>>, canlii: Option<String>) -> Reading {
    use sha2::{Digest, Sha256};
    let digest = Sha256::new().chain_update(READER).chain_update([0]).chain_update(role).chain_update([0])
        .chain_update(identity).finalize();
    Reading { revision: digest.iter().map(|byte| format!("{byte:02x}")).collect(), input, cited_names, canlii }
}

/// A reading derived: its structure, with the reading's revision.
pub fn derived_structure(reading: Reading) -> CoreResult<DocumentStructure> {
    let mut structure = legal_structure::provider_text_document_structure(reading.input).map_err(|error| error.to_string())?;
    structure.revision = reading.revision;
    Ok(structure)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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

fn readings<D>(read: Read, cited_names: Option<Vec<String>>, make: &impl Make<D>) -> CoreResult<Found<D>> {
    let metadata = read.metadata.clone();
    let pages = read.printed_pages.iter().map(|page| (&page.label, page.start, page.end)).collect::<Vec<_>>();
    let identity = serde_json::to_string(&(&read.metadata, &read.text, &read.provisions, pages)).map_err(|error| error.to_string())?;
    let canlii = metadata.canlii_url.clone();
    let (native, search) = read.inputs();
    Ok(Found { search: search.map(|search| Arc::new(make(reading(&identity, "search", search, cited_names.clone(), canlii.clone())))),
        native: Arc::new(make(reading(&identity, "native", native, cited_names, canlii))), metadata })
}

#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[cfg_attr(feature = "binding-types", ts(optional_fields = nullable))]
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
    Ok(sqlite::read(path, |connection, _| {
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
    #[cfg(feature = "citator-graph")]
    let cited = crate::citator_graph::cited_names(path, id, read.metadata.language);
    #[cfg(not(feature = "citator-graph"))]
    let cited = None;
    let found = readings(read, cited, make)?;
    keep(key, &found);
    Ok(Some(found))
}

/// The id of the one document the store holds under a citation.
pub fn find_id(path: &Path, lookup: &Lookup) -> CoreResult<Option<i64>> {
    Ok(sqlite::read(path, |connection, _| find(connection, lookup))?.flatten())
}

/// A document the store holds under a citation.
pub fn document<D: Send + Sync + 'static>(path: &Path, lookup: &Lookup, make: impl Make<D>) -> CoreResult<Option<Found<D>>> {
    let language = lookup.language.unwrap_or(Language::En);
    Ok(sqlite::read(path, |connection, stamp| {
        let Some(id) = find(connection, lookup)? else { return Ok(None) };
        by_id(connection, path, stamp, id, language, &make)
    })?
    .flatten())
}

/// The store's documents with these ids, of a kind: what experiments sample the store by.
pub fn documents<D: Send + Sync + 'static>(path: &Path, ids: &[i64], doc_type: &str, language: Language, make: impl Make<D>)
    -> CoreResult<Option<Vec<(i64, Found<D>)>>> {
    sqlite::read(path, |connection, stamp| {
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
    Ok(sqlite::read(path, |connection, stamp| {
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
        let identity = serde_json::to_string(&(&key.2, &text)).map_err(|error| error.to_string())?;
        let native = Arc::new(make(reading(&identity, "provision", a2aj::provision_input(excerpt, metadata.language, section, text), None, metadata.canlii_url.clone())));
        keep(key, &Found { metadata, native: native.clone(), search: None });
        Ok(Some(Some(native)))
    })?
    .flatten())
}

/// What a search asks of the store (and of A2AJ, where the store has no full-text indexes).
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[cfg_attr(feature = "binding-types", ts(optional_fields = nullable))]
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
    let compiled = crate::search_query::compile(&search.query, None);
    let names = if compiled.operators {
        compiled.fts5.clone()
    } else {
        a2aj::name_words(&search.query).iter().map(|word| format!("\"{word}\"")).collect::<Vec<_>>().join(" ")
    };
    Ok(sqlite::read(path, |connection, _| {
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
            Ok(named.into_iter().chain(matching("document_search", &compiled.fts5)?).filter(|(id, _)| seen.insert(*id))
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
    sqlite::read(path, |connection, _| {
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
    sqlite::read(path, |connection, _| {
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
    let key = (PathBuf::new(), (0, None), Sha256::digest(identity.as_bytes()).iter().fold(String::from("record\0"), |key, byte| key + &format!("{byte:02x}")));
    if let Some(found) = derived(&key) {
        return Ok(Some(found));
    }
    let found = readings(read, None, &make)?;
    keep(key, &found);
    Ok(Some(found))
}

/// The records answering a lookup of a statute's provision that one is read from: of the collection asked for, at the
/// address asked for, else the first.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[cfg_attr(feature = "binding-types", ts(optional_fields = nullable))]
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
        Some(read) if !section.is_empty() => {
            let identity = serde_json::to_string(&(&read.metadata, section, &excerpt.citation, &excerpt.url,
                &excerpt.alternate_citation, &excerpt.dataset, &excerpt.name, &read.text)).map_err(|error| error.to_string())?;
            Ok(Some(Arc::new(make(reading(&identity, "provision", a2aj::provision_input(excerpt, language, section, read.text), None, read.metadata.canlii_url.clone())))))
        }
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
    let found = sqlite::read(path, |connection, _| {
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
            let text = |value: ValueRef| a2aj::cell(value).unwrap_or_default();
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

/// A citation read once for its lookup: the forms the store and A2AJ are asked for, in order and at most twelve (the
/// citation as written, a Recueil citation as the S.C.R. it is, its case citations' cores, then the forms the store's
/// inventory writes for the decision), the keys the decision is known by, and whether a key names several decisions.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct CitationLookup {
    pub forms: Vec<LookupForm>,
    pub aliases: Vec<String>,
    pub ambiguous: bool,
}

/// A form a citation is asked for by, and what A2AJ's coverage must hold for A2AJ to be asked for it: the court's
/// dataset ("SCC") of a neutral or S.C.R. citation, the jurisdiction ("FED", "ON") of a statute's. None where A2AJ is
/// asked by another form: a CanLII number, another reporter, a bill, a case's name written before its citation.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LookupForm {
    pub text: String,
    pub covered_by: Option<String>,
}

pub fn citation_lookup(path: &Path, citation: &str, laws: bool) -> CoreResult<CitationLookup> {
    static RECUEIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u:\b)R\.?\s?C\.?\s?S\.?(\s+\d)").unwrap());
    // The citation is read once, for its cores and for how A2AJ is asked by it; every other form is a text of its own.
    let read = legal_structure::read_citations(citation);
    let (english, core) = if laws { (citation.to_owned(), Vec::new()) } else {
        (RECUEIL.replace_all(citation, "S.C.R.$1").into_owned(),
            legal_structure::citation_occurrences(citation, &read, 0).into_iter().filter(|found| found.kind == "case")
                .map(|found| found.spans.core_span.text).collect::<Vec<_>>())
    };
    let mut own = Some(legal_structure::provider_citations(citation, &read));
    // A2AJ is asked by citation alone, never with a case's name written before it: that only asks again for the same
    // decision.
    let named = [citation.to_owned(), english.clone()].into_iter()
        .filter(|form| !core.is_empty() && !core.contains(form)).collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let requested = [citation.to_owned(), english].into_iter().chain(core).filter(|form| seen.insert(form.clone()))
        .collect::<Vec<_>>();
    let inventory = if laws { Vec::new() } else {
        alias_groups(path, &requested.iter().map(|form| legal_structure::citation_lookup_key(form)).collect::<Vec<_>>())?
    };
    let mut kept = HashSet::new();
    let aliases = inventory.iter().flat_map(|group| group.keys.iter().cloned()).filter(|key| kept.insert(key.clone())).collect();
    let written = inventory.iter().flat_map(|group| group.forms.iter().cloned()).filter(|form| seen.insert(form.clone()));
    let forms = requested.into_iter().chain(written).take(12).map(|text| {
        let read = if text == citation { own.take().unwrap_or_default() } else { legal_structure::provider_citations_in_text(&text) };
        let covered_by = (if named.contains(&text) { None } else if laws {
            read.into_iter().find(|cite| cite.family == "statute").and_then(|cite| cite.jurisdiction).and_then(|place| match place.as_str() {
                "ca" => Some("FED".to_owned()),
                _ => place.strip_prefix("ca-").map(|code| match code.to_uppercase() { code if code == "YK" => "YT".to_owned(), code => code }),
            })
        } else {
            read.into_iter().next().and_then(|cite| {
                let court = cite.court?.chars().filter(|c| !c.is_whitespace() && *c != '.').collect::<String>().to_uppercase();
                match cite.family { "neutral" => Some(court), "reporter" if court == "SCR" => Some("SCC".to_owned()), _ => None }
            })
        }).filter(|code| !code.is_empty());
        LookupForm { text, covered_by }
    }).collect();
    Ok(CitationLookup { forms, aliases, ambiguous: inventory.iter().any(|group| group.ambiguous) })
}
