use crate::{each_record, hash, Index, Result};
use serde::{Deserialize, Serialize};
use serde_json::{
    json,
    value::{to_raw_value, RawValue},
    Map, Value,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

const MEMBERSHIP_SCHEMA: &str = "courtlistener.cluster-membership.v1";
const MEMBERSHIP_FILE: &str = "cluster-membership.json";
const JOINED_SCHEMA: &str = "courtlistener.joined-metadata.v2";

// Retain complete serialized rows instead of a Value and allocation per cell.
// Joined-v2 is unchanged; validation and lookup expand one row at a time.
#[derive(Deserialize, Serialize)]
struct Table {
    columns: Vec<String>,
    rows: BTreeMap<String, Box<RawValue>>,
}

#[derive(Deserialize, Serialize)]
struct CitationEvidence {
    #[serde(flatten)]
    header: Map<String, Value>,
    rows_by_cluster: BTreeMap<String, Vec<Box<RawValue>>>,
}

#[derive(Deserialize, Serialize)]
pub struct JoinedMetadata {
    #[serde(flatten)]
    header: Map<String, Value>,
    clusters: Table,
    dockets: Table,
    courts: Table,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_citations"
    )]
    citations: Option<CitationEvidence>,
}

fn deserialize_citations<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<CitationEvidence>, D::Error> {
    // Only an absent table is None; a present null/malformed table is an error.
    CitationEvidence::deserialize(deserializer).map(Some)
}

impl JoinedMetadata {
    fn header(&self, name: &str) -> &Value {
        self.header.get(name).unwrap_or(&Value::Null)
    }

    fn citation_header(&self, name: &str) -> &Value {
        self.citations
            .as_ref()
            .and_then(|citations| citations.header.get(name))
            .unwrap_or(&Value::Null)
    }
}

#[derive(Default, Deserialize, Serialize)]
pub struct ClusterMembership {
    schema: String,
    index_sha256: String,
    rows: usize,
    cluster_ids: BTreeSet<String>,
    opinions_missing_cluster_id: BTreeSet<String>,
}

impl ClusterMembership {
    pub fn record(&mut self, cluster_id: Option<&str>, opinion_id: &str) {
        self.rows += 1;
        if let Some(cluster_id) = cluster_id.filter(|id| !id.is_empty()) {
            self.cluster_ids.insert(cluster_id.to_owned());
        } else {
            self.opinions_missing_cluster_id
                .insert(opinion_id.to_owned());
        }
    }

    pub fn persist(&mut self, pack: &Path, index_bytes: &[u8]) -> Result<()> {
        self.schema = MEMBERSHIP_SCHEMA.to_owned();
        self.index_sha256 = hash(index_bytes);
        let destination = pack.join(MEMBERSHIP_FILE);
        let pending = destination.with_extension("pending");
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&pending)?;
        let result = (|| -> Result<()> {
            serde_json::to_writer_pretty(&mut file, self)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&pending, destination)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(pending);
        }
        result
    }
}

pub(crate) fn freeze_pool(pack: &Path, out: Option<&Path>) -> Result<Option<(Value, Value)>> {
    if pack.join("index.json").exists() {
        return Ok(None);
    }
    let bytes = fs::read(pack.join("source.json"))?;
    let manifest: Value = serde_json::from_slice(&bytes)?;
    if manifest["schema"] != "courtlistener.acquired-pack-pool.v1" {
        return Err("unrecognized corpus pool".into());
    }
    let mut frozen_manifest = None;
    if let Some(out) = out {
        let destination = out.with_extension("source.json");
        let mut file = match File::options()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&destination)? != bytes {
                    return Err("existing frozen opinion source differs from invocation".into());
                }
                None
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(file) = file.as_mut() {
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        frozen_manifest = destination
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
    }
    let descriptor = json!({"kind":"pool_manifest","manifest_sha256":hash(&bytes),
        "manifest_bytes":bytes.len(),"frozen_manifest":frozen_manifest,
        "parts":manifest["parts"].as_array().ok_or("pool parts missing")?.len(),
        "opinion_rows":manifest["opinion_rows"],"source":manifest["source"]});
    Ok(Some((manifest, descriptor)))
}

