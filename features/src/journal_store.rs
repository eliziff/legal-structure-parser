//! The journals database (public_endpoint.db), read by the engine on its kept connections (`sqlite::read`):
//! its articles and page maps, searched through its full-text index (public_endpoint-search.sqlite) where that index was
//! built from this copy of it, and the final contracts' registry (journals.db) of articles kept as a package of pages.

use crate::sqlite::{self, sql, Stamp};
use legal_structure::citations::{format, url::source_url};
use legal_structure::{trim_javascript_whitespace as js_trim, JournalPageLabel};
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashMap;
use std::path::{absolute, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::UNIX_EPOCH;
use unicode_normalization::UnicodeNormalization;

type CoreResult<T> = Result<T, String>;

/// Where the journal files are, as the host's settings place them: the articles' database as configured
/// (MIKE_PUBLIC_ENDPOINT_DB), else the shared journals provider's; the search index; the final contracts' registry.
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct JournalStores {
    pub configured: Option<PathBuf>,
    pub shared: PathBuf,
    pub search: PathBuf,
    pub contracts: PathBuf,
}


/// A text cell, trimmed; None for an empty one or one of another type.
fn text(value: ValueRef) -> Option<String> {
    let ValueRef::Text(bytes) = value else { return None };
    Some(js_trim(&String::from_utf8_lossy(bytes)).to_owned()).filter(|value| !value.is_empty())
}

fn one<T>(connection: &Connection, query: &str, values: impl rusqlite::Params, read: impl FnOnce(&rusqlite::Row) -> rusqlite::Result<T>)
    -> CoreResult<Option<T>> {
    connection.query_row(query, values, read).optional().map_err(sql)
}

fn key_values(connection: &Connection, table: &str) -> CoreResult<HashMap<String, String>> {
    let mut statement = connection.prepare_cached(&format!("SELECT key, value FROM {table}")).map_err(sql)?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

/// The articles' database: as configured, else the shared one, else the one the search index was built from.
pub fn database_path(stores: &JournalStores) -> CoreResult<PathBuf> {
    if let Some(configured) = &stores.configured {
        return Ok(configured.clone());
    }
    if stores.shared.exists() {
        return Ok(stores.shared.clone());
    }
    let indexed = sqlite::read(&stores.search, |index, _| one(index, "SELECT value FROM meta WHERE key='source_path'", [], |row| row.get_ref(0).map(text)))?;
    match indexed.flatten().flatten().filter(|indexed| Path::new(indexed).exists()) {
        Some(indexed) => absolute(indexed).map_err(|error| error.to_string()),
        None => Ok(stores.shared.clone()),
    }
}

/// Runs `work` on the articles' database, and on the search index where it was built from this copy of the database.
fn with_index<T>(stores: &JournalStores, work: impl FnOnce(&Connection, Option<&Connection>) -> CoreResult<T>) -> CoreResult<T> {
    let path = database_path(stores)?;
    let found = sqlite::read(&path, |database, (size, modified)| {
        if one(database, "SELECT 1 FROM sqlite_master WHERE type='table' AND name='articles'", [], |_| Ok(()))?.is_none() {
            return Err("Unsupported public_endpoint.db schema".to_owned());
        }
        let built = key_values(database, "export_metadata");
        let source = absolute(&path).map_err(|error| error.to_string())?;
        let millis = modified.and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |time| time.as_millis());
        let mut work = Some(work);
        let indexed = sqlite::read(&stores.search, |index, _| {
            let (Ok(meta), Ok(built)) = (key_values(index, "meta"), &built) else { return Ok(None) };
            let field = |key: &str| meta.get(key).map(String::as_str).unwrap_or_default();
            let indexed = absolute(field("source_path")).unwrap_or_default();
            let same = if cfg!(windows) { indexed.to_string_lossy().to_lowercase() == source.to_string_lossy().to_lowercase() } else { indexed == source };
            let valid = meta.contains_key("source_schema_version") && meta.contains_key("source_created_at") && field("schema_version") == "3"
                && field("source_size") == size.to_string() && field("source_mtime_ms") == millis.to_string() && same
                && field("source_schema_version") == built.get("schema_version").map_or("", String::as_str)
                && field("source_created_at") == built.get("created_at").map_or("", String::as_str);
            if !valid { return Ok(None) }
            work.take().map(|work| work(database, Some(index))).transpose()
        })?.flatten();
        match (indexed, work) {
            (Some(done), _) => Ok(done),
            (None, Some(work)) => work(database, None),
            (None, None) => unreachable!("the work ran on the index"),
        }
    })?;
    found.ok_or_else(|| format!("Journal article database not found: {}. Set MIKE_PUBLIC_ENDPOINT_DB or place public_endpoint.db in the shared journals provider directory.", path.display()))
}

/// An article's row, its text cells by column.
struct Row {
    id: i64,
    cells: HashMap<String, Option<String>>,
}

impl Row {
    fn read(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        let columns = row.as_ref().column_names().into_iter().map(str::to_owned).collect::<Vec<_>>();
        let cells = columns.into_iter().enumerate().map(|(at, column)| Ok((column, text(row.get_ref(at)?)))).collect::<rusqlite::Result<_>>()?;
        Ok(Row { id: row.get("article_id")?, cells })
    }
    fn get(&self, column: &str) -> Option<String> {
        self.cells.get(column).cloned().flatten()
    }
    /// Its http(s) address without credentials, its fragment dropped.
    fn url(&self) -> Option<String> {
        let mut url = url::Url::parse(&self.get("galley_url").or_else(|| self.get("url_en"))?).ok()?;
        let trusted = ["http", "https"].contains(&url.scheme()) && url.username().is_empty() && url.password().is_none();
        trusted.then(|| { url.set_fragment(None); url.into() })
    }
    fn name(&self) -> String {
        self.get("name_en").unwrap_or_else(|| format!("Article {}", self.id))
    }
    /// McGill's form of the article: byline, title, year, volume and issue, journal, first page.
    fn citation(&self) -> String {
        format::article(&format::Article {
            authors: self.get("authors").unwrap_or_default().split(';').map(js_trim).filter(|author| !author.is_empty()).map(str::to_owned).collect(),
            title: self.get("name_en").unwrap_or_default(), year: self.get("document_date_en"), volume: self.get("volume"),
            issue: self.get("issue"), journal: self.get("journal_abbrev"), first_page: self.get("first_page"), document: None,
        })
    }
}

fn rows(connection: &Connection, query: &str, values: Vec<Sql>) -> CoreResult<Vec<Row>> {
    let mut statement = connection.prepare_cached(query).map_err(sql)?;
    let rows = statement.query_map(params_from_iter(values), Row::read).map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

fn ids(connection: &Connection, query: &str, values: Vec<Sql>) -> CoreResult<Vec<i64>> {
    let mut statement = connection.prepare_cached(query).map_err(sql)?;
    let ids = statement.query_map(params_from_iter(values), |row| row.get(0)).map_err(sql)?;
    ids.collect::<Result<_, _>>().map_err(sql)
}

/// `IN (?, …)` over ids, and the ids as values.
fn among(ids: &[i64]) -> (String, impl Iterator<Item = Sql> + '_) {
    (format!("IN ({})", vec!["?"; ids.len()].join(",")), ids.iter().map(|&id| Sql::from(id)))
}

/// Missing dates last.
fn by_date(left: Option<&str>, right: Option<&str>) -> std::cmp::Ordering {
    left.unwrap_or("\u{ffff}").cmp(right.unwrap_or("\u{ffff}"))
}

/// A search of the articles. `names` searches only what a citation names an article by (its name, authors, journal,
/// year, volume, issue and first page), in the index's small table of those, not the articles' text.
#[derive(Default, Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "JournalSearch", optional_fields = nullable))]
#[serde(rename_all = "camelCase")]
pub struct Search {
    pub query: String,
    pub size: Option<f64>,
    pub author: Option<String>,
    pub journal: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub sort: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<bool>", optional))]
    pub names: bool,
}

/// An article as a search lists it.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "JournalArticleSearchResult"))]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    #[cfg_attr(feature = "binding-types", ts(type = r#""journal""#))]
    pub provider: &'static str,
    pub hit_id: String,
    pub article_id: i64,
    pub dataset: String,
    pub citation: String,
    pub name: String,
    pub date: Option<String>,
    pub url: Option<String>,
    pub snippet: Option<String>,
    pub journal_name: Option<String>,
    pub authors: Option<String>,
}

impl Hit {
    /// `terms`: the snippet runs from 120 UTF-16 units before the first one the abstract holds (as the page slices it).
    fn of(row: &Row, terms: &[String]) -> Self {
        let snippet = row.get("abstract").map(|summary| {
            let folded = summary.to_lowercase();
            let at = terms.iter().find_map(|term| folded.find(&term.to_lowercase())).map_or(0, |at| folded[..at].encode_utf16().count());
            let units = summary.encode_utf16().collect::<Vec<_>>();
            String::from_utf16_lossy(&units[at.saturating_sub(120).min(units.len())..(at + 880).min(units.len())])
        });
        Hit { provider: "journal", hit_id: format!("journal:{}", row.id), article_id: row.id, dataset: row.get("dataset").unwrap_or_default(),
            citation: row.citation(), name: row.name(), date: row.get("document_date_en"), url: row.url(), snippet,
            journal_name: row.get("journal_name"), authors: row.get("authors") }
    }
}

/// An article's number, where the identifier is one ("123", "journal:123").
fn direct_id(identifier: &str) -> Option<i64> {
    let digits = identifier.get(..8).filter(|prefix| prefix.eq_ignore_ascii_case("journal:")).map_or(identifier, |_| &identifier[8..]);
    digits.bytes().all(|byte| byte.is_ascii_digit()).then(|| digits.parse().ok()).flatten()
}

const SEARCH_COLUMNS: &str = "article_id, dataset, citation_en, name_en, authors, document_date_en, volume, issue, first_page, \
    journal_name, journal_abbrev, galley_url, url_en, abstract";
const WITH_TEXT: &str = "text IS NOT NULL AND length(text) > 0";

/// The articles answering a search, at most 25: the one an article number names, else the index's best matches.
pub fn search(stores: &JournalStores, search: &Search) -> CoreResult<Vec<Hit>> {
    with_index(stores, |database, index| find(database, index, search))
}

fn find(database: &Connection, index: Option<&Connection>, search: &Search) -> CoreResult<Vec<Hit>> {
    let query = js_trim(&search.query);
    if query.is_empty() {
        return Err("query is required".to_owned());
    }
    let compiled = crate::search_query::compile(query, None);
    let terms = if compiled.valid { compiled.terms } else { Vec::new() };
    if let Some(id) = direct_id(query) {
        let found = rows(database, &format!("SELECT {SEARCH_COLUMNS} FROM articles WHERE article_id = ? AND {WITH_TEXT}"), vec![id.into()])?;
        return Ok(found.iter().map(|row| Hit::of(row, &terms)).collect());
    }
    if !query.chars().any(char::is_alphanumeric) {
        return Ok(Vec::new());
    }
    let wanted = search.size.map_or(10.0, f64::trunc).clamp(1.0, 25.0) as usize;
    let given = |value: &Option<String>| value.clone().filter(|value| !value.is_empty());
    let filters = [
        ("LOWER(authors) LIKE ?", given(&search.author).map(|author| format!("%{}%", author.to_lowercase()))),
        ("LOWER(COALESCE(journal_name, '') || ' ' || COALESCE(journal_abbrev, '')) LIKE ?", given(&search.journal).map(|journal| format!("%{}%", journal.to_lowercase()))),
        ("document_date_en >= ?", given(&search.date_from)),
        ("document_date_en <= ?", given(&search.date_to)),
    ].into_iter().filter_map(|(condition, value)| Some((condition, value?))).collect::<Vec<_>>();
    let index = index.ok_or("Journal search requires the journal FTS index")?;
    let table = if search.names { "article_metadata" } else { "article_search" };
    let rank = if search.names { "bm25(article_metadata)" } else { "bm25(article_search, 4.0, 1.0)" };
    let candidates = if filters.is_empty() { wanted } else { 250.min(wanted * 10) };
    let found = ids(index, &format!("SELECT rowid FROM {table} WHERE {table} MATCH ? ORDER BY {rank} LIMIT ?"),
        vec![compiled.fts5.into(), (candidates as i64).into()])?;
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let (within, values) = among(&found);
    let query = format!("SELECT {SEARCH_COLUMNS} FROM articles WHERE article_id {within} {}",
        filters.iter().map(|(condition, _)| format!(" AND {condition}")).collect::<String>());
    let by_id = rows(database, &query, values.chain(filters.into_iter().map(|(_, value)| value.into())).collect())?
        .into_iter().map(|row| (row.id, row)).collect::<HashMap<_, _>>();
    let mut hits = found.iter().filter_map(|id| by_id.get(id)).map(|row| Hit::of(row, &terms)).collect::<Vec<_>>();
    match search.sort.as_deref() {
        Some("newest") => hits.sort_by(|left, right| right.date.as_deref().unwrap_or("").cmp(left.date.as_deref().unwrap_or(""))),
        None | Some("oldest") => hits.sort_by(|left, right| by_date(left.date.as_deref(), right.date.as_deref())),
        _ => {}
    }
    hits.truncate(wanted);
    Ok(hits)
}

/// The one article with text an identifier names: by its number, else among the first eight by id named or cited
/// exactly so. Those are sought among the articles the index finds by the identifier's whole words that it reads as
/// whole tokens too (ASCII letters and digits set off by spaces or punctuation), the whole table only where the index
/// cannot narrow them; none where the index shows none is.
fn article(database: &Connection, index: Option<&Connection>, identifier: &str) -> CoreResult<Option<Row>> {
    static WORD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new("[A-Za-z0-9]+").expect("word"));
    let apart = |character: Option<char>| character.is_none_or(|character| character.is_whitespace() || character == '\u{feff}'
        || "!\"#%&'()*,-./:;?@[\\]_{}".contains(character));
    let named = match direct_id(identifier) {
        Some(id) => vec![id],
        None => {
            let mut words = Vec::new();
            for word in WORD.find_iter(identifier) {
                let whole = apart(identifier[..word.start()].chars().next_back()) && apart(identifier[word.end()..].chars().next());
                if whole && !words.contains(&format!("\"{}\"", word.as_str())) { words.push(format!("\"{}\"", word.as_str())); }
            }
            let narrowed = match index.filter(|_| !words.is_empty()) {
                Some(index) => ids(index, "SELECT rowid FROM article_metadata WHERE article_metadata MATCH ?", vec![words.join(" AND ").into()])?,
                None => Vec::new(),
            };
            if index.is_some() && !words.is_empty() && narrowed.is_empty() {
                return Ok(None);
            }
            let narrowed = if narrowed.len() <= 30000 { narrowed } else { Vec::new() };
            let (within, values) = among(&narrowed);
            let restricted = if narrowed.is_empty() { String::new() } else { format!("article_id {within} AND ") };
            let found = ids(database, &format!("SELECT article_id FROM articles WHERE {restricted}(LOWER(citation_en) = LOWER(?) \
                OR LOWER(name_en) = LOWER(?)) ORDER BY article_id LIMIT 8"), values.chain([Sql::from(identifier.to_owned()), Sql::from(identifier.to_owned())]).collect())?;
            if !narrowed.is_empty() && found.is_empty() {
                return Ok(None);
            }
            found
        }
    };
    let mut found = Vec::new();
    for id in named {
        found.extend(rows(database, &format!("SELECT * FROM articles WHERE article_id = ? AND {WITH_TEXT}"), vec![id.into()])?);
        if found.len() > 1 {
            return Ok(None);
        }
    }
    Ok(found.pop())
}

