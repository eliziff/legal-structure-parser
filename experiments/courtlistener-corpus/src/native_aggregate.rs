//! Full original COPY CSV parser observations; retain complete originals only on failure.
use crate::{compile, hash, metadata, Result, RENDITIONS};
use legal_structure::{Derivation, DocumentQuery, DocumentStructure};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    time::{Duration, Instant},
};

const WORKERS: usize = 3;
const IN_FLIGHT_ROWS: usize = WORKERS * 2;
const IN_FLIGHT_BYTES: usize = 64 * 1024 * 1024;
const COUNT_KEYS: usize = 256;
const COUNT_KEY_BYTES: usize = 128;
const ERROR_SAMPLES: usize = 32;
const RECEIPT_RESERVE: u64 = 1024 * 1024;
const COLUMNS: [&str; 22] = [
    "id",
    "date_created",
    "date_modified",
    "author_str",
    "per_curiam",
    "joined_by_str",
    "type",
    "sha1",
    "page_count",
    "download_url",
    "local_path",
    "plain_text",
    "html",
    "html_lawbox",
    "html_columbia",
    "html_anon_2020",
    "xml_harvard",
    "xml_scan",
    "html_with_citations",
    "extracted_by_ocr",
    "author_id",
    "cluster_id",
];

struct DecodedRead<R> {
    input: R,
    digest: Sha256,
    bytes: u64,
}
impl<R: Read> Read for DecodedRead<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(buffer)?;
        self.bytes = self
            .bytes
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("decoded byte counter overflow"))?;
        self.digest.update(&buffer[..count]);
        Ok(count)
    }
}