fn memberships(
    pack: &Path,
    expected_index: Option<&str>,
    pool: Option<&Value>,
    part_path: &str,
    wanted: &mut HashSet<String>,
    missing: &mut BTreeSet<String>,
    seen: &mut HashSet<String>,
    sources: &mut Vec<Value>,
) -> Result<usize> {
    let index_path = pack.join("index.json");
    if !index_path.exists() {
        if expected_index.is_some() {
            return Err("pool part index missing".into());
        }
        let source = pool.ok_or("frozen corpus pool source missing")?;
        if source["schema"] != "courtlistener.acquired-pack-pool.v1" {
            return Err("unrecognized corpus pool".into());
        }
        let mut rows = 0;
        for part in source["parts"].as_array().ok_or("pool parts missing")? {
            let name = part["path"].as_str().ok_or("part path missing")?;
            let path = Path::new(name);
            if path.is_absolute()
                || path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err("part path outside pool".into());
            }
            rows += memberships(
                &pack.join(path),
                Some(part["index_sha256"].as_str().ok_or("part digest missing")?),
                None,
                name,
                wanted,
                missing,
                seen,
                sources,
            )?;
        }
        if rows != source["opinion_rows"].as_u64().unwrap_or(0) as usize {
            return Err("pool count differs from acquired source".into());
        }
        return Ok(rows);
    }
    let index_bytes = fs::read(index_path)?;
    let index_sha256 = hash(&index_bytes);
    if expected_index.is_some_and(|expected| expected != index_sha256) {
        return Err("pool part index digest mismatch".into());
    }
    let index: Index = serde_json::from_slice(&index_bytes)?;
    if index.schema != "courtlistener.lossless-zstd-frames.v1"
        || index
            .frames
            .iter()
            .map(|frame| frame.ids.len())
            .sum::<usize>()
            != index.rows
        || pack.join("opinions.jsonl.zst").metadata()?.len() != index.compressed_bytes
    {
        return Err("pack identity, size or membership count mismatch".into());
    }
    let ids = index
        .frames
        .iter()
        .flat_map(|frame| &frame.ids)
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if ids.len() != index.rows || ids.iter().any(|id| !seen.insert((*id).to_owned())) {
        return Err("duplicate opinion across corpus frames or parts".into());
    }
    let cache_path = pack.join(MEMBERSHIP_FILE);
    let mut backfilled = false;
    let membership: ClusterMembership = match File::open(&cache_path) {
        Ok(file) => serde_json::from_reader(file)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut membership = ClusterMembership::default();
            each_record(pack, |row| {
                membership.record(
                    row["opinion"]["cluster_id"].as_str(),
                    row["opinion"]["id"].as_str().ok_or("source id missing")?,
                );
                Ok(())
            })?;
            membership.persist(pack, &index_bytes)?;
            backfilled = true;
            membership
        }
        Err(error) => return Err(error.into()),
    };
    if membership.schema != MEMBERSHIP_SCHEMA
        || membership.index_sha256 != index_sha256
        || membership.rows != index.rows
        || membership.cluster_ids.iter().any(String::is_empty)
        || membership.cluster_ids.len() + membership.opinions_missing_cluster_id.len() > index.rows
        || membership
            .opinions_missing_cluster_id
            .iter()
            .any(|id| !ids.contains(id.as_str()))
    {
        return Err("cluster membership cache differs from source index".into());
    }
    sources.push(json!({"path":part_path,"index_sha256":index_sha256,"rows":index.rows,
        "cache_sha256":hash(&fs::read(cache_path)?),"backfilled":backfilled,
        "clusters":membership.cluster_ids.len(),"opinions_missing_cluster_id":membership.opinions_missing_cluster_id.len()}));
    wanted.extend(membership.cluster_ids);
    missing.extend(membership.opinions_missing_cluster_id);
    Ok(index.rows)
}

pub fn export_membership(pack: &Path, out: &Path) -> Result<()> {
    let started = Instant::now();
    let frozen = freeze_pool(pack, None)?;
    let mut wanted = HashSet::new();
    let mut missing = BTreeSet::new();
    let mut sources = Vec::new();
    let rows = memberships(
        pack,
        None,
        frozen.as_ref().map(|(manifest, _)| manifest),
        ".",
        &mut wanted,
        &mut missing,
        &mut HashSet::new(),
        &mut sources,
    )?;
    let opinion_source = frozen.map(|(_, descriptor)| descriptor).unwrap_or_else(||
        json!({"kind":"pack_index","index_file":"index.json","index_sha256":sources[0]["index_sha256"],"opinion_rows":rows}));
    let clusters = wanted.into_iter().collect::<BTreeSet<_>>();
    let value = json!({"schema":"courtlistener.cluster-membership-export.v1",
        "source_rows":rows,"clusters":clusters.len(),"cluster_ids":clusters,
        "opinions_missing_cluster_id":missing,"source_membership":sources,"opinion_source":opinion_source,
        "seconds":started.elapsed().as_secs_f64()});
    let raw = serde_json::to_vec(&value)?;
    let mut file = File::options().write(true).create_new(true).open(out)?;
    file.write_all(&raw)?;
    file.write_all(b"\n")?;
    eprintln!(
        "cluster membership export: {rows} rows; {} clusters; {} JSON bytes; {}s",
        value["clusters"],
        raw.len() + 1,
        value["seconds"]
    );
    Ok(())
}

