use crate::{hash, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Component, Path},
};

const SCHEMA: &str = "courtlistener.cap-served-citation-context.v1";
const TYPES: [&str; 4] = ["official", "nominative", "parallel", "vendor"];

fn ordered(cap: &Value) -> Vec<(usize, usize, &str)> {
    let mut citations = cap["citations"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, citation)| {
            let cite = citation["cite"].as_str().filter(|cite| !cite.is_empty())?;
            let rank = citation["type"]
                .as_str()
                .and_then(|kind| TYPES.iter().position(|value| *value == kind))
                .unwrap_or(99);
            Some((rank, index, cite))
        })
        .collect::<Vec<_>>();
    citations.sort_by_key(|(rank, index, _)| (*rank, *index));
    citations
}

/// Existing CAP consumer order, preserving original array order within each type.
pub fn ordered_citations(cap: &Value) -> Vec<String> {
    ordered(cap)
        .into_iter()
        .map(|(_, _, cite)| cite.to_owned())
        .collect()
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("CAP context string missing: {key}").into())
}

fn number(value: &Value, key: &str) -> Result<u64> {
    value[key]
        .as_u64()
        .ok_or_else(|| format!("CAP context integer missing: {key}").into())
}

fn archive_path(value: &str) -> Result<String> {
    let trimmed = value.trim();
    let path = trimmed
        .strip_prefix("https://archive.org/download/")
        .unwrap_or_else(|| trimmed.trim_start_matches('/'));
    if !path.ends_with(".json")
        || path.contains("..")
        || !path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
    {
        return Err("CAP original archive filepath is invalid".into());
    }
    Ok(path.to_owned())
}

fn source_path(root: &Path, relative: &str) -> Result<std::path::PathBuf> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("CAP evidence path must remain relative to its owning root".into());
    }
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(root) {
        return Err("CAP evidence path escaped its owning root".into());
    }
    Ok(path)
}

fn canonical_url(id: &str, slug: &str) -> Result<String> {
    if id.is_empty()
        || !id.bytes().all(|byte| byte.is_ascii_digit())
        || slug.is_empty()
        || slug.chars().any(|value| matches!(value, '/' | '?' | '#'))
    {
        return Err("CAP context canonical cluster identity is invalid".into());
    }
    Ok(format!(
        "https://www.courtlistener.com/opinion/{id}/{slug}/"
    ))
}

enum Witness {
    Declaration(Value),
    Catalog(Value),
}

type Witnesses = BTreeMap<String, BTreeMap<u64, Vec<Witness>>>;

fn add_witness(rows: &mut Witnesses, file: &str, ordinal: u64, witness: Witness) -> Result<()> {
    if ordinal == 0 {
        return Err("CAP evidence source ordinals are one based".into());
    }
    rows.entry(file.to_owned())
        .or_default()
        .entry(ordinal)
        .or_default()
        .push(witness);
    Ok(())
}

fn reference(context: &Value, witness: &Value, field: &str) -> Result<Value> {
    let path = text(witness, field)?;
    Ok(
        json!({"path":path,"sha256":text(&context["sources"][path],"sha256")?,
        "source_ordinal":number(witness,"source_ordinal")?}),
    )
}

