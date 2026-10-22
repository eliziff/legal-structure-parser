//! Beaver's citator: the note-up graph and the queries it answers.
//!
//! The graph is built from the local A2AJ store (a2aj.sqlite) and the journal store (public_endpoint.db). From the
//! A2AJ store: for each decision with text, every full citation of a case it makes, with the paragraph it stands in,
//! its paragraph pinpoints and a bounded excerpt, and the store's own curated cited/citing lists beside them. From
//! the journal store: each paired footnote whose first case citation names a decision, with the sentence of the
//! article the footnote annotates (the note pairing's proposition). Citations and keys come from the citation
//! engine, paragraphs and note pairing from the structure engine. The graph is written to `<output>.new` and
//! swapped in whole.
//!
//! The queries take the cited decision's keys (its alias group in the A2AJ store's citation index) and answer a
//! note-up: the decisions citing it, and the passages, judicial and academic, that say what it stands for.
//! Offsets and lengths are UTF-16 code units, as Beaver counts them.

use crate::{
    caselaw_citation_lookup_key, classify_citator_excerpt, pair_numbered_footnotes, provider_text_document_structure,
    DocumentKind, DocumentQuery, ProviderTextInput, ProviderTextSourceKind,
};
use legal_citations::{Authority, Citation, Form, Options, PinpointKind};
use regex::Regex;
use rusqlite::{params, params_from_iter, types::ValueRef, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, LazyLock, Mutex};
use std::time::{Instant, SystemTime};

pub type GraphResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const EXCERPT_BEFORE: usize = 250;
const EXCERPT_AFTER: usize = 350;
const EXCERPT_MAX: usize = 600;
/// The citing passages an analysis reads, newest first, before ranking them.
const STANDS_FOR_CONSIDERED: usize = 300;

pub struct GraphOptions {
    pub store: PathBuf,
    /// The journal store; its commentary is left out where it is not installed.
    pub journals: PathBuf,
    pub output: PathBuf,
    /// Only these datasets (court codes), when given.
    pub courts: Vec<String>,
    /// At most this many decisions and this many articles, when given.
    pub limit: Option<usize>,
    pub threads: usize,
}

/// What a build read and wrote; also the graph's `meta` table.
#[derive(Default, Serialize)]
pub struct GraphSummary {
    pub cases_indexed: u64,
    pub rows_skipped_no_text: u64,
    pub rows_skipped_duplicate: u64,
    pub case_citation_occurrences: u64,
    pub self_citations_skipped: u64,
    pub edges: u64,
    pub provider_cited_edges: u64,
    pub provider_citing_edges: u64,
    pub provider_keys_mined_confirmed: u64,
    pub provider_keys_unmined: u64,
    pub articles: u64,
    pub notes_paired: u64,
    pub commentary: u64,
    pub failures: u64,
    pub seconds: f64,
}

/// A citation's case-law key; none for text the engine reads no single citation in.
fn key_of(citation: &str) -> Option<String> {
    caselaw_citation_lookup_key(citation).ok().filter(|key| !key.is_empty())
}

/// The engine's case citations in `text`, with the name each form counts as.
fn case_citations(text: &str) -> Vec<Citation> {
    legal_citations::extract(text, &Options::default()).into_iter()
        .filter(|citation| citation.form == Form::Full && citation.authority == Authority::Case && citation.key.is_some())
        .collect()
}

/// A citation's paragraph pinpoints, as the graph stores them: "par12,par14".
fn paragraph_pinpoints(citation: &Citation) -> Option<String> {
    let values: Vec<String> = citation.pinpoints.iter().filter(|pin| pin.kind == PinpointKind::Paragraph)
        .flat_map(|pin| std::iter::once(pin.first.clone()).chain(pin.last.clone()))
        .filter(|value| !value.is_empty()).map(|value| format!("par{value}")).collect();
    (!values.is_empty()).then(|| values.join(","))
}

/// Runs `work` over what `produce` sends, on `threads` workers, handing each result to `consume` on this thread.
/// A job the engine panics on yields its error; the rest go on.
fn parallel<J: Send, R: Send>(
    threads: usize,
    produce: impl FnOnce(&mpsc::SyncSender<J>) -> GraphResult<()> + Send,
    work: impl Fn(&J) -> R + Sync,
    mut consume: impl FnMut(J, Result<R, String>) -> GraphResult<()>,
) -> GraphResult<()> {
    let (jobs, queue) = mpsc::sync_channel::<J>(threads * 8);
    let queue = Arc::new(Mutex::new(queue));
    let (done, results) = mpsc::channel::<(J, Result<R, String>)>();
    std::thread::scope(|scope| -> GraphResult<()> {
        for _ in 0..threads.max(1) {
            let (queue, done, work) = (Arc::clone(&queue), done.clone(), &work);
            std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(scope, move || loop {
                let Ok(job) = queue.lock().unwrap().recv() else { break };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&job))).map_err(|panic| {
                    panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|value| value.to_string()))
                        .unwrap_or_else(|| "the engine panicked".to_owned())
                });
                if done.send((job, result)).is_err() { break; }
            })?;
        }
        drop(done);
        let producer = scope.spawn(move || produce(&jobs));
        for (job, result) in results { consume(job, result)?; }
        producer.join().map_err(|_| "the store reader panicked")?
    })
}

