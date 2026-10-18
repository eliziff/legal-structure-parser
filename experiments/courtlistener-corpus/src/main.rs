use legal_structure::{analyze_native_markup, DocumentQuery, DocumentStructure, NativeMarkupInput};
use regex::{Regex, RegexSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
mod cap_citations;
mod census;
mod census_features;
mod derivations;
mod metadata;
mod native_aggregate;
mod record_census;
mod richness;
mod witness_run;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const RENDITIONS: [&str; 8] = [
    "html_with_citations",
    "xml_harvard",
    "html_columbia",
    "html_lawbox",
    "html_anon_2020",
    "html",
    "plain_text",
    "xml_scan",
];

#[derive(Serialize, Deserialize)]
struct Frame {
    offset: u64,
    bytes: u64,
    raw_bytes: usize,
    sha256: String,
    ids: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Index {
    schema: String,
    rows: usize,
    raw_bytes: usize,
    compressed_bytes: u64,
    level: i32,
    block_bytes: usize,
    frames: Vec<Frame>,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write_json(path: impl AsRef<Path>, value: &impl Serialize) -> Result<()> {
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn flush_frame(
    output: &mut File,
    raw: &mut Vec<u8>,
    ids: &mut Vec<String>,
    index: &mut Index,
    maximum_bytes: u64,
) -> Result<()> {
    if raw.is_empty() {
        return Ok(());
    }
    let compressed = zstd::stream::encode_all(raw.as_slice(), index.level)?;
    let restored = zstd::stream::decode_all(compressed.as_slice())?;
    if restored != *raw {
        return Err("source bytes changed during compression".into());
    }
    let offset = output.stream_position()?;
    if offset + compressed.len() as u64 > maximum_bytes.saturating_sub(16 * 1024 * 1024) {
        return Err("compressed part would exceed the iteration disk budget".into());
    }
    output.write_all(&compressed)?;
    index.frames.push(Frame {
        offset,
        bytes: compressed.len() as u64,
        raw_bytes: raw.len(),
        sha256: hash(raw),
        ids: std::mem::take(ids),
    });
    index.raw_bytes += raw.len();
    raw.clear();
    Ok(())
}

fn retained_ids(pack: &Path, ids: &mut HashSet<String>) -> Result<()> {
    let index_path = pack.join("index.json");
    if index_path.exists() {
        let index: Index = serde_json::from_reader(File::open(index_path)?)?;
        if index.schema != "courtlistener.lossless-zstd-frames.v1"
            || index
                .frames
                .iter()
                .map(|frame| frame.ids.len())
                .sum::<usize>()
                != index.rows
        {
            return Err("invalid exclusion index".into());
        }
        for frame in index.frames {
            ids.extend(frame.ids);
        }
        return Ok(());
    }
    let source: Value = serde_json::from_reader(File::open(pack.join("source.json"))?)?;
    if source["schema"] != "courtlistener.acquired-pack-pool.v1" {
        return Err("unrecognized exclusion pool".into());
    }
    for part in source["parts"]
        .as_array()
        .ok_or("exclusion pool parts missing")?
    {
        let path = Path::new(part["path"].as_str().ok_or("exclusion part path missing")?);
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("exclusion part path outside pool".into());
        }
        let directory = pack.join(path);
        if hash(&fs::read(directory.join("index.json"))?)
            != part["index_sha256"]
                .as_str()
                .ok_or("exclusion part digest missing")?
        {
            return Err("exclusion index digest mismatch".into());
        }
        retained_ids(&directory, ids)?;
    }
    Ok(())
}

fn pack(
    out: &Path,
    level: i32,
    block_bytes: usize,
    maximum_bytes: u64,
    exclusion_packs: &[PathBuf],
) -> Result<()> {
    let mut existing = HashSet::new();
    let mut exclusion_sources = Vec::new();
    for directory in exclusion_packs {
        retained_ids(directory, &mut existing)?;
        let receipt = if directory.join("index.json").exists() {
            "index.json"
        } else {
            "source.json"
        };
        exclusion_sources.push(json!({"path":directory,"receipt":receipt,"sha256":hash(&fs::read(directory.join(receipt))?)}));
    }
    fs::create_dir_all(out)?;
    let path = out.join("opinions.jsonl.zst");
    let mut output = File::options().write(true).create_new(true).open(path)?;
    let mut index = Index {
        schema: "courtlistener.lossless-zstd-frames.v1".into(),
        rows: 0,
        raw_bytes: 0,
        compressed_bytes: 0,
        level,
        block_bytes,
        frames: Vec::new(),
    };
    let started = Instant::now();
    let mut raw = Vec::with_capacity(block_bytes);
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    let mut membership = metadata::ClusterMembership::default();
    let mut skipped_existing_rows = 0usize;
    for line in io::stdin().lock().lines() {
        let line = line?;
        let row: Value = serde_json::from_str(&line)?;
        let id = row["opinion"]["id"]
            .as_str()
            .ok_or("source record id missing")?
            .to_owned();
        if !seen.insert(id.clone()) {
            return Err("duplicate source record".into());
        }
        if existing.contains(&id) {
            skipped_existing_rows += 1;
            continue;
        }
        if raw.len() + line.len() + 1 > block_bytes {
            flush_frame(&mut output, &mut raw, &mut ids, &mut index, maximum_bytes)?;
        }
        raw.extend_from_slice(line.as_bytes());
        raw.push(b'\n');
        membership.record(row["opinion"]["cluster_id"].as_str(), &id);
        ids.push(id);
        index.rows += 1;
        if index.rows % 1000 == 0 {
            eprintln!(
                "packed {} source records in {:.2}s",
                index.rows,
                started.elapsed().as_secs_f64()
            );
        }
    }
    flush_frame(&mut output, &mut raw, &mut ids, &mut index, maximum_bytes)?;
    output.sync_all()?;
    index.compressed_bytes = output.metadata()?.len();
    let mut receipt = serde_json::to_value(&index)?;
    receipt["input_rows"] = json!(seen.len());
    receipt["skipped_existing_rows"] = json!(skipped_existing_rows);
    receipt["exclusion_sources"] = json!(exclusion_sources);
    write_json(out.join("index.json"), &receipt)?;
    membership.persist(out, &fs::read(out.join("index.json"))?)?;
    eprintln!(
        "{}",
        json!({"rows":index.rows,"raw_bytes":index.raw_bytes,"compressed_bytes":index.compressed_bytes,"seconds":started.elapsed().as_secs_f64(),"every_source_byte_verified":true})
    );
    Ok(())
}

fn each_record(pack: &Path, mut visit: impl FnMut(Value) -> Result<()>) -> Result<Index> {
    read_records(pack, None, &mut visit)
}

fn read_records(
    pack: &Path,
    wanted: Option<&HashSet<String>>,
    visit: &mut dyn FnMut(Value) -> Result<()>,
) -> Result<Index> {
    read_records_from_pool(pack, None, wanted, visit)
}

fn read_records_from_pool(
    pack: &Path,
    frozen_pool: Option<&Value>,
    wanted: Option<&HashSet<String>>,
    visit: &mut dyn FnMut(Value) -> Result<()>,
) -> Result<Index> {
    if !pack.join("index.json").exists() {
        let loaded_source;
        let source = if let Some(source) = frozen_pool {
            source
        } else {
            loaded_source =
                serde_json::from_reader::<_, Value>(File::open(pack.join("source.json"))?)?;
            &loaded_source
        };
        if source["schema"] != "courtlistener.acquired-pack-pool.v1" {
            return Err("unrecognized corpus pool".into());
        }
        let mut combined = Index {
            schema: "courtlistener.acquired-pack-pool.v1".into(),
            rows: 0,
            raw_bytes: 0,
            compressed_bytes: 0,
            level: 0,
            block_bytes: 0,
            frames: Vec::new(),
        };
        let mut seen = HashSet::new();
        for part in source["parts"].as_array().ok_or("pool parts missing")? {
            let path = Path::new(part["path"].as_str().ok_or("part path missing")?);
            if path.is_absolute()
                || path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err("part path outside pool".into());
            }
            let directory = pack.join(path);
            if hash(&fs::read(directory.join("index.json"))?)
                != part["index_sha256"].as_str().ok_or("part digest missing")?
            {
                return Err("pool part index digest mismatch".into());
            }
            let index = read_records(&directory, wanted, &mut |row| {
                let id = row["opinion"]["id"]
                    .as_str()
                    .ok_or("source id missing")?
                    .to_owned();
                if !seen.insert(id) {
                    return Err("duplicate opinion across pool parts".into());
                }
                visit(row)
            })?;
            combined.rows += index.rows;
            combined.raw_bytes += index.raw_bytes;
            combined.compressed_bytes += index.compressed_bytes;
        }
        if combined.rows != source["opinion_rows"].as_u64().unwrap_or(0) as usize {
            return Err("pool count differs from acquired source".into());
        }
        return Ok(combined);
    }
    let index: Index = serde_json::from_reader(File::open(pack.join("index.json"))?)?;
    let mut file = File::open(pack.join("opinions.jsonl.zst"))?;
    if index.schema != "courtlistener.lossless-zstd-frames.v1"
        || file.metadata()?.len() != index.compressed_bytes
        || index
            .frames
            .iter()
            .map(|frame| frame.ids.len())
            .sum::<usize>()
            != index.rows
    {
        return Err("pack identity or size mismatch".into());
    }
    let mut count = 0;
    let frames = index
        .frames
        .iter()
        .filter(|frame| wanted.is_none_or(|ids| frame.ids.iter().any(|id| ids.contains(id))))
        .collect::<Vec<_>>();
    let expected = frames.iter().map(|frame| frame.ids.len()).sum::<usize>();
    for frame in frames {
        file.seek(SeekFrom::Start(frame.offset))?;
        let mut compressed = vec![0; frame.bytes as usize];
        file.read_exact(&mut compressed)?;
        let raw = zstd::stream::decode_all(compressed.as_slice())?;
        if raw.len() != frame.raw_bytes || hash(&raw) != frame.sha256 {
            return Err("corpus frame digest mismatch".into());
        }
        let mut ids = Vec::new();
        for line in raw
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let mut row: Value = serde_json::from_slice(line)?;
            let id = row["opinion"]["id"]
                .as_str()
                .ok_or("record id missing")?
                .to_owned();
            ids.push(id.clone());
            if wanted.is_none_or(|ids| ids.contains(&id)) {
                derivations::restore(&mut row)?;
                visit(row)?;
            }
            count += 1;
        }
        if ids != frame.ids {
            return Err("corpus index membership differs from source".into());
        }
    }
    if count != expected {
        return Err("corpus row count mismatch".into());
    }
    Ok(index)
}

fn selected_markup(row: &Value) -> Option<&str> {
    selected_markup_field(row).and_then(|field| row[field].as_str())
}

fn selected_markup_field(row: &Value) -> Option<&'static str> {
    RENDITIONS[..6].iter().copied().find(|field| {
        row[*field]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn catalog(pack: &Path, out: &Path, metadata_path: &Path) -> Result<()> {
    let count = usize::MAX;
    let started = Instant::now();
    let mut scanned = 0usize;
    let metadata = metadata::read(metadata_path)?;
    let frozen_pool = metadata::freeze_pool(pack, Some(out))?;
    let opinion_source = match &frozen_pool {
        Some((_, descriptor)) => descriptor.clone(),
        None => {
            json!({"kind":"pack_index","index_sha256":hash(&fs::read(pack.join("index.json"))?)})
        }
    };
    let source_windows = frozen_pool
        .as_ref()
        .map(|(source, _)| {
            source["parts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|part| {
                    let ordinal = part["ordinal"]
                        .as_u64()
                        .ok_or("source window ordinal missing")?;
                    Ok((
                        ordinal,
                        json!({
                            "ordinal":ordinal,"start":part["start"],"end":part["end"],
                            "index_sha256":part["index_sha256"],"rows":part["rows"]
                        }),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let subjects = [
        (
            "criminal",
            r"\b(?:criminal|homicide|felony|conviction|sentencing|prosecution)\b",
        ),
        (
            "constitutional",
            r"\b(?:constitutional|due process|equal protection|first amendment|fourth amendment)\b",
        ),
        (
            "civil_rights",
            r"\b(?:civil rights|section 1983|discrimination|qualified immunity)\b",
        ),
        (
            "contracts",
            r"\b(?:contract|breach of contract|arbitration|indemnity|promissory estoppel)\b",
        ),
        (
            "torts",
            r"\b(?:negligence|defamation|malpractice|product liability|wrongful death)\b",
        ),
        (
            "property",
            r"\b(?:real property|easement|quiet title|eminent domain|foreclosure)\b",
        ),
        (
            "family",
            r"\b(?:divorce|child custody|child support|adoption|marital property)\b",
        ),
        (
            "probate",
            r"\b(?:probate|testamentary|decedent|intestate|executor)\b",
        ),
        (
            "bankruptcy",
            r"\b(?:bankruptcy|chapter 11|chapter 7|automatic stay|dischargeability)\b",
        ),
        (
            "employment",
            r"\b(?:employment|wrongful termination|labor relations|collective bargaining|wage)\b",
        ),
        (
            "immigration",
            r"\b(?:immigration|deportation|asylum|removal proceedings|naturalization)\b",
        ),
        (
            "tax",
            r"\b(?:taxation|income tax|tax deficiency|tax refund|internal revenue)\b",
        ),
        (
            "intellectual_property",
            r"\b(?:patent|copyright|trademark|trade secret)\b",
        ),
        (
            "administrative",
            r"\b(?:administrative procedure|agency action|rulemaking|administrative law)\b",
        ),
        (
            "environment",
            r"\b(?:environmental|clean water act|clean air act|endangered species|pollution)\b",
        ),
        (
            "commercial",
            r"\b(?:securities|antitrust|corporate|shareholder|uniform commercial code)\b",
        ),
        (
            "insurance",
            r"\b(?:insurance|policyholder|insured|underwriter)\b",
        ),
        (
            "benefits",
            r"\b(?:social security|veterans benefits|disability benefits|workers compensation)\b",
        ),
        (
            "habeas",
            r"\b(?:habeas corpus|postconviction|post-conviction|2254|2255)\b",
        ),
        (
            "election",
            r"\b(?:election|ballot|voting rights|redistricting)\b",
        ),
        (
            "tribal",
            r"\b(?:tribal|indian reservation|tribal sovereignty|indian country)\b",
        ),
        ("maritime", r"\b(?:admiralty|maritime|seaman|jones act)\b"),
        (
            "international",
            r"\b(?:foreign sovereign|international law|extradition|treaty)\b",
        ),
        (
            "civil_procedure",
            r"\b(?:civil procedure|summary judgment|personal jurisdiction|class action|res judicata)\b",
        ),
        (
            "evidence",
            r"\b(?:hearsay|expert testimony|rules of evidence|admissibility)\b",
        ),
    ];
    let topic_matcher = RegexSet::new(
        subjects
            .iter()
            .map(|(_, pattern)| format!("(?i-u){pattern}")),
    )?;
    let shape = Regex::new(
        r#"(?i-u)<(?:table|footnote|h[1-6])\b|(?:page-label|star-pagination|data-label)=?|id=["'](?:fn|footnote)"#,
    )?;
    let elements = scraper::Selector::parse("*").map_err(|_| "invalid fixed element selector")?;
    #[derive(Clone)]
    struct Candidate {
        cluster_key: String,
        raw_bytes: usize,
        compressed: Vec<u8>,
    }
    impl Candidate {
        fn decode(&self) -> Result<Value> {
            Ok(serde_json::from_slice(&zstd::bulk::decompress(
                &self.compressed,
                self.raw_bytes,
            )?)?)
        }
    }
    let mut groups = BTreeMap::<String, VecDeque<Candidate>>::new();
    let mut skipped = BTreeMap::<&str, usize>::new();
    let mut missing_metadata = BTreeMap::<&str, usize>::new();
    let mut range_coverage = BTreeMap::<String, Value>::new();
    let compressed = out.extension().is_some_and(|extension| extension == "zst");
    let unjoined_path = if compressed {
        out.with_extension("")
            .with_extension("unjoined-source-features.jsonl.zst")
    } else {
        out.with_extension("unjoined-source-features.jsonl")
    };
    let mut unjoined_plain = None::<io::BufWriter<File>>;
    let mut unjoined_encoder = None;
    let increment = |counts: &mut Value, key: &str| {
        counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
    };
    read_records_from_pool(
        pack,
        frozen_pool.as_ref().map(|(source, _)| source),
        None,
        &mut |record| {
            scanned += 1;
            if scanned % 1000 == 0 {
                eprintln!(
                    "classified {scanned} source records in {:.2}s",
                    started.elapsed().as_secs_f64()
                );
            }
            let row = &record["opinion"];
            let renditions = RENDITIONS
                .iter()
                .filter(|field| row[**field].as_str().is_some_and(|s| !s.is_empty()))
                .copied()
                .collect::<Vec<_>>();
            let markup = selected_markup(row).unwrap_or("");
            let text = row["plain_text"].as_str().unwrap_or("");
            let length = RENDITIONS
                .iter()
                .filter_map(|field| row[*field].as_str())
                .map(str::len)
                .max()
                .unwrap_or(0);
            let mut shapes = BTreeSet::new();
            let mut source_features = BTreeMap::new();
            for field in RENDITIONS.iter().filter(|field| **field != "plain_text") {
                let Some(source) = row[*field].as_str().filter(|value| !value.is_empty()) else {
                    continue;
                };
                let inventory = richness::features(source, &elements);
                let rendition_shapes = shape
                    .find_iter(source)
                    .map(|found| found.as_str().to_ascii_lowercase())
                    .collect::<BTreeSet<_>>();
                shapes.extend(rendition_shapes.iter().cloned());
                for (name, prefix) in [
                    ("tags", "tag"),
                    ("attribute_names", "attribute"),
                    ("classes", "class"),
                    ("structural_categories", "structure"),
                ] {
                    shapes.extend(
                        inventory[name]
                            .as_object()
                            .unwrap()
                            .keys()
                            .map(|family| format!("{prefix}:{family}")),
                    );
                }
                source_features.insert(*field, json!({"source_utf8_bytes":source.len(),
                "source_sha256":hash(source.as_bytes()),"shapes":rendition_shapes,
                "tags":inventory["tags"],"attribute_names":inventory["attribute_names"],
                "classes":inventory["classes"],"structural_categories":inventory["structural_categories"]}));
            }
            let coverage = range_coverage.entry(record["range_index"].to_string())
            .or_insert_with(|| json!({"records":0,"courts":{},"eras":{},"source_feature_tokens":{},"strata":{},"missing_metadata":{},
                "source_window":record["range_index"].as_u64().and_then(|ordinal|source_windows.get(&ordinal))}));
            coverage["records"] = json!(coverage["records"].as_u64().unwrap() + 1);
            for token in &shapes {
                increment(&mut coverage["source_feature_tokens"], token);
            }
            let cluster_id = row["cluster_id"].as_str().filter(|id| !id.is_empty());
            let cluster = metadata::lookup(&metadata, "clusters", cluster_id.unwrap_or(""))?;
            let slug = cluster["slug"].as_str().filter(|slug| !slug.is_empty());
            let docket = metadata::lookup(
                &metadata,
                "dockets",
                cluster["docket_id"].as_str().unwrap_or(""),
            )?;
            let court_id = docket["court_id"].as_str().filter(|id| !id.is_empty());
            let court = metadata::lookup(&metadata, "courts", court_id.unwrap_or(""))?;
            let era = cluster["date_filed"]
                .as_str()
                .and_then(|s| s.get(..4))
                .and_then(|s| s.parse::<u32>().ok())
                .map(|year| year / 25 * 25);
            let subject_hints = topic_matcher
                .matches(if text.is_empty() { markup } else { text })
                .iter()
                .map(|index| subjects[index].0)
                .collect::<Vec<_>>();
            let metadata_presence = [
                "syllabus",
                "headnotes",
                "summary",
                "procedural_history",
                "posture",
                "disposition",
                "arguments",
                "headmatter",
                "correction",
                "history",
                "scdb_id",
            ]
            .into_iter()
            .filter(|field| cluster[*field].as_str().is_some_and(|s| !s.is_empty()))
            .collect::<Vec<_>>();
            let mut record_dimension = |name: &str, value: &Value| {
                increment(&mut coverage["strata"][name], &value.to_string());
            };
            for (name, value) in [
                ("court_id", json!(court_id)),
                ("jurisdiction", court["jurisdiction"].clone()),
                ("era", json!(era)),
                ("nature_of_suit", docket["nature_of_suit"].clone()),
                ("type", row["type"].clone()),
                ("ocr", row["extracted_by_ocr"].clone()),
                ("size_bucket", json!(length.checked_ilog10())),
                ("source", cluster["source"].clone()),
                (
                    "precedential_status",
                    cluster["precedential_status"].clone(),
                ),
                ("per_curiam", row["per_curiam"].clone()),
                (
                    "author_present",
                    json!(row["author_id"].as_str().is_some_and(|s| !s.is_empty())),
                ),
            ] {
                record_dimension(name, &value);
            }
            for (name, values) in [
                ("subject_hints", &subject_hints),
                ("renditions", &renditions),
                ("metadata_presence", &metadata_presence),
            ] {
                for value in values {
                    record_dimension(name, &json!(value));
                }
            }
            for shape in &shapes {
                record_dimension("shapes", &json!(shape));
            }
            if let Some(court_id) = court_id {
                increment(&mut coverage["courts"], court_id);
            }
            if let Some(era) = era {
                increment(&mut coverage["eras"], &era.to_string());
            }
            for (name, missing) in [
                ("source_cluster_id", cluster_id.is_none()),
                ("cluster", cluster.is_null()),
                ("slug", slug.is_none()),
                ("docket", docket.is_null()),
                ("court", court.is_null()),
                ("era", era.is_none()),
            ] {
                if missing {
                    *missing_metadata.entry(name).or_default() += 1;
                    increment(&mut coverage["missing_metadata"], name);
                }
            }
            let Some(slug) = slug else {
                let reason = if cluster_id.is_none() {
                    "source_cluster_id_missing"
                } else if cluster.is_null() {
                    "cluster_metadata_missing"
                } else {
                    "cluster_slug_missing"
                };
                *skipped.entry(reason).or_default() += 1;
                if unjoined_plain.is_none() && unjoined_encoder.is_none() {
                    let file = io::BufWriter::new(File::create(&unjoined_path)?);
                    if compressed {
                        unjoined_encoder = Some(zstd::stream::write::Encoder::new(file, 3)?);
                    } else {
                        unjoined_plain = Some(file);
                    }
                }
                let writer: &mut dyn Write = match unjoined_encoder.as_mut() {
                    Some(writer) => writer,
                    None => unjoined_plain.as_mut().unwrap(),
                };
                serde_json::to_writer(
                    &mut *writer,
                    &json!({"opinion_id":row["id"],"cluster_id":row["cluster_id"],
                "range_index":record["range_index"],"court_id":court_id,"era":era,"url":null,
                "canonical_url_unavailable_reason":reason,"renditions":renditions,"shapes":shapes,
                "source_features":source_features}),
                )?;
                writer.write_all(b"\n")?;
                return Ok(());
            };
            if length == 0 {
                *skipped.entry("empty_renditions").or_default() += 1;
                return Ok(());
            }
            let size_bucket = length.ilog10();
            let court_id = court_id.unwrap_or("unknown");
            let key = format!("{court_id}:{era:?}:{}:{}:{}:{:?}:{}:{}:{}:{:?}:{subject_hints:?}:{metadata_presence:?}:{}:{}",court["jurisdiction"],docket["nature_of_suit"],cluster["precedential_status"],renditions,row["type"],row["extracted_by_ocr"],size_bucket,shapes,cluster["source"],row["per_curiam"]);
            let candidate = json!({"opinion_id":row["id"],"cluster_id":row["cluster_id"],"url":format!("https://www.courtlistener.com/opinion/{}/{slug}/",row["cluster_id"].as_str().unwrap_or("")),"url_source":"official cluster id and slug","court_id":court_id,"jurisdiction":court["jurisdiction"],"era":era,"nature_of_suit":docket["nature_of_suit"],"range_index":record["range_index"],"renditions":renditions,"type":row["type"],"ocr":row["extracted_by_ocr"],"source_bytes":length,"size_bucket":size_bucket,"shapes":shapes,"source_features":source_features,"subject_hints":subject_hints,"subject_hint_method":"explicit keyword matches; not verified legal issue labels","metadata_presence":metadata_presence,"source":cluster["source"],"precedential_status":cluster["precedential_status"],"per_curiam":row["per_curiam"],"author_present":row["author_id"].as_str().is_some_and(|s|!s.is_empty())});
            let bytes = serde_json::to_vec(&candidate)?;
            groups.entry(key).or_default().push_back(Candidate {
                cluster_key: candidate["cluster_id"].to_string(),
                raw_bytes: bytes.len(),
                compressed: zstd::bulk::compress(&bytes, 1)?,
            });
            Ok(())
        },
    )?;
    let unjoined_exists = unjoined_plain.is_some() || unjoined_encoder.is_some();
    if let Some(encoder) = unjoined_encoder {
        encoder.finish()?.flush()?;
    }
    if let Some(writer) = unjoined_plain.as_mut() {
        writer.flush()?;
    }
    let classification_seconds = started.elapsed().as_secs_f64();
    let candidate_records = groups.values().map(VecDeque::len).sum::<usize>();
    let candidate_raw_bytes = groups
        .values()
        .flatten()
        .map(|candidate| candidate.raw_bytes as u64)
        .sum::<u64>();
    let candidate_compressed_bytes = groups
        .values()
        .flatten()
        .map(|candidate| candidate.compressed.len() as u64)
        .sum::<u64>();
    let mut queues = groups.into_values().collect::<Vec<_>>();
    queues.sort_by_key(|queue| queue.len());
    let mut selected = Vec::<Candidate>::new();
    let mut clusters = BTreeSet::new();
    // Establish marginal coverage before filling rare combinations. Metadata
    // remains the source of jurisdiction/date facts; subject hints stay labelled.
    let all = queues
        .iter()
        .flat_map(|queue| queue.iter())
        .collect::<Vec<_>>();
    let mut dimensions = BTreeMap::<String, BTreeMap<String, VecDeque<usize>>>::new();
    for (index, saved) in all.iter().enumerate() {
        let candidate = saved.decode()?;
        for field in [
            "court_id",
            "jurisdiction",
            "era",
            "nature_of_suit",
            "type",
            "ocr",
            "size_bucket",
            "source",
            "precedential_status",
            "per_curiam",
            "author_present",
        ] {
            dimensions
                .entry(field.into())
                .or_default()
                .entry(candidate[field].to_string())
                .or_default()
                .push_back(index);
        }
        for field in ["subject_hints", "renditions", "metadata_presence", "shapes"] {
            for value in candidate[field].as_array().unwrap() {
                dimensions
                    .entry(field.into())
                    .or_default()
                    .entry(value.to_string())
                    .or_default()
                    .push_back(index);
            }
        }
    }
    let mut coverage = dimensions
        .into_values()
        .map(|values| {
            let mut bins = values.into_values().collect::<Vec<_>>();
            bins.sort_by_key(|bin| bin.len());
            bins
        })
        .collect::<Vec<_>>();
    let widest = coverage.iter().map(Vec::len).max().unwrap_or(0);
    for position in 0..widest {
        for dimension in &mut coverage {
            if selected.len() == count {
                break;
            }
            if let Some(bin) = dimension.get_mut(position) {
                while let Some(index) = bin.pop_front() {
                    let candidate = all[index];
                    if clusters.insert(candidate.cluster_key.clone()) {
                        selected.push(candidate.clone());
                        break;
                    }
                }
            }
        }
        if selected.len() == count {
            break;
        }
    }
    drop(all);
    while selected.len() < count {
        let before = selected.len();
        for queue in &mut queues {
            while let Some(candidate) = queue.pop_front() {
                if clusters.insert(candidate.cluster_key.clone()) {
                    selected.push(candidate);
                    break;
                }
            }
            if selected.len() == count {
                break;
            }
        }
        if selected.len() == before {
            break;
        }
    }
    let file = io::BufWriter::new(File::create(out)?);
    let (mut encoder, mut plain) = if compressed {
        (Some(zstd::stream::write::Encoder::new(file, 3)?), None)
    } else {
        (None, Some(file))
    };
    let mut writer: &mut dyn Write = match encoder.as_mut() {
        Some(writer) => writer,
        None => plain.as_mut().unwrap(),
    };
    serde_json::to_writer(
        &mut writer,
        &json!({"schema":"courtlistener.selection-catalog.v1","metadata_sha256":hash(&fs::read(metadata_path)?),"opinion_source":opinion_source,"records":scanned,"clusters":selected.len(),
            "classification_seconds":classification_seconds,"cached_selection":"Separate select command reads the preordered catalog prefix; no source classification or native parser call",
            "candidate_cache":{"records":candidate_records,"serialized_bytes":candidate_raw_bytes,"compressed_bytes":candidate_compressed_bytes,
                "encoding":"Complete candidate JSON in level-1 Zstandard buffers; one candidate decoded at a time. Full source metadata is attached while writing."},
            "skipped_records":skipped,"missing_metadata_records":missing_metadata,"range_coverage":range_coverage,
            "unjoined_source_features_file":if unjoined_exists {unjoined_path.file_name().map(|name|name.to_string_lossy().into_owned())} else {None},
            "source_features_scope":{"renditions":RENDITIONS.iter().filter(|field|**field!="plain_text").collect::<Vec<_>>(),
                "inventory":"All populated markup renditions; HTML5 fragment DOM observations may include repaired parents and generated wrapper nodes; not native parser output",
                "ranked_families":["tags","attribute_names","classes","structural_categories"],
                "provenance":"Per-rendition source_features records exact UTF-8 source byte length and SHA-256; repeated selection reads the cached catalog"}}),
    )?;
    writer.write_all(b"\n")?;
    for saved in selected {
        let mut candidate = saved.decode()?;
        let cluster = metadata::lookup(
            &metadata,
            "clusters",
            candidate["cluster_id"].as_str().unwrap_or(""),
        )?;
        let docket = metadata::lookup(
            &metadata,
            "dockets",
            cluster["docket_id"].as_str().unwrap_or(""),
        )?;
        let court = metadata::lookup(
            &metadata,
            "courts",
            candidate["court_id"].as_str().unwrap_or(""),
        )?;
        candidate["cluster_metadata"] = cluster;
        candidate["docket_metadata"] = docket;
        candidate["court_metadata"] = court;
        if let Some(citations) = metadata::citation_metadata(
            &metadata,
            candidate["cluster_id"].as_str().unwrap_or(""),
        )? {
            candidate["citation_metadata"] = citations;
        }
        serde_json::to_writer(&mut writer, &candidate)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    if let Some(encoder) = encoder {
        encoder.finish()?.flush()?;
    }
    eprintln!(
        "indexed {} distinct clusters from {scanned} records in {:.2}s",
        clusters.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn select(catalog: &Path, out: &Path, count: usize) -> Result<()> {
    let started = Instant::now();
    let file = File::open(catalog)?;
    let input: Box<dyn Read> = if catalog
        .extension()
        .is_some_and(|extension| extension == "zst")
    {
        Box::new(zstd::stream::read::Decoder::new(file)?)
    } else {
        Box::new(file)
    };
    let mut lines = BufReader::new(input).lines();
    let header: Value = serde_json::from_str(&lines.next().ok_or("empty selection catalog")??)?;
    if header["schema"] != "courtlistener.selection-catalog.v1" {
        return Err("unrecognized selection catalog".into());
    }
    let mut file = File::create(out)?;
    file.write_all(b"[\n")?;
    let mut selected = 0;
    for line in lines.take(count) {
        let candidate = line?;
        if selected != 0 {
            file.write_all(b",\n")?;
        }
        file.write_all(candidate.as_bytes())?;
        selected += 1;
    }
    file.write_all(b"\n]\n")?;
    eprintln!(
        "selected {selected} indexed clusters in {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

fn compile(
    row: &Value,
    field: Option<&str>,
    metadata: Option<&metadata::JoinedMetadata>,
    citation_context: Option<&cap_citations::CitationContext>,
) -> Result<DocumentStructure> {
    let cluster = match metadata {
        Some(metadata) => metadata::lookup(
            metadata,
            "clusters",
            row["cluster_id"].as_str().unwrap_or(""),
        )?,
        None => Value::Null,
    };
    let url = cluster["slug"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|slug| {
            format!(
                "https://www.courtlistener.com/opinion/{}/{slug}/",
                row["cluster_id"].as_str().unwrap_or("")
            )
        });
    let markup_field = field.or_else(|| selected_markup_field(row));
    let markup = markup_field
        .filter(|field| *field != "plain_text")
        .and_then(|field| row[field].as_str());
    let text = if field.is_none() || field == Some("plain_text") {
        row["plain_text"].as_str().unwrap_or("")
    } else {
        ""
    };
    let page_citations = citation_context
        .map(|context| {
            context.cluster(
                row["cluster_id"].as_str().unwrap_or(""),
                url.as_deref().unwrap_or(""),
                cluster["filepath_json_harvard"].as_str(),
                None,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let input: NativeMarkupInput = serde_json::from_value(
        json!({"provider":"courtlistener","id":row["id"],"url":url,"text":text,"markup":markup,
            "pageCitations":page_citations,
            "scope":{"kind":"complete","markupSerialization":if matches!(markup_field,Some("xml_harvard"|"xml_scan")) {"xml_source"} else {"html_source"}}}),
    )?;
    Ok(analyze_native_markup(input)?)
}

fn run(pack: &Path, out: &Path, limit: usize) -> Result<()> {
    fs::create_dir_all(out)?;
    let mut receipts = File::create(out.join("records.jsonl"))?;
    let started = Instant::now();
    let mut processed = 0;
    let mut parse_seconds = 0.0;
    let mut counts = BTreeMap::<String, usize>::new();
    let mut bytes = 0;
    let mut failures = 0;
    each_record(pack, |record| {
        if limit != 0 && processed >= limit {
            return Ok(());
        }
        let row = &record["opinion"];
        let then = Instant::now();
        let result = compile(row, None, None, None);
        let elapsed = then.elapsed().as_secs_f64();
        parse_seconds += elapsed;
        bytes += selected_markup(row)
            .map_or_else(|| row["plain_text"].as_str().unwrap_or("").len(), str::len);
        let receipt = match result {
            Ok(document) => {
                let query = DocumentQuery::new();
                let mut kinds = BTreeMap::<String, usize>::new();
                for node in &document.nodes {
                    *kinds.entry(node.kind.name().to_owned()).or_default() += 1;
                }
                for (kind, amount) in &kinds {
                    *counts.entry(kind.clone()).or_default() += amount;
                }
                let anchored = document
                    .nodes
                    .iter()
                    .filter(|node| node.anchor.is_some())
                    .count();
                let blocks = query.blocks(&document, None).take(3).collect::<Vec<_>>();
                let scalar = legal_structure::ScalarText::new(document.query_text());
                let mut plans = Vec::new();
                for block in blocks {
                    let quote = scalar
                        .slice_utf16(block.start..block.end)
                        .unwrap_or("")
                        .chars()
                        .take(160)
                        .collect::<String>();
                    if !quote.trim().is_empty() {
                        let plan = query.text_fragment_plan(
                            &document,
                            &quote,
                            &[quote.clone()],
                            false,
                            true,
                            true,
                            false,
                        );
                        let mut plan = serde_json::to_value(plan)?;
                        plan["label"] = json!(block.label);
                        plans.push(plan);
                    }
                }
                json!({"opinion_id":row["id"],"status":"parsed","text_sha256":document.text_sha256,"revision":document.revision,"nodes":kinds,"anchored_nodes":anchored,"parse_seconds":elapsed,"plans":plans,"live_browser_verification":"not_run"})
            }
            Err(error) => {
                failures += 1;
                json!({"opinion_id":row["id"],"status":"error","error":error.to_string()})
            }
        };
        serde_json::to_writer(&mut receipts, &receipt)?;
        receipts.write_all(b"\n")?;
        processed += 1;
        if processed % 100 == 0 {
            eprintln!("processed {processed} real records; {failures} parser errors");
            receipts.flush()?;
        }
        Ok(())
    })?;
    write_json(
        out.join("summary.json"),
        &json!({"engine_source_sha256":legal_structure::ENGINE_SOURCE_SHA256,"records":processed,"errors":failures,"input_bytes":bytes,"parse_seconds":parse_seconds,"parse_mib_per_second":bytes as f64/1048576.0/parse_seconds,"total_seconds":started.elapsed().as_secs_f64(),"nodes":counts,"full_courtlistener_corpus":false,"live_browser_checks":0}),
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    match args.get(1).map(String::as_str) {
        Some("record-census") => {
            let table = args.get(2).ok_or("source table name required")?;
            let out = Path::new(args.get(3).ok_or("record census output directory required")?);
            let maximum = args.get(4).ok_or("total record census output byte ceiling required")?.parse::<u64>()?;
            let witnesses = args.get(5).ok_or("record witness byte ceiling required")?.parse::<u64>()?;
            let features = args.get(6).ok_or("record feature ceiling required")?.parse::<usize>()?;
            match args.get(7..) {
                Some([]) => record_census::run(table, out, maximum, witnesses, features),
                Some([mode]) if mode == "--native-aggregate" && witnesses == 0 && features != 0 => {
                    native_aggregate::run(table, out, maximum)
                }
                _ => Err("record-census accepts only --native-aggregate; that mode requires zero witness bytes".into()),
            }
        }
        Some("census") => census::run(
            Path::new(args.get(2).ok_or("census output directory required")?),
            args.get(3).ok_or("total census output byte ceiling required")?.parse()?,
            args.get(4).ok_or("original witness byte ceiling required")?.parse()?,
            args.get(5).ok_or("distinct observed feature ceiling required")?.parse()?,
            args.get(6).map(|value| value.parse()).transpose()?,
        ),
        Some("decode") => {
            let mut input = zstd::stream::read::Decoder::new(File::open(
                args.get(2).ok_or("Zstandard file required")?,
            )?)?;
            io::copy(&mut input, &mut io::stdout().lock())?;
            Ok(())
        }
        Some("stream") | Some("stream-selected") => {
            let started = Instant::now();
            let wanted = if args[1] == "stream-selected" {
                let candidates: Vec<Value> = serde_json::from_reader(File::open(
                    args.get(3).ok_or("selected candidates JSON required")?
                )?)?;
                let mut ids = HashSet::new();
                for candidate in candidates {
                    let id = candidate["opinion_id"].as_str().filter(|id| !id.is_empty())
                        .ok_or("selected opinion id missing")?;
                    if !ids.insert(id.to_owned()) { return Err("duplicate selected opinion id".into()); }
                }
                Some(ids)
            } else { None };
            let mut found = HashSet::new();
            let mut output = io::BufWriter::new(io::stdout().lock());
            read_records(Path::new(args.get(2).ok_or("pack required")?), wanted.as_ref(), &mut |record| {
                if wanted.is_some() && !found.insert(
                    record["opinion"]["id"].as_str().ok_or("source id missing")?.to_owned()
                ) {
                    return Err("duplicate selected source opinion".into());
                }
                serde_json::to_writer(&mut output, &record)?;
                output.write_all(b"\n")?;
                Ok(())
            })?;
            output.flush()?;
            if wanted.as_ref().is_some_and(|ids| ids != &found) {
                return Err("selected source opinion absent from verified corpus".into());
            }
            if wanted.is_some() {
                eprintln!("streamed {} complete selected source records in {:.3} ms", found.len(), started.elapsed().as_secs_f64()*1000.0);
            }
            Ok(())
        }
        Some("verify-pack") => derivations::verify(
            Path::new(args.get(2).ok_or("original pack required")?),
            Path::new(args.get(3).ok_or("candidate pack required")?),
            Path::new(args.get(4).ok_or("proof output required")?),
        ),
        Some("pack") => {
            let mut exclusion_packs = Vec::new();
            for pair in args.get(6..).unwrap_or_default().chunks(2) {
                if pair.len() != 2 || pair[0] != "--exclude-pack" {
                    return Err("pack exclusion must be --exclude-pack DIRECTORY".into());
                }
                exclusion_packs.push(PathBuf::from(&pair[1]));
            }
            pack(
            Path::new(args.get(2).ok_or("output directory required")?),
            args.get(3).map_or(Ok(9), |s| s.parse())?,
            args.get(4).map_or(Ok(8 * 1024 * 1024), |s| s.parse())?,
            args.get(5).map_or(Ok(u64::MAX), |s| s.parse())?,
            &exclusion_packs,
        )
        },
        Some("catalog") => catalog(
            Path::new(args.get(2).ok_or("pack required")?),
            Path::new(args.get(3).ok_or("catalog output required")?),
            Path::new(
                args.get(4)
                    .ok_or("joined metadata required for canonical source URLs")?,
            ),
        ),
        Some("select") => select(
            Path::new(args.get(2).ok_or("selection catalog required")?),
            Path::new(args.get(3).ok_or("candidate output required")?),
            args.get(4).map_or(Ok(2000), |s| s.parse())?,
        ),
        Some("derive-pack") => derivations::pack(
            Path::new(args.get(2).ok_or("original pack required")?),
            Path::new(args.get(3).ok_or("output pack required")?),
            args.get(4).map_or(Ok(1_000_000_000), |s| s.parse())?,
            match args.get(5).map(String::as_str) {
                None => false,
                Some("plain-text") => true,
                _ => return Err("annotation recipe policy must be plain-text or omitted".into()),
            },
        ),
        Some("metadata") => metadata::collect(
            Path::new(args.get(2).ok_or("pack required")?),
            Path::new(args.get(3).ok_or("metadata source directory required")?),
            Path::new(args.get(4).ok_or("output required")?),
            args.get(5).ok_or("bzip2 executable required")?,
        ),
        Some("metadata-membership") => metadata::export_membership(
            Path::new(args.get(2).ok_or("pack required")?),
            Path::new(args.get(3).ok_or("membership output required")?),
        ),
        Some("metadata-citations") => metadata::enrich_citations(
            Path::new(args.get(2).ok_or("joined metadata required")?),
            Path::new(args.get(3).ok_or("metadata source directory required")?),
            Path::new(args.get(4).ok_or("output required")?),
            args.get(5).ok_or("bzip2 executable required")?,
        ),
        Some("metadata-selected") => metadata::selected(
            Path::new(args.get(2).ok_or("joined metadata required")?),
            Path::new(args.get(3).ok_or("selected candidates JSON required")?),
            Path::new(args.get(4).ok_or("output required")?),
        ),
        Some("richness") => {
            let mut citation_input = None;
            let mut retain_rendered_text = false;
            let mut option = 6;
            while let Some(flag) = args.get(option) {
                match flag.as_str() {
                    "--retain-rendered-text" => retain_rendered_text = true,
                    "--cap-citations" => {
                        let context = args.get(option + 1).ok_or("CAP citation context required")?;
                        let home = args.get(option + 2).ok_or("OpenLegalData home required")?;
                        citation_input = Some((Path::new(context), Path::new(home)));
                        option += 2;
                    }
                    _ => return Err("richness options are --retain-rendered-text and --cap-citations CONTEXT OPENLEGALDATA_HOME".into()),
                }
                option += 1;
            }
            richness::run(
                Path::new(args.get(2).ok_or("pack required")?),
                Path::new(args.get(3).ok_or("output directory required")?),
                args.get(4).map_or(Ok(0), |s| s.parse())?,
                Path::new(args.get(5).ok_or("joined metadata required")?),
                citation_input,
                retain_rendered_text,
            )
        },
        Some("web-run") => {
            let mut plan_fragments = true;
            let mut citation_path = None;
            let mut option = 5;
            while let Some(flag) = args.get(option) {
                match flag.as_str() {
                    "--structure-only" => plan_fragments = false,
                    "--cap-citations" => {
                        option += 1;
                        citation_path = Some(Path::new(args.get(option).ok_or("CAP citation context required")?));
                    }
                    _ => return Err("web-run options are --structure-only and --cap-citations CONTEXT".into()),
                }
                option += 1;
            }
            richness::web_run(
            Path::new(args.get(2).ok_or("browser corpus manifest required")?),
            Path::new(args.get(3).ok_or("OpenLegalData home required")?),
            Path::new(args.get(4).ok_or("output required")?),
            plan_fragments,
            citation_path,
        )
        },
        Some("source-run") => richness::source_run(
            Path::new(args.get(2).ok_or("retained-source JSONL manifest required")?),
            Path::new(args.get(3).ok_or("OpenLegalData home required")?),
            Path::new(args.get(4).ok_or("output directory required")?),
        ),
        Some("witness-run") => witness_run::run(
            Path::new(args.get(2).ok_or("retained census witness directory required")?),
            Path::new(args.get(3).ok_or("inventory output directory required")?),
            args.get(4).ok_or("inventory output byte cap required")?.parse()?,
            args.get(5).filter(|path| path.as_str() != "--semantic-digests").map(|path| Path::new(path)),
            args.iter().skip(5).any(|arg| arg == "--semantic-digests"),
        ),
        Some("run") => run(
            Path::new(args.get(2).ok_or("pack directory required")?),
            Path::new(args.get(3).ok_or("receipt directory required")?),
            args.get(4).map_or(Ok(0), |s| s.parse())?,
        ),
        _ => Err(
            "use: record-census TABLE OUT MAXIMUM_BYTES WITNESS_BYTES FEATURE_LIMIT [--native-aggregate] | census | pack | derive-pack | verify-pack | stream | stream-selected | decode | metadata | metadata-membership | metadata-citations | metadata-selected | catalog | select | richness | web-run | source-run | witness-run | run"
                .into(),
        ),
    }
}