fn verify_witness_files(root: &Path, context: &Value, witnesses: Witnesses) -> Result<Value> {
    let mut verified = Vec::new();
    for (name, mut expected) in witnesses {
        let receipt = &context["sources"][&name];
        if text(receipt, "path")? != name {
            return Err("CAP evidence receipt path disagrees with its key".into());
        }
        let mut input = BufReader::new(File::open(source_path(root, &name)?)?);
        let mut digest = Sha256::new();
        let (mut bytes, mut ordinal, mut verified_rows) = (0u64, 0u64, 0usize);
        let mut line = Vec::new();
        loop {
            line.clear();
            if input.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            bytes += line.len() as u64;
            digest.update(&line);
            ordinal += 1;
            let Some(checks) = expected.remove(&ordinal) else {
                continue;
            };
            let mut row: Value = serde_json::from_slice(&line)?;
            for check in checks {
                match check {
                    Witness::Declaration(declaration) => {
                        if row != declaration {
                            return Err("CAP context changed an original source declaration".into());
                        }
                    }
                    Witness::Catalog(witness) => {
                        let cluster = &row["cluster_metadata"];
                        if row["cluster_id"] != witness["cluster_id"]
                            || row["opinion_id"] != witness["opinion_id"]
                            || row["url"] != witness["canonical_url"]
                            || cluster["id"] != witness["cluster_id"]
                            || cluster["filepath_json_harvard"] != witness["filepath_json_harvard"]
                            || canonical_url(text(cluster, "id")?, text(cluster, "slug")?)?
                                != text(&witness, "canonical_url")?
                        {
                            return Err(
                                "CAP context does not match its official catalog identity witness"
                                    .into(),
                            );
                        }
                        // serde_json preserves input object order in this crate;
                        // the prepared row-value digest explicitly sorts all keys.
                        row.sort_all_objects();
                        if hash(&serde_json::to_vec(&row)?) != text(&witness, "row_value_sha256")? {
                            return Err(
                                "CAP catalog complete original row-value digest disagrees".into()
                            );
                        }
                    }
                }
                verified_rows += 1;
            }
        }
        let actual_hash = format!("{:x}", digest.finalize());
        if !expected.is_empty()
            || bytes != number(receipt, "bytes")?
            || actual_hash != text(receipt, "sha256")?
            || receipt["physical_lines"]
                .as_u64()
                .is_some_and(|lines| lines != ordinal)
        {
            return Err(
                "CAP source evidence file digest, length or source ordinal disagrees".into(),
            );
        }
        verified.push(json!({"path":name,"sha256":actual_hash,"bytes":bytes,
            "physical_lines":ordinal,"verified_witnesses":verified_rows}));
    }
    Ok(json!(verified))
}

struct Binding {
    url: String,
    archive_path: Option<String>,
    citations: Vec<String>,
    served_shas: BTreeSet<String>,
    provenance: Value,
}

pub struct CitationContext {
    bindings: BTreeMap<String, Binding>,
    provenance: Value,
}