fn text(value: ValueRef) -> Option<String> {
    match value {
        ValueRef::Text(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()).filter(|value| !value.trim().is_empty()),
        _ => None,
    }
}

// ---- Decisions ---------------------------------------------------------------------------------------------------

struct Case {
    sequence: i64,
    store_id: i64,
    dataset: String,
    citation: Option<String>,
    name: Option<String>,
    date: Option<String>,
    url: Option<String>,
    language: &'static str,
    text: String,
    own: HashSet<String>,
    provider_cited: Vec<String>,
    provider_citing: Vec<String>,
}

struct Edge {
    key: String,
    citation: String,
    paragraph: Option<i64>,
    offset: usize,
    pinpoints: Option<String>,
    excerpt: String,
}

/// The text around an occurrence, white space collapsed, at most EXCERPT_MAX code units.
fn excerpt(units: &[u16], start: usize, end: usize) -> String {
    let window = String::from_utf16_lossy(&units[start.saturating_sub(EXCERPT_BEFORE)..(end + EXCERPT_AFTER).min(units.len())]);
    let collapsed = window.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut taken = 0;
    collapsed.chars().take_while(|character| { taken += character.len_utf16(); taken <= EXCERPT_MAX }).collect()
}

/// A decision's citations of other cases: the edges it gives, and how many cited itself.
fn case_edges(case: &Case) -> (Vec<Edge>, u64, u64) {
    let citations = case_citations(&case.text);
    let total = citations.len() as u64;
    let citations: Vec<&Citation> = citations.iter().filter(|citation| !case.own.contains(citation.key.as_ref().unwrap())).collect();
    let own = total - citations.len() as u64;
    if citations.is_empty() { return (Vec::new(), total, own); }
    // The structure engine's numbered paragraphs, by character range; a citation is in the one holding its start.
    let paragraphs: Vec<(i64, usize, usize)> = provider_text_document_structure(ProviderTextInput {
        provider: Some("a2aj".to_owned()),
        citation: case.citation.clone().unwrap_or_default(),
        source_kind: ProviderTextSourceKind::Cases,
        text: case.text.clone(),
        id: None,
        url: None,
        dataset: Some(case.dataset.clone()),
        name: None,
        alternate_citation: None,
        section_map: None,
        excerpt_of: None,
        require_report_start: case.dataset.eq_ignore_ascii_case("SCC"),
    }).map(|structure| {
        DocumentQuery::new().anchors(&structure, None)
            .filter(|anchor| anchor.kind() == DocumentKind::Paragraph && anchor.parent_label().is_none())
            .filter_map(|anchor| {
                let number = anchor.label().split(|character: char| !character.is_ascii_digit())
                    .find(|digits| !digits.is_empty())?.parse().ok().filter(|number| *number != 0)?;
                let (start, end) = anchor.range();
                Some((number, start, end))
            }).collect()
    }).unwrap_or_default();
    // Each citation's start in characters and both ends in UTF-16 units, from its byte span, in one walk.
    let mut bytes: Vec<usize> = citations.iter().flat_map(|citation| [citation.span.start, citation.span.end]).collect();
    bytes.sort_unstable();
    bytes.dedup();
    let mut at: HashMap<usize, (usize, usize)> = HashMap::with_capacity(bytes.len());
    let (mut next, mut characters, mut units) = (0, 0, 0);
    for (byte, character) in case.text.char_indices().chain(std::iter::once((case.text.len(), ' '))) {
        while next < bytes.len() && bytes[next] <= byte { at.insert(bytes[next], (characters, units)); next += 1; }
        if next == bytes.len() { break; }
        characters += 1;
        units += character.len_utf16();
    }
    let text: Vec<u16> = case.text.encode_utf16().collect();
    let edges = citations.into_iter().map(|citation| {
        let (character, start) = at[&citation.span.start];
        let end = at[&citation.span.end].1;
        Edge {
            key: citation.key.clone().unwrap(),
            citation: citation.span.text.clone(),
            paragraph: paragraphs.iter().find(|(_, from, to)| *from <= character && character < *to).map(|(number, ..)| *number),
            offset: start,
            pinpoints: paragraph_pinpoints(citation),
            excerpt: excerpt(&text, start, end),
        }
    }).collect();
    (edges, total, own)
}

/// A curated citation list, in the scanned text's language, else the other; an empty list is the provider's.
fn provider_list(cells: &HashMap<&str, Option<String>>, base: &str, language: &str) -> Vec<String> {
    let other = if language == "en" { "fr" } else { "en" };
    for candidate in [language, other] {
        let Some(value) = cells.get(format!("{base}_{candidate}").as_str()).cloned().flatten() else { continue };
        return serde_json::from_str::<Vec<serde_json::Value>>(&value).unwrap_or_default().into_iter()
            .filter_map(|item| item.as_str().map(|value| value.trim().to_owned()).filter(|value| !value.is_empty()))
            .collect();
    }
    Vec::new()
}

