//! All-column source discovery. CSV EOF is independent of compressed-source proof.
use crate::{census_features, metadata, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
    time::Instant,
};

const SCHEMA: &str = "courtlistener.record-census.v1";
const RECEIPT_RESERVE: u64 = 64 * 1024;
const WITNESS_ADMISSION_FLOOR: u64 = 64 * 1024;
const KEY_BYTES: usize = 768;
const JSON_KEY_BYTES: usize = 96;
const JSON_PATH_BYTES: usize = 256;
const ENUM_BYTES: usize = 64;
const ENUM_FIELDS: &[&str] = &[
    "type", "source", "status", "precedential_status", "jurisdiction",
    "jurisdiction_type", "docket_number_source", "document_type", "party_type",
    "ocr_status", "gender", "race", "political_affiliation", "religion",
];

struct DecodedRead<R> {
    input: R,
    digest: Sha256,
    bytes: u64,
}

impl<R: Read> Read for DecodedRead<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(bytes)?;
        self.bytes = self.bytes.checked_add(count as u64)
            .ok_or_else(|| io::Error::other("decoded CSV byte counter overflow"))?;
        self.digest.update(&bytes[..count]);
        Ok(count)
    }
}

#[derive(Default, Serialize)]
struct Column {
    name: String,
    key_name_cue: Option<&'static str>,
    null_rows: u64,
    empty_string_rows: u64,
    nonempty_string_rows: u64,
    string_utf8_bytes: u64,
    minimum_nonempty_utf8_bytes: Option<u64>,
    maximum_utf8_bytes: u64,
    maximum_source_ordinal_zero_based: Option<u64>,
    value_shapes: BTreeMap<&'static str, u64>,
    enum_literal_omitted_rows: u64,
    json_candidate_rows: u64,
    json_parse_failed_rows: u64,
    json_normalized_path_events: u64,
    markup_candidate_rows: u64,
    text_observed_rows: u64,
    uncertain_rows: u64,
}

#[derive(Serialize)]
struct Feature {
    source_rows: u64,
    observed_nodes: u64,
    first_source_ordinal_zero_based: u64,
    witness_ordinal: Option<u64>,
    #[serde(skip)]
    last_source_ordinal_zero_based: Option<u64>,
}

#[derive(Default, Serialize)]
struct Overflow {
    source_rows: u64,
    token_observations: u64,
    observed_nodes: u64,
    oversized_token_observations: u64,
    first_source_ordinal_zero_based: Option<u64>,
    #[serde(skip)]
    last_source_ordinal_zero_based: Option<u64>,
}

#[derive(Default, Serialize)]
struct WitnessCounts {
    retained_rows: u64,
    requested_rows: u64,
    omitted_rows: u64,
    compressed_bytes: u64,
    index_bytes: u64,
    uncompressed_json_bytes: u64,
    maximum_attempted_row_json_bytes: u64,
    capture_saturated: bool,
    remaining_bytes_when_saturated: Option<u64>,
}

#[derive(Serialize)]
struct FieldHash {
    utf8_bytes: usize,
    sha256: String,
}

#[derive(Serialize)]
struct Witness<'a> {
    schema: &'static str,
    table: &'a str,
    source_ordinal_zero_based: u64,
    columns: &'a [String],
    values: &'a [Value],
    raw_csv: &'a str,
    original_csv_row_bytes: usize,
    original_csv_row_sha256: String,
    source_string_hashes: BTreeMap<&'a str, FieldHash>,
}

#[derive(Serialize)]
struct FeatureProof {
    column: usize,
    token: String,
}

#[derive(Serialize)]
struct RepresentationProof {
    column: usize,
    change: &'static str,
    value_shape: Option<&'static str>,
    utf8_bytes: Option<u64>,
}

#[derive(Default)]
struct Row {
    needed: Vec<FeatureProof>,
    reasons: Vec<RepresentationProof>,
    uncertain_columns: BTreeSet<usize>,
    wants_witness: bool,
}

struct Census {
    columns: Vec<Column>,
    features: Vec<BTreeMap<String, Feature>>,
    overflow: Vec<Overflow>,
    feature_keys: usize,
    feature_bytes_reserved: u64,
    features_without_witness: u64,
    records_read: u64,
    records_observed: u64,
    maximum_raw_record_bytes: u64,
    witnesses: WitnessCounts,
}

fn add(counter: &mut u64, amount: u64) -> Result<()> {
    *counter = counter.checked_add(amount).ok_or("record census counter overflow")?;
    Ok(())
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn new_file(out: &Path, name: &str) -> Result<File> {
    Ok(OpenOptions::new().write(true).create_new(true).open(out.join(name))?)
}

fn rewrite(file: &mut File, bytes: &[u8]) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.flush()?;
    Ok(())
}

