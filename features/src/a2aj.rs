//! A document of A2AJ's: a row of Beaver's local A2AJ store (a2aj.sqlite), or a record A2AJ's API answers with, which
//! holds the same columns. Every reader of one reads it here: the engine's lookups, searches and readings, the
//! citator's graph and the corpus census. A source gives its columns as it holds them ([`Columns`]); the two sources
//! differ only in the few values they have always given differently, which [`Source`] names.

use legal_structure::{canlii_case_url, canlii_law_url, citation_lookup_key, trim_javascript_whitespace as js_trim, windows_1252_punctuation,
    ProviderTextInput, ProviderTextSourceKind};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::convert::Infallible;
use std::sync::LazyLock;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    En,
    Fr,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Fr => "fr",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Language::En => Language::Fr,
            Language::Fr => Language::En,
        }
    }
}

/// JavaScript's String.prototype.trim, then each run of ASCII blanks one space.
pub fn normalize_whitespace(value: &str) -> String {
    static BLANKS: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[ \t\r\n\x0C\x0B]+").expect("blanks"));
    BLANKS.replace_all(js_trim(value), " ").into_owned()
}

/// Where a document was read from. The store gives a field as it holds it, its date's day, and its own address as its
/// page and its publisher's; A2AJ's API gives a field with the Windows-1252 character each C1 control in it stands for,
/// its date whole, its provisions as an object or as their JSON, and as its page CanLII's page of a statute, else its
/// publisher's page, else CanLII's page of a decision, each address as a URL writes it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Store,
    Api,
}

/// A document's columns as its source holds them.
pub trait Columns {
    type Error;

    /// A column's text; None where it holds none.
    fn text(&self, column: &str) -> Result<Option<Cow<'_, str>>, Self::Error>;

    /// A column's value: a store's columns are text, an API record's JSON.
    fn value(&self, column: &str) -> Result<Option<Cow<'_, Value>>, Self::Error> {
        Ok(self.text(column)?.map(|text| Cow::Owned(Value::String(text.into_owned()))))
    }

    /// Whether a column holds text other than white space.
    fn holds(&self, column: &str) -> Result<bool, Self::Error> {
        Ok(self.text(column)?.is_some_and(|text| !js_trim(&text).is_empty()))
    }
}

/// An API record.
impl Columns for Map<String, Value> {
    type Error = Infallible;

    fn text(&self, column: &str) -> Result<Option<Cow<'_, str>>, Infallible> {
        Ok(match self.get(column) {
            Some(Value::String(text)) => Some(Cow::Borrowed(text)),
            _ => None,
        })
    }

    fn value(&self, column: &str) -> Result<Option<Cow<'_, Value>>, Infallible> {
        Ok(self.get(column).map(Cow::Borrowed))
    }
}

/// An address where it is a web page's, as JavaScript's URL writes it; None for any other, or one carrying credentials.
fn web_url(value: Option<String>) -> Option<String> {
    let url = url::Url::parse(&value?).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.username().is_empty() && url.password().is_none()).then(|| url.into())
}

/// Provisions in the order JavaScript gives an object's keys: whole-number keys first, in their order, then the others
/// as written. None where none is text.
fn ordered(object: &Map<String, Value>) -> Option<Vec<(String, String)>> {
    let index = |key: &str| (key == "0" || (!key.starts_with('0') && key.bytes().all(|byte| byte.is_ascii_digit())))
        .then(|| key.parse::<u64>().ok()).flatten().filter(|value| *value < u32::MAX as u64);
    let mut entries: Vec<(String, String)> = object.iter()
        .filter_map(|(key, value)| match value { Value::String(text) => Some((key.clone(), text.clone())), _ => None })
        .collect();
    entries.sort_by_key(|(key, _)| index(key).map_or((1, 0), |value| (0, value)));
    (!entries.is_empty()).then_some(entries)
}

fn parsed(text: &str) -> Option<Vec<(String, String)>> {
    serde_json::from_str::<Value>(text).ok().as_ref().and_then(Value::as_object).and_then(ordered)
}

/// `cases` or `laws`: a document's kind, where its column or a lookup names one.
pub fn doc_type(value: &str) -> &'static str {
    if value == "laws" { "laws" } else { "cases" }
}