impl CitationContext {
    /// Read originals once and bind their labels to exact frozen identity evidence.
    /// Declared repository-relative evidence files resolve from the CLI workspace cwd.
    pub fn load(path: &Path, corpus_home: &Path) -> Result<Self> {
        let bytes = fs::read(path)?;
        let context_sha = hash(&bytes);
        let context: Value = serde_json::from_slice(&bytes)?;
        if context["schema"] != SCHEMA || context["ordering_rule"]["type_order"] != json!(TYPES) {
            return Err("CAP citation context schema or ordering rule is unsupported".into());
        }
        let corpus_home = corpus_home.canonicalize()?;
        let workspace = std::env::current_dir()?.canonicalize()?;
        let sources = context["cap_sources"]
            .as_object()
            .ok_or("CAP original source map missing")?;
        let clusters = context["clusters"]
            .as_object()
            .ok_or("CAP cluster identity map missing")?;
        let mut witnesses = Witnesses::new();
        let mut verified_sources = BTreeMap::new();
        let mut cap_bytes = 0u64;
        for (sha, source) in sources {
            let original_bytes =
                fs::read(source_path(&corpus_home, text(source, "corpus_path")?)?)?;
            if text(source, "sha256")? != sha
                || hash(&original_bytes) != *sha
                || original_bytes.len() as u64 != number(source, "bytes")?
                || archive_path(text(source, "source_url")?)? != text(source, "archive_path")?
            {
                return Err("Retained CAP original bytes or archive identity disagree".into());
            }
            let original: Value = serde_json::from_slice(&original_bytes)?;
            if !original["citations"].is_array()
                || original["citations"] != source["original_citations"]
            {
                return Err("CAP context omitted or changed complete original citations".into());
            }
            let ordered = ordered(&original);
            let labels = ordered
                .iter()
                .map(|(_, _, cite)| (*cite).to_owned())
                .collect::<Vec<_>>();
            let indexes = ordered
                .iter()
                .map(|(_, index, _)| *index)
                .collect::<Vec<_>>();
            let mut unknown_types = Vec::new();
            let mut original_labels = Vec::new();
            for citation in original["citations"].as_array().unwrap() {
                if let Some(cite) = citation["cite"].as_str().filter(|cite| !cite.is_empty()) {
                    original_labels.push(cite.to_owned());
                }
                if !citation["type"]
                    .as_str()
                    .is_some_and(|kind| TYPES.contains(&kind))
                    && !unknown_types.contains(&citation["type"])
                {
                    unknown_types.push(citation["type"].clone());
                }
            }
            if source["page_citations"] != json!(labels)
                || source["ordered_original_indexes"] != json!(indexes)
                || source["ordering"]["type_order"] != json!(TYPES)
                || source["ordering"]["unknown_types"] != json!(unknown_types)
                || source["ordering"]["original_order_equal_to_adapter"]
                    != json!(original_labels == labels)
            {
                return Err("CAP context ordered labels disagree with retained originals".into());
            }
            let manifests = source["manifest_sources"]
                .as_array()
                .filter(|rows| !rows.is_empty())
                .ok_or("CAP source manifest declaration missing")?;
            for row in manifests {
                let declaration = &row["declaration"];
                if declaration["format"] != "cap-json"
                    || declaration["sha256"] != *sha
                    || declaration["corpus_path"] != source["corpus_path"]
                    || archive_path(text(declaration, "url")?)? != text(source, "archive_path")?
                    || declaration["bytes"]
                        .as_u64()
                        .is_some_and(|size| size != original_bytes.len() as u64)
                    || declaration
                        .get("citations")
                        .is_some_and(|value| *value != original["citations"])
                {
                    return Err(
                        "CAP source declaration disagrees with its retained original".into(),
                    );
                }
                add_witness(
                    &mut witnesses,
                    text(row, "manifest")?,
                    number(row, "source_ordinal")?,
                    Witness::Declaration(declaration.clone()),
                )?;
            }
            let identities = source["identity_witnesses"]
                .as_array()
                .ok_or("CAP catalog identity witnesses missing")?;
            for row in identities {
                if archive_path(text(row, "filepath_json_harvard")?)?
                    != text(source, "archive_path")?
                {
                    return Err(
                        "CAP original archive path disagrees with official filepath witness".into(),
                    );
                }
                add_witness(
                    &mut witnesses,
                    text(row, "source_catalog")?,
                    number(row, "source_ordinal")?,
                    Witness::Catalog(row.clone()),
                )?;
            }
            cap_bytes += original_bytes.len() as u64;
            verified_sources.insert(sha.clone(), labels);
        }
        let mut bindings = BTreeMap::new();
        let (mut known, mut unknown) = (0usize, 0usize);
        for (id, cluster) in clusters {
            let url = canonical_url(id, text(cluster, "slug")?)?;
            if text(cluster, "cluster_id")? != id || text(cluster, "url")? != url {
                return Err("CAP context cluster key and canonical identity disagree".into());
            }
            let status = text(cluster, "status")?;
            let mut served_shas = BTreeSet::new();
            let served = cluster["served"]
                .as_array()
                .ok_or("CAP served witnesses missing")?;
            for row in served {
                let declaration = &row["declaration"];
                let served_sha = text(row, "sha256")?;
                if declaration["format"] != "html"
                    || declaration["sha256"] != served_sha
                    || declaration["cluster_id"] != *id
                    || declaration["url"] != url
                    || declaration["corpus_path"] != row["corpus_path"]
                {
                    return Err("Frozen served HTML witness disagrees with cluster identity".into());
                }
                served_shas.insert(served_sha.to_owned());
                add_witness(
                    &mut witnesses,
                    text(row, "manifest")?,
                    number(row, "source_ordinal")?,
                    Witness::Declaration(declaration.clone()),
                )?;
            }
            let source_sha = cluster
                .get("cap_sha256")
                .ok_or("CAP cluster source digest field missing")?;
            if !source_sha.is_null() && !source_sha.is_string() {
                return Err("CAP cluster source digest must be a string or null".into());
            }
            let (archive, citations, receipt) = if let Some(sha) = source_sha.as_str() {
                let source = sources.get(sha).ok_or("Known CAP source digest missing")?;
                let labels = verified_sources
                    .get(sha)
                    .ok_or("Known CAP original was not verified")?;
                let identities = source["identity_witnesses"].as_array().unwrap();
                if identities.is_empty()
                    || identities
                        .iter()
                        .any(|row| row["cluster_id"] != *id || row["canonical_url"] != url)
                    || source["manifest_sources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|row| {
                            let declaration = &row["declaration"];
                            declaration
                                .get("cluster_id")
                                .is_some_and(|value| *value != *id)
                                || declaration
                                    .get("opinion_url")
                                    .is_some_and(|value| *value != url)
                        })
                {
                    return Err(
                        "Known CAP cluster does not respect its exact identity witnesses".into(),
                    );
                }
                let citations = if !source["ordering"]["unknown_types"]
                    .as_array()
                    .unwrap()
                    .is_empty()
                {
                    if status != "unknown_cap_citation_type"
                        || cluster["page_citations"] != json!([])
                    {
                        return Err("Unknown CAP citation types supplied unsupported labels".into());
                    }
                    unknown += 1;
                    Vec::new()
                } else {
                    if !matches!(
                        status,
                        "matched_cap_and_frozen_served" | "matched_cap_source_only"
                    ) || (status == "matched_cap_source_only") != served.is_empty()
                        || cluster["page_citations"] != json!(labels)
                    {
                        return Err(
                            "Known CAP cluster status or labels disagree with original evidence"
                                .into(),
                        );
                    }
                    known += 1;
                    labels.clone()
                };
                (
                    Some(text(source, "archive_path")?.to_owned()),
                    citations,
                    json!({"context_sha256":context_sha,"status":status,"cap_sha256":sha,
                        "official_filepath_json_harvard":identities[0]["filepath_json_harvard"],
                        "identity_witness":reference(&context,&identities[0],"source_catalog")?,
                        "source_record":reference(&context,&source["manifest_sources"][0],"manifest")?}),
                )
            } else {
                if status != "unknown_no_retained_cap" || cluster["page_citations"] != json!([]) {
                    return Err("Unknown CAP cluster supplied unsupported citation labels".into());
                }
                unknown += 1;
                (
                    None,
                    Vec::new(),
                    json!({"context_sha256":context_sha,"status":status,"cap_sha256":null}),
                )
            };
            bindings.insert(
                id.clone(),
                Binding {
                    url,
                    archive_path: archive,
                    citations,
                    served_shas,
                    provenance: receipt,
                },
            );
        }
        let verified_files = verify_witness_files(&workspace, &context, witnesses)?;
        let context_path = path.canonicalize()?;
        let context_file = context_path
            .strip_prefix(&workspace)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        Ok(Self {
            bindings,
            provenance: json!({"schema":SCHEMA,
            "context_file":context_file,
            "context_sha256":context_sha,"context_bytes":bytes.len(),
            "verified_cap_sources":verified_sources.len(),"verified_cap_bytes":cap_bytes,
            "known_clusters":known,"unknown_clusters":unknown,"verified_source_files":verified_files,
            "type_order":TYPES,"ordering_scope":"Stable original CAP array index within type; rendered and citation-CSV order unused"}),
        })
    }

    pub fn cluster(
        &self,
        id: &str,
        url: &str,
        filepath: Option<&str>,
        served_sha: Option<&str>,
    ) -> Result<Vec<String>> {
        let Some(binding) = self.bindings.get(id) else {
            return Ok(Vec::new());
        };
        let Some(archive) = &binding.archive_path else {
            return Ok(Vec::new());
        };
        if url != binding.url
            || filepath
                .map(archive_path)
                .transpose()?
                .is_some_and(|value| value != *archive)
            || served_sha.is_some_and(|value| !binding.served_shas.contains(value))
        {
            return Err("CAP label request disagrees with frozen cluster, filepath or served digest evidence".into());
        }
        Ok(binding.citations.clone())
    }

    pub fn provenance(&self) -> &Value {
        &self.provenance
    }

    pub fn cluster_provenance(&self, id: &str) -> Value {
        self.bindings.get(id).map(|binding| binding.provenance.clone()).unwrap_or_else(||
            json!({"context_sha256":self.provenance["context_sha256"],"status":"unknown_cluster_not_in_context","cap_sha256":null}))
    }
}