fn fits(maximum: u64, sizes: &[u64]) -> bool {
    sizes.iter().try_fold(0u64, |sum, size| sum.checked_add(*size))
        .is_some_and(|size| size <= maximum)
}

fn shape(value: &str) -> &'static str {
    if value.is_empty() { return "empty"; }
    if value.chars().all(char::is_whitespace) { return "whitespace"; }
    let trimmed = value.trim();
    if matches!(trimmed, "t" | "f" | "true" | "false") { return "boolean_literal"; }
    let digits = trimmed.strip_prefix(|ch| matches!(ch, '+' | '-')).unwrap_or(trimmed);
    if !digits.is_empty() && digits.bytes().all(|ch| ch.is_ascii_digit()) {
        return if value == trimmed { "integer_lexeme" } else { "padded_integer_lexeme" };
    }
    if trimmed.len() == 36 && trimmed.bytes().enumerate().all(|(i, ch)| {
        if matches!(i, 8 | 13 | 18 | 23) { ch == b'-' } else { ch.is_ascii_hexdigit() }
    }) { return "uuid_lexeme"; }
    if trimmed.starts_with('{') { return "object_or_pg_array_candidate"; }
    if trimmed.starts_with('[') { return "array_candidate"; }
    if markup(value) { return "markup_candidate"; }
    if uri_scheme(trimmed).is_some() { return "uri_scheme_lexeme"; }
    if trimmed.starts_with("//") { return "network_path_lexeme"; }
    if trimmed.starts_with('/') || trimmed.starts_with("./") || trimmed.starts_with("../") {
        return "relative_path_lexeme";
    }
    if digits.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        if trimmed.len() >= 10 && trimmed.as_bytes().get(4) == Some(&b'-')
            && trimmed.as_bytes().get(7) == Some(&b'-')
        { return "date_or_timestamp_lexeme"; }
        if trimmed.parse::<f64>().is_ok() { return "number_lexeme"; }
    }
    if value.bytes().all(|ch| ch.is_ascii_alphabetic()) { return "alphabetic"; }
    if value.bytes().all(|ch| ch.is_ascii_alphanumeric() || b"_:-.".contains(&ch)) {
        return "identifier_lexeme";
    }
    if value.contains(['\r', '\n']) { return "multiline_text"; }
    if value.chars().any(char::is_whitespace) { return "spaced_text"; }
    "other"
}

fn uri_scheme(value: &str) -> Option<&str> {
    if !value.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) { return None; }
    let colon = value.find(':')?;
    let scheme = &value[..colon];
    (scheme.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && scheme.bytes().all(|ch| ch.is_ascii_alphanumeric() || b"+-.".contains(&ch)))
        .then_some(scheme)
}

fn markup(value: &str) -> bool {
    value.match_indices('<').any(|(offset, _)| {
        let tail = &value[offset + 1..];
        let tag = tail.strip_prefix('/').unwrap_or(tail);
        (tag.chars().next().is_some_and(char::is_alphabetic)
            || tag.starts_with("!--") || tag.starts_with("!DOCTYPE") || tag.starts_with("?xml"))
            && tag.contains('>')
    })
}

fn enum_field(name: &str) -> bool {
    ENUM_FIELDS.contains(&name) || name.ends_with("_type") || name.ends_with("_status")
}

fn text_field(name: &str) -> bool {
    matches!(name, "plain_text" | "text" | "description" | "notes" | "syllabus"
        | "headnotes" | "summary" | "disposition" | "history" | "procedural_history"
        | "posture" | "arguments" | "headmatter" | "attorneys" | "judges"
        | "other_dates" | "cross_reference" | "correction" | "transcript"
        | "stt_transcript" | "addendum_content_raw")
}

fn json_field(name: &str, value: &str) -> bool {
    name.starts_with("json_")
        || (name.ends_with("_json") && !name.starts_with("filepath"))
        || value.trim_start().starts_with('{') || value.trim_start().starts_with('[')
}

fn bucket(length: usize) -> &'static str {
    match length {
        0 => "0", 1 => "1", 2..=3 => "2-3", 4..=7 => "4-7",
        8..=15 => "8-15", 16..=31 => "16-31", 32..=63 => "32-63",
        64..=127 => "64-127", _ => "128+",
    }
}

