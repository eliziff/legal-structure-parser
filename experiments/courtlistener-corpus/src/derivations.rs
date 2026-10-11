use crate::{each_record, flush_frame, hash, write_json, Index, Result};
use legal_structure::{
    courtlistener_markup_recipe, restore_courtlistener_markup, CourtListenerMarkupRecipe,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::Path,
    time::Instant,
};

// Literal upstream source preference; this differs from the display preference.
const SOURCES: [&str; 6] = [
    "xml_harvard",
    "html_anon_2020",
    "html_columbia",
    "html_lawbox",
    "html",
    "plain_text",
];
const ORDER: [&str; 8] = [
    "xml_harvard",
    "html_anon_2020",
    "html_columbia",
    "html_lawbox",
    "html",
    "plain_text",
    "html_with_citations",
    "xml_scan",
];

#[derive(Deserialize, Serialize)]
#[serde(tag = "method", deny_unknown_fields)]
enum Recipe {
    Alias {
        from: String,
    },
    CourtListener {
        from: String,
        recipe: CourtListenerMarkupRecipe,
    },
}

pub fn restore(record: &mut Value) -> Result<()> {
    let Some(encoded) = record
        .as_object_mut()
        .ok_or("record is not an object")?
        .remove("rendition_recipes")
    else {
        return Ok(());
    };
    let recipes: BTreeMap<String, Recipe> = serde_json::from_value(encoded)?;
    let raw = &record["opinion"];
    let mut rebuilt = BTreeMap::new();
    for (field, recipe) in recipes {
        if !ORDER.contains(&field.as_str()) || raw.get(&field).is_some() {
            return Err("invalid rendition recipe target".into());
        }
        let (from, value) = match recipe {
            Recipe::Alias { from } => {
                let value = raw[&from]
                    .as_str()
                    .ok_or("recipe source is not retained raw text")?
                    .to_owned();
                (from, value)
            }
            Recipe::CourtListener { from, recipe } => {
                let source = raw[&from]
                    .as_str()
                    .ok_or("recipe source is not retained raw text")?;
                (from, restore_courtlistener_markup(source, &recipe)?)
            }
        };
        if !ORDER.contains(&from.as_str()) {
            return Err("invalid rendition recipe source".into());
        }
        rebuilt.insert(field, value);
    }
    for (field, value) in rebuilt {
        record["opinion"][field] = json!(value);
    }
    record.sort_all_objects();
    Ok(())
}