/// An article as JavaScript is given it, beside its document.
#[derive(Serialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS), ts(rename = "JournalArticle"))]
#[serde(rename_all = "camelCase")]
pub struct Article {
    #[cfg_attr(feature = "binding-types", ts(type = r#""journal""#))]
    pub provider: &'static str,
    pub identity: String,
    pub article_id: i64,
    pub dataset: String,
    pub citation: String,
    pub title: String,
    pub date: Option<String>,
    pub url: String,
    pub upstream_license: Option<String>,
    pub journal_name: Option<String>,
    pub authors: Option<String>,
    #[cfg_attr(feature = "binding-types", ts(type = r#""en""#))]
    pub language: &'static str,
}

/// What an article's document is derived from: its public text, or the package of its printed pages, and its page map.
pub struct Source {
    pub article_id: usize,
    pub url: String,
    pub package: Option<PathBuf>,
    pub text: String,
    pub pages: Vec<JournalPageLabel>,
}

/// An article's printed pages kept as a package the final contracts' registry names: `pages.jsonl` in its directory,
/// relative to the registry's directory or its parent and inside it, and its copy.
fn package(stores: &JournalStores, article: i64) -> CoreResult<Option<(PathBuf, Stamp)>> {
    let directory = sqlite::read(&stores.contracts, |registry, _| {
        let registered = one(registry, "SELECT 1 FROM sqlite_master WHERE type='table' AND name='article_final_contracts'", [], |_| Ok(()))?;
        Ok(match registered {
            Some(()) => one(registry, "SELECT source_dir FROM article_final_contracts WHERE article_id = ?", [article], |row| row.get_ref(0).map(text))?.flatten(),
            None => None,
        })
    })?.flatten().unwrap_or_default();
    let drive = directory.as_bytes().get(1..3).is_some_and(|after| after[0] == b':' && matches!(after[1], b'/' | b'\\'));
    if directory.is_empty() || directory.starts_with(['/', '\\']) || drive || Path::new(&directory).is_absolute() {
        return Ok(None);
    }
    let parent = stores.contracts.parent().unwrap_or(Path::new(""));
    for base in [parent, parent.parent().unwrap_or(parent)] {
        let mut candidate = base.to_path_buf();
        for part in directory.split(['/', '\\']).filter(|part| !part.is_empty()).chain(["pages.jsonl"]) {
            match part { "." => {} ".." => { candidate.pop(); } part => candidate.push(part) }
        }
        // An unreadable registration is equivalent to no canonical package.
        let real = (candidate.starts_with(base) && candidate.exists()).then(|| Some((std::fs::canonicalize(base).ok()?, std::fs::canonicalize(&candidate).ok()?))).flatten();
        if let Some((_, real)) = real.filter(|(base, real)| real.starts_with(base) && real.is_file()) {
            let metadata = std::fs::metadata(&real).map_err(|error| error.to_string())?;
            return Ok(Some((real, (metadata.len(), metadata.modified().ok()))));
        }
    }
    Ok(None)
}