/// A document of A2AJ's as its source gives it.
pub struct Document<'a, C: ?Sized> {
    columns: &'a C,
    source: Source,
    written: bool,
}

impl<'a, C: Columns + ?Sized> Document<'a, C> {
    pub fn new(columns: &'a C, source: Source) -> Self {
        Document { columns, source, written: false }
    }

    /// The document with each field as its source writes it, the white space around it kept: as the citator reads a
    /// decision, whose offsets are into the store's text.
    pub fn as_written(self) -> Self {
        Document { written: true, ..self }
    }

    /// A column's text, trimmed (unless read as written); None where it is not text or only white space.
    pub fn field(&self, column: &str) -> Result<Option<String>, C::Error> {
        let Some(text) = self.columns.text(column)? else { return Ok(None) };
        let trimmed = js_trim(&text);
        if trimmed.is_empty() {
            return Ok(None);
        }
        let value = if self.written { &text[..] } else { trimmed };
        Ok(Some(match self.source {
            Source::Store => value.to_owned(),
            Source::Api => windows_1252_punctuation(value).into_owned(),
        }))
    }

    /// A field in a language, else in the other, else unsuffixed.
    pub fn language_field(&self, field: &str, language: Language) -> Result<Option<String>, C::Error> {
        for column in [format!("{field}_{}", language.code()), format!("{field}_{}", language.other().code()), field.to_owned()] {
            if let Some(value) = self.field(&column)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    pub fn citation(&self, language: Language) -> Result<Option<String>, C::Error> {
        match self.language_field("citation", language)? {
            Some(citation) => Ok(Some(citation)),
            None => self.language_field("citation2", language),
        }
    }

    pub fn date(&self, language: Language) -> Result<Option<String>, C::Error> {
        let date = self.language_field("document_date", language)?;
        Ok(match self.source {
            // The day: the first ten UTF-16 units, as the page reads a date; the store's dates are ASCII.
            Source::Store => date.map(|date| date.chars().take(10).collect()),
            Source::Api => date,
        })
    }

    /// The publisher's page: the record's source address, else its address.
    pub fn publisher_url(&self, language: Language) -> Result<Option<String>, C::Error> {
        Ok(match self.source {
            Source::Store => self.language_field("url", language)?,
            Source::Api => match web_url(self.language_field("source_url", language)?) {
                Some(url) => Some(url),
                None => web_url(self.language_field("url", language)?),
            },
        })
    }

    /// The document's public page. A2AJ's API gives CanLII's for legislation (its publishers' own links are feeds),
    /// else the publisher's, else the page CanLII publishes for a decision's citation.
    pub fn url(&self, language: Language) -> Result<Option<String>, C::Error> {
        if self.source == Source::Store {
            return self.language_field("url", language);
        }
        let citation = self.language_field("citation", language)?;
        if let Some(url) = canlii_law_url(citation.as_deref().unwrap_or(""), language.code(), true) {
            return Ok(Some(url));
        }
        if let Some(url) = self.publisher_url(language)? {
            return Ok(Some(url));
        }
        let citations = [citation, self.language_field("citation2", language)?].into_iter().flatten().collect::<Vec<_>>();
        Ok(canlii_case_url(&citations, language.code(), Some(&self.field("dataset")?.unwrap_or_default())))
    }

    /// The document's provisions in a language, else in the other.
    pub fn provisions(&self, language: Language) -> Result<Option<Vec<(String, String)>>, C::Error> {
        if self.source == Source::Store {
            return Ok(self.language_field("unofficial_sections", language)?.as_deref().and_then(parsed));
        }
        for language in [language, language.other()] {
            let Some(value) = self.columns.value(&format!("unofficial_sections_{}", language.code()))? else { continue };
            return Ok(match value.as_ref() {
                Value::Null => continue,
                Value::String(text) => parsed(text),
                Value::Object(object) => ordered(object),
                _ => None,
            });
        }
        Ok(None)
    }

    /// The language a document is read in: the one asked for where it has text in it, else the other.
    pub fn text_language(&self, language: Language) -> Result<Language, C::Error> {
        Ok(if self.columns.holds(&format!("unofficial_text_{}", language.code()))? { language } else { language.other() })
    }

    /// The document's metadata in a language, where it has a citation in it.
    pub fn metadata(&self, language: Language, doc_type: &'static str) -> Result<Option<Metadata>, C::Error> {
        let Some(citation) = self.citation(language)? else { return Ok(None) };
        Ok(Some(Metadata {
            doc_type,
            dataset: self.field("dataset")?.unwrap_or_default(),
            citation,
            alternate_citation: self.language_field("citation2", language)?,
            name: self.language_field("name", language)?,
            date: self.date(language)?,
            url: self.url(language)?,
            publisher_url: self.publisher_url(language)?,
            language,
            upstream_license: self.field("upstream_license")?,
        }))
    }

    /// What the engine reads of the document: its metadata, text and provisions, in the language asked for where it has
    /// text in it, else in the other; None without a text or a citation.
    pub fn read(&self, language: Language, doc_type: &'static str) -> Result<Option<Read>, C::Error> {
        let (language, text) = match self.field(&format!("unofficial_text_{}", language.code()))? {
            Some(text) => (language, Some(text)),
            None => (language.other(), self.language_field("unofficial_text", language.other())?),
        };
        let Some(text) = text else { return Ok(None) };
        let Some(metadata) = self.metadata(language, doc_type)? else { return Ok(None) };
        Ok(Some(Read { provisions: self.provisions(language)?, metadata, text }))
    }

    /// The document as a search lists it; None for one without a citation. The store's search reads no text.
    pub fn listing(&self, language: Language) -> Result<Option<Listing>, C::Error> {
        let Some(citation) = self.citation(language)? else { return Ok(None) };
        let snippet = match self.source {
            Source::Store => None,
            Source::Api => match self.language_field("snippet", language)? {
                Some(snippet) => Some(snippet),
                None => match self.language_field("highlight", language)? {
                    Some(highlight) => Some(highlight),
                    None => self.language_field("unofficial_text", language)?,
                },
            },
        };
        Ok(Some(Listing {
            dataset: self.field("dataset")?.unwrap_or_default(),
            citation,
            alternate_citation: self.language_field("citation2", language)?,
            name: self.language_field("name", language)?,
            date: self.date(language)?,
            url: self.url(language)?,
            snippet,
        }))
    }

    /// Whether the document answers a query: of the collection asked for, at the address asked for, with one of its
    /// citations having one of the query's keys or, with none, written as its citation.
    pub fn answers(&self, query: &Answering) -> Result<bool, C::Error> {
        if let Some(dataset) = &query.dataset {
            if !self.field("dataset")?.is_some_and(|own| own.to_lowercase() == *dataset) {
                return Ok(false);
            }
        }
        if let Some(address) = query.address.as_deref() {
            let mut found = false;
            for language in [Language::En, Language::Fr] {
                found = found || self.url(language)?.as_deref() == Some(address);
            }
            for language in [Language::En, Language::Fr] {
                found = found || self.publisher_url(language)?.as_deref() == Some(address);
            }
            if !found {
                return Ok(false);
            }
        }
        for column in ["citation_en", "citation2_en", "citation_fr", "citation2_fr"] {
            if self.field(column)?.is_some_and(|own| query.keys.contains(&citation_lookup_key(&own))
                || query.title.as_ref().is_some_and(|title| normalize_whitespace(&own).to_lowercase() == *title)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// What a document must answer to: a citation (by its key and the keys of its other forms), a collection, an address.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Query {
    pub citation: String,
    pub dataset: Option<String>,
    pub source_url: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// A query as documents are compared with it.
pub struct Answering {
    keys: BTreeSet<String>,
    title: Option<String>,
    dataset: Option<String>,
    address: Option<String>,
}

impl Answering {
    /// None where nothing could answer the query: it has no key, and no address with which its citation as written
    /// would do.
    pub fn new(query: &Query) -> Option<Self> {
        let keys = std::iter::once(citation_lookup_key(&query.citation)).chain(query.aliases.iter().cloned())
            .filter(|key| !key.is_empty()).collect::<BTreeSet<_>>();
        let address = query.source_url.as_deref().map(js_trim).filter(|value| !value.is_empty()).map(str::to_owned);
        let title = (keys.is_empty() && address.is_some()).then(|| normalize_whitespace(&query.citation).to_lowercase());
        if keys.is_empty() && title.is_none() {
            return None;
        }
        let dataset = query.dataset.as_deref().map(js_trim).filter(|value| !value.is_empty()).map(str::to_lowercase);
        Some(Answering { keys, title, dataset, address })
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub doc_type: &'static str,
    pub dataset: String,
    pub citation: String,
    pub alternate_citation: Option<String>,
    pub name: Option<String>,
    pub date: Option<String>,
    pub url: Option<String>,
    pub publisher_url: Option<String>,
    pub language: Language,
    pub upstream_license: Option<String>,
}

/// A document as the engine reads it.
#[derive(Serialize)]
pub struct Read {
    pub metadata: Metadata,
    pub text: String,
    pub provisions: Option<Vec<(String, String)>>,
}

impl Read {
    /// What the engine derives the document from: its provisions (else its text), and its full text where that is
    /// another. The provisions make the structured read; searches read the provider's own full text.
    pub fn inputs(self) -> (ProviderTextInput, Option<ProviderTextInput>) {
        let Read { metadata, text, provisions } = self;
        match provisions {
            Some(provisions) => (input(&metadata, String::new(), Some(provisions), None, None),
                Some(input(&metadata, text, None, None, None))),
            None => (input(&metadata, text, None, None, None), None),
        }
    }
}

/// What a provision's excerpt carries of the statute it comes from.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Excerpt {
    pub citation: String,
    pub url: Option<String>,
    pub alternate_citation: Option<String>,
    pub dataset: Option<String>,
    pub name: Option<String>,
}

/// What the engine derives one provision of a statute from, its text read as an excerpt of the statute.
pub fn provision_input(excerpt: Excerpt, language: Language, section: &str, text: String) -> ProviderTextInput {
    let metadata = Metadata { doc_type: "laws", dataset: excerpt.dataset.unwrap_or_default(), citation: excerpt.citation.clone(),
        alternate_citation: excerpt.alternate_citation, name: excerpt.name, date: None, url: excerpt.url, publisher_url: None,
        language, upstream_license: None };
    input(&metadata, text.clone(), Some(vec![(section.to_owned(), text)]), Some(excerpt.citation.clone()), Some(excerpt.citation))
}

/// The engine's reading of a provider text, as the page derives an A2AJ document.
fn input(metadata: &Metadata, text: String, provisions: Option<Vec<(String, String)>>, id: Option<String>,
    excerpt_of: Option<String>) -> ProviderTextInput {
    let present = |value: &Option<String>| value.clone().filter(|value| !value.is_empty());
    ProviderTextInput {
        provider: Some("a2aj".to_owned()),
        citation: metadata.citation.clone(),
        source_kind: if metadata.doc_type == "laws" { ProviderTextSourceKind::Laws } else { ProviderTextSourceKind::Cases },
        text,
        id,
        url: present(&metadata.url),
        dataset: present(&Some(metadata.dataset.clone())),
        name: present(&metadata.name),
        alternate_citation: present(&metadata.alternate_citation),
        section_map: provisions,
        excerpt_of,
        require_report_start: metadata.doc_type == "cases" && metadata.dataset.to_uppercase() == "SCC",
    }
}

/// A document as a search lists it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub dataset: String,
    pub citation: String,
    pub alternate_citation: Option<String>,
    pub name: Option<String>,
    pub date: Option<String>,
    pub url: Option<String>,
    pub snippet: Option<String>,
}

/// A name's words as the store's full-text index reads them (unicode61, diacritics removed): its letters and digits,
/// lowercase.
pub fn name_words(text: &str) -> Vec<String> {
    use unicode_normalization::UnicodeNormalization;
    static MARK: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"\p{M}").expect("marks"));
    static NOT_WORD: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[^\p{L}\p{N}]+").expect("words"));
    let decomposed: String = text.nfd().collect();
    NOT_WORD.split(&MARK.replace_all(&decomposed, "").to_lowercase()).filter(|word| !word.is_empty()).map(str::to_owned).collect()
}

#[cfg(feature = "a2aj-store")]
pub use store::{LazyRow, StoreRow};

#[cfg(feature = "a2aj-store")]
mod store {
    use super::Columns;
    use legal_structure::trim_javascript_whitespace as js_trim;
    use rusqlite::{types::ValueRef, Connection, OptionalExtension};
    use std::borrow::Cow;
    use std::collections::HashMap;

    /// The store's text columns.
    const COLUMNS: [&str; 21] = ["doc_type", "dataset", "citation_en", "citation_fr", "citation2_en", "citation2_fr",
        "name_en", "name_fr", "document_date_en", "document_date_fr", "url_en", "url_fr", "unofficial_text_en",
        "unofficial_text_fr", "unofficial_sections_en", "unofficial_sections_fr", "cases_cited_en", "cases_cited_fr",
        "cases_citing_en", "cases_citing_fr", "upstream_license"];

    /// A GLOB that a text matches when it holds a character other than JavaScript's white space.
    const NOT_BLANK: &str = "*[^\t\n\u{b}\u{c}\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]*";

    fn cell(value: ValueRef) -> Option<String> {
        match value {
            ValueRef::Text(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        }
    }

    /// A row of the store's documents as a query selected it.
    pub struct StoreRow {
        columns: HashMap<&'static str, Option<String>>,
    }

    impl StoreRow {
        /// `columns`, in their order, from the query's column `from`.
        pub fn from_query(row: &rusqlite::Row, from: usize, columns: &[&'static str]) -> rusqlite::Result<Self> {
            Ok(StoreRow { columns: columns.iter().enumerate()
                .map(|(at, column)| Ok((*column, cell(row.get_ref(from + at)?))))
                .collect::<rusqlite::Result<_>>()? })
        }
    }

    impl Columns for StoreRow {
        type Error = rusqlite::Error;

        fn text(&self, column: &str) -> rusqlite::Result<Option<Cow<'_, str>>> {
            Ok(self.columns.get(column).and_then(|value| value.as_deref()).map(Cow::Borrowed))
        }
    }

    /// A document read by its id: the columns read with it, and the others when asked for (a decision's text runs
    /// over many pages).
    pub struct LazyRow<'a> {
        connection: &'a Connection,
        id: i64,
        row: StoreRow,
    }

    impl<'a> LazyRow<'a> {
        /// None where the store holds no document with the id.
        pub fn read(connection: &'a Connection, id: i64, columns: &[&'static str]) -> rusqlite::Result<Option<Self>> {
            let mut statement = connection.prepare_cached(&format!("SELECT {} FROM document WHERE id = ?", columns.join(", ")))?;
            let row = statement.query_row([id], |row| StoreRow::from_query(row, 0, columns)).optional()?;
            Ok(row.map(|row| LazyRow { connection, id, row }))
        }

        fn lazy(&self, column: &str) -> bool {
            !self.row.columns.contains_key(column) && COLUMNS.contains(&column)
        }
    }

    impl Columns for LazyRow<'_> {
        type Error = rusqlite::Error;

        fn text(&self, column: &str) -> rusqlite::Result<Option<Cow<'_, str>>> {
            if !self.lazy(column) {
                return self.row.text(column);
            }
            let mut statement = self.connection.prepare_cached(&format!("SELECT {column} FROM document WHERE id = ?"))?;
            Ok(statement.query_row([self.id], |row| Ok(cell(row.get_ref(0)?)))?.map(Cow::Owned))
        }

        /// Read without reading all of a long text.
        fn holds(&self, column: &str) -> rusqlite::Result<bool> {
            if !self.lazy(column) {
                return Ok(self.row.text(column)?.is_some_and(|text| !js_trim(&text).is_empty()));
            }
            let mut statement = self.connection.prepare_cached(&format!(
                "SELECT typeof({column}) = 'text' AND {column} GLOB ? FROM document WHERE id = ?"))?;
            statement.query_row(rusqlite::params![NOT_BLANK, self.id], |row| row.get::<_, bool>(0))
        }
    }
}