//! Persist retained producer facts independently of the authored composition.
//! Only source identities and scalar coordinates are durable; UTF-16 offsets are hydrated.
use crate::{
    CompositionAnnotation, CompositionLine, ScalarRange, ScalarText, SourceSpan, SourceTextEdit,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionNativeFacts {
    pub nodes: Vec<NativeCompositionFact>,
    pub pages: BTreeMap<usize, NativeCompositionFact>,
    pub metadata: BTreeMap<String, NativeCompositionFact>,
    pub cross_reference_edges: Vec<NativeCompositionFact>,
    pub unconfirmed_native_witnesses: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCompositionFact {
    pub value: Value,
    pub bindings: Vec<NativeCompositionBinding>,
    /// Immutable producer evidence, never interpreted as current coordinates.
    pub witness: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCompositionBinding {
    /// JSON pointer to the coordinate field (including an escaped object key).
    pub path: String,
    pub kind: NativeCompositionBindingKind,
    pub spans: Vec<SourceSpan>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCompositionBindingKind {
    Range,
    Envelope,
    WordingEnvelope,
    Point,
    Membership,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionNativeCaptureRequest {
    pub structure: Value,
    #[serde(default)]
    pub lines: Vec<CompositionLine>,
    pub retained_node_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionNativeHydrationRequest {
    pub native_facts: CompositionNativeFacts,
    pub lines: Vec<CompositionLine>,
    pub reading_order: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct HydratedCompositionNativeFacts {
    pub structure: Value,
    pub retained_node_ids: Vec<String>,
    pub removable_nodes: Vec<CompositionAnnotation>,
    pub unconfirmed_native_witnesses: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionNativeRemapRequest {
    pub native_facts: CompositionNativeFacts,
    pub before_lines: Vec<CompositionLine>,
    pub after_lines: Vec<CompositionLine>,
    #[serde(default)]
    pub expansion: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub fragment_ranges: BTreeMap<String, [usize; 2]>,
    #[serde(default)]
    pub edits: Vec<SourceTextEdit>,
    #[serde(default)]
    pub retained_node_ids: Option<Vec<String>>,
}

fn decode<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}
fn pointer(path: &str, key: &str) -> String {
    format!("{}/{}", path, key.replace('~', "~0").replace('/', "~1"))
}
fn record_unconfirmed(
    out: &mut Vec<Value>,
    kind: &str,
    fact: &NativeCompositionFact,
    reason: String,
) {
    let record = json!({"kind":kind,"witness":fact.witness,"reason":reason});
    if !out.contains(&record) {
        out.push(record);
    }
}

struct SourceCoordinates {
    text: String,
    fragments: Vec<(String, ScalarRange, String, usize)>,
    page_ranges: BTreeMap<usize, ScalarRange>,
}
impl SourceCoordinates {
    fn capture(structure: &Value) -> Result<Self> {
        let text = structure["text"]
            .as_str()
            .ok_or("Native capture needs current source text")?
            .to_owned();
        let coordinates = ScalarText::new(&text);
        let mut fragments = Vec::new();
        let mut ids = HashSet::new();
        for fragment in structure["source_fragments"]
            .as_array()
            .ok_or("Native capture needs current source fragments")?
        {
            let id = fragment["id"]
                .as_str()
                .ok_or("Source fragment identity missing")?
                .to_owned();
            if !ids.insert(id.clone()) {
                return Err("Repeated native source fragment".into());
            }
            let range: ScalarRange = decode(fragment["range"].clone())?;
            let value = coordinates
                .slice_utf16(range.start..range.end)
                .ok_or("Source fragment outside scalar boundaries")?
                .to_owned();
            let page = fragment["page_index"]
                .as_u64()
                .ok_or("Source fragment page missing")? as usize;
            fragments.push((id, range, value, page));
        }
        fragments.sort_by_key(|(_, range, _, _)| range.start);
        if fragments.windows(2).any(|p| p[0].1.end > p[1].1.start) {
            return Err("Native source fragments overlap".into());
        }
        Ok(Self {
            text,
            fragments,
            page_ranges: BTreeMap::new(),
        })
    }
    fn current(
        lines: &[CompositionLine],
        order: &[String],
        physical_pages: impl Iterator<Item = usize>,
    ) -> Result<Self> {
        let by_id: HashMap<_, _> = lines.iter().map(|line| (line.id.as_str(), line)).collect();
        let selected: HashSet<_> = order.iter().map(String::as_str).collect();
        if by_id.len() != lines.len()
            || selected.len() != order.len()
            || selected.len() != lines.len()
            || selected.iter().any(|id| !by_id.contains_key(id))
            || lines
                .iter()
                .any(|line| line.id.is_empty() || line.page == 0)
        {
            return Err("Native hydration order must name each source unit once".into());
        }
        // Physical source coordinates group pages just as the shared projector does.
        let pages: BTreeSet<_> = lines
            .iter()
            .map(|line| line.page)
            .chain(physical_pages.map(|p| p + 1))
            .collect();
        let mut text = String::new();
        let mut fragments = Vec::new();
        let mut cursor = 0;
        let mut page_ranges = BTreeMap::new();
        for (page_position, page) in pages.into_iter().enumerate() {
            if page_position > 0 {
                text.push('\n');
                cursor += 1;
            }
            let page_start = cursor;
            let mut first = true;
            for id in order.iter().filter(|id| by_id[id.as_str()].page == page) {
                let line = by_id[id.as_str()];
                if !first {
                    text.push('\n');
                    cursor += 1;
                }
                first = false;
                let end = cursor + line.text.encode_utf16().count();
                fragments.push((
                    id.clone(),
                    ScalarRange { start: cursor, end },
                    line.text.clone(),
                    page - 1,
                ));
                text.push_str(&line.text);
                cursor = end;
            }
            page_ranges.insert(
                page - 1,
                ScalarRange {
                    start: page_start,
                    end: cursor,
                },
            );
        }
        Ok(Self {
            text,
            fragments,
            page_ranges,
        })
    }
    fn bind(&self, range: ScalarRange) -> Result<Vec<SourceSpan>> {
        if range.start > range.end {
            return Err("Native anchor is reversed".into());
        }
        let mut result = Vec::new();
        for (id, extent, text, _) in &self.fragments {
            let overlaps = if range.start == range.end {
                extent.start <= range.start && range.end <= extent.end
            } else {
                extent.start < range.end && range.start < extent.end
            };
            if !overlaps {
                continue;
            }
            let text = ScalarText::new(text);
            let start = text
                .scalar_at_utf16(range.start.max(extent.start) - extent.start)
                .ok_or("Native anchor splits a character")?;
            let end = text
                .scalar_at_utf16(range.end.min(extent.end) - extent.start)
                .ok_or("Native anchor splits a character")?;
            result.push(SourceSpan {
                line_id: id.clone(),
                start,
                end,
            });
        }
        if result.is_empty() || range.start == range.end && result.len() != 1 {
            return Err("Native anchor lacks unique source membership".into());
        }
        let mapped = self.ranges(&result)?;
        if mapped.first().unwrap().start != range.start || mapped.last().unwrap().end != range.end {
            return Err("Native anchor endpoint has no exact source identity".into());
        }
        let text = ScalarText::new(&self.text);
        if mapped.windows(2).any(|p| {
            !text
                .slice_utf16(p[0].end..p[1].start)
                .is_some_and(|s| s.chars().all(char::is_whitespace))
        }) {
            return Err("Native anchor crosses unrepresented source text".into());
        }
        Ok(result)
    }
    fn ranges(&self, spans: &[SourceSpan]) -> Result<Vec<ScalarRange>> {
        spans
            .iter()
            .map(|span| {
                let (_, range, text, _) = self
                    .fragments
                    .iter()
                    .find(|(id, _, _, _)| id == &span.line_id)
                    .ok_or("Native anchor source no longer exists")?;
                let text = ScalarText::new(text);
                if span.start > span.end || span.end > text.len() {
                    return Err("Native anchor outside current source".into());
                }
                Ok(ScalarRange {
                    start: range.start + text.utf16(span.start),
                    end: range.start + text.utf16(span.end),
                })
            })
            .collect()
    }
}

fn add_binding(
    value: &mut Value,
    path: String,
    kind: NativeCompositionBindingKind,
    range: ScalarRange,
    source: &SourceCoordinates,
    bindings: &mut Vec<NativeCompositionBinding>,
) -> Result<()> {
    bindings.push(NativeCompositionBinding {
        path,
        kind,
        spans: source.bind(range)?,
    });
    *value = Value::Null;
    Ok(())
}
fn capture_value(
    value: &mut Value,
    path: &str,
    source: &SourceCoordinates,
    bindings: &mut Vec<NativeCompositionBinding>,
) -> Result<()> {
    if value
        .as_object()
        .is_some_and(|v| v.len() == 2 && v.contains_key("start") && v.contains_key("end"))
    {
        let range = decode(value.clone())?;
        return add_binding(
            value,
            path.to_owned(),
            NativeCompositionBindingKind::Range,
            range,
            source,
            bindings,
        );
    }
    match value {
        Value::Object(object) => {
            // Typed graph and contents fields use numeric coordinate pairs rather than range objects.
            for (start, end) in [
                ("sourceStart", "sourceEnd"),
                ("source_start", "source_end"),
                ("targetStart", "targetEnd"),
                ("target_start", "target_end"),
                ("regionStart", "regionEnd"),
                ("region_start", "region_end"),
            ] {
                if object.get(start).is_some_and(|v| !v.is_null())
                    || object.get(end).is_some_and(|v| !v.is_null())
                {
                    let a = object
                        .get(start)
                        .and_then(Value::as_u64)
                        .ok_or("Native coordinate pair is incomplete")?
                        as usize;
                    let b = object
                        .get(end)
                        .and_then(Value::as_u64)
                        .ok_or("Native coordinate pair is incomplete")?
                        as usize;
                    let spans = source.bind(ScalarRange { start: a, end: b })?;
                    // Paired endpoints share complete membership so deleting an interior fragment invalidates the fact.
                    bindings.push(NativeCompositionBinding {
                        path: pointer(path, start),
                        kind: if start == "sourceStart" || start == "source_start" {
                            NativeCompositionBindingKind::WordingEnvelope
                        } else {
                            NativeCompositionBindingKind::Envelope
                        },
                        spans: spans.clone(),
                    });
                    bindings.push(NativeCompositionBinding {
                        path: pointer(path, end),
                        kind: if start == "sourceStart" || start == "source_start" {
                            NativeCompositionBindingKind::WordingEnvelope
                        } else {
                            NativeCompositionBindingKind::Envelope
                        },
                        spans,
                    });
                    object.insert(start.into(), Value::Null);
                    object.insert(end.into(), Value::Null);
                }
            }
            for (key, child) in object.iter_mut() {
                if key == "native_witness" {
                    continue;
                }
                if matches!(
                    key.as_str(),
                    "content_start" | "contents_line_start" | "contentsLineStart"
                ) && !child.is_null()
                {
                    let point = child.as_u64().ok_or("Native point is not an integer")? as usize;
                    add_binding(
                        child,
                        pointer(path, key),
                        NativeCompositionBindingKind::Point,
                        ScalarRange {
                            start: point,
                            end: point,
                        },
                        source,
                        bindings,
                    )?;
                } else {
                    capture_value(child, &pointer(path, key), source, bindings)?;
                }
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter_mut().enumerate() {
                capture_value(child, &pointer(path, &index.to_string()), source, bindings)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn capture_fact(value: Value, source: &SourceCoordinates) -> Result<NativeCompositionFact> {
    let mut fact = NativeCompositionFact {
        witness: value.clone(),
        value,
        bindings: Vec::new(),
    };
    capture_value(&mut fact.value, "", source, &mut fact.bindings)?;
    Ok(fact)
}
fn capture_node(
    value: Value,
    source: &SourceCoordinates,
    page: bool,
) -> Result<NativeCompositionFact> {
    let witness = value.clone();
    let mut value = value;
    let object = value
        .as_object_mut()
        .ok_or("Native node must be an object")?;
    let ranges: Vec<ScalarRange> = if page {
        Vec::new()
    } else if let Some(ranges) = object
        .get("ranges")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
    {
        decode(json!(ranges))?
    } else {
        vec![decode(
            object
                .get("range")
                .cloned()
                .ok_or("Native node range absent")?,
        )?]
    };
    for key in [
        "range",
        "ranges",
        "extent",
        "level",
        "rendered_range",
        "rendered_ranges",
        "rendered_membership",
        "line_ids",
        "page_indexes",
    ] {
        object.remove(key);
    }
    if page {
        object.remove("content_start");
        object.remove("heading_range");
        object.remove("marker_range");
    }
    let mut fact = capture_fact(value, source)?;
    fact.witness = witness;
    if !page {
        let mut spans = Vec::new();
        for range in ranges {
            spans.extend(source.bind(range)?);
        }
        fact.value["ranges"] = Value::Null;
        fact.bindings.push(NativeCompositionBinding {
            path: "/ranges".into(),
            kind: NativeCompositionBindingKind::Membership,
            spans,
        });
    }
    Ok(fact)
}

fn provenance(structure: &Value, raw: &Value) -> Value {
    json!({"document_id":structure["document_id"],"text_sha256":structure["text_sha256"],"source_sha256":structure["source_sha256"],"offset_unit":structure["offset_unit"],"facts":raw})
}
pub(crate) fn preserve_reading_stream_evidence(
    structure: &Value, physical: &[ScalarRange], witnesses: &mut Vec<Value>, unconfirmed: &mut Vec<Value>,
) -> Result<()> {
    let text = ScalarText::new(structure["text"].as_str().ok_or("Native stream evidence needs source text")?);
    for raw in structure.get("scope_annotations").and_then(Value::as_array).into_iter().flatten()
        .filter(|scope| scope["kind"] == "reading_stream") {
        let scope: crate::StructureScopeAnnotation = decode(raw.clone())?;
        let (_, reason) = crate::composition_seed::reading_stream_source_membership(&scope, &text, physical)?;
        let witness = if let Some(prior) = witnesses.iter().find(|w| w["field"] == "reading_stream_scope" && w["facts"]["id"] == scope.id
            && (reason.is_none() || w["facts"] == *raw && w["text_sha256"] == structure["text_sha256"])) {
            prior.clone()
        } else {
            let mut witness = provenance(structure, raw);
            witness["field"] = json!("reading_stream_scope");
            witnesses.push(witness.clone());
            witness
        };
        if let Some(reason) = reason {
            let record = json!({"kind":"reading_stream","witness":witness,"reason":reason});
            if !unconfirmed.contains(&record) { unconfirmed.push(record); }
        }
    }
    Ok(())
}

fn witnessed_value(mut typed: Value, raw: &Value, structure: &Value) -> Value {
    let witnesses = raw
        .get("native_witness")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![provenance(structure, raw)]);
    if let Some(object) = typed.as_object_mut() {
        object.insert("native_witness".into(), json!(witnesses));
    }
    typed
}
fn page_skeleton(value: &Value) -> Value {
    let mut result = Map::new();
    for key in [
        "id",
        "kind",
        "source",
        "origin_id",
        "parent_id",
        "ownership",
        "document_id",
    ] {
        if let Some(value) = value.get(key) {
            result.insert(key.into(), value.clone());
        }
    }
    Value::Object(result)
}

pub fn capture_composition_native_facts(
    request: CompositionNativeCaptureRequest,
) -> Result<CompositionNativeFacts> {
    let mut source_structure = request.structure.clone();
    if source_structure
        .get("source_fragments")
        .and_then(Value::as_array)
        .is_none_or(|items| items.is_empty())
        && !request.lines.is_empty()
    {
        let fragments = request
            .lines
            .iter()
            .map(|line| {
                let range = line
                    .evidence
                    .get("range")
                    .ok_or("Native capture fallback needs explicit current line ranges")?;
                if line.page == 0 {
                    return Err("Native source page must be positive".to_owned());
                }
                Ok(json!({"id":line.id,"range":range,"page_index":line.page-1}))
            })
            .collect::<Result<Vec<_>>>()?;
        source_structure["source_fragments"] = json!(fragments);
    }
    let source = SourceCoordinates::capture(&source_structure)?;
    let canonical = serde_json::to_value(crate::composition_annotation_seed::typed_structure(
        &request.structure,
    )?)
    .map_err(|e| e.to_string())?;
    let object = request
        .structure
        .as_object()
        .ok_or("Native structure must be an object")?;
    let nodes = object
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("Native nodes missing")?;
    let selected: HashSet<_> = request
        .retained_node_ids
        .iter()
        .map(String::as_str)
        .collect();
    if selected.len() != request.retained_node_ids.len()
        || selected
            .iter()
            .any(|id| !nodes.iter().any(|n| n["id"].as_str() == Some(id)))
    {
        return Err("Native capture selection has absent or repeated identities".into());
    }
    let mut result = CompositionNativeFacts::default();
    result.unconfirmed_native_witnesses = object
        .get("unconfirmed_native_witnesses")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for raw in nodes {
        let page = raw["kind"].as_str() == Some("page");
        if !page && !raw["id"].as_str().is_some_and(|id| selected.contains(id)) {
            continue;
        }
        let typed: crate::StructureNode = decode(raw.clone())?;
        let typed = serde_json::to_value(typed).map_err(|e| e.to_string())?;
        let value = witnessed_value(typed, raw, &request.structure);
        let fact = match capture_node(value.clone(), &source, page) {
            Ok(mut fact) => {
                fact.witness = value["native_witness"].clone();
                Some(fact)
            }
            Err(reason) => {
                result
                    .unconfirmed_native_witnesses
                    .push(json!({"kind":"node","witness":value["native_witness"],"reason":reason}));
                page.then(|| NativeCompositionFact {
                    value: page_skeleton(&value),
                    bindings: vec![],
                    witness: value["native_witness"].clone(),
                })
            }
        };
        if let Some(fact) = fact {
            if page {
                let page = raw["page_indexes"]
                    .as_array()
                    .and_then(|v| v.first())
                    .and_then(Value::as_u64)
                    .ok_or("Native page index absent")? as usize;
                if result.pages.insert(page, fact).is_some() {
                    return Err("Repeated native physical page".into());
                }
            } else {
                result.nodes.push(fact);
            }
        }
    }
    let mut metadata_witnesses = object
        .get("native_witness")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    preserve_reading_stream_evidence(&request.structure,
        &source.fragments.iter().map(|(_, range, _, _)| *range).collect::<Vec<_>>(),
        &mut metadata_witnesses, &mut result.unconfirmed_native_witnesses)?;
    for (key, raw) in object {
        if matches!(
            key.as_str(),
            "native_witness"
                | "unconfirmed_native_witnesses"
                | "text"
                | "revision"
                | "text_sha256"
                | "nodes"
                | "notes"
                | "source_fragments"
                | "fields"
                | "annotations"
                | "documents"
                | "navigation"
                | "continuations"
                | "joins"
                | "manuscript_edits"
                | "rendered_text"
                | "scope_annotations"
        ) {
            continue;
        }
        if !metadata_witnesses
            .iter()
            .any(|w| w["field"].as_str() == Some(key))
        {
            let mut witness = provenance(&request.structure, raw);
            witness["field"] = json!(key);
            metadata_witnesses.push(witness);
        }
        let Some(mut value) = canonical.get(key).cloned() else {
            continue;
        };
        if key == "cross_references" && !value.is_null() {
            let edges = value["edges"]
                .as_array()
                .ok_or("Native graph edges absent")?
                .clone();
            let raw_edges = raw["edges"]
                .as_array()
                .ok_or("Native graph raw edges absent")?;
            for (edge, raw_edge) in edges.into_iter().zip(raw_edges) {
                let edge = witnessed_value(edge, raw_edge, &request.structure);
                match capture_fact(edge.clone(),&source) {
                    Ok(mut fact)=> {fact.witness=edge["native_witness"].clone();result.cross_reference_edges.push(fact);},
                    Err(reason)=>result.unconfirmed_native_witnesses.push(json!({"kind":"cross_reference","witness":edge["native_witness"],"reason":reason})),
                }
            }
            value["edges"] = json!([]);
        }
        match capture_fact(value, &source) {
            Ok(mut fact) => {
                fact.witness = metadata_witnesses
                    .iter()
                    .find(|w| w["field"].as_str() == Some(key))
                    .unwrap()
                    .clone();
                result.metadata.insert(key.clone(), fact);
            }
            Err(reason) => result
                .unconfirmed_native_witnesses
                .push(json!({"kind":"metadata","field":key,"witness":metadata_witnesses.iter().find(|w|w["field"].as_str()==Some(key)).unwrap(),"reason":reason})),
        }
    }
    let value = json!(metadata_witnesses);
    result.metadata.insert(
        "native_witness".into(),
        NativeCompositionFact {
            value: value.clone(),
            bindings: vec![],
            witness: value,
        },
    );
    Ok(result)
}

fn hydrate_fact(fact: &NativeCompositionFact, source: &SourceCoordinates) -> Result<Value> {
    let mut value = fact.value.clone();
    for binding in &fact.bindings {
        if binding.spans.is_empty() {
            return Err("Native binding has no source membership".into());
        }
        let mut ranges = source.ranges(&binding.spans)?;
        if matches!(binding.kind, NativeCompositionBindingKind::Membership) {
            ranges.sort_by_key(|r| (r.start, r.end));
        }
        let first = ranges.first().unwrap().start;
        let last = ranges.last().unwrap().end;
        if ranges.windows(2).any(|p| p[0].end > p[1].start) {
            return Err("Native source membership is no longer ordered".into());
        }
        let mapped = match binding.kind {
            NativeCompositionBindingKind::Membership => json!(ranges),
            NativeCompositionBindingKind::Point => {
                if ranges.len() != 1 || first != last {
                    return Err("Native point is no longer unique".into());
                }
                json!(first)
            }
            NativeCompositionBindingKind::Range
            | NativeCompositionBindingKind::Envelope
            | NativeCompositionBindingKind::WordingEnvelope => {
                let text = ScalarText::new(&source.text);
                if !matches!(binding.kind, NativeCompositionBindingKind::Envelope)
                    && ranges.windows(2).any(|p| {
                        !text
                            .slice_utf16(p[0].end..p[1].start)
                            .is_some_and(|s| s.chars().all(char::is_whitespace))
                    })
                {
                    return Err("Native range crosses changed or reordered source".into());
                }
                if matches!(
                    binding.kind,
                    NativeCompositionBindingKind::Envelope
                        | NativeCompositionBindingKind::WordingEnvelope
                ) {
                    json!(
                        if binding.path.ends_with("_end") || binding.path.ends_with("End") {
                            last
                        } else {
                            first
                        }
                    )
                } else {
                    json!({"start":first,"end":last})
                }
            }
        };
        *value
            .pointer_mut(&binding.path)
            .ok_or("Native binding path absent")? = mapped;
    }
    Ok(value)
}
fn node_membership(fact: &NativeCompositionFact) -> Vec<SourceSpan> {
    fact.bindings
        .iter()
        .find(|b| matches!(b.kind, NativeCompositionBindingKind::Membership) && b.path == "/ranges")
        .map(|b| b.spans.clone())
        .unwrap_or_default()
}
fn inventory(
    node: &Value,
    spans: Vec<SourceSpan>,
    witness: &Value,
) -> Result<CompositionAnnotation> {
    let spans: Vec<_> = spans
        .into_iter()
        .filter(|span| span.start < span.end)
        .collect();
    let line_ids = spans
        .iter()
        .map(|s| s.line_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(CompositionAnnotation {
        id: node["id"].as_str().ok_or("Native identity absent")?.into(),
        kind: node["kind"].as_str().ok_or("Native kind absent")?.into(),
        line_ids,
        spans,
        attributes: Map::new(),
        evidence: BTreeMap::from([
            (
                "parent_id".into(),
                node.get("parent_id").cloned().unwrap_or(Value::Null),
            ),
            (
                "native_witness".into(),
                if witness.is_array() {
                    witness.clone()
                } else {
                    json!([witness])
                },
            ),
        ]),
    })
}
fn refresh_graph_counts(graph: &mut Value) -> Result<()> {
    let mut typed: crate::InstrumentCrossReferenceGraph = decode(graph.clone())?;
    let c = &mut typed.counts;
    c.detected = typed.edges.len();
    c.resolved = 0;
    c.external = 0;
    c.unresolved = 0;
    c.abstained = 0;
    c.self_loops = 0;
    for edge in &typed.edges {
        match edge.status {
            crate::InstrumentCrossReferenceStatus::Resolved => c.resolved += 1,
            crate::InstrumentCrossReferenceStatus::External => c.external += 1,
            crate::InstrumentCrossReferenceStatus::Unresolved => c.unresolved += 1,
            crate::InstrumentCrossReferenceStatus::Abstained => c.abstained += 1,
        }
        c.self_loops += usize::from(edge.self_loop);
    }
    c.integrity = if c.resolved + c.unresolved == 0 {
        1.0
    } else {
        c.resolved as f64 / (c.resolved + c.unresolved) as f64
    };
    graph["counts"] = serde_json::to_value(c).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn hydrate_composition_native_facts(
    request: CompositionNativeHydrationRequest,
) -> Result<HydratedCompositionNativeFacts> {
    let source = SourceCoordinates::current(
        &request.lines,
        &request.reading_order,
        request.native_facts.pages.keys().copied(),
    )?;
    let facts = request.native_facts;
    let mut unconfirmed = facts.unconfirmed_native_witnesses;
    let mut structure = Map::new();
    for (key, fact) in &facts.metadata {
        match hydrate_fact(fact, &source) {
            Ok(mut value) => {
                if key == "structure_tags" {
                    if let Some(tags) = value.as_array_mut() {
                        for (index, tag) in tags.iter_mut().enumerate() {
                            let prefix = format!("/{index}/ranges/");
                            let ids: BTreeSet<_> = fact
                                .bindings
                                .iter()
                                .filter(|b| b.path.starts_with(&prefix))
                                .flat_map(|b| b.spans.iter().map(|s| s.line_id.clone()))
                                .collect();
                            tag["line_ids"] = json!(ids);
                        }
                    }
                }
                structure.insert(key.clone(), value);
            }
            Err(reason) => record_unconfirmed(&mut unconfirmed, "metadata", fact, reason),
        }
    }
    if let Some(graph) = structure
        .get_mut("cross_references")
        .filter(|v| !v.is_null())
    {
        let mut edges = Vec::new();
        for fact in &facts.cross_reference_edges {
            match hydrate_fact(fact, &source) {
                Ok(value) => edges.push(value),
                Err(reason) => {
                    record_unconfirmed(&mut unconfirmed, "cross_reference", fact, reason)
                }
            }
        }
        graph["edges"] = json!(edges);
        refresh_graph_counts(graph)?;
    }
    let mut nodes = Vec::new();
    let mut retained = Vec::new();
    let mut removable = Vec::new();
    for (page, fact) in &facts.pages {
        let mut node = match hydrate_fact(fact, &source) {
            Ok(node) => node,
            Err(reason) => {
                record_unconfirmed(&mut unconfirmed, "page", fact, reason);
                page_skeleton(&fact.value)
            }
        };
        let fragments: Vec<_> = source
            .fragments
            .iter()
            .filter(|(_, _, _, p)| p == page)
            .collect();
        let spans: Vec<_> = fragments
            .iter()
            .filter(|(_, _, text, _)| !text.is_empty())
            .map(|(id, _, text, _)| SourceSpan {
                line_id: id.clone(),
                start: 0,
                end: text.chars().count(),
            })
            .collect();
        let page_range = source
            .page_ranges
            .get(page)
            .copied()
            .ok_or("Native physical page absent")?;
        let start = page_range.start;
        let end = page_range.end;
        node["range"] = json!({"start":start,"end":end});
        node["page_indexes"] = json!([page]);
        node["line_ids"] = json!(fragments.iter().map(|f| &f.0).collect::<Vec<_>>());
        removable.push(inventory(&node, spans, &fact.witness)?);
        nodes.push(node);
    }
    for fact in &facts.nodes {
        match hydrate_fact(fact, &source) {
            Ok(mut node) => {
                let spans = node_membership(fact);
                let ranges = source.ranges(&spans)?;
                let start = ranges
                    .iter()
                    .map(|r| r.start)
                    .min()
                    .ok_or("Native node membership missing")?;
                let end = ranges.iter().map(|r| r.end).max().unwrap();
                node["range"] = json!({"start":start,"end":end});
                node["line_ids"] = json!(spans.iter().map(|s| &s.line_id).collect::<BTreeSet<_>>());
                node["page_indexes"] = json!(spans
                    .iter()
                    .filter_map(|s| source
                        .fragments
                        .iter()
                        .find(|f| f.0 == s.line_id)
                        .map(|f| f.3))
                    .collect::<BTreeSet<_>>());
                let entry = inventory(&node, spans, &fact.witness)?;
                retained.push(entry.id.clone());
                removable.push(entry);
                nodes.push(node);
            }
            Err(reason) => record_unconfirmed(&mut unconfirmed, "node", fact, reason),
        }
    }
    for key in [
        "notes",
        "fields",
        "annotations",
        "documents",
        "navigation",
        "continuations",
        "joins",
        "scope_annotations",
    ] {
        structure.insert(key.into(), json!([]));
    }
    structure.entry("diagnostics").or_insert_with(|| json!([]));
    structure.insert("manuscript_edits".into(), json!({}));
    structure.insert("nodes".into(), json!(nodes));
    structure.insert("unconfirmed_native_witnesses".into(), json!(unconfirmed));
    structure.insert(
        "text_sha256".into(),
        json!(format!("{:x}", Sha256::digest(source.text.as_bytes()))),
    );
    structure.insert("text".into(), json!(source.text));
    structure.insert(
        "source_fragments".into(),
        json!(source
            .fragments
            .iter()
            .map(|(id, range, _, page)| json!({"id":id,"range":range,"page_index":page}))
            .collect::<Vec<_>>()),
    );
    Ok(HydratedCompositionNativeFacts {
        structure: Value::Object(structure),
        retained_node_ids: retained,
        removable_nodes: removable,
        unconfirmed_native_witnesses: unconfirmed,
    })
}

fn remap_span(
    span: &SourceSpan,
    before: &HashMap<&str, &CompositionLine>,
    after: &HashMap<&str, &CompositionLine>,
    request: &CompositionNativeRemapRequest,
    preserve_wording: bool,
) -> Result<Vec<SourceSpan>> {
    let old = before
        .get(span.line_id.as_str())
        .ok_or("Native binding names absent input source")?;
    if span.start > span.end || span.end > old.text.chars().count() {
        return Err("Native input binding outside source".into());
    }
    let mut edits: Vec<_> = request
        .edits
        .iter()
        .filter(|e| e.line_id == span.line_id)
        .collect();
    edits.sort_by_key(|e| (e.start, e.end));
    let point = span.start == span.end;
    let mapped = crate::corrections::map_correction_anchor(
        span,
        &edits,
        if point {
            crate::CorrectionAnchorKind::Normalization
        } else {
            crate::CorrectionAnchorKind::Annotation
        },
    )
    .map_err(str::to_owned)?;
    let start = mapped.start;
    let end = mapped.end;
    if !request.edits.is_empty() {
        let line = after
            .get(span.line_id.as_str())
            .ok_or("Native transcription output absent")?;
        if preserve_wording
            && ScalarText::new(&old.text).slice(span.start..span.end)
                != ScalarText::new(&line.text).slice(start..end)
        {
            return Err("Native fact crosses changed source wording".into());
        }
        return Ok(vec![mapped]);
    }
    let descendants = request
        .expansion
        .get(&span.line_id)
        .cloned()
        .unwrap_or_else(|| {
            if after.contains_key(span.line_id.as_str()) {
                vec![span.line_id.clone()]
            } else {
                vec![]
            }
        });
    let mut mapped = Vec::new();
    let mut covered = Vec::new();
    for id in descendants {
        let line = after
            .get(id.as_str())
            .ok_or("Native source expansion names absent output")?;
        let [a, b] = request
            .fragment_ranges
            .get(&id)
            .copied()
            .unwrap_or([0, line.text.chars().count()]);
        if a > b {
            return Err("Native fragment range reversed".into());
        }
        let lo = start.max(a);
        let hi = end.min(b);
        if lo < hi || point && a <= start && start <= b {
            let old_text = ScalarText::new(&old.text);
            let new_text = ScalarText::new(&line.text);
            let source_start = if request.edits.is_empty() {
                lo
            } else {
                span.start
            };
            let source_end = if request.edits.is_empty() {
                hi
            } else {
                span.end
            };
            if new_text.slice(lo - a..hi - a) != old_text.slice(source_start..source_end) {
                return Err("Native source correspondence is not unchanged".into());
            }
            mapped.push(SourceSpan {
                line_id: id,
                start: lo - a,
                end: hi - a,
            });
            covered.push((lo, hi));
        }
    }
    if point {
        if mapped.len() != 1 {
            return Err("Native point lost unique source correspondence".into());
        }
    } else {
        covered.sort_unstable();
        let mut cursor = start;
        for (lo, hi) in covered {
            if lo != cursor {
                return Err("Native fact lost complete source membership".into());
            }
            cursor = hi;
        }
        if cursor != end {
            return Err("Native fact lost source membership".into());
        }
    }
    Ok(mapped)
}

pub fn remap_composition_native_facts(
    request: CompositionNativeRemapRequest,
) -> Result<CompositionNativeFacts> {
    let before: HashMap<_, _> = request
        .before_lines
        .iter()
        .map(|l| (l.id.as_str(), l))
        .collect();
    let after: HashMap<_, _> = request
        .after_lines
        .iter()
        .map(|l| (l.id.as_str(), l))
        .collect();
    if before.len() != request.before_lines.len() || after.len() != request.after_lines.len() {
        return Err("Native source identities repeat".into());
    }
    // Reuse the owning transcription validator; supplied output must be exactly its result.
    if !request.edits.is_empty() {
        if !request.expansion.is_empty() {
            return Err(
                "Native remap applies source operations and transcription separately".into(),
            );
        }
        let corrected = crate::correct_source_text(crate::SourceCorrectionRequest {
            lines: before
                .iter()
                .map(|(id, l)| (id.to_string(), l.text.clone()))
                .collect(),
            edits: request.edits.clone(),
            anchors: vec![],
            native_anchors: vec![],
            witnesses: BTreeMap::new(),
        })?;
        if corrected.lines.len() != after.len()
            || corrected
                .lines
                .iter()
                .any(|(id, text)| after.get(id.as_str()).is_none_or(|l| &l.text != text))
        {
            return Err("Native remap output differs from exact transcription".into());
        }
    }
    let mut result = CompositionNativeFacts::default();
    result.unconfirmed_native_witnesses = request.native_facts.unconfirmed_native_witnesses.clone();
    let remap = |fact: &NativeCompositionFact| -> Result<NativeCompositionFact> {
        let mut mapped = fact.clone();
        for binding in &mut mapped.bindings {
            binding.spans = binding
                .spans
                .iter()
                .map(|s| {
                    remap_span(
                        s,
                        &before,
                        &after,
                        &request,
                        matches!(
                            binding.kind,
                            NativeCompositionBindingKind::Range
                                | NativeCompositionBindingKind::WordingEnvelope
                        ),
                    )
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect();
        }
        Ok(mapped)
    };
    for fact in &request.native_facts.nodes {
        if request.retained_node_ids.as_ref().is_some_and(|ids| {
            !fact.value["id"]
                .as_str()
                .is_some_and(|id| ids.iter().any(|v| v == id))
        }) {
            continue;
        }
        match remap(fact) {
            Ok(fact) => result.nodes.push(fact),
            Err(reason) => record_unconfirmed(
                &mut result.unconfirmed_native_witnesses,
                "node",
                fact,
                reason,
            ),
        }
    }
    for (page, fact) in &request.native_facts.pages {
        match remap(fact) {
            Ok(fact) => {
                result.pages.insert(*page, fact);
            }
            Err(reason) => record_unconfirmed(
                &mut result.unconfirmed_native_witnesses,
                "page",
                fact,
                reason,
            ),
        }
    }
    for (key, fact) in &request.native_facts.metadata {
        match remap(fact) {
            Ok(fact) => {
                result.metadata.insert(key.clone(), fact);
            }
            Err(reason) => record_unconfirmed(
                &mut result.unconfirmed_native_witnesses,
                "metadata",
                fact,
                reason,
            ),
        }
    }
    for fact in &request.native_facts.cross_reference_edges {
        match remap(fact) {
            Ok(fact) => result.cross_reference_edges.push(fact),
            Err(reason) => record_unconfirmed(
                &mut result.unconfirmed_native_witnesses,
                "cross_reference",
                fact,
                reason,
            ),
        }
    }
    Ok(result)
}