// PostgreSQL COPY CSV: backslash escapes inside quoted fields; unquoted empty
// fields are NULL, quoted empty fields are strings. Preserve every selected column.
pub(super) fn record(reader: &mut impl BufRead, raw: &mut Vec<u8>, line: &mut Vec<u8>) -> Result<bool> {
    raw.clear();
    let (mut quoted, mut escaped) = (false, false);
    loop {
        line.clear();
        if reader.read_until(b'\n', line)? == 0 {
            if quoted || escaped {
                return Err("truncated quoted CSV record".into());
            }
            return Ok(!raw.is_empty());
        }
        let mut cursor = 0;
        while cursor < line.len() {
            if escaped {
                escaped = false;
                cursor += 1;
                continue;
            }
            let next = if quoted {
                memchr::memchr2(b'\\', b'"', &line[cursor..])
            } else {
                memchr::memchr(b'"', &line[cursor..])
            };
            let Some(next) = next else {
                break;
            };
            cursor += next;
            if line[cursor] == b'\\' {
                escaped = true;
            } else {
                quoted = !quoted;
            }
            cursor += 1;
        }
        raw.extend_from_slice(line);
        if !quoted && !escaped {
            return Ok(true);
        }
    }
}

pub(super) fn fields(raw: &[u8]) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    let mut value = Vec::new();
    let (mut quoted, mut was_quoted, mut escaped) = (false, false, false);
    let mut cursor = 0;
    while cursor < raw.len() {
        if quoted && !escaped {
            let end = memchr::memchr2(b'\\', b'"', &raw[cursor..])
                .map_or(raw.len(), |offset| cursor + offset);
            value.extend_from_slice(&raw[cursor..end]);
            cursor = end;
            if cursor == raw.len() {
                break;
            }
        }
        let byte = raw[cursor];
        cursor += 1;
        if escaped {
            value.push(byte);
            escaped = false;
        } else if quoted && byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
            was_quoted = true;
        } else if !quoted && matches!(byte, b',' | b'\n') {
            result.push(if !was_quoted && value.is_empty() {
                Value::Null
            } else {
                Value::String(String::from_utf8(std::mem::take(&mut value))?)
            });
            value.clear();
            was_quoted = false;
            if byte == b'\n' {
                return Ok(result);
            }
        } else if !quoted && byte == b'\r' {
        } else {
            value.push(byte);
        }
    }
    if quoted || escaped {
        return Err("truncated CSV field".into());
    }
    result.push(if !was_quoted && value.is_empty() {
        Value::Null
    } else {
        Value::String(String::from_utf8(value)?)
    });
    Ok(result)
}

fn csv(
    path: &Path,
    bzip: &str,
    mut visit: impl FnMut(&[String], &[u8], usize) -> Result<bool>,
) -> Result<(Vec<String>, usize)> {
    let mut command = Command::new(bzip);
    command.args(["-dc"]).arg(path).stdout(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    let result = (|| {
        let mut reader = BufReader::with_capacity(
            1024 * 1024,
            child.stdout.take().ok_or("decompressor output missing")?,
        );
        let (mut raw, mut line) = (Vec::new(), Vec::new());
        if !record(&mut reader, &mut raw, &mut line)? {
            return Err("CSV header missing".into());
        }
        let header = fields(&raw)?
            .into_iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("CSV header field missing".into())
            })
            .collect::<Result<Vec<_>>>()?;
        if header.first().map(String::as_str) != Some("id") {
            return Err("unexpected metadata primary key".into());
        }
        let mut retained = 0usize;
        let mut scanned = 0usize;
        while record(&mut reader, &mut raw, &mut line)? {
            scanned += 1;
            if visit(&header, &raw, scanned)? {
                retained += 1;
            }
            if scanned % 1_000_000 == 0 {
                eprintln!(
                    "metadata {}: scanned {scanned}; retained {}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    retained
                );
            }
        }
        Ok((header, scanned))
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    if !status.success() && result.is_ok() {
        return Err("metadata decompression failed".into());
    }
    result
}

fn table(path: &Path, bzip: &str, wanted: Option<&HashSet<String>>) -> Result<Table> {
    let mut retained = BTreeMap::new();
    let (columns, _) = csv(path, bzip, |header, raw, _| {
        let end = raw
            .iter()
            .position(|b| *b == b',')
            .ok_or("metadata id missing")?;
        let id = std::str::from_utf8(&raw[..end])?.trim_matches('"');
        if wanted.is_some_and(|ids| !ids.contains(id)) {
            return Ok(false);
        }
        let values = fields(raw)?;
        if values.len() != header.len() {
            return Err("ragged metadata CSV record".into());
        }
        if values[0].as_str() != Some(id) {
            return Err("metadata primary key differs from source".into());
        }
        if retained
            .insert(id.to_owned(), to_raw_value(&values)?)
            .is_some()
        {
            return Err("duplicate metadata id".into());
        }
        Ok(true)
    })?;
    Ok(Table {
        columns,
        rows: retained,
    })
}

pub fn lookup(joined: &JoinedMetadata, table: &str, id: &str) -> Result<Value> {
    let table = match table {
        "clusters" => &joined.clusters,
        "dockets" => &joined.dockets,
        "courts" => &joined.courts,
        _ => return Err("unrecognized metadata table".into()),
    };
    let Some(row) = table.rows.get(id) else {
        return Ok(Value::Null);
    };
    let values: Vec<Value> = serde_json::from_str(row.get())?;
    Ok(Value::Object(
        table.columns.iter().cloned().zip(values).collect(),
    ))
}

pub fn citation_metadata(joined: &JoinedMetadata, cluster: &str) -> Result<Option<Value>> {
    let Some(citations) = &joined.citations else {
        return Ok(None);
    };
    let rows = citations
        .rows_by_cluster
        .get(cluster)
        .into_iter()
        .flatten()
        .map(|row| serde_json::from_str::<Value>(row.get()))
        .collect::<serde_json::Result<Vec<_>>>()?;
    Ok(Some(json!({
        "columns":joined.citation_header("columns"),"rows":rows,
        "cap_order":joined.citation_header("cap_order"),"source":joined.citation_header("source")
    })))
}

fn source_path(source: &Path, receipt: &Value) -> Result<PathBuf> {
    let name = Path::new(
        receipt["path"]
            .as_str()
            .ok_or("complete metadata source missing")?,
    );
    if name.is_absolute()
        || name
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("metadata source path outside source directory".into());
    }
    let path = source.join(name);
    if path.metadata()?.len()
        != receipt["bytes"]
            .as_u64()
            .ok_or("metadata byte count missing")?
    {
        return Err("metadata source size changed".into());
    }
    Ok(path)
}