pub fn pack(input: &Path, out: &Path, maximum_bytes: u64, plain_text_only: bool) -> Result<()> {
    fs::create_dir_all(out)?;
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(out.join("opinions.jsonl.zst"))?;
    let mut index = Index {
        schema: "courtlistener.lossless-zstd-frames.v1".into(),
        rows: 0,
        raw_bytes: 0,
        compressed_bytes: 0,
        level: 9,
        block_bytes: 8 * 1024 * 1024,
        frames: Vec::new(),
    };
    let mut raw = Vec::with_capacity(index.block_bytes);
    let mut ids = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut sources = BTreeMap::<String, usize>::new();
    let started = Instant::now();
    let mut original_digest = Sha256::new();
    let (mut replay_seconds, mut original_bytes) = (0.0, 0usize);
    let original_index = each_record(input, |mut original| {
        original.sort_all_objects();
        let opinion = &original["opinion"];
        if original.get("rendition_recipes").is_some() {
            return Err("source contains the reserved recipe envelope".into());
        }
        let source = SOURCES
            .iter()
            .find(|field| {
                opinion[**field]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
            })
            .copied();
        if let Some(source) = source {
            *sources.entry(source.to_owned()).or_default() += 1;
        }
        let mut encoded = original.clone();
        let mut recipes = BTreeMap::new();
        let mut roots = BTreeMap::<&str, &str>::new();
        for field in ORDER {
            let Some(value) = opinion[field].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            let recipe = if let Some(root) = roots.get(value) {
                Some(Recipe::Alias {
                    from: (*root).into(),
                })
            } else if field == "html_with_citations"
                && (!plain_text_only || source == Some("plain_text"))
            {
                source.and_then(|source| {
                    courtlistener_markup_recipe(
                        opinion[source].as_str().unwrap(),
                        value,
                        source == "plain_text",
                    )
                    .map(|recipe| Recipe::CourtListener {
                        from: roots[opinion[source].as_str().unwrap()].into(),
                        recipe,
                    })
                })
            } else {
                None
            };
            if let Some(recipe) = recipe.filter(|recipe| {
                serde_json::to_vec(recipe).map_or(false, |bytes| {
                    bytes.len() < serde_json::to_vec(&opinion[field]).unwrap().len()
                })
            }) {
                let method = match &recipe {
                    Recipe::Alias { .. } => "alias",
                    Recipe::CourtListener { recipe, .. } if recipe.source_is_plain_text => {
                        "plain_text_and_insertions"
                    }
                    _ => "markup_and_insertions",
                };
                *counts.entry(method.into()).or_default() += 1;
                encoded["opinion"].as_object_mut().unwrap().remove(field);
                recipes.insert(field.to_owned(), recipe);
            } else {
                roots.entry(value).or_insert(field);
            }
        }
        if !recipes.is_empty() {
            encoded["rendition_recipes"] = serde_json::to_value(recipes)?;
        }
        let then = Instant::now();
        let mut restored = encoded.clone();
        restore(&mut restored)?;
        replay_seconds += then.elapsed().as_secs_f64();
        if restored != original {
            return Err("derived pack did not reproduce every original field exactly".into());
        }
        let original_line = serde_json::to_vec(&original)?;
        original_digest.update(&original_line);
        original_digest.update(b"\n");
        original_bytes += original_line.len() + 1;
        let line = serde_json::to_vec(&encoded)?;
        if raw.len() + line.len() + 1 > index.block_bytes {
            flush_frame(&mut file, &mut raw, &mut ids, &mut index, maximum_bytes)?;
        }
        raw.extend_from_slice(&line);
        raw.push(b'\n');
        ids.push(
            opinion["id"]
                .as_str()
                .ok_or("source id missing")?
                .to_owned(),
        );
        index.rows += 1;
        if index.rows % 1000 == 0 {
            eprintln!(
                "derived and exactly replayed {} actual records in {:.2}s",
                index.rows,
                started.elapsed().as_secs_f64()
            );
        }
        Ok(())
    })?;
    flush_frame(&mut file, &mut raw, &mut ids, &mut index, maximum_bytes)?;
    file.flush()?;
    file.sync_all()?;
    index.compressed_bytes = file.metadata()?.len();
    write_json(out.join("index.json"), &index)?;
    // Read the actual persisted candidate through the ordinary corpus reader.
    let mut reopened_digest = Sha256::new();
    let reopened_index = each_record(out, |record| {
        reopened_digest.update(serde_json::to_vec(&record)?);
        reopened_digest.update(b"\n");
        Ok(())
    })?;
    let original_digest = format!("{:x}", original_digest.finalize());
    let reopened_digest = format!("{:x}", reopened_digest.finalize());
    if reopened_index.rows != index.rows || reopened_digest != original_digest {
        return Err("persisted candidate differs from independently retained source rows".into());
    }
    let summary = json!({"schema":"courtlistener.forward-derivation.v1","records":index.rows,"sources":sources,"recipes":counts,
        "annotation_recipe_policy":if plain_text_only{"plain_text_only"}else{"all_admitted_sources"},
        "original_jsonl_bytes":original_bytes,"recipe_jsonl_bytes":index.raw_bytes,"original_zstd_bytes":original_index.compressed_bytes,
        "recipe_zstd_bytes":index.compressed_bytes,"compressed_savings_bytes":original_index.compressed_bytes as i64-index.compressed_bytes as i64,
        "exact_original_rows_verified":index.rows,"persisted_pack_reopened":true,"query_replay_seconds":replay_seconds,
        "source_rows_sha256":original_digest,"reopened_rows_sha256":reopened_digest,
        "source_manifest_sha256":hash(&fs::read(input.join(if input.join("index.json").exists(){"index.json"}else{"source.json"}))?),
        "query_replay_microseconds_per_record":replay_seconds*1_000_000.0/index.rows as f64,"seconds":started.elapsed().as_secs_f64(),
        "engine_source_sha256":legal_structure::ENGINE_SOURCE_SHA256,"original_pack_removed":false,"full_courtlistener_corpus":false,
        "upstream_revision":"79cdb523158861eeabb19f2aea296529d007b992"});
    write_json(out.join("summary.json"), &summary)?;
    eprintln!("{summary}");
    Ok(())
}