struct DigestWriter<'a>(&'a mut Sha256);
impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn digest_value(value: &impl Serialize) -> std::result::Result<String, String> {
    let mut digest = Sha256::new();
    {
        let mut writer = BufWriter::with_capacity(8 * 1024, DigestWriter(&mut digest));
        serde_json::to_writer(&mut writer, value).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn append(digest: &mut Sha256, value: &impl Serialize) -> Result<()> {
    {
        let mut writer = BufWriter::with_capacity(8 * 1024, DigestWriter(&mut *digest));
        serde_json::to_writer(&mut writer, value)?;
        writer.flush()?;
    }
    digest.update(b"\n");
    Ok(())
}
fn current(digest: &Sha256) -> String {
    format!("{:x}", digest.clone().finalize())
}
fn short(value: &str) -> String {
    value.chars().take(512).collect()
}

#[derive(Default, Serialize)]
struct Counts {
    values: BTreeMap<String, u64>,
    omitted_observation_events: u64,
}
impl Counts {
    fn add(&mut self, key: &str, amount: u64) {
        if let Some(value) = self.values.get_mut(key) {
            *value += amount;
        } else if key.len() <= COUNT_KEY_BYTES && self.values.len() < COUNT_KEYS {
            self.values.insert(key.to_owned(), amount);
        } else {
            self.omitted_observation_events += amount;
        }
    }
    fn merge(&mut self, other: &Self) {
        for (key, value) in &other.values {
            self.add(key, *value);
        }
        self.omitted_observation_events += other.omitted_observation_events;
    }
}

#[derive(Default, Serialize)]
struct Observations {
    nodes_by_derivation: BTreeMap<&'static str, Counts>,
    native_roles_by_source_tag: Counts,
    source_tags: Counts,
    source_attribute_names: Counts,
    diagnostic_codes: Counts,
    diagnostic_severities: Counts,
    relationships: Counts,
    source_roles: Counts,
}
impl Observations {
    fn merge(&mut self, other: &Self) {
        for (source, counts) in &other.nodes_by_derivation {
            self.nodes_by_derivation
                .entry(*source)
                .or_default()
                .merge(counts);
        }
        self.native_roles_by_source_tag
            .merge(&other.native_roles_by_source_tag);
        self.source_tags.merge(&other.source_tags);
        self.source_attribute_names
            .merge(&other.source_attribute_names);
        self.diagnostic_codes.merge(&other.diagnostic_codes);
        self.diagnostic_severities
            .merge(&other.diagnostic_severities);
        self.relationships.merge(&other.relationships);
        self.source_roles.merge(&other.source_roles);
    }
}

fn observe(doc: &DocumentStructure, query: &DocumentQuery) -> Observations {
    let mut result = Observations::default();
    for node in &doc.nodes {
        let derivation = match node.source {
            Derivation::Native => "native",
            Derivation::Heuristic => "heuristic",
            Derivation::Model => "model",
        };
        result
            .nodes_by_derivation
            .entry(derivation)
            .or_default()
            .add(node.kind.name(), 1);
        if node.source == Derivation::Native {
            result.native_roles_by_source_tag.add(
                &format!(
                    "{}:{}",
                    node.markup_tag.as_deref().unwrap_or("(no_markup_tag)"),
                    node.kind.name()
                ),
                1,
            );
        }
        if node.anchor.is_some() {
            result
                .relationships
                .add("native_or_inferred_node_anchor_present", 1);
        }
        if node.parent_id.is_some() {
            result.relationships.add("node_parent_present", 1);
        }
    }
    let mut ids = HashMap::<&str, u64>::new();
    let mut names = HashMap::<&str, u64>::new();
    for element in &doc.markup_elements {
        result.source_tags.add(&element.tag, 1);
        result.relationships.add("source_elements", 1);
        if element.parent.is_some() {
            result.relationships.add("source_parent_present", 1);
        }
        for (name, value) in &element.attributes {
            result.source_attribute_names.add(name, 1);
            result.relationships.add("source_attributes", 1);
            if value.is_empty() {
                result.relationships.add("empty_source_attribute_values", 1);
            }
        }
        if let Some(id) = element.attributes.get("id") {
            result.relationships.add("source_id_attributes", 1);
            if id.is_empty() {
                result.relationships.add("empty_source_ids", 1);
            } else {
                *ids.entry(id.as_str()).or_default() += 1;
            }
        }
        if element.tag.eq_ignore_ascii_case("a") {
            if let Some(name) = element.attributes.get("name") {
                result.relationships.add("source_A_name_attributes", 1);
                if name.is_empty() {
                    result.relationships.add("empty_source_A_names", 1);
                } else {
                    *names.entry(name.as_str()).or_default() += 1;
                }
            }
        }
        if let Some(href) = element.attributes.get("href") {
            result.relationships.add("source_href_attributes", 1);
            result.relationships.add(
                if href.is_empty() {
                    "empty_source_hrefs"
                } else if href == "#" {
                    "literal_document_top_hrefs"
                } else if href.starts_with('#') {
                    "literal_local_fragment_hrefs"
                } else {
                    "nonfragment_source_hrefs"
                },
                1,
            );
        }
        if element.attributes.contains_key("src") {
            result.relationships.add("source_src_attributes", 1);
        }
        if [
            "img", "audio", "video", "object", "embed", "iframe", "source", "picture",
        ]
        .iter()
        .any(|tag| element.tag.eq_ignore_ascii_case(tag))
        {
            result.relationships.add("source_media_elements", 1);
        }
    }
    for (kind, values) in [("source_id", ids), ("source_A_name", names)] {
        result.relationships.add(
            &format!("distinct_nonempty_{kind}_values"),
            values.len() as u64,
        );
        for count in values.values().filter(|&&count| count > 1) {
            result
                .relationships
                .add(&format!("duplicate_{kind}_values"), 1);
            result
                .relationships
                .add(&format!("duplicate_{kind}_occurrences"), *count);
        }
    }
    result.source_roles.add("notes", doc.notes.len() as u64);
    result.source_roles.add(
        "note_references",
        doc.notes.iter().map(|n| n.references.len() as u64).sum(),
    );
    let components = query.source_opinion_components(doc);
    result
        .source_roles
        .add("opinions", components.opinions.len() as u64);
    result
        .source_roles
        .add("unowned_authors", components.unowned_authors.len() as u64);
    result.source_roles.add(
        "unowned_type_declarations",
        components.unowned_type_declarations.len() as u64,
    );
    for opinion in &components.opinions {
        result
            .source_roles
            .add("owned_authors", opinion.authors.len() as u64);
        result.source_roles.add(
            "owned_type_declarations",
            opinion.type_declarations.len() as u64,
        );
        if opinion.parent_opinion_id.is_some() {
            result.source_roles.add("nested_source_opinions", 1);
        }
    }
    for diagnostic in &doc.diagnostics {
        result.diagnostic_codes.add(&diagnostic.code, 1);
        result.diagnostic_severities.add(
            &format!("{:?}", diagnostic.severity).to_ascii_lowercase(),
            1,
        );
    }
    result
}

#[derive(Serialize)]
struct SourceCell {
    column_index: usize,
    presence: &'static str,
    utf8_bytes: usize,
    sha256: Option<String>,
}
#[derive(Serialize)]
struct SemanticField {
    field: &'static str,
    presence: &'static str,
    status: &'static str,
    source_utf8_bytes: usize,
    source_sha256: Option<String>,
    document_structure_sha256: Option<String>,
    document_outline_sha256: Option<String>,
    source_opinion_components_sha256: Option<String>,
    error_sha256: Option<String>,
}
#[derive(Serialize)]
struct CompatibleField<'a> {
    field: &'static str,
    presence: &'static str,
    status: &'static str,
    source_utf8_bytes: usize,
    source_sha256: &'a Option<String>,
    document_outline_sha256: &'a Option<String>,
    source_opinion_components_sha256: &'a Option<String>,
    error_sha256: &'a Option<String>,
}
struct FieldResult {
    semantic: SemanticField,
    observations: Observations,
    seconds: f64,
    error: Option<String>,
    failure_stage: Option<&'static str>,
}
struct Job {
    ordinal: u64,
    raw_bytes: usize,
    raw_sha256: String,
    raw: Vec<u8>,
    source_bytes: usize,
    row: Value,
}
struct RowResult {
    job: Job,
    sources: Vec<SourceCell>,
    fields: Vec<FieldResult>,
}

fn field(row: &Value, name: &'static str) -> FieldResult {
    let value = row[name].as_str();
    let presence = match value {
        None => "null",
        Some("") => "empty",
        Some(_) => "nonempty",
    };
    let mut semantic = SemanticField {
        field: name,
        presence,
        status: if presence == "nonempty" {
            "parsed"
        } else {
            presence
        },
        source_utf8_bytes: value.map_or(0, str::len),
        source_sha256: value.map(|v| hash(v.as_bytes())),
        document_structure_sha256: None,
        document_outline_sha256: None,
        source_opinion_components_sha256: None,
        error_sha256: None,
    };
    let mut observations = Observations::default();
    let mut error = None;
    let mut stage = "compile";
    let then = Instant::now();
    if presence == "nonempty" {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> std::result::Result<_, String> {
                let doc = compile(row, Some(name), None, None).map_err(|e| e.to_string())?;
                let query = DocumentQuery::new();
                stage = "serialize-document";
                let document = digest_value(&doc)?;
                stage = "document-outline";
                let outline = digest_value(&legal_structure::document_outline(&doc, false, None))?;
                stage = "source-opinion-components";
                let opinions = digest_value(&query.source_opinion_components(&doc))?;
                stage = "observe";
                Ok((document, outline, opinions, observe(&doc, &query)))
            },
        ));
        match result {
            Ok(Ok((document, outline, opinions, counts))) => {
                semantic.document_structure_sha256 = Some(document);
                semantic.document_outline_sha256 = Some(outline);
                semantic.source_opinion_components_sha256 = Some(opinions);
                observations = counts;
            }
            Ok(Err(message)) => {
                semantic.status = "error";
                semantic.error_sha256 = Some(hash(message.as_bytes()));
                error = Some(message);
            }
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("non-string parser panic");
                semantic.status = "panic";
                semantic.error_sha256 = Some(hash(message.as_bytes()));
                error = Some(message.to_owned());
            }
        }
    }
    let failure_stage = error.as_ref().map(|_| stage);
    FieldResult {
        semantic,
        observations,
        seconds: then.elapsed().as_secs_f64(),
        error,
        failure_stage,
    }
}

