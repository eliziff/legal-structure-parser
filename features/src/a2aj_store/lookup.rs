//! Finding a document in Beaver's local A2AJ store (a2aj.sqlite): by its citation's key, and a decision a reporter
//! citation names where the store does not index the reporter, by its style of cause. A2AJ gives reporter citations
//! for the Supreme Court's decisions alone, so an OR or a DLR citation is found by its name, year and court, and is
//! then linked by the decision's neutral citation. Only a name the store keys exactly, in one decision of that court
//! and year, finds it. The engine (a2aj.rs) and the store's census bins read the store through these.

use crate::a2aj::{Columns, Document, Language, Source, StoreRow, CITATIONS};
use legal_structure::{
    citation_lookup_key, javascript_whitespace, reporter_decision, trim_javascript_whitespace as js_trim, ReporterDecision,
    JS_WHITESPACE_CLASS,
};
use regex::Regex;
use crate::sqlite::sql;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::sync::LazyLock;


/// The fields of a document of the store that are text.
pub fn fields(document: &Document<StoreRow>, columns: &[&str]) -> Result<Vec<String>, String> {
    let mut values = Vec::new();
    for column in columns {
        values.extend(document.field(column).map_err(sql)?);
    }
    Ok(values)
}

/// The documents a query selects (`from`: its FROM and WHERE, the documents' table `document`), each its id and `columns`.
fn document_rows(connection: &Connection, from: &str, columns: &[&'static str], values: impl rusqlite::Params)
    -> Result<Vec<(i64, StoreRow)>, String> {
    let selected = columns.iter().map(|column| format!("document.{column}")).collect::<Vec<_>>().join(", ");
    let mut statement = connection.prepare_cached(&format!("SELECT document.id, {selected} {from}")).map_err(sql)?;
    statement.query_map(values, |row| Ok((row.get::<_, i64>(0)?, StoreRow::from_query(row, 1, columns)?)))
        .and_then(|rows| rows.collect()).map_err(sql)
}

/// A collection list as a lookup or search names it ("ONCA, onsc"): each named once, at most fifty.
pub fn datasets(value: Option<&str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    value.unwrap_or_default().split(',').map(js_trim).filter(|name| !name.is_empty() && seen.insert(*name)).take(50)
        .map(str::to_owned).collect()
}

/// The one document the store holds under a citation's key, among the collections and at the address asked for.
pub fn find(connection: &Connection, citation: &str, doc_type: Option<&str>, dataset: Option<&str>, source_url: Option<&str>)
    -> Result<Option<i64>, String> {
    const NAMES: [&str; 2] = ["name_en", "name_fr"];
    let citation = js_trim(citation);
    if citation.is_empty() {
        return Err("citation is required".to_owned());
    }
    let key = citation_lookup_key(citation);
    if key.is_empty() {
        return Ok(None);
    }
    let mut filters = vec![
        "document.id IN (SELECT document_id FROM citation_lookup WHERE citation_key = ?)".to_owned(),
        "document.doc_type = ?".to_owned(),
    ];
    let mut values = vec![key, doc_type.unwrap_or("cases").to_owned()];
    let datasets = datasets(dataset);
    if !datasets.is_empty() {
        filters.push(format!("LOWER(document.dataset) IN ({})", vec!["LOWER(?)"; datasets.len()].join(", ")));
        values.extend(datasets.iter().cloned());
    }
    if let Some(url) = source_url.map(js_trim).filter(|url| !url.is_empty()) {
        filters.push("(document.url_en = ? OR document.url_fr = ?)".to_owned());
        values.extend([url.to_owned(), url.to_owned()]);
    }
    let columns: Vec<&'static str> = NAMES.iter().chain(&CITATIONS).copied().collect();
    let from = format!("FROM document WHERE {} ORDER BY document.id LIMIT 16", filters.join(" AND "));
    let rows = document_rows(connection, &from, &columns, rusqlite::params_from_iter(values.iter()))?;
    if rows.len() == 1 {
        return Ok(Some(rows[0].0));
    }
    // A reporter the store does not index: the decision its style of cause names in its court and year.
    if rows.is_empty() && source_url.is_none_or(|url| js_trim(url).is_empty()) && doc_type.is_none_or(|kind| kind == "cases") {
        return find_by_name(connection, citation, &datasets);
    }
    // Several documents under one key ("RSC 1985, c 1" is a statute and its supplements' chapters 1): the one whose
    // own citation the citation is ("RSC 1985, c 1 (2nd Supp)"), else the one whose name it writes ("Customs Act,
    // RSC 1985, c 1").
    let written = folded(citation);
    let only = |chosen: Vec<i64>| (chosen.len() == 1).then(|| chosen[0]);
    let (mut cited, mut named) = (Vec::new(), Vec::new());
    for (id, row) in &rows {
        let document = Document::new(row, Source::Store);
        if fields(&document, &CITATIONS)?.iter().any(|own| folded(own) == written) {
            cited.push(*id);
        }
        if fields(&document, &NAMES)?.iter().any(|name| folded(name).len() > 3 && written.contains(&folded(name))) {
            named.push(*id);
        }
    }
    Ok(only(cited).or_else(|| only(named)))
}

/// Lowercase words, each set apart by one space: a name compared as written in a citation.
pub fn folded(value: &str) -> String {
    let words = value.split(|character: char| !character.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_lowercase);
    format!(" {} ", words.collect::<Vec<_>>().join(" "))
}

static INITIALS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"((?-u:\w))\.((?-u:\w))\.?").expect("initials"));
static VERSUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"{JS_WHITESPACE_CLASS}+[vV]\.?{JS_WHITESPACE_CLASS}+")).expect("versus"));
static JOINERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[-\u{2010}-\u{2015}/]+").expect("joiners"));
static MARKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"[^\p{{L}}\p{{N}}_{JS_WHITESPACE_CLASS}]")).expect("marks"));