fn stream_hash(mut reader: impl Read) -> Result<(u64, String)> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    let mut count = 0u64;
    loop {
        let bytes = reader.read(&mut buffer)?;
        if bytes == 0 {
            return Ok((count, format!("{:x}", digest.finalize())));
        }
        count += bytes as u64;
        digest.update(&buffer[..bytes]);
    }
}

fn file_hash(path: &Path) -> Result<String> {
    Ok(stream_hash(File::open(path)?)?.1)
}

fn citation_evidence(
    source: &Path,
    receipt: &Value,
    wanted: &HashSet<String>,
    bzip: &str,
) -> Result<CitationEvidence> {
    let started = Instant::now();
    let path = source_path(source, receipt)?;
    let source_sha256 = file_hash(&path)?;
    if receipt["sha256"].as_str() != Some(source_sha256.as_str()) {
        return Err("citation source digest mismatch".into());
    }
    let mut rows_by_cluster = BTreeMap::<String, Vec<Box<RawValue>>>::new();
    let mut ids = HashSet::new();
    let mut retained_raw_csv_bytes = 0usize;
    let mut cluster_column = None;
    let (columns, scanned_rows) = csv(&path, bzip, |header, raw, source_ordinal| {
        let column = match cluster_column {
            Some(column) => column,
            None => {
                let column = header
                    .iter()
                    .position(|name| name == "cluster_id")
                    .ok_or("citation cluster column missing")?;
                cluster_column = Some(column);
                column
            }
        };
        let values = fields(raw)?;
        if values.len() != header.len() {
            return Err("ragged citation CSV record".into());
        }
        let Some(cluster_id) = values[column].as_str().filter(|id| wanted.contains(*id)) else {
            return Ok(false);
        };
        let cluster_id = cluster_id.to_owned();
        let id = values[0].as_str().ok_or("citation id missing")?;
        if !ids.insert(id.to_owned()) {
            return Err("duplicate selected citation id".into());
        }
        let mut row = json!({"source_ordinal":source_ordinal,"raw_csv":std::str::from_utf8(raw)?});
        row["original"] = Value::Array(values);
        rows_by_cluster
            .entry(cluster_id)
            .or_default()
            .push(to_raw_value(&row)?);
        retained_raw_csv_bytes += raw.len();
        Ok(true)
    })?;
    let Value::Object(header) = json!({"source":receipt,"verified_sha256":source_sha256,"columns":columns,
        "cap_order":{"status":"unknown","reason":"bulk_csv_has_no_cap_citation_index"},
        "source_ordinal_basis":"One-based CSV record after the header; not CAP citation-index or reporter ordering",
        "selected_clusters":wanted.len(),"selected_clusters_without_rows":wanted.len()-rows_by_cluster.len(),
        "scanned_rows":scanned_rows,"retained_rows":ids.len(),"retained_clusters":rows_by_cluster.len(),
        "retained_raw_csv_bytes":retained_raw_csv_bytes,"seconds":started.elapsed().as_secs_f64()}) else {
            unreachable!()
        };
    Ok(CitationEvidence {
        header,
        rows_by_cluster,
    })
}