fn json_key(key: &str) -> String {
    let key_shape = shape(key);
    if matches!(key_shape, "integer_lexeme" | "padded_integer_lexeme" | "uuid_lexeme") {
        return format!("{{{key_shape}}}");
    }
    if key.len() > JSON_KEY_BYTES {
        return format!("{{long-key:{key_shape}:{}}}", bucket(key.len()));
    }
    if key.len() >= 16 && key.bytes().all(|ch| ch.is_ascii_alphanumeric())
        && key.as_bytes().windows(2)
            .filter(|pair| pair[0].is_ascii_digit() != pair[1].is_ascii_digit()).count() >= 3
    { return format!("{{opaque-key:{}}}", bucket(key.len())); }
    let mut result = String::new();
    let mut previous_digit = false;
    for ch in key.chars() {
        if ch.is_ascii_digit() {
            if !previous_digit { result.push('#'); }
            previous_digit = true;
        } else {
            previous_digit = false;
            match ch { '~' => result.push_str("~0"), '/' => result.push_str("~1"), _ => result.push(ch) }
        }
    }
    result
}

impl Census {
    fn overflow(&mut self, column: usize, ordinal: u64, nodes: u64, oversized: bool, row: &mut Row) -> Result<()> {
        let overflow = &mut self.overflow[column];
        if overflow.last_source_ordinal_zero_based != Some(ordinal) {
            add(&mut overflow.source_rows, 1)?;
            overflow.last_source_ordinal_zero_based = Some(ordinal);
        }
        add(&mut overflow.token_observations, 1)?;
        add(&mut overflow.observed_nodes, nodes)?;
        if oversized { add(&mut overflow.oversized_token_observations, 1)?; }
        overflow.first_source_ordinal_zero_based.get_or_insert(ordinal);
        row.wants_witness = true;
        row.uncertain_columns.insert(column);
        Ok(())
    }

    fn observe(&mut self, column: usize, ordinal: u64, token: String, nodes: u64,
        feature_limit: usize, feature_budget: u64, row: &mut Row) -> Result<()> {
        if nodes == 0 { return Ok(()); }
        if token.len() > KEY_BYTES {
            return self.overflow(column, ordinal, nodes, true, row);
        }
        // Six bytes per input byte bounds JSON escaping; counters have fixed width reserves.
        let reservation = ((token.len() + self.columns[column].name.len()) as u64)
            .checked_mul(6).and_then(|bytes| bytes.checked_add(512))
            .ok_or("record census feature reservation overflow")?;
        if !self.features[column].contains_key(&token) {
            if self.feature_keys >= feature_limit
                || !fits(feature_budget, &[self.feature_bytes_reserved, reservation])
            { return self.overflow(column, ordinal, nodes, false, row); }
            self.feature_keys += 1;
            add(&mut self.feature_bytes_reserved, reservation)?;
            add(&mut self.features_without_witness, 1)?;
            self.features[column].insert(token.clone(), Feature { source_rows: 0, observed_nodes: 0,
                first_source_ordinal_zero_based: ordinal, witness_ordinal: None,
                last_source_ordinal_zero_based: None });
        }
        let feature = self.features[column].get_mut(&token).ok_or("observed feature missing")?;
        if feature.last_source_ordinal_zero_based != Some(ordinal) {
            add(&mut feature.source_rows, 1)?;
            feature.last_source_ordinal_zero_based = Some(ordinal);
            if feature.witness_ordinal.is_none() {
                row.wants_witness = true;
                if !self.witnesses.capture_saturated { row.needed.push(FeatureProof { column, token }); }
            }
        }
        add(&mut feature.observed_nodes, nodes)
    }

    fn urls(&mut self, column: usize, ordinal: u64, prefix: &str, value: &str,
        feature_limit: usize, feature_budget: u64, row: &mut Row) -> Result<()> {
        let value = value.trim();
        if let Some(scheme) = uri_scheme(value) {
            self.observe(column, ordinal, format!("{prefix}uri:literal-scheme:{scheme}"), 1,
                feature_limit, feature_budget, row)?;
            if let Some(authority) = value[scheme.len() + 1..].strip_prefix("//") {
                let authority = authority.split(|ch: char| ch.is_whitespace() || matches!(ch, '/' | '?' | '#'))
                    .next().unwrap_or_default();
                self.observe(column, ordinal, format!("{prefix}uri:literal-origin:{scheme}://{authority}"), 1,
                    feature_limit, feature_budget, row)?;
            }
        } else if let Some(authority) = value.strip_prefix("//") {
            let authority = authority.split(|ch: char| ch.is_whitespace() || matches!(ch, '/' | '?' | '#'))
                .next().unwrap_or_default();
            self.observe(column, ordinal, format!("{prefix}uri:literal-network-authority:{authority}"), 1,
                feature_limit, feature_budget, row)?;
        }
        Ok(())
    }

