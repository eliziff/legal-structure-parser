//! Actual parser inventory over retained census frames; never an independent gold oracle.
use crate::{compile, hash, Result, RENDITIONS};
use legal_structure::{
    document_fingerprint, Derivation, DocumentFingerprint, DocumentKind, DocumentQuery, DocumentStructure,
    NodeKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
    time::Instant,
};

const RECEIPT_RESERVE: u64 = 64 * 1024;
type Counts = BTreeMap<String, u64>;

#[derive(Deserialize)]
struct Index {
    schema: String,
    witness_ordinal: u64,
    source_ordinal_zero_based: u64,
    compressed_offset: u64,
    compressed_bytes: usize,
    raw_bytes: usize,
    raw_sha256: String,
    #[serde(default)]
    compressed_sha256: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct SourceHash {
    utf8_bytes: usize,
    sha256: String,
}

#[derive(Deserialize)]
struct Witness {
    schema: String,
    source_ordinal_zero_based: u64,
    columns: Vec<String>,
    values: Vec<Value>,
    original_csv_row_bytes: usize,
    original_csv_row_sha256: String,
    source_string_hashes: BTreeMap<String, SourceHash>,
}

#[derive(Default, Serialize)]
struct Totals {
    fields: Counts,
    passage_range_coverage: Counts,
    nodes_by_derivation: BTreeMap<String, Counts>,
    native_roles_by_source_tag: BTreeMap<String, Counts>,
    source_tags: Counts,
    source_roles: Counts,
    relationships: Counts,
    diagnostic_codes: Counts,
    analysis_seconds: f64,
}

#[derive(Default, Serialize)]
struct Facts {
    markup_sha256: Option<String>,
    text_sha256: String,
    query_text_utf16: usize,
    markup_elements: usize,
    passage_range_coverage: Counts,
    nodes_by_derivation: BTreeMap<String, Counts>,
    native_roles_by_source_tag: BTreeMap<String, Counts>,
    source_tags: Counts,
    source_roles: Counts,
    relationships: Counts,
    diagnostic_codes: Counts,
    diagnostics: Vec<Value>,
    discrepancies: Vec<Value>,
    local_fragment_observations: Vec<Value>,
    source_paragraph_marker_observations: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_digests: Option<SemanticDigests>,
}

#[derive(Serialize)]
struct SemanticDigests {
    document_fingerprint: DocumentFingerprint,
    rendered_text_present: bool,
    document_outline_sha256: String,
    source_opinion_components_sha256: String,
}

fn add(counts: &mut Counts, name: &str, amount: u64) {
    *counts.entry(name.to_owned()).or_default() += amount;
}

fn merge(counts: &mut Counts, values: &Counts) {
    for (name, amount) in values {
        add(counts, name, *amount);
    }
}

fn proof(path: &Path) -> Result<Value> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        digest.update(&buffer[..count]);
        bytes += count as u64;
    }
    Ok(json!({"path":path.to_string_lossy().replace('\\',"/"),"bytes":bytes,"sha256":format!("{:x}",digest.finalize())}))
}