fn worker(input: Receiver<Job>, output: SyncSender<RowResult>) {
    while let Ok(job) = input.recv() {
        let sources = COLUMNS
            .iter()
            .enumerate()
            .map(|(column_index, name)| {
                let value = job.row[*name].as_str();
                SourceCell {
                    column_index,
                    presence: match value {
                        None => "null",
                        Some("") => "empty",
                        Some(_) => "nonempty",
                    },
                    utf8_bytes: value.map_or(0, str::len),
                    sha256: value.map(|v| hash(v.as_bytes())),
                }
            })
            .collect();
        let fields: Vec<FieldResult> = RENDITIONS
            .into_iter()
            .map(|name| field(&job.row, name))
            .collect();
        // Successful source strings/raw CSV need not wait in the ordered output queue.
        // A failed row stays complete until its seekable original and all failures are persisted.
        let failed = fields.iter().any(|field| field.error.is_some());
        let Job {
            ordinal,
            raw_bytes,
            raw_sha256,
            raw,
            source_bytes,
            row: original,
        } = job;
        let row = if failed {
            original
        } else {
            json!({"id":original["id"],"cluster_id":original["cluster_id"]})
        };
        let raw = if failed { raw } else { Vec::new() };
        let job = Job {
            ordinal,
            raw_bytes,
            raw_sha256,
            raw,
            source_bytes,
            row,
        };
        if output
            .send(RowResult {
                job,
                sources,
                fields,
            })
            .is_err()
        {
            break;
        }
    }
}