pub fn verify(input: &Path, candidate: &Path, out: &Path) -> Result<()> {
    let mut source_digest = Sha256::new();
    let mut source_groups = BTreeMap::<String, usize>::new();
    let mut regeneration = BTreeMap::<String, usize>::new();
    let mut replay_times = BTreeMap::<String, Vec<f64>>::new();
    let source_index = each_record(input, |mut row| {
        if let Some(field) = SOURCES.iter().find(|field| {
            row["opinion"][**field]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        }) {
            *source_groups.entry((*field).into()).or_default() += 1;
            if let Some(cited) = row["opinion"]["html_with_citations"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                if let Some(recipe) = courtlistener_markup_recipe(
                    row["opinion"][*field].as_str().unwrap(),
                    cited,
                    *field == "plain_text",
                ) {
                    let started = Instant::now();
                    let restored = restore_courtlistener_markup(
                        row["opinion"][*field].as_str().unwrap(),
                        &recipe,
                    )?;
                    replay_times
                        .entry((*field).into())
                        .or_default()
                        .push(started.elapsed().as_secs_f64() * 1_000_000.0);
                    if restored != cited {
                        return Err("query-time replay differs from independent cited HTML".into());
                    }
                    *regeneration.entry((*field).into()).or_default() += 1;
                }
            }
        }
        row.sort_all_objects();
        source_digest.update(serde_json::to_vec(&row)?);
        source_digest.update(b"\n");
        Ok(())
    })?;
    let mut candidate_digest = Sha256::new();
    let started = Instant::now();
    let candidate_index = each_record(candidate, |mut row| {
        row.sort_all_objects();
        candidate_digest.update(serde_json::to_vec(&row)?);
        candidate_digest.update(b"\n");
        Ok(())
    })?;
    let source_digest = format!("{:x}", source_digest.finalize());
    let candidate_digest = format!("{:x}", candidate_digest.finalize());
    if source_index.rows != candidate_index.rows || source_digest != candidate_digest {
        return Err("persisted reconstructed rows differ from independent source".into());
    }
    let replay_times = replay_times
        .into_iter()
        .map(|(field, mut values)| {
            values.sort_by(f64::total_cmp);
            let percentile = |p: usize| values[((values.len() - 1) * p).div_ceil(100)];
            let timing = json!({"records":values.len(),"p50_us":percentile(50),"p95_us":percentile(95),
                "max_us":values.last(),"total_us":values.iter().sum::<f64>()});
            (field, timing)
        })
        .collect::<BTreeMap<_, _>>();
    let summary = json!({"schema":"courtlistener.persisted-derivation-proof.v1","records":source_index.rows,
        "source_groups":source_groups,"exact_regeneration_matches":regeneration,
        "query_time_replay":replay_times,"replay_timing_scope":"Rust markup replay only; excludes recipe extraction, I/O, decompression and canonical hashing",
        "canonical_source_rows_sha256":source_digest,"canonical_reconstructed_rows_sha256":candidate_digest,
        "canonicalization":"Sort JSON object keys; preserve every field value and string byte",
        "original_zstd_bytes":source_index.compressed_bytes,"candidate_zstd_bytes":candidate_index.compressed_bytes,
        "compressed_savings_bytes":source_index.compressed_bytes as i64-candidate_index.compressed_bytes as i64,
        "persisted_replay_and_canonical_hash_seconds":started.elapsed().as_secs_f64(),
        "source_removed":false,"full_courtlistener_corpus":false});
    write_json(out, &summary)?;
    eprintln!("{summary}");
    Ok(())
}