fn facts(doc: &DocumentStructure, semantic_digests: bool) -> Facts {
    let query = DocumentQuery::new();
    let graph = query.source_markup(doc);
    let mut result = Facts {
        markup_sha256: graph.markup_sha256.map(str::to_owned),
        text_sha256: doc.text_sha256.clone(),
        query_text_utf16: doc.query_text().encode_utf16().count(),
        markup_elements: graph.markup_elements.len(),
        ..Facts::default()
    };
    for node in &doc.nodes {
        let source = match node.source {
            Derivation::Native => "native",
            Derivation::Heuristic => "heuristic",
            Derivation::Model => "model",
        };
        add(result.nodes_by_derivation.entry(source.into()).or_default(), node.kind.name(), 1);
        if node.source == Derivation::Native {
            add(result.native_roles_by_source_tag.entry(node.markup_tag.as_deref().unwrap_or("(no_markup_tag)").into()).or_default(), node.kind.name(), 1);
        }
    }
    // This public projection contains Paragraph/Prose nodes, never a Section or
    // viewer fallback. Coverage describes passage addressability, not legal truth.
    for name in ["blocks", "maximum_block_utf16", "blocks_over_2000_utf16",
        "blocks_over_10000_utf16", "blocks_spanning_whole_query_text", "blocks_outside_query_text",
        "paragraph_node_blocks", "source_backed_native_prose_blocks", "other_prose_blocks"] {
        add(&mut result.passage_range_coverage, name, 0);
    }
    let mut passage_ranges = Vec::new();
    for block in query.blocks(doc, Some(DocumentKind::Paragraph)) {
        let length = block.end.saturating_sub(block.start);
        add(&mut result.passage_range_coverage, "blocks", 1);
        let maximum = result.passage_range_coverage.entry("maximum_block_utf16".into()).or_default();
        *maximum = (*maximum).max(length as u64);
        if length > 2_000 { add(&mut result.passage_range_coverage, "blocks_over_2000_utf16", 1); }
        if length > 10_000 { add(&mut result.passage_range_coverage, "blocks_over_10000_utf16", 1); }
        if block.start == 0 && block.end == result.query_text_utf16 && length != 0 {
            add(&mut result.passage_range_coverage, "blocks_spanning_whole_query_text", 1);
        }
        if block.start > block.end || block.end > result.query_text_utf16 {
            add(&mut result.passage_range_coverage, "blocks_outside_query_text", 1);
        }
        passage_ranges.push((block.start.min(result.query_text_utf16), block.end.min(result.query_text_utf16)));
    }
    for node in doc.nodes.iter().filter(|node| node.kind == NodeKind::Prose
        || (node.kind == NodeKind::Paragraph && node.label.is_some())) {
        let kind = if node.kind == NodeKind::Paragraph { "paragraph_node_blocks" }
            else if node.source == Derivation::Native && node.markup_start.is_some() { "source_backed_native_prose_blocks" }
            else { "other_prose_blocks" };
        add(&mut result.passage_range_coverage, kind, 1);
    }
    passage_ranges.sort_unstable();
    let mut covered = 0usize;
    let mut covered_end = 0usize;
    for (start, end) in passage_ranges {
        covered += end.saturating_sub(start.max(covered_end));
        covered_end = covered_end.max(end);
    }
    add(&mut result.passage_range_coverage, "query_text_utf16", result.query_text_utf16 as u64);
    add(&mut result.passage_range_coverage, "covered_utf16", covered as u64);
    add(&mut result.passage_range_coverage, "uncovered_utf16", result.query_text_utf16.saturating_sub(covered) as u64);
    let mut ids = BTreeMap::<&str, Vec<usize>>::new();
    let mut names = BTreeMap::<&str, Vec<usize>>::new();
    for (index, element) in graph.markup_elements.iter().enumerate() {
        add(&mut result.source_tags, &element.tag, 1);
        if let Some(id) = element.attributes.get("id").filter(|id| !id.is_empty()) {
            ids.entry(id.as_str()).or_default().push(index);
        }
        if element.tag.eq_ignore_ascii_case("a") {
            if let Some(name) = element.attributes.get("name").filter(|name| !name.is_empty()) {
                names.entry(name.as_str()).or_default().push(index);
            }
        }
    }
    let extents = |indexes: &[usize]| indexes.iter()
        .map(|&index| [graph.markup_elements[index].markup_start, graph.markup_elements[index].markup_end]).collect::<Vec<_>>();
    for (kind, targets) in [("duplicate_source_id", &ids), ("duplicate_A_name", &names)] {
        for (id, indexes) in targets.iter().filter(|(_, indexes)| indexes.len() > 1) {
            add(&mut result.relationships, kind, 1);
            result.discrepancies.push(json!({"kind":kind,"id":id,"source_indexes":indexes,
                "markup_extents":extents(indexes),"ID_has_lookup_priority":ids.contains_key(*id)}));
        }
    }
    for (index, element) in graph.markup_elements.iter().enumerate() {
        if !element.tag.eq_ignore_ascii_case("a") { continue; }
        let Some(fragment) = element.attributes.get("href").and_then(|href| href.strip_prefix('#')) else { continue; };
        add(&mut result.relationships, "local_links", 1);
        let percent_escape_present = fragment.as_bytes().windows(3)
            .any(|bytes| bytes[0] == b'%' && bytes[1].is_ascii_hexdigit() && bytes[2].is_ascii_hexdigit());
        let (status, targets) = if fragment.is_empty() {
            add(&mut result.relationships, "local_fragment_document_top_scope", 1);
            ("not_applicable", None)
        } else {
            let targets = ids.get(fragment).or_else(|| names.get(fragment));
            let status = if query.source_element(doc, fragment).is_some() { "found" }
                else if targets.is_some() { "ambiguous" } else { "missing" };
            add(&mut result.relationships, &format!("local_fragment_exact_literal_lookup_{status}"), 1);
            if percent_escape_present {
                add(&mut result.relationships, &format!("local_fragment_percent_escape_exact_literal_lookup_{status}"), 1);
            }
            (status, targets)
        };
        if fragment.is_empty() || percent_escape_present || status != "found" {
            result.local_fragment_observations.push(json!({"kind":"local_fragment_observation",
                "source_index":index,"markup_start":element.markup_start,"markup_end":element.markup_end,"href":element.attributes["href"],
                "raw_fragment":fragment,"percent_escape_present":percent_escape_present,
                "status":if fragment.is_empty() {"document_top_scope"} else {"unqualified_exact_literal_lookup"},
                "exact_literal_lookup_status":status,"lookup_boundary":"retained_parsed_attributes_without_percent_decoding",
                "navigation_evaluated":false,"target_source_indexes":targets,"target_markup_extents":targets.map(|targets|extents(targets))}));
        }
    }
    let components = query.source_opinion_components(doc);
    add(&mut result.source_roles, "opinions", components.opinions.len() as u64);
    for author in components.opinions.iter().flat_map(|opinion| &opinion.authors).chain(&components.unowned_authors) {
        add(&mut result.source_roles, "authors", 1);
        if author.author.is_none() {
            add(&mut result.source_roles, "authors_without_native_projection", 1);
        } else {
            add(&mut result.source_roles, "authors_with_native_projection", 1);
        }
    }
    add(&mut result.source_roles, "unowned_authors", components.unowned_authors.len() as u64);
    for author in &components.unowned_authors {
        result.discrepancies.push(json!({"kind":"unowned_source_author","markup_start":author.source_element.markup_start,
            "markup_end":author.source_element.markup_end,
            "source_id":author.source_element.attributes.get("id"),"native_node_id":author.author.map(|node|&node.id)}));
    }
    let mut opinions_by_start = BTreeMap::<usize, Option<usize>>::new();
    for (index, component) in components.opinions.iter().enumerate() {
        if let Some(start) = component.opinion.markup_start {
            opinions_by_start.entry(start).and_modify(|owner| *owner = None).or_insert(Some(index));
        }
    }
    let mut owners = Vec::<Option<usize>>::with_capacity(graph.markup_elements.len());
    let mut paragraphs_by_start = BTreeMap::<usize, Option<usize>>::new();
    for (index, element) in graph.markup_elements.iter().enumerate() {
        let owner = opinions_by_start.get(&element.markup_start).copied()
            .unwrap_or_else(|| element.parent.and_then(|parent| owners[parent]));
        owners.push(owner);
        if element.tag.eq_ignore_ascii_case("p") && element.range.start < element.range.end {
            paragraphs_by_start.entry(element.range.start).and_modify(|entry| *entry = None).or_insert(Some(index));
        }
    }
    let opinion_detail = |index: usize| {
        let component = &components.opinions[index];
        json!({"node_id":component.opinion.id,"markup_start":component.opinion.markup_start,
            "source_tag":component.opinion.markup_tag,"source_anchor":component.source_anchor,
            "parent_opinion_id":component.parent_opinion_id,"range":doc.query_range(component.opinion)})
    };
    let markers = query.source_paragraph_markers(doc);
    add(&mut result.source_roles, "source_paragraph_markers", markers.markers.len() as u64);
    for marker in &markers.markers {
        let resolved = query.source_paragraph_marker(doc, marker.source_element.markup_start);
        let getter_agrees = resolved.as_ref().is_some_and(|found| {
            std::ptr::eq(found.source_element, marker.source_element)
                && found.marker_range == marker.marker_range && found.number_range == marker.number_range
                && found.printed_marker == marker.printed_marker && found.number == marker.number
        });
        add(&mut result.relationships, if getter_agrees { "source_paragraph_marker_getter_agrees" }
            else { "source_paragraph_marker_getter_differs" }, 1);
        if !getter_agrees {
            result.discrepancies.push(json!({"kind":"source_paragraph_marker_getter_differs",
                "markup_start":marker.source_element.markup_start,"marker_range":marker.marker_range}));
        }
        let bracketnum = marker.marker_elements.iter().any(|element| element.tag.eq_ignore_ascii_case("bracketnum"));
        if bracketnum { add(&mut result.source_roles, "source_paragraph_markers_with_bracketnum", 1); }
        let source_index = graph.markup_elements.binary_search_by_key(&marker.source_element.markup_start,
            |element| element.markup_start).ok();
        result.source_paragraph_marker_observations.push(json!({
            "markup_start":marker.source_element.markup_start,"markup_end":marker.source_element.markup_end,
            "source_index":source_index,"physical_p_range":marker.source_element.range,
            "marker_range":marker.marker_range,"number_range":marker.number_range,
            "printed_marker":marker.printed_marker,"number":marker.number,"source_anchor":marker.source_anchor,
            "getter_agrees":getter_agrees,"bracketnum_present":bracketnum,
            "native_source_anchor_node_id":marker.source_anchor.and_then(|anchor| query.source_anchor(doc, anchor)).map(|node|&node.id),
            "nearest_represented_source_opinion":source_index.and_then(|index| owners[index]).map(&opinion_detail),
            "source_ancestor_openings":marker.source_ancestors.iter().map(|element|element.markup_start).collect::<Vec<_>>(),
            "marker_elements":marker.marker_elements.iter().map(|element|json!({"markup_start":element.markup_start,
                "tag":element.tag,"range":element.range})).collect::<Vec<_>>()
        }));
    }
    let mut opinion_starts = components.opinions.iter().enumerate()
        .map(|(index, component)|(doc.query_range(component.opinion).start, index)).collect::<Vec<_>>();
    opinion_starts.sort_unstable();
    for node in doc.nodes.iter().filter(|node| node.kind == NodeKind::Paragraph && node.source == Derivation::Heuristic) {
        add(&mut result.relationships, "heuristic_paragraphs_boundary_observed", 1);
        let range = doc.query_range(node);
        let Some(source_index) = paragraphs_by_start.get(&range.start).copied().flatten() else {
            add(&mut result.relationships, "heuristic_paragraphs_without_unique_physical_p_start", 1);
            continue;
        };
        add(&mut result.relationships, "heuristic_paragraphs_with_unique_physical_p_start", 1);
        let element = &graph.markup_elements[source_index];
        let owner = owners[source_index];
        if owner.is_none() { add(&mut result.relationships, "heuristic_paragraphs_without_represented_source_owner", 1); }
        let exceeds_owner = owner.is_some_and(|index| range.end > doc.query_range(components.opinions[index].opinion).end);
        let first = opinion_starts.partition_point(|&(start, _)| start <= range.start);
        let entered = opinion_starts[first..].iter().take_while(|&&(start, _)| start < range.end)
            .filter(|&&(_, index)| Some(index) != owner).map(|&(_, index)| {
                let opinion = components.opinions[index].opinion;
                json!({"opinion":opinion_detail(index),"opening_inside_physical_p":opinion.markup_start
                    .is_some_and(|start| start >= element.markup_start && start < element.markup_end)})
            }).collect::<Vec<_>>();
        if exceeds_owner { add(&mut result.relationships, "heuristic_paragraphs_past_represented_owner_end", 1); }
        if !entered.is_empty() {
            add(&mut result.relationships, "heuristic_paragraphs_entering_represented_writing", 1);
            add(&mut result.relationships, "represented_writing_starts_inside_heuristic_paragraphs", entered.len() as u64);
        }
        if exceeds_owner || !entered.is_empty() {
            add(&mut result.relationships, "heuristic_paragraphs_crossing_represented_writing_boundaries", 1);
            result.discrepancies.push(json!({"kind":"heuristic_paragraph_crosses_represented_writing_boundary",
                "node_id":node.id,"label":node.label,"range":range,"source_index":source_index,
                "markup_start":element.markup_start,"markup_end":element.markup_end,"physical_p_range":element.range,
                "nearest_represented_source_opinion":owner.map(&opinion_detail),"past_owner_end":exceeds_owner,
                "entered_source_opinions":entered}));
        }
    }
    add(&mut result.source_roles, "notes", doc.notes.len() as u64);
    add(&mut result.source_roles, "note_references", doc.notes.iter().map(|note|note.references.len() as u64).sum());
    for diagnostic in &doc.diagnostics {
        add(&mut result.diagnostic_codes, &diagnostic.code, 1);
        result.diagnostics.push(json!(diagnostic));
    }
    if semantic_digests {
        result.semantic_digests = Some(SemanticDigests {
            document_fingerprint: document_fingerprint(doc),
            rendered_text_present: doc.rendered_text.is_some(),
            document_outline_sha256: hash(&serde_json::to_vec(
                &legal_structure_features::document_outline(doc, false, None),
            ).expect("outline digest serialization")),
            source_opinion_components_sha256: hash(&serde_json::to_vec(&components)
                .expect("source opinion digest serialization")),
        });
    }
    result
}