/// The documents derived, kept while JavaScript holds them.
static DERIVED: Mutex<Vec<(String, Weak<dyn Any + Send + Sync>)>> = Mutex::new(Vec::new());

/// The article an identifier names, with its document (`make`); None where no one article with a public text and
/// address answers.
pub fn document<D: Send + Sync + 'static>(stores: &JournalStores, identifier: &str, make: impl FnOnce(Source) -> CoreResult<D>)
    -> CoreResult<Option<(Article, Arc<D>)>> {
    let identifier = js_trim(identifier);
    if identifier.is_empty() {
        return Err("identifier is required".to_owned());
    }
    let found = with_index(stores, |database, index| {
        let Some(row) = article(database, index, identifier)? else { return Ok(None) };
        let (Some(text), Some(url)) = (row.get("text"), row.url()) else { return Ok(None) };
        let mut statement = database.prepare_cached("SELECT CAST(page_label AS TEXT), CAST(pdf_page AS INTEGER) FROM article_pages \
            WHERE article_id = ? ORDER BY page_order").map_err(sql)?;
        let pages = statement.query_map([row.id], |page| Ok(JournalPageLabel { label: page.get(0)?, pdf_page: page.get::<_, i64>(1)?.max(0) as usize })).map_err(sql)?
            .collect::<Result<Vec<_>, _>>().map_err(sql)?;
        Ok(Some((row, text, url, pages)))
    })?;
    let Some((row, text, url, pages)) = found else { return Ok(None) };
    let package = package(stores, row.id)?;
    let key = format!("{}\n{}\n{:?}", database_path(stores)?.display(), row.id, package);
    let article = Article { provider: "journal", identity: row.id.to_string(), article_id: row.id, dataset: row.get("dataset").unwrap_or_default(),
        citation: row.citation(), title: row.name(), date: row.get("document_date_en"), url: url.clone(),
        upstream_license: row.get("upstream_license"), journal_name: row.get("journal_name"), authors: row.get("authors"), language: "en" };
    let held = DERIVED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().find(|(kept, _)| *kept == key)
        .and_then(|(_, document)| document.upgrade()?.downcast::<D>().ok());
    if let Some(document) = held {
        return Ok(Some((article, document)));
    }
    let document = Arc::new(make(Source { article_id: row.id as usize, url, package: package.map(|(file, _)| file), text, pages })?);
    let mut derived = DERIVED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    derived.retain(|(kept, document)| *kept != key && document.strong_count() > 0);
    derived.push((key, Arc::downgrade(&(document.clone() as Arc<dyn Any + Send + Sync>))));
    Ok(Some((article, document)))
}