    fn source_forms(&mut self, column: usize, ordinal: u64, prefix: &str, value: &str,
        prose: bool, feature_limit: usize, feature_budget: u64, row: &mut Row) -> Result<()> {
        let declared_plain_text = prefix.is_empty() && self.columns[column].name == "plain_text";
        if !declared_plain_text && markup(value) {
            if prefix.is_empty() { add(&mut self.columns[column].markup_candidate_rows, 1)?; }
            for (token, nodes) in census_features::observe("markup", value) {
                self.observe(column, ordinal, format!("{prefix}markup:{token}"), nodes,
                    feature_limit, feature_budget, row)?;
            }
        } else if declared_plain_text || prose || value.contains(['\r', '\n']) {
            if prefix.is_empty() { add(&mut self.columns[column].text_observed_rows, 1)?; }
            for (token, nodes) in census_features::observe("plain_text", value) {
                self.observe(column, ordinal, format!("{prefix}{token}"), nodes,
                    feature_limit, feature_budget, row)?;
            }
        }
        Ok(())
    }

    fn json_forms(&mut self, column: usize, ordinal: u64, path: &str, value: &Value,
        feature_limit: usize, feature_budget: u64, row: &mut Row) -> Result<()> {
        let kind = match value { Value::Null => "null", Value::Bool(_) => "boolean",
            Value::Number(_) => "number", Value::String(_) => "string",
            Value::Array(_) => "array", Value::Object(_) => "object" };
        self.observe(column, ordinal, format!("json:{path}:type:{kind}"), 1,
            feature_limit, feature_budget, row)?;
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    let candidate = format!("{path}/{}", json_key(key));
                    let next = if candidate.len() > JSON_PATH_BYTES {
                        add(&mut self.columns[column].json_normalized_path_events, 1)?;
                        "$/{overlong-path}".to_owned()
                    } else { candidate };
                    self.observe(column, ordinal, format!("json:{next}:key"), 1,
                        feature_limit, feature_budget, row)?;
                    self.json_forms(column, ordinal, &next, child, feature_limit, feature_budget, row)?;
                }
            }
            Value::Array(array) => {
                self.observe(column, ordinal, format!("json:{path}:array-length:{}", bucket(array.len())), 1,
                    feature_limit, feature_budget, row)?;
                let candidate = format!("{path}[]");
                let next = if candidate.len() > JSON_PATH_BYTES {
                    add(&mut self.columns[column].json_normalized_path_events, 1)?;
                    "$/{overlong-path}[]".to_owned()
                } else { candidate };
                for child in array {
                    self.json_forms(column, ordinal, &next, child, feature_limit, feature_budget, row)?;
                }
            }
            Value::String(source) => {
                self.observe(column, ordinal, format!("json:{path}:string-shape:{}", shape(source)), 1,
                    feature_limit, feature_budget, row)?;
                let prefix = format!("json:{path}:");
                self.urls(column, ordinal, &prefix, source, feature_limit, feature_budget, row)?;
                self.source_forms(column, ordinal, &prefix, source, false, feature_limit, feature_budget, row)?;
            }
            _ => (),
        }
        Ok(())
    }

    fn account(&mut self, column: usize, ordinal: u64, value: &Value, row: &mut Row) -> Result<()> {
        let stat = &mut self.columns[column];
        let Some(source) = value.as_str() else {
            if !value.is_null() { return Err("unexpected non-string COPY CSV value".into()); }
            if stat.null_rows == 0 {
                row.reasons.push(RepresentationProof { column, change: "first_null", value_shape: None, utf8_bytes: None });
            }
            add(&mut stat.null_rows, 1)?;
            return Ok(());
        };
        let bytes = source.len() as u64;
        add(&mut stat.string_utf8_bytes, bytes)?;
        if source.is_empty() {
            if stat.empty_string_rows == 0 {
                row.reasons.push(RepresentationProof { column, change: "first_empty_string", value_shape: Some("empty"), utf8_bytes: Some(0) });
            }
            add(&mut stat.empty_string_rows, 1)?;
        } else {
            add(&mut stat.nonempty_string_rows, 1)?;
            if bytes > stat.maximum_utf8_bytes {
                stat.maximum_utf8_bytes = bytes;
                stat.maximum_source_ordinal_zero_based = Some(ordinal);
                row.reasons.push(RepresentationProof { column, change: "maximum_utf8_bytes", value_shape: None, utf8_bytes: Some(bytes) });
            }
            if stat.minimum_nonempty_utf8_bytes.is_none_or(|minimum| bytes < minimum) {
                stat.minimum_nonempty_utf8_bytes = Some(bytes);
                row.reasons.push(RepresentationProof { column, change: "minimum_nonempty_utf8_bytes", value_shape: None, utf8_bytes: Some(bytes) });
            }
        }
        let value_shape = shape(source);
        let count = stat.value_shapes.entry(value_shape).or_default();
        if *count == 0 {
            row.reasons.push(RepresentationProof { column, change: "first_value_shape", value_shape: Some(value_shape), utf8_bytes: Some(bytes) });
        }
        add(count, 1)
    }
}