const CASE_COLUMNS: [&str; 18] = ["id", "dataset", "citation_en", "citation_fr", "citation2_en", "citation2_fr", "name_en",
    "name_fr", "document_date_en", "document_date_fr", "url_en", "url_fr", "unofficial_text_en", "unofficial_text_fr",
    "cases_cited_en", "cases_cited_fr", "cases_citing_en", "cases_citing_fr"];

fn build_cases(graph: &Connection, options: &GraphOptions, summary: &mut GraphSummary, started: Instant) -> GraphResult<()> {
    let courts: Vec<String> = options.courts.iter().map(|court| court.trim().to_uppercase()).filter(|court| !court.is_empty()).collect();
    let query = format!("SELECT {} FROM document WHERE doc_type = 'cases'{} ORDER BY dataset, id{}", CASE_COLUMNS.join(", "),
        if courts.is_empty() { String::new() } else { format!(" AND UPPER(dataset) IN ({})", vec!["?"; courts.len()].join(", ")) },
        options.limit.map_or(String::new(), |limit| format!(" LIMIT {limit}")));
    let store_path = options.store.clone();
    let skipped = Arc::new(Mutex::new((0u64, 0u64)));
    let counted = Arc::clone(&skipped);
    let mut add_case = graph.prepare("INSERT INTO case_doc VALUES (?, ?, ?, ?, ?, ?, ?, ?)")?;
    let mut add_edge = graph.prepare("INSERT INTO edge (cited_key, cited_citation, case_id, paragraph, text_offset, pinpoints,
        excerpt) VALUES (?, ?, ?, ?, ?, ?, ?)")?;
    let mut add_provider = graph.prepare("INSERT INTO provider_edge (case_id, direction, citation, citation_key) VALUES (?, ?, ?, ?)")?;
    let mut add_key = graph.prepare("INSERT OR IGNORE INTO case_key VALUES (?, ?)")?;
    parallel(options.threads, move |jobs| {
        let store = Connection::open_with_flags(&store_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut statement = store.prepare(&query)?;
        let mut rows = statement.query(params_from_iter(courts.iter()))?;
        let (mut seen, mut sequence) = (HashSet::new(), 0);
        while let Some(row) = rows.next()? {
            let cells: HashMap<&str, Option<String>> = CASE_COLUMNS.iter().enumerate()
                .map(|(index, column)| Ok((*column, text(row.get_ref(index)?))))
                .collect::<rusqlite::Result<_>>()?;
            let field = |name: &str| cells.get(name).cloned().flatten();
            let either = |name: &str| field(&format!("{name}_en")).or_else(|| field(&format!("{name}_fr")));
            let english = field("unofficial_text_en");
            let language = if english.is_some() { "en" } else { "fr" };
            let Some(body) = english.or_else(|| field("unofficial_text_fr")) else { counted.lock().unwrap().0 += 1; continue };
            let identity = either("url").map(|url| format!("u:{url}")).or_else(|| field("citation_en").map(|citation| format!("c:{citation}")));
            if identity.is_some_and(|identity| !seen.insert(identity)) { counted.lock().unwrap().1 += 1; continue; }
            sequence += 1;
            let case = Case {
                sequence,
                store_id: row.get(0)?,
                dataset: field("dataset").unwrap_or_default(),
                citation: either("citation"),
                name: either("name"),
                date: either("document_date").map(|date| date.chars().take(10).collect()),
                url: either("url"),
                language,
                text: body,
                own: ["citation_en", "citation2_en", "citation_fr", "citation2_fr"].iter()
                    .filter_map(|name| field(name).and_then(|citation| key_of(&citation))).collect(),
                provider_cited: provider_list(&cells, "cases_cited", language),
                provider_citing: provider_list(&cells, "cases_citing", language),
            };
            if jobs.send(case).is_err() { break; }
        }
        Ok(())
    }, case_edges, |case, result| {
        let (edges, total, own) = result.unwrap_or_else(|error| {
            summary.failures += 1;
            eprintln!("{} {}: {error}", case.dataset, case.citation.as_deref().unwrap_or_default());
            (Vec::new(), 0, 0)
        });
        summary.cases_indexed += 1;
        summary.case_citation_occurrences += total;
        summary.self_citations_skipped += own;
        summary.edges += edges.len() as u64;
        add_case.execute(params![case.sequence, case.store_id, case.citation, case.name, case.dataset, case.date, case.url,
            case.language])?;
        for edge in &edges {
            add_edge.execute(params![edge.key, edge.citation, case.sequence, edge.paragraph, edge.offset as i64,
                edge.pinpoints, edge.excerpt])?;
        }
        for key in &case.own { add_key.execute(params![case.sequence, key])?; }
        let mined: HashSet<&str> = edges.iter().map(|edge| edge.key.as_str()).collect();
        for (direction, citations) in [("cited", &case.provider_cited), ("citing", &case.provider_citing)] {
            for citation in citations {
                let Some(key) = key_of(citation) else { continue };
                add_provider.execute(params![case.sequence, direction, citation, key])?;
                if direction == "citing" { summary.provider_citing_edges += 1; continue; }
                summary.provider_cited_edges += 1;
                if mined.contains(key.as_str()) { summary.provider_keys_mined_confirmed += 1 } else { summary.provider_keys_unmined += 1 }
            }
        }
        if summary.cases_indexed % 10_000 == 0 {
            eprintln!("{} cases, {} edges, {:.0} s", summary.cases_indexed, summary.edges, started.elapsed().as_secs_f64());
        }
        Ok(())
    })?;
    let (no_text, duplicate) = *skipped.lock().unwrap();
    summary.rows_skipped_no_text = no_text;
    summary.rows_skipped_duplicate = duplicate;
    Ok(())
}

// ---- Journal commentary ------------------------------------------------------------------------------------------

/// Reporter readings that are really access dates or volume/issue numbers.
static NOT_A_REPORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"(?i)^(?:\d{1,2}\s+(?:January|February|March|April|May|June|July|August|September|October|November|December|janvier|février|mars|avril|mai|juin|juillet|août|septembre|octobre|novembre|décembre)\s+\d{4}|\d{1,4}\s+No\.?\s+\d+)$",
).expect("pattern"));