struct HashingWriter<W> {
    inner: W,
    bytes: u64,
    digest: Sha256,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            bytes: 0,
            digest: Sha256::new(),
        }
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.bytes += written as u64;
        self.digest.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn write_joined(value: &JoinedMetadata, out: &Path, new_output: bool) -> Result<()> {
    let output = if new_output {
        File::options().write(true).create_new(true).open(out)?
    } else {
        File::create(out)?
    };
    let encoder = zstd::stream::write::Encoder::new(HashingWriter::new(output), 9)?;
    let mut writer = BufWriter::with_capacity(1024 * 1024, HashingWriter::new(encoder));
    serde_json::to_writer(&mut writer, value)?;
    writer.flush()?;
    let raw = writer.into_inner().map_err(|error| error.into_error())?;
    let raw_bytes = raw.bytes;
    let raw_sha256 = format!("{:x}", raw.digest.finalize());
    let compressed = raw.inner.finish()?;
    compressed.inner.sync_all()?;
    let compressed_bytes = compressed.bytes;
    let output_sha256 = format!("{:x}", compressed.digest.finalize());
    drop(compressed.inner);
    let (reconstructed_raw_bytes, reconstructed_raw_sha256) =
        stream_hash(zstd::stream::read::Decoder::new(File::open(out)?)?)?;
    if reconstructed_raw_bytes != raw_bytes || reconstructed_raw_sha256 != raw_sha256 {
        return Err("metadata reconstruction differs from serialized source".into());
    }
    let receipt = json!({"schema":"courtlistener.joined-metadata-output.v1",
        "joined_schema":value.header("schema"),
        "output":out.file_name().map(|name|name.to_string_lossy()),
        "raw_bytes":raw_bytes,"raw_sha256":raw_sha256,"compressed_bytes":compressed_bytes,"output_sha256":output_sha256,
        "reconstructed_raw_bytes":reconstructed_raw_bytes,"reconstructed_raw_sha256":reconstructed_raw_sha256,
        "matched_clusters":value.clusters.rows.len(),"matched_dockets":value.dockets.rows.len(),
        "matched_courts":value.courts.rows.len(),
        "citation_rows":value.citations.as_ref().map(|citations|citations.rows_by_cluster.values().map(Vec::len).sum::<usize>()),
        "citation_clusters":value.citations.as_ref().map(|citations|citations.rows_by_cluster.len()),
        "citation_raw_csv_bytes":value.citation_header("retained_raw_csv_bytes"),
        "citation_source":value.citation_header("source"),
        "opinion_source":value.header("opinion_source"),"source_membership":value.header("source_membership"),
        "input_metadata_sha256":if value.header("metadata_subset").is_null() {
            &value.header("citation_enrichment")["input_metadata_sha256"]
        } else { &value.header("metadata_subset")["input_context"]["sha256"] },
        "metadata_subset":value.header("metadata_subset")});
    let receipt_path = out.with_extension("receipt.json");
    let mut receipt_file = if new_output {
        File::options()
            .write(true)
            .create_new(true)
            .open(receipt_path)?
    } else {
        File::create(receipt_path)?
    };
    serde_json::to_writer_pretty(&mut receipt_file, &receipt)?;
    receipt_file.write_all(b"\n")?;
    eprintln!("joined metadata: {} clusters, {} dockets; {} citation rows across {} clusters; {} raw bytes; {} compressed bytes",
        value.header("matched_clusters"), value.header("matched_dockets"), value.citation_header("retained_rows"),
        value.citation_header("retained_clusters"), raw_bytes, compressed_bytes);
    Ok(())
}

pub fn collect(pack: &Path, source: &Path, out: &Path, bzip: &str) -> Result<()> {
    let started = Instant::now();
    let frozen = freeze_pool(pack, Some(out))?;
    let mut wanted = HashSet::new();
    let mut missing = BTreeSet::new();
    let mut membership_sources = Vec::new();
    let rows = memberships(
        pack,
        None,
        frozen.as_ref().map(|(manifest, _)| manifest),
        ".",
        &mut wanted,
        &mut missing,
        &mut HashSet::new(),
        &mut membership_sources,
    )?;
    let opinion_source = frozen.map(|(_, descriptor)| descriptor).unwrap_or_else(||
        json!({"kind":"pack_index","index_file":"index.json","index_sha256":membership_sources[0]["index_sha256"],"opinion_rows":rows}));
    let membership_seconds = started.elapsed().as_secs_f64();
    eprintln!(
        "cluster membership: {rows} source rows; {} clusters; {} missing; {membership_seconds:.3}s",
        wanted.len(),
        missing.len()
    );
    let receipts: Value = serde_json::from_reader(File::open(source.join("source.json"))?)?;
    let path = |kind: &str| source_path(source, &receipts[kind]);
    let clusters = table(&path("opinion-clusters")?, bzip, Some(&wanted))?;
    let docket_column = clusters
        .columns
        .iter()
        .position(|column| column == "docket_id")
        .ok_or("cluster docket column missing")?;
    let mut docket_ids = HashSet::new();
    for row in clusters.rows.values() {
        let values: Vec<Value> = serde_json::from_str(row.get())?;
        if let Some(id) = values[docket_column].as_str() {
            docket_ids.insert(id.to_owned());
        }
    }
    let dockets = table(&path("dockets")?, bzip, Some(&docket_ids))?;
    drop(docket_ids);
    let courts = table(&path("courts")?, bzip, None)?;
    let citations = citation_evidence(source, &receipts["citations"], &wanted, bzip)?;
    let Value::Object(header) = json!({"schema":JOINED_SCHEMA,"source":receipts,
      "source_membership":membership_sources,"opinion_source":opinion_source,"membership_seconds":membership_seconds,
      "source_rows":rows,"opinions_missing_cluster_id":missing,
      "wanted_clusters":wanted.len(),"matched_clusters":clusters.rows.len(),"matched_dockets":dockets.rows.len(),
      "seconds":started.elapsed().as_secs_f64()}) else {
        unreachable!()
    };
    drop(wanted);
    let value = JoinedMetadata {
        header,
        clusters,
        dockets,
        courts,
        citations: Some(citations),
    };
    write_joined(&value, out, false)
}

