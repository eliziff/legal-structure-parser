use crate::{cap_citations, compile, each_record, hash, metadata, write_json, Result, RENDITIONS};
use legal_structure::{normalize_javascript_whitespace, Derivation, DocumentKind, DocumentQuery};
use scraper::{ElementRef, Html, Selector};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
    time::Instant,
};

fn pinpoint_queries(
    doc: &legal_structure::DocumentStructure,
    query: &DocumentQuery,
    derivation: Derivation,
) -> Vec<Value> {
    doc.nodes.iter().filter(|node| node.source == derivation).filter_map(|node| {
        let kind = match node.kind {
            legal_structure::NodeKind::Page => DocumentKind::Page,
            legal_structure::NodeKind::Footnote => DocumentKind::Footnote,
            _ => return None,
        };
        let label = node.label.as_deref()?;
        let lookup = query.read_range(doc, kind, label, label, 0);
        let range = doc.query_range(node);
        Some(json!({"node_id":node.id,"kind":node.kind,"label":label,
            "resolves":lookup.as_ref().is_some_and(|lookup| lookup.selected.iter().any(|selected|
                selected.block.label == label && selected.block.start == range.start && selected.block.end == range.end)),
            "selected":lookup.map(|lookup| lookup.selected.into_iter().map(|selected| {
                let mut observation = json!({"label":selected.block.label,"start":selected.block.start,"end":selected.block.end});
                if derivation == Derivation::Heuristic {
                    observation["text"] = json!(selected.text);
                }
                observation
            }).collect::<Vec<_>>())}))
    }).collect()
}

fn paragraph_queries(
    doc: &legal_structure::DocumentStructure,
    query: &DocumentQuery,
) -> Value {
    let blocks = query.blocks(doc, Some(DocumentKind::Paragraph)).collect::<Vec<_>>();
    let mut indices = vec![0, blocks.len() / 2, blocks.len().saturating_sub(1)];
    indices.sort_unstable();
    indices.dedup();
    let reads = indices.into_iter().filter_map(|index| {
        let block = blocks.get(index)?;
        let lookup = query.read_range(doc, DocumentKind::Paragraph, &block.label, &block.label, 0);
        Some(json!({"label":block.label,"expected_range":[block.start,block.end],
            "resolves":lookup.as_ref().is_some_and(|lookup| lookup.selected.iter().any(|selected|
                selected.block.start == block.start && selected.block.end == block.end)),
            "selected":lookup.map(|lookup| lookup.selected.into_iter().map(|selected|
                json!({"label":selected.block.label,"start":selected.block.start,
                    "end":selected.block.end,"text":selected.text})).collect::<Vec<_>>())}))
    }).collect::<Vec<_>>();
    json!({"blocks":blocks,"reads":reads})
}

fn page_alias_queries(
    doc: &legal_structure::DocumentStructure,
    query: &DocumentQuery,
) -> Vec<Value> {
    doc.nodes.iter()
        .filter(|node| node.kind == legal_structure::NodeKind::Page)
        .flat_map(|node| node.aliases.iter().flatten().map(move |alias| {
            let expected = doc.query_range(node);
            let request = format!("printed:{alias}");
            let result = query.resolve_address_spans(doc, &request, legal_structure::FollowDirection::None, 0);
            let (resolved, selected) = match &result {
                legal_structure::DocumentAddressResolution::Found { spans } => (
                    spans.iter().any(|span| *span == expected),
                    Some(spans.iter().map(|span| {
                        let owner = doc.nodes.iter().find(|candidate|
                            candidate.kind == legal_structure::NodeKind::Page && doc.query_range(candidate) == *span);
                        json!({"label":owner.and_then(|owner|owner.label.as_deref()),"start":span.start,"end":span.end})
                    }).collect::<Vec<_>>()),
                ),
                _ => (false, None),
            };
            json!({"alias":alias,"request":request,"query_api":"resolve_address_spans","address_resolution":result,
                "label":node.label,"node_id":node.id,
                "source_markup_start":node.markup_start,"start":expected.start,"end":expected.end,
                "resolves":resolved,"selected":selected})
        })).collect()
}

fn note_label_queries(
    doc: &legal_structure::DocumentStructure,
    query: &DocumentQuery,
) -> Vec<Value> {
    let anchors = doc.markup_elements.iter()
        .flat_map(|element| element.attributes.get("id").into_iter().chain(
            element.tag.eq_ignore_ascii_case("a")
                .then(|| element.attributes.get("name")).flatten()))
        .filter(|anchor| !anchor.is_empty())
        .collect::<BTreeSet<_>>();
    anchors.into_iter()
        .filter_map(|anchor| {
            let (note, marker) = query.source_note_label(doc, anchor)?;
            Some(
                json!({"anchor":anchor,"node_id":note.node_id,"markup_start":marker.markup_start,
            "label_range":note.label_range,"body_range":note.body_range}),
            )
        })
        .collect()
}

fn markup_name_lookups(
    doc: &legal_structure::DocumentStructure,
    query: &DocumentQuery,
) -> Vec<Value> {
    doc.markup_elements.iter()
        .filter_map(|element| element.attributes.get("name"))
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|name| json!({"anchor":name,
            "markup_start":query.source_element(doc,name).map(|element|element.markup_start)}))
        .collect()
}