#[derive(Default, Serialize)]
struct Totals {
    input_fields: u64,
    null: u64,
    empty: u64,
    nonempty: u64,
    attempted: u64,
    parsed: u64,
    errors: u64,
    panics: u64,
    source_utf8_bytes: u64,
    analysis_seconds: f64,
    observations: Observations,
}
#[derive(Serialize)]
struct ColumnStats {
    name: &'static str,
    null_rows: u64,
    empty_string_rows: u64,
    nonempty_string_rows: u64,
    string_utf8_bytes: u64,
    minimum_nonempty_utf8_bytes: Option<usize>,
    maximum_utf8_bytes: usize,
    maximum_source_ordinal_zero_based: Option<u64>,
}
struct Aggregate {
    records_read: u64,
    records_observed: u64,
    source_utf8_bytes: u64,
    input_eof: bool,
    queued_bytes: usize,
    queued_raw_bytes: usize,
    inflight: usize,
    maximum_inflight_bytes: usize,
    maximum_inflight_decoded_bytes: usize,
    maximum_raw_record_bytes: usize,
    semantic: Sha256,
    compatible: Sha256,
    sources: Sha256,
    field_digests: BTreeMap<&'static str, Sha256>,
    totals: BTreeMap<&'static str, Totals>,
    samples: Vec<Value>,
    sample_omissions: u64,
    header: Vec<String>,
    header_sha256: Option<String>,
    raw_header_csv: Vec<u8>,
    columns: Vec<ColumnStats>,
    started: Instant,
}
impl Aggregate {
    fn new() -> Self {
        let mut semantic = Sha256::new();
        semantic.update(b"courtlistener.native-aggregate.semantic.v1\0");
        let mut compatible = Sha256::new();
        compatible.update(b"courtlistener.native-aggregate.compatible.v1\0");
        let mut sources = Sha256::new();
        sources.update(b"courtlistener.native-aggregate.sources.v1\0");
        Self {
            records_read: 0,
            records_observed: 0,
            source_utf8_bytes: 0,
            input_eof: false,
            queued_bytes: 0,
            queued_raw_bytes: 0,
            inflight: 0,
            maximum_inflight_bytes: 0,
            maximum_inflight_decoded_bytes: 0,
            maximum_raw_record_bytes: 0,
            semantic,
            compatible,
            sources,
            field_digests: RENDITIONS
                .into_iter()
                .map(|name| {
                    let mut digest = Sha256::new();
                    digest.update(b"courtlistener.native-aggregate.field.v1\0");
                    digest.update(name.as_bytes());
                    (name, digest)
                })
                .collect(),
            totals: RENDITIONS
                .into_iter()
                .map(|name| (name, Totals::default()))
                .collect(),
            samples: Vec::new(),
            sample_omissions: 0,
            header: Vec::new(),
            header_sha256: None,
            raw_header_csv: Vec::new(),
            columns: COLUMNS
                .into_iter()
                .map(|name| ColumnStats {
                    name,
                    null_rows: 0,
                    empty_string_rows: 0,
                    nonempty_string_rows: 0,
                    string_utf8_bytes: 0,
                    minimum_nonempty_utf8_bytes: None,
                    maximum_utf8_bytes: 0,
                    maximum_source_ordinal_zero_based: None,
                })
                .collect(),
            started: Instant::now(),
        }
    }
    fn reduce(&mut self, result: RowResult) -> Result<()> {
        if result.job.ordinal != self.records_observed {
            return Err("ordered reducer ordinal differs".into());
        }
        for cell in &result.sources {
            let column = &mut self.columns[cell.column_index];
            match cell.presence {
                "null" => column.null_rows += 1,
                "empty" => column.empty_string_rows += 1,
                _ => {
                    column.nonempty_string_rows += 1;
                    column.minimum_nonempty_utf8_bytes = Some(
                        column
                            .minimum_nonempty_utf8_bytes
                            .map_or(cell.utf8_bytes, |n| n.min(cell.utf8_bytes)),
                    );
                }
            }
            column.string_utf8_bytes += cell.utf8_bytes as u64;
            if cell.utf8_bytes > column.maximum_utf8_bytes {
                column.maximum_utf8_bytes = cell.utf8_bytes;
                column.maximum_source_ordinal_zero_based = Some(result.job.ordinal);
            }
        }
        let source = json!({"source_ordinal_zero_based":result.job.ordinal,
            "original_csv_row_bytes":result.job.raw_bytes,"original_csv_row_sha256":result.job.raw_sha256,
            "opinion_id":result.job.row["id"],"cluster_id":result.job.row["cluster_id"],
            "source_fields":result.sources});
        let semantics = result
            .fields
            .iter()
            .map(|f| &f.semantic)
            .collect::<Vec<_>>();
        append(&mut self.sources, &source)?;
        append(&mut self.semantic, &(&source, &semantics))?;
        let compatible = semantics
            .iter()
            .map(|s| CompatibleField {
                field: s.field,
                presence: s.presence,
                status: s.status,
                source_utf8_bytes: s.source_utf8_bytes,
                source_sha256: &s.source_sha256,
                document_outline_sha256: &s.document_outline_sha256,
                source_opinion_components_sha256: &s.source_opinion_components_sha256,
                error_sha256: &s.error_sha256,
            })
            .collect::<Vec<_>>();
        append(&mut self.compatible, &(&source, &compatible))?;
        for result in result.fields {
            let field = result.semantic.field;
            append(
                self.field_digests.get_mut(field).unwrap(),
                &(&source, &result.semantic),
            )?;
            let total = self.totals.get_mut(field).unwrap();
            total.input_fields += 1;
            total.source_utf8_bytes += result.semantic.source_utf8_bytes as u64;
            total.analysis_seconds += result.seconds;
            match result.semantic.presence {
                "null" => total.null += 1,
                "empty" => total.empty += 1,
                _ => {
                    total.nonempty += 1;
                    total.attempted += 1;
                }
            }
            match result.semantic.status {
                "parsed" => total.parsed += 1,
                "error" => total.errors += 1,
                "panic" => total.panics += 1,
                _ => {}
            }
            total.observations.merge(&result.observations);
            if let Some(message) = result.error {
                if self.samples.len() < ERROR_SAMPLES {
                    self.samples.push(json!({"source_ordinal_zero_based":source["source_ordinal_zero_based"],
                        "original_csv_row_sha256":source["original_csv_row_sha256"],"field":field,
                        "source_utf8_bytes":result.semantic.source_utf8_bytes,"source_sha256":result.semantic.source_sha256,
                        "status":result.semantic.status,"error_sha256":result.semantic.error_sha256,"error_excerpt":short(&message)}));
                } else {
                    self.sample_omissions += 1;
                }
            }
        }
        self.inflight -= 1;
        self.queued_bytes -= result.job.source_bytes;
        self.queued_raw_bytes -= result.job.raw_bytes;
        self.source_utf8_bytes += result.job.source_bytes as u64;
        self.records_observed += 1;
        Ok(())
    }
    fn snapshot(
        &self,
        decoded_bytes: u64,
        decoded_digest: &Sha256,
        terminal: bool,
        failure: Option<&str>,
    ) -> Value {
        let nonempty: u64 = self.totals.values().map(|t| t.nonempty).sum();
        let attempted: u64 = self.totals.values().map(|t| t.attempted).sum();
        let parsed: u64 = self.totals.values().map(|t| t.parsed).sum();
        let errors: u64 = self.totals.values().map(|t| t.errors).sum();
        let panics: u64 = self.totals.values().map(|t| t.panics).sum();
        let complete = terminal
            && failure.is_none()
            && self.input_eof
            && self.inflight == 0
            && self.records_read == self.records_observed
            && attempted == nonempty;
        let native_aggregate = json!({
            "attempts_complete":complete,"all_nonempty_parsed":complete && parsed == nonempty && errors == 0 && panics == 0,
            "status":if !complete {"incomplete_prefix"} else if errors+panics != 0 {"complete_with_parser_errors"} else {"complete"},
            "source_eof":self.input_eof,"reduced_records":self.records_observed,
            "field_order":RENDITIONS,"nonempty_fields":nonempty,"attempted_fields":attempted,
            "parsed_fields":parsed,"parser_errors":errors,"parser_panics":panics,
            "ordered_semantic_sha256":if complete {Some(current(&self.semantic))} else {None},
            "ordered_semantic_prefix_sha256":current(&self.semantic),
            "ordered_compatible_semantic_sha256":if complete {Some(current(&self.compatible))} else {None},
            "ordered_compatible_semantic_prefix_sha256":current(&self.compatible),
            "compatible_hash_scope":"Same original header/source-row/all-field proof and eight outcomes as ordered semantic hash, excluding only document_structure_sha256; separate compatible.v1 NUL domain. Other source/status/outline/opinion/error SHA fields remain exact. This projection permits independent reconstruction from existing historical observations, without making full-model equality claims.",
            "ordered_source_prefix_sha256":current(&self.sources),
            "ordered_rendition_prefix_sha256":self.field_digests.iter().map(|(k,v)|(*k,current(v))).collect::<BTreeMap<_,_>>(),
            "hash_scope":"Versioned header binding followed by original-ordinal JSON records and newline delimiters; every original field presence/UTF-8 SHA, raw row SHA/size, original id/cluster_id and all eight ordered outcomes. Successful outcomes include complete serialized DocumentStructure, document_outline(false,None) and source_opinion_components SHA256. Timing/capped counts are outside semantic hash.",
            "workers":WORKERS,"maximum_inflight_rows":IN_FLIGHT_ROWS,"maximum_inflight_source_bytes":IN_FLIGHT_BYTES,
            "actual_maximum_inflight_source_bytes":self.maximum_inflight_bytes,
            "pending_rows":self.inflight,"pending_source_bytes":self.queued_bytes,
            "reduced_all_column_source_utf8_bytes":self.source_utf8_bytes,
            "count_map_maximum_keys":COUNT_KEYS,"count_key_maximum_utf8_bytes":COUNT_KEY_BYTES,
            "error_samples":self.samples,"error_sample_omissions":self.sample_omissions,
            "raw_sources_or_documents_retained":false,"witness_capture_enabled":false,"form_discovery_enabled":false,
            "limits":["Hash and parser coverage never stop at a detail-map/sample cap; omitted observation events do not claim distinct omitted keys",
                "SourceEOF is decoded stdin EOF; complete whole-snapshot proof also requires transport pinned bytes/hash, bzip/curl terminal proof and expected counts",
                "Incomplete or cancelled runs retain only an ordered reduced prefix, including NULL/empty fields; read-ahead bytes and pending rows are not semantic coverage",
                "No other-rendition fallback, joined canonical metadata, CAP citation context or browser navigation; counts/hash observe output identity, not independent source recall",
                "Memory is bounded by admitted rows plus one reader row and one parser document per worker; per-document parser allocations follow the actual source size"]
        });
        let mut native_aggregate = native_aggregate;
        native_aggregate["maximum_inflight_decoded_bytes"] = json!(IN_FLIGHT_BYTES);
        native_aggregate["actual_maximum_inflight_decoded_bytes"] =
            json!(self.maximum_inflight_decoded_bytes);
        native_aggregate["pending_raw_csv_bytes"] = json!(self.queued_raw_bytes);
        json!({"schema":"courtlistener.native-aggregate.v1","table":"opinions",
            "status":if complete {"complete_decoded_input"} else if terminal {"incomplete"} else {"running"},
            "decoded_input_complete":complete,"full_snapshot_verified_by_this_module":false,
            "failure":failure.map(short),"records_read":self.records_read,"records_observed":self.records_observed,
            "source_columns":self.header,"original_header_csv_sha256":self.header_sha256,"column_stats":self.columns,
            "decoded_csv_bytes":decoded_bytes,"decoded_csv_sha256":current(decoded_digest),
            "maximum_raw_record_bytes":self.maximum_raw_record_bytes,"seconds":self.started.elapsed().as_secs_f64(),
            "engine_source_sha256":legal_structure::ENGINE_SOURCE_SHA256,
            "query_source_sha256":hash(include_bytes!("../../../query/src/document_query.rs")),
            "main_source_sha256":hash(include_bytes!("main.rs")),
            "harness_source_sha256":hash(include_bytes!("native_aggregate.rs")),
            "aggregate_source_sha256":hash(include_str!("native_aggregate.rs").as_bytes()),
            "renditions":self.totals,"native_aggregate":native_aggregate,"tests_written_or_run":0})
    }
}

