//! Streaming source discovery. These are source observations, not parser gold.
use crate::{census_features, metadata, Result, RENDITIONS};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{btree_map::Entry, BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
    time::Instant,
};

const SCHEMA: &str = "courtlistener.source-census.v1";
const RECEIPT_RESERVE: u64 = 64 * 1024;
const WITNESS_ADMISSION_FLOOR: u64 = 64 * 1024;

struct DecodedRead<R> {
    input: R,
    digest: Sha256,
    bytes: u64,
}

impl<R: Read> Read for DecodedRead<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(bytes)?;
        self.bytes = self
            .bytes
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("decoded CSV byte counter overflow"))?;
        self.digest.update(&bytes[..count]);
        Ok(count)
    }
}

#[derive(Default, Serialize)]
struct Column {
    name: String,
    null_rows: u64,
    empty_string_rows: u64,
    nonempty_string_rows: u64,
    string_utf8_bytes: u64,
    minimum_nonempty_utf8_bytes: Option<u64>,
    maximum_utf8_bytes: u64,
    maximum_source_ordinal_zero_based: Option<u64>,
}

#[derive(Serialize)]
struct Feature {
    source_rows: u64,
    observed_nodes: u64,
    first_source_ordinal_zero_based: u64,
    witness_ordinal: Option<u64>,
}

#[derive(Serialize)]
struct FeatureProof {
    rendition: String,
    token: String,
    observed_nodes: u64,
    retained_in_feature_inventory: bool,
}

#[derive(Default, Serialize)]
struct FeatureOverflow {
    source_rows: u64,
    token_observations: u64,
    observed_nodes: u64,
    first_source_ordinal_zero_based: Option<u64>,
}

#[derive(Serialize)]
struct RepresentationProof {
    column: String,
    change: &'static str,
    utf8_bytes: Option<u64>,
}

#[derive(Serialize)]
struct SourceHash {
    utf8_bytes: usize,
    sha256: String,
}

#[derive(Serialize)]
struct Witness<'a> {
    schema: &'static str,
    source_ordinal_zero_based: u64,
    columns: &'a [String],
    values: &'a [Value],
    original_csv_row_bytes: usize,
    original_csv_row_sha256: String,
    source_string_hashes: BTreeMap<&'a str, SourceHash>,
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

#[derive(Default)]
struct Census {
    header: Vec<String>,
    header_sha256: String,
    columns: Vec<Column>,
    features: BTreeMap<String, BTreeMap<String, Feature>>,
    feature_overflow: BTreeMap<String, FeatureOverflow>,
    feature_keys: usize,
    features_without_witness: u64,
    rows_read: u64,
    rows_observed: u64,
    decoded_bytes: u64,
    maximum_raw_record_bytes: u64,
    witnesses: WitnessCounts,
}

fn add(counter: &mut u64, amount: u64) -> Result<()> {
    *counter = counter
        .checked_add(amount)
        .ok_or("census counter overflow")?;
    Ok(())
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn new_file(out: &Path, name: &str) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join(name))?)
}

fn rewrite(file: &mut File, bytes: &[u8]) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.flush()?;
    Ok(())
}

fn fits(maximum: u64, sizes: &[u64]) -> bool {
    sizes
        .iter()
        .try_fold(0u64, |sum, size| sum.checked_add(*size))
        .is_some_and(|sum| sum <= maximum)
}

fn progress(
    file: &mut File,
    summary: &File,
    data: &File,
    index: &File,
    census: &Census,
    digest: &Sha256,
    started: Instant,
    maximum: u64,
    status: &str,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(&serde_json::json!({
        "schema":SCHEMA,"status":status,"complete":status == "complete_decoded_input",
        "records_read":census.rows_read,"records_observed":census.rows_observed,
        "decoded_csv_bytes":census.decoded_bytes,
        "decoded_csv_sha256":format!("{:x}",digest.clone().finalize()),
        "maximum_raw_record_bytes":census.maximum_raw_record_bytes,
        "columns":census.columns.len(),"feature_keys":census.feature_keys,
        "feature_inventory_complete":status == "complete_decoded_input" && census.feature_overflow.is_empty(),
        "feature_overflow":census.feature_overflow,
        "features_without_retained_witness":census.features_without_witness,
        "witnesses":census.witnesses,"seconds":started.elapsed().as_secs_f64()
    }))?;
    bytes.push(b'\n');
    if !fits(
        maximum,
        &[
            summary.metadata()?.len(),
            data.metadata()?.len(),
            index.metadata()?.len(),
            bytes.len() as u64,
        ],
    ) {
        return Err("census progress would exceed total output cap".into());
    }
    rewrite(file, &bytes)
}