pub fn enrich_citations(joined: &Path, source: &Path, out: &Path, bzip: &str) -> Result<()> {
    if out.exists() || out.with_extension("receipt.json").exists() {
        return Err("citation enrichment requires new output and receipt paths".into());
    }
    let started = Instant::now();
    let input_metadata_sha256 = file_hash(joined)?;
    let mut value = read(joined)?;
    let receipts: Value = serde_json::from_reader(File::open(source.join("source.json"))?)?;
    if value.header("source")["citations"] != receipts["citations"] {
        return Err("citation snapshot differs from joined metadata source".into());
    }
    let wanted = value.clusters.rows.keys().cloned().collect();
    value.citations = None;
    value.citations = Some(citation_evidence(source, &receipts["citations"], &wanted, bzip)?);
    drop(wanted);
    value.header.insert(
        "citation_enrichment".to_owned(),
        json!({"input_metadata_sha256":input_metadata_sha256,
            "seconds":started.elapsed().as_secs_f64()}),
    );
    write_joined(&value, out, true)
}

fn materialize_row(table: &mut Table, id: &str, row: &Value) -> Result<()> {
    let object = row.as_object().ok_or("cached metadata row is not an object")?;
    if object.len() != table.columns.len() || row["id"].as_str() != Some(id) {
        return Err("cached metadata differs from source columns or related id".into());
    }
    let cells = table.columns.iter().map(|column| {
        object.get(column).cloned().ok_or_else(|| "cached metadata column missing".into())
    }).collect::<Result<Vec<Value>>>()?;
    if let Some(previous) = table.rows.get(id) {
        if serde_json::from_str::<Vec<Value>>(previous.get())? != cells {
            return Err(format!("conflicting cached metadata for id {id}").into());
        }
    } else {
        table.rows.insert(id.to_owned(), to_raw_value(&cells)?);
    }
    Ok(())
}