fn persist(file: &mut File, value: &Value, bytes: &mut u64, maximum: u64) -> Result<bool> {
    let mut raw = serde_json::to_vec(value)?;
    raw.push(b'\n');
    let frame = zstd::bulk::compress(&raw, 3)?;
    if bytes.checked_add(frame.len() as u64).and_then(|n|n.checked_add(RECEIPT_RESERVE))
        .is_none_or(|n| n > maximum) { return Ok(false); }
    file.write_all(&frame)?;
    *bytes += frame.len() as u64;
    Ok(true)
}

pub(super) fn run(home: &Path, out: &Path, maximum: u64, bindings_path: Option<&Path>, semantic_digests: bool) -> Result<()> {
    if maximum < RECEIPT_RESERVE { return Err("witness-run cap must reserve at least 65536 receipt bytes".into()); }
    let index_path = home.join("witness-index.jsonl");
    let data_path = home.join("witnesses.jsonl.zst");
    let index_proof = proof(&index_path)?;
    let data_proof = proof(&data_path)?;
    let bindings = if let Some(path) = bindings_path {
        let expected: Value = serde_json::from_reader(File::open(path)?)?;
        for (key, actual) in [("witness_index", &index_proof), ("witnesses", &data_proof)] {
            let expected = expected.get(key).ok_or_else(||format!("bound {key} identity required"))?;
            if expected["bytes"] != actual["bytes"] || expected["sha256"] != actual["sha256"] {
                return Err(format!("bound {key} identity differs").into());
            }
        }
        Some(json!({"file":proof(path)?,"witness_index":expected.get("witness_index"),"witnesses":expected.get("witnesses")}))
    } else { None };
    fs::create_dir_all(out)?;
    let mut receipt = File::options().write(true).create_new(true).open(out.join("summary.json"))?;
    let initial = b"{\"schema\":\"courtlistener.native-witness-inventory.v1\",\"complete\":false,\"status\":\"running\"}\n";
    receipt.write_all(initial)?;
    let mut output = File::options().write(true).create_new(true).open(out.join("observations.jsonl.zst"))?;
    let started = Instant::now();
    let mut totals = BTreeMap::<String, Totals>::new();
    let mut rows = 0u64;
    let mut bytes = 0u64;
    let mut cap_stop = false;
    let mut first_omitted = Value::Null;
    let mut current_witness = Value::Null;
    let mut columns = None::<Vec<String>>;
    let result = (|| -> Result<()> {
        let mut index_file = BufReader::new(File::open(&index_path)?);
        let mut data_file = File::open(&data_path)?;
        let mut index_digest = Sha256::new();
        let mut data_digest = Sha256::new();
        let mut index_line = Vec::new();
        let mut index_bytes = 0u64;
        let mut data_bytes = 0u64;
        while index_file.read_until(b'\n', &mut index_line)? != 0 {
            index_digest.update(&index_line);
            current_witness = json!({"stage":"original_frame_verification","witness_ordinal":rows,"index_offset":index_bytes,
                "index_bytes":index_line.len(),"index_line_sha256":hash(&index_line)});
            index_bytes += index_line.len() as u64;
            let entry: Index = serde_json::from_slice(&index_line)?;
            index_line.clear();
            current_witness["source_ordinal_zero_based"] = json!(entry.source_ordinal_zero_based);
            current_witness["compressed_offset"] = json!(entry.compressed_offset);
            current_witness["compressed_bytes"] = json!(entry.compressed_bytes);
            current_witness["raw_bytes"] = json!(entry.raw_bytes);
            current_witness["raw_sha256"] = json!(entry.raw_sha256);
            if entry.schema != "courtlistener.source-census-witness-index.v1"
                || entry.witness_ordinal != rows || entry.compressed_offset != data_bytes
                || entry.compressed_bytes as u64 > data_proof["bytes"].as_u64().unwrap().saturating_sub(data_bytes) {
                return Err("census witness index ordering/extent differs".into());
            }
            let mut compressed = vec![0; entry.compressed_bytes];
            data_file.read_exact(&mut compressed)?;
            data_digest.update(&compressed);
            data_bytes += compressed.len() as u64;
            let compressed_sha = hash(&compressed);
            if entry.compressed_sha256.as_ref().is_some_and(|expected| expected != &compressed_sha) {
                return Err("bound compressed witness frame differs".into());
            }
            let raw = zstd::bulk::decompress(&compressed, entry.raw_bytes)?;
            if raw.len() != entry.raw_bytes || hash(&raw) != entry.raw_sha256 { return Err("decoded original witness hash differs".into()); }
            let witness: Witness = serde_json::from_slice(&raw)?;
            drop(raw);
            drop(compressed);
            if witness.schema != "courtlistener.source-census-witness.v1" || witness.source_ordinal_zero_based != entry.source_ordinal_zero_based
                || witness.columns.len() != 22 || witness.values.len() != witness.columns.len()
                || witness.columns.iter().collect::<BTreeSet<_>>().len() != witness.columns.len()
                || RENDITIONS.iter().any(|field| !witness.columns.iter().any(|column| column.as_str() == *field))
                || ["id", "cluster_id"].iter().any(|field| !witness.columns.iter().any(|column| column.as_str() == *field))
                || witness.source_string_hashes.len() != witness.values.iter().filter(|value| !value.is_null()).count()
                || columns.as_ref().is_some_and(|columns| columns != &witness.columns) {
                return Err("original witness schema/columns/ordinal differs".into());
            }
            if columns.is_none() { columns = Some(witness.columns.clone()); }
            for (column, value) in witness.columns.iter().zip(&witness.values) {
                if value.is_null() { continue; }
                let value = value.as_str().ok_or("original witness scalar must be string or NULL")?;
                let expected = witness.source_string_hashes.get(column).ok_or("original string proof missing")?;
                if expected.utf8_bytes != value.len() || expected.sha256 != hash(value.as_bytes()) {
                    return Err("original source string hash differs".into());
                }
            }
            let row = Value::Object(witness.columns.iter().cloned().zip(witness.values).collect());
            let source = json!({"witness_ordinal":entry.witness_ordinal,"source_ordinal_zero_based":entry.source_ordinal_zero_based,
                "compressed_offset":entry.compressed_offset,"compressed_bytes":entry.compressed_bytes,"compressed_sha256":compressed_sha,
                "raw_bytes":entry.raw_bytes,"raw_sha256":entry.raw_sha256,"original_csv_row_bytes":witness.original_csv_row_bytes,
                "original_csv_row_sha256":witness.original_csv_row_sha256,"opinion_id":row["id"],"cluster_id":row["cluster_id"]});
            rows += 1;
            for field in RENDITIONS {
                current_witness["stage"] = json!("field_inventory");
                current_witness["field"] = json!(field);
                let total = totals.entry(field.into()).or_default();
                add(&mut total.fields, "input_fields", 1);
                let value = row[field].as_str();
                let presence = match value { None => "null", Some("") => "empty", Some(_) => "nonempty" };
                add(&mut total.fields, presence, 1);
                add(&mut total.fields, "source_utf8_bytes", value.map_or(0, |value|value.len() as u64));
                let mut observation = json!({"schema":"courtlistener.native-witness-field.v1","source":source,
                    "field":field,"presence":presence,"field_source":witness.source_string_hashes.get(field)});
                if presence != "nonempty" {
                    observation["status"] = json!(format!("skipped_{presence}"));
                } else if cap_stop {
                    add(&mut total.fields, "skipped_output_cap", 1);
                    observation["status"] = json!("skipped_output_cap");
                } else {
                    add(&mut total.fields, "attempted", 1);
                    let then = Instant::now();
                    let attempted = std::panic::catch_unwind(||compile(&row, Some(field), None, None).map(|doc|facts(&doc, semantic_digests)));
                    let seconds = then.elapsed().as_secs_f64();
                    total.analysis_seconds += seconds;
                    observation["analysis_seconds"] = json!(seconds);
                    match attempted {
                        Ok(Ok(facts)) => {
                            add(&mut total.fields, "parsed", 1);
                            for (source, counts) in &facts.nodes_by_derivation { merge(total.nodes_by_derivation.entry(source.clone()).or_default(), counts); }
                            for (tag, counts) in &facts.native_roles_by_source_tag { merge(total.native_roles_by_source_tag.entry(tag.clone()).or_default(), counts); }
                            merge(&mut total.source_tags, &facts.source_tags);
                            merge(&mut total.source_roles, &facts.source_roles);
                            merge(&mut total.relationships, &facts.relationships);
                            for (name, amount) in &facts.passage_range_coverage {
                                if name == "maximum_block_utf16" {
                                    let maximum = total.passage_range_coverage.entry(name.clone()).or_default();
                                    *maximum = (*maximum).max(*amount);
                                } else { add(&mut total.passage_range_coverage, name, *amount); }
                            }
                            merge(&mut total.diagnostic_codes, &facts.diagnostic_codes);
                            add(&mut total.fields, "discrepancies", facts.discrepancies.len() as u64);
                            add(&mut total.fields, "diagnostics", facts.diagnostics.len() as u64);
                            add(&mut total.fields, "local_fragment_observations", facts.local_fragment_observations.len() as u64);
                            add(&mut total.fields, "source_paragraph_marker_observations", facts.source_paragraph_marker_observations.len() as u64);
                            observation["status"] = json!("parsed");
                            observation["parsed"] = json!(facts);
                        }
                        Ok(Err(error)) => {
                            add(&mut total.fields, "errors", 1);
                            observation["status"] = json!("error");
                            observation["error"] = json!(error.to_string());
                        }
                        Err(panic) => {
                            add(&mut total.fields, "errors", 1);
                            add(&mut total.fields, "panics", 1);
                            observation["status"] = json!("panic");
                            observation["error"] = json!(panic.downcast_ref::<String>().map(String::as_str)
                                .or_else(||panic.downcast_ref::<&str>().copied()).unwrap_or("non-string parser/query panic"));
                        }
                    }
                }
                if !cap_stop && persist(&mut output, &observation, &mut bytes, maximum)? {
                    add(&mut total.fields, "observations_written", 1);
                } else {
                    cap_stop = true;
                    add(&mut total.fields, "observations_omitted_output_cap", 1);
                    add(&mut total.fields, "discrepancies_omitted_output_cap", observation["parsed"]["discrepancies"].as_array().map_or(0, |values|values.len() as u64));
                    add(&mut total.fields, "diagnostics_omitted_output_cap", observation["parsed"]["diagnostics"].as_array().map_or(0, |values|values.len() as u64));
                    add(&mut total.fields, "local_fragment_observations_omitted_output_cap", observation["parsed"]["local_fragment_observations"].as_array().map_or(0, |values|values.len() as u64));
                    add(&mut total.fields, "source_paragraph_marker_observations_omitted_output_cap", observation["parsed"]["source_paragraph_marker_observations"].as_array().map_or(0, |values|values.len() as u64));
                    if observation["error"].is_string() { add(&mut total.fields, "errors_omitted_output_cap", 1); }
                    if first_omitted.is_null() { first_omitted = json!({"source":source,"field":field,"presence":presence,"status":observation["status"]}); }
                }
            }
            if rows % 1000 == 0 { output.flush()?; eprintln!("inventoried {rows} retained source witnesses"); }
        }
        current_witness = json!({"stage":"whole_input_identity_verification","index_bytes":index_bytes,"compressed_bytes":data_bytes});
        let mut trailing = [0];
        if data_file.read(&mut trailing)? != 0 || data_bytes != data_proof["bytes"].as_u64().unwrap()
            || format!("{:x}",data_digest.finalize()) != data_proof["sha256"].as_str().unwrap()
            || format!("{:x}",index_digest.finalize()) != index_proof["sha256"].as_str().unwrap() {
            return Err("whole original witness/index identity changed during inventory".into());
        }
        Ok(())
    })();
    output.flush()?;
    let mut complete = result.is_ok() && !cap_stop;
    let parser_errors: u64 = totals.values().map(|total|total.fields.get("errors").copied().unwrap_or(0)).sum();
    let mut summary = json!({"schema":"courtlistener.native-witness-inventory.v1","complete":complete,
        "status":if !complete {"incomplete"} else if parser_errors != 0 {"complete_with_parser_errors"} else {"complete"},
        "parser_errors":parser_errors,
        "source_witnesses_verified":rows,"original_columns":columns,"inputs":{"witness_index":index_proof,"witnesses":data_proof,"supplied_bindings":bindings},
        "source_verification_complete":result.is_ok(),"observation_order":{"witness":"witness_ordinal ascending","fields":RENDITIONS},
        "maximum_bytes":maximum,"observations_bytes":output.metadata()?.len(),"first_unpersisted_field":first_omitted,
        "failure":result.as_ref().err().map(ToString::to_string),"failed_witness":if result.is_err() {Some(current_witness)} else {None},
        "output_cap_reached":cap_stop,"renditions":totals,
        "engine_source_sha256":legal_structure::ENGINE_SOURCE_SHA256,"features_source_sha256":legal_structure_features::SOURCE_SHA256,
        "query_source_sha256":hash(include_bytes!("../../../query/src/document_query.rs")),
        "main_source_sha256":hash(include_bytes!("main.rs")),
        "harness_source_sha256":hash(include_bytes!("witness_run.rs")),"seconds":started.elapsed().as_secs_f64(),
        "scope":"Actual parser inventory of retained full-snapshot-v2 opinion witness renditions; other CourtListener collections are outside this runner. Parser output and its relationships are observations, not independent semantic gold or whole-source/browser correctness.",
        "limits":["NULL/empty fields remain distinct; each nonempty field uses compile with no other-field fallback, canonical metadata or CAP reporter context",
            "Source IDs have priority over legacy A names; href='#' is document-top scope and skips source-ID lookup",
            "Nonempty fragment matching is exact parsed-attribute equality without percent decoding; missing/ambiguous lookup counts do not prove broken navigation",
            "Local fragment details retain document-top scope, every percent-escaped fragment and every missing/ambiguous literal lookup; ordinary found lookups are aggregated",
            "Source paragraph markers/getter relations retain literal P openings, printed numbering and BRACKETNUM annotations; they do not assert canonical paragraph endpoints or active legal numbering",
            "Boundary conflicts require a heuristic Paragraph start uniquely matching a nonempty physical P; only represented source-opinion query ranges are observed, and embedded writings inside that P are identified separately",
            "Borrowed source authors remain preserved without an optional Native Prose projection; projection absence is counted without declaring the byline lost",
            "Passage range coverage/granularity uses query.blocks(Paragraph), including Prose but excluding Sections and viewer fallbacks; whole-text blocks are counted explicitly, and coverage is not legal paragraph completeness",
            "Original CSV row hashes are retained provenance; exact indexed JSON/frame and decoded strings are verified here, without reopening the original CSV archive",
            "Offsets address the retained frame and field's markup SHA; rendered ranges/role ownership are existing parser/query observations",
            "On output-cap stop remaining originals are verified/accounted without further parser attempts; omitted observations are explicit and completeness is false"]});
    if semantic_digests {
        summary["semantic_digests"] = json!({
            "schema":"courtlistener.native-witness-semantic-digests.v1",
            "document_fingerprint_schema":"legalpdf.document-fingerprint.v1",
            "rendered_text_present":"doc.rendered_text.is_some()",
            "document_outline":"document_outline(doc,false,None)",
            "source_opinion_components":"source_opinion_components(doc)",
            "scope":"Existing model semantic fingerprint components, rendered-text presence, complete outline and source-opinion projections; no full document copies persisted",
            "limits":["Unexecuted query APIs and publisher browser landing are outside these observations"]
        });
    }
    let mut encoded = serde_json::to_vec(&summary)?;
    encoded.push(b'\n');
    if output.metadata()?.len().saturating_add(encoded.len() as u64) > maximum {
        complete = false;
        summary["complete"] = json!(false);
        summary["status"] = json!("incomplete");
        summary["output_cap_reached"] = json!(true);
        summary["aggregate_maps_omitted_output_cap"] = json!(["source_tags","native_roles_by_source_tag","diagnostic_codes"]);
        for total in summary["renditions"].as_object_mut().unwrap().values_mut() {
            total["source_tags"] = Value::Null;
            total["native_roles_by_source_tag"] = Value::Null;
            total["diagnostic_codes"] = Value::Null;
        }
        encoded = serde_json::to_vec(&summary)?;
        encoded.push(b'\n');
    }
    if output.metadata()?.len().saturating_add(encoded.len() as u64) > maximum {
        complete = false;
        summary = json!({"schema":"courtlistener.native-witness-inventory.v1","complete":false,
            "status":"aggregate_receipt_exceeded_output_cap","source_witnesses_verified":rows,"maximum_bytes":maximum,
            "parser_errors":parser_errors,"observations_bytes":bytes,"output_cap_reached":true,
            "first_unpersisted_field":if first_omitted.is_null() {Value::Null} else {json!({
                "witness_ordinal":first_omitted["source"]["witness_ordinal"],
                "source_ordinal_zero_based":first_omitted["source"]["source_ordinal_zero_based"],
                "compressed_offset":first_omitted["source"]["compressed_offset"],
                "raw_sha256":first_omitted["source"]["raw_sha256"],"field":first_omitted["field"],
                "presence":first_omitted["presence"],"status":first_omitted["status"]})},
            "source_verification_complete":summary["source_verification_complete"],"failure":summary["failure"],
            "failed_witness":summary["failed_witness"],"aggregate_details_omitted_output_cap":true,
            "fields_by_rendition":totals.iter().map(|(field,total)|(field,&total.fields)).collect::<BTreeMap<_,_>>()});
        encoded = serde_json::to_vec(&summary)?;
        encoded.push(b'\n');
        if bytes.saturating_add(encoded.len() as u64) > maximum {
            return Err("bounded terminal inventory receipt exceeds output cap".into());
        }
    }
    receipt.seek(SeekFrom::Start(0))?;
    receipt.write_all(&encoded)?;
    receipt.set_len(encoded.len() as u64)?;
    if !complete { return Err("retained witness inventory incomplete; see summary.json".into()); }
    Ok(())
}