struct Output {
    summary: File,
    progress: File,
    witnesses: File,
    witness_index: File,
    witness_bytes: u64,
    index_bytes: u64,
    failed_rows: u64,
    failed_fields: u64,
    witness_digest: Sha256,
    index_digest: Sha256,
    unretained_failure: Option<Value>,
    maximum: u64,
    cancel: PathBuf,
    last: Instant,
}
impl Output {
    fn new(out: &Path, maximum: u64) -> Result<Self> {
        fs::create_dir_all(out)?;
        let create = |name| -> Result<File> {
            Ok(OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(out.join(name))?)
        };
        Ok(Self {
            summary: create("summary.json")?,
            progress: create("progress.json")?,
            witnesses: create("witnesses.jsonl.zst")?,
            witness_index: create("witness-index.jsonl")?,
            witness_bytes: 0,
            index_bytes: 0,
            failed_rows: 0,
            failed_fields: 0,
            witness_digest: Sha256::new(),
            index_digest: Sha256::new(),
            unretained_failure: None,
            maximum,
            cancel: out.join("cancel.request"),
            last: Instant::now(),
        })
    }
    fn retain_failure(&mut self, state: &Aggregate, result: &RowResult) -> Result<()> {
        let failures: Vec<Value> = result.fields.iter().filter(|field| field.error.is_some())
            .map(|field| json!({"field":field.semantic.field,"status":field.semantic.status,
                "source_utf8_bytes":field.semantic.source_utf8_bytes,"source_sha256":field.semantic.source_sha256,
                "error_sha256":field.semantic.error_sha256,"error":field.error,"stage":field.failure_stage}))
            .collect();
        if failures.is_empty() {
            return Ok(());
        }
        let job = &result.job;
        let retain = (|| -> Result<()> {
            let values: Vec<&Value> = state.header.iter().map(|name| &job.row[name]).collect();
            let string_hashes: BTreeMap<&str, Value> = result
                .sources
                .iter()
                .filter(|cell| cell.presence != "null")
                .map(|cell| {
                    (
                        state.header[cell.column_index].as_str(),
                        json!({"utf8_bytes":cell.utf8_bytes,"sha256":cell.sha256}),
                    )
                })
                .collect();
            let witness = json!({"schema":"courtlistener.record-census-witness.v1","table":"opinions",
                "source_ordinal_zero_based":job.ordinal,"columns":state.header,"values":values,
                "raw_csv":std::str::from_utf8(&job.raw)?,"original_csv_row_bytes":job.raw_bytes,
                "original_csv_row_sha256":job.raw_sha256,"source_string_hashes":string_hashes,
                "original_header_csv":std::str::from_utf8(&state.raw_header_csv)?,
                "original_header_csv_sha256":state.header_sha256});
            let mut encoded = serde_json::to_vec(&witness)?;
            encoded.push(b'\n');
            let frame = zstd::bulk::compress(&encoded, 3)?;
            let mut index = serde_json::to_vec(
                &json!({"schema":"courtlistener.native-aggregate-failure-index.v1",
                "table":"opinions","witness_ordinal":self.failed_rows,"source_ordinal_zero_based":job.ordinal,
                "opinion_id":job.row["id"],"cluster_id":job.row["cluster_id"],
                "original_csv_row_bytes":job.raw_bytes,"original_csv_row_sha256":job.raw_sha256,
                "source_fields":result.sources,"failures":&failures,
                "compressed_offset":self.witness_bytes,"compressed_bytes":frame.len(),"compressed_sha256":hash(&frame),
                "raw_bytes":encoded.len(),"raw_sha256":hash(&encoded)}),
            )?;
            index.push(b'\n');
            let size = self
                .witness_bytes
                .checked_add(self.index_bytes)
                .and_then(|n| n.checked_add(frame.len() as u64))
                .and_then(|n| n.checked_add(index.len() as u64))
                .and_then(|n| n.checked_add(RECEIPT_RESERVE))
                .ok_or("failure evidence byte counter overflow")?;
            if size > self.maximum {
                return Err("output cap cannot retain this complete failure source; capture cannot be omitted".into());
            }
            self.witnesses.write_all(&frame)?;
            self.witness_index.write_all(&index)?;
            self.witnesses.flush()?;
            self.witness_index.flush()?;
            self.witness_digest.update(&frame);
            self.index_digest.update(&index);
            self.witness_bytes += frame.len() as u64;
            self.index_bytes += index.len() as u64;
            self.failed_rows += 1;
            self.failed_fields += failures.len() as u64;
            Ok(())
        })();
        if let Err(error) = retain {
            let excerpts: Vec<Value> = result.fields.iter().filter_map(|field| {
                field.error.as_ref().map(|message| json!({"field":field.semantic.field,
                    "status":field.semantic.status,"stage":field.failure_stage,
                    "source_utf8_bytes":field.semantic.source_utf8_bytes,"source_sha256":field.semantic.source_sha256,
                    "error_sha256":field.semantic.error_sha256,"error_excerpt":short(message)}))
            }).collect();
            self.unretained_failure = Some(json!({"source_ordinal_zero_based":job.ordinal,
                "opinion_id":job.row["id"],"cluster_id":job.row["cluster_id"],
                "original_csv_row_sha256":job.raw_sha256,"failed_fields":excerpts,
                "failure_capture_error":error.to_string()}));
            // A failed write may have left a partial frame/line. The successful prefix stays seekable.
            let _ = self.witnesses.set_len(self.witness_bytes);
            let _ = self.witness_index.set_len(self.index_bytes);
            return Err(format!(
                "failure source could not be retained at ordinal {}: {error}",
                job.ordinal
            )
            .into());
        }
        Ok(())
    }
    fn check_cancel(&self) -> Result<()> {
        if self.cancel.exists() {
            Err("transport cancellation requested; retained hash is an incomplete prefix".into())
        } else {
            Ok(())
        }
    }
    fn write(
        &mut self,
        state: &Aggregate,
        bytes: u64,
        digest: &Sha256,
        terminal: bool,
        failure: Option<&str>,
    ) -> Result<()> {
        let mut snapshot = state.snapshot(bytes, digest, terminal, failure);
        let expected_failures: u64 = state
            .totals
            .values()
            .map(|total| total.errors + total.panics)
            .sum();
        let witness_file_bytes = self.witnesses.metadata()?.len();
        let index_file_bytes = self.witness_index.metadata()?.len();
        let prefix_is_full_file =
            witness_file_bytes == self.witness_bytes && index_file_bytes == self.index_bytes;
        snapshot["native_aggregate"]["raw_sources_or_documents_retained"] =
            json!(self.failed_rows != 0);
        snapshot["native_aggregate"]["witness_capture_enabled"] = json!(true);
        snapshot["native_aggregate"]["failure_evidence"] = json!({
            "schema":"courtlistener.native-aggregate-failure-evidence.v1",
            "witness_file":"witnesses.jsonl.zst","index_file":"witness-index.jsonl",
            "retained_failed_rows":self.failed_rows,"retained_failed_fields":self.failed_fields,
            "compressed_bytes":self.witness_bytes,"index_bytes":self.index_bytes,
            "compressed_sha256":prefix_is_full_file.then(||current(&self.witness_digest)),
            "index_sha256":prefix_is_full_file.then(||current(&self.index_digest)),
            "successful_prefix_compressed_sha256":current(&self.witness_digest),
            "successful_prefix_index_sha256":current(&self.index_digest),
            "witness_file_bytes":witness_file_bytes,"index_file_bytes":index_file_bytes,
            "successful_prefix_is_full_file":prefix_is_full_file,
            "complete_for_reduced_prefix":self.unretained_failure.is_none()&&prefix_is_full_file&&self.failed_fields==expected_failures,
            "first_unretained_failure":self.unretained_failure,
            "scope":"Every reduced failed source row once, including all22 ordered original values, literal raw CSV/header and all field failures. Display sample omissions never omit durable failure evidence."});
        // Keep both receipts within their reserved space before any failure arrives.
        // The complete failed-original allocation may then grow without transiently exceeding the cap.
        let available = self
            .maximum
            .saturating_sub(witness_file_bytes)
            .saturating_sub(index_file_bytes)
            .min(RECEIPT_RESERVE);
        let mut encoded = serde_json::to_vec(&snapshot)?;
        encoded.push(b'\n');
        if (encoded.len() as u64).saturating_mul(2) > available {
            snapshot["native_aggregate"]["detail_maps_omitted_output_cap"] = json!(true);
            snapshot["native_aggregate"]["error_samples_omitted_output_cap"] =
                json!(state.samples.len());
            snapshot["native_aggregate"]["error_samples"] = json!([]);
            for total in snapshot["renditions"].as_object_mut().unwrap().values_mut() {
                for key in [
                    "native_roles_by_source_tag",
                    "source_tags",
                    "source_attribute_names",
                    "diagnostic_codes",
                ] {
                    let counts = &total["observations"][key];
                    let represented: u64 = counts["values"]
                        .as_object()
                        .unwrap()
                        .values()
                        .map(|value| value.as_u64().unwrap())
                        .sum();
                    let omitted = counts["omitted_observation_events"].as_u64().unwrap();
                    total["observations"][key] = json!({"values":null,
                        "represented_observation_events":represented,"omitted_observation_events":omitted,
                        "detail_values_omitted_output_cap":true});
                }
            }
            encoded = serde_json::to_vec(&snapshot)?;
            encoded.push(b'\n');
        }
        // Both receipts and the complete failure pack/index share one existing output allocation.
        if (encoded.len() as u64)
            .checked_mul(2)
            .is_none_or(|n| n > available)
        {
            return Err("native aggregate bounded receipt exceeds allocated output".into());
        }
        for file in [&mut self.summary, &mut self.progress] {
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&encoded)?;
            file.set_len(encoded.len() as u64)?;
            file.flush()?;
        }
        self.last = Instant::now();
        Ok(())
    }
    fn progress(&mut self, state: &Aggregate, bytes: u64, digest: &Sha256) -> Result<()> {
        self.check_cancel()?;
        if self.last.elapsed() >= Duration::from_secs(10) {
            self.write(state, bytes, digest, false, None)?;
        }
        Ok(())
    }
}