#[derive(Serialize)]
struct Summary<'a> {
    schema: &'static str,
    status: &'a str,
    decoded_input_complete: bool,
    full_snapshot_verified_by_this_module: bool,
    failure: Option<String>,
    scope: &'static str,
    feature_scope: &'static str,
    feature_key_scope: &'static str,
    observation_limit: usize,
    feature_keys: usize,
    feature_inventory_omitted_due_to_output_cap: bool,
    feature_inventory_complete: bool,
    feature_overflow: &'a BTreeMap<String, FeatureOverflow>,
    features_without_retained_witness: u64,
    row_limit: Option<u64>,
    records_read: u64,
    records_observed: u64,
    decoded_csv_bytes: u64,
    decoded_csv_sha256: String,
    maximum_raw_record_bytes: u64,
    source_columns: &'a [String],
    original_header_csv_sha256: &'a str,
    column_stats: &'a [Column],
    feature_counts: &'a BTreeMap<String, BTreeMap<String, Feature>>,
    witnesses: &'a WitnessCounts,
    witness_maximum_bytes: u64,
    witness_admission_floor_bytes: u64,
    maximum_bytes: u64,
    actual_witness_data_bytes: u64,
    actual_witness_index_bytes: u64,
    seconds: f64,
    limits: [&'static str; 6],
}