struct Article {
    id: i64,
    text: String,
    pages: Vec<String>,
}

struct Comment {
    key: String,
    pinpoints: Option<String>,
    proposition: String,
    page: Option<String>,
}

/// An article's paired footnotes whose first case citation names a decision, with the sentence each annotates. The
/// notes are read together, numbered, so a supra or ibid counts for the decision it refers to (at the Safe tier), with
/// its own pinpoint.
fn article_commentary(article: &Article) -> (Vec<Comment>, u64) {
    let pairing = pair_numbered_footnotes(&article.text, &article.pages);
    let mut notes = String::new();
    let ranges: Vec<legal_citations::NoteRange> = pairing.notes.iter().map(|note| {
        if !notes.is_empty() { notes.push_str("\n\n"); }
        let start = notes.len();
        notes.push_str(&note.body);
        legal_citations::NoteRange { number: note.label.trim().parse().unwrap_or(0), start, end: notes.len(),
            sequence: note.restart_sequence as u32, anchor: None }
    }).collect();
    // References resolve at the Safe tier, the most conservative: it refuses the links the other tiers risk.
    let citations = legal_citations::extract(&notes, &Options { notes: Some(ranges.clone()),
        split_tier: Some(legal_citations::SplitTier::Safe), ..Options::default() });
    // Each note's first citation of a case, as the full citation it is or refers to.
    let mut first: Vec<Option<(&Citation, &Citation)>> = vec![None; ranges.len()];
    for citation in &citations {
        let Some(note) = ranges.iter().position(|range| range.start <= citation.span.start && citation.span.start < range.end)
            else { continue };
        if first[note].is_some() { continue; }
        let full = &citations[citation.authority_index()];
        if full.form != Form::Full || full.authority != Authority::Case || full.key.is_none() { continue; }
        if full.format == Some(legal_citations::Format::Reporter) && NOT_A_REPORT.is_match(full.span.text.trim()) { continue; }
        first[note] = Some((citation, full));
    }
    let mut paired = 0;
    let comments = pairing.notes.iter().zip(first).filter_map(|(note, cited)| {
        let page = note.ref_page_index?;
        paired += 1;
        let proposition = note.proposition.as_deref().map(str::trim).filter(|value| !value.is_empty())?;
        let (citing, full) = cited?;
        Some(Comment {
            key: full.key.clone()?,
            pinpoints: paragraph_pinpoints(citing),
            proposition: proposition.to_owned(),
            page: pairing.page_labels.get(page).cloned().flatten(),
        })
    }).collect();
    (comments, paired)
}

/// An article's citation as McGill writes it, where the store has none.
fn article_citation(authors: Option<&str>, title: Option<&str>, year: Option<&str>, journal: Option<&str>) -> Option<String> {
    if title.is_none() && journal.is_none() { return None; }
    legal_citations::api::call_value("formatArticle", serde_json::json!({
        "authors": authors.unwrap_or_default().split(';').map(str::trim).filter(|author| !author.is_empty()).collect::<Vec<_>>(),
        "title": title.unwrap_or_default(), "year": year, "journal": journal,
    })).ok().and_then(|value| value.as_str().map(str::to_owned))
}