/// A style of cause compared as its words, as the installer keys `name_lookup` (a2ajInstall.ts a2ajNameKey) and as
/// CanLII's titles are compared (the Python app's canlii_search.normalize_title): initials closed up ("R.B.C." as
/// "RBC"), "v." as "v", dashes and slashes as spaces, other marks dropped, lowercase.
pub fn name_key(value: &str) -> String {
    let value = INITIALS.replace_all(value, "${1}${2}");
    let value = VERSUS.replace_all(&value, " v ");
    let value = JOINERS.replace_all(&value, " ");
    let value = MARKS.replace_all(&value, "");
    value.split(javascript_whitespace).filter(|word| !word.is_empty()).collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Whether the store indexes the reporter of the citation `key` names (`3:reporter:scr:scr:…`): a citation of a
/// reporter it indexes that it does not find is not found by name instead.
pub fn reporter_indexed(connection: &Connection, key: &str) -> Result<bool, String> {
    let parts = key.split(':').collect::<Vec<_>>();
    if parts.len() < 5 || parts[1] != "reporter" {
        return Ok(false);
    }
    let prefix = format!("{}:", parts[..4].join(":"));
    connection.prepare_cached("SELECT 1 FROM citation_lookup WHERE citation_key >= ? AND citation_key < ? LIMIT 1")
        .and_then(|mut statement| statement.query_row([&prefix, &format!("{prefix}\u{10ffff}")], |_| Ok(())).optional())
        .map(|found| found.is_some())
        .map_err(sql)
}

/// A decision asked for by its style of cause, its year and the courts that may have decided it (any where none).
#[derive(Deserialize)]
#[cfg_attr(feature = "binding-types", derive(ts_rs::TS))]
pub struct Named {
    pub name: String,
    pub year: String,
    #[serde(default)]
    #[cfg_attr(feature = "binding-types", ts(as = "Option<Vec<String>>", optional))]
    pub courts: Vec<String>,
}

impl Named {
    /// Whether a decision answers: its style of cause keyed as the name asked for, dated in the year, of one of the
    /// courts.
    pub fn answers<C: Columns + ?Sized>(&self, document: &Document<C>, name: Option<&str>) -> Result<bool, C::Error> {
        let year = js_trim(&self.year);
        if year.len() != 4 || !year.bytes().all(|byte| byte.is_ascii_digit()) || name.is_some_and(|name| name_key(name) != name_key(&self.name)) {
            return Ok(false);
        }
        let court = self.courts.is_empty()
            || document.field("dataset")?.is_some_and(|dataset| self.courts.iter().any(|court| court.eq_ignore_ascii_case(&dataset)));
        Ok(court && document.language_field("document_date", Language::En)?.is_some_and(|date| date.starts_with(year)))
    }
}

/// The decisions the store holds under a style of cause that answer `named`, with their citations.
pub fn named_decisions(connection: &Connection, named: &Named) -> Result<Vec<(i64, StoreRow)>, String> {
    const COLUMNS: [&str; 5] = ["dataset", "document_date_en", "document_date_fr", "citation_en", "citation_fr"];
    let key = name_key(&named.name);
    if key.is_empty() {
        return Ok(Vec::new());
    }
    let rows = document_rows(connection, "FROM name_lookup JOIN document ON document.id = name_lookup.document_id
        WHERE name_lookup.name_key = ? AND document.doc_type = 'cases'", &COLUMNS, [&key])?;
    let mut found = Vec::new();
    for (id, row) in rows {
        if named.answers(&Document::new(&row, Source::Store), None).map_err(sql)? {
            found.push((id, row));
        }
    }
    Ok(found)
}

/// The one decision a reporter citation names by its style, year and court, among `datasets` where any are given,
/// where the store does not index its reporter.
pub fn find_by_name(connection: &Connection, citation: &str, datasets: &[String]) -> Result<Option<i64>, String> {
    let Some(ReporterDecision { name, year, mut courts, .. }) = reporter_decision(citation) else { return Ok(None) };
    if reporter_indexed(connection, &citation_lookup_key(citation))? {
        return Ok(None);
    }
    if !datasets.is_empty() {
        courts.retain(|court| datasets.iter().any(|dataset| dataset.eq_ignore_ascii_case(court)));
    }
    // A citation of no court asked for names none.
    if courts.is_empty() {
        return Ok(None);
    }
    let found = named_decisions(connection, &Named { name, year: year.to_string(), courts })?;
    Ok((found.len() == 1).then(|| found[0].0))
}