fn source_list_item_queries(
    doc: &legal_structure::DocumentStructure,
    query: &DocumentQuery,
    requests: &Value,
) -> Result<Value> {
    let coordinates = legal_structure::ScalarText::new(doc.query_text());
    let requests = requests.as_array().ok_or("source list requests must be an array")?;
    let observations = requests.iter().map(|request| -> Result<Value> {
        let owner = request["list_node_id"].as_str().ok_or("source list owner must be a string")?;
        let ordinal = request["ordinal"].as_i64().ok_or("source list ordinal must be a signed integer")?;
        let selected = query.source_list_item(doc, owner, ordinal).map(|node| {
            let range = doc.query_range(node);
            json!({"node_id":node.id,"start":range.start,"end":range.end,
                "text":coordinates.slice_utf16(range.start..range.end)
                    .map(legal_structure::trim_javascript_whitespace)})
        });
        Ok(json!({"list_node_id":owner,"ordinal":ordinal,"selected":selected}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(json!(observations))
}

fn source_paragraph_marker_queries(
    doc: &legal_structure::DocumentStructure,
    query: &DocumentQuery,
    requests: &Value,
) -> Result<Value> {
    let requests = requests.as_array().ok_or("source paragraph marker requests must be an array")?;
    let observations = requests.iter().map(|request| -> Result<Value> {
        let start = request["markup_start"].as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or("source paragraph marker position must be an opening-byte integer")?;
        Ok(json!({"markup_start":start,"selected":query.source_paragraph_marker(doc, start)}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(json!(observations))
}

fn source_anchor_section_requests(requests: &Value) -> Result<Vec<&str>> {
    requests
        .as_array()
        .ok_or("source anchor section requests must be an array")?
        .iter()
        .map(|request| {
            request
                .as_object()
                .and_then(|request| request.get("anchor"))
                .and_then(Value::as_str)
                .filter(|anchor| !anchor.is_empty())
                .ok_or_else(|| "source anchor section anchor must be a nonempty string".into())
        })
        .collect()
}

fn source_anchor_section_queries(
    doc: &legal_structure::DocumentStructure,
    query: &DocumentQuery,
    anchors: &[&str],
) -> Value {
    json!(anchors.iter().map(|anchor| {
        let source_anchor = query.source_anchor(doc, anchor);
        let section_range = query.read_range(doc, DocumentKind::Section, anchor, anchor, 0);
        let mut observation = json!({"anchor":anchor,
            "source_anchor":source_anchor.map(|node| {
                let range = doc.query_range(node);
                json!({"node_id":node.id,"kind":node.kind,"markup_start":node.markup_start,
                    "start":range.start,"end":range.end})
            }),
            "section_range":section_range.as_ref().map(|lookup| json!({"selected":lookup.selected.iter()
                .map(|selected| json!({"label":selected.block.label,"start":selected.block.start,
                    "end":selected.block.end,"text":selected.text})).collect::<Vec<_>>()}))});
        if source_anchor.is_none() {
            observation["source_anchor_error"] = json!("unresolved-or-ambiguous");
        }
        if section_range.is_none() {
            observation["section_range_error"] = json!("unresolved-or-ambiguous");
        }
        observation
    }).collect::<Vec<_>>())
}

pub(super) fn features(markup: &str, elements: &Selector) -> Value {
    let dom = Html::parse_fragment(markup);
    let mut tags = BTreeMap::<String, usize>::new();
    let mut ids = BTreeSet::new();
    let mut paragraph_ids = BTreeSet::new();
    let mut links = BTreeSet::new();
    let mut empty_paragraph_ids = BTreeSet::new();
    let mut attributes = BTreeMap::<String, usize>::new();
    let mut classes = BTreeMap::<String, usize>::new();
    let mut witnesses = BTreeMap::<&str, BTreeSet<String>>::new();
    let mut observations = BTreeMap::<&str, usize>::new();
    let mut structural_categories = BTreeMap::<&str, usize>::new();
    for element in dom.select(elements) {
        let tag = element.value().name();
        let id = element.value().attr("id").unwrap_or_default();
        *tags.entry(tag.to_owned()).or_default() += 1;
        if !id.is_empty() {
            ids.insert(id.to_owned());
            if tag == "p" {
                paragraph_ids.insert(id.to_owned());
                if element
                    .text()
                    .all(|text| normalize_javascript_whitespace(text).is_empty())
                {
                    empty_paragraph_ids.insert(id.to_owned());
                }
            }
        }
        if let Some(href) = element.value().attr("href") {
            links.insert(href.to_owned());
        }
        let attrs = element.value().attrs().collect::<BTreeMap<_, _>>();
        for (name, value) in &attrs {
            *attributes.entry((*name).to_owned()).or_default() += 1;
            witnesses.entry("attributes").or_default().insert(hash(
                serde_json::to_string(&(tag, id, name, value))
                    .unwrap()
                    .as_bytes(),
            ));
            if name.starts_with("data-") || matches!(*name, "bbox" | "page" | "pg") {
                witnesses.entry("data_attributes").or_default().insert(hash(
                    serde_json::to_string(&(tag, id, name, value))
                        .unwrap()
                        .as_bytes(),
                ));
                *observations.entry("data_attribute_bytes").or_default() += value.len();
            }
            if *name == "data-blocks" {
                *observations.entry("layout_block_attributes").or_default() += 1;
                match serde_json::from_str::<Value>(value) {
                    Ok(Value::Array(blocks)) => {
                        *observations.entry("layout_blocks").or_default() += blocks.len();
                        for block in blocks {
                            if block.get("bbox").is_some()
                                || block.get(2).is_some_and(Value::is_array)
                            {
                                *observations.entry("layout_bounding_boxes").or_default() += 1;
                            }
                            if block.get("pg").is_some()
                                || block.get("page").is_some()
                                || block.get(1).is_some_and(Value::is_number)
                            {
                                *observations.entry("layout_page_references").or_default() += 1;
                            }
                        }
                    }
                    _ => *observations.entry("uninterpreted_data_blocks").or_default() += 1,
                }
            }
        }
        for class in element.value().classes() {
            *classes.entry(class.to_owned()).or_default() += 1;
            witnesses
                .entry("semantic_classes")
                .or_default()
                .insert(hash(
                    serde_json::to_string(&(tag, id, class)).unwrap().as_bytes(),
                ));
        }
        let class = |name| element.value().classes().any(|value| value == name);
        let media = matches!(tag, "img" | "svg" | "math" | "object" | "embed");
        let footnote_body = matches!(tag, "footnote" | "footnote_body")
            || class("footnote")
            || class("co_footnoteBody")
            || class("footnotes");
        let footnote_reference = tag == "footnotemark"
            || class("footnote-ref")
            || class("footnote-reference")
            || (tag == "a"
                && element.value().attr("href").is_some_and(|href| {
                    href.starts_with("#fn")
                        || href.starts_with("#footnote")
                        || href.starts_with("#ftn")
                }));
        let page_marker = matches!(tag, "page-number" | "page-label")
            || class("page-label")
            || class("star-pagination")
            || class("co_starPage");
        let opinion_boundary = tag == "opinion"
            || class("opinion")
            || class("co_opinionBlock")
            || element.value().attr("data-type").is_some();
        let heading = matches!(
            tag,
            "h" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "heading"
        ) || class("co_printHeading")
            || class("co_headtext");
        let section = matches!(tag, "opinion" | "casebody" | "section" | "article")
            || class("co_opinionBlock")
            || class("co_section")
            || class("opinion")
            || class("casebody");
        let headmatter = id == "headmatter" || class("head-matter");
        let list = matches!(tag, "ul" | "ol" | "li");
        let nested_list = matches!(tag, "ul" | "ol")
            && element
                .ancestors()
                .filter_map(ElementRef::wrap)
                .any(|ancestor| matches!(ancestor.value().name(), "ul" | "ol"));
        let list_numbering = (tag == "ol"
            && ["start", "type", "reversed"]
                .iter()
                .any(|name| element.value().attr(name).is_some()))
            || (tag == "li" && element.value().attr("value").is_some());
        let table = matches!(
            tag,
            "table"
                | "thead"
                | "tbody"
                | "tfoot"
                | "tr"
                | "td"
                | "th"
                | "caption"
                | "tgroup"
                | "colspec"
        );
        let nested_table = tag == "table"
            && element
                .ancestors()
                .filter_map(ElementRef::wrap)
                .any(|ancestor| ancestor.value().name() == "table");
        let table_span = matches!(tag, "td" | "th")
            && ["rowspan", "colspan"].iter().any(|name| {
                element
                    .value()
                    .attr(name)
                    .and_then(|value| value.parse::<u32>().ok())
                    .is_some_and(|value| value > 1)
            });
        let citation = matches!(tag, "ref" | "citation")
            || class("citation")
            || class("case_cite")
            || class("co_cites");
        let categories = [
            ("media", media),
            ("footnote_bodies", footnote_body),
            ("footnote_labels", class("co_footnoteNumber")),
            ("footnote_references", footnote_reference),
            ("page_markers", page_marker),
            ("opinion_boundaries", opinion_boundary),
            ("headings", heading),
            ("sections", section),
            ("headmatter", headmatter),
            ("lists", list),
            ("nested_lists", nested_list),
            ("list_numbering", list_numbering),
            ("tables", table),
            ("nested_tables", nested_table),
            ("table_spans", table_span),
            ("citations", citation),
        ];
        if categories.iter().any(|(_, selected)| *selected) {
            let text = normalize_javascript_whitespace(&element.text().collect::<String>());
            let fingerprint = hash(
                serde_json::to_string(&(tag, &attrs, hash(text.as_bytes())))
                    .unwrap()
                    .as_bytes(),
            );
            for (category, selected) in categories {
                if selected {
                    witnesses
                        .entry(category)
                        .or_default()
                        .insert(fingerprint.clone());
                    *observations.entry(category).or_default() += 1;
                    *structural_categories.entry(category).or_default() += 1;
                }
            }
            if media {
                if let Some(src) = element
                    .value()
                    .attr("src")
                    .filter(|src| src.starts_with("data:"))
                {
                    *observations.entry("embedded_media_url_bytes").or_default() += src.len();
                }
            }
        }
    }
    json!({"tags":tags,"ids":ids,"paragraph_ids":paragraph_ids,"empty_paragraph_ids":empty_paragraph_ids,
        "links":links,"attribute_names":attributes,"classes":classes,"structural_categories":structural_categories,
        "witnesses":witnesses,"observations":observations})
}

pub fn run(
    pack: &Path,
    out: &Path,
    limit: usize,
    metadata_path: &Path,
    citation_input: Option<(&Path, &Path)>,
    retain_rendered_text: bool,
) -> Result<()> {
    let started = Instant::now();
    fs::create_dir_all(out)?;
    let metadata = metadata::read(metadata_path)?;
    let citation_context = citation_input
        .map(|(path, home)| cap_citations::CitationContext::load(path, home))
        .transpose()?;
    let input_context_seconds = started.elapsed().as_secs_f64();
    let elements = Selector::parse("*").map_err(|_| "invalid fixed element selector")?;
    let mut receipts =
        zstd::stream::write::Encoder::new(File::create(out.join("records.jsonl.zst"))?, 3)?;
    let mut count = 0usize;
    let mut records_with_citation_context = 0usize;
    let mut totals = BTreeMap::<String, Value>::new();
    let mut metadata_totals = BTreeMap::<String, usize>::new();
    let mut unique_witness_totals = BTreeMap::<String, BTreeMap<String, usize>>::new();
    each_record(pack, |record| {
        if limit != 0 && count >= limit {
            return Ok(());
        }
        let row = &record["opinion"];
        let cluster = metadata::lookup(
            &metadata,
            "clusters",
            row["cluster_id"].as_str().unwrap_or_default(),
        )?;
        let cluster_id = row["cluster_id"].as_str().unwrap_or_default();
        let url = cluster["slug"]
            .as_str()
            .map(|slug| format!("https://www.courtlistener.com/opinion/{cluster_id}/{slug}/"));
        let page_citations = citation_context
            .as_ref()
            .map(|context| {
                context.cluster(
                    cluster_id,
                    url.as_deref().unwrap_or(""),
                    cluster["filepath_json_harvard"].as_str(),
                    None,
                )
            })
            .transpose()?
            .unwrap_or_default();
        records_with_citation_context += usize::from(!page_citations.is_empty());
        for field in [
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
        ] {
            if cluster[field]
                .as_str()
                .is_some_and(|value| !value.is_empty())
            {
                *metadata_totals.entry(field.into()).or_default() += 1;
            }
        }
        let mut renditions = BTreeMap::new();
        let mut equal = Vec::new();
        for (index, field) in RENDITIONS.iter().enumerate() {
            let original = &row[*field];
            let presence = if original.is_null() {
                "null"
            } else if original == "" {
                "empty"
            } else {
                "nonempty"
            };
            if !original.is_string() || original == "" {
                renditions.insert(*field, json!({"presence":presence}));
                continue;
            }
            let source = original.as_str().unwrap();
            for other in &RENDITIONS[..index] {
                if row[*other] == *original {
                    equal.push(json!([other, field]));
                }
            }
            let source_features = if *field == "plain_text" {
                features("", &elements)
            } else {
                features(source, &elements)
            };
            let then = Instant::now();
            let attempted = std::panic::catch_unwind(|| {
                compile(row, Some(field), Some(&metadata), citation_context.as_ref())
            });
            let panicked = attempted.is_err();
            let result = attempted.unwrap_or_else(|_| {
                eprintln!("engine panic: opinion {} rendition {field}", row["id"]);
                Err("engine panicked on retained source".into())
            });
            let seconds = then.elapsed().as_secs_f64();
            let mut facts = json!({"presence":presence,"source_utf8_bytes":source.len(),"source_sha256":hash(source.as_bytes()),"source":source_features,"parse_seconds":seconds,"engine_panicked":panicked});
            let entry=totals.entry(field.to_string()).or_insert(json!({"records":0,"source_bytes":0,"parse_seconds":0.0,"errors":0,"source_ids":0,"recovered_source_ids":0,"source_paragraph_ids":0,"recovered_paragraph_ids":0,"source_empty_paragraph_ids":0,"recovered_empty_paragraph_ids":0,"empty_anchor_lookups":0,"nodes":{},"observations":{},"attribute_names":{},"classes":{}}));
            entry["records"] = json!(entry["records"].as_u64().unwrap() + 1);
            entry["source_bytes"] =
                json!(entry["source_bytes"].as_u64().unwrap() + source.len() as u64);
            entry["parse_seconds"] = json!(entry["parse_seconds"].as_f64().unwrap() + seconds);
            for name in ["observations", "attribute_names", "classes"] {
                for (key, amount) in source_features[name].as_object().unwrap() {
                    entry[name][key] =
                        json!(entry[name][key].as_u64().unwrap_or(0) + amount.as_u64().unwrap());
                }
            }
            match result {
                Ok(document) => {
                    let mut kinds = BTreeMap::<String, usize>::new();
                    let mut derivations = BTreeMap::<&str, BTreeMap<&str, usize>>::new();
                    let mut native_tags = BTreeMap::<&str, usize>::new();
                    let anchors = document
                        .nodes
                        .iter()
                        .filter_map(|n| n.anchor.clone())
                        .collect::<BTreeSet<_>>();
                    for node in &document.nodes {
                        *kinds.entry(node.kind.name().into()).or_default() += 1;
                        let derivation = match node.source {
                            Derivation::Native => "native",
                            Derivation::Heuristic => "heuristic",
                            Derivation::Model => "model",
                        };
                        *derivations
                            .entry(derivation)
                            .or_default()
                            .entry(node.kind.name())
                            .or_default() += 1;
                        if let Some(tag) = node.markup_tag.as_deref() {
                            *native_tags.entry(tag).or_default() += 1;
                        }
                    }
                    let observed = source_features["ids"].as_array().unwrap();
                    let paragraphs = source_features["paragraph_ids"].as_array().unwrap();
                    let recovered = observed
                        .iter()
                        .filter(|id| anchors.contains(id.as_str().unwrap()))
                        .count();
                    let recovered_paragraphs = paragraphs
                        .iter()
                        .filter(|id| anchors.contains(id.as_str().unwrap()))
                        .count();
                    let empty_paragraphs =
                        source_features["empty_paragraph_ids"].as_array().unwrap();
                    let recovered_empty = empty_paragraphs
                        .iter()
                        .filter(|id| anchors.contains(id.as_str().unwrap()))
                        .count();
                    let query = DocumentQuery::new();
                    let markup_ids = document
                        .markup_elements
                        .iter()
                        .filter_map(|element| element.attributes.get("id").map(String::as_str))
                        .collect::<BTreeSet<_>>();
                    let missing_markup_ids = observed
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|id| !markup_ids.contains(id))
                        .collect::<Vec<_>>();
                    let ambiguous_markup_ids = observed
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|id| {
                            markup_ids.contains(id) && query.source_element(&document, id).is_none()
                        })
                        .collect::<Vec<_>>();
                    let supplemental_nodes = document
                        .nodes
                        .iter()
                        .filter(|node| node.source != Derivation::Native)
                        .collect::<Vec<_>>();
                    let empty_lookup = empty_paragraphs
                        .iter()
                        .filter(|id| {
                            let id = id.as_str().unwrap();
                            query
                                .read_range(&document, DocumentKind::Paragraph, id, id, 0)
                                .is_some_and(|range| {
                                    range.selected.iter().any(|block| {
                                        block.block.label == id && block.text.is_empty()
                                    })
                                })
                        })
                        .count();
                    for (name, amount) in [
                        ("source_ids", observed.len()),
                        ("recovered_source_ids", recovered),
                        ("source_paragraph_ids", paragraphs.len()),
                        ("recovered_paragraph_ids", recovered_paragraphs),
                        ("source_empty_paragraph_ids", empty_paragraphs.len()),
                        ("recovered_empty_paragraph_ids", recovered_empty),
                        ("empty_anchor_lookups", empty_lookup),
                        (
                            "retained_source_element_ids",
                            observed.len() - missing_markup_ids.len(),
                        ),
                        ("missing_source_element_ids", missing_markup_ids.len()),
                        ("ambiguous_source_element_ids", ambiguous_markup_ids.len()),
                        (
                            "supplemental_nodes_with_anchor",
                            supplemental_nodes
                                .iter()
                                .filter(|node| node.anchor.is_some())
                                .count(),
                        ),
                    ] {
                        entry[name] = json!(entry[name].as_u64().unwrap_or(0) + amount as u64);
                    }
                    for (kind, amount) in &kinds {
                        entry["nodes"][kind] =
                            json!(entry["nodes"][kind].as_u64().unwrap_or(0) + *amount as u64);
                    }
                    for (derivation, counts) in &derivations {
                        for (kind, amount) in counts {
                            entry["nodes_by_derivation"][derivation][kind] = json!(
                                entry["nodes_by_derivation"][derivation][kind]
                                    .as_u64()
                                    .unwrap_or(0)
                                    + *amount as u64
                            );
                        }
                    }
                    for (tag, amount) in &native_tags {
                        entry["native_nodes_by_tag"][tag] = json!(
                            entry["native_nodes_by_tag"][tag].as_u64().unwrap_or(0)
                                + *amount as u64
                        );
                    }
                    facts["parsed"] = json!({"nodes":kinds,"anchors":anchors,"text_sha256":document.text_sha256,
                        "text_utf16":document.query_text().encode_utf16().count(),"recovered_source_ids":recovered,
                        "recovered_paragraph_ids":recovered_paragraphs,"recovered_empty_paragraph_ids":recovered_empty,
                        "empty_anchor_lookups":empty_lookup,"url":document.url,
                        "nodes_by_derivation":derivations,"native_nodes_by_tag":native_tags,
                        "missing_source_element_ids":missing_markup_ids,"ambiguous_source_element_ids":ambiguous_markup_ids,
                        "markup_elements":document.markup_elements.len(),"supplemental_nodes":supplemental_nodes,
                        "page_nodes":document.nodes.iter().filter(|node|node.kind == legal_structure::NodeKind::Page).collect::<Vec<_>>(),
                        "notes":document.notes,"diagnostics":document.diagnostics,
                        "page_alias_queries":page_alias_queries(&document, &query)});
                    if retain_rendered_text {
                        facts["parsed"]["rendered_text"] = json!(document.query_text());
                    }
                }
                Err(error) => {
                    entry["errors"] = json!(entry["errors"].as_u64().unwrap() + 1);
                    facts["error"] = json!(error.to_string());
                }
            }
            renditions.insert(*field, facts);
        }
        let mut unique_witnesses = BTreeMap::new();
        let mut text_equal = Vec::new();
        for (index, field) in RENDITIONS.iter().enumerate() {
            let Some(witnesses) = renditions[field]["source"]["witnesses"].as_object() else {
                continue;
            };
            let mut unique = BTreeMap::new();
            for (category, values) in witnesses {
                let other = renditions
                    .iter()
                    .filter(|(name, _)| *name != field)
                    .flat_map(|(_, facts)| {
                        facts["source"]["witnesses"][category]
                            .as_array()
                            .into_iter()
                            .flatten()
                    })
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>();
                let count = values
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|value| !other.contains(value.as_str().unwrap()))
                    .count();
                unique.insert(category, count);
                *unique_witness_totals
                    .entry((*field).into())
                    .or_default()
                    .entry(category.clone())
                    .or_default() += count;
            }
            unique_witnesses.insert(*field, unique);
            if let Some(text_hash) = renditions[field]["parsed"]["text_sha256"].as_str() {
                for other in &RENDITIONS[..index] {
                    if renditions[other]["parsed"]["text_sha256"] == text_hash {
                        text_equal.push(json!([other, field]));
                    }
                }
            }
        }
        serde_json::to_writer(
            &mut receipts,
            &json!({"opinion_id":row["id"],"cluster_id":row["cluster_id"],"opinion_type":row["type"],
                "author_id":row["author_id"],"per_curiam":row["per_curiam"],"renditions":renditions,
                "page_citations":page_citations,
                "citation_context":citation_context.as_ref().map(|context|context.cluster_provenance(cluster_id)).unwrap_or(json!({"status":"not_supplied"})),
                "exact_equal_fields":equal,"exact_rendered_text_equal_fields":text_equal,"witnesses_unique_to_rendition":unique_witnesses}),
        )?;
        receipts.write_all(b"\n")?;
        count += 1;
        if count % 100 == 0 {
            eprintln!("compared all populated renditions of {count} source records");
            receipts.flush()?;
        }
        Ok(())
    })?;
    receipts.finish()?;
    write_json(
        out.join("summary.json"),
        &json!({"schema":"courtlistener.rendition-richness.v3","records":count,
        "engine_source_sha256":legal_structure::ENGINE_SOURCE_SHA256,"features_source_sha256":legal_structure_features::SOURCE_SHA256,"renditions":totals,
        "citation_context":citation_context.as_ref().map(|context|context.provenance()),
        "records_with_citation_context":records_with_citation_context,
        "retained_rendered_text":retain_rendered_text,
        "input_context_seconds":input_context_seconds,
        "query_source_sha256":hash(include_bytes!("../../../query/src/document_query.rs")),
        "witnesses_unique_to_rendition":unique_witness_totals,"cluster_metadata_presence_by_opinion":metadata_totals,
        "witness_comparison":"Exact DOM attribute/class/element fingerprints; original field bytes remain independently retained. Uniqueness is relative to this record's populated renditions, not proof of irreconstructibility.",
        "seconds":started.elapsed().as_secs_f64(),"any_rendition_removed":false,
        "supplementation_scope":"Native and heuristic/model nodes counted separately by kind and source rendition; additions are retained with original provenance and ranges. Counts alone do not establish independent legal correctness.",
        "limits":["DOM source features are observations, not independent legal interpretation","Native anchors and browser fragment painting require browser confirmation","No parser failure or lower node count establishes that a rendition is needless"]}),
    )
}

fn structural_element(element: ElementRef<'_>) -> bool {
    matches!(
        element.value().name(),
        "p" | "blockquote"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "heading"
            | "footnote"
            | "footnote_body"
    ) || (matches!(element.value().name(), "aside" | "div" | "li" | "section")
        && element.value().classes().any(|class| class == "footnote"))
}

fn opinion_heading(body: ElementRef<'_>) -> Option<ElementRef<'_>> {
    for ancestor in std::iter::once(body).chain(body.ancestors().filter_map(ElementRef::wrap)) {
        for sibling in ancestor.prev_siblings().filter_map(ElementRef::wrap) {
            if sibling.value().name() == "h3"
                && sibling
                    .value()
                    .classes()
                    .any(|class| class == "opinion-section-title")
                && sibling.value().attr("id").is_some()
            {
                return Some(sibling);
            }
        }
    }
    None
}

/// Compare retained CAP XML/HTML directly, without treating its IDs as deployed
/// CourtListener anchors. The original complete JSON/HTML remains the source.
pub fn source_run(manifest: &Path, corpus_home: &Path, out: &Path) -> Result<()> {
    fs::create_dir_all(out)?;
    let mut receipts =
        zstd::stream::write::Encoder::new(File::create(out.join("records.jsonl.zst"))?, 3)?;
    let mut anchor_targets = BufWriter::new(File::create(out.join("anchor-targets.jsonl"))?);
    let mut page_manifest = BufWriter::new(File::create(out.join("page-html-manifest.jsonl"))?);
    let elements = Selector::parse("*").map_err(|_| "invalid fixed selector")?;
    let id_selector = Selector::parse("[id]").map_err(|_| "invalid fixed selector")?;
    let started = Instant::now();
    let (mut records, mut errors, mut bytes, mut text_bytes, mut missing_regions) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut observations = BTreeMap::<String, usize>::new();
    for line in BufReader::new(File::open(manifest)?).lines() {
        let capture: Value = serde_json::from_str(&line?)?;
        let requested_sections = capture
            .get("source_anchor_section_requests")
            .map(source_anchor_section_requests)
            .transpose()?;
        let plain_text = capture["format"] == "plain_text";
        if plain_text && ["source_region", "markup_serialization"].iter().any(|name|
            capture.get(name).is_some_and(|value| !value.is_null()))
        {
            return Err("plain text has no markup region or serialization".into());
        }
        let relative = Path::new(
            capture["corpus_path"]
                .as_str()
                .ok_or("source path missing")?,
        );
        if relative.is_absolute()
            || relative
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir))
        {
            return Err("source path outside corpus".into());
        }
        let raw = fs::read(corpus_home.join(relative))?;
        if capture["sha256"] != hash(&raw) {
            return Err("captured source digest mismatch".into());
        }
        let cap;
        let mut page_citations = Vec::new();
        let markup = match capture["format"].as_str() {
            Some("cap-json") => {
                cap = serde_json::from_slice::<Value>(&raw)?;
                page_citations = cap_citations::ordered_citations(&cap);
                cap["casebody"]["data"]
                    .as_str()
                    .ok_or("CAP casebody is not XML text")?
            }
            Some("html") | Some("plain_text") => std::str::from_utf8(&raw)?,
            _ => return Err("source format must be cap-json, html or plain_text".into()),
        };
        let regions = match capture["source_region"].as_str() {
            Some("headmatter") => Some(legal_structure::courtlistener_source_markups(markup)?),
            None => None,
            Some(_) => return Err("unknown requested source region".into()),
        };
        let (markup_byte_start, markup) = if let Some(regions) = &regions {
            let Some(region) = regions.iter().find(|region| region.kind == "headmatter") else {
                missing_regions += 1;
                continue;
            };
            (region.markup_start, region.markup)
        } else {
            (0, markup)
        };
        let markup_serialization = if plain_text {
            Value::Null
        } else {
            capture
                .get("markup_serialization")
                .cloned()
                .unwrap_or_else(|| {
                    json!(if capture["format"] == "cap-json" {
                        "xml_source"
                    } else {
                        "html_source"
                    })
                })
        };
        if plain_text { text_bytes += markup.len(); } else { bytes += markup.len(); }
        let source = features(if plain_text { "" } else { markup }, &elements);
        for (name, count) in source["observations"].as_object().unwrap() {
            *observations.entry(name.clone()).or_default() += count.as_u64().unwrap() as usize;
        }
        let input = serde_json::from_value(json!({"provider":"courtlistener",
        "id":capture["sha256"],"url":capture["url"],
        "markup":if plain_text { Value::Null } else { json!(markup) },
        "text":if plain_text { markup } else { "" },
        "pageCitations":page_citations,
        "scope":if plain_text {
            json!({"kind":"complete"})
        } else if regions.is_some() {
            json!({"kind":"excerpt","excerptOf":capture["url"],"markupSerialization":markup_serialization})
        } else {
            json!({"kind":"complete","markupSerialization":markup_serialization})
        }}))?;
        let mut receipt = json!({"source_capture":capture,"source":source,
            "html_sha256":capture["sha256"],"body_index":0,
            "markup_serialization":markup_serialization,
            "page_citations":page_citations,
            "source_markup_byte_start":markup_byte_start,
            "source_markup_byte_end":markup_byte_start+markup.len()});
        match legal_structure::analyze_native_markup(input) {
            Ok(doc) => {
                let query = DocumentQuery::new();
                let ids = doc
                    .markup_elements
                    .iter()
                    .filter_map(|element| element.attributes.get("id"))
                    .collect::<BTreeSet<_>>();
                let lookups = ids.into_iter().map(|anchor|json!({"anchor":anchor,"markup_start":query.source_element(&doc,anchor).map(|element|element.markup_start)})).collect::<Vec<_>>();
                if let Some(url) = capture["url"].as_str().filter(|url|
                    !url.is_empty() && capture["source_region"] == "headmatter" && capture["format"] == "html")
                {
                    let page = Html::parse_document(std::str::from_utf8(&raw)?);
                    let mut page_ids = BTreeMap::<&str, usize>::new();
                    for element in page.select(&id_selector) {
                        *page_ids.entry(element.value().attr("id").unwrap()).or_default() += 1;
                    }
                    let targets = lookups.iter().filter_map(|lookup| {
                        let anchor = lookup["anchor"].as_str()?;
                        if page_ids.get(anchor) != Some(&1) { return None; }
                        let element = query.source_element(&doc, anchor)?;
                        Some(json!({"anchor":anchor,"source_tag":element.tag,
                            "kind":"source_element","target":format!("{url}#{anchor}")}))
                    }).collect::<Vec<_>>();
                    // Source-backed candidates include IDs whose visibility is still unmeasured.
                    serde_json::to_writer(&mut anchor_targets, &json!({"url":url,
                        "html_sha256":capture["sha256"],"file":relative.file_name().ok_or("source filename missing")?.to_string_lossy(),
                        "body_index":null,"source_region":"headmatter","source_region_index":0,
                        "source_candidate":true,"browser_navigation_verified":false,"targets":targets}))?;
                    anchor_targets.write_all(b"\n")?;
                    serde_json::to_writer(&mut page_manifest, &json!({"url":url,
                        "file":relative.file_name().unwrap().to_string_lossy(),
                        "path":corpus_home.join(relative).canonicalize()?,"sha256":capture["sha256"],
                        "contentType":"text/html"}))?;
                    page_manifest.write_all(b"\n")?;
                }
                receipt["text"] = json!(doc.query_text());
                receipt["table_cells"] = json!(query.table_cells(&doc));
                receipt["markup_anchor_lookups"] = json!(lookups);
                receipt["markup_name_lookups"] = json!(markup_name_lookups(&doc, &query));
                receipt["markup_byte_ranges_valid"] = json!(
                    doc.markup_sha256.as_deref() == Some(hash(markup.as_bytes()).as_str())
                        && doc.markup_elements.iter().all(|element| markup
                            .get(element.markup_start..element.markup_end)
                            .is_some_and(|source| source
                                .strip_prefix('<')
                                .is_some_and(|source| source.starts_with(&element.tag))))
                );
                receipt["native_structures"] = json!(doc.nodes);
                receipt["source_markup"] = json!(query.source_markup(&doc));
                receipt["source_opinion_components"] =
                    json!(query.source_opinion_components(&doc));
                receipt["document_outline"] =
                    json!(legal_structure_features::document_outline(&doc, false, None));
                receipt["native_pinpoint_queries"] = json!(pinpoint_queries(&doc, &query, Derivation::Native));
                receipt["heuristic_pinpoint_queries"] = json!(pinpoint_queries(&doc, &query, Derivation::Heuristic));
                if capture["paragraph_queries"] == true {
                    receipt["paragraph_queries"] = paragraph_queries(&doc, &query);
                }
                receipt["note_label_queries"] = json!(note_label_queries(&doc, &query));
                receipt["markup_elements"] = json!(doc.markup_elements);
                receipt["markup_ancestors"] = json!(doc.markup_ancestors);
                receipt["markup_sha256"] = json!(doc.markup_sha256);
                receipt["notes"] = json!(doc.notes);
                receipt["page_alias_queries"] = json!(page_alias_queries(&doc, &query));
                if let Some(requests) = capture.get("source_list_item_requests") {
                    receipt["source_list_item_queries"] = source_list_item_queries(&doc, &query, requests)?;
                }
                if let Some(requests) = capture.get("source_paragraph_marker_requests") {
                    receipt["source_paragraph_markers"] = json!(query.source_paragraph_markers(&doc));
                    receipt["source_paragraph_marker_queries"] = source_paragraph_marker_queries(&doc, &query, requests)?;
                }
                if let Some(anchors) = &requested_sections {
                    receipt["source_anchor_section_queries"] = source_anchor_section_queries(&doc, &query, anchors);
                }
            }
            Err(error) => {
                errors += 1;
                receipt["error"] = json!(error.to_string());
            }
        }
        serde_json::to_writer(&mut receipts, &receipt)?;
        receipts.write_all(b"\n")?;
        records += 1;
    }
    receipts.finish()?;
    anchor_targets.flush()?;
    page_manifest.flush()?;
    write_json(
        out.join("summary.json"),
        &json!({"schema":"courtlistener.retained-source.v1",
        "records":records,"errors":errors,"markup_bytes":bytes,"plain_text_bytes":text_bytes,"observations":observations,
        "missing_requested_source_regions":missing_regions,
        "engine_source_sha256":legal_structure::ENGINE_SOURCE_SHA256,"features_source_sha256":legal_structure_features::SOURCE_SHA256,
        "query_source_sha256":hash(include_bytes!("../../../query/src/document_query.rs")),
        "seconds":started.elapsed().as_secs_f64(),
        "scope":"Exact retained CAP casebody XML, HTML, plain text, or explicitly selected served-page headmatter; plain text uses the existing text-only engine input and has no source markup elements. DOM observations are independent of native output. Source IDs alone do not prove deployed link reliability."}),
    )
}

pub fn web_run(
    manifest: &Path,
    corpus_home: &Path,
    out: &Path,
    plan_fragments: bool,
    citation_path: Option<&Path>,
) -> Result<()> {
    let started = Instant::now();
    fs::create_dir_all(out)?;
    let citation_context = citation_path
        .map(|path| cap_citations::CitationContext::load(path, corpus_home))
        .transpose()?;
    let captures: Vec<Value> = serde_json::from_reader(File::open(manifest)?)?;
    let input_context_seconds = started.elapsed().as_secs_f64();
    let selector = Selector::parse(".subopinion-content").map_err(|_| "invalid fixed selector")?;
    let id_selector = Selector::parse("[id]").map_err(|_| "invalid fixed ID selector")?;
    let link_selector = Selector::parse("a[href]").map_err(|_| "invalid fixed link selector")?;
    let mut native_links = BTreeMap::<String, Value>::new();
    let mut unrepresented_ids = BTreeMap::<String, usize>::new();
    let mut structural_totals = BTreeMap::<String, Value>::new();
    let (mut footnote_links, mut linked_references) = (0usize, 0usize);
    let mut receipts =
        zstd::stream::write::Encoder::new(File::create(out.join("records.jsonl.zst"))?, 3)?;
    let mut targets = BufWriter::new(File::create(out.join("targets.jsonl"))?);
    let mut source_texts = BufWriter::new(File::create(out.join("doctext.jsonl"))?);
    let mut page_manifest = BufWriter::new(File::create(out.join("page-html-manifest.jsonl"))?);
    let mut anchor_targets = BufWriter::new(File::create(out.join("anchor-targets.jsonl"))?);
    let (mut planned, mut complete_plans) = (0usize, 0usize);
    let mut source_ids = 0usize;
    let mut recovered_ids = 0usize;
    let mut bodies = 0usize;
    let mut pages_with_citation_context = 0usize;
    let (mut lookup_found, mut exact_text, mut text_mismatches) = (0usize, 0usize, 0usize);
    for capture in &captures {
        let relative = Path::new(
            capture["corpus_path"]
                .as_str()
                .ok_or("source HTML path missing")?,
        );
        if relative.is_absolute()
            || relative
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir))
        {
            return Err("source path outside corpus".into());
        }
        let raw = fs::read(corpus_home.join(relative))?;
        if hash(&raw) != capture["sha256"].as_str().ok_or("source digest missing")? {
            return Err("captured source digest mismatch".into());
        }
        let cluster_id = capture["cluster_id"].as_str().ok_or("cluster ID missing")?;
        let page_citations = citation_context
            .as_ref()
            .map(|context| {
                context.cluster(
                    cluster_id,
                    capture["final_url"].as_str().ok_or("source URL missing")?,
                    capture["sampling"]["cluster_metadata"]["filepath_json_harvard"].as_str(),
                    capture["sha256"].as_str(),
                )
            })
            .transpose()?
            .unwrap_or_default();
        pages_with_citation_context += usize::from(!page_citations.is_empty());
        let source_path = corpus_home.join(relative).canonicalize()?;
        serde_json::to_writer(
            &mut page_manifest,
            &json!({"url":capture["final_url"],
            "file":relative.file_name().ok_or("source filename missing")?.to_string_lossy(),
            "path":source_path,"sha256":capture["sha256"],"contentType":"text/html"}),
        )?;
        page_manifest.write_all(b"\n")?;
        let source_html = std::str::from_utf8(&raw)?;
        let html = Html::parse_document(source_html);
        let source_regions = legal_structure::courtlistener_source_markups(source_html)?;
        let opinion_markups = source_regions
            .iter()
            .filter(|region| region.kind == "opinion")
            .map(|region| (region.markup_start, region.markup))
            .collect::<Vec<_>>();
        if opinion_markups.len() != html.select(&selector).count() {
            return Err("exact source opinion boundaries disagree with independent DOM".into());
        }
        let evidence_path = capture["browser_evidence_path"]
            .as_str()
            .ok_or("independent browser evidence missing")?;
        let evidence_bytes = fs::read(manifest.parent().unwrap().join(evidence_path))?;
        if hash(&evidence_bytes)
            != capture["browser_evidence_sha256"]
                .as_str()
                .ok_or("browser evidence digest missing")?
        {
            return Err("browser evidence changed".into());
        }
        let evidence: Value = serde_json::from_slice(&evidence_bytes)?;
        let mut page_ids = BTreeMap::<&str, usize>::new();
        for element in html.select(&id_selector) {
            *page_ids
                .entry(element.value().attr("id").unwrap())
                .or_default() += 1;
        }
        for (index, body) in html.select(&selector).enumerate() {
            // Publisher container attributes affect visible opinion text. Keep
            // that actual source context rather than stripping it at the seam.
            let (markup_byte_start, markup) = opinion_markups[index];
            let markup_ancestors = body
                .ancestors()
                .filter_map(ElementRef::wrap)
                .map(|ancestor| legal_structure::MarkupAncestor {
                    tag: ancestor.value().name().to_owned(),
                    attributes: ancestor
                        .value()
                        .attrs()
                        .map(|(name, value)| (name.to_owned(), value.to_owned()))
                        .collect(),
                })
                .collect::<Vec<_>>();
            let input = serde_json::from_value(
                json!({"provider":"courtlistener","id":format!("{}:{index}",capture["cluster_id"].as_str().unwrap_or("")),"url":capture["final_url"],"markup":markup,"markupAncestors":markup_ancestors,"text":"",
                    "pageCitations":page_citations,
                    "scope":{"kind":"complete","markupSerialization":"browser_html"}}),
            )?;
            let doc = legal_structure::analyze_native_markup(input)
                .map_err(|error| format!("{} body {index}: {error}", capture["final_url"]))?;
            let query = DocumentQuery::new();
            let anchors = doc
                .nodes
                .iter()
                .filter_map(|node| node.anchor.as_deref())
                .collect::<BTreeSet<_>>();
            let reference_anchors = doc
                .notes
                .iter()
                .flat_map(|note| &note.references)
                .filter_map(|reference| reference.anchor.as_deref())
                .collect::<BTreeSet<_>>();
            let element_anchors = doc
                .markup_elements
                .iter()
                .filter_map(|element| element.attributes.get("id").map(String::as_str))
                .collect::<BTreeSet<_>>();
            let observed = evidence["bodies"][index]["anchors"]
                .as_array()
                .ok_or("browser body evidence missing")?;
            let observed_ids = observed
                .iter()
                .filter_map(|anchor| anchor["id"].as_str())
                .collect::<BTreeSet<_>>();
            let coordinates = legal_structure::ScalarText::new(doc.query_text());
            let markup_byte_ranges_valid = doc.markup_sha256.as_deref()
                == Some(hash(markup.as_bytes()).as_str())
                && doc.markup_elements.iter().all(|element| {
                    markup
                        .get(element.markup_start..element.markup_end)
                        .is_some_and(|source| {
                            source
                                .strip_prefix('<')
                                .is_some_and(|source| source.starts_with(&element.tag))
                        })
                });
            let markup_anchor_lookups = element_anchors.iter().map(|anchor| {
                json!({"anchor":anchor,"markup_start":query.source_element(&doc,anchor).map(|element|element.markup_start)})
            }).collect::<Vec<_>>();
            let markup_name_lookups = markup_name_lookups(&doc, &query);
            let mut source_structures = Vec::new();
            let mut footnote_ids = BTreeSet::new();
            for element in body
                .select(&id_selector)
                .filter(|element| structural_element(*element))
            {
                let id = element.value().attr("id").unwrap();
                let tag = element.value().name();
                if tag == "footnote"
                    || tag == "footnote_body"
                    || element.value().classes().any(|class| class == "footnote")
                {
                    footnote_ids.insert(id);
                }
                let native = doc
                    .nodes
                    .iter()
                    .filter(|node| node.anchor.as_deref() == Some(id))
                    .collect::<Vec<_>>();
                let node = query.source_anchor(&doc, id);
                let expected_parent = element
                    .ancestors()
                    .filter_map(ElementRef::wrap)
                    .take_while(|ancestor| ancestor.id() != body.id())
                    .find(|ancestor| {
                        structural_element(*ancestor) && ancestor.value().attr("id").is_some()
                    })
                    .and_then(|ancestor| ancestor.value().attr("id"));
                let actual_parent = node
                    .and_then(|node| node.parent_id.as_deref())
                    .and_then(|id| doc.nodes.iter().find(|node| node.id == id))
                    .and_then(|node| node.anchor.as_deref());
                let source = observed.iter().find(|source| source["id"] == id);
                let exact = node.zip(source).is_some_and(|(node, source)| {
                    let range = doc.query_range(node);
                    coordinates
                        .slice_utf16(range.start..range.end)
                        .is_some_and(|text| {
                            normalize_javascript_whitespace(text)
                                == normalize_javascript_whitespace(source["text"].as_str().unwrap())
                        })
                });
                let entry = structural_totals.entry(tag.to_owned()).or_insert(json!({"source":0,"retained":0,"unique_native_anchor":0,"matching_source_tags":0,"exact_normalized_browser_text":0,"expected_parents":0,"matching_parents":0}));
                for (field, amount) in [
                    ("source", 1),
                    ("retained", usize::from(!native.is_empty())),
                    ("unique_native_anchor", usize::from(node.is_some())),
                    (
                        "matching_source_tags",
                        usize::from(
                            node.is_some_and(|node| node.markup_tag.as_deref() == Some(tag)),
                        ),
                    ),
                    ("exact_normalized_browser_text", usize::from(exact)),
                    ("expected_parents", usize::from(expected_parent.is_some())),
                    (
                        "matching_parents",
                        usize::from(expected_parent.is_some() && expected_parent == actual_parent),
                    ),
                ] {
                    entry[field] = json!(entry[field].as_u64().unwrap() + amount as u64);
                }
                source_structures.push(
                    json!({"anchor":id,"tag":tag,"native_kind":node.map(|node|node.kind),
                    "native_node_id":node.map(|node|&node.id),"native_anchor_count":native.len(),"native_markup_tag":node.and_then(|node|node.markup_tag.as_deref()),
                    "expected_parent_anchor":expected_parent,"actual_parent_anchor":actual_parent,
                    "exact_normalized_browser_text":exact}),
                );
            }
            let source_note_links = evidence["bodies"][index]["links"].as_array().ok_or("browser links missing")?
                .iter().filter_map(|link| {
                    let target = link["href"].as_str()?.strip_prefix('#')?;
                    if !footnote_ids.contains(target) { return None; }
                    let note_node = query.source_anchor(&doc, target);
                    let matched = note_node.is_some_and(|node| doc.notes.iter().filter(|note| note.node_id == node.id)
                        .flat_map(|note| &note.references).any(|reference|
                            (link["id"] == "" || reference.anchor.as_deref() == link["id"].as_str()) && coordinates.slice_utf16(reference.range.start..reference.range.end).is_some_and(|text|
                                text == normalize_javascript_whitespace(link["text"].as_str().unwrap()))));
                    footnote_links += 1;
                    linked_references += usize::from(matched);
                    Some(json!({"source_id":link["id"],"target_anchor":target,"text":link["text"],"native_note_reference":matched}))
                }).collect::<Vec<_>>();
            let mut links = Vec::new();
            let mut exact_source_links = Vec::new();
            for node in doc.nodes.iter().filter(|node| node.anchor.is_some()) {
                let anchor = node.anchor.as_deref().unwrap();
                let source = observed.iter().find(|source| source["id"] == anchor);
                let unique = page_ids.get(anchor) == Some(&1);
                let visible = source.is_some_and(|source| source["visible"] == true);
                let resolved = query
                    .source_anchor(&doc, anchor)
                    .is_some_and(|found| found.id == node.id);
                exact_source_links.push(json!({"kind":node.kind,"label":node.label,"anchor":anchor,
                    "source_tag":source.map(|source|&source["tag"]),
                    "eligible_anchor_url":if source.is_some() && unique && visible && resolved {
                        Some(format!("{}#{anchor}", capture["final_url"].as_str().unwrap()))
                    } else { None },
                    "source_id_match":source.is_some(),"unique_page_id":unique,"visible_source_target":visible,
                    "native_source_anchor_resolves":resolved,"range":doc.query_range(node),"browser_navigation_verified":false}));
            }
            for note in &doc.notes {
                for reference in &note.references {
                    let Some(anchor) = reference.anchor.as_deref() else {
                        continue;
                    };
                    let source = observed.iter().find(|source| source["id"] == anchor);
                    let unique = page_ids.get(anchor) == Some(&1);
                    let visible = source.is_some_and(|source| source["visible"] == true);
                    let resolved = query.source_note_reference(&doc, anchor).is_some_and(
                        |(found, found_reference)| {
                            found.node_id == note.node_id && found_reference == reference
                        },
                    );
                    exact_source_links.push(json!({"kind":"footnote_reference","anchor":anchor,"note_node_id":note.node_id,
                        "source_tag":source.map(|source|&source["tag"]),
                        "eligible_anchor_url":if source.is_some() && unique && visible && resolved {
                            Some(format!("{}#{anchor}", capture["final_url"].as_str().unwrap()))
                        } else { None },
                        "source_id_match":source.is_some(),"unique_page_id":unique,"visible_source_target":visible,
                        "native_source_anchor_resolves":resolved,"range":reference.range,"browser_navigation_verified":false}));
                }
            }
            for element in &doc.markup_elements {
                let Some(anchor) = element.attributes.get("id") else {
                    continue;
                };
                if anchors.contains(anchor.as_str()) || reference_anchors.contains(anchor.as_str())
                {
                    continue;
                }
                let source = observed.iter().find(|source| source["id"] == *anchor);
                let unique = page_ids.get(anchor.as_str()) == Some(&1);
                let visible = source.is_some_and(|source| source["visible"] == true);
                let resolved = query
                    .source_element(&doc, anchor)
                    .is_some_and(|found| found.markup_start == element.markup_start);
                let note_label = query.source_note_label(&doc, anchor);
                exact_source_links.push(json!({"kind":if note_label.is_some() {"footnote_label"} else {"source_element"},
                    "anchor":anchor,"note_node_id":note_label.map(|(note,_)|&note.node_id),"source_tag":source.map(|source|&source["tag"]),
                    "eligible_anchor_url":if source.is_some() && unique && visible && resolved {Some(format!("{}#{anchor}",capture["final_url"].as_str().unwrap()))} else {None},
                    "source_id_match":source.is_some(),"unique_page_id":unique,"visible_source_target":visible,
                    "native_source_anchor_resolves":resolved,"range":element.range,"browser_navigation_verified":false}));
            }
            for block in query
                .blocks(&doc, None)
                .filter(|block| block.anchor.is_some())
            {
                let anchor = block.anchor.as_deref().unwrap();
                let source = observed.iter().find(|source| source["id"] == anchor);
                let unique = page_ids.get(anchor) == Some(&1);
                let visible = source.is_some_and(|source| source["visible"] == true);
                let resolved = query
                    .read_range(&doc, block.kind, &block.label, &block.label, 0)
                    .is_some_and(|range| !range.selected.is_empty());
                let kind = serde_json::to_value(block.kind)?
                    .as_str()
                    .unwrap()
                    .to_owned();
                let entry = native_links.entry(kind).or_insert(json!({"anchors":0,"source_id_matches":0,
                    "unique_page_ids":0,"visible_source_targets":0,"native_query_resolutions":0,"point_anchors":0}));
                for (field, amount) in [
                    ("anchors", 1),
                    ("source_id_matches", usize::from(source.is_some())),
                    ("unique_page_ids", usize::from(unique)),
                    ("visible_source_targets", usize::from(visible)),
                    ("native_query_resolutions", usize::from(resolved)),
                    ("point_anchors", usize::from(block.start == block.end)),
                ] {
                    entry[field] = json!(entry[field].as_u64().unwrap() + amount as u64);
                }
                links.push(json!({"kind":block.kind,"label":block.label,"aliases":block.aliases,
                    "anchor":anchor,"url":format!("{}#{anchor}", capture["final_url"].as_str().unwrap()),
                    "eligible_anchor_url":if source.is_some() && unique && visible {
                        Some(format!("{}#{anchor}", capture["final_url"].as_str().unwrap()))
                    } else { None },
                    "source_id_match":source.is_some(),"unique_page_id":unique,"visible_source_target":visible,
                    "native_query_resolves":resolved,"range":[block.start,block.end],"browser_navigation_verified":false}));
            }
            for source in observed.iter().filter(|source| {
                !anchors.contains(source["id"].as_str().unwrap())
                    && !reference_anchors.contains(source["id"].as_str().unwrap())
                    && !element_anchors.contains(source["id"].as_str().unwrap())
            }) {
                *unrepresented_ids
                    .entry(source["tag"].as_str().unwrap().to_owned())
                    .or_default() += 1;
            }
            let container_id = evidence["bodies"][index]["containerId"]
                .as_str()
                .unwrap_or_default();
            let heading = opinion_heading(body);
            let heading_id = heading
                .and_then(|heading| heading.value().attr("id"))
                .unwrap_or_default();
            let published_href = format!("#{heading_id}");
            let navigation_match = !heading_id.is_empty()
                && html
                    .select(&link_selector)
                    .any(|link| link.value().attr("href") == Some(published_href.as_str()));
            let boundary_link = json!({"anchor":heading_id,"url":format!("{}#{heading_id}", capture["final_url"].as_str().unwrap()),
                "container_anchor":container_id,"heading_text":heading.map(|heading|normalize_javascript_whitespace(&heading.text().collect::<String>())),
                "published_navigation_href":if navigation_match {Some(published_href)} else {None},
                "eligible_anchor_url":if navigation_match && page_ids.get(heading_id)==Some(&1)
                    && evidence["bodies"][index]["visible"]==true {
                    Some(format!("{}#{heading_id}", capture["final_url"].as_str().unwrap()))
                } else { None },
                "unique_page_id":page_ids.get(heading_id)==Some(&1),"body_visible":evidence["bodies"][index]["visible"],
                "heading_visible":null,"heading_source":"Retained full browser HTML and published navigation href",
                "browser_navigation_verified":false});
            let mut browser_targets = exact_source_links.iter().filter(|link| !link["eligible_anchor_url"].is_null())
                .map(|link|json!({"anchor":link["anchor"],"source_tag":link["source_tag"],"kind":link["kind"],"target":link["eligible_anchor_url"]}))
                .collect::<Vec<_>>();
            if !boundary_link["eligible_anchor_url"].is_null() {
                browser_targets.push(json!({"anchor":heading_id,"source_tag":"h3","kind":"opinion_boundary","target":boundary_link["eligible_anchor_url"]}));
            }
            serde_json::to_writer(
                &mut anchor_targets,
                &json!({"url":capture["final_url"],"html_sha256":capture["sha256"],
                "file":relative.file_name().unwrap().to_string_lossy(),"body_index":index,"targets":browser_targets}),
            )?;
            anchor_targets.write_all(b"\n")?;
            let internal_links = evidence["bodies"][index]["links"].as_array().ok_or("browser links missing")?
                .iter().filter_map(|link| {
                    let target = link["href"].as_str()?.strip_prefix('#')?;
                    Some(json!({"source_id":link["id"],"href":link["href"],"target_id":target,
                        "target_in_same_body":observed_ids.contains(target),"target_in_page":page_ids.contains_key(target),
                        "unique_page_id":page_ids.get(target)==Some(&1)}))
                }).collect::<Vec<_>>();
            let paragraph_ids = observed
                .iter()
                .filter(|a| a["tag"] == "p")
                .map(|a| a["id"].as_str().unwrap())
                .collect::<Vec<_>>();
            let recovered = paragraph_ids
                .iter()
                .filter(|id| anchors.contains(**id))
                .count();
            let mut paragraphs = Vec::new();
            let mut plans = Vec::new();
            for paragraph in observed.iter().filter(|a| a["tag"] == "p") {
                let id = paragraph["id"]
                    .as_str()
                    .ok_or("browser paragraph id missing")?;
                let original_text = paragraph["text"]
                    .as_str()
                    .ok_or("browser paragraph text missing")?;
                let lookup = query.read_range(&doc, DocumentKind::Paragraph, id, id, 0);
                let parsed_text = lookup
                    .as_ref()
                    .and_then(|range| range.selected.first())
                    .map(|block| block.text.as_str());
                let equal = parsed_text.is_some_and(|text| {
                    normalize_javascript_whitespace(text)
                        == normalize_javascript_whitespace(original_text)
                });
                if parsed_text.is_some() {
                    lookup_found += 1;
                }
                if equal {
                    exact_text += 1;
                } else if parsed_text.is_some() {
                    text_mismatches += 1;
                }
                paragraphs.push(json!({"id":id,"lookup_found":parsed_text.is_some(),"exact_normalized_browser_text":equal,
                    "parsed_text_on_mismatch":if equal{None}else{parsed_text},
                    "source_text_sha256":hash(original_text.as_bytes()),"parsed_text_sha256":parsed_text.map(|text|hash(text.as_bytes()))}));
            }
            let key = format!(
                "{}-b{index}",
                capture["cluster_id"].as_str().ok_or("cluster ID missing")?
            );
            serde_json::to_writer(
                &mut source_texts,
                &json!({"key":key,"text":doc.query_text()}),
            )?;
            source_texts.write_all(b"\n")?;
            if plan_fragments && evidence["bodies"][index]["visible"] == true {
                let observed_text = evidence["bodies"][index]["innerText"]
                    .as_str()
                    .ok_or("independent visible browser text missing")?;
                let words = observed_text.split_whitespace().collect::<Vec<_>>();
                let samples = words.len().min(8);
                // One repeatable random window per eighth of the whole visible opinion,
                // including unanchored prose, notes and crossings between source blocks.
                for sample in 0..samples {
                    let seed = hash(
                        format!("{}:{index}:{sample}:random-window-v1", capture["sha256"])
                            .as_bytes(),
                    );
                    let random = u64::from_str_radix(&seed[..16], 16)? as usize;
                    let first = words.len() * sample / samples;
                    let last = words.len() * (sample + 1) / samples;
                    let start = first + random % (last - first);
                    let end = (start + 6 + (random >> 16) % 43).min(words.len());
                    let quote = words[start..end].join(" ");
                    let mut plan = serde_json::to_value(query.text_fragment_plan(
                        &doc,
                        &quote,
                        &[quote.clone()],
                        false,
                        true,
                        true,
                        false,
                        false,
                    ))?;
                    plan["quote"] = json!(quote);
                    plan["sample_index"] = json!(sample);
                    plan["browser_word_interval"] = json!([start, end]);
                    plan["browser_word_count"] = json!(words.len());
                    plan["live_paint_verified"] = json!(false);
                    let directives = plan["directives"]
                        .as_array()
                        .ok_or("native directives missing")?
                        .iter()
                        .map(|value| value.as_str().ok_or("native directive is not text"))
                        .collect::<std::result::Result<Vec<_>, _>>()?
                        .join("&");
                    let mut target = plan.clone();
                    target["label"] = json!(format!("COURTLISTENER_{key}_p{sample}_random"));
                    target["dataset"] = json!("COURTLISTENER");
                    target["target"] = json!(format!(
                        "{}#:~:{directives}",
                        capture["final_url"].as_str().ok_or("source URL missing")?
                    ));
                    target["quotes"] = json!([quote]);
                    target["blockText"] = json!(quote);
                    target["html_sha256"] = capture["sha256"].clone();
                    serde_json::to_writer(&mut targets, &target)?;
                    targets.write_all(b"\n")?;
                    planned += 1;
                    complete_plans += usize::from(plan["sourceSafeComplete"] == true);
                    plans.push(plan);
                }
            }
            source_ids += paragraph_ids.len();
            recovered_ids += recovered;
            bodies += 1;
            serde_json::to_writer(
                &mut receipts,
                &json!({"html_sha256":capture["sha256"],"body_index":index,"url":capture["final_url"],"source_paragraph_ids":paragraph_ids,"recovered_paragraph_ids":recovered,"node_anchors":anchors,"reference_anchors":reference_anchors,"paragraphs":paragraphs,
                    "page_citations":page_citations,
                    "citation_context":citation_context.as_ref().map(|context|context.cluster_provenance(cluster_id)).unwrap_or(json!({"status":"not_supplied"})),
                    "markup_serialization":"browser_html",
                    "native_anchor_links":links,"opinion_boundary_link":boundary_link,"source_internal_links":internal_links,
                    "source_structures":source_structures,"source_note_links":source_note_links,"notes":doc.notes,
                    "document_outline":legal_structure_features::document_outline(&doc, false, None),
                    "source_markup":query.source_markup(&doc),
                    "source_opinion_components":query.source_opinion_components(&doc),
                    "native_pinpoint_queries":pinpoint_queries(&doc, &query, Derivation::Native),
                    "page_alias_queries":page_alias_queries(&doc, &query),
                    "note_label_queries":note_label_queries(&doc, &query),
                    "native_structures":doc.nodes,"markup_elements":doc.markup_elements,"markup_ancestors":doc.markup_ancestors,"markup_sha256":doc.markup_sha256,"source_markup_byte_start":markup_byte_start,"source_markup_byte_end":markup_byte_start+markup.len(),
                    "markup_byte_ranges_valid":markup_byte_ranges_valid,"markup_anchor_lookups":markup_anchor_lookups,
                    "markup_name_lookups":markup_name_lookups,"table_cells":query.table_cells(&doc),
                    "exact_source_anchor_links":exact_source_links,
                    "plans":plans,"text":doc.query_text()}),
            )?;
            receipts.write_all(b"\n")?;
        }
    }
    receipts.finish()?;
    write_json(
        out.join("summary.json"),
        &json!({"schema":"courtlistener.browser-richness.v1","htmls":captures.len(),"bodies":bodies,
      "source_paragraph_ids":source_ids,"recovered_paragraph_ids":recovered_ids,"engine_source_sha256":legal_structure::ENGINE_SOURCE_SHA256,"features_source_sha256":legal_structure_features::SOURCE_SHA256,
      "query_source_sha256":hash(include_bytes!("../../../query/src/document_query.rs")),
      "citation_context":citation_context.as_ref().map(|context|context.provenance()),
      "pages_with_citation_context":pages_with_citation_context,
      "input_context_seconds":input_context_seconds,
      "native_anchor_lookups_found":lookup_found,"exact_normalized_browser_paragraphs":exact_text,"browser_text_mismatches":text_mismatches,"seconds":started.elapsed().as_secs_f64(),
      "random_fragment_plans":planned,"source_complete_plans":complete_plans,
      "fragment_planning_enabled":plan_fragments,
      "native_anchor_links":native_links,"source_ids_without_native_nodes_by_tag":unrepresented_ids,
      "source_structural_nodes":structural_totals,"source_footnote_links":footnote_links,"native_linked_footnote_references":linked_references,
      "anchor_link_proof":"Independent browser DOM ID, whole-page uniqueness, captured visibility and native locator resolution; browser navigation is not claimed",
      "sampling":"SHA-256 seeded 6–48 word windows, one random start per eighth of independent visible browser body text",
      "gold_source":"Independently captured browser DOM anchors; never generated from parser output","live_fragment_painting":"not_run"}),
    )
}