/// The article a citation (or one of its other forms) names: one whose citation or name it is, else the one its words
/// (each taken literally) find whose citation or name one of its forms is, else, where none is, the one its title finds.
pub fn named(stores: &JournalStores, texts: &[String]) -> CoreResult<Option<i64>> {
    let candidates = texts.iter().map(|text| text.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|text| !text.is_empty()).collect::<Vec<_>>();
    let Some(first) = candidates.first() else { return Ok(None) };
    with_index(stores, |database, index| {
        for candidate in &candidates {
            if let Some(row) = article(database, index, candidate)?.filter(|row| row.get("text").is_some() && row.url().is_some()) {
                return Ok(Some(row.id));
            }
        }
        let identity = |value: &str| js_trim(value).nfkc().collect::<String>().to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
        let words = first.nfkc().collect::<String>().split(|c: char| !c.is_alphanumeric()).filter(|word| word.encode_utf16().count() > 1)
            .take(8).map(|word| format!("\"{word}\"")).collect::<Vec<_>>().join(" ");
        let wanted = candidates.iter().map(|candidate| identity(candidate)).collect::<Vec<_>>();
        let matches = if words.is_empty() { Vec::new() } else {
            find(database, index, &Search { query: words, size: Some(10.0), names: true, ..Search::default() })?.into_iter()
                .filter(|hit| wanted.contains(&identity(&hit.citation)) || wanted.contains(&identity(&hit.name))).collect()
        };
        match matches.as_slice() {
            [one] => Ok(Some(one.article_id)),
            [] => titled(database, index, first),
            _ => Ok(None),
        }
    })
}