/// Consume uncompressed PostgreSQL COPY CSV stdin once. Transport verification
/// belongs to the caller; a complete decoded input alone is not a full snapshot.
pub(super) fn run(
    out: &Path,
    maximum_bytes: u64,
    witness_maximum_bytes: u64,
    observation_limit: usize,
    row_limit: Option<u64>,
) -> Result<()> {
    if maximum_bytes < RECEIPT_RESERVE
        || witness_maximum_bytes > maximum_bytes
        || observation_limit == 0
    {
        return Err("invalid census output, witness or feature-key allocation".into());
    }
    fs::create_dir_all(out)?;
    let mut summary_file = new_file(out, "summary.json")?;
    let mut progress_file = new_file(out, "progress.json")?;
    let mut witness_file = new_file(out, "witnesses.jsonl.zst")?;
    let mut index_file = new_file(out, "witness-index.jsonl")?;
    rewrite(&mut summary_file, b"{\"schema\":\"courtlistener.source-census.v1\",\"status\":\"running\",\"decoded_input_complete\":false}\n")?;
    let started = Instant::now();
    let stdin = io::stdin();
    let mut input = BufReader::with_capacity(
        1024 * 1024,
        DecodedRead {
            input: stdin.lock(),
            digest: Sha256::new(),
            bytes: 0,
        },
    );
    let mut census = Census::default();
    let result = (|| -> Result<bool> {
        let (mut raw, mut line) = (Vec::new(), Vec::new());
        if !metadata::record(&mut input, &mut raw, &mut line)? {
            return Err("source CSV header missing".into());
        }
        census.decoded_bytes = input.get_ref().bytes;
        census.header_sha256 = hash(&raw);
        census.header = metadata::fields(&raw)?
            .into_iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| "source CSV header field missing".into())
            })
            .collect::<Result<Vec<_>>>()?;
        let names = census.header.iter().collect::<HashSet<_>>();
        if names.len() != census.header.len()
            || census.header.first().map(String::as_str) != Some("id")
            || !census.header.iter().any(|name| name == "cluster_id")
            || RENDITIONS
                .iter()
                .any(|field| !census.header.iter().any(|name| name == field))
        {
            return Err("source opinion CSV header is incomplete or has duplicate columns".into());
        }
        census.columns = census
            .header
            .iter()
            .map(|name| Column {
                name: name.clone(),
                ..Column::default()
            })
            .collect();
        let rendition_columns = census
            .header
            .iter()
            .enumerate()
            .filter(|(_, name)| RENDITIONS.contains(&name.as_str()))
            .map(|(index, name)| (index, name.clone()))
            .collect::<Vec<_>>();
        let scalar_columns = census
            .header
            .iter()
            .enumerate()
            .filter(|(_, name)| matches!(name.as_str(), "type" | "per_curiam" | "extracted_by_ocr"))
            .map(|(index, name)| (index, name.clone()))
            .collect::<Vec<_>>();
        progress(
            &mut progress_file,
            &summary_file,
            &witness_file,
            &index_file,
            &census,
            &input.get_ref().digest,
            started,
            maximum_bytes,
            "running",
        )?;
        loop {
            if row_limit.is_some_and(|limit| census.rows_observed >= limit) {
                return Ok(false);
            }
            if !metadata::record(&mut input, &mut raw, &mut line)? {
                return Ok(true);
            }
            let ordinal = census.rows_read;
            add(&mut census.rows_read, 1)?;
            census.decoded_bytes = input.get_ref().bytes;
            census.maximum_raw_record_bytes = census.maximum_raw_record_bytes.max(raw.len() as u64);
            let values = metadata::fields(&raw)?;
            if values.len() != census.header.len() {
                return Err(format!("source CSV field count differs at ordinal {ordinal}").into());
            }
            // All rows are observed even after the bounded inventory fills.
            // Unindexed token events are counted, never claimed distinct.
            let mut observations = rendition_columns
                .iter()
                .filter_map(|(index, field)| {
                    values[*index]
                        .as_str()
                        .filter(|source| !source.is_empty())
                        .map(|source| (field.clone(), census_features::observe(field, source)))
                })
                .collect::<Vec<_>>();
            let mut record_features = BTreeMap::new();
            for (index, field) in &scalar_columns {
                record_features.insert(
                    format!("scalar:{field}:{}", serde_json::to_string(&values[*index])?),
                    1,
                );
            }
            let presence = rendition_columns
                .iter()
                .map(|(index, field)| {
                    let state = match values[*index].as_str() {
                        None => "null",
                        Some("") => "empty",
                        Some(_) => "nonempty",
                    };
                    format!("{field}={state}")
                })
                .collect::<Vec<_>>()
                .join(";");
            record_features.insert(format!("rendition_presence:{presence}"), 1);
            observations.push(("$record".to_owned(), record_features));
            let mut reasons = Vec::new();
            for (column, value) in census.columns.iter_mut().zip(&values) {
                let (change, bytes) = if value.is_null() {
                    let first = column.null_rows == 0;
                    add(&mut column.null_rows, 1)?;
                    (first.then_some("first_null"), None)
                } else {
                    let source = value
                        .as_str()
                        .ok_or("unexpected non-string source CSV field")?;
                    let bytes = source.len() as u64;
                    add(&mut column.string_utf8_bytes, bytes)?;
                    if source.is_empty() {
                        let first = column.empty_string_rows == 0;
                        add(&mut column.empty_string_rows, 1)?;
                        (first.then_some("first_empty_string"), Some(bytes))
                    } else {
                        add(&mut column.nonempty_string_rows, 1)?;
                        let change = if bytes > column.maximum_utf8_bytes {
                            column.maximum_utf8_bytes = bytes;
                            column.maximum_source_ordinal_zero_based = Some(ordinal);
                            Some("maximum_utf8_bytes")
                        } else if column
                            .minimum_nonempty_utf8_bytes
                            .is_none_or(|minimum| bytes < minimum)
                        {
                            Some("minimum_nonempty_utf8_bytes")
                        } else {
                            None
                        };
                        column.minimum_nonempty_utf8_bytes = Some(
                            column
                                .minimum_nonempty_utf8_bytes
                                .map_or(bytes, |minimum| minimum.min(bytes)),
                        );
                        (change, Some(bytes))
                    }
                };
                if let Some(change) = change {
                    reasons.push(RepresentationProof {
                        column: column.name.clone(),
                        change,
                        utf8_bytes: bytes,
                    });
                }
            }
            let remaining_witness_bytes = witness_maximum_bytes
                .saturating_sub(census.witnesses.compressed_bytes)
                .saturating_sub(census.witnesses.index_bytes);
            let remaining_output_bytes = maximum_bytes
                .saturating_sub(RECEIPT_RESERVE)
                .saturating_sub(census.witnesses.compressed_bytes)
                .saturating_sub(census.witnesses.index_bytes);
            let remaining = remaining_witness_bytes.min(remaining_output_bytes);
            if !census.witnesses.capture_saturated && remaining < WITNESS_ADMISSION_FLOOR {
                census.witnesses.capture_saturated = true;
                census.witnesses.remaining_bytes_when_saturated = Some(remaining);
            }
            let mut needed = Vec::new();
            let mut needs_example = false;
            for (field, tokens) in observations {
                let known = census.features.entry(field.clone()).or_default();
                let mut overflow_row = false;
                for (token, nodes) in tokens.into_iter().filter(|(_, nodes)| *nodes != 0) {
                    let feature = match known.entry(token.clone()) {
                        Entry::Occupied(entry) => Some(entry.into_mut()),
                        Entry::Vacant(entry) if census.feature_keys < observation_limit => {
                            census.feature_keys += 1;
                            census.features_without_witness += 1;
                            Some(entry.insert(Feature {
                                source_rows: 0,
                                observed_nodes: 0,
                                first_source_ordinal_zero_based: ordinal,
                                witness_ordinal: None,
                            }))
                        }
                        Entry::Vacant(_) => None,
                    };
                    let (needs_witness, retained_in_feature_inventory) = if let Some(feature) =
                        feature
                    {
                        add(&mut feature.source_rows, 1)?;
                        add(&mut feature.observed_nodes, nodes)?;
                        (feature.witness_ordinal.is_none(), true)
                    } else {
                        let overflow = census.feature_overflow.entry(field.clone()).or_default();
                        if !overflow_row {
                            add(&mut overflow.source_rows, 1)?;
                            overflow_row = true;
                        }
                        add(&mut overflow.token_observations, 1)?;
                        add(&mut overflow.observed_nodes, nodes)?;
                        overflow
                            .first_source_ordinal_zero_based
                            .get_or_insert(ordinal);
                        (true, false)
                    };
                    if needs_witness {
                        needs_example = true;
                        if !census.witnesses.capture_saturated {
                            needed.push(FeatureProof {
                                rendition: field.clone(),
                                token,
                                observed_nodes: nodes,
                                retained_in_feature_inventory,
                            });
                        }
                    }
                }
            }
            add(&mut census.rows_observed, 1)?;
            if needs_example || !reasons.is_empty() {
                add(&mut census.witnesses.requested_rows, 1)?;
                if census.witnesses.capture_saturated {
                    add(&mut census.witnesses.omitted_rows, 1)?;
                } else {
                    let selected = Witness {
                        schema: "courtlistener.source-census-witness.v1",
                        source_ordinal_zero_based: ordinal,
                        columns: &census.header,
                        values: &values,
                        original_csv_row_bytes: raw.len(),
                        original_csv_row_sha256: hash(&raw),
                        source_string_hashes: census
                            .header
                            .iter()
                            .zip(&values)
                            .filter_map(|(field, value)| {
                                value.as_str().map(|source| {
                                    (
                                        field.as_str(),
                                        SourceHash {
                                            utf8_bytes: source.len(),
                                            sha256: hash(source.as_bytes()),
                                        },
                                    )
                                })
                            })
                            .collect(),
                    };
                    let mut encoded = serde_json::to_vec(&selected)?;
                    encoded.push(b'\n');
                    census.witnesses.maximum_attempted_row_json_bytes = census
                        .witnesses
                        .maximum_attempted_row_json_bytes
                        .max(encoded.len() as u64);
                    let frame = zstd::bulk::compress(&encoded, 3)?;
                    let witness_ordinal = census.witnesses.retained_rows;
                    let mut index = serde_json::to_vec(&serde_json::json!({
                        "schema":"courtlistener.source-census-witness-index.v1",
                        "witness_ordinal":witness_ordinal,"source_ordinal_zero_based":ordinal,
                        "compressed_offset":census.witnesses.compressed_bytes,
                        "compressed_bytes":frame.len(),"raw_bytes":encoded.len(),"raw_sha256":hash(&encoded),
                        "feature_proofs":needed,"representation_proofs":reasons
                    }))?;
                    index.push(b'\n');
                    if fits(
                        witness_maximum_bytes,
                        &[
                            census.witnesses.compressed_bytes,
                            census.witnesses.index_bytes,
                            frame.len() as u64,
                            index.len() as u64,
                        ],
                    ) && fits(
                        maximum_bytes,
                        &[
                            census.witnesses.compressed_bytes,
                            census.witnesses.index_bytes,
                            frame.len() as u64,
                            index.len() as u64,
                            RECEIPT_RESERVE,
                        ],
                    ) {
                        witness_file.write_all(&frame)?;
                        index_file.write_all(&index)?;
                        add(&mut census.witnesses.compressed_bytes, frame.len() as u64)?;
                        add(&mut census.witnesses.index_bytes, index.len() as u64)?;
                        add(
                            &mut census.witnesses.uncompressed_json_bytes,
                            encoded.len() as u64,
                        )?;
                        add(&mut census.witnesses.retained_rows, 1)?;
                        for proof in needed {
                            if proof.retained_in_feature_inventory {
                                census
                                    .features
                                    .get_mut(&proof.rendition)
                                    .unwrap()
                                    .get_mut(&proof.token)
                                    .unwrap()
                                    .witness_ordinal = Some(witness_ordinal);
                                census.features_without_witness -= 1;
                            }
                        }
                    } else {
                        add(&mut census.witnesses.omitted_rows, 1)?;
                    }
                }
            }
            if census.rows_observed % 10_000 == 0 {
                witness_file.flush()?;
                index_file.flush()?;
                progress(
                    &mut progress_file,
                    &summary_file,
                    &witness_file,
                    &index_file,
                    &census,
                    &input.get_ref().digest,
                    started,
                    maximum_bytes,
                    "running",
                )?;
            }
        }
    })();
    census.decoded_bytes = input.get_ref().bytes;
    let mut failure = result.as_ref().err().map(ToString::to_string);
    let complete = matches!(result, Ok(true));
    let data_bytes = witness_file.metadata()?.len();
    let index_bytes = index_file.metadata()?.len();
    let empty_features = BTreeMap::new();
    let mut omitted = false;
    let make_summary = |failure: Option<&str>, omitted: bool| {
        Summary {
        schema: SCHEMA,
        status: if failure.is_some() { "failed" } else if complete { "complete_decoded_input" } else { "row_limited" },
        decoded_input_complete: complete && failure.is_none(),
        full_snapshot_verified_by_this_module: false,
        failure: failure.map(str::to_owned),
        scope: "Decoded CSV rows consumed from stdin; full snapshot coverage additionally requires the caller's pinned complete compressed-source transport, decompressor and EOF receipts.",
        feature_scope: census_features::SCOPE,
        feature_key_scope: "Distinct (rendition, token) pairs, counted globally; $record counts exact JSON scalar states for type/per_curiam/extracted_by_ocr and eight-rendition null/empty/nonempty combinations once per row",
        observation_limit, feature_keys: census.feature_keys, feature_inventory_omitted_due_to_output_cap: omitted,
        feature_inventory_complete: complete && failure.is_none() && !omitted && census.feature_overflow.is_empty(),
        feature_overflow: &census.feature_overflow,
        features_without_retained_witness: census.features_without_witness,
        row_limit, records_read: census.rows_read, records_observed: census.rows_observed,
        decoded_csv_bytes: census.decoded_bytes, decoded_csv_sha256: format!("{:x}", input.get_ref().digest.clone().finalize()),
        maximum_raw_record_bytes: census.maximum_raw_record_bytes, source_columns: &census.header,
        original_header_csv_sha256: &census.header_sha256, column_stats: &census.columns,
        feature_counts: if omitted { &empty_features } else { &census.features },
        witnesses: &census.witnesses, witness_maximum_bytes,
        witness_admission_floor_bytes: WITNESS_ADMISSION_FLOOR, maximum_bytes,
        actual_witness_data_bytes: data_bytes, actual_witness_index_bytes: index_bytes,
        seconds: started.elapsed().as_secs_f64(),
        limits: ["Ordinals count source rows; global opinion-ID uniqueness is not indexed",
            "All original columns are retained in witnesses; feature extraction addresses the eight named rendition columns",
            "Below the stated remaining witness admission floor capture stops; omissions and unwitnessed features remain counted while census continues",
            "The feature ceiling limits exact per-token inventory, not input coverage; overflow counts row/token-observation/node events without deduplication or distinct-key claims, with exact token proofs in admitted row witnesses; missing-exemplar counts cover tracked keys only",
            "Decoded hash covers all bytes successfully read, including buffered read-ahead on row-limited or failed runs; complete EOF covers the exact decoded stream",
            "A failed run or row limit is incomplete; source observations do not prove parser behavior or browser landing"],
    }
    };
    let mut summary = serde_json::to_vec(&make_summary(failure.as_deref(), omitted))?;
    summary.push(b'\n');
    if !fits(
        maximum_bytes,
        &[
            data_bytes,
            index_bytes,
            summary.len() as u64,
            RECEIPT_RESERVE,
        ],
    ) {
        failure = Some("final feature inventory exceeds total output cap".to_owned());
        omitted = true;
        summary = serde_json::to_vec(&make_summary(failure.as_deref(), omitted))?;
        summary.push(b'\n');
    }
    if !fits(
        maximum_bytes,
        &[
            data_bytes,
            index_bytes,
            summary.len() as u64,
            RECEIPT_RESERVE,
        ],
    ) {
        return Err("even the incomplete census receipt exceeds total output cap".into());
    }
    rewrite(&mut summary_file, &summary)?;
    witness_file.flush()?;
    index_file.flush()?;
    progress(
        &mut progress_file,
        &summary_file,
        &witness_file,
        &index_file,
        &census,
        &input.get_ref().digest,
        started,
        maximum_bytes,
        if failure.is_some() {
            "failed"
        } else if complete {
            "complete_decoded_input"
        } else {
            "row_limited"
        },
    )?;
    if let Some(failure) = failure {
        return Err(failure.into());
    }
    Ok(())
}