pub fn selected(input: &Path, candidates_path: &Path, out: &Path) -> Result<()> {
    if out.exists() || out.with_extension("receipt.json").exists() {
        return Err("metadata selection requires new output and receipt paths".into());
    }
    let started = Instant::now();
    let selection_bytes = fs::read(candidates_path)?;
    let candidates: Vec<Value> = serde_json::from_slice(&selection_bytes)?;
    let selection = json!({"file":candidates_path.file_name().map(|name|name.to_string_lossy()),
        "sha256":hash(&selection_bytes),"bytes":selection_bytes.len(),"opinions":candidates.len()});
    drop(selection_bytes);
    let mut opinions = HashSet::new();
    let mut wanted = HashSet::new();
    for candidate in &candidates {
        let opinion = candidate["opinion_id"].as_str().filter(|id| !id.is_empty())
            .ok_or("selected opinion id missing")?;
        if !opinions.insert(opinion.to_owned()) {
            return Err("duplicate selected opinion id".into());
        }
        wanted.insert(candidate["cluster_id"].as_str().filter(|id| !id.is_empty())
            .ok_or("selected cluster id missing")?.to_owned());
    }
    drop(opinions);
    let (input_bytes, input_sha256) = stream_hash(File::open(input)?)?;
    let mut value = read(input)?;
    let input_context = json!({"file":input.file_name().map(|name|name.to_string_lossy()),
        "sha256":input_sha256,"bytes":input_bytes,
        "counts":{"source_rows":value.header("source_rows"),"wanted_clusters":value.header("wanted_clusters"),
            "clusters":value.clusters.rows.len(),"dockets":value.dockets.rows.len(),"courts":value.courts.rows.len(),
            "citation_rows":value.citations.as_ref().map(|citations|citations.rows_by_cluster.values().map(Vec::len).sum::<usize>()),
            "citation_clusters":value.citations.as_ref().map(|citations|citations.rows_by_cluster.len()),
            "citation_raw_csv_bytes":value.citation_header("retained_raw_csv_bytes")}});
    let base_context = value.header("metadata_subset").clone();
    let mut attached_metadata_candidates = 0usize;
    let mut absent_dockets = HashSet::new();
    let mut absent_courts = HashSet::new();
    for candidate in &candidates {
        if ["cluster_metadata", "docket_metadata", "court_metadata", "citation_metadata"]
            .iter().all(|field| candidate.get(*field).is_none()) {
            continue;
        }
        let cluster = candidate["cluster_id"].as_str().unwrap();
        let cluster_known = value.clusters.rows.contains_key(cluster);
        let cached_cluster = candidate.get("cluster_metadata").ok_or("cached cluster metadata missing")?;
        let cached_docket = candidate.get("docket_metadata").ok_or("cached docket metadata missing")?;
        let cached_court = candidate.get("court_metadata").ok_or("cached court metadata missing")?;
        materialize_row(&mut value.clusters, cluster, cached_cluster)?;
        if cached_docket.is_null() {
            if !cached_court.is_null() {
                return Err("cached court has no related docket".into());
            }
            if let Some(id) = cached_cluster["docket_id"].as_str() {
                absent_dockets.insert(id.to_owned());
            }
        } else {
            let docket = cached_cluster["docket_id"].as_str().ok_or("cached docket has no related id")?;
            materialize_row(&mut value.dockets, docket, cached_docket)?;
            if cached_court.is_null() {
                if let Some(id) = cached_docket["court_id"].as_str() {
                    absent_courts.insert(id.to_owned());
                }
            } else {
                let court = cached_docket["court_id"].as_str().ok_or("cached court has no related id")?;
                if !value.courts.rows.contains_key(court) {
                    return Err("cached court absent from authoritative input courts".into());
                }
                materialize_row(&mut value.courts, court, cached_court)?;
            }
        }
        match (candidate.get("citation_metadata"), &mut value.citations) {
            (None, None) => {}
            (Some(cached), Some(citations)) => {
                let object = cached.as_object().ok_or("cached citation evidence is not an object")?;
                if object.len() != 4 || ["columns", "cap_order", "source"].iter().any(|field| {
                    object.get(*field) != citations.header.get(*field)
                }) {
                    return Err("cached citation columns, order or source differ from input".into());
                }
                let rows = cached["rows"].as_array().ok_or("cached citation rows missing")?;
                if cluster_known || citations.rows_by_cluster.contains_key(cluster) {
                    let previous = citations.rows_by_cluster.get(cluster).into_iter().flatten()
                        .map(|row| serde_json::from_str::<Value>(row.get()))
                        .collect::<serde_json::Result<Vec<_>>>()?;
                    if &previous != rows {
                        return Err(format!("conflicting cached citation evidence for cluster {cluster}").into());
                    }
                }
                if !rows.is_empty() {
                    citations.rows_by_cluster.insert(cluster.to_owned(), rows.iter()
                        .map(to_raw_value).collect::<serde_json::Result<Vec<_>>>()?);
                }
            }
            _ => return Err("cached citation evidence is absent or lacks authoritative input headers".into()),
        }
        attached_metadata_candidates += 1;
    }
    if absent_dockets.iter().any(|id| value.dockets.rows.contains_key(id))
        || absent_courts.iter().any(|id| value.courts.rows.contains_key(id)) {
        return Err("conflicting absent and present cached related metadata".into());
    }
    validate(&value)?;
    for cluster in &wanted {
        if !value.clusters.rows.contains_key(cluster) {
            return Err(format!("selected cluster {cluster} absent from joined metadata").into());
        }
    }
    value.clusters.rows.retain(|id, _| wanted.contains(id));
    let docket_column = value.clusters.columns.iter().position(|column| column == "docket_id")
        .ok_or("cluster docket column missing")?;
    let mut docket_ids = HashSet::new();
    for row in value.clusters.rows.values() {
        let cells: Vec<Value> = serde_json::from_str(row.get())?;
        if let Some(id) = cells[docket_column].as_str() {
            docket_ids.insert(id.to_owned());
        }
    }
    value.dockets.rows.retain(|id, _| docket_ids.contains(id));
    let missing_docket_ids = docket_ids.iter().filter(|id| !value.dockets.rows.contains_key(*id))
        .cloned().collect::<BTreeSet<_>>();
    // Court rows are small; keeping all avoids a second relationship projection.
    if let Some(citations) = &mut value.citations {
        citations.rows_by_cluster.retain(|cluster, _| wanted.contains(cluster));
        let mut rows = 0usize;
        let mut raw_csv_bytes = 0usize;
        for wrappers in citations.rows_by_cluster.values() {
            rows += wrappers.len();
            for wrapper in wrappers {
                let row: Value = serde_json::from_str(wrapper.get())?;
                raw_csv_bytes += row["raw_csv"].as_str().unwrap().len();
            }
        }
        for (field, count) in [("selected_clusters", wanted.len()),
            ("selected_clusters_without_rows", wanted.len()-citations.rows_by_cluster.len()),
            ("retained_rows", rows), ("retained_clusters", citations.rows_by_cluster.len()),
            ("retained_raw_csv_bytes", raw_csv_bytes)] {
            citations.header.insert(field.to_owned(), json!(count));
        }
    }
    value.header.insert("wanted_clusters".to_owned(), json!(wanted.len()));
    value.header.insert("matched_clusters".to_owned(), json!(value.clusters.rows.len()));
    value.header.insert("matched_dockets".to_owned(), json!(value.dockets.rows.len()));
    let subset = json!({"kind":"selected_candidates","input_context":input_context,"selection":selection,
        "base_context":base_context,"attached_metadata_candidates":attached_metadata_candidates,
        "source_scope":"Authoritative source, opinion_source, source_membership, source_rows, missing opinions and source timings remain the original context; matched and retained counts describe this subset",
        "court_scope":"all_input_courts","missing_docket_ids":missing_docket_ids,
        "retained_counts":{"clusters":value.clusters.rows.len(),"dockets":value.dockets.rows.len(),
            "courts":value.courts.rows.len(),"citation_rows":value.citation_header("retained_rows"),
            "citation_clusters":value.citation_header("retained_clusters"),
            "citation_raw_csv_bytes":value.citation_header("retained_raw_csv_bytes")},
        "seconds":started.elapsed().as_secs_f64()});
    value.header.insert("metadata_subset".to_owned(), subset);
    write_joined(&value, out, true)
}