fn progress(file: &mut File, summary: &File, data: &File, index: &File, table: &str,
    census: &Census, decoded_bytes: u64, digest: &Sha256, maximum: u64,
    started: Instant, status: &str) -> Result<()> {
    let overflow_events = census.overflow.iter().try_fold(0u64, |sum, item|sum.checked_add(item.token_observations))
        .ok_or("record census overflow event counter overflow")?;
    let mut bytes = serde_json::to_vec(&json!({"schema":SCHEMA,"table":table,"status":status,
        "decoded_input_complete":status == "complete_decoded_input","full_snapshot_verified_by_this_module":false,
        "records_read":census.records_read,"records_observed":census.records_observed,
        "columns":census.columns.len(),"decoded_csv_bytes":decoded_bytes,
        "decoded_csv_sha256":format!("{:x}",digest.clone().finalize()),
        "feature_keys":census.feature_keys,"features_without_retained_witness":census.features_without_witness,
        "overflow_token_observations":overflow_events,
        "witnesses":census.witnesses,"seconds":started.elapsed().as_secs_f64()}))?;
    bytes.push(b'\n');
    if !fits(maximum, &[summary.metadata()?.len(), data.metadata()?.len(), index.metadata()?.len(), bytes.len() as u64]) {
        return Err("record census progress exceeds total output cap".into());
    }
    rewrite(file, &bytes)
}