fn build_commentary(graph: &Connection, options: &GraphOptions, summary: &mut GraphSummary) -> GraphResult<()> {
    let journals = Connection::open_with_flags(&options.journals, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut add_article = graph.prepare("INSERT INTO article VALUES (?, ?, ?, ?, ?)")?;
    {
        let mut rows = journals.prepare("SELECT article_id, citation_en, name_en, authors, document_date_en, journal_name,
            galley_url, url_en FROM articles WHERE text IS NOT NULL AND length(text) > 0")?;
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            let cell = |index| row.get_ref(index).map(text);
            let (name, authors, date, journal) = (cell(2)?, cell(3)?, cell(4)?, cell(5)?);
            let citation = cell(1)?.or_else(|| article_citation(authors.as_deref(), name.as_deref(), date.as_deref(), journal.as_deref()));
            add_article.execute(params![row.get::<_, i64>(0)?, citation, date, journal, cell(6)?.or(cell(7)?)])?;
        }
    }
    let journals_path = options.journals.clone();
    let limit = options.limit;
    let mut add = graph.prepare("INSERT INTO commentary (article_id, cited_key, pinpoints, proposition, page_label)
        VALUES (?, ?, ?, ?, ?)")?;
    parallel(options.threads, move |jobs| {
        let journals = Connection::open_with_flags(&journals_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut pages = journals.prepare("SELECT page_label FROM article_pages WHERE article_id = ? ORDER BY page_order")?;
        let mut statement = journals.prepare(&format!("SELECT article_id, text FROM articles WHERE text IS NOT NULL
            AND length(text) > 0 ORDER BY article_id{}", limit.map_or(String::new(), |limit| format!(" LIMIT {limit}"))))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let pages = pages.query_map([id], |page| Ok(page.get_ref(0)?.as_str().map(str::trim).unwrap_or_default().to_owned()))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if jobs.send(Article { id, text: row.get(1)?, pages }).is_err() { break; }
        }
        Ok(())
    }, article_commentary, |article, result| {
        let (comments, paired) = result.unwrap_or_else(|error| {
            summary.failures += 1;
            eprintln!("journal article {}: {error}", article.id);
            (Vec::new(), 0)
        });
        summary.articles += 1;
        summary.notes_paired += paired;
        summary.commentary += comments.len() as u64;
        for comment in comments {
            add.execute(params![article.id, comment.key, comment.pinpoints, comment.proposition, comment.page])?;
        }
        Ok(())
    })
}

// ---- Build -------------------------------------------------------------------------------------------------------

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE case_doc (id INTEGER PRIMARY KEY, store_id INTEGER NOT NULL, citation TEXT, name TEXT, court TEXT, date TEXT,
  url TEXT, language TEXT NOT NULL);
CREATE TABLE edge (id INTEGER PRIMARY KEY, cited_key TEXT NOT NULL, cited_citation TEXT NOT NULL,
  case_id INTEGER NOT NULL REFERENCES case_doc(id), paragraph INTEGER, text_offset INTEGER NOT NULL, pinpoints TEXT,
  excerpt TEXT NOT NULL);
CREATE TABLE case_key (case_id INTEGER NOT NULL REFERENCES case_doc(id), citation_key TEXT NOT NULL,
  PRIMARY KEY (case_id, citation_key)) WITHOUT ROWID;
CREATE TABLE provider_edge (id INTEGER PRIMARY KEY, case_id INTEGER NOT NULL REFERENCES case_doc(id),
  direction TEXT NOT NULL CHECK (direction IN ('cited', 'citing')), citation TEXT NOT NULL, citation_key TEXT NOT NULL);
CREATE TABLE authority_metric (cited_key TEXT PRIMARY KEY, citing_cases INTEGER NOT NULL, citing_paragraphs INTEGER NOT NULL,
  occurrences INTEGER NOT NULL) WITHOUT ROWID;";

const COMMENTARY_SCHEMA: &str = "
CREATE TABLE article (article_id INTEGER PRIMARY KEY, citation TEXT, date TEXT, journal_name TEXT, url TEXT);
CREATE TABLE commentary (id INTEGER PRIMARY KEY, article_id INTEGER NOT NULL REFERENCES article(article_id),
  cited_key TEXT NOT NULL, pinpoints TEXT, proposition TEXT NOT NULL, page_label TEXT);";

const INDEXES: &str = "
CREATE INDEX edge_cited_idx ON edge(cited_key, case_id, text_offset);
CREATE INDEX case_key_key_idx ON case_key(citation_key);
CREATE INDEX provider_edge_key_idx ON provider_edge(citation_key, direction);
CREATE INDEX provider_edge_case_idx ON provider_edge(case_id);
INSERT INTO authority_metric SELECT cited_key, COUNT(DISTINCT case_id),
  COUNT(DISTINCT CAST(case_id AS TEXT) || ':' || COALESCE(CAST(paragraph AS TEXT), '')), COUNT(*) FROM edge GROUP BY cited_key;";

/// Builds the graph at `options.output` (beside the A2AJ store, where the queries read it).
pub fn build_citator_graph(options: &GraphOptions) -> GraphResult<GraphSummary> {
    let started = Instant::now();
    let temporary = PathBuf::from(format!("{}.new", options.output.display()));
    let _ = std::fs::remove_file(&temporary);
    if let Some(parent) = options.output.parent() { std::fs::create_dir_all(parent)?; }
    let graph = Connection::open(&temporary)?;
    graph.execute_batch(&format!("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF; PRAGMA temp_store = MEMORY; {SCHEMA}"))?;
    let mut summary = GraphSummary::default();
    graph.execute_batch("BEGIN")?;
    build_cases(&graph, options, &mut summary, started)?;
    if options.journals.is_file() {
        graph.execute_batch(COMMENTARY_SCHEMA)?;
        build_commentary(&graph, options, &mut summary)?;
        graph.execute_batch("CREATE INDEX commentary_key_idx ON commentary(cited_key);")?;
    }
    graph.execute_batch("COMMIT")?;
    graph.execute_batch(INDEXES)?;
    summary.seconds = started.elapsed().as_secs_f64();
    let mut meta = vec![("builder".to_owned(), "legal-structure citator-graph".to_owned()),
        ("store".to_owned(), options.store.display().to_string()), ("journals".to_owned(), options.journals.display().to_string())];
    if let serde_json::Value::Object(values) = serde_json::to_value(&summary)? {
        meta.extend(values.into_iter().map(|(key, value)| (key, value.to_string())));
    }
    for (key, value) in &meta { graph.execute("INSERT INTO meta VALUES (?, ?)", params![key, value])?; }
    graph.execute_batch("ANALYZE")?;
    drop(graph);
    std::fs::rename(&temporary, &options.output)?;
    Ok(summary)
}

/// Where Beaver keeps its local legal data: OPEN_LEGAL_DATA_HOME, else %LOCALAPPDATA%/OpenLegalData.
fn legal_data_home() -> PathBuf {
    if let Some(home) = std::env::var_os("OPEN_LEGAL_DATA_HOME").filter(|value| !value.is_empty()) { return home.into(); }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("AppData").join("Local"));
    local.join("OpenLegalData")
}

fn configured(variable: &str, provider: &str, file: &str) -> PathBuf {
    std::env::var_os(variable).filter(|value| !value.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| legal_data_home().join("providers").join(provider).join(file))
}

pub fn default_store() -> PathBuf { configured("MIKE_A2AJ_BULK_DB", "a2aj", "a2aj.sqlite") }
pub fn default_journals() -> PathBuf { configured("MIKE_PUBLIC_ENDPOINT_DB", "journals", "public_endpoint.db") }
/// The graph built from, and read beside, the A2AJ store at `store`.
pub fn graph_path(store: &Path) -> PathBuf { store.with_file_name("noteup.sqlite") }

// ---- Queries -----------------------------------------------------------------------------------------------------

type Stamp = (u64, Option<SystemTime>);
/// Read-only connections to graphs, kept while their copy of the file is.
static GRAPHS: Mutex<Vec<(PathBuf, Stamp, Connection)>> = Mutex::new(Vec::new());

/// Runs `work` on a read-only connection to the graph; None where no graph is built.
fn with_graph<T>(path: &Path, work: impl FnOnce(&Connection) -> GraphResult<T>) -> GraphResult<Option<T>> {
    let Ok(metadata) = std::fs::metadata(path) else { return Ok(None) };
    let stamp = (metadata.len(), metadata.modified().ok());
    let kept = {
        let mut graphs = GRAPHS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        graphs.retain(|(other, held, _)| other != path || *held == stamp);
        graphs.iter().position(|(other, ..)| other == path).map(|index| graphs.swap_remove(index).2)
    };
    let connection = match kept {
        Some(connection) => connection,
        None => Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?,
    };
    let result = work(&connection);
    GRAPHS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push((path.to_owned(), stamp, connection));
    result.map(Some)
}

/// A court's place in the hierarchy, from the citation engine's registry: 5 apex, 4 appellate, 3 superior or
/// federal trial, 2 inferior trial, 1 tribunal.
pub fn court_level(code: &str) -> Option<u8> {
    static LEVELS: LazyLock<Mutex<HashMap<String, Option<u8>>>> = LazyLock::new(Default::default);
    let surface = code.trim().to_uppercase();
    if surface.is_empty() { return None; }
    if let Some(level) = LEVELS.lock().unwrap().get(&surface) { return *level; }
    let level = legal_citations::api::call_value("registry", serde_json::json!({ "table": "courts", "surface": surface }))
        .ok().and_then(|courts| match courts[0]["hierarchy"].as_str()? {
            "apex" => Some(5), "appellate" => Some(4), "superior_trial" => Some(3), "inferior_trial" => Some(2),
            "tribunal" => Some(1), _ => None,
        });
    LEVELS.lock().unwrap().insert(surface, level);
    level
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteUpRequest {
    pub citation: String,
    /// The cited decision's keys: its alias group.
    #[serde(default)]
    pub keys: Vec<String>,
    pub cited_paragraph: Option<i64>,
    pub size: Option<usize>,
    pub court_scope: Option<String>,
    pub court_code: Option<String>,
    pub sort: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteUpEntry {
    pub citation: Option<String>,
    pub name: Option<String>,
    pub court: Option<String>,
    pub date: Option<String>,
    pub language: String,
    pub url: Option<String>,
    pub paragraph: Option<i64>,
    pub occurrences: i64,
    pub distinct_paragraphs: i64,
    pub court_level: Option<u8>,
    pub cited_as: String,
    pub pinpoints: Option<String>,
    pub excerpt: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCiting {
    pub citing_in_corpus: i64,
    pub citing_reported: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StandsFor {
    pub source_kind: &'static str,
    pub journal_name: Option<String>,
    pub text: String,
    pub excerpt_kind: &'static str,
    pub citing_citation: Option<String>,
    pub citing_name: Option<String>,
    pub citing_court: Option<String>,
    pub citing_level: Option<u8>,
    pub citing_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub paragraph: Option<i64>,
    pub page_label: Option<String>,
    pub span_sha256: String,
    pub source_article_id: Option<String>,
    pub citing_url: Option<String>,
    #[serde(skip)]
    occurrences: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteUp {
    /// Every citing decision matched, not just the page returned.
    pub total: usize,
    pub entries: Vec<NoteUpEntry>,
    pub provider: Option<ProviderCiting>,
    pub total_citers: i64,
    pub judicial_discussion: Vec<StandsFor>,
    /// None where no journal commentary is built.
    pub journal_analysis: Option<Vec<StandsFor>>,
    pub excerpts_considered: usize,
    pub excerpts_rejected: BTreeMap<&'static str, usize>,
}

fn sha256(text: &str) -> String { format!("{:x}", Sha256::digest(text.as_bytes())) }

/// The decisions citing the request's keys and what those decisions and journal authors say the cited decision
/// stands for; None where no graph is built.
pub fn note_up(store: &Path, request: &NoteUpRequest) -> GraphResult<Option<NoteUp>> {
    let path = &graph_path(store);
    let scope = request.court_scope.as_deref().unwrap_or("all");
    let code = request.court_code.as_deref().map(str::trim).filter(|code| !code.is_empty()).map(str::to_uppercase);
    if request.cited_paragraph.is_some_and(|paragraph| paragraph < 1) { return Err("cited_paragraph must be a positive integer".into()); }
    if code.is_some() && scope != "all" { return Err("court_code cannot be combined with a non-all court_scope".into()); }
    let matches_court = |court: &Option<String>| {
        let court = court.as_deref().unwrap_or_default().trim().to_uppercase();
        if let Some(code) = &code { return court == *code; }
        match (scope, court_level(&court)) {
            ("all", _) => true,
            ("scc", level) => level == Some(5),
            ("appellate", level) => level == Some(4),
            ("trial", level) => matches!(level, Some(2 | 3)),
            (_, level) => level == Some(1),
        }
    };
    let keys = &request.keys;
    let marks = vec!["?"; keys.len()].join(", ");
    let pinpoint = request.cited_paragraph.map(|paragraph| format!(",par{paragraph},"));
    let filter = if pinpoint.is_some() { "AND instr(',' || edge.pinpoints || ',', ?) > 0" } else { "" };
    let bound = || keys.iter().cloned().chain(pinpoint.clone()).map(rusqlite::types::Value::from).collect::<Vec<_>>();
    with_graph(path, |graph| {
        // One row per citing decision; its first occurrence's columns ride the MIN() row (SQLite's bare-column rule).
        let mut groups = graph.prepare(&format!("SELECT case_doc.citation, case_doc.name, case_doc.court, case_doc.date,
                case_doc.url, case_doc.language, case_doc.id, COUNT(*), COUNT(DISTINCT edge.paragraph), MIN(edge.text_offset),
                edge.cited_citation, edge.paragraph, edge.pinpoints, edge.excerpt
            FROM edge JOIN case_doc ON case_doc.id = edge.case_id WHERE edge.cited_key IN ({marks}) {filter}
            GROUP BY edge.case_id"))?
            .query_map(params_from_iter(bound()), |row| Ok((
                NoteUpEntry {
                    citation: row.get(0)?, name: row.get(1)?, court: row.get(2)?, date: row.get(3)?, url: row.get(4)?,
                    language: if row.get::<_, Option<String>>(5)?.as_deref() == Some("fr") { "fr" } else { "en" }.to_owned(),
                    occurrences: row.get(7)?, distinct_paragraphs: row.get(8)?, court_level: None,
                    cited_as: row.get(10)?, paragraph: row.get(11)?, pinpoints: row.get(12)?, excerpt: row.get(13)?,
                },
                row.get::<_, i64>(6)?,
            )))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let total_citers = graph.query_row(&format!("SELECT COUNT(DISTINCT case_id) FROM edge WHERE cited_key IN ({marks})"),
            params_from_iter(keys.iter()), |row| row.get(0))?;
        let newest = |left: &(NoteUpEntry, i64), right: &(NoteUpEntry, i64)| left.0.date.is_none().cmp(&right.0.date.is_none())
            .then_with(|| right.0.date.cmp(&left.0.date)).then_with(|| left.1.cmp(&right.1));
        groups.retain(|(entry, _)| matches_court(&entry.court));
        for (entry, _) in &mut groups { entry.court_level = entry.court.as_deref().and_then(court_level); }

        // What citing decisions say the cited one stands for: the newest first occurrences, by the excerpt classifier.
        groups.sort_by(newest);
        let considered: Vec<&(NoteUpEntry, i64)> = groups.iter().take(STANDS_FOR_CONSIDERED).collect();
        let mut rejected = BTreeMap::from([("authorityList", 0), ("insufficient", 0)]);
        let mut judicial: Vec<StandsFor> = considered.iter().filter_map(|(entry, _)| {
            let verdict = classify_citator_excerpt(&entry.excerpt);
            if verdict.kind == "authority_list" { *rejected.get_mut("authorityList").unwrap() += 1; return None; }
            let Some(window) = verdict.prose_window.filter(|_| verdict.kind != "insufficient")
                else { *rejected.get_mut("insufficient").unwrap() += 1; return None };
            Some(StandsFor {
                source_kind: "case", journal_name: None, span_sha256: sha256(&window), text: window,
                excerpt_kind: if verdict.kind == "prose" { "prose" } else { "mixed" },
                citing_citation: entry.citation.clone(), citing_name: entry.name.clone(), citing_court: entry.court.clone(),
                citing_level: entry.court_level, citing_date: entry.date.clone(), language: Some(entry.language.clone()),
                paragraph: entry.paragraph, page_label: None, source_article_id: None, citing_url: entry.url.clone(),
                occurrences: entry.occurrences,
            })
        }).collect();
        let excerpts_considered = considered.len();
        let cap = request.size.unwrap_or(8).clamp(1, 24);
        let by_date = |left: &StandsFor, right: &StandsFor| left.citing_date.is_none().cmp(&right.citing_date.is_none())
            .then_with(|| right.citing_date.cmp(&left.citing_date));
        judicial.sort_by(|left, right| right.citing_level.cmp(&left.citing_level)
            .then_with(|| right.occurrences.cmp(&left.occurrences)).then_with(|| by_date(left, right)));
        judicial.truncate(cap);

        // What journal authors say it stands for: the sentence each paired footnote citing it first annotates.
        let journal_analysis = if graph.prepare("SELECT 1 FROM sqlite_master WHERE name = 'commentary'")?.exists([])? {
            let filter = if pinpoint.is_some() { "AND instr(',' || commentary.pinpoints || ',', ?) > 0" } else { "" };
            let mut found: Vec<StandsFor> = graph.prepare(&format!("SELECT commentary.proposition, commentary.page_label,
                    article.citation, article.date, article.journal_name, article.article_id, article.url
                FROM commentary JOIN article ON article.article_id = commentary.article_id
                WHERE commentary.cited_key IN ({marks}) {filter}
                ORDER BY (article.date IS NULL), article.date DESC, commentary.id LIMIT {STANDS_FOR_CONSIDERED}"))?
                .query_map(params_from_iter(bound()), |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?, row.get::<_, Option<String>>(6)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?.into_iter()
                .filter_map(|(proposition, page, citation, date, journal, article, url)| {
                    // The classifier gates a proposition but never trims it: a paired proposition is sentence-exact.
                    let verdict = classify_citator_excerpt(&proposition);
                    (matches!(verdict.kind, "prose" | "mixed") && verdict.prose_window.is_some()).then(|| StandsFor {
                        source_kind: "commentary", journal_name: journal, span_sha256: sha256(&proposition), text: proposition,
                        excerpt_kind: if verdict.kind == "prose" { "prose" } else { "mixed" }, citing_citation: citation,
                        citing_name: None, citing_court: None, citing_level: None, citing_date: date, language: None,
                        paragraph: None, page_label: page, source_article_id: Some(article.to_string()), citing_url: url,
                        occurrences: 1,
                    })
                }).collect();
            found.sort_by(by_date);
            found.truncate(cap);
            Some(found)
        } else { None };

        // The page of citing decisions.
        if request.sort.as_deref() == Some("most_discussed") {
            groups.sort_by(|left, right| right.0.occurrences.cmp(&left.0.occurrences)
                .then_with(|| right.0.distinct_paragraphs.cmp(&left.0.distinct_paragraphs))
                .then_with(|| right.0.court_level.unwrap_or(0).cmp(&left.0.court_level.unwrap_or(0)))
                .then_with(|| newest(left, right)));
        }
        let total = groups.len();
        let entries = groups.into_iter().take(request.size.unwrap_or(10).clamp(1, 50)).map(|(entry, _)| entry).collect();
        let provider = if request.cited_paragraph.is_none() {
            let citing_in_corpus = graph.query_row(&format!("SELECT COUNT(DISTINCT case_id) FROM provider_edge
                WHERE direction = 'cited' AND citation_key IN ({marks})"), params_from_iter(keys.iter()), |row| row.get(0))?;
            let citing_reported = graph.prepare(&format!("SELECT DISTINCT provider_edge.citation FROM provider_edge
                JOIN case_key ON case_key.case_id = provider_edge.case_id
                WHERE provider_edge.direction = 'citing' AND case_key.citation_key IN ({marks})
                ORDER BY provider_edge.citation LIMIT 50"))?
                .query_map(params_from_iter(keys.iter()), |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
            Some(ProviderCiting { citing_in_corpus, citing_reported })
        } else { None };
        Ok(NoteUp { total, entries, provider, total_citers, judicial_discussion: judicial, journal_analysis,
            excerpts_considered, excerpts_rejected: rejected })
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityMetric {
    pub citing_cases: i64,
    pub distinct_citing_paragraphs: i64,
    pub occurrences: i64,
}

/// How much each citation is cited, by its key; None where no graph is built.
pub fn authority_metrics(store: &Path, citations: &[String]) -> GraphResult<Option<Vec<Option<AuthorityMetric>>>> {
    with_graph(&graph_path(store), |graph| {
        let mut metric = graph.prepare_cached("SELECT citing_cases, citing_paragraphs, occurrences FROM authority_metric
            WHERE cited_key = ?")?;
        citations.iter().map(|citation| {
            let Some(key) = key_of(citation) else { return Ok(None) };
            Ok(metric.query_map([key], |row| Ok(AuthorityMetric {
                citing_cases: row.get(0)?, distinct_citing_paragraphs: row.get(1)?, occurrences: row.get(2)?,
            }))?.next().transpose()?)
        }).collect()
    })
}