pub fn read(path: &Path) -> Result<JoinedMetadata> {
    let reader = BufReader::with_capacity(
        1024 * 1024,
        zstd::stream::read::Decoder::new(File::open(path)?)?,
    );
    let value: JoinedMetadata = serde_json::from_reader(reader)?;
    validate(&value)?;
    Ok(value)
}

fn validate(value: &JoinedMetadata) -> Result<()> {
    if value.header("schema").as_str() != Some(JOINED_SCHEMA) {
        return Err("unrecognized joined metadata".into());
    }
    for table in [&value.clusters, &value.dockets, &value.courts] {
        let columns = &table.columns;
        let mut names = HashSet::new();
        for column in columns {
            if !names.insert(column) {
                return Err("duplicate metadata column".into());
            }
        }
        let id_column = columns
            .iter()
            .position(|column| column == "id")
            .ok_or("metadata id column missing")?;
        for (id, row) in &table.rows {
            let values: Vec<Value> = serde_json::from_str(row.get())?;
            if values.len() != columns.len()
                || values
                    .iter()
                    .any(|value| !value.is_null() && !value.is_string())
                || values[id_column].as_str() != Some(id.as_str())
            {
                return Err("metadata row differs from columns or source id".into());
            }
        }
    }
    if let Some(citations) = &value.citations {
        let columns = value.citation_header("columns")
            .as_array()
            .ok_or("citation columns missing")?;
        let mut names = HashSet::new();
        for column in columns {
            let name = column.as_str().ok_or("citation column is not a string")?;
            if !names.insert(name) {
                return Err("duplicate citation column".into());
            }
        }
        let cluster_column = columns
            .iter()
            .position(|column| column.as_str() == Some("cluster_id"))
            .ok_or("citation cluster column missing")?;
        let id_column = columns.iter().position(|column| column.as_str() == Some("id"))
            .ok_or("citation id column missing")?;
        let mut ids = HashSet::new();
        for (cluster, rows) in &citations.rows_by_cluster {
            let mut previous_ordinal = 0;
            for row in rows {
                let row: Value = serde_json::from_str(row.get())?;
                let original = row["original"]
                    .as_array()
                    .ok_or("citation original is not an array")?;
                if original.len() != columns.len()
                    || original
                        .iter()
                        .any(|value| !value.is_null() && !value.is_string())
                    || original[cluster_column].as_str() != Some(cluster.as_str())
                    || !row["raw_csv"].is_string()
                    || row["source_ordinal"]
                        .as_u64()
                        .is_none_or(|ordinal| ordinal == 0)
                {
                    return Err("citation evidence differs from columns or source identity".into());
                }
                let ordinal = row["source_ordinal"].as_u64().unwrap();
                let id = original[id_column].as_str().ok_or("citation source id missing")?;
                if ordinal <= previous_ordinal || !ids.insert(id.to_owned())
                    || fields(row["raw_csv"].as_str().unwrap().as_bytes())? != *original {
                    return Err("citation raw source, id or record ordering differs".into());
                }
                previous_ordinal = ordinal;
            }
        }
    }
    Ok(())
}