/// Read every column of one decoded COPY CSV stream. No primary-key column is required.
pub(super) fn run(table: &str, out: &Path, maximum_bytes: u64, witness_maximum_bytes: u64,
    feature_limit: usize) -> Result<()> {
    if table.is_empty() || table.len() > 128 || maximum_bytes < RECEIPT_RESERVE
        || witness_maximum_bytes > maximum_bytes || feature_limit == 0
    { return Err("invalid table name or record census allocation".into()); }
    fs::create_dir_all(out)?;
    let mut summary_file = new_file(out, "summary.json")?;
    let mut progress_file = new_file(out, "progress.json")?;
    let mut witness_file = new_file(out, "witnesses.jsonl.zst")?;
    let mut index_file = new_file(out, "witness-index.jsonl")?;
    rewrite(&mut summary_file, b"{\"schema\":\"courtlistener.record-census.v1\",\"status\":\"running\",\"decoded_input_complete\":false}\n")?;
    let started = Instant::now();
    let stdin = io::stdin();
    let mut input = BufReader::with_capacity(1024 * 1024,
        DecodedRead { input: stdin.lock(), digest: Sha256::new(), bytes: 0 });
    let mut header = Vec::new();
    let mut header_sha256 = String::new();
    let mut feature_budget = 0;
    let mut receipt_reserve = RECEIPT_RESERVE;
    let mut census = Census { columns: Vec::new(), features: Vec::new(), overflow: Vec::new(),
        feature_keys: 0, feature_bytes_reserved: 0, features_without_witness: 0,
        records_read: 0, records_observed: 0, maximum_raw_record_bytes: 0,
        witnesses: WitnessCounts::default() };
    let result = (|| -> Result<()> {
        let (mut raw, mut line) = (Vec::new(), Vec::new());
        if !metadata::record(&mut input, &mut raw, &mut line)? { return Err("source CSV header missing".into()); }
        header_sha256 = hash(&raw);
        header = metadata::fields(&raw)?.into_iter().map(|value| value.as_str()
            .filter(|name|!name.is_empty()).map(str::to_owned).ok_or_else(||"source CSV header field missing".into()))
            .collect::<Result<Vec<_>>>()?;
        if header.iter().collect::<HashSet<_>>().len() != header.len() {
            return Err("duplicate source CSV columns".into());
        }
        census.columns = header.iter().map(|name|Column { name:name.clone(),
            key_name_cue:if name == "id" { Some("id_name") } else if name.ends_with("_id") { Some("foreign_key_name") } else { None },
            ..Column::default() }).collect();
        census.features = (0..header.len()).map(|_|BTreeMap::new()).collect();
        census.overflow = (0..header.len()).map(|_|Overflow::default()).collect();
        let header_bytes = header.iter().try_fold(0u64, |sum, name|sum.checked_add(name.len() as u64))
            .ok_or("source header byte count overflow")?;
        receipt_reserve = header_bytes.checked_mul(6)
            .and_then(|bytes|bytes.checked_add((header.len() as u64).checked_mul(4096)?))
            .and_then(|bytes|bytes.checked_add(RECEIPT_RESERVE)).ok_or("record census receipt reservation overflow")?;
        feature_budget = maximum_bytes.saturating_sub(witness_maximum_bytes).saturating_sub(receipt_reserve);
        let enums = header.iter().map(|name|enum_field(name)).collect::<Vec<_>>();
        let prose = header.iter().map(|name|text_field(name)).collect::<Vec<_>>();
        loop {
            if !metadata::record(&mut input, &mut raw, &mut line)? { return Ok(()); }
            let ordinal = census.records_read;
            add(&mut census.records_read, 1)?;
            census.maximum_raw_record_bytes = census.maximum_raw_record_bytes.max(raw.len() as u64);
            let values = metadata::fields(&raw)?;
            if values.len() != header.len() { return Err(format!("source CSV field count differs at ordinal {ordinal}").into()); }
            let mut row = Row::default();
            for (column, value) in values.iter().enumerate() {
                census.account(column, ordinal, value, &mut row)?;
                let Some(source) = value.as_str().filter(|source|!source.is_empty()) else { continue; };
                if enums[column] {
                    if source.len() <= ENUM_BYTES {
                        census.observe(column, ordinal, format!("enum-name-cue:literal:{}",serde_json::to_string(source)?), 1,
                            feature_limit, feature_budget, &mut row)?;
                    } else {
                        add(&mut census.columns[column].enum_literal_omitted_rows, 1)?;
                        census.observe(column, ordinal, "enum-name-cue:overlong-literal".to_owned(), 1,
                            feature_limit, feature_budget, &mut row)?;
                        row.uncertain_columns.insert(column);
                    }
                }
                census.urls(column, ordinal, "", source, feature_limit, feature_budget, &mut row)?;
                if json_field(&header[column], source) {
                    add(&mut census.columns[column].json_candidate_rows, 1)?;
                    match serde_json::from_str::<Value>(source) {
                        Ok(parsed) => census.json_forms(column, ordinal, "$", &parsed, feature_limit, feature_budget, &mut row)?,
                        Err(error) => {
                            add(&mut census.columns[column].json_parse_failed_rows, 1)?;
                            row.uncertain_columns.insert(column);
                            census.observe(column, ordinal, format!("json:parse-failed-or-non-json-candidate:{:?}",error.classify()), 1,
                                feature_limit, feature_budget, &mut row)?;
                            census.source_forms(column, ordinal, "", source, prose[column], feature_limit, feature_budget, &mut row)?;
                        }
                    }
                } else {
                    census.source_forms(column, ordinal, "", source, prose[column], feature_limit, feature_budget, &mut row)?;
                }
            }
            for &column in &row.uncertain_columns { add(&mut census.columns[column].uncertain_rows, 1)?; }
            add(&mut census.records_observed, 1)?;
            if row.wants_witness || !row.reasons.is_empty() {
                add(&mut census.witnesses.requested_rows, 1)?;
                let remaining = witness_maximum_bytes.saturating_sub(census.witnesses.compressed_bytes)
                    .saturating_sub(census.witnesses.index_bytes).min(maximum_bytes
                        .saturating_sub(receipt_reserve).saturating_sub(census.feature_bytes_reserved)
                        .saturating_sub(census.witnesses.compressed_bytes).saturating_sub(census.witnesses.index_bytes));
                if !census.witnesses.capture_saturated && remaining < WITNESS_ADMISSION_FLOOR {
                    census.witnesses.capture_saturated = true;
                    census.witnesses.remaining_bytes_when_saturated = Some(remaining);
                }
                if census.witnesses.capture_saturated {
                    add(&mut census.witnesses.omitted_rows, 1)?;
                } else {
                    let selected = Witness { schema:"courtlistener.record-census-witness.v1",table,
                        source_ordinal_zero_based:ordinal,columns:&header,values:&values,
                        raw_csv:std::str::from_utf8(&raw)?, original_csv_row_bytes:raw.len(),
                        original_csv_row_sha256:hash(&raw), source_string_hashes:header.iter().zip(&values)
                            .filter_map(|(name,value)|value.as_str().map(|source|(name.as_str(),FieldHash {
                                utf8_bytes:source.len(),sha256:hash(source.as_bytes()) }))).collect() };
                    let mut encoded = serde_json::to_vec(&selected)?;
                    encoded.push(b'\n');
                    census.witnesses.maximum_attempted_row_json_bytes = census.witnesses.maximum_attempted_row_json_bytes.max(encoded.len() as u64);
                    let frame = zstd::bulk::compress(&encoded, 3)?;
                    let witness_ordinal = census.witnesses.retained_rows;
                    let mut index = serde_json::to_vec(&json!({"schema":"courtlistener.record-census-witness-index.v1",
                        "table":table,"witness_ordinal":witness_ordinal,"source_ordinal_zero_based":ordinal,
                        "compressed_offset":census.witnesses.compressed_bytes,"compressed_bytes":frame.len(),
                        "raw_bytes":encoded.len(),"raw_sha256":hash(&encoded),"feature_proofs":row.needed,
                        "representation_proofs":row.reasons,"uncertain_columns":row.uncertain_columns,
                        "column_reference_basis":"Zero-based index into complete witness columns"}))?;
                    index.push(b'\n');
                    if fits(witness_maximum_bytes, &[census.witnesses.compressed_bytes,census.witnesses.index_bytes,
                        frame.len() as u64,index.len() as u64]) && fits(maximum_bytes,
                        &[receipt_reserve,census.feature_bytes_reserved,census.witnesses.compressed_bytes,
                            census.witnesses.index_bytes,frame.len() as u64,index.len() as u64]) {
                        witness_file.write_all(&frame)?;
                        index_file.write_all(&index)?;
                        add(&mut census.witnesses.compressed_bytes, frame.len() as u64)?;
                        add(&mut census.witnesses.index_bytes, index.len() as u64)?;
                        add(&mut census.witnesses.uncompressed_json_bytes, encoded.len() as u64)?;
                        add(&mut census.witnesses.retained_rows, 1)?;
                        for proof in row.needed {
                            let feature = census.features[proof.column].get_mut(&proof.token).ok_or("witness feature missing")?;
                            if feature.witness_ordinal.is_none() {
                                feature.witness_ordinal = Some(witness_ordinal);
                                census.features_without_witness = census.features_without_witness.checked_sub(1).ok_or("witness feature counter underflow")?;
                            }
                        }
                    } else { add(&mut census.witnesses.omitted_rows, 1)?; }
                }
            }
            if census.records_observed % 10_000 == 0 {
                witness_file.flush()?;
                index_file.flush()?;
                progress(&mut progress_file,&summary_file,&witness_file,&index_file,table,&census,
                    input.get_ref().bytes,&input.get_ref().digest,maximum_bytes,started,"running")?;
            }
        }
    })();
    witness_file.flush()?;
    index_file.flush()?;
    let data_bytes = witness_file.metadata()?.len();
    let index_bytes = index_file.metadata()?.len();
    let failure = result.as_ref().err().map(ToString::to_string);
    let complete = result.is_ok();
    let decoded_bytes = input.get_ref().bytes;
    let decoded_sha256 = format!("{:x}",input.get_ref().digest.clone().finalize());
    let feature_counts = header.iter().zip(&census.features).filter(|(_,features)|!features.is_empty()).collect::<BTreeMap<_,_>>();
    let feature_overflow = header.iter().zip(&census.overflow).filter(|(_,overflow)|overflow.token_observations != 0).collect::<BTreeMap<_,_>>();
    let json_complete = census.columns.iter().all(|column|column.json_parse_failed_rows == 0);
    let enum_complete = census.columns.iter().all(|column|column.enum_literal_omitted_rows == 0);
    let mut inventory_omitted = false;
    let summary = |omitted:bool| {
        let parts = [json!({
        "schema":SCHEMA,"table":table,"status":if complete { "complete_decoded_input" } else { "failed" },
        "decoded_input_complete":complete,"full_snapshot_verified_by_this_module":false,"failure":failure,
        "column_accounting_complete":complete,"records_read":census.records_read,"records_observed":census.records_observed,
        "decoded_csv_bytes":decoded_bytes,"decoded_csv_sha256":decoded_sha256,
        "maximum_raw_record_bytes":census.maximum_raw_record_bytes,"source_columns":header,
        "original_header_csv_sha256":header_sha256,"column_stats":census.columns,
        }), json!({
        "csv_schema":"Ordered original CSV columns; every decoded cell is a UTF-8 string or SQL NULL. Quoted empty strings remain distinct. No logical SQL type, nullability, primary key or foreign-key declaration is inferred.",
        "key_role_scope":"id and *_id are column-name cues only. Their exact presence/NULL/empty/value-shape counts are in column_stats. Global ID uniqueness, reference-target membership and official FK declarations are unmeasured.",
        "feature_scope":census_features::SCOPE,
        "feature_key_scope":"Bounded (original column, token) forms. CSV value-shape histograms are finite and counted independently of the feature ceiling. URI schemes/origins preserve literal case and authority after trimming; this is lexical discovery, not URL validation, retrieval or landing proof. Enum literals use explicit name cues and at most 64 UTF-8 bytes; long literal omissions retain one uncertain form and original row evidence. JSON arrays use [] paths, numeric/UUID/opaque/long keys and overlong paths are normalized; string values/party names are not key novelty. Only valid serde_json values are traversed; candidate parse failures are uncertain and counted. Markup candidates use existing HTML5 DOM observations; plain text is never submitted as markup.",
        "enum_name_cues":ENUM_FIELDS,"enum_name_suffix_cues":["_type","_status"],
        "enum_literal_maximum_utf8_bytes":ENUM_BYTES,"feature_token_maximum_utf8_bytes":KEY_BYTES,
        "json_key_maximum_utf8_bytes":JSON_KEY_BYTES,"json_path_maximum_utf8_bytes":JSON_PATH_BYTES,
        "feature_limit":feature_limit,"feature_keys":census.feature_keys,"feature_budget_bytes":feature_budget,
        "feature_bytes_reserved":census.feature_bytes_reserved,
        }), json!({
        "feature_overflow":feature_overflow,
        "feature_inventory_omitted_due_to_output_cap":omitted,
        "json_candidate_analysis_complete":json_complete,
        "enum_literal_inventory_complete":enum_complete,
        "feature_inventory_complete":complete && !omitted && feature_overflow.is_empty() && json_complete
            && enum_complete && census.witnesses.omitted_rows == 0 && census.features_without_witness == 0,
        "feature_counts":if omitted { json!({}) } else { json!(feature_counts) },
        "features_without_retained_witness":census.features_without_witness,
        "witness_capture_complete":complete && census.witnesses.omitted_rows == 0 && census.features_without_witness == 0,
        }), json!({
        "witnesses":census.witnesses,"witness_maximum_bytes":witness_maximum_bytes,
        "witness_admission_floor_bytes":WITNESS_ADMISSION_FLOOR,"maximum_bytes":maximum_bytes,
        "actual_witness_data_bytes":data_bytes,"actual_witness_index_bytes":index_bytes,
        "seconds":started.elapsed().as_secs_f64(),
        "scope":"Every original column of decoded COPY CSV stdin. Complete native EOF must additionally be bound to the caller's pinned complete compressed-source bytes/hash, source identity and decompressor terminal proof. Discovery observations are not legal-semantic gold or parser/browser qualification.",
        "limits":["Zero-based ordinals count CSV records after the original header, not lines or unique legal documents",
            "Input is held one complete record at a time; JSON/DOM processing may allocate in proportion to that record, not the whole collection",
            "Feature/witness caps never truncate the input scan. Overflow counts token-observation/node/row events without claiming distinct dropped keys",
            "Witnesses are complete original column/value arrays plus literal raw CSV, CSV/string hashes and independent Zstandard frames; cap omissions are explicit",
            "Decoded hash covers all bytes read, including buffered read-ahead on failure; only successful EOF covers the exact decoded stream"]
        })];
        let mut fields = serde_json::Map::new();
        for part in parts {
            match part {
                Value::Object(part) => fields.extend(part),
                _ => unreachable!("summary parts are object literals"),
            }
        }
        Value::Object(fields)
    };
    let mut encoded = serde_json::to_vec(&summary(false))?;
    encoded.push(b'\n');
    if !fits(maximum_bytes,&[data_bytes,index_bytes,
        encoded.len() as u64,RECEIPT_RESERVE]) {
        inventory_omitted = true;
        encoded = serde_json::to_vec(&summary(true))?;
        encoded.push(b'\n');
    }
    if !fits(maximum_bytes,&[data_bytes,index_bytes,
        encoded.len() as u64,RECEIPT_RESERVE]) { return Err("record census column receipt exceeds total output cap".into()); }
    rewrite(&mut summary_file,&encoded)?;
    progress(&mut progress_file,&summary_file,&witness_file,&index_file,table,&census,
        decoded_bytes,&input.get_ref().digest,maximum_bytes,started,if complete { "complete_decoded_input" } else { "failed" })?;
    eprintln!("record census {table}: {} records; {} columns; {} feature keys; {} witnesses; feature inventory omitted {}; {:.3}s",
        census.records_observed,header.len(),census.feature_keys,census.witnesses.retained_rows,inventory_omitted,started.elapsed().as_secs_f64());
    if let Some(failure) = failure { return Err(failure.into()); }
    Ok(())
}