fn receive(
    receiver: &Receiver<RowResult>,
    pending: &mut BTreeMap<u64, RowResult>,
    state: &mut Aggregate,
    output: &mut Output,
    decoded_bytes: u64,
    decoded_digest: &Sha256,
) -> Result<()> {
    loop {
        output.progress(state, decoded_bytes, decoded_digest)?;
        match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => {
                let ordinal = result.job.ordinal;
                if ordinal < state.records_observed || pending.insert(ordinal, result).is_some() {
                    return Err("duplicate worker outcome ordinal".into());
                }
                while let Some(result) = pending.remove(&state.records_observed) {
                    output.retain_failure(state, &result)?;
                    state.reduce(result)?;
                }
                return Ok(());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err("native aggregate worker results disconnected".into())
            }
        }
    }
}

/// Only the explicitly selected complete original opinions COPY stream uses this mode.
pub(super) fn run(table: &str, out: &Path, maximum: u64) -> Result<()> {
    if table != "opinions" || maximum < 2 * 1024 * 1024 {
        return Err("native aggregate requires opinions and at least 2 MiB bounded output".into());
    }
    let mut output = Output::new(out, maximum)?;
    let mut state = Aggregate::new();
    let stdin = io::stdin();
    let mut input = BufReader::with_capacity(
        1024 * 1024,
        DecodedRead {
            input: stdin.lock(),
            digest: Sha256::new(),
            bytes: 0,
        },
    );
    output.write(&state, 0, &input.get_ref().digest, false, None)?;
    // Catch each field's panic as an outcome; avoid millions of unbounded default hook logs.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::thread::scope(|scope| -> Result<()> {
        let (results_tx, results_rx) = mpsc::sync_channel(IN_FLIGHT_ROWS);
        let mut senders = Vec::with_capacity(WORKERS);
        for _ in 0..WORKERS {
            let (tx, rx) = mpsc::sync_channel(1);
            let results = results_tx.clone();
            scope.spawn(move || worker(rx, results));
            senders.push(tx);
        }
        drop(results_tx);
        let mut pending = BTreeMap::new();
        let (mut raw, mut line) = (Vec::new(), Vec::new());
        if !metadata::record(&mut input, &mut raw, &mut line)? {
            return Err("original CSV header missing".into());
        }
        state.header_sha256 = Some(hash(&raw));
        state.raw_header_csv = raw.clone();
        state.header = metadata::fields(&raw)?
            .into_iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "original header must contain strings".into())
            })
            .collect::<Result<Vec<_>>>()?;
        if state.header.iter().map(String::as_str).ne(COLUMNS) {
            return Err("original 22 opinion columns differ".into());
        }
        let binding = json!({"original_header_csv_sha256":state.header_sha256,"columns":state.header,"renditions":RENDITIONS});
        append(&mut state.semantic, &binding)?;
        append(&mut state.compatible, &binding)?;
        append(&mut state.sources, &binding)?;
        for digest in state.field_digests.values_mut() {
            append(digest, &binding)?;
        }
        loop {
            output.progress(&state, input.get_ref().bytes, &input.get_ref().digest)?;
            if !metadata::record(&mut input, &mut raw, &mut line)? {
                state.input_eof = true;
                break;
            }
            let ordinal = state.records_read;
            state.records_read += 1;
            state.maximum_raw_record_bytes = state.maximum_raw_record_bytes.max(raw.len());
            let values = metadata::fields(&raw)?;
            if values.len() != COLUMNS.len() {
                return Err(format!("CSV field count differs at ordinal {ordinal}").into());
            }
            let source_bytes = values
                .iter()
                .filter_map(Value::as_str)
                .try_fold(0usize, |n, value| n.checked_add(value.len()))
                .ok_or("row source byte count overflow")?;
            let raw_bytes = raw.len();
            let decoded_bytes = source_bytes
                .checked_add(raw_bytes)
                .ok_or("row decoded byte count overflow")?;
            if decoded_bytes > IN_FLIGHT_BYTES {
                return Err(format!(
                    "original row {ordinal} raw CSV and values exceed in-flight source admission"
                )
                .into());
            }
            while state.inflight >= IN_FLIGHT_ROWS
                || state.queued_bytes + state.queued_raw_bytes + decoded_bytes > IN_FLIGHT_BYTES
            {
                receive(
                    &results_rx,
                    &mut pending,
                    &mut state,
                    &mut output,
                    input.get_ref().bytes,
                    &input.get_ref().digest,
                )?;
            }
            let row = Value::Object(state.header.iter().cloned().zip(values).collect());
            let job = Job {
                ordinal,
                raw_bytes,
                raw_sha256: hash(&raw),
                raw: std::mem::take(&mut raw),
                source_bytes,
                row,
            };
            senders[ordinal as usize % WORKERS]
                .send(job)
                .map_err(|_| "native aggregate worker disconnected")?;
            state.inflight += 1;
            state.queued_bytes += source_bytes;
            state.queued_raw_bytes += raw_bytes;
            state.maximum_inflight_bytes = state.maximum_inflight_bytes.max(state.queued_bytes);
            state.maximum_inflight_decoded_bytes = state
                .maximum_inflight_decoded_bytes
                .max(state.queued_bytes + state.queued_raw_bytes);
        }
        drop(senders);
        while state.inflight != 0 {
            receive(
                &results_rx,
                &mut pending,
                &mut state,
                &mut output,
                input.get_ref().bytes,
                &input.get_ref().digest,
            )?;
        }
        if !pending.is_empty() {
            return Err("worker outcome ordering incomplete".into());
        }
        Ok(())
    });
    std::panic::set_hook(hook);
    let failure = result.as_ref().err().map(ToString::to_string);
    output.write(
        &state,
        input.get_ref().bytes,
        &input.get_ref().digest,
        true,
        failure.as_deref(),
    )?;
    eprintln!(
        "native aggregate: {} ordered rows, {} attempts, {} errors, {} panics, {:.3}s",
        state.records_observed,
        state.totals.values().map(|t| t.attempted).sum::<u64>(),
        state.totals.values().map(|t| t.errors).sum::<u64>(),
        state.totals.values().map(|t| t.panics).sum::<u64>(),
        state.started.elapsed().as_secs_f64()
    );
    if result.is_err() {
        return Err("native aggregate incomplete; see bounded summary and prefix".into());
    }
    Ok(())
}