/// The article a citation names by the title it quotes (journal_titles plans the searches and chooses), among the five
/// best names each search finds, oldest first.
fn titled(database: &Connection, index: Option<&Connection>, citation: &str) -> CoreResult<Option<i64>> {
    let mut hits = Vec::new();
    for search in crate::journal_title_searches(citation) {
        let index = index.ok_or("Boolean search requires the journal FTS index")?;
        let found = ids(index, "SELECT rowid FROM article_metadata WHERE article_metadata MATCH ? ORDER BY bm25(article_metadata) LIMIT 5",
            vec![search.query.into()])?;
        let (within, values) = among(&found);
        let mut names = if found.is_empty() { Vec::new() } else {
            rows(database, &format!("SELECT article_id, name_en, document_date_en FROM articles WHERE article_id {within}"), values.collect())?
        };
        names.sort_by_key(|row| found.iter().position(|id| *id == row.id));
        names.sort_by(|left, right| by_date(left.get("document_date_en").as_deref(), right.get("document_date_en").as_deref()));
        hits.extend(names.iter().map(|row| crate::JournalTitleHit { title: search.title.clone(), name: row.name(), article_id: row.id }));
    }
    Ok(crate::journal_title_choice(&hits))
}

/// An article's PDF opened at a printed page: the database's page map gives the PDF page, which counts cover and front
/// pages the printed numbers skip. None where the map lacks the page.
pub fn page_link(stores: &JournalStores, identity: &str, printed: &str) -> CoreResult<Option<String>> {
    with_index(stores, |database, index| {
        let Some(row) = article(database, index, identity)? else { return Ok(None) };
        let Some(url) = row.url() else { return Ok(None) };
        let page = one(database, "SELECT CAST(pdf_page AS INTEGER) FROM article_pages WHERE article_id = ? AND CAST(page_label AS TEXT) = ? \
            ORDER BY page_order LIMIT 1", rusqlite::params![row.id, js_trim(printed)], |page| page.get::<_, Option<i64>>(0))?.flatten();
        Ok(page.filter(|page| *page != 0).and_then(|page| source_url(&url, Some(&format!("page={page}")))))
    })
}

/// Forgets the documents derived (the store's connections close with their copies).
pub fn forget() {
    DERIVED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    sqlite::forget();
}
